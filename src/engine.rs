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
//! The request engine: identity, pacing, cache, retry and provenance for every
//! remote document.
//!
//! One engine thread owns the response cache, the pacing clock and the request
//! records. Callers send a request spec and receive the served body; they own no
//! retry, cache or rate state.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::acquire::Transport;
use crate::cache::{CacheError, Store};
use crate::config::EnabledProviders;
use crate::provider::FetchError;
use crate::retry;
use crate::sources::{Refresh, Source, SourceId};

/// Bounded backoff for a request that may succeed later.
pub(crate) const RETRY_BASE: Duration = Duration::from_secs(2);
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(60);
pub(crate) const MAX_ATTEMPTS: u32 = 4;

/// What one request is for, independent of how the caller parses the body.
#[derive(Debug, Clone)]
pub(crate) struct RequestSpec {
    pub(crate) source: &'static Source,
    pub(crate) url: String,
    pub(crate) accept: &'static str,
    /// Cache and dedup identity. It must name every value that changes the
    /// response, including coordinates and times.
    pub(crate) key: String,
    /// What this value is, in the reader's terms.
    pub(crate) subject: String,
    pub(crate) station: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) scan_unix_ms: Option<u64>,
    /// A cache lifetime override. `None` uses the registry refresh cadence.
    pub(crate) ttl: Option<Duration>,
    pub(crate) store: bool,
}

impl RequestSpec {
    pub(crate) fn new(
        source: &'static Source,
        url: impl Into<String>,
        accept: &'static str,
        key: impl Into<String>,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            source,
            url: url.into(),
            accept,
            key: key.into(),
            subject: subject.into(),
            station: None,
            product: None,
            scan_unix_ms: None,
            ttl: None,
            store: true,
        }
    }

    /// A document whose identity is its exact request URL.
    pub(crate) fn for_source(
        source: &'static Source,
        url: impl Into<String>,
        accept: &'static str,
    ) -> Self {
        let url = url.into();
        Self {
            source,
            key: url.clone(),
            url,
            accept,
            subject: String::from(source.name),
            station: None,
            product: None,
            scan_unix_ms: None,
            ttl: None,
            store: true,
        }
    }

    pub(crate) fn station(mut self, station: impl Into<String>) -> Self {
        self.station = Some(station.into());
        self
    }

    pub(crate) fn product(mut self, product: impl Into<String>) -> Self {
        self.product = Some(product.into());
        self
    }

    pub(crate) const fn scan(mut self, scan_unix_ms: u64) -> Self {
        self.scan_unix_ms = Some(scan_unix_ms);
        self
    }

    pub(crate) const fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    #[cfg(test)]
    pub(crate) fn without_store(mut self) -> Self {
        self.store = false;
        self
    }

    fn cache_ttl(&self) -> Duration {
        self.ttl.unwrap_or_else(|| match self.source.refresh {
            Refresh::Seconds(seconds) => Duration::from_secs(u64::from(seconds)),
            Refresh::ProviderDefined => Duration::from_secs(300),
        })
    }

    fn provenance(&self) -> Provenance {
        Provenance {
            source: self.source.id.as_str(),
            source_name: self.source.name,
            credit: self.source.credit,
            subject: self.subject.clone(),
            station: self.station.clone(),
            product: self.product.clone(),
            scan_unix_ms: self.scan_unix_ms,
            url: self.url.clone(),
            rate_limit_seconds: self.source.rate_limit_seconds,
        }
    }
}

/// Where a served body came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheState {
    Network,
    Cached,
    NotStored,
}

impl CacheState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Cached => "cache",
            Self::NotStored => "not stored",
        }
    }
}

/// The request that produced a value, for display and export.
#[derive(Debug, Clone)]
pub(crate) struct Provenance {
    pub(crate) source: &'static str,
    pub(crate) source_name: &'static str,
    pub(crate) credit: &'static str,
    pub(crate) subject: String,
    pub(crate) station: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) scan_unix_ms: Option<u64>,
    pub(crate) url: String,
    pub(crate) rate_limit_seconds: u32,
}

/// A body the engine served, with the request that produced it.
#[derive(Debug)]
pub(crate) struct Fetched {
    pub(crate) body: Vec<u8>,
    pub(crate) provenance: Provenance,
    pub(crate) cache: CacheState,
}

impl Fetched {
    pub(crate) fn text(&self) -> Result<String, FetchError> {
        String::from_utf8(self.body.clone()).map_err(|_| FetchError::Shape {
            url: self.provenance.url.clone(),
            field: "a body written as text",
        })
    }

    pub(crate) fn json(&self) -> Result<Value, FetchError> {
        serde_json::from_slice(&self.body).map_err(|source| FetchError::Json {
            url: self.provenance.url.clone(),
            source,
        })
    }
}

/// One engine record: the last request with this identity and what it cost.
#[derive(Debug, Clone)]
pub(crate) struct Record {
    pub(crate) provenance: Provenance,
    pub(crate) cache: CacheState,
    pub(crate) attempts: u32,
    pub(crate) bytes: usize,
    pub(crate) served_at_unix_ms: Option<u64>,
    pub(crate) age_ms: Option<u64>,
}

impl Record {
    fn to_json(&self, key: &str) -> Value {
        json!({
            "key": key,
            "source": self.provenance.source,
            "source_name": self.provenance.source_name,
            "credit": self.provenance.credit,
            "subject": self.provenance.subject,
            "station": self.provenance.station,
            "product": self.provenance.product,
            "scan_unix_ms": self.provenance.scan_unix_ms,
            "url": self.provenance.url,
            "cache": self.cache.label(),
            "age_ms": self.age_ms,
            "attempts": self.attempts,
            "bytes": self.bytes,
            "served_at_unix_ms": self.served_at_unix_ms,
            "rate_limit_seconds": self.provenance.rate_limit_seconds,
        })
    }
}

/// Cache, pacing and request records owned by the engine thread.
pub(crate) struct EngineState {
    store: Option<Store>,
    cache_error: Option<String>,
    last_request: BTreeMap<SourceId, Instant>,
    records: BTreeMap<String, Record>,
    #[cfg(test)]
    immediate_retries: bool,
}

impl EngineState {
    pub(crate) fn new(open_store: bool) -> Self {
        let (store, cache_error) = if open_store {
            match Store::default_location() {
                Ok(store) => (Some(store), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (None, None)
        };
        Self {
            store,
            cache_error,
            last_request: BTreeMap::new(),
            records: BTreeMap::new(),
            #[cfg(test)]
            immediate_retries: false,
        }
    }

    /// A state with an explicit store, for deterministic cache tests.
    #[cfg(test)]
    pub(crate) fn test_with_store(store: Store) -> Self {
        let mut state = Self::new(false);
        state.store = Some(store);
        state.immediate_retries = true;
        state
    }

    fn store(&self) -> Option<Store> {
        self.store.clone()
    }

    /// Waits until the source's published rate allows another request.
    fn pace(&mut self, source: &'static Source) -> Duration {
        let interval = Duration::from_secs(u64::from(source.rate_limit_seconds.max(1)));
        let now = Instant::now();
        let allowed = self
            .last_request
            .get(&source.id)
            .and_then(|previous| previous.checked_add(interval));
        let wait = allowed.map_or(Duration::ZERO, |allowed| {
            allowed.saturating_duration_since(now)
        });
        self.last_request
            .insert(source.id, now.checked_add(wait).unwrap_or(now));
        wait
    }

    fn observe(&mut self, key: &str, record: Record) {
        self.records.insert(String::from(key), record);
    }

    #[cfg(test)]
    pub(crate) fn records(&self) -> &BTreeMap<String, Record> {
        &self.records
    }

    fn note_cache_error(&mut self, error: &CacheError) {
        self.cache_error = Some(error.to_string());
    }

    fn evict(&mut self, key: &str) -> Result<(), String> {
        let Some(store) = self.store() else {
            return Ok(());
        };
        store.remove(key).map_err(|error| error.to_string())
    }

    /// The most recently served request, for the status bar.
    fn latest(&self) -> Option<(&str, &Record)> {
        self.records
            .iter()
            .max_by_key(|(_, record)| record.served_at_unix_ms)
            .map(|(key, record)| (key.as_str(), record))
    }

    fn provenance_json(&self) -> Value {
        let entries: Vec<Value> = self
            .records
            .iter()
            .map(|(key, record)| record.to_json(key))
            .collect();
        json!({
            "requests": entries,
            "cache_error": self.cache_error,
        })
    }
}

/// Messages accepted by the engine thread.
pub(crate) enum Command {
    Fetch {
        spec: RequestSpec,
        reply: Sender<Result<Fetched, FetchError>>,
    },
    Report(Sender<Value>),
    Export(Sender<Result<PathBuf, String>>),
    CacheError(Sender<Option<String>>),
    Evict {
        key: String,
        reply: Sender<Result<(), String>>,
    },
}

/// A non-blocking snapshot of the engine, published after every command so a
/// drawing thread never waits on a request in flight.
#[derive(Debug, Clone)]
pub(crate) struct Status {
    pub(crate) requests: usize,
    pub(crate) summary: Option<String>,
    pub(crate) cache_error: Option<String>,
}

/// The receiving end of the engine's status snapshots.
pub(crate) struct EngineStatus {
    receiver: Receiver<Status>,
}

impl EngineStatus {
    pub(crate) const fn new(receiver: Receiver<Status>) -> Self {
        Self { receiver }
    }

    /// Returns the newest snapshot, discarding older ones.
    pub(crate) fn latest(&self) -> Option<Status> {
        let mut latest = None;
        while let Ok(status) = self.receiver.try_recv() {
            latest = Some(status);
        }
        latest
    }
}

/// Runs the one owner of cache, pacing and request records.
pub(crate) fn serve(
    receiver: &Receiver<Command>,
    enabled: &EnabledProviders,
    transport: &Transport,
    open_store: bool,
    updates: Option<&Sender<Status>>,
) {
    let mut state = EngineState::new(open_store);
    publish(updates, &state);
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Fetch { spec, reply } => {
                let result = state.fetch(&spec, enabled, transport);
                if reply.send(result).is_err() {
                    return;
                }
            }
            Command::Report(reply) => {
                if reply.send(state.provenance_json()).is_err() {
                    return;
                }
            }
            Command::Export(reply) => {
                if reply.send(state.export()).is_err() {
                    return;
                }
            }
            Command::CacheError(reply) => {
                if reply.send(state.cache_error.clone()).is_err() {
                    return;
                }
            }
            Command::Evict { key, reply } => {
                if reply.send(state.evict(&key)).is_err() {
                    return;
                }
            }
        }
        publish(updates, &state);
    }
}

fn publish(updates: Option<&Sender<Status>>, state: &EngineState) {
    let Some(updates) = updates else {
        return;
    };
    if updates
        .send(Status {
            requests: state.records.len(),
            summary: state.summary(),
            cache_error: state.cache_error.clone(),
        })
        .is_err()
    {}
}

impl EngineState {
    fn fetch(
        &mut self,
        spec: &RequestSpec,
        enabled: &EnabledProviders,
        transport: &Transport,
    ) -> Result<Fetched, FetchError> {
        execute(
            self,
            |url| transport.get(enabled, spec.source, url, spec.accept),
            spec,
        )
    }

    fn summary(&self) -> Option<String> {
        self.latest().map(|(_, record)| {
            let age = record.age_ms.map_or_else(String::new, |millis| {
                format!(" · cached {:.1} s ago", millis as f64 / 1000.0)
            });
            format!(
                "{} · {} · {}{} · {} attempt{} · {}",
                record.provenance.subject,
                record.provenance.source_name,
                record.cache.label(),
                age,
                record.attempts,
                if record.attempts == 1 { "" } else { "s" },
                record.provenance.credit
            )
        })
    }

    fn export(&self) -> Result<PathBuf, String> {
        let directory = crate::paths::cache_directory().map_err(|error| error.to_string())?;
        let path = directory.join("provenance.json");
        let encoded = serde_json::to_vec_pretty(&self.provenance_json())
            .map_err(|error| error.to_string())?;
        std::fs::write(&path, encoded).map_err(|error| error.to_string())?;
        Ok(path)
    }
}

/// A body read from the store, with the age the engine needs for its TTL.
pub(crate) struct Aged {
    pub(crate) body: Vec<u8>,
    pub(crate) age: Duration,
}

/// Loads a stored body and its age in milliseconds since it was fetched.
pub(crate) fn load_aged(store: &Store, key: &str) -> Result<Option<Aged>, CacheError> {
    let Some(entry) = store.load_aged(key)? else {
        return Ok(None);
    };
    let now = now_unix_ms()?;
    let age_ms = now.saturating_sub(entry.fetched_at_unix_ms);
    Ok(Some(Aged {
        body: entry.body,
        age: Duration::from_millis(age_ms),
    }))
}

fn now_unix_ms() -> Result<u64, CacheError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| CacheError::Clock { source })?;
    u64::try_from(elapsed.as_millis()).map_err(|_| CacheError::Corrupt {
        key: String::from("clock"),
        detail: "the current time exceeds the supported range",
    })
}

/// The backoff for one attempt, honoring the server's Retry-After.
pub(crate) fn backoff(error: &FetchError, attempt: u32) -> Option<Duration> {
    if !error.is_transient() || attempt >= MAX_ATTEMPTS {
        return None;
    }
    if let Some(value) = error.retry_after()
        && let Ok(delay) = retry::retry_after_delay(value)
    {
        return Some(delay.min(RETRY_MAX));
    }
    let steps = attempt.min(5);
    Some(RETRY_BASE.saturating_mul(1_u32 << steps).min(RETRY_MAX))
}

/// Records one served request.
fn observe(
    state: &mut EngineState,
    spec: &RequestSpec,
    cache: CacheState,
    attempts: u32,
    bytes: usize,
    age: Option<Duration>,
) {
    let age_ms = age.and_then(|value| u64::try_from(value.as_millis()).ok());
    state.observe(
        &spec.key,
        Record {
            provenance: spec.provenance(),
            cache,
            attempts,
            bytes,
            served_at_unix_ms: now_unix_ms().ok(),
            age_ms,
        },
    );
}

/// Stores a body, reporting a cache failure without replacing live bytes.
fn store_body(state: &mut EngineState, spec: &RequestSpec, body: &[u8]) -> CacheState {
    if !spec.store {
        return CacheState::NotStored;
    }
    let Some(store) = state.store() else {
        return CacheState::NotStored;
    };
    match store.store(&spec.key, body) {
        Ok(()) => CacheState::Network,
        Err(error) => {
            state.note_cache_error(&error);
            CacheState::NotStored
        }
    }
}

/// Serves a cached body when it is younger than the source's cache lifetime.
fn cached(state: &mut EngineState, spec: &RequestSpec) -> Option<Fetched> {
    if !spec.store {
        return None;
    }
    let store = state.store()?;
    match load_aged(&store, &spec.key) {
        Ok(Some(aged)) if aged.age < spec.cache_ttl() => {
            let bytes = aged.body.len();
            observe(state, spec, CacheState::Cached, 0, bytes, Some(aged.age));
            Some(Fetched {
                body: aged.body,
                provenance: spec.provenance(),
                cache: CacheState::Cached,
            })
        }
        Ok(Some(_)) | Ok(None) => None,
        Err(error) => {
            state.note_cache_error(&error);
            None
        }
    }
}

/// Runs one request to a body: cache first, then paced network attempts.
pub(crate) fn execute(
    state: &mut EngineState,
    fetch: impl Fn(&str) -> Result<Vec<u8>, FetchError>,
    spec: &RequestSpec,
) -> Result<Fetched, FetchError> {
    if let Some(hit) = cached(state, spec) {
        return Ok(hit);
    }
    let mut attempt = 0_u32;
    loop {
        attempt = attempt.saturating_add(1);
        let wait = state.pace(spec.source);
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        match fetch(&spec.url) {
            Ok(body) => {
                let cache = store_body(state, spec, &body);
                observe(state, spec, cache, attempt, body.len(), None);
                return Ok(Fetched {
                    body,
                    provenance: spec.provenance(),
                    cache,
                });
            }
            Err(error) => {
                if let Some(delay) = backoff(&error, attempt) {
                    #[cfg(test)]
                    if state.immediate_retries {
                        continue;
                    }
                    std::thread::sleep(delay);
                    continue;
                }
                observe(state, spec, CacheState::NotStored, attempt, 0, None);
                return Err(error);
            }
        }
    }
}

/// Runs one typed read off the UI thread and reports its single answer.
///
/// The read itself goes through the engine, so the worker owns no retry, cache
/// or pacing state of its own. Dropping the receiver abandons the answer.
pub(crate) fn task<T, E>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Receiver<Result<T, E>>
where
    T: Send + 'static,
    E: Send + 'static,
{
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        // A dropped receiver abandons the answer; there is no one to report to.
        if sender.send(work()).is_err() {}
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::sources;

    fn temporary_store(name: &str) -> Store {
        let path = std::env::temp_dir().join(format!(
            "oxrad-engine-{name}-{}-{}",
            std::process::id(),
            now_unix_ms().map_or(0, |now| now)
        ));
        Store::at(path)
    }

    fn status(code: u16) -> FetchError {
        FetchError::Status {
            url: String::from("https://example.test/document"),
            status: code,
            retry_after: None,
        }
    }

    fn spec() -> RequestSpec {
        RequestSpec::for_source(
            &sources::NIFC_WFIGS,
            "https://example.test/document",
            "application/json",
        )
    }

    #[test]
    fn backoff_grows_and_stops_at_the_attempt_limit() {
        assert_eq!(backoff(&status(503), 1), Some(Duration::from_secs(4)));
        assert_eq!(backoff(&status(503), 2), Some(Duration::from_secs(8)));
        assert_eq!(backoff(&status(503), 4), None);
        assert_eq!(backoff(&status(404), 1), None);
    }

    #[test]
    fn a_transient_failure_is_retried_until_a_body_arrives() -> Result<(), FetchError> {
        let mut state = EngineState::test_with_store(temporary_store("retry"));
        let calls = AtomicU32::new(0);
        let fetched = execute(
            &mut state,
            |_url| {
                let attempt = calls.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err(status(503))
                } else {
                    Ok(br#"{"ok":true}"#.to_vec())
                }
            },
            &spec().without_store(),
        )?;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(fetched.cache, CacheState::NotStored);
        let record = state
            .records()
            .get("https://example.test/document")
            .ok_or(FetchError::WorkerStopped)?;
        assert_eq!(record.attempts, 3);
        Ok(())
    }

    #[test]
    fn a_fresh_cache_entry_is_served_without_a_second_request() -> Result<(), FetchError> {
        let store = temporary_store("cache");
        let mut state = EngineState::test_with_store(store.clone());
        let calls = AtomicU32::new(0);
        let fetch = |_url: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(br#"{"value":7}"#.to_vec())
        };
        let first = execute(&mut state, fetch, &spec())?;
        assert_eq!(first.cache, CacheState::Network);
        let second = execute(&mut state, fetch, &spec())?;
        assert_eq!(second.cache, CacheState::Cached);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn every_record_carries_its_station_product_scan_and_credit() -> Result<(), FetchError> {
        let mut state = EngineState::test_with_store(temporary_store("provenance"));
        let request = spec()
            .station("KTWX")
            .product("reflectivity")
            .scan(1_793_600_000_000)
            .without_store();
        execute(&mut state, |_url| Ok(b"{}".to_vec()), &request)?;
        let report = state.provenance_json();
        let entry = &report["requests"][0];
        assert_eq!(entry["source"], "nifc-wfigs");
        assert_eq!(entry["station"], "KTWX");
        assert_eq!(entry["product"], "reflectivity");
        assert_eq!(entry["scan_unix_ms"], 1_793_600_000_000_u64);
        assert_eq!(entry["credit"], sources::NIFC_WFIGS.credit);
        assert_eq!(entry["url"], "https://example.test/document");
        assert_eq!(entry["attempts"], 1);
        assert_eq!(
            entry["rate_limit_seconds"],
            sources::NIFC_WFIGS.rate_limit_seconds
        );
        Ok(())
    }

    #[test]
    fn a_served_request_reports_its_credit_in_the_status_summary() -> Result<(), FetchError> {
        let mut state = EngineState::test_with_store(temporary_store("summary"));
        execute(
            &mut state,
            |_url| Ok(b"{}".to_vec()),
            &spec().without_store(),
        )?;
        let summary = state.summary().ok_or(FetchError::WorkerStopped)?;
        assert!(summary.contains(sources::NIFC_WFIGS.credit));
        assert!(summary.contains("1 attempt"));
        Ok(())
    }
}
