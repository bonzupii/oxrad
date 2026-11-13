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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use sha2::{Digest, Sha256};

use crate::acquire::{
    CancelToken, Checksum, TransferFailure, TransferRequest, TransferResponse, Transport,
};
use crate::cache::{CacheError, Store, Validators};
use crate::config::EnabledProviders;
use crate::provider::FetchError;
use crate::retry;
use crate::sources::{EndpointError, Refresh, Source, SourceId};

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
    /// The registry operation that produced the URL, when one did.
    pub(crate) operation: Option<&'static str>,
    pub(crate) station: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) scan_unix_ms: Option<u64>,
    /// A cache lifetime override. `None` uses the registry refresh cadence.
    pub(crate) ttl: Option<Duration>,
    pub(crate) store: bool,
    /// Whether the response has no meaning without a declared whole length or
    /// a checksum, as for a radar archive or an image.
    pub(crate) verify_whole: bool,
}

impl RequestSpec {
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
            operation: None,
            station: None,
            product: None,
            scan_unix_ms: None,
            ttl: None,
            store: true,
            verify_whole: matches!(accept, "application/octet-stream" | "application/zip")
                || accept.starts_with("image/"),
        }
    }

    /// Builds a request from a registry operation and its values.
    ///
    /// # Errors
    ///
    /// Returns a typed template failure when the operation is unknown or a
    /// value is missing, empty or unused.
    pub(crate) fn for_operation(
        source: &'static Source,
        operation: &'static str,
        values: &[(&str, &str)],
    ) -> Result<Self, FetchError> {
        let endpoint = source
            .operation(operation)
            .map_err(|error| template_error(source, operation, error))?;
        let url = endpoint
            .build(values)
            .map_err(|error| template_error(source, operation, error))?;
        let mut spec = Self::for_source(source, url, endpoint.accept);
        spec.subject = String::from(endpoint.subject);
        spec.operation = Some(endpoint.operation);
        spec.ttl = endpoint.ttl_seconds.map(Duration::from_secs);
        Ok(spec)
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

    #[cfg(test)]
    pub(crate) const fn without_store(mut self) -> Self {
        self.store = false;
        self
    }

    fn cache_ttl(&self) -> Duration {
        self.ttl.unwrap_or_else(|| match self.source.refresh {
            Refresh::Seconds(seconds) => Duration::from_secs(u64::from(seconds)),
            Refresh::ProviderDefined => Duration::from_secs(300),
        })
    }

    pub(crate) fn provenance(&self) -> Provenance {
        Provenance {
            source: self.source.id.as_str(),
            source_name: self.source.name,
            credit: self.source.credit,
            licence: self
                .source
                .licence
                .map_or("unverified", |licence| licence.name),
            licence_url: self.source.licence.map_or("", |licence| licence.url),
            operation: self.operation,
            subject: self.subject.clone(),
            station: self.station.clone(),
            product: self.product.clone(),
            scan_unix_ms: self.scan_unix_ms,
            url: self.url.clone(),
            rate_limit_seconds: self.source.rate_limit_seconds,
        }
    }
}

/// Where a served body came from and how fresh it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheState {
    /// Fetched over the network and stored when storage was available.
    Network,
    /// Served from a fresh cache entry without a request.
    Cached,
    /// Served from cache after the origin answered 304.
    Refreshed,
    /// Served from cache after revalidation or a refetch failed.
    Stale,
    /// The cache is being checked or refreshed; a display-only state.
    Refreshing,
    /// The caller asked for no storage.
    NotStored,
    /// Storage was requested but is not available or failed.
    Unavailable,
}

impl CacheState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Network => "live",
            Self::Cached => "cached",
            Self::Refreshed => "refreshed",
            Self::Stale => "stale",
            Self::Refreshing => "refreshing",
            Self::NotStored => "not stored",
            Self::Unavailable => "unavailable",
        }
    }
}

/// The request that produced a value, for display and export.
#[derive(Debug, Clone)]
pub(crate) struct Provenance {
    pub(crate) source: &'static str,
    pub(crate) source_name: &'static str,
    pub(crate) credit: &'static str,
    pub(crate) licence: &'static str,
    pub(crate) licence_url: &'static str,
    pub(crate) operation: Option<&'static str>,
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

/// One request behind a served value.
#[derive(Debug, Clone)]
pub(crate) struct SourceRecord {
    pub(crate) provenance: Provenance,
    pub(crate) cache: CacheState,
    /// What this request served when it is one of many, such as a tile's
    /// coordinates. Empty for a request that needs no separate identity.
    pub(crate) label: Option<String>,
}

/// A parsed value beside the request or requests that produced it.
#[derive(Debug, Clone)]
pub(crate) struct Served<T> {
    pub(crate) value: T,
    pub(crate) provenance: Provenance,
    pub(crate) cache: CacheState,
    /// What the primary request served when the value is one of many.
    pub(crate) label: Option<String>,
    /// Further requests a composite readout needed, such as the solar-wind
    /// pair beside the Kp index. Empty for a single-request readout.
    pub(crate) extra: Vec<SourceRecord>,
}

/// One request and the part of the displayed data it served.
pub(crate) struct RequestRef<'a> {
    pub(crate) label: Option<&'a str>,
    pub(crate) provenance: &'a Provenance,
    pub(crate) cache: CacheState,
}

impl<T> Served<T> {
    pub(crate) const fn new(value: T, provenance: Provenance, cache: CacheState) -> Self {
        Self {
            value,
            provenance,
            cache,
            label: None,
            extra: Vec::new(),
        }
    }

    /// Names the part of the displayed data the primary request served.
    pub(crate) fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Adds one more request behind the same value.
    pub(crate) fn with_request(mut self, provenance: Provenance, cache: CacheState) -> Self {
        self.extra.push(SourceRecord {
            provenance,
            cache,
            label: None,
        });
        self
    }

    /// Adds one more request and names the part of the data it served.
    pub(crate) fn with_labelled_request(
        mut self,
        label: impl Into<String>,
        provenance: Provenance,
        cache: CacheState,
    ) -> Self {
        self.extra.push(SourceRecord {
            provenance,
            cache,
            label: Some(label.into()),
        });
        self
    }

    /// A served value with stand-in provenance, for deterministic tests.
    #[cfg(test)]
    pub(crate) fn test(value: T) -> Self {
        Self::new(
            value,
            Provenance {
                source: "test",
                source_name: "Test source",
                credit: "Test credit",
                licence: "Test licence",
                licence_url: "https://example.test/licence",
                operation: Some("test"),
                subject: String::new(),
                station: None,
                product: None,
                scan_unix_ms: None,
                url: String::from("https://example.test/request"),
                rate_limit_seconds: 0,
            },
            CacheState::Network,
        )
    }

    /// Adds every request that backed another served value.
    pub(crate) fn absorb<U>(mut self, other: &Served<U>) -> Self {
        self.extra.push(SourceRecord {
            provenance: other.provenance.clone(),
            cache: other.cache,
            label: other.label.clone(),
        });
        self.extra.extend(other.extra.iter().cloned());
        self
    }

    /// Every request with the part of the data it served, primary first.
    pub(crate) fn entries(&self) -> impl Iterator<Item = RequestRef<'_>> {
        std::iter::once(RequestRef {
            label: self.label.as_deref(),
            provenance: &self.provenance,
            cache: self.cache,
        })
        .chain(self.extra.iter().map(|record| RequestRef {
            label: record.label.as_deref(),
            provenance: &record.provenance,
            cache: record.cache,
        }))
    }
}

impl<T> Deref for Served<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> DerefMut for Served<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
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

    /// Parses a collection endpoint whose provider answers 204 when it has no
    /// rows. An object endpoint keeps using [`Self::json`] and fails on an
    /// empty body.
    pub(crate) fn json_or_empty_collection(&self) -> Result<Value, FetchError> {
        if self.body.is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        self.json()
    }
}

/// One engine record: the last request with this identity and what it cost.
#[derive(Debug, Clone)]
pub(crate) struct Record {
    pub(crate) provenance: Provenance,
    pub(crate) cache: CacheState,
    pub(crate) attempts: u32,
    pub(crate) bytes: usize,
    pub(crate) resumed_from: u64,
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
            "licence": self.provenance.licence,
            "licence_url": self.provenance.licence_url,
            "operation": self.provenance.operation,
            "subject": self.provenance.subject,
            "station": self.provenance.station,
            "product": self.provenance.product,
            "scan_unix_ms": self.provenance.scan_unix_ms,
            "url": self.provenance.url,
            "cache": self.cache.label(),
            "age_ms": self.age_ms,
            "attempts": self.attempts,
            "bytes": self.bytes,
            "resumed_from": self.resumed_from,
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
    pub(crate) const fn records(&self) -> &BTreeMap<String, Record> {
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
        cancel: CancelToken,
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
/// What a transfer is doing right now, published before it completes.
#[derive(Debug, Clone)]
pub(crate) struct ActiveTransfer {
    pub(crate) subject: String,
    pub(crate) cache: CacheState,
    pub(crate) received: u64,
    pub(crate) total: Option<u64>,
    pub(crate) resumed_from: u64,
}

impl ActiveTransfer {
    pub(crate) fn describe(&self) -> String {
        let total = self
            .total
            .map_or_else(String::new, |total| format!(" / {total}"));
        let resumed = if self.resumed_from > 0 {
            format!(" · resumed from {}", self.resumed_from)
        } else {
            String::new()
        };
        format!(
            "{} · {} · {}{}{}",
            self.subject,
            self.cache.label(),
            self.received,
            total,
            resumed
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Status {
    pub(crate) requests: usize,
    pub(crate) summary: Option<String>,
    pub(crate) cache_error: Option<String>,
    pub(crate) active: Option<ActiveTransfer>,
}

/// The receiving end of the engine's status snapshots.
pub(crate) struct EngineStatus {
    receiver: Receiver<Status>,
    last: Option<Status>,
}

impl EngineStatus {
    pub(crate) const fn new(receiver: Receiver<Status>) -> Self {
        Self {
            receiver,
            last: None,
        }
    }

    /// Returns the newest snapshot and retains it for later frames.
    pub(crate) fn latest(&mut self) -> Option<&Status> {
        while let Ok(status) = self.receiver.try_recv() {
            self.last = Some(status);
        }
        self.last.as_ref()
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
            Command::Fetch {
                spec,
                cancel,
                reply,
            } => {
                let result = state.fetch(&spec, enabled, transport, &cancel, updates);
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
            active: None,
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
        cancel: &CancelToken,
        updates: Option<&Sender<Status>>,
    ) -> Result<Fetched, FetchError> {
        let requests = self.records.len();
        let mut progress = |update: TransferProgress| {
            publish_progress(updates, requests, spec, update);
        };
        execute(
            self,
            |mut request| transport.get(enabled, spec.source, &mut request),
            spec,
            cancel,
            &mut progress,
        )
    }

    fn summary(&self) -> Option<String> {
        self.latest().map(|(_, record)| {
            let age = record.age_ms.map_or_else(String::new, |millis| {
                format!(" · cached {:.1} s ago", millis as f64 / 1000.0)
            });
            let resumed = if record.resumed_from > 0 {
                format!(" · resumed from {}", record.resumed_from)
            } else {
                String::new()
            };
            format!(
                "{} · {} · {}{}{} · {} attempt{} · {}",
                record.provenance.subject,
                record.provenance.source_name,
                record.cache.label(),
                age,
                resumed,
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
    pub(crate) validators: Validators,
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
        validators: entry.validators,
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

/// A deterministic spread for one request and attempt, so retries from a
/// fleet of clients do not arrive in lockstep.
fn jitter_seed(key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// The backoff for one attempt, honoring the server's Retry-After.
pub(crate) fn backoff(error: &FetchError, attempt: u32, seed: u64) -> Option<Duration> {
    if !error.is_transient() || attempt >= MAX_ATTEMPTS {
        return None;
    }
    if let Some(value) = error.retry_after()
        && let Ok(delay) = retry::retry_after_delay(value)
    {
        return Some(delay.min(RETRY_MAX));
    }
    let steps = attempt.min(5);
    let base = RETRY_BASE.saturating_mul(1_u32 << steps).min(RETRY_MAX);
    let spread = seed.wrapping_mul(u64::from(attempt).wrapping_add(1)) % 501;
    Some(base.saturating_add(Duration::from_millis(spread)))
}

/// One progress observation while a transfer is running.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TransferProgress {
    pub(crate) state: CacheState,
    pub(crate) received: u64,
    pub(crate) total: Option<u64>,
    pub(crate) resumed_from: u64,
}

fn publish_progress(
    updates: Option<&Sender<Status>>,
    requests: usize,
    spec: &RequestSpec,
    update: TransferProgress,
) {
    let Some(updates) = updates else {
        return;
    };
    let active = ActiveTransfer {
        subject: spec.subject.clone(),
        cache: update.state,
        received: update.received,
        total: update.total,
        resumed_from: update.resumed_from,
    };
    if updates
        .send(Status {
            requests,
            summary: Some(active.describe()),
            cache_error: None,
            active: Some(active),
        })
        .is_err()
    {}
}

/// Records one served request.
fn observe(
    state: &mut EngineState,
    spec: &RequestSpec,
    cache: CacheState,
    attempts: u32,
    bytes: usize,
    resumed_from: u64,
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
            resumed_from,
            served_at_unix_ms: now_unix_ms().ok(),
            age_ms,
        },
    );
}

/// Stores a body, reporting a cache failure without replacing live bytes.
fn store_body(
    state: &mut EngineState,
    spec: &RequestSpec,
    body: &[u8],
    validators: &Validators,
) -> CacheState {
    if !spec.store {
        return CacheState::NotStored;
    }
    let Some(store) = state.store() else {
        return CacheState::Unavailable;
    };
    match store.store_with(&spec.key, body, validators) {
        Ok(()) => CacheState::Network,
        Err(error) => {
            state.note_cache_error(&error);
            CacheState::Unavailable
        }
    }
}

/// Reads a cached entry, removing one that failed integrity checking.
fn cached_entry(state: &mut EngineState, spec: &RequestSpec) -> Option<Aged> {
    if !spec.store {
        return None;
    }
    let store = state.store()?;
    match load_aged(&store, &spec.key) {
        Ok(entry) => entry,
        Err(error) => {
            state.note_cache_error(&error);
            if let Err(remove) = store.remove(&spec.key) {
                state.note_cache_error(&remove);
            }
            None
        }
    }
}

fn validators_from(aged: Option<&Aged>) -> Validators {
    aged.map_or_else(Validators::default, |entry| entry.validators.clone())
}

fn checksum_matches(body: &[u8], checksum: &Checksum) -> bool {
    let digest = Sha256::digest(body);
    digest.as_slice() == checksum.digest()
}

fn sleep_or_cancel(delay: Duration, cancel: &CancelToken) -> bool {
    let mut remaining = delay;
    while !remaining.is_zero() {
        if cancel.is_cancelled() {
            return false;
        }
        let step = remaining.min(Duration::from_millis(100));
        std::thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    !cancel.is_cancelled()
}

/// Runs one request to a body: cache first, conditional revalidation, resumable
/// binary attempts, and a stale fallback only when the cache holds a whole body.
pub(crate) fn execute<F>(
    state: &mut EngineState,
    fetch: F,
    spec: &RequestSpec,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(TransferProgress),
) -> Result<Fetched, FetchError>
where
    F: for<'a> Fn(TransferRequest<'a>) -> Result<TransferResponse, TransferFailure>,
{
    let mut entry = cached_entry(state, spec);
    if let Some(aged) = &entry
        && aged.age < spec.cache_ttl()
    {
        let bytes = aged.body.len();
        observe(state, spec, CacheState::Cached, 0, bytes, 0, Some(aged.age));
        let value = entry.take().ok_or(FetchError::WorkerStopped)?;
        return Ok(Fetched {
            body: value.body,
            provenance: spec.provenance(),
            cache: CacheState::Cached,
        });
    }
    let mut run = TransferRun::new(validators_from(entry.as_ref()));
    let mut attempt = 0_u32;
    loop {
        if cancel.is_cancelled() {
            observe(state, spec, CacheState::Unavailable, attempt, 0, 0, None);
            return Err(FetchError::Cancelled);
        }
        attempt = attempt.saturating_add(1);
        let wait = state.pace(spec.source);
        if !wait.is_zero() && !sleep_or_cancel(wait, cancel) {
            observe(state, spec, CacheState::Unavailable, attempt, 0, 0, None);
            return Err(FetchError::Cancelled);
        }
        let offset = run.offset();
        let state_label = if entry.is_some() && run.body.is_empty() {
            CacheState::Refreshing
        } else {
            CacheState::Network
        };
        progress(TransferProgress {
            state: state_label,
            received: offset,
            total: run.total,
            resumed_from: run.resumed_from,
        });
        let mut on_read = |chunk: u64| {
            progress(TransferProgress {
                state: state_label,
                received: offset.saturating_add(chunk),
                total: run.total,
                resumed_from: run.resumed_from,
            });
        };
        let request = TransferRequest {
            url: &spec.url,
            accept: spec.accept,
            etag: run.etag(),
            last_modified: run.last_modified(),
            offset,
            cancel,
            on_read: &mut on_read,
        };
        match fetch(request) {
            Ok(response) => {
                if cancel.is_cancelled() {
                    observe(
                        state,
                        spec,
                        CacheState::Unavailable,
                        attempt,
                        run.body.len(),
                        run.resumed_from,
                        None,
                    );
                    return Err(FetchError::Cancelled);
                }
                if response.status == 304 {
                    return serve_revalidated(state, spec, entry, response, attempt);
                }
                return finish_transfer(state, spec, &mut run, response, attempt, cancel);
            }
            Err(failure) => {
                if let Some(fetched) =
                    handle_failure(state, spec, &mut run, &mut entry, failure, attempt, cancel)?
                {
                    return Ok(fetched);
                }
            }
        }
    }
}

struct TransferRun {
    body: Vec<u8>,
    total: Option<u64>,
    checksum: Option<Checksum>,
    range_supported: bool,
    resumed_from: u64,
    validators: Validators,
}

impl TransferRun {
    const fn new(validators: Validators) -> Self {
        Self {
            body: Vec::new(),
            total: None,
            checksum: None,
            range_supported: false,
            resumed_from: 0,
            validators,
        }
    }

    fn offset(&mut self) -> u64 {
        if !self.range_supported {
            return 0;
        }
        let offset = u64::try_from(self.body.len()).unwrap_or(u64::MAX);
        if offset > 0 && self.resumed_from == 0 {
            self.resumed_from = offset;
        }
        offset
    }

    fn etag(&self) -> Option<&str> {
        if self.body.is_empty() {
            self.validators.etag.as_deref()
        } else {
            None
        }
    }

    fn last_modified(&self) -> Option<&str> {
        if self.body.is_empty() {
            self.validators.last_modified.as_deref()
        } else {
            None
        }
    }

    fn absorb(&mut self, failure: &TransferFailure) {
        self.range_supported = self.range_supported || failure.accept_ranges;
        self.total = failure.total_length.or(self.total);
        self.checksum = failure.checksum.clone().or_else(|| self.checksum.clone());
        if self.range_supported && !failure.partial.is_empty() {
            self.body.extend_from_slice(&failure.partial);
        } else if !self.range_supported {
            self.body.clear();
        }
    }
}

fn serve_revalidated(
    state: &mut EngineState,
    spec: &RequestSpec,
    entry: Option<Aged>,
    response: TransferResponse,
    attempt: u32,
) -> Result<Fetched, FetchError> {
    let Some(cached) = entry else {
        observe(state, spec, CacheState::Unavailable, attempt, 0, 0, None);
        return Err(FetchError::Integrity {
            url: spec.url.clone(),
            detail: "the origin answered 304 without a cached body",
        });
    };
    let refreshed = Validators {
        etag: response.etag.or(cached.validators.etag),
        last_modified: response.last_modified.or(cached.validators.last_modified),
    };
    let cache = state.store().map_or(CacheState::Unavailable, |store| {
        match store.revalidated(&spec.key, &refreshed) {
            Ok(true) => CacheState::Refreshed,
            Ok(false) => {
                state.note_cache_error(&CacheError::Corrupt {
                    key: spec.key.clone(),
                    detail: "revalidation found no metadata",
                });
                CacheState::Cached
            }
            Err(error) => {
                state.note_cache_error(&error);
                CacheState::Cached
            }
        }
    });
    let bytes = cached.body.len();
    observe(state, spec, cache, attempt, bytes, 0, Some(Duration::ZERO));
    Ok(Fetched {
        body: cached.body,
        provenance: spec.provenance(),
        cache,
    })
}

fn finish_transfer(
    state: &mut EngineState,
    spec: &RequestSpec,
    run: &mut TransferRun,
    response: TransferResponse,
    attempt: u32,
    cancel: &CancelToken,
) -> Result<Fetched, FetchError> {
    let resumed_response = response.status == 206;
    if resumed_response {
        if response.range_start != Some(run.offset()) {
            observe(
                state,
                spec,
                CacheState::Unavailable,
                attempt,
                run.body.len(),
                run.resumed_from,
                None,
            );
            return Err(FetchError::Range {
                url: spec.url.clone(),
                detail: "the resumed response does not start at the requested offset",
            });
        }
        if let (Some(known), Some(observed)) = (run.total, response.total_length)
            && known != observed
        {
            observe(
                state,
                spec,
                CacheState::Unavailable,
                attempt,
                run.body.len(),
                run.resumed_from,
                None,
            );
            return Err(FetchError::Range {
                url: spec.url.clone(),
                detail: "the resumed response declares a different total length",
            });
        }
        run.body.extend_from_slice(&response.body);
        run.total = response.total_length.or(run.total);
        run.checksum = response.checksum.clone().or_else(|| run.checksum.clone());
    } else {
        run.body = response.body;
        run.resumed_from = 0;
        run.total = response.total_length;
        run.checksum = response.checksum.clone();
    }
    verify_run(state, spec, run, attempt)?;
    if cancel.is_cancelled() {
        observe(
            state,
            spec,
            CacheState::Unavailable,
            attempt,
            run.body.len(),
            run.resumed_from,
            None,
        );
        return Err(FetchError::Cancelled);
    }
    let stored = if resumed_response {
        Validators {
            etag: response.etag.or_else(|| run.validators.etag.clone()),
            last_modified: response
                .last_modified
                .or_else(|| run.validators.last_modified.clone()),
        }
    } else {
        Validators {
            etag: response.etag,
            last_modified: response.last_modified,
        }
    };
    let cache = store_body(state, spec, &run.body, &stored);
    observe(
        state,
        spec,
        cache,
        attempt,
        run.body.len(),
        run.resumed_from,
        None,
    );
    Ok(Fetched {
        body: std::mem::take(&mut run.body),
        provenance: spec.provenance(),
        cache,
    })
}

fn verify_run(
    state: &mut EngineState,
    spec: &RequestSpec,
    run: &TransferRun,
    attempt: u32,
) -> Result<(), FetchError> {
    let fail = |detail: &'static str| FetchError::Integrity {
        url: spec.url.clone(),
        detail,
    };
    if spec.verify_whole && run.total.is_none() && run.checksum.is_none() {
        observe(
            state,
            spec,
            CacheState::Unavailable,
            attempt,
            run.body.len(),
            run.resumed_from,
            None,
        );
        return Err(fail(
            "a binary response needs a declared length or a checksum",
        ));
    }
    if let Some(expected) = run.total
        && run.body.len() as u64 != expected
    {
        observe(
            state,
            spec,
            CacheState::Unavailable,
            attempt,
            run.body.len(),
            run.resumed_from,
            None,
        );
        return Err(fail(
            "the assembled body length does not match the declared length",
        ));
    }
    if let Some(expected) = &run.checksum
        && !checksum_matches(&run.body, expected)
    {
        observe(
            state,
            spec,
            CacheState::Unavailable,
            attempt,
            run.body.len(),
            run.resumed_from,
            None,
        );
        return Err(fail(
            "the assembled body checksum does not match the origin",
        ));
    }
    Ok(())
}

fn handle_failure(
    state: &mut EngineState,
    spec: &RequestSpec,
    run: &mut TransferRun,
    entry: &mut Option<Aged>,
    failure: TransferFailure,
    attempt: u32,
    cancel: &CancelToken,
) -> Result<Option<Fetched>, FetchError> {
    if matches!(failure.error.as_ref(), FetchError::Cancelled) || cancel.is_cancelled() {
        observe(
            state,
            spec,
            CacheState::Unavailable,
            attempt,
            run.body.len(),
            run.resumed_from,
            None,
        );
        return Err(FetchError::Cancelled);
    }
    run.absorb(&failure);
    if let Some(delay) = backoff(&failure.error, attempt, jitter_seed(&spec.key)) {
        #[cfg(test)]
        if state.immediate_retries {
            return Ok(None);
        }
        if !sleep_or_cancel(delay, cancel) {
            observe(
                state,
                spec,
                CacheState::Unavailable,
                attempt,
                run.body.len(),
                run.resumed_from,
                None,
            );
            return Err(FetchError::Cancelled);
        }
        return Ok(None);
    }
    if failure.error.is_transient()
        && let Some(cached) = entry.take()
    {
        let bytes = cached.body.len();
        observe(state, spec, CacheState::Stale, attempt, bytes, 0, None);
        return Ok(Some(Fetched {
            body: cached.body,
            provenance: spec.provenance(),
            cache: CacheState::Stale,
        }));
    }
    let cache = if spec.store {
        CacheState::Unavailable
    } else {
        CacheState::NotStored
    };
    observe(state, spec, cache, attempt, 0, run.resumed_from, None);
    Err(*failure.error)
}

const fn template_error(
    source: &'static Source,
    operation: &'static str,
    error: EndpointError,
) -> FetchError {
    FetchError::Template {
        source: source.id,
        operation,
        error,
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
    use crate::test_support::TestError;

    fn temporary_store(name: &str) -> Store {
        let path = std::env::temp_dir().join(format!(
            "oxrad-engine-{name}-{}-{}",
            std::process::id(),
            now_unix_ms().unwrap_or(0)
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

    #[test]
    fn a_served_value_lists_its_registry_requests_in_order() -> Result<(), FetchError> {
        let kp = RequestSpec::for_operation(&sources::NOAA_SWPC, "kp", &[])?;
        let wind = RequestSpec::for_operation(&sources::NOAA_SWPC, "wind", &[])?;
        let served = Served::new((), kp.provenance(), CacheState::Network)
            .with_request(wind.provenance(), CacheState::Cached);
        let requests: Vec<_> = served.entries().collect();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests.first().map(|entry| entry.provenance.url.clone()),
            Some(kp.url)
        );
        assert_eq!(
            requests
                .first()
                .and_then(|entry| entry.provenance.operation),
            Some("kp")
        );
        assert_eq!(
            requests.get(1).map(|entry| entry.provenance.url.clone()),
            Some(wind.url)
        );
        assert_eq!(
            requests.get(1).map(|entry| entry.cache),
            Some(CacheState::Cached)
        );
        assert_eq!(
            requests.first().map(|entry| entry.provenance.licence),
            Some(
                sources::NOAA_SWPC
                    .licence
                    .map_or("unverified", |licence| licence.name)
            )
        );
        assert_eq!(
            requests.first().map(|entry| entry.provenance.credit),
            Some(sources::NOAA_SWPC.credit)
        );
        Ok(())
    }

    fn spec() -> RequestSpec {
        RequestSpec::for_source(
            &sources::NIFC_WFIGS,
            "https://example.test/document",
            "application/json",
        )
    }

    fn response(body: &[u8]) -> TransferResponse {
        TransferResponse {
            status: 200,
            body: body.to_vec(),
            etag: None,
            last_modified: None,
            total_length: Some(body.len() as u64),
            range_start: None,
            checksum: None,
        }
    }

    fn failure(error: FetchError) -> TransferFailure {
        TransferFailure {
            error: Box::new(error),
            partial: Vec::new(),
            accept_ranges: false,
            total_length: None,
            checksum: None,
        }
    }

    fn run<F>(state: &mut EngineState, fetch: F, spec: &RequestSpec) -> Result<Fetched, FetchError>
    where
        F: for<'a> Fn(TransferRequest<'a>) -> Result<TransferResponse, TransferFailure>,
    {
        let cancel = CancelToken::new();
        execute(state, fetch, spec, &cancel, &mut |_update| {})
    }

    #[test]
    fn cache_states_have_distinct_reader_labels() {
        let labels = [
            CacheState::Network.label(),
            CacheState::Cached.label(),
            CacheState::Refreshed.label(),
            CacheState::Stale.label(),
            CacheState::Refreshing.label(),
            CacheState::Unavailable.label(),
        ];
        assert_eq!(labels[0], "live");
        for (index, label) in labels.iter().enumerate() {
            assert!(
                labels
                    .iter()
                    .enumerate()
                    .all(|(other, candidate)| other == index || candidate != label)
            );
        }
    }

    #[test]
    fn a_status_snapshot_is_kept_between_frames() -> Result<(), TestError> {
        let (sender, receiver) = channel();
        let mut status = EngineStatus::new(receiver);
        sender
            .send(Status {
                requests: 2,
                summary: Some(String::from("2 requests")),
                cache_error: None,
                active: None,
            })
            .map_err(|_| TestError::from("the status channel closed"))?;
        assert_eq!(status.latest().map(|snapshot| snapshot.requests), Some(2));
        assert_eq!(status.latest().map(|snapshot| snapshot.requests), Some(2));
        Ok(())
    }

    #[test]
    fn backoff_grows_with_jitter_and_stops_at_the_attempt_limit() -> Result<(), TestError> {
        let first = backoff(&status(503), 1, 7).ok_or("the first retry must wait")?;
        let second = backoff(&status(503), 2, 7).ok_or("the second retry must wait")?;
        assert!(first >= Duration::from_secs(4));
        assert!(first <= Duration::from_millis(4_500));
        assert!(second >= Duration::from_secs(8));
        assert!(second <= Duration::from_millis(8_500));
        assert_eq!(backoff(&status(503), 4, 7), None);
        assert_eq!(backoff(&status(404), 1, 7), None);
        Ok(())
    }

    #[test]
    fn a_transient_failure_is_retried_until_a_body_arrives() -> Result<(), FetchError> {
        let mut state = EngineState::test_with_store(temporary_store("retry"));
        let calls = AtomicU32::new(0);
        let fetched = run(
            &mut state,
            |_request| {
                let attempt = calls.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err(failure(status(503)))
                } else {
                    Ok(response(br#"{"ok":true}"#))
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
        let mut state = EngineState::test_with_store(temporary_store("cache"));
        let calls = AtomicU32::new(0);
        let fetch = |_request: TransferRequest<'_>| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(response(br#"{"value":7}"#))
        };
        let first = run(&mut state, fetch, &spec())?;
        assert_eq!(first.cache, CacheState::Network);
        let second = run(&mut state, fetch, &spec())?;
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
        run(&mut state, |_request| Ok(response(b"{}")), &request)?;
        let report = state.provenance_json();
        let entry = &report["requests"][0];
        assert_eq!(entry["source"], "nifc-wfigs");
        assert_eq!(entry["station"], "KTWX");
        assert_eq!(entry["product"], "reflectivity");
        assert_eq!(entry["scan_unix_ms"], 1_793_600_000_000_u64);
        assert_eq!(entry["credit"], sources::NIFC_WFIGS.credit);
        assert_eq!(entry["licence"], "US Government work, public domain");
        assert_eq!(
            entry["licence_url"],
            sources::NIFC_WFIGS
                .licence
                .map_or("", |licence| licence.url)
        );
        assert_eq!(entry["operation"], serde_json::Value::Null);
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
        run(
            &mut state,
            |_request| Ok(response(b"{}")),
            &spec().without_store(),
        )?;
        let summary = state.summary().ok_or(FetchError::WorkerStopped)?;
        assert!(summary.contains(sources::NIFC_WFIGS.credit));
        assert!(summary.contains("1 attempt"));
        Ok(())
    }

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use crate::acquire::{MAX_BINARY_BYTES, MAX_DOCUMENT_BYTES, TransferPolicy, live_get};

    fn read_request(stream: &mut TcpStream) -> Result<String, TestError> {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| TestError::from(error.to_string()))?;
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream
                .read(&mut buffer)
                .map_err(|error| TestError::from(error.to_string()))?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(bytes).map_err(|error| TestError::from(error.to_string()))
    }

    fn request_header(request: &str, name: &str) -> Option<String> {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| String::from(value.trim()))
        })
    }

    fn send(
        stream: &mut TcpStream,
        status: u16,
        extra: &str,
        body: &[u8],
    ) -> Result<(), TestError> {
        let head = format!(
            "HTTP/1.1 {status} OK\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .map_err(|error| TestError::from(error.to_string()))?;
        stream
            .write_all(body)
            .map_err(|error| TestError::from(error.to_string()))?;
        stream
            .flush()
            .map_err(|error| TestError::from(error.to_string()))
    }

    fn send_partial(
        stream: &mut TcpStream,
        full: &[u8],
        partial: usize,
        extra: &str,
    ) -> Result<(), TestError> {
        let head = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n",
            full.len()
        );
        stream
            .write_all(head.as_bytes())
            .map_err(|error| TestError::from(error.to_string()))?;
        stream
            .write_all(&full[..partial])
            .map_err(|error| TestError::from(error.to_string()))?;
        stream
            .flush()
            .map_err(|error| TestError::from(error.to_string()))
    }

    fn hex_digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn a_304_revalidation_returns_the_cached_body_without_a_second_transfer()
    -> Result<(), TestError> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            let (mut first, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let request = read_request(&mut first)?;
            if request_header(&request, "if-none-match").is_some() {
                return Err(TestError::from("the first request was conditional"));
            }
            send(
                &mut first,
                200,
                "ETag: \"v1\"\r\nLast-Modified: Wed, 12 Nov 2026 10:00:00 GMT\r\nAccept-Ranges: bytes\r\n",
                b"cached payload",
            )?;
            let (mut second, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let request = read_request(&mut second)?;
            if request_header(&request, "if-none-match").as_deref() != Some("\"v1\"") {
                return Err(TestError::from("the revalidation omitted If-None-Match"));
            }
            if request_header(&request, "if-modified-since").as_deref()
                != Some("Wed, 12 Nov 2026 10:00:00 GMT")
            {
                return Err(TestError::from(
                    "the revalidation omitted If-Modified-Since",
                ));
            }
            send(&mut second, 304, "ETag: \"v1\"\r\n", b"")
        });
        let url = format!("http://{address}/conditional");
        let mut request =
            RequestSpec::for_source(&sources::NIFC_WFIGS, url.clone(), "application/json");
        request.ttl = Some(Duration::ZERO);
        let store = temporary_store("revalidate");
        let mut state = EngineState::test_with_store(store.clone());
        let cancel = CancelToken::new();
        let policy = TransferPolicy::for_source(&sources::NIFC_WFIGS);
        let limit = MAX_DOCUMENT_BYTES;
        let fetch = |mut request: TransferRequest<'_>| live_get(&mut request, policy, limit);
        let mut progress = |_update| {};
        let first = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(first.cache, CacheState::Network);
        let mut updates = Vec::new();
        let second = {
            let mut progress = |update| updates.push(update);
            execute(&mut state, fetch, &request, &cancel, &mut progress)?
        };
        assert_eq!(second.cache, CacheState::Refreshed);
        assert_eq!(second.body, b"cached payload");
        assert!(
            updates
                .iter()
                .any(|update| update.state == CacheState::Refreshing),
            "the in-flight revalidation was not reported as refreshing"
        );
        request.ttl = Some(Duration::from_secs(3600));
        let mut progress = |_update| {};
        let third = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(third.cache, CacheState::Cached);
        server
            .join()
            .map_err(|_| TestError::from("the conditional server thread panicked"))??;
        let entry = store
            .load_aged(&url)
            .map_err(|error| TestError::from(error.to_string()))?
            .ok_or_else(|| TestError::from("the 304 lost the cached entry"))?;
        assert_eq!(entry.validators.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            entry.validators.last_modified.as_deref(),
            Some("Wed, 12 Nov 2026 10:00:00 GMT")
        );
        assert_eq!(entry.body, b"cached payload");
        Ok(())
    }

    #[test]
    fn a_dropped_binary_body_resumes_with_a_range_and_verifies_the_whole_file()
    -> Result<(), TestError> {
        let full: Vec<u8> = (0..4096).map(|index| (index % 251) as u8).collect();
        let expected = full.clone();
        let digest = hex_digest(&expected);
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server_full = full;
        let server_digest = digest;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            let (mut first, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let request = read_request(&mut first)?;
            if request_header(&request, "range").is_some() {
                return Err(TestError::from(
                    "the first request already asked for a range",
                ));
            }
            let extra = format!("Accept-Ranges: bytes\r\nX-Checksum-SHA256: {server_digest}\r\n");
            send_partial(&mut first, &server_full, 512, &extra)?;
            drop(first);
            let (mut second, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let request = read_request(&mut second)?;
            if request_header(&request, "range").as_deref() != Some("bytes=512-") {
                return Err(TestError::from("the retry did not resume at 512"));
            }
            let remaining = &server_full[512..];
            let extra = format!(
                "Content-Range: bytes 512-{}/4096\r\nX-Checksum-SHA256: {server_digest}\r\n",
                server_full.len().saturating_sub(1)
            );
            send(&mut second, 206, &extra, remaining)
        });
        let url = format!("http://{address}/volume");
        let request = RequestSpec::for_source(
            &sources::NEXRAD_LEVEL_TWO,
            url.clone(),
            "application/octet-stream",
        );
        let store = temporary_store("resume");
        let mut state = EngineState::test_with_store(store.clone());
        let cancel = CancelToken::new();
        let policy = TransferPolicy::for_source(&sources::NEXRAD_LEVEL_TWO);
        let limit = MAX_BINARY_BYTES;
        let fetch = |mut request: TransferRequest<'_>| live_get(&mut request, policy, limit);
        let mut updates = Vec::new();
        let mut progress = |update| updates.push(update);
        let fetched = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(fetched.body, expected);
        assert_eq!(fetched.cache, CacheState::Network);
        assert!(
            updates
                .iter()
                .any(|update| update.resumed_from == 512 && update.received >= 512),
            "progress did not report the resumed offset"
        );
        let record = state
            .records()
            .get(&url)
            .ok_or_else(|| TestError::from("the resumed request left no record"))?;
        assert_eq!(record.resumed_from, 512);
        server
            .join()
            .map_err(|_| TestError::from("the resume server thread panicked"))??;
        let entry = store
            .load_aged(&url)
            .map_err(|error| TestError::from(error.to_string()))?
            .ok_or_else(|| TestError::from("the resumed body was not cached"))?;
        assert_eq!(entry.body, expected);
        Ok(())
    }

    #[test]
    fn an_origin_without_range_support_restarts_cleanly() -> Result<(), TestError> {
        let full: Vec<u8> = (0..2048).map(|index| (index % 239) as u8).collect();
        let expected = full.clone();
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server_full = full;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            let (mut first, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let _request = read_request(&mut first)?;
            send_partial(&mut first, &server_full, 256, "")?;
            drop(first);
            let (mut second, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let request = read_request(&mut second)?;
            if request_header(&request, "range").is_some() {
                return Err(TestError::from(
                    "the retry asked for a range without support",
                ));
            }
            send(&mut second, 200, "", &server_full)
        });
        let url = format!("http://{address}/plain");
        let request = RequestSpec::for_source(
            &sources::NEXRAD_LEVEL_TWO,
            url.clone(),
            "application/octet-stream",
        );
        let store = temporary_store("restart");
        let mut state = EngineState::test_with_store(store.clone());
        let cancel = CancelToken::new();
        let policy = TransferPolicy::for_source(&sources::NEXRAD_LEVEL_TWO);
        let limit = MAX_BINARY_BYTES;
        let fetch = |mut request: TransferRequest<'_>| live_get(&mut request, policy, limit);
        let mut progress = |_update| {};
        let fetched = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(fetched.body, expected);
        server
            .join()
            .map_err(|_| TestError::from("the restart server thread panicked"))??;
        assert_eq!(
            store
                .load_aged(&url)
                .map_err(|error| TestError::from(error.to_string()))?
                .ok_or_else(|| TestError::from("the restarted body was not cached"))?
                .body,
            expected
        );
        Ok(())
    }

    #[test]
    fn cancelling_a_stalled_body_interrupts_the_read_and_leaves_no_cache_entry()
    -> Result<(), TestError> {
        use std::time::Instant;

        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let _request = read_request(&mut stream)?;
            send_partial(&mut stream, &[7_u8; 4096], 64, "Accept-Ranges: bytes\r\n")?;
            std::thread::sleep(Duration::from_secs(2));
            Ok(())
        });
        let url = format!("http://{address}/stalled-cancel");
        let request = RequestSpec::for_source(
            &sources::NEXRAD_LEVEL_TWO,
            url.clone(),
            "application/octet-stream",
        );
        let store = temporary_store("stalled-cancel");
        let mut state = EngineState::test_with_store(store.clone());
        let cancel = CancelToken::new();
        let (chunk_sent, chunk_received) = channel();
        let (cancel_started, cancel_time) = channel();
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || -> Result<(), TestError> {
                chunk_received
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| TestError::from("the first body chunk never arrived"))?;
                let at = Instant::now();
                cancel.cancel();
                cancel_started
                    .send(at)
                    .map_err(|_| TestError::from("the cancel observation channel closed"))
            })
        };
        let policy = TransferPolicy {
            resolve: Duration::from_secs(5),
            connect: Duration::from_secs(5),
            response: Duration::from_secs(5),
            read: Duration::from_secs(5),
            write: Duration::from_secs(5),
        };
        let limit = MAX_BINARY_BYTES;
        let fetch = |mut request: TransferRequest<'_>| live_get(&mut request, policy, limit);
        let mut latest = 0;
        let mut progress = |update: TransferProgress| {
            latest = update.received;
            if update.received >= 64 && chunk_sent.send(()).is_err() {}
        };
        let result = execute(&mut state, fetch, &request, &cancel, &mut progress);
        let cancelled_at = cancel_time
            .recv()
            .map_err(|_| TestError::from("the cancel never happened"))?;
        canceller
            .join()
            .map_err(|_| TestError::from("the cancelling thread panicked"))??;
        assert!(matches!(result, Err(FetchError::Cancelled)), "{result:?}");
        assert!(
            cancelled_at.elapsed() < Duration::from_millis(500),
            "cancellation waited {cancelled_at:?}"
        );
        assert_eq!(latest, 64, "received-byte accounting changed after cancel");
        assert!(
            store
                .load_aged(&url)
                .map_err(|error| TestError::from(error.to_string()))?
                .is_none(),
            "a cancelled stalled transfer left a readable cache entry"
        );
        server
            .join()
            .map_err(|_| TestError::from("the stalled server thread panicked"))??;
        Ok(())
    }

    #[test]
    fn a_silent_body_is_stalled_not_cancelled_or_failed() -> Result<(), TestError> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            for _ in 0..4 {
                let (mut stream, _) = listener
                    .accept()
                    .map_err(|error| TestError::from(error.to_string()))?;
                let _request = read_request(&mut stream)?;
                send_partial(&mut stream, &[9_u8; 4096], 64, "Accept-Ranges: bytes\r\n")?;
                std::thread::sleep(Duration::from_millis(250));
            }
            Ok(())
        });
        let url = format!("http://{address}/silent");
        let request = RequestSpec::for_source(
            &sources::NEXRAD_LEVEL_TWO,
            url.clone(),
            "application/octet-stream",
        );
        let store = temporary_store("stalled");
        let mut state = EngineState::test_with_store(store.clone());
        let cancel = CancelToken::new();
        let policy = TransferPolicy {
            resolve: Duration::from_secs(5),
            connect: Duration::from_secs(5),
            response: Duration::from_secs(5),
            read: Duration::from_millis(100),
            write: Duration::from_secs(5),
        };
        let limit = MAX_BINARY_BYTES;
        let fetch = |mut request: TransferRequest<'_>| live_get(&mut request, policy, limit);
        let mut progress = |_update| {};
        let result = execute(&mut state, fetch, &request, &cancel, &mut progress);
        server
            .join()
            .map_err(|_| TestError::from("the silent server thread panicked"))??;
        assert!(
            matches!(result, Err(FetchError::Stalled { .. })),
            "a silent body was not reported as stalled: {result:?}"
        );
        assert!(
            store
                .load_aged(&url)
                .map_err(|error| TestError::from(error.to_string()))?
                .is_none(),
            "a stalled transfer left a readable cache entry"
        );
        Ok(())
    }

    #[test]
    fn cancelling_a_transfer_leaves_no_cache_entry() -> Result<(), TestError> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            let (mut stream, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let _request = read_request(&mut stream)?;
            let head = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 4096\r\n\r\n";
            if stream.write_all(head.as_bytes()).is_err() {
                return Ok(());
            }
            let chunk = [7_u8; 64];
            for _ in 0..64 {
                if stream.write_all(&chunk).is_err() || stream.flush().is_err() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(16));
            }
            Ok(())
        });
        let url = format!("http://{address}/slow-body");
        let request = RequestSpec::for_source(
            &sources::NEXRAD_LEVEL_TWO,
            url.clone(),
            "application/octet-stream",
        );
        let store = temporary_store("cancel");
        let mut state = EngineState::test_with_store(store.clone());
        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        let cancelling = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            canceller.cancel();
        });
        let policy = TransferPolicy::for_source(&sources::NEXRAD_LEVEL_TWO);
        let limit = MAX_BINARY_BYTES;
        let fetch = |mut request: TransferRequest<'_>| live_get(&mut request, policy, limit);
        let mut progress = |_update| {};
        let result = execute(&mut state, fetch, &request, &cancel, &mut progress);
        cancelling
            .join()
            .map_err(|_| TestError::from("the cancelling thread panicked"))?;
        server
            .join()
            .map_err(|_| TestError::from("the cancelled server thread panicked"))??;
        assert!(matches!(result, Err(FetchError::Cancelled)));
        assert!(
            store
                .load_aged(&url)
                .map_err(|error| TestError::from(error.to_string()))?
                .is_none(),
            "a cancelled transfer left a readable cache entry"
        );
        Ok(())
    }
    #[test]
    fn a_200_revalidation_replaces_the_body_and_validators_atomically() -> Result<(), TestError> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|error| TestError::from(error.to_string()))?;
        let address = listener
            .local_addr()
            .map_err(|error| TestError::from(error.to_string()))?;
        let server = std::thread::spawn(move || -> Result<(), TestError> {
            let (mut first, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let _request = read_request(&mut first)?;
            send(
                &mut first,
                200,
                "ETag: \"old\"\r\nLast-Modified: Wed, 12 Nov 2026 09:00:00 GMT\r\n",
                b"old bytes",
            )?;
            let (mut second, _) = listener
                .accept()
                .map_err(|error| TestError::from(error.to_string()))?;
            let request = read_request(&mut second)?;
            if request_header(&request, "if-none-match").as_deref() != Some("\"old\"") {
                return Err(TestError::from("the replacement omitted If-None-Match"));
            }
            send(
                &mut second,
                200,
                "ETag: \"new\"\r\nLast-Modified: Thu, 13 Nov 2026 09:00:00 GMT\r\n",
                b"new bytes",
            )
        });
        let url = format!("http://{address}/replace");
        let mut request =
            RequestSpec::for_source(&sources::NIFC_WFIGS, url.clone(), "application/json");
        request.ttl = Some(Duration::ZERO);
        let store = temporary_store("replace");
        let mut state = EngineState::test_with_store(store.clone());
        let cancel = CancelToken::new();
        let policy = TransferPolicy::for_source(&sources::NIFC_WFIGS);
        let limit = MAX_DOCUMENT_BYTES;
        let fetch = |mut request: TransferRequest<'_>| live_get(&mut request, policy, limit);
        let mut progress = |_update| {};
        let first = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(first.cache, CacheState::Network);
        let second = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(second.cache, CacheState::Network);
        assert_eq!(second.body, b"new bytes");
        request.ttl = Some(Duration::from_secs(3600));
        let third = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(third.cache, CacheState::Cached);
        assert_eq!(third.body, b"new bytes");
        server
            .join()
            .map_err(|_| TestError::from("the replacement server thread panicked"))??;
        let entry = store
            .load_aged(&url)
            .map_err(|error| TestError::from(error.to_string()))?
            .ok_or_else(|| TestError::from("the replacement was not cached"))?;
        assert_eq!(entry.body, b"new bytes");
        assert_eq!(entry.validators.etag.as_deref(), Some("\"new\""));
        assert_eq!(
            entry.validators.last_modified.as_deref(),
            Some("Thu, 13 Nov 2026 09:00:00 GMT")
        );
        Ok(())
    }

    #[test]
    fn a_failed_revalidation_serves_a_stale_body_and_says_so() -> Result<(), TestError> {
        let url = String::from("https://example.test/stale");
        let store = temporary_store("stale");
        store
            .store_with(
                &url,
                b"stale bytes",
                &Validators {
                    etag: Some(String::from("\"v1\"")),
                    last_modified: None,
                },
            )
            .map_err(|error| TestError::from(error.to_string()))?;
        let mut state = EngineState::test_with_store(store);
        let mut request = RequestSpec::for_source(&sources::NIFC_WFIGS, url, "application/json");
        request.ttl = Some(Duration::ZERO);
        let calls = AtomicU32::new(0);
        let fetch = |_request: TransferRequest<'_>| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(failure(status(503)))
        };
        let cancel = CancelToken::new();
        let mut progress = |_update| {};
        let fetched = execute(&mut state, fetch, &request, &cancel, &mut progress)?;
        assert_eq!(fetched.cache, CacheState::Stale);
        assert_eq!(fetched.body, b"stale bytes");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        Ok(())
    }
}
