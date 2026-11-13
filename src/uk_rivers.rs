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
//! England's real-time river levels from the Environment Agency.
//!
//! The service publishes the latest value for each measure, with the station,
//! qualifier and unit in the same response when it is asked for the full view.
//! A level is relative to the station's local datum, so the unit and qualifier
//! are carried exactly as published rather than converted.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{Receiver, TryRecvError};

use serde_json::Value;

use crate::acquire::Acquire;
use crate::engine::Served;
use crate::provider::FetchError;
use crate::sources;
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Environment Agency flood-monitoring";

/// How many latest readings one request asks for.
pub const LIMIT: &str = "12";

/// One latest level reading.
#[derive(Debug, Clone, PartialEq)]
pub struct UkReading {
    pub station: String,
    pub station_reference: String,
    pub measure: String,
    pub qualifier: Option<String>,
    pub unit: String,
    pub value: f64,
    pub at_ms: u64,
}

/// The readings the service returned, with the records it could not read.
#[derive(Debug, Clone, PartialEq)]
pub struct UkReadings {
    pub readings: Vec<UkReading>,
    pub rejected: usize,
}

impl UkReadings {
    /// The newest reading, if the feed holds one.
    pub fn newest(&self) -> Option<&UkReading> {
        self.readings.iter().max_by_key(|reading| reading.at_ms)
    }
}

/// Why a reading response could not be used.
#[derive(Debug)]
pub enum UkRiverError {
    Provider { source: FetchError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for UkRiverError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The Environment Agency request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The Environment Agency response is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => formatter.write_str(
                "The Environment Agency worker stopped, so no river levels can be requested.",
            ),
        }
    }
}

impl Error for UkRiverError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

const fn shape(detail: &'static str) -> UkRiverError {
    UkRiverError::Shape { detail }
}

fn text(row: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}

fn reading_of(item: &Value) -> Option<UkReading> {
    let row = item.as_object()?;
    let measure = row.get("measure")?.as_object()?;
    let station = measure.get("station")?.as_object()?;
    let value = row
        .get("value")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())?;
    let at_ms = row
        .get("dateTime")
        .and_then(Value::as_str)
        .and_then(time::parse_iso_utc)?;
    Some(UkReading {
        station: text(station, "label")?,
        station_reference: text(measure, "stationReference")?,
        measure: text(measure, "@id")?,
        qualifier: text(measure, "qualifier"),
        unit: text(measure, "unitName")?,
        value,
        at_ms,
    })
}

/// Reads the service's latest-reading response.
pub fn parse(document: &Value) -> Result<UkReadings, UkRiverError> {
    let Some(items) = document.get("items").and_then(Value::as_array) else {
        return Err(shape("a list of readings"));
    };
    let mut readings = Vec::with_capacity(items.len());
    let mut rejected = 0_usize;
    for item in items {
        match reading_of(item) {
            Some(reading) => readings.push(reading),
            None => rejected += 1,
        }
    }
    readings.sort_by(|left, right| {
        right
            .at_ms
            .cmp(&left.at_ms)
            .then_with(|| left.station.cmp(&right.station))
    });
    Ok(UkReadings { readings, rejected })
}

/// Fetches the latest readings through the request engine.
pub fn fetch(acquire: &Acquire) -> Result<Served<UkReadings>, UkRiverError> {
    let fetched = acquire
        .fetch_operation(
            &sources::UK_EA_FLOOD,
            "latest-readings",
            &[("limit", LIMIT)],
        )
        .map_err(|source| UkRiverError::Provider { source })?;
    let value = parse(
        &fetched
            .json()
            .map_err(|source| UkRiverError::Provider { source })?,
    )?;
    Ok(Served::new(value, fetched.provenance, fetched.cache))
}

/// Where the latest river-level request stands.
#[derive(Debug)]
pub enum UkRiverState {
    Idle,
    Loading {
        last: Option<Served<UkReadings>>,
    },
    Ready {
        served: Served<UkReadings>,
    },
    Failed {
        error: UkRiverError,
        last: Option<Served<UkReadings>>,
    },
}

impl UkRiverState {
    /// The most recent successful answer, however old.
    pub const fn last(&self) -> Option<&Served<UkReadings>> {
        match self {
            Self::Ready { served } => Some(served),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// Owns the one in-flight request and the last successful answer.
pub struct UkRiverView {
    acquire: Acquire,
    pending: Option<Receiver<Result<Served<UkReadings>, UkRiverError>>>,
    state: UkRiverState,
}

impl UkRiverView {
    pub const fn new(acquire: Acquire) -> Self {
        Self {
            acquire,
            pending: None,
            state: UkRiverState::Idle,
        }
    }

    pub const fn state(&self) -> &UkRiverState {
        &self.state
    }

    pub const fn readings(&self) -> Option<&Served<UkReadings>> {
        self.state.last()
    }

    /// Whether a read is already in flight.
    pub const fn started(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts one request, keeping the last answer on screen meanwhile.
    pub fn start(&mut self) {
        let last = take_last(&mut self.state);
        self.state = UkRiverState::Loading { last };
        let acquire = self.acquire.clone();
        self.pending = Some(crate::engine::task(move || fetch(&acquire)));
    }

    /// Takes in the worker's single answer.
    pub fn update(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(served)) => {
                self.state = UkRiverState::Ready { served };
                self.pending = None;
            }
            Ok(Err(error)) => {
                let last = take_last(&mut self.state);
                self.state = UkRiverState::Failed { error, last };
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let last = take_last(&mut self.state);
                self.state = UkRiverState::Failed {
                    error: UkRiverError::WorkerStopped,
                    last,
                };
                self.pending = None;
            }
        }
    }
}

fn take_last(state: &mut UkRiverState) -> Option<Served<UkReadings>> {
    match std::mem::replace(state, UkRiverState::Idle) {
        UkRiverState::Loading { last } | UkRiverState::Failed { last, .. } => last,
        UkRiverState::Ready { served } => Some(served),
        UkRiverState::Idle => None,
    }
}

/// A short sentence for the status line.
pub fn describe(readings: &UkReadings) -> String {
    let newest = readings.newest().map_or_else(
        || String::from("no reading time"),
        |reading| match time::format_utc(reading.at_ms) {
            Ok(when) => when,
            Err(error) => format!("time unavailable: {error}"),
        },
    );
    let rejected = if readings.rejected == 0 {
        String::new()
    } else {
        format!(" · {} unreadable", readings.rejected)
    };
    format!(
        "{} latest levels · newest {newest}{rejected}",
        readings.readings.len()
    )
}

/// A time label for one reading.
pub fn time_label(reading: &UkReading) -> String {
    match time::format_utc(reading.at_ms) {
        Ok(when) => when,
        Err(error) => format!("time unavailable: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        let body = r#"{
            "items": [
                {
                    "dateTime": "2026-11-13T03:15:00Z",
                    "measure": {
                        "@id": "https://environment.data.gov.uk/flood-monitoring/id/measures/1029TH-level-stage-i-15_min-mASD",
                        "qualifier": "Stage",
                        "station": {
                            "@id": "https://environment.data.gov.uk/flood-monitoring/id/stations/1029TH",
                            "label": "Bourton Dickler"
                        },
                        "stationReference": "1029TH",
                        "unitName": "mASD"
                    },
                    "value": 0.069
                },
                {
                    "dateTime": "2026-11-13T03:00:00Z",
                    "measure": {
                        "@id": "https://environment.data.gov.uk/flood-monitoring/id/measures/1029TH-level-downstage-i-15_min-mASD",
                        "qualifier": "Downstream Stage",
                        "station": {
                            "@id": "https://environment.data.gov.uk/flood-monitoring/id/stations/1029TH",
                            "label": "Bourton Dickler"
                        },
                        "stationReference": "1029TH",
                        "unitName": "mASD"
                    },
                    "value": -0.355
                },
                {
                    "dateTime": "2026-11-13T03:15:00Z",
                    "measure": {
                        "station": {"label": "No unit"}
                    },
                    "value": 2.0
                }
            ]
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    #[test]
    fn readings_keep_their_station_qualifier_unit_and_time() {
        let parsed = parse(&document());
        assert!(parsed.is_ok(), "the fixture should parse");
        if let Ok(readings) = parsed {
            assert_eq!(readings.readings.len(), 2);
            assert_eq!(readings.rejected, 1);
            let first = &readings.readings[0];
            assert_eq!(first.station, "Bourton Dickler");
            assert_eq!(first.station_reference, "1029TH");
            assert_eq!(first.qualifier.as_deref(), Some("Stage"));
            assert_eq!(first.unit, "mASD");
            assert!((first.value - 0.069).abs() < 1e-9);
            assert_eq!(first.at_ms, 1_794_539_700_000);
            assert_eq!(readings.newest().map(|reading| reading.value), Some(0.069));
        }
    }

    #[test]
    fn a_response_without_items_is_refused() {
        let document = serde_json::json!({"meta": {"publisher": "Environment Agency"}});
        assert!(matches!(
            parse(&document),
            Err(UkRiverError::Shape {
                detail: "a list of readings"
            })
        ));
    }
}
