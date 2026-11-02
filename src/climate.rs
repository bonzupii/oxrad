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
//! Monthly climate anomalies against the reanalysis normals.
//!
//! The archive service serves the ERA5 reanalysis as daily values back to 1940.
//! The layer asks for the days since 1991 at one point and builds its own
//! baseline from 1991 to 2020 in the same record, so an anomaly is always
//! measured against the same reanalysis it came from rather than against a
//! normal computed elsewhere.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant, SystemTimeError};

use serde_json::Value;

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo archive, ERA5 reanalysis";

const BASE: &str = "https://archive-api.open-meteo.com/v1/archive";
/// The first year of the baseline the layer computes its own normals from.
pub const BASELINE_START: i32 = 1991;
/// The last year of that baseline.
pub const BASELINE_END: i32 = 2020;
/// Era5 lags the calendar by a few days; asking past that lag avoids counting
/// missing days as a cold or dry spell.
const LAG_DAYS: i64 = 6;
/// A month counts towards a normal when it has this share of its days.
const COMPLETE_DAYS: usize = 25;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Why a climate record could not be read.
#[derive(Debug)]
pub enum ClimateError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    Clock { source: SystemTimeError },
    WorkerStopped,
}

impl Display for ClimateError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The reanalysis request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The reanalysis record is not usable: it wants {detail}."
                )
            }
            Self::Clock { source } => write!(
                formatter,
                "The system clock cannot be read, so the record's last day is unknown: {source}"
            ),
            Self::WorkerStopped => write!(
                formatter,
                "The reanalysis worker stopped, so no climate record can be requested."
            ),
        }
    }
}

impl Error for ClimateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Clock { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl ClimateError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::Clock { .. } | Self::WorkerStopped => false,
        }
    }
}

/// One day of the reanalysis at the requested point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayValue {
    /// Days since the epoch, at midnight UTC.
    pub day: i64,
    pub temperature: Option<f64>,
    pub precipitation: Option<f64>,
}

/// One calendar month of that record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonthValue {
    pub year: i32,
    pub month: u32,
    pub temperature: Option<f64>,
    pub precipitation: Option<f64>,
    /// How many days in the month carried a temperature.
    pub days: usize,
    /// Whether the month has enough days to be averaged against the baseline.
    pub complete: bool,
}

/// The baseline the anomaly is measured against, for one calendar month.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Normal {
    pub month: u32,
    pub temperature: f64,
    pub low: f64,
    pub high: f64,
    pub precipitation: Option<f64>,
    pub years: usize,
}

/// The reanalysis at one point, from the start of the baseline to the last
/// day the archive holds.
#[derive(Debug, Clone, PartialEq)]
pub struct ClimateRecord {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: Option<f64>,
    pub temperature_unit: String,
    pub precipitation_unit: String,
    pub days: Vec<DayValue>,
}

impl ClimateRecord {
    /// The record folded into calendar months, oldest first.
    pub fn months(&self) -> Vec<MonthValue> {
        let mut months: Vec<MonthValue> = Vec::new();
        let mut sum = 0.0_f64;
        let mut wet = 0.0_f64;
        let mut counted = 0_usize;
        let mut wet_days = 0_usize;
        for day in &self.days {
            let (year, month, _) = time::civil_from_days(day.day);
            let key = (year, month);
            let open = months.last().map(|value| (value.year, value.month));
            if open != Some(key) {
                if let Some(previous) = months.last_mut() {
                    previous.temperature = mean_of(sum, counted);
                    previous.precipitation = mean_of(wet, wet_days);
                    previous.days = counted;
                    previous.complete = counted >= COMPLETE_DAYS;
                }
                months.push(MonthValue {
                    year,
                    month,
                    temperature: None,
                    precipitation: None,
                    days: 0,
                    complete: false,
                });
                sum = 0.0;
                wet = 0.0;
                counted = 0;
                wet_days = 0;
            }
            if let Some(value) = day.temperature.filter(|value| value.is_finite()) {
                sum += value;
                counted += 1;
            }
            if let Some(value) = day.precipitation.filter(|value| value.is_finite()) {
                wet += value;
                wet_days += 1;
            }
        }
        if let Some(previous) = months.last_mut() {
            previous.temperature = mean_of(sum, counted);
            previous.precipitation = mean_of(wet, wet_days);
            previous.days = counted;
            previous.complete = counted >= COMPLETE_DAYS;
        }
        months
    }

    /// Every month of one calendar month across the record, oldest first.
    pub fn same_month(&self, month: u32) -> Vec<MonthValue> {
        self.months()
            .into_iter()
            .filter(|value| value.month == month)
            .collect()
    }

    /// The baseline for one calendar month, from the baseline years only.
    pub fn normal(&self, month: u32) -> Option<Normal> {
        let values: Vec<MonthValue> = self
            .same_month(month)
            .into_iter()
            .filter(|value| value.complete && (BASELINE_START..=BASELINE_END).contains(&value.year))
            .collect();
        let mut temperatures = Vec::with_capacity(values.len());
        let mut precipitations = Vec::with_capacity(values.len());
        for value in &values {
            temperatures.push(value.temperature?);
            if let Some(wet) = value.precipitation {
                precipitations.push(wet);
            }
        }
        let count = temperatures.len();
        if count == 0 {
            return None;
        }
        let total: f64 = temperatures.iter().sum();
        let low = temperatures.iter().copied().fold(f64::MAX, f64::min);
        let high = temperatures.iter().copied().fold(f64::MIN, f64::max);
        let wet_total: f64 = precipitations.iter().sum();
        let precipitation = mean_of(wet_total, precipitations.len());
        Some(Normal {
            month,
            temperature: total / count as f64,
            low,
            high,
            precipitation,
            years: count,
        })
    }

    /// The newest month with enough days to compare against the baseline.
    pub fn latest_complete_month(&self) -> Option<MonthValue> {
        self.months().into_iter().rev().find(|value| value.complete)
    }

    /// Where a month's temperature ranks among the same month in the record,
    /// counting from the warmest.
    pub fn rank(&self, month: MonthValue) -> Option<(usize, usize)> {
        let value = month.temperature?;
        let mut warmer = 0_usize;
        let mut total = 0_usize;
        for candidate in self.same_month(month.month) {
            let Some(other) = candidate.temperature else {
                continue;
            };
            total += 1;
            if other > value {
                warmer += 1;
            }
        }
        Some((warmer + 1, total))
    }
}

fn mean_of(total: f64, count: usize) -> Option<f64> {
    if count == 0 {
        return None;
    }
    Some(total / count as f64)
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> ClimateError {
    ClimateError::Shape { detail }
}

/// One daily series, with a field the provider omitted standing as missing
/// days rather than as zero.
fn read_series(
    row: &serde_json::Map<String, Value>,
    field: &str,
    count: usize,
) -> Vec<Option<f64>> {
    let Some(values) = row.get(field).and_then(Value::as_array) else {
        return vec![None; count];
    };
    values
        .iter()
        .map(|value| value.as_f64().filter(|value| value.is_finite()))
        .collect()
}

/// Reads the archive's answer into a record.
pub fn parse(document: &Value) -> Result<ClimateRecord, ClimateError> {
    let Some(latitude) = document.get("latitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let Some(longitude) = document.get("longitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let Some(daily) = document.get("daily").and_then(Value::as_object) else {
        return Err(shape("a daily record"));
    };
    let Some(times) = daily.get("time").and_then(Value::as_array) else {
        return Err(shape("an array of days"));
    };
    let count = times.len();
    let temperatures = read_series(daily, "temperature_2m_mean", count);
    let precipitations = read_series(daily, "precipitation_sum", count);
    let mut days = Vec::with_capacity(count);
    for (position, text) in times.iter().enumerate() {
        let Some(text) = text.as_str() else {
            return Err(shape("every day written as text"));
        };
        let Some(day) = day_from_text(text) else {
            return Err(shape("every day written as YYYY-MM-DD"));
        };
        days.push(DayValue {
            day,
            temperature: temperatures.get(position).copied().flatten(),
            precipitation: precipitations.get(position).copied().flatten(),
        });
    }
    if days.is_empty() {
        return Err(shape("at least one day of record"));
    }
    days.sort_by_key(|value| value.day);
    Ok(ClimateRecord {
        latitude,
        longitude,
        elevation_m: document.get("elevation").and_then(Value::as_f64),
        temperature_unit: unit_of(document, "daily_units", "temperature_2m_mean"),
        precipitation_unit: unit_of(document, "daily_units", "precipitation_sum"),
        days,
    })
}

fn unit_of(document: &Value, group: &str, field: &str) -> String {
    document
        .get(group)
        .and_then(|units| units.get(field))
        .and_then(Value::as_str)
        .map_or_else(|| String::from("unit not stated"), String::from)
}

/// Reads `YYYY-MM-DD` into days since the epoch.
fn day_from_text(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let year = year.parse::<i32>().ok()?;
    let month = month.parse::<u32>().ok()?;
    let day = day.parse::<u32>().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(time::days_from_civil(year, month, day))
}

/// The number of days a calendar month has.
const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        _ => 28,
    }
}

fn date_text(day: i64) -> String {
    let (year, month, day) = time::civil_from_days(day);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The request for one point: every day since the baseline began, up to the
/// last day the archive is expected to hold.
pub fn request_url(latitude: f64, longitude: f64, today: i64) -> String {
    let start = time::days_from_civil(BASELINE_START, 1, 1);
    let end = today - LAG_DAYS;
    format!(
        "{BASE}?latitude={latitude:.3}&longitude={longitude:.3}&start_date={}&end_date={}&daily=temperature_2m_mean,precipitation_sum&timezone=UTC",
        date_text(start),
        date_text(end)
    )
}

/// Fetches the record for one point.
pub fn fetch(latitude: f64, longitude: f64) -> Result<ClimateRecord, ClimateError> {
    let now = time::now_unix_ms().map_err(|source| ClimateError::Clock { source })?;
    let today = i64::try_from(now / time::MS_PER_DAY).map_err(|_| ClimateError::Shape {
        detail: "a clock the archive dates can follow",
    })?;
    let url = request_url(latitude, longitude, today);
    let document = provider::get_json(&url, "application/json")
        .map_err(|source| ClimateError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct ClimateRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum ClimateEvent {
    Ready {
        generation: u64,
        record: ClimateRecord,
    },
    Failed {
        generation: u64,
        error: ClimateError,
    },
}

/// Fetches the reanalysis off the UI thread.
pub struct ClimateFeed {
    requests: Sender<ClimateRequest>,
    events: Receiver<ClimateEvent>,
    generation: u64,
    stopped: bool,
}

impl ClimateFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<ClimateRequest>();
        let (event_sender, events) = mpsc::channel::<ClimateEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let outcome = fetch(request.latitude, request.longitude);
                let event = match outcome {
                    Ok(record) => ClimateEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => ClimateEvent::Failed {
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

    pub fn request(&mut self, request: ClimateRequest) -> Result<(), ClimateError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(ClimateError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<ClimateEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(ClimateEvent::Failed {
                    generation: self.generation,
                    error: ClimateError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum ClimateState {
    Idle,
    Loading {
        last: Option<ClimateRecord>,
    },
    Ready {
        record: ClimateRecord,
    },
    Failed {
        error: ClimateError,
        last: Option<ClimateRecord>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl ClimateState {
    /// The most recent record still worth drawing.
    pub const fn last(&self) -> Option<&ClimateRecord> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct ClimateView {
    feed: ClimateFeed,
    state: ClimateState,
    generation: u64,
}

impl ClimateView {
    pub fn new() -> Self {
        Self {
            feed: ClimateFeed::new(),
            state: ClimateState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &ClimateState {
        &self.state
    }

    pub const fn record(&self) -> Option<&ClimateRecord> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = ClimateState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = ClimateRequest {
            generation: self.generation,
            latitude,
            longitude,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = ClimateState::Failed {
                error,
                last: None,
                attempts: attempt,
                retry_due: None,
            };
        }
    }

    /// Takes in the worker's answer and returns whether a retry came due.
    pub fn update(&mut self, now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                ClimateEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = ClimateState::Ready { record };
                }
                ClimateEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = ClimateState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            ClimateState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            ClimateState::Idle | ClimateState::Loading { .. } | ClimateState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            ClimateState::Failed { attempts, .. } => *attempts,
            ClimateState::Idle | ClimateState::Loading { .. } | ClimateState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            ClimateState::Failed { retry_due, .. } => retry_due.is_some(),
            ClimateState::Idle | ClimateState::Loading { .. } | ClimateState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut ClimateState) -> Option<ClimateRecord> {
    match std::mem::replace(state, ClimateState::Idle) {
        ClimateState::Loading { last } | ClimateState::Failed { last, .. } => last,
        ClimateState::Ready { record } => Some(record),
        ClimateState::Idle => None,
    }
}

/// Exponential backoff for a request that may succeed later.
fn retry_due(transient: bool, attempts: u32) -> Option<Instant> {
    if !transient || attempts == 0 || attempts >= MAX_ATTEMPTS {
        return None;
    }
    let steps = attempts.min(4);
    let delay = RETRY_BASE.saturating_mul(1_u32 << steps).min(RETRY_MAX);
    Instant::now().checked_add(delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two complete Octobers and one complete January, enough to exercise the
    /// folding without a megabyte of fixture.
    fn document() -> Value {
        let body = r#"{
            "latitude": 39.0,
            "longitude": -96.2,
            "elevation": 347.0,
            "daily_units": {"time": "iso8601", "temperature_2m_mean": "°C", "precipitation_sum": "mm"},
            "daily": {
                "time": [
                    "1991-10-01", "1991-10-02", "1991-10-03",
                    "1992-10-01", "1992-10-02",
                    "1991-01-01"
                ],
                "temperature_2m_mean": [12.0, 10.0, 8.0, 16.0, null, -2.0],
                "precipitation_sum": [0.0, 1.0, 0.0, 4.0, 2.0, 0.0]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn record() -> Result<ClimateRecord, ClimateError> {
        parse(&document())
    }

    #[test]
    fn a_record_folds_its_days_into_calendar_months() -> Result<(), ClimateError> {
        let record = record()?;
        let months = record.months();
        assert_eq!(months.len(), 3, "January, October 1991, October 1992");
        assert_eq!(months[0].month, 1, "the months are in day order");
        assert_eq!(months[1].temperature, Some(10.0));
        assert_eq!(months[1].days, 3);
        assert_eq!(
            months[2].days, 1,
            "the month with a missing value counts only the day it had"
        );
        assert_eq!(months[2].precipitation, Some(3.0));
        assert_eq!(record.temperature_unit, "°C");
        assert_eq!(record.elevation_m, Some(347.0));
        Ok(())
    }

    #[test]
    fn a_month_short_of_days_is_not_counted_as_complete() -> Result<(), ClimateError> {
        let record = record()?;
        assert!(record.months().iter().all(|month| !month.complete));
        assert!(record.latest_complete_month().is_none());
        Ok(())
    }

    #[test]
    fn the_normal_is_spread_over_its_baseline_years() -> Result<(), ClimateError> {
        let record = record()?;
        assert!(
            record.normal(10).is_none(),
            "two days is not a month of baseline"
        );
        Ok(())
    }

    #[test]
    fn a_short_record_still_names_its_months() -> Result<(), ClimateError> {
        let record = record()?;
        assert_eq!(record.same_month(10).len(), 2);
        assert_eq!(record.same_month(10)[0].year, 1991);
        Ok(())
    }

    #[test]
    fn a_record_without_days_is_refused() {
        let body = r#"{"latitude": 1.0, "longitude": 2.0, "daily": {"time": []}}"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(matches!(
            parse(&value),
            Err(ClimateError::Shape {
                detail: "at least one day of record"
            })
        ));
    }

    #[test]
    fn a_day_written_as_a_date_is_read_as_midnight() {
        assert_eq!(day_from_text("2026-11-01"), Some(20_758));
        assert_eq!(day_from_text("2024-02-29"), Some(19_782), "a leap day");
        assert_eq!(day_from_text("2026-11-31"), None, "November has 30 days");
        assert_eq!(day_from_text("2026-02-29"), None, "2026 is not a leap year");
        assert_eq!(day_from_text("2026-13-01"), None);
        assert_eq!(day_from_text("2026-11"), None);
    }
}
