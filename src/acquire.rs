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

use std::io::Read as _;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
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
/// One transfer's conditional and range request.
pub(crate) struct TransferRequest<'a> {
    pub(crate) url: &'a str,
    pub(crate) accept: &'a str,
    pub(crate) etag: Option<&'a str>,
    pub(crate) last_modified: Option<&'a str>,
    pub(crate) offset: u64,
    pub(crate) cancel: &'a CancelToken,
    pub(crate) on_read: &'a mut dyn FnMut(u64),
}

/// A whole or ranged response, with the headers the cache must retain.
#[derive(Debug)]
pub(crate) struct TransferResponse {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
    pub(crate) total_length: Option<u64>,
    pub(crate) range_start: Option<u64>,
    pub(crate) checksum: Option<Checksum>,
}

/// A transfer that stopped after its headers arrived, keeping the bytes already
/// read so the engine can ask for the remainder.
#[derive(Debug)]
pub(crate) struct TransferFailure {
    pub(crate) error: Box<FetchError>,
    pub(crate) partial: Vec<u8>,
    pub(crate) accept_ranges: bool,
    pub(crate) total_length: Option<u64>,
    pub(crate) checksum: Option<Checksum>,
}

/// A digest the origin supplied for the whole representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checksum {
    digest: Vec<u8>,
}

impl Checksum {
    pub(crate) fn digest(&self) -> &[u8] {
        &self.digest
    }
}

/// A reader-owned cancellation flag. The engine checks it between reads and
/// before every attempt, so a cancelled transfer never reaches the cache.
#[derive(Debug, Clone, Default)]
pub(crate) struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cancelled, &other.cancelled)
    }
}

/// A handle to the engine thread and the sources this configuration enables.
#[derive(Clone)]
pub struct Acquire {
    commands: Sender<Command>,
    jobs: Option<Sender<Job>>,
    active: Arc<Mutex<Option<CancelToken>>>,
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
        let active = Arc::new(Mutex::new(None));
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
        let worker_active = Arc::clone(&active);
        std::thread::spawn(move || {
            let worker = Self {
                commands: worker_commands,
                jobs: None,
                active: worker_active,
            };
            while let Ok(job) = job_receiver.recv() {
                job(&worker);
            }
        });
        Self {
            commands,
            jobs: Some(jobs),
            active,
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
        self.fetch_cancellable(spec, &CancelToken::new())
    }

    /// Runs one request with a reader-owned cancellation flag. Cancellation is
    /// checked between reads and during backoff, and never commits a body.
    pub fn fetch_cancellable(
        &self,
        spec: &RequestSpec,
        cancel: &CancelToken,
    ) -> Result<Fetched, FetchError> {
        if let Ok(mut slot) = self.active.lock() {
            *slot = Some(cancel.clone());
        }
        let (reply, receiver) = channel();
        let result = self
            .commands
            .send(Command::Fetch {
                spec: spec.clone(),
                cancel: cancel.clone(),
                reply,
            })
            .map_err(|_| FetchError::WorkerStopped)
            .and_then(|()| receiver.recv().map_err(|_| FetchError::WorkerStopped))?;
        if let Ok(mut slot) = self.active.lock()
            && slot.as_ref().is_some_and(|active| active.same(cancel))
        {
            *slot = None;
        }
        result
    }

    /// Cancels the transfer this handle last started, if it is still running.
    /// The engine checks the flag between reads and during backoff.
    pub fn cancel_active(&self) -> bool {
        self.active.lock().ok().is_some_and(|slot| {
            slot.as_ref().is_some_and(|cancel| {
                cancel.cancel();
                true
            })
        })
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
        request: &mut TransferRequest<'_>,
    ) -> Result<TransferResponse, TransferFailure> {
        if !enabled.enables(source.id.as_str()) {
            return Err(TransferFailure {
                error: Box::new(FetchError::Disabled { id: source.id }),
                partial: Vec::new(),
                accept_ranges: false,
                total_length: None,
                checksum: None,
            });
        }
        match self {
            Self::Live => live_get(
                request,
                TransferPolicy::for_source(source),
                body_limit(source),
            ),
            #[cfg(test)]
            Self::Offline => Err(TransferFailure {
                error: Box::new(FetchError::WorkerStopped),
                partial: Vec::new(),
                accept_ranges: false,
                total_length: None,
                checksum: None,
            }),
            #[cfg(test)]
            Self::Scripted { body, observed } => {
                observed
                    .send(Observed {
                        source: source.id,
                        url: String::from(request.url),
                    })
                    .map_err(|_| TransferFailure {
                        error: Box::new(FetchError::WorkerStopped),
                        partial: Vec::new(),
                        accept_ranges: false,
                        total_length: None,
                        checksum: None,
                    })?;
                (request.on_read)(body.len() as u64);
                Ok(TransferResponse {
                    status: 200,
                    body: body.to_vec(),
                    etag: None,
                    last_modified: None,
                    total_length: Some(body.len() as u64),
                    range_start: None,
                    checksum: None,
                })
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

/// Carries one request with its conditional and range headers. Bytes already
/// read are returned with a read failure so the caller can resume them.
pub(crate) fn live_get(
    request: &mut TransferRequest<'_>,
    policy: TransferPolicy,
    limit: u64,
) -> Result<TransferResponse, TransferFailure> {
    if request.cancel.is_cancelled() {
        return Err(cancelled_failure());
    }
    let response = call_transfer(request, policy)?;
    let status = response.status().as_u16();
    let meta = response_meta(&response)?;
    if status == 304 {
        return Ok(TransferResponse {
            status,
            body: Vec::new(),
            etag: meta.etag.or_else(|| request.etag.map(String::from)),
            last_modified: meta
                .last_modified
                .or_else(|| request.last_modified.map(String::from)),
            total_length: None,
            range_start: None,
            checksum: None,
        });
    }
    if !(200..300).contains(&status) {
        let retry_after = header_text(response.headers(), "retry-after");
        return Err(fail(
            FetchError::Status {
                url: String::from(request.url),
                status,
                retry_after,
            },
            &meta,
            Vec::new(),
        ));
    }
    let range_start = checked_range_start(&meta, status, request.offset, request.url)?;
    read_transfer_body(response, request, limit, meta, status, range_start)
}

fn call_transfer(
    request: &TransferRequest<'_>,
    policy: TransferPolicy,
) -> Result<ureq::http::Response<ureq::Body>, TransferFailure> {
    let mut builder = ureq::get(request.url)
        .header("User-Agent", crate::sources::USER_AGENT)
        .header("Accept", request.accept);
    if let Some(etag) = request.etag {
        builder = builder.header("If-None-Match", etag);
    }
    if let Some(modified) = request.last_modified {
        builder = builder.header("If-Modified-Since", modified);
    }
    let range = format!("bytes={}-", request.offset);
    if request.offset > 0 {
        builder = builder.header("Range", &range);
    }
    builder
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
        .map_err(|source| TransferFailure {
            error: Box::new(FetchError::Request {
                url: String::from(request.url),
                source: Box::new(source),
            }),
            partial: Vec::new(),
            accept_ranges: false,
            total_length: None,
            checksum: None,
        })
}

struct TransferMeta {
    etag: Option<String>,
    last_modified: Option<String>,
    accept_ranges: bool,
    content_length: Option<u64>,
    content_range: Option<String>,
    checksum: Option<Checksum>,
}

impl TransferMeta {
    fn total_length(&self) -> Option<u64> {
        self.content_range
            .as_deref()
            .and_then(parse_content_range)
            .map(|(_start, total)| total)
            .or(self.content_length)
    }
}

fn fail(error: FetchError, meta: &TransferMeta, partial: Vec<u8>) -> TransferFailure {
    TransferFailure {
        error: Box::new(error),
        partial,
        accept_ranges: meta.accept_ranges,
        total_length: meta.total_length(),
        checksum: meta.checksum.clone(),
    }
}

fn response_meta(
    response: &ureq::http::Response<ureq::Body>,
) -> Result<TransferMeta, TransferFailure> {
    let accept_ranges = header_text(response.headers(), "accept-ranges")
        .is_some_and(|value| value.split(',').any(|part| part.trim() == "bytes"));
    let encoded = header_text(response.headers(), "content-encoding")
        .is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("identity"));
    let content_length = if encoded {
        None
    } else {
        header_text(response.headers(), "content-length")
            .and_then(|value| value.parse::<u64>().ok())
    };
    let mut meta = TransferMeta {
        etag: header_text(response.headers(), "etag"),
        last_modified: header_text(response.headers(), "last-modified"),
        accept_ranges,
        content_length,
        content_range: header_text(response.headers(), "content-range"),
        checksum: None,
    };
    meta.checksum = match checksum_from_headers(response.headers()) {
        Ok(checksum) => checksum,
        Err(error) => return Err(fail(error, &meta, Vec::new())),
    };
    Ok(meta)
}

fn checked_range_start(
    meta: &TransferMeta,
    status: u16,
    offset: u64,
    url: &str,
) -> Result<Option<u64>, TransferFailure> {
    if status != 206 {
        return Ok(None);
    }
    let Some((start, _total)) = meta.content_range.as_deref().and_then(parse_content_range) else {
        return Err(fail(
            FetchError::Range {
                url: String::from(url),
                detail: "a 206 response needs a valid Content-Range",
            },
            meta,
            Vec::new(),
        ));
    };
    if start != offset {
        return Err(fail(
            FetchError::Range {
                url: String::from(url),
                detail: "the response starts at a different offset than the request",
            },
            meta,
            Vec::new(),
        ));
    }
    Ok(Some(start))
}

fn read_transfer_body(
    mut response: ureq::http::Response<ureq::Body>,
    request: &mut TransferRequest<'_>,
    limit: u64,
    meta: TransferMeta,
    status: u16,
    range_start: Option<u64>,
) -> Result<TransferResponse, TransferFailure> {
    if meta.content_length.is_some_and(|length| length > limit) {
        return Err(fail(
            FetchError::Integrity {
                url: String::from(request.url),
                detail: "the declared body length exceeds the request limit",
            },
            &meta,
            Vec::new(),
        ));
    }
    let mut reader = response.body_mut().as_reader();
    let mut body = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if request.cancel.is_cancelled() {
            return Err(fail(FetchError::Cancelled, &meta, body));
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let received = request
                    .offset
                    .saturating_add(body.len() as u64)
                    .saturating_add(count as u64);
                if received > limit {
                    return Err(fail(
                        FetchError::Integrity {
                            url: String::from(request.url),
                            detail: "the body grew past the request limit",
                        },
                        &meta,
                        body,
                    ));
                }
                let Some(chunk) = buffer.get(..count) else {
                    return Err(fail(
                        FetchError::Integrity {
                            url: String::from(request.url),
                            detail: "the read count exceeded the buffer",
                        },
                        &meta,
                        body,
                    ));
                };
                body.extend_from_slice(chunk);
                (request.on_read)(body.len() as u64);
            }
            Err(source) => {
                return Err(fail(
                    FetchError::Body {
                        url: String::from(request.url),
                        source,
                    },
                    &meta,
                    body,
                ));
            }
        }
    }
    if let Some(expected) = meta.content_length {
        if body.len() as u64 > expected {
            return Err(fail(
                FetchError::Integrity {
                    url: String::from(request.url),
                    detail: "the body is longer than the declared length",
                },
                &meta,
                body,
            ));
        }
        if (body.len() as u64) < expected {
            return Err(fail(
                FetchError::Body {
                    url: String::from(request.url),
                    source: std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the origin closed before the declared body length",
                    ),
                },
                &meta,
                body,
            ));
        }
    }
    let total_length = meta.total_length();
    Ok(TransferResponse {
        status,
        body,
        etag: meta.etag,
        last_modified: meta.last_modified,
        total_length,
        range_start,
        checksum: meta.checksum,
    })
}

fn cancelled_failure() -> TransferFailure {
    TransferFailure {
        error: Box::new(FetchError::Cancelled),
        partial: Vec::new(),
        accept_ranges: false,
        total_length: None,
        checksum: None,
    }
}

fn header_text(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .map(|value| String::from(value.trim()))
}

fn parse_content_range(value: &str) -> Option<(u64, u64)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    Some((start.parse().ok()?, total.parse().ok()?))
}

fn checksum_from_headers(headers: &ureq::http::HeaderMap) -> Result<Option<Checksum>, FetchError> {
    if let Some(value) = header_text(headers, "x-checksum-sha256") {
        let digest = decode_hex(&value).ok_or_else(|| FetchError::Integrity {
            url: String::from("the response"),
            detail: "X-Checksum-SHA256 is not hexadecimal",
        })?;
        return Ok(Some(Checksum { digest }));
    }
    if let Some(value) = header_text(headers, "x-amz-checksum-sha256") {
        let digest = decode_base64(&value).ok_or_else(|| FetchError::Integrity {
            url: String::from("the response"),
            detail: "X-Amz-Checksum-SHA256 is not base64",
        })?;
        return Ok(Some(Checksum { digest }));
    }
    for name in ["content-digest", "repr-digest"] {
        let Some(value) = header_text(headers, name) else {
            continue;
        };
        for part in value.split(',') {
            let Some(encoded) = part.trim().strip_prefix("sha-256=") else {
                continue;
            };
            let encoded = encoded.trim().trim_matches(':');
            let digest = decode_base64(encoded).ok_or_else(|| FetchError::Integrity {
                url: String::from("the response"),
                detail: "the SHA-256 digest is not base64",
            })?;
            return Ok(Some(Checksum { digest }));
        }
    }
    Ok(None)
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let (pairs, remainder) = text.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    for pair in pairs {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let text = text.trim().trim_matches('"');
    let mut bytes = Vec::with_capacity(text.len().saturating_mul(3) / 4);
    let mut accumulator = 0_u32;
    let mut bits = 0_u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits = bits.saturating_add(6);
        if bits >= 8 {
            bits = bits.saturating_sub(8);
            bytes.push((accumulator >> bits) as u8);
        }
    }
    Some(bytes)
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
        let cancel = CancelToken::new();
        let mut on_read = |_bytes| {};
        let mut request = TransferRequest {
            url: &url,
            accept: "text/plain",
            etag: None,
            last_modified: None,
            offset: 0,
            cancel: &cancel,
            on_read: &mut on_read,
        };
        let body = live_get(
            &mut request,
            TransferPolicy::for_source(&sources::OPENFREEMAP),
            1024,
        )
        .map_err(|failure| TestError::from(failure.error.to_string()));
        server
            .join()
            .map_err(|_| TestError::from("the slow server thread panicked"))??;
        assert_eq!(body?.body, b"slow");
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
