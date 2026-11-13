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
use crate::dispatch::{self, Job};
use crate::engine::{self, Command, EngineStatus, Fetched, RequestSpec, Status};
use crate::provider::FetchError;
use crate::sources::{Format, Source};

/// The largest document a JSON or text request may return.
pub(crate) const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;
/// A binary archive is allowed to be larger than a document. Level II volumes
/// routinely exceed four mebibytes, and the same gate and cache must carry them.
pub(crate) const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;
/// The time limits for one transfer class.
///
/// A policy bounds each phase and the silence between reads. It carries no
/// total deadline, so a large body on a slow link may finish while a stalled
/// socket still fails in bounded time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferPolicy {
    pub(crate) resolve: Duration,
    pub(crate) connect: Duration,
    pub(crate) response: Duration,
    pub(crate) read: Duration,
    pub(crate) write: Duration,
}

impl TransferPolicy {
    /// Selects limits from the registry's wire format.
    pub(crate) const fn for_source(source: &Source) -> Self {
        match source.format {
            Format::Binary => Self {
                resolve: Duration::from_secs(30),
                connect: Duration::from_secs(30),
                response: Duration::from_secs(60),
                read: Duration::from_secs(60),
                write: Duration::from_secs(30),
            },
            Format::Json | Format::GeoJson | Format::Xml | Format::Tiles | Format::VectorTiles => {
                Self {
                    resolve: Duration::from_secs(20),
                    connect: Duration::from_secs(20),
                    response: Duration::from_secs(30),
                    read: Duration::from_secs(30),
                    write: Duration::from_secs(20),
                }
            }
        }
    }
}
/// A handle to the engine thread and the sources this configuration enables.
#[derive(Clone)]
pub struct Acquire {
    commands: Sender<Command>,
    jobs: Option<Sender<Job>>,
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
        let (jobs, job_receiver) = channel::<Job>();
        let worker_commands = commands.clone();
        std::thread::spawn(move || {
            let worker = Self {
                commands: worker_commands,
                jobs: None,
            };
            while let Ok(job) = job_receiver.recv() {
                job(&worker);
            }
        });
        Self {
            commands,
            jobs: Some(jobs),
        }
    }

    /// Queues one off-thread read on the single shared queue.
    ///
    /// # Errors
    ///
    /// Returns [`dispatch::Disconnected`] when the queue has stopped.
    pub(crate) fn offload<F>(&self, work: F) -> Result<(), dispatch::Disconnected>
    where
        F: FnOnce(&Self) + Send + 'static,
    {
        let jobs = self.jobs.as_ref().ok_or(dispatch::Disconnected)?;
        jobs.send(Box::new(work))
            .map_err(|_| dispatch::Disconnected)
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

    /// Requests one named registry operation and returns its served body.
    pub fn fetch_operation(
        &self,
        source: &'static Source,
        operation: &'static str,
        values: &[(&str, &str)],
    ) -> Result<Fetched, FetchError> {
        let spec = RequestSpec::for_operation(source, operation, values)?;
        self.fetch(&spec)
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
            Self::Live => live_get(
                url,
                accept,
                TransferPolicy::for_source(source),
                body_limit(source),
            ),
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

fn body_limit(source: &Source) -> u64 {
    if source.format == Format::Binary {
        MAX_BINARY_BYTES
    } else {
        MAX_DOCUMENT_BYTES
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

fn live_get(
    url: &str,
    accept: &str,
    policy: TransferPolicy,
    limit: u64,
) -> Result<Vec<u8>, FetchError> {
    let mut response = ureq::get(url)
        .header("User-Agent", crate::sources::USER_AGENT)
        .header("Accept", accept)
        .config()
        .timeout_resolve(Some(policy.resolve))
        .timeout_connect(Some(policy.connect))
        .timeout_send_request(Some(policy.write))
        .timeout_send_body(Some(policy.write))
        .timeout_recv_response(Some(policy.response))
        .timeout_per_read(Some(policy.read))
        .timeout_per_write(Some(policy.write))
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
    fn binary_archives_get_a_larger_body_limit() {
        assert_eq!(body_limit(&sources::NEXRAD_LEVEL_TWO), MAX_BINARY_BYTES);
        assert_eq!(body_limit(&sources::OPENFREEMAP), MAX_DOCUMENT_BYTES);
    }

    #[test]
    fn binary_archives_get_longer_phase_limits_than_documents() {
        let binary = TransferPolicy::for_source(&sources::NEXRAD_LEVEL_TWO);
        let document = TransferPolicy::for_source(&sources::OPENFREEMAP);
        assert!(binary.connect > document.connect);
        assert!(binary.response > document.response);
        assert!(binary.read > document.read);
        assert!(document.response >= Duration::from_secs(30));
    }

    #[test]
    fn a_slow_response_is_not_cut_off_by_a_total_deadline() -> Result<(), TestError> {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let mut request = [0_u8; 1024];
            stream
                .read(&mut request)
                .map_err(|error| TestError::from(error.to_string()))?;
            std::thread::sleep(Duration::from_secs(11));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nslow")
                .map_err(|error| TestError::from(error.to_string()))?;
            Ok(())
        });
        let url = format!("http://{address}/slow");
        let body = live_get(
            &url,
            "text/plain",
            TransferPolicy::for_source(&sources::OPENFREEMAP),
            1024,
        )
        .map_err(TestError::from);
        server
            .join()
            .map_err(|_| TestError::from("the slow server thread panicked"))??;
        assert_eq!(body?, b"slow");
        Ok(())
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
