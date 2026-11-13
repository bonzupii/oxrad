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
//! River discharge from the Open-Meteo flood service.
//!
//! The service serves the GloFAS modelled river network on a global grid, at a
//! daily step, with past days and a forecast. Its value is a modelled discharge,
//! not a gauge reading, and the rail says so.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Instant, SystemTimeError};

use serde_json::Value;

use crate::acquire::Acquire;
use crate::engine::Served;
use crate::provider::FetchError;
use crate::sources;
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo flood service, GloFAS modelled discharge";

const MAX_MEMBERS: usize = 51;

/// Why a discharge record could not be read.
#[derive(Debug)]
pub enum HydroError {
    Provider { source: FetchError },
    Shape { detail: &'static str },
    Clock { source: SystemTimeError },
    WorkerStopped,
}

impl Display for HydroError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The discharge request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The discharge record is not usable: it wants {detail}."
                )
            }
            Self::Clock { source } => write!(
                formatter,
                "The system clock cannot be read, so the discharge days have no order: {source}"
            ),
            Self::WorkerStopped => write!(
                formatter,
                "The discharge worker stopped, so no river data can be requested."
            ),
        }
    }
}

impl Error for HydroError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Clock { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl HydroError {
    /// Whether asking again may succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::Clock { .. } | Self::WorkerStopped => false,
        }
    }
}

/// One day of the record, in the provider's millisecond clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HydroDay {
    pub time_ms: u64,
    pub discharge: Option<f64>,
}

/// Daily discharge at one point.
#[derive(Debug, Clone, PartialEq)]
pub struct Discharge {
    pub latitude: f64,
    pub longitude: f64,
    pub units: String,
    pub days: Vec<HydroDay>,
    /// Ensemble member series, aligned with `days`, when they were asked for.
    pub members: Vec<Vec<Option<f64>>>,
}

impl Discharge {
    /// The newest day at or before `now_ms` that carries a value.
    pub fn observed(&self, now_ms: u64) -> Option<HydroDay> {
        self.days
            .iter()
            .rev()
            .find(|day| day.time_ms <= now_ms && day.discharge.is_some())
            .copied()
    }

    /// The strongest day in the record, which is not necessarily today.
    pub fn peak(&self) -> Option<HydroDay> {
        strongest(self.days.iter().copied())
    }

    /// The forecast peak, from the first day after `now_ms`.
    pub fn forecast_peak(&self, now_ms: u64) -> Option<HydroDay> {
        strongest(self.days.iter().copied().filter(|day| day.time_ms > now_ms))
    }

    /// The lowest and highest ensemble member for a day, when the response
    /// carried members. A spread is a range, never a probability.
    pub fn spread(&self, index: usize) -> Option<(f64, f64)> {
        let mut low: Option<f64> = None;
        let mut high: Option<f64> = None;
        for member in &self.members {
            let Some(Some(value)) = member.get(index) else {
                continue;
            };
            if !value.is_finite() {
                continue;
            }
            low = Some(low.map_or(*value, |held| held.min(*value)));
            high = Some(high.map_or(*value, |held| held.max(*value)));
        }
        match (low, high) {
            (Some(low), Some(high)) => Some((low, high)),
            _ => None,
        }
    }

    /// The widest ensemble value in the record and the day it belongs to.
    pub fn band_peak(&self) -> Option<(f64, HydroDay)> {
        let mut peak: Option<(f64, HydroDay)> = None;
        for index in 0..self.days.len() {
            let Some((_, high)) = self.spread(index) else {
                continue;
            };
            let Some(day) = self.days.get(index) else {
                continue;
            };
            peak = Some(match peak {
                Some((held, held_day)) if held >= high => (held, held_day),
                _ => (high, *day),
            });
        }
        peak
    }

    /// The first index at or after `now_ms`.
    pub fn first_after(&self, now_ms: u64) -> Option<usize> {
        self.days.iter().position(|day| day.time_ms > now_ms)
    }
}

/// The strongest day of a series, ignoring days without a value.
fn strongest(days: impl Iterator<Item = HydroDay>) -> Option<HydroDay> {
    days.filter(|day| day.discharge.is_some())
        .fold(None, |best: Option<HydroDay>, day| {
            best.map_or(Some(day), |current| {
                let higher = current
                    .discharge
                    .zip(day.discharge)
                    .is_some_and(|(held, next)| next > held);
                if higher { Some(day) } else { Some(current) }
            })
        })
}

/// A sentence for the rail: the record's span and its range.
pub fn describe(record: &Discharge) -> String {
    let Some(first) = record.days.first() else {
        return String::from("The provider sent no days.");
    };
    let Some(last) = record.days.last() else {
        return String::from("The provider sent no days.");
    };
    let start = time::format_utc(first.time_ms).unwrap_or_else(|error| format!("<{error}>"));
    let end = time::format_utc(last.time_ms).unwrap_or_else(|error| format!("<{error}>"));
    let start = start.get(..10).unwrap_or(&start);
    let end = end.get(..10).unwrap_or(&end);
    let peak = record.peak().map_or_else(
        || String::from("no day carries a value"),
        |day| {
            day.discharge.map_or_else(
                || String::from("no day carries a value"),
                |value| format!("peak {value:.2} {}", record.units),
            )
        },
    );
    format!("{} days · {start} to {end} · {peak}", record.days.len())
}
const fn shape(detail: &'static str) -> HydroError {
    HydroError::Shape { detail }
}

fn series(value: &Value, field: &'static str) -> Result<Vec<Option<f64>>, HydroError> {
    let Some(items) = value.as_array() else {
        return Err(shape(field));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::Null => out.push(None),
            Value::Number(number) => {
                let Some(value) = number.as_f64() else {
                    return Err(shape("numbers that fit a double"));
                };
                if !value.is_finite() || value < 0.0 {
                    return Err(shape("discharge values that are finite and not negative"));
                }
                out.push(Some(value));
            }
            _ => return Err(shape("numbers or nulls in the discharge series")),
        }
    }
    Ok(out)
}

/// Reads the provider's document.
///
/// A day whose date does not parse, a series of a different length, or a value
/// that is not a finite discharge rejects the whole record: a partially read
/// hydrograph would put the wrong peak in front of the user.
pub fn parse(document: &Value, _now_ms: u64) -> Result<Discharge, HydroError> {
    let Some(latitude) = document.get("latitude").and_then(Value::as_f64) else {
        return Err(shape("a latitude"));
    };
    let Some(longitude) = document.get("longitude").and_then(Value::as_f64) else {
        return Err(shape("a longitude"));
    };
    let Some(daily) = document.get("daily") else {
        return Err(shape("a daily record"));
    };
    let Some(units) = document
        .get("daily_units")
        .and_then(|units| units.get("river_discharge"))
        .and_then(Value::as_str)
    else {
        return Err(shape("the discharge unit"));
    };
    let units = units.to_owned();
    let Some(times) = daily.get("time").and_then(Value::as_array) else {
        return Err(shape("a list of days"));
    };
    let Some(discharge) = daily.get("river_discharge") else {
        return Err(shape("a river discharge series"));
    };
    let values = series(discharge, "a river discharge series")?;
    if times.is_empty() || times.len() != values.len() {
        return Err(shape("one discharge value per day"));
    }
    let mut days = Vec::with_capacity(times.len());
    for (stamp, discharge) in times.iter().zip(values) {
        let Some(text) = stamp.as_str() else {
            return Err(shape("dates written as text"));
        };
        let day = day_from_text(text)?;
        days.push(HydroDay {
            time_ms: day,
            discharge,
        });
    }
    let mut members = Vec::new();
    if let Some(object) = daily.as_object() {
        for (key, value) in object {
            if !key.starts_with("river_discharge_member") {
                continue;
            }
            if members.len() >= MAX_MEMBERS {
                break;
            }
            let member = series(value, "an ensemble member series")?;
            if member.len() != days.len() {
                return Err(shape("one value per day in every ensemble member"));
            }
            members.push(member);
        }
    }
    Ok(Discharge {
        latitude,
        longitude,
        units,
        days,
        members,
    })
}

/// Reads `YYYY-MM-DD` into milliseconds since the epoch, at midnight UTC.
fn day_from_text(text: &str) -> Result<u64, HydroError> {
    let mut parts = text.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(shape("dates written as YYYY-MM-DD"));
    };
    let Ok(year) = year.parse::<i32>() else {
        return Err(shape("a four-digit year"));
    };
    let Ok(month) = month.parse::<u32>() else {
        return Err(shape("a month number"));
    };
    let Ok(day) = day.parse::<u32>() else {
        return Err(shape("a day number"));
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(shape("a real month and day"));
    }
    let days = time::days_from_civil(year, month, day);
    let Ok(days) = u64::try_from(days) else {
        return Err(shape("a date after 1970"));
    };
    days.checked_mul(time::MS_PER_DAY)
        .ok_or(shape("a date the millisecond clock can hold"))
}

/// Fetches the record for one point.
pub fn fetch(
    acquire: &Acquire,
    latitude: f64,
    longitude: f64,
    ensemble: bool,
) -> Result<Served<Discharge>, HydroError> {
    let latitude_text = format!("{latitude:.3}");
    let longitude_text = format!("{longitude:.3}");
    let operation = if ensemble { "hydro-ensemble" } else { "hydro" };
    let fetched = acquire
        .fetch_operation(
            &sources::OPEN_METEO_FLOOD,
            operation,
            &[
                ("latitude", latitude_text.as_str()),
                ("longitude", longitude_text.as_str()),
                ("past_days", "21"),
                ("forecast_days", "7"),
            ],
        )
        .map_err(|source| HydroError::Provider { source })?;
    let document = fetched
        .json()
        .map_err(|source| HydroError::Provider { source })?;
    let now = time::now_unix_ms().map_err(|source| HydroError::Clock { source })?;
    let value = parse(&document, now)?;
    Ok(Served::new(value, fetched.provenance, fetched.cache))
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct HydroRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
    pub ensemble: bool,
}

/// Answers from the worker.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "the served value travels whole so its provenance cannot be separated from it"
)]
pub enum HydroEvent {
    Ready {
        generation: u64,
        record: Served<Discharge>,
    },
    Failed {
        generation: u64,
        error: HydroError,
    },
}

/// Fetches discharge off the UI thread.
pub struct HydroFeed {
    requests: Sender<HydroRequest>,
    events: Receiver<HydroEvent>,
    generation: u64,
    stopped: bool,
}

impl HydroFeed {
    pub fn new(acquire: Acquire) -> Self {
        let (requests, request_receiver) = mpsc::channel::<HydroRequest>();
        let (event_sender, events) = mpsc::channel::<HydroEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let outcome = fetch(
                    &acquire,
                    request.latitude,
                    request.longitude,
                    request.ensemble,
                );
                let event = match outcome {
                    Ok(record) => HydroEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => HydroEvent::Failed {
                        generation: request.generation,
                        error,
                    },
                };
                if event_sender.send(event).is_err() {
                    return;
                }
            }
        });
        Self {
            requests,
            events,
            generation: 0,
            stopped: false,
        }
    }

    pub fn request(&mut self, request: HydroRequest) -> Result<(), HydroError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(HydroError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<HydroEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(HydroEvent::Failed {
                    generation: self.generation,
                    error: HydroError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum HydroState {
    Idle,
    Loading {
        last: Option<Served<Discharge>>,
    },
    Ready {
        served: Served<Discharge>,
    },
    Failed {
        error: HydroError,
        last: Option<Served<Discharge>>,
    },
}

impl HydroState {
    /// The most recent record still worth drawing.
    pub const fn last(&self) -> Option<&Served<Discharge>> {
        match self {
            Self::Ready { served } => Some(served),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and the value it last delivered.
pub struct HydroView {
    feed: HydroFeed,
    state: HydroState,
    generation: u64,
}

impl HydroView {
    pub fn new(acquire: Acquire) -> Self {
        Self {
            feed: HydroFeed::new(acquire),
            state: HydroState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &HydroState {
        &self.state
    }

    pub fn record(&self) -> Option<&Discharge> {
        self.state.last().map(|served| &served.value)
    }

    pub const fn served(&self) -> Option<&Served<Discharge>> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, ensemble: bool) {
        let previous = std::mem::replace(&mut self.state, HydroState::Idle);
        let last = match previous {
            HydroState::Loading { last } | HydroState::Failed { last, .. } => last,
            HydroState::Ready { served } => Some(served),
            HydroState::Idle => None,
        };
        self.state = HydroState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = HydroRequest {
            generation: self.generation,
            latitude,
            longitude,
            ensemble,
        };
        if let Err(error) = self.feed.request(request) {
            let _transient = error.is_transient();
            self.state = HydroState::Failed { error, last: None };
        }
    }

    /// Takes in the worker's answers. The engine owns retry and cache.
    pub fn update(&mut self, _now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                HydroEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = HydroState::Ready { served: record };
                }
                HydroEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = match std::mem::replace(&mut self.state, HydroState::Idle) {
                        HydroState::Loading { last } | HydroState::Failed { last, .. } => last,
                        HydroState::Ready { served } => Some(served),
                        HydroState::Idle => None,
                    };
                    self.state = HydroState::Failed { error, last };
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        let body = r#"{
            "latitude": 39.025,
            "longitude": -96.225,
            "utc_offset_seconds": 0,
            "daily_units": {"time": "iso8601", "river_discharge": "m³/s"},
            "daily": {
                "time": ["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"],
                "river_discharge": [1.5, 2.0, null, 4.0]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn record() -> Result<Discharge, HydroError> {
        parse(&document(), 1_793_500_000_000)
    }

    #[test]
    fn a_record_keeps_every_day_and_its_missing_values() -> Result<(), HydroError> {
        let record = record()?;
        assert_eq!(record.days.len(), 4);
        assert_eq!(record.units, "m³/s");
        assert_eq!(
            record.days[2].discharge, None,
            "a missing day stays missing"
        );
        Ok(())
    }

    #[test]
    fn the_latest_observed_day_is_not_a_forecast() -> Result<(), HydroError> {
        let record = record()?;
        let now = record.days[2].time_ms + 3_600_000;
        let observed = record.observed(now).ok_or(HydroError::Shape {
            detail: "an observed day",
        })?;
        assert_eq!(observed.discharge, Some(2.0), "the newest day with a value");
        let forecast = record.forecast_peak(now);
        assert_eq!(forecast.and_then(|day| day.discharge), Some(4.0));
        Ok(())
    }

    #[test]
    fn the_peak_is_the_strongest_day_in_the_record() -> Result<(), HydroError> {
        let record = record()?;
        assert_eq!(
            record.peak().and_then(|day| day.discharge),
            Some(4.0),
            "the forecast day is part of the record"
        );
        Ok(())
    }

    #[test]
    fn a_short_series_is_refused() {
        let body = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "daily_units": {"river_discharge": "m³/s"},
            "daily": {"time": ["2026-10-30", "2026-10-31"], "river_discharge": [1.0]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(matches!(
            parse(&value, 0),
            Err(HydroError::Shape {
                detail: "one discharge value per day"
            })
        ));
    }

    #[test]
    fn a_negative_or_endless_discharge_is_refused() {
        for series in ["[-1.0]", "[1e999]"] {
            let body = format!(
                r#"{{"latitude": 39.0, "longitude": -96.0,
                "daily_units": {{"river_discharge": "m³/s"}},
                "daily": {{"time": ["2026-10-30"], "river_discharge": {series}}}}}"#
            );
            let Ok(value) = serde_json::from_str::<Value>(&body) else {
                continue;
            };
            assert!(parse(&value, 0).is_err(), "{series} was accepted");
        }
    }

    #[test]
    fn a_date_is_read_as_midnight_utc() -> Result<(), HydroError> {
        let day = day_from_text("2026-11-01")?;
        assert_eq!(day, 20_758 * time::MS_PER_DAY);
        assert!(day_from_text("2026-13-01").is_err());
        assert!(day_from_text("2026-11").is_err());
        Ok(())
    }

    #[test]
    fn an_ensemble_spread_is_the_range_of_its_members() -> Result<(), HydroError> {
        let body = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "daily_units": {"river_discharge": "m³/s"},
            "daily": {
                "time": ["2026-10-30", "2026-10-31"],
                "river_discharge": [1.0, 2.0],
                "river_discharge_member01": [0.5, 1.5],
                "river_discharge_member02": [3.0, null]
            }
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Err(HydroError::Shape {
                detail: "a readable test document",
            });
        };
        let record = parse(&value, 0)?;
        assert_eq!(record.members.len(), 2);
        assert_eq!(record.spread(0), Some((0.5, 3.0)));
        assert_eq!(
            record.spread(1),
            Some((1.5, 1.5)),
            "a null member is skipped"
        );
        Ok(())
    }
}
