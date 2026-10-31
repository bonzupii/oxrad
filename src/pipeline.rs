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
//! The fetch and decode workers for one site, and the state they report.

use std::collections::hash_map::RandomState;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::cache::CacheError;
use crate::error::RadarError;
use crate::network;
use crate::radar::{self, ArchiveOrigin, RadarVolume, RetrievedVolume};
use crate::retry::{self, BASE_DELAY, MAX_DELAY, error_update};
use crate::stations::SiteId;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const SHUTDOWN_CHECK_INTERVAL: Duration = Duration::from_millis(250);

/// A failed pipeline step and the stage it failed in.
#[derive(Debug)]
pub struct FetchFailure {
    pub operation: Operation,
    pub error: RadarError,
    pub cache_warning: Option<Box<CacheError>>,
}

#[derive(Debug)]
pub struct FetchUpdate {
    pub result: Result<Option<RadarVolume>, FetchFailure>,
    pub retry_in: Option<Duration>,
    pub waiting_for_next_volume: bool,
    pub from_cache: bool,
    pub cache_warning: Option<CacheError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Fetching,
    Decoding,
}

impl Operation {
    pub const fn description(self) -> &'static str {
        match self {
            Self::Fetching => "Fetch",
            Self::Decoding => "Decode",
        }
    }
}

pub struct Failure {
    pub operation: Operation,
    pub error: RadarError,
    pub retry_in: Option<Duration>,
    pub waiting_for_next_volume: bool,
}

pub enum PipelineState {
    Working {
        operation: Operation,
        last_failure: Option<Failure>,
    },
    Ready,
    Failed(Failure),
}

impl PipelineState {
    pub const fn failure(&self) -> Option<&Failure> {
        match self {
            Self::Working { last_failure, .. } => last_failure.as_ref(),
            Self::Failed(failure) => Some(failure),
            Self::Ready => None,
        }
    }

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Working {
                operation: Operation::Fetching,
                ..
            } => "Fetching",
            Self::Working {
                operation: Operation::Decoding,
                ..
            } => "Decoding",
            Self::Failed(_) => "Error",
            Self::Ready => "Ready",
        }
    }

    pub const fn is_working(&self) -> bool {
        matches!(self, Self::Working { .. })
    }

    const fn operation(&self) -> Operation {
        match self {
            Self::Working { operation, .. } => *operation,
            Self::Failed(failure) => failure.operation,
            Self::Ready => Operation::Fetching,
        }
    }

    fn into_failure(self) -> Option<Failure> {
        match self {
            Self::Working { last_failure, .. } => last_failure,
            Self::Failed(failure) => Some(failure),
            Self::Ready => None,
        }
    }
}

enum Event {
    Working(Operation),
    Update(Box<FetchUpdate>),
}

struct Archive {
    key: String,
    bytes: Vec<u8>,
}

struct Fetched {
    key: String,
    volume: RadarVolume,
    from_cache: bool,
    cache_warning: Option<CacheError>,
}

pub struct Pipeline {
    site: SiteId,
    events: Receiver<Event>,
    retry: Sender<()>,
    pub state: PipelineState,
    cache_warning: Option<String>,
}

impl Pipeline {
    pub fn start(site: SiteId) -> Self {
        let (events, event_receiver) = mpsc::channel();
        let (retry, retries) = mpsc::channel();
        let (jobs, decode_jobs) = mpsc::channel();
        let (decoded, results) = mpsc::channel();
        let decoder_site = site.clone();
        std::thread::spawn(move || {
            Decoder {
                site: decoder_site,
                jobs: decode_jobs,
                results: decoded,
            }
            .run();
        });
        let fetch_site = site.clone();
        std::thread::spawn(move || {
            Fetcher {
                site: fetch_site,
                events,
                retries,
                jobs,
                results,
                state: FetchState::new(),
            }
            .run();
        });
        Self {
            site,
            events: event_receiver,
            retry,
            state: PipelineState::Working {
                operation: Operation::Fetching,
                last_failure: None,
            },
            cache_warning: None,
        }
    }

    pub fn poll(&mut self) -> Option<RetrievedVolume> {
        match self.events.try_recv() {
            Ok(event) => self.apply(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                let site = String::from(self.site.as_str());
                self.state = PipelineState::Failed(Failure {
                    operation: self.state.operation(),
                    error: RadarError::WorkerStopped { site },
                    retry_in: None,
                    waiting_for_next_volume: false,
                });
                None
            }
        }
    }

    fn apply(&mut self, event: Event) -> Option<RetrievedVolume> {
        match event {
            Event::Working(operation) => {
                let previous = std::mem::replace(&mut self.state, PipelineState::Ready);
                self.state = PipelineState::Working {
                    operation,
                    last_failure: previous.into_failure(),
                };
                None
            }
            Event::Update(update) => {
                let FetchUpdate {
                    result,
                    retry_in,
                    waiting_for_next_volume,
                    from_cache,
                    cache_warning,
                } = *update;
                if let Some(warning) = cache_warning {
                    self.cache_warning = Some(warning.to_string());
                }
                match result {
                    Ok(Some(volume)) => {
                        self.state = PipelineState::Ready;
                        Some(RetrievedVolume {
                            volume,
                            origin: ArchiveOrigin::from_cache(from_cache),
                        })
                    }
                    Ok(None) => {
                        let previous = std::mem::replace(&mut self.state, PipelineState::Ready);
                        if let Some(failure) = previous.into_failure() {
                            self.state = PipelineState::Failed(failure);
                        }
                        None
                    }
                    Err(failure) => {
                        self.state = PipelineState::Failed(Failure {
                            operation: failure.operation,
                            error: failure.error,
                            retry_in,
                            waiting_for_next_volume,
                        });
                        None
                    }
                }
            }
        }
    }

    /// Takes the most recent non-fatal cache problem, if any.
    pub const fn take_cache_warning(&mut self) -> Option<String> {
        self.cache_warning.take()
    }

    /// Asks the fetch worker to try again now.
    pub fn retry(&mut self) -> Result<(), RadarError> {
        self.retry.send(()).map_err(|_| RadarError::WorkerStopped {
            site: String::from(self.site.as_str()),
        })
    }
}

struct Decoder {
    site: SiteId,
    jobs: Receiver<Archive>,
    results: Sender<Result<RadarVolume, RadarError>>,
}

impl Decoder {
    fn run(self) {
        for archive in self.jobs {
            let result = catch_unwind(AssertUnwindSafe(|| {
                radar::decode_volume(&self.site, &archive.key, &archive.bytes)
            }))
            .unwrap_or_else(|_| {
                Err(RadarError::DecoderPanic {
                    site: String::from(self.site.as_str()),
                })
            });
            if self.results.send(result).is_err() {
                return;
            }
        }
    }
}

struct FetchState {
    last_key: Option<String>,
    failed_key: Option<String>,
    decode_failures: u8,
    delay: Duration,
    attempt: u32,
    jitter: RandomState,
}

impl FetchState {
    fn new() -> Self {
        Self {
            last_key: None,
            failed_key: None,
            decode_failures: 0,
            delay: BASE_DELAY,
            attempt: 0,
            jitter: RandomState::new(),
        }
    }

    fn reset_retry(&mut self) {
        self.failed_key = None;
        self.decode_failures = 0;
        self.delay = BASE_DELAY;
        self.attempt = 0;
    }
}

fn worker_failure(site: &SiteId, operation: Operation, error: RadarError) -> FetchFailure {
    FetchFailure {
        operation,
        error: match error {
            RadarError::WorkerStopped { .. } => RadarError::WorkerStopped {
                site: String::from(site.as_str()),
            },
            other => other,
        },
        cache_warning: None,
    }
}

fn decode_failure(site: &SiteId) -> FetchFailure {
    FetchFailure {
        operation: Operation::Decoding,
        error: RadarError::DecoderStopped {
            site: String::from(site.as_str()),
        },
        cache_warning: None,
    }
}

fn fetch(
    state: &mut FetchState,
    site: &SiteId,
    events: &Sender<Event>,
    jobs: &Sender<Archive>,
    results: &Receiver<Result<RadarVolume, RadarError>>,
) -> Result<Option<Fetched>, FetchFailure> {
    let key = network::latest_volume_key(site).map_err(|error| FetchFailure {
        operation: Operation::Fetching,
        error,
        cache_warning: None,
    })?;
    if state.last_key.as_ref() == Some(&key)
        || (state.failed_key.as_ref() == Some(&key) && state.decode_failures >= 2)
    {
        return Ok(None);
    }
    if state.failed_key.as_ref() != Some(&key) {
        state.failed_key = None;
        state.decode_failures = 0;
    }
    let archive = network::fetch_archive_cached(&key).map_err(|error| {
        if matches!(error, RadarError::VolumeGone { .. }) {
            state.last_key = None;
        }
        FetchFailure {
            operation: Operation::Fetching,
            error,
            cache_warning: None,
        }
    })?;
    let from_cache = archive.from_cache;
    let mut cache_warning = archive.cache_warning;
    if events.send(Event::Working(Operation::Decoding)).is_err() {
        return Err(worker_failure(
            site,
            Operation::Fetching,
            RadarError::WorkerStopped {
                site: String::from(site.as_str()),
            },
        ));
    }
    jobs.send(Archive {
        key: key.clone(),
        bytes: archive.bytes,
    })
    .map_err(|_| decode_failure(site))?;
    match results.recv().map_err(|_| decode_failure(site))? {
        Ok(volume) => Ok(Some(Fetched {
            key,
            volume,
            from_cache,
            cache_warning,
        })),
        Err(error) => {
            if retry::is_decode_failure(&error) {
                state.failed_key = Some(key.clone());
                state.decode_failures = state.decode_failures.saturating_add(1);
            }
            if let Err(remove_error) = network::evict_archive(&key) {
                cache_warning = Some(remove_error);
            }
            Err(FetchFailure {
                operation: Operation::Decoding,
                error,
                cache_warning: cache_warning.map(Box::new),
            })
        }
    }
}

enum WaitOutcome {
    Retry,
    Elapsed,
    Stopped,
}

/// Waits in short slices so dropping the pipeline closes the retry channel and
/// releases the worker without waiting for the full poll interval.
fn wait_for_retry(retries: &Receiver<()>, timeout: Duration) -> WaitOutcome {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return WaitOutcome::Elapsed;
        }
        match retries.recv_timeout(remaining.min(SHUTDOWN_CHECK_INTERVAL)) {
            Ok(()) => return WaitOutcome::Retry,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return WaitOutcome::Stopped,
        }
    }
}

struct Fetcher {
    site: SiteId,
    events: Sender<Event>,
    retries: Receiver<()>,
    jobs: Sender<Archive>,
    results: Receiver<Result<RadarVolume, RadarError>>,
    state: FetchState,
}

impl Fetcher {
    fn run(mut self) {
        loop {
            let Self {
                site,
                events,
                retries,
                jobs,
                results,
                state,
            } = &mut self;
            if events.send(Event::Working(Operation::Fetching)).is_err() {
                return;
            }
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                fetch(state, site, events, jobs, results)
            }));
            let update = match outcome {
                Ok(Ok(Some(fetched))) => {
                    state.last_key = Some(fetched.key);
                    state.reset_retry();
                    FetchUpdate {
                        result: Ok(Some(fetched.volume)),
                        retry_in: None,
                        waiting_for_next_volume: false,
                        from_cache: fetched.from_cache,
                        cache_warning: fetched.cache_warning,
                    }
                }
                Ok(Ok(None)) => {
                    state.delay = BASE_DELAY;
                    state.attempt = 0;
                    FetchUpdate {
                        result: Ok(None),
                        retry_in: None,
                        waiting_for_next_volume: false,
                        from_cache: false,
                        cache_warning: None,
                    }
                }
                Ok(Err(failure)) => error_update(
                    failure,
                    state.decode_failures,
                    state.delay,
                    state.attempt,
                    &state.jitter,
                ),
                Err(_) => FetchUpdate {
                    result: Err(FetchFailure {
                        operation: Operation::Fetching,
                        error: RadarError::WorkerPanic {
                            site: String::from(site.as_str()),
                        },
                        cache_warning: None,
                    }),
                    retry_in: Some(POLL_INTERVAL),
                    waiting_for_next_volume: false,
                    from_cache: false,
                    cache_warning: None,
                },
            };
            let wait = update.retry_in;
            if events.send(Event::Update(Box::new(update))).is_err() {
                return;
            }
            let timeout = wait.unwrap_or(POLL_INTERVAL);
            match wait_for_retry(retries, timeout) {
                WaitOutcome::Retry => state.reset_retry(),
                WaitOutcome::Elapsed if wait.is_some() => {
                    state.delay = state.delay.saturating_mul(2).min(MAX_DELAY);
                    state.attempt = state.attempt.saturating_add(1);
                }
                WaitOutcome::Elapsed => {}
                WaitOutcome::Stopped => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TestError;

    fn volume(site: SiteId) -> RadarVolume {
        RadarVolume {
            site,
            site_lat: 37.06833,
            site_lon: -88.77194,
            scan_unix_ms: 1_793_327_729_000,
            sweep: crate::radar::RadarSweep {
                elevation_deg: 0.5,
                elevation_number: 1,
                radials: Vec::new(),
                products: Vec::new(),
                below_threshold: 0,
                range_folded: 0,
                max_ground_range_km: 0.0,
            },
            sweeps: Vec::new(),
            skipped_radials: 0,
            skipped_non_data: 0,
            moment_error: None,
        }
    }

    #[test]
    fn cache_origin_belongs_to_the_fetched_volume() -> Result<(), TestError> {
        let site = SiteId::parse("KPAH")?;
        let (_events, event_receiver) = mpsc::channel();
        let (retry, _retries) = mpsc::channel();
        let mut pipeline = Pipeline {
            site: site.clone(),
            events: event_receiver,
            retry,
            state: PipelineState::Ready,
            cache_warning: None,
        };
        let fetched = pipeline
            .apply(Event::Update(Box::new(FetchUpdate {
                result: Ok(Some(volume(site))),
                retry_in: None,
                waiting_for_next_volume: false,
                from_cache: true,
                cache_warning: None,
            })))
            .ok_or("cached volume not returned")?;
        assert_eq!(fetched.origin, ArchiveOrigin::Cached);
        assert!(
            pipeline
                .apply(Event::Working(Operation::Fetching))
                .is_none()
        );
        assert_eq!(fetched.origin, ArchiveOrigin::Cached);
        assert!(
            pipeline
                .apply(Event::Update(Box::new(FetchUpdate {
                    result: Ok(None),
                    retry_in: None,
                    waiting_for_next_volume: false,
                    from_cache: false,
                    cache_warning: None,
                })))
                .is_none()
        );
        assert_eq!(fetched.origin, ArchiveOrigin::Cached);
        Ok(())
    }

    #[test]
    fn operation_names_read_as_actions() {
        assert_eq!(Operation::Fetching.description(), "Fetch");
        assert_eq!(Operation::Decoding.description(), "Decode");
    }

    #[test]
    fn dropping_the_retry_channel_releases_a_long_wait() -> Result<(), TestError> {
        let (sender, receiver) = mpsc::channel::<()>();
        drop(sender);
        let started = Instant::now();
        assert!(matches!(
            wait_for_retry(&receiver, Duration::from_secs(30)),
            WaitOutcome::Stopped
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn a_failed_step_keeps_its_operation_and_error() -> Result<(), TestError> {
        let site = SiteId::parse("KPAH")?;
        let failed = worker_failure(
            &site,
            Operation::Fetching,
            RadarError::WorkerStopped {
                site: String::from("KPAH"),
            },
        );
        assert_eq!(failed.operation, Operation::Fetching);
        assert!(matches!(
            failed.error,
            RadarError::WorkerStopped { ref site } if site == "KPAH"
        ));
        Ok(())
    }
}
