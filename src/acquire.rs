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
//!
//! The gate is a handle to one engine thread, which owns the cache, pacing
//! clock and request records. Callers send a spec and wait for the body.

#[cfg(test)]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use serde_json::Value;

use crate::config::EnabledProviders;
use crate::engine::{self, Command, EngineStatus, Fetched, RequestSpec, Status};
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

/// A handle to the engine thread and the sources this configuration enables.
#[derive(Clone)]
pub struct Acquire {
    commands: Sender<Command>,
}

impl Acquire {
    /// Builds a gate for the providers a configuration enables.
    pub fn new(enabled: EnabledProviders) -> Self {
        Self::start(enabled, Transport::Live, true, None)
    }

    /// Builds a gate and the non-blocking status reader for its engine.
    pub fn new_with_status(enabled: EnabledProviders) -> (Self, EngineStatus) {
        let (updates, receiver) = channel();
        let acquire = Self::start(enabled, Transport::Live, true, Some(updates));
        (acquire, EngineStatus::new(receiver))
    }

    fn start(
        enabled: EnabledProviders,
        transport: Transport,
        open_store: bool,
        updates: Option<Sender<Status>>,
    ) -> Self {
        let (commands, receiver) = channel();
        std::thread::spawn(move || {
            engine::serve(
                &receiver,
                &enabled,
                &transport,
                open_store,
                updates.as_ref(),
            );
        });
        Self { commands }
    }

    /// Runs one request through the engine: cache, pacing, retry and record.
    pub fn fetch(&self, spec: &RequestSpec) -> Result<Fetched, FetchError> {
        let (reply, receiver) = channel();
        self.commands
            .send(Command::Fetch {
                spec: spec.clone(),
                reply,
            })
            .map_err(|_| FetchError::WorkerStopped)?;
        receiver.recv().map_err(|_| FetchError::WorkerStopped)?
    }

    /// The engine's request records, newest state per identity.
    pub fn provenance(&self) -> Value {
        let (reply, receiver) = channel();
        if self.commands.send(Command::Report(reply)).is_err() {
            return serde_json::json!({"error": "the request engine stopped"});
        }
        receiver
            .recv()
            .unwrap_or_else(|_| serde_json::json!({"error": "the request engine stopped"}))
    }

    /// Writes the engine's request records to a JSON file and returns its path.
    pub fn export_provenance(&self) -> Result<std::path::PathBuf, String> {
        let (reply, receiver) = channel();
        self.commands
            .send(Command::Export(reply))
            .map_err(|_| String::from("the request engine stopped"))?;
        receiver
            .recv()
            .map_err(|_| String::from("the request engine stopped"))?
    }

    /// Why the response cache could not be opened, when it could not.
    pub fn cache_error(&self) -> Option<String> {
        let (reply, receiver) = channel();
        if self.commands.send(Command::CacheError(reply)).is_err() {
            return Some(String::from("the request engine stopped"));
        }
        receiver
            .recv()
            .unwrap_or_else(|_| Some(String::from("the request engine stopped")))
    }

    /// Removes one stored body so a corrupt decode is not replayed.
    pub fn evict(&self, key: &str) -> Result<(), String> {
        let (reply, receiver) = channel();
        self.commands
            .send(Command::Evict {
                key: String::from(key),
                reply,
            })
            .map_err(|_| String::from("the request engine stopped"))?;
        receiver
            .recv()
            .map_err(|_| String::from("the request engine stopped"))?
    }
}

/// The live HTTPS transport, and test transports that count a request without
/// a socket.
pub(crate) enum Transport {
    Live,
    #[cfg(test)]
    Offline,
    #[cfg(test)]
    Scripted {
        body: &'static [u8],
        observed: Sender<Observed>,
    },
}

impl Transport {
    /// Carries one admitted request. The engine has already paced it.
    pub(crate) fn get(
        &self,
        enabled: &EnabledProviders,
        source: &'static Source,
        url: &str,
        accept: &str,
    ) -> Result<Vec<u8>, FetchError> {
        if !enabled.enables(source.id.as_str()) {
            return Err(FetchError::Disabled { id: source.id });
        }
        match self {
            Self::Live => live_get(url, accept, REQUEST_TIMEOUT, MAX_DOCUMENT_BYTES),
            #[cfg(test)]
            Self::Offline => Err(FetchError::WorkerStopped),
            #[cfg(test)]
            Self::Scripted { body, observed } => {
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
        Self::start(
            EnabledProviders::all_admitted(),
            Transport::Offline,
            false,
            None,
        )
    }

    /// A gate for the enabled set whose transport returns `body` and reports
    /// every request it is asked to carry.
    pub fn scripted(enabled: EnabledProviders, body: &'static [u8]) -> (Self, Receiver<Observed>) {
        let (observed, receiver) = channel();
        (
            Self::start(enabled, Transport::Scripted { body, observed }, false, None),
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
        let result = acquire
            .fetch(&RequestSpec::for_source(
                &sources::DWD_OPEN_DATA,
                "https://example.test/fire",
                "application/json",
            ))
            .and_then(|fetched| fetched.json());
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
        let document = acquire
            .fetch(&RequestSpec::for_source(
                &sources::NIFC_WFIGS,
                "https://example.test/fire",
                "application/json",
            ))
            .and_then(|fetched| fetched.json())?;
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
        let text = acquire
            .fetch(&RequestSpec::for_source(
                &sources::USGS_WATER,
                "https://example.test/series",
                "text/csv",
            ))
            .and_then(|fetched| fetched.text())?;
        assert_eq!(text, "a,b\n1,2\n");
        Ok(())
    }

    #[test]
    fn a_body_that_is_not_utf8_is_a_shape_failure() -> Result<(), TestError> {
        let (acquire, _observed) = Acquire::scripted(enabled(), &[0xff, 0xfe]);
        let result = acquire
            .fetch(&RequestSpec::for_source(
                &sources::USGS_WATER,
                "https://example.test/series",
                "text/csv",
            ))
            .and_then(|fetched| fetched.text());
        assert!(matches!(result, Err(FetchError::Shape { .. })));
        Ok(())
    }
}
