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
//! On-demand review of recent NEXRAD volumes.
//!
//! History is deliberately not a background archive: each selected volume is
//! fetched and decoded only when the user asks for it, so a workstation can
//! inspect an earlier scan without keeping a multi-gigabyte ring in memory.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use crate::error::RadarError;
use crate::network;
use crate::radar::{self, RadarVolume};
use crate::stations::SiteId;

/// Enough scans to cover roughly half an hour of continuous volume updates.
pub const RECENT_LIMIT: usize = 8;

#[derive(Debug)]
pub enum HistoryError {
    Radar(RadarError),
    WorkerStopped,
}

impl Display for HistoryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Radar(error) => Display::fmt(error, formatter),
            Self::WorkerStopped => formatter.write_str("the radar history worker stopped"),
        }
    }
}

impl Error for HistoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Radar(error) => Some(error),
            Self::WorkerStopped => None,
        }
    }
}

impl From<RadarError> for HistoryError {
    fn from(error: RadarError) -> Self {
        Self::Radar(error)
    }
}

#[derive(Debug)]
pub enum HistoryEvent {
    Keys {
        generation: u64,
        keys: Vec<String>,
    },
    Volume {
        generation: u64,
        key: String,
        volume: RadarVolume,
    },
    Failed {
        generation: u64,
        error: HistoryError,
    },
}

enum HistoryRequest {
    Keys {
        generation: u64,
        site: SiteId,
    },
    Volume {
        generation: u64,
        site: SiteId,
        key: String,
    },
}

pub struct HistoryFeed {
    requests: Sender<HistoryRequest>,
    events: Receiver<HistoryEvent>,
    generation: u64,
}

impl Default for HistoryFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryFeed {
    pub fn new() -> Self {
        let (request_sender, requests) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        thread::spawn(move || {
            HistoryWorker {
                requests,
                events: event_sender,
            }
            .run();
        });
        Self {
            requests: request_sender,
            events,
            generation: 0,
        }
    }

    pub fn request_keys(&mut self, site: &SiteId) -> Result<u64, HistoryError> {
        self.send(HistoryRequest::Keys {
            generation: 0,
            site: site.clone(),
        })
    }

    pub fn request_volume(&mut self, site: &SiteId, key: &str) -> Result<u64, HistoryError> {
        self.send(HistoryRequest::Volume {
            generation: 0,
            site: site.clone(),
            key: key.to_owned(),
        })
    }

    pub fn poll(&self) -> Result<Option<HistoryEvent>, HistoryError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(HistoryError::WorkerStopped),
        }
    }

    fn send(&mut self, mut request: HistoryRequest) -> Result<u64, HistoryError> {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
        let generation = self.generation;
        match &mut request {
            HistoryRequest::Keys {
                generation: request_generation,
                ..
            }
            | HistoryRequest::Volume {
                generation: request_generation,
                ..
            } => *request_generation = generation,
        }
        self.requests
            .send(request)
            .map_err(|_| HistoryError::WorkerStopped)?;
        Ok(generation)
    }
}

struct HistoryWorker {
    requests: Receiver<HistoryRequest>,
    events: Sender<HistoryEvent>,
}

impl HistoryWorker {
    fn run(self) {
        let Self { requests, events } = self;
        while let Ok(request) = requests.recv() {
            let event = match request {
                HistoryRequest::Keys { generation, site } => {
                    match network::recent_volume_keys(&site, RECENT_LIMIT) {
                        Ok(keys) => HistoryEvent::Keys { generation, keys },
                        Err(error) => HistoryEvent::Failed {
                            generation,
                            error: error.into(),
                        },
                    }
                }
                HistoryRequest::Volume {
                    generation,
                    site,
                    key,
                } => match network::fetch_archive(&key) {
                    Ok(bytes) => match radar::decode_volume(&site, &key, &bytes) {
                        Ok(volume) => HistoryEvent::Volume {
                            generation,
                            key,
                            volume,
                        },
                        Err(error) => HistoryEvent::Failed {
                            generation,
                            error: error.into(),
                        },
                    },
                    Err(error) => HistoryEvent::Failed {
                        generation,
                        error: error.into(),
                    },
                },
            };
            if events.send(event).is_err() {
                return;
            }
        }
    }
}

/// A stable, human-readable label for an Archive II object key.
pub fn key_label(key: &str) -> String {
    let name = key.rsplit('/').next().unwrap_or(key);
    let Some(stamp) = name.get(4..19) else {
        return name.to_owned();
    };
    let ascii = stamp
        .bytes()
        .enumerate()
        .all(|(index, byte)| (index == 8 && byte == b'_') || (index != 8 && byte.is_ascii_digit()));
    if !ascii {
        return name.to_owned();
    }
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        stamp.get(0..4),
        stamp.get(4..6),
        stamp.get(6..8),
        stamp.get(9..11),
        stamp.get(11..13),
        stamp.get(13..15),
    ) else {
        return name.to_owned();
    };
    format!("{year}-{month}-{day} {hour}:{minute}:{second} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_keys_render_their_utc_scan_time() {
        assert_eq!(
            key_label("2026/10/30/KPAH/KPAH20261030_023529_V06"),
            "2026-10-30 02:35:29 UTC"
        );
    }

    #[test]
    fn malformed_volume_keys_keep_their_file_name() {
        assert_eq!(key_label("2026/10/30/KPAH/not-a-volume"), "not-a-volume");
        assert_eq!(
            key_label("2026/10/30/KPAH/KPAH20261030-023529_V06"),
            "KPAH20261030-023529_V06"
        );
    }
}
