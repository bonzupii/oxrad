// oxrad — A storm-chaser-grade NEXRAD weather radar viewer
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
use std::time::Duration;

use crate::error::RadarError;
use crate::network;
use crate::radar::{self, RadarVolume};
use crate::retry::{self, BASE_DELAY, MAX_DELAY, error_update};
use crate::stations::SiteId;

const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// A failed pipeline step and the stage it failed in.
#[derive(Debug)]
pub struct FetchFailure {
    pub operation: Operation,
    pub error: RadarError,
}

#[derive(Debug)]
pub struct FetchUpdate {
    pub result: Result<Option<RadarVolume>, FetchFailure>,
    pub retry_in: Option<Duration>,
    pub waiting_for_next_volume: bool,
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
    Update(FetchUpdate),
}

struct Archive {
    key: String,
    bytes: Vec<u8>,
}

pub struct Pipeline {
    site: SiteId,
    events: Receiver<Event>,
    retry: Sender<()>,
    pub state: PipelineState,
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
        }
    }

    pub fn poll(&mut self) -> Option<RadarVolume> {
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

    fn apply(&mut self, event: Event) -> Option<RadarVolume> {
        match event {
            Event::Working(operation) => {
                let previous = std::mem::replace(&mut self.state, PipelineState::Ready);
                self.state = PipelineState::Working {
                    operation,
                    last_failure: previous.into_failure(),
                };
                None
            }
            Event::Update(update) => match update.result {
                Ok(Some(volume)) => {
                    self.state = PipelineState::Ready;
                    Some(volume)
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
                        retry_in: update.retry_in,
                        waiting_for_next_volume: update.waiting_for_next_volume,
                    });
                    None
                }
            },
        }
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
    }
}

fn decode_failure(site: &SiteId) -> FetchFailure {
    FetchFailure {
        operation: Operation::Decoding,
        error: RadarError::DecoderStopped {
            site: String::from(site.as_str()),
        },
    }
}

fn fetch(
    state: &mut FetchState,
    site: &SiteId,
    events: &Sender<Event>,
    jobs: &Sender<Archive>,
    results: &Receiver<Result<RadarVolume, RadarError>>,
) -> Result<Option<(String, RadarVolume)>, FetchFailure> {
    let key = network::latest_volume_key(site).map_err(|error| FetchFailure {
        operation: Operation::Fetching,
        error,
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
    let bytes = network::fetch_archive(&key).map_err(|error| {
        if matches!(error, RadarError::VolumeGone { .. }) {
            state.last_key = None;
        }
        FetchFailure {
            operation: Operation::Fetching,
            error,
        }
    })?;
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
        bytes,
    })
    .map_err(|_| decode_failure(site))?;
    match results.recv().map_err(|_| decode_failure(site))? {
        Ok(volume) => Ok(Some((key, volume))),
        Err(error) => {
            if retry::is_decode_failure(&error) {
                state.failed_key = Some(key);
                state.decode_failures = state.decode_failures.saturating_add(1);
            }
            Err(FetchFailure {
                operation: Operation::Decoding,
                error,
            })
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
                Ok(Ok(Some((key, volume)))) => {
                    state.last_key = Some(key);
                    state.reset_retry();
                    FetchUpdate {
                        result: Ok(Some(volume)),
                        retry_in: None,
                        waiting_for_next_volume: false,
                    }
                }
                Ok(Ok(None)) => {
                    state.delay = BASE_DELAY;
                    state.attempt = 0;
                    FetchUpdate {
                        result: Ok(None),
                        retry_in: None,
                        waiting_for_next_volume: false,
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
                    }),
                    retry_in: Some(POLL_INTERVAL),
                    waiting_for_next_volume: false,
                },
            };
            let wait = update.retry_in;
            if events.send(Event::Update(update)).is_err() {
                return;
            }
            let timeout = wait.unwrap_or(POLL_INTERVAL);
            match retries.recv_timeout(timeout) {
                Ok(()) => state.reset_retry(),
                Err(mpsc::RecvTimeoutError::Timeout) if wait.is_some() => {
                    state.delay = state.delay.saturating_mul(2).min(MAX_DELAY);
                    state.attempt = state.attempt.saturating_add(1);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_names_read_as_actions() {
        assert_eq!(Operation::Fetching.description(), "Fetch");
        assert_eq!(Operation::Decoding.description(), "Decode");
    }

    #[test]
    fn a_failed_step_keeps_its_operation_and_error() -> Result<(), String> {
        let site = SiteId::parse("KPAH").map_err(|error| error.to_string())?;
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
