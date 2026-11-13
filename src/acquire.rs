// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//! The single gate every network request passes through.
//!
//! A caller names the registry source it is asking. The gate refuses a source
//! the effective configuration does not enable before a socket is opened, so a
//! disabled provider cannot fetch even when a caller still asks for it.

use std::time::Duration;

use serde_json::Value;

use crate::config::EnabledProviders;
use crate::provider::FetchError;
use crate::sources::Source;

/// The largest document a JSON or text request may return.
pub(crate) const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one address may take to answer, so a dual-stack host with a dead
/// IPv6 route does not hold up a request the working address answers at once.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const USER_AGENT: &str = concat!(
    "oxrad/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/bonzupii/oxrad)"
);

/// The enabled source set and the transport that carries admitted requests.
#[derive(Clone)]
pub struct Acquire {
    enabled: EnabledProviders,
    transport: Transport,
}

impl Acquire {
    /// Builds a gate for the providers a configuration enables.
    pub const fn new(enabled: EnabledProviders) -> Self {
        Self {
            enabled,
            transport: Transport::Live,
        }
    }

    /// Fetches a JSON document from a registry source.
    pub fn json(
        &self,
        source: &'static Source,
        url: &str,
        accept: &str,
    ) -> Result<Value, FetchError> {
        let body = self.bytes_within(source, url, accept, REQUEST_TIMEOUT, MAX_DOCUMENT_BYTES)?;
        serde_json::from_slice(&body).map_err(|source| FetchError::Json {
            url: String::from(url),
            source,
        })
    }

    /// Fetches a JSON document within a stated time, for a heavy query.
    pub fn json_within(
        &self,
        source: &'static Source,
        url: &str,
        accept: &str,
        timeout: Duration,
    ) -> Result<Value, FetchError> {
        let body = self.bytes_within(source, url, accept, timeout, MAX_DOCUMENT_BYTES)?;
        serde_json::from_slice(&body).map_err(|source| FetchError::Json {
            url: String::from(url),
            source,
        })
    }

    /// Fetches a document written as text, such as a comma-separated index.
    pub fn text(
        &self,
        source: &'static Source,
        url: &str,
        accept: &str,
    ) -> Result<String, FetchError> {
        let body = self.bytes_within(source, url, accept, REQUEST_TIMEOUT, MAX_DOCUMENT_BYTES)?;
        String::from_utf8(body).map_err(|_| FetchError::Shape {
            url: String::from(url),
            field: "a body written as text",
        })
    }

    /// Fetches a binary document, such as an imagery tile.
    pub fn bytes(
        &self,
        source: &'static Source,
        url: &str,
        accept: &str,
        limit: u64,
    ) -> Result<Vec<u8>, FetchError> {
        self.bytes_within(source, url, accept, REQUEST_TIMEOUT, limit)
    }

    fn bytes_within(
        &self,
        source: &'static Source,
        url: &str,
        accept: &str,
        timeout: Duration,
        limit: u64,
    ) -> Result<Vec<u8>, FetchError> {
        self.admit(source)?;
        match &self.transport {
            Transport::Live => live_get(url, accept, timeout, limit),
            #[cfg(test)]
            Transport::Offline => Err(FetchError::WorkerStopped),
            #[cfg(test)]
            Transport::Scripted { body, observed } => {
                observed
                    .send(Observed {
                        source: source.id,
                        url: String::from(url),
                    })
                    .map_err(|_| FetchError::WorkerStopped)?;
                Ok(body.to_vec())
            }
        }
    }

    fn admit(&self, source: &'static Source) -> Result<(), FetchError> {
        if self.enabled.enables(source.id.as_str()) {
            return Ok(());
        }
        Err(FetchError::Disabled { id: source.id })
    }
}

/// The live HTTPS transport, and a scripted one that lets a test count a
/// request without a socket.
#[derive(Clone)]
enum Transport {
    Live,
    #[cfg(test)]
    Offline,
    #[cfg(test)]
    Scripted {
        body: &'static [u8],
        observed: std::sync::mpsc::Sender<Observed>,
    },
}

/// One request a scripted transport was asked to carry.
#[cfg(test)]
#[derive(Debug)]
pub struct Observed {
    pub source: crate::sources::SourceId,
    pub url: String,
}

#[cfg(test)]
impl Acquire {
    /// A gate that enables every admitted source and carries no request.
    pub fn offline() -> Self {
        Self {
            enabled: EnabledProviders::all_admitted(),
            transport: Transport::Offline,
        }
    }

    /// A gate for the enabled set whose transport returns `body` and reports
    /// every request it is asked to carry.
    pub fn scripted(
        enabled: EnabledProviders,
        body: &'static [u8],
    ) -> (Self, std::sync::mpsc::Receiver<Observed>) {
        let (observed, receiver) = std::sync::mpsc::channel();
        (
            Self {
                enabled,
                transport: Transport::Scripted { body, observed },
            },
            receiver,
        )
    }
}

fn live_get(url: &str, accept: &str, timeout: Duration, limit: u64) -> Result<Vec<u8>, FetchError> {
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", accept)
        .config()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|source| FetchError::Request {
            url: String::from(url),
            source,
        })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = response
            .headers()
            .get("retry-after")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
        return Err(FetchError::Status {
            url: String::from(url),
            status,
            retry_after,
        });
    }
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|source| FetchError::Body {
            url: String::from(url),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::sources;
    use crate::test_support::TestError;

    fn enabled() -> EnabledProviders {
        EnabledProviders::all_admitted()
    }

    #[test]
    fn a_disabled_source_is_refused_without_a_request() -> Result<(), TestError> {
        let (acquire, observed) = Acquire::scripted(enabled(), br#"{"features":[]}"#);
        let result = acquire.json(
            &sources::DWD_OPEN_DATA,
            "https://example.test/fire",
            "application/json",
        );
        assert!(matches!(
            result,
            Err(FetchError::Disabled { id }) if id.as_str() == "dwd-open-data"
        ));
        assert!(observed.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn an_enabled_source_is_requested_once_and_parsed() -> Result<(), TestError> {
        let (acquire, observed) = Acquire::scripted(enabled(), br#"{"features":[]}"#);
        let document = acquire.json(
            &sources::NIFC_WFIGS,
            "https://example.test/fire",
            "application/json",
        )?;
        assert_eq!(document["features"].as_array().map(Vec::len), Some(0));
        let request = observed.try_recv();
        assert!(matches!(
            request,
            Ok(Observed { source, url })
                if url == "https://example.test/fire" && source.as_str() == "nifc-wfigs"
        ));
        assert!(observed.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn a_text_document_keeps_its_bytes() -> Result<(), TestError> {
        let (acquire, _observed) = Acquire::scripted(enabled(), b"a,b\n1,2\n");
        let text = acquire.text(
            &sources::USGS_WATER,
            "https://example.test/series",
            "text/csv",
        )?;
        assert_eq!(text, "a,b\n1,2\n");
        Ok(())
    }

    #[test]
    fn a_body_that_is_not_utf8_is_a_shape_failure() -> Result<(), TestError> {
        let (acquire, _observed) = Acquire::scripted(enabled(), &[0xff, 0xfe]);
        let result = acquire.text(
            &sources::USGS_WATER,
            "https://example.test/series",
            "text/csv",
        );
        assert!(matches!(result, Err(FetchError::Shape { .. })));
        Ok(())
    }
}
