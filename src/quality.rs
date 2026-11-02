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
//! Air quality from the Open-Meteo air-quality service.
//!
//! The service serves the CAMS global and European atmospheric-composition
//! forecasts, with a current hour and an hourly record around it. Its
//! concentrations are modelled values on a coarse grid, not a nearby monitor,
//! and the rail says so.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo, CAMS modelled concentrations";

const BASE: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";
const CURRENT_FIELDS: &str =
    "us_aqi,european_aqi,pm2_5,pm10,ozone,nitrogen_dioxide,sulphur_dioxide,carbon_monoxide";
const HOURLY_FIELDS: &str = "pm2_5,us_aqi";
const PAST_DAYS: u8 = 1;
const FORECAST_DAYS: u8 = 2;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// The United States air-quality index band a concentration falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AqiCategory {
    Good,
    Moderate,
    Sensitive,
    Unhealthy,
    VeryUnhealthy,
    Hazardous,
}

impl AqiCategory {
    /// The band a United States index value falls in.
    pub fn of(us_aqi: f64) -> Self {
        if us_aqi <= 50.0 {
            Self::Good
        } else if us_aqi <= 100.0 {
            Self::Moderate
        } else if us_aqi <= 150.0 {
            Self::Sensitive
        } else if us_aqi <= 200.0 {
            Self::Unhealthy
        } else if us_aqi <= 300.0 {
            Self::VeryUnhealthy
        } else {
            Self::Hazardous
        }
    }

    /// The band's name, as the United States index names it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Moderate => "moderate",
            Self::Sensitive => "unhealthy for sensitive groups",
            Self::Unhealthy => "unhealthy",
            Self::VeryUnhealthy => "very unhealthy",
            Self::Hazardous => "hazardous",
        }
    }

    /// The value at which the band ends.
    pub const fn top(self) -> f64 {
        match self {
            Self::Good => 50.0,
            Self::Moderate => 100.0,
            Self::Sensitive => 150.0,
            Self::Unhealthy => 200.0,
            Self::VeryUnhealthy => 300.0,
            Self::Hazardous => 500.0,
        }
    }
}

/// Why an air-quality record could not be read.
#[derive(Debug)]
pub enum AirError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for AirError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The air-quality request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The air-quality record is not usable: it wants {detail}."
                )
            }

            Self::WorkerStopped => write!(
                formatter,
                "The air-quality worker stopped, so no air data can be requested."
            ),
        }
    }
}

impl Error for AirError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl AirError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// The provider's current hour at the requested point.
#[derive(Debug, Clone, PartialEq)]
pub struct AirCurrent {
    pub time_ms: u64,
    pub us_aqi: Option<f64>,
    pub european_aqi: Option<f64>,
    pub pm2_5: Option<f64>,
    pub pm10: Option<f64>,
    pub ozone: Option<f64>,
    pub nitrogen_dioxide: Option<f64>,
    pub sulphur_dioxide: Option<f64>,
    pub carbon_monoxide: Option<f64>,
}

impl AirCurrent {
    /// The category of the current United States index, when it is present.
    pub fn category(&self) -> Option<AqiCategory> {
        self.us_aqi.map(AqiCategory::of)
    }
}

/// One hour of the provider's record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AirHour {
    pub time_ms: u64,
    pub pm2_5: Option<f64>,
    pub us_aqi: Option<f64>,
}

/// An air-quality record for one point.
#[derive(Debug, Clone, PartialEq)]
pub struct AirQuality {
    pub latitude: f64,
    pub longitude: f64,
    /// The provider's own unit string for each value, as it reported them.
    pub units: BTreeMap<String, String>,
    pub current: AirCurrent,
    pub hours: Vec<AirHour>,
}

impl AirQuality {
    /// The provider's unit for a field, or a message saying it sent none.
    pub fn unit(&self, field: &str) -> &str {
        self.units
            .get(field)
            .map_or("unit not stated", String::as_str)
    }

    /// The highest United States index in the hourly record.
    pub fn peak_index(&self) -> Option<AirHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.us_aqi.map(|aqi| (*hour, aqi)))
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(hour, _)| hour)
    }

    /// The highest index the chart's axis has to hold, never below the band
    /// where the index becomes unhealthy for sensitive groups.
    pub fn axis_top(&self) -> f64 {
        let Some(peak) = self
            .peak_index()
            .and_then(|hour| hour.us_aqi)
            .or(self.current.us_aqi)
        else {
            return AqiCategory::Sensitive.top();
        };
        AqiCategory::of(peak)
            .top()
            .max(AqiCategory::Sensitive.top())
    }
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> AirError {
    AirError::Shape { detail }
}

/// Reads an optional concentration, refusing a value that cannot exist.
fn concentration(row: &Map<String, Value>, field: &str) -> Result<Option<f64>, AirError> {
    let Some(value) = row.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(number) = value.as_f64() else {
        return Err(shape("a number for every concentration it sends"));
    };
    if !number.is_finite() || number < 0.0 {
        return Err(shape("a finite, non-negative concentration"));
    }
    Ok(Some(number))
}

/// Reads one hourly series, checking it against the hour count.
fn series(
    hourly: &Map<String, Value>,
    field: &str,
    count: usize,
) -> Result<Vec<Option<f64>>, AirError> {
    let Some(values) = hourly.get(field) else {
        return Ok(vec![None; count]);
    };
    let Some(values) = values.as_array() else {
        return Err(shape("an array for every hourly series"));
    };
    if values.len() != count {
        return Err(shape("one value per hour in every series"));
    }
    values
        .iter()
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let Some(number) = value.as_f64() else {
                return Err(shape("a number for every hourly value"));
            };
            if !number.is_finite() || number < 0.0 {
                return Err(shape("a finite, non-negative hourly value"));
            }
            Ok(Some(number))
        })
        .collect()
}

fn units_of(document: &Value) -> BTreeMap<String, String> {
    let mut units = BTreeMap::new();
    let Some(object) = document.get("current_units").and_then(Value::as_object) else {
        return units;
    };
    for (field, value) in object {
        if let Some(text) = value.as_str() {
            units.insert(field.clone(), String::from(text));
        }
    }
    units
}

fn current_of(document: &Value) -> Result<AirCurrent, AirError> {
    let Some(current) = document.get("current").and_then(Value::as_object) else {
        return Err(shape("a current reading"));
    };
    let Some(text) = current.get("time").and_then(Value::as_str) else {
        return Err(shape("a time on the current reading"));
    };
    let Some(time_ms) = time::parse_iso_utc(text) else {
        return Err(shape("the current time written as ISO 8601"));
    };
    Ok(AirCurrent {
        time_ms,
        us_aqi: concentration(current, "us_aqi")?,
        european_aqi: concentration(current, "european_aqi")?,
        pm2_5: concentration(current, "pm2_5")?,
        pm10: concentration(current, "pm10")?,
        ozone: concentration(current, "ozone")?,
        nitrogen_dioxide: concentration(current, "nitrogen_dioxide")?,
        sulphur_dioxide: concentration(current, "sulphur_dioxide")?,
        carbon_monoxide: concentration(current, "carbon_monoxide")?,
    })
}

fn hours_of(document: &Value) -> Result<Vec<AirHour>, AirError> {
    let Some(hourly) = document.get("hourly").and_then(Value::as_object) else {
        return Err(shape("an hourly record"));
    };
    let Some(times) = hourly.get("time").and_then(Value::as_array) else {
        return Err(shape("an array of hourly times"));
    };
    let count = times.len();
    let particles = series(hourly, "pm2_5", count)?;
    let index = series(hourly, "us_aqi", count)?;
    let mut hours = Vec::with_capacity(count);
    for (position, text) in times.iter().enumerate() {
        let Some(text) = text.as_str() else {
            return Err(shape("every hourly time written as text"));
        };
        let Some(time_ms) = time::parse_iso_utc(text) else {
            return Err(shape("every hourly time written as ISO 8601"));
        };
        hours.push(AirHour {
            time_ms,
            pm2_5: particles.get(position).copied().flatten(),
            us_aqi: index.get(position).copied().flatten(),
        });
    }
    Ok(hours)
}

/// Reads the provider's document into a record.
pub fn parse(document: &Value) -> Result<AirQuality, AirError> {
    let Some(latitude) = document.get("latitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let Some(longitude) = document.get("longitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let hours = hours_of(document)?;
    if hours.is_empty() {
        return Err(shape("at least one hour of record"));
    }
    Ok(AirQuality {
        latitude,
        longitude,
        units: units_of(document),
        current: current_of(document)?,
        hours,
    })
}

/// The request for one point.
pub fn request_url(latitude: f64, longitude: f64) -> String {
    format!(
        "{BASE}?latitude={latitude:.3}&longitude={longitude:.3}&current={CURRENT_FIELDS}&hourly={HOURLY_FIELDS}&timezone=UTC&past_days={PAST_DAYS}&forecast_days={FORECAST_DAYS}"
    )
}

/// Fetches the record for one point.
pub fn fetch(latitude: f64, longitude: f64) -> Result<AirQuality, AirError> {
    let url = request_url(latitude, longitude);
    let document = provider::get_json(&url, "application/json")
        .map_err(|source| AirError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct AirRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum AirEvent {
    Ready { generation: u64, record: AirQuality },
    Failed { generation: u64, error: AirError },
}

/// Fetches air quality off the UI thread.
pub struct AirFeed {
    requests: Sender<AirRequest>,
    events: Receiver<AirEvent>,
    generation: u64,
    stopped: bool,
}

impl AirFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<AirRequest>();
        let (event_sender, events) = mpsc::channel::<AirEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let outcome = fetch(request.latitude, request.longitude);
                let event = match outcome {
                    Ok(record) => AirEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => AirEvent::Failed {
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

    pub fn request(&mut self, request: AirRequest) -> Result<(), AirError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(AirError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<AirEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(AirEvent::Failed {
                    generation: self.generation,
                    error: AirError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum AirState {
    Idle,
    Loading {
        last: Option<AirQuality>,
    },
    Ready {
        record: AirQuality,
    },
    Failed {
        error: AirError,
        last: Option<AirQuality>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl AirState {
    /// The most recent record still worth drawing.
    pub const fn last(&self) -> Option<&AirQuality> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct AirView {
    feed: AirFeed,
    state: AirState,
    generation: u64,
}

impl AirView {
    pub fn new() -> Self {
        Self {
            feed: AirFeed::new(),
            state: AirState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &AirState {
        &self.state
    }

    pub const fn record(&self) -> Option<&AirQuality> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = AirState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = AirRequest {
            generation: self.generation,
            latitude,
            longitude,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = AirState::Failed {
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
                AirEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = AirState::Ready { record };
                }
                AirEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = AirState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            AirState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            AirState::Idle | AirState::Loading { .. } | AirState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            AirState::Failed { attempts, .. } => *attempts,
            AirState::Idle | AirState::Loading { .. } | AirState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            AirState::Failed { retry_due, .. } => retry_due.is_some(),
            AirState::Idle | AirState::Loading { .. } | AirState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut AirState) -> Option<AirQuality> {
    match std::mem::replace(state, AirState::Idle) {
        AirState::Loading { last } | AirState::Failed { last, .. } => last,
        AirState::Ready { record } => Some(record),
        AirState::Idle => None,
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

    fn document() -> Value {
        let body = r#"{
            "latitude": 39.0,
            "longitude": -96.25,
            "current_units": {"time": "iso8601", "us_aqi": "USAQI", "pm2_5": "μg/m³"},
            "current": {
                "time": "2026-11-02T01:00",
                "interval": 3600,
                "us_aqi": 118,
                "european_aqi": 34,
                "pm2_5": 42.5,
                "pm10": 58.0,
                "ozone": 62.0,
                "nitrogen_dioxide": 3.7,
                "sulphur_dioxide": 0.3,
                "carbon_monoxide": 148.0
            },
            "hourly_units": {"time": "iso8601", "pm2_5": "μg/m³", "us_aqi": "USAQI"},
            "hourly": {
                "time": ["2026-11-01T23:00", "2026-11-02T00:00", "2026-11-02T01:00"],
                "pm2_5": [20.0, null, 42.5],
                "us_aqi": [70, 96, 118]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn record() -> Result<AirQuality, AirError> {
        parse(&document())
    }

    #[test]
    fn a_record_keeps_each_concentration_and_the_providers_units() -> Result<(), AirError> {
        let record = record()?;
        assert_eq!(record.current.pm2_5, Some(42.5));
        assert_eq!(record.current.ozone, Some(62.0));
        assert_eq!(record.unit("pm2_5"), "μg/m³");
        assert_eq!(record.hours.len(), 3);
        assert_eq!(record.hours[1].pm2_5, None, "a missing hour stays missing");
        Ok(())
    }

    #[test]
    fn the_index_bands_follow_the_united_states_scale() {
        assert_eq!(AqiCategory::of(50.0), AqiCategory::Good);
        assert_eq!(AqiCategory::of(51.0), AqiCategory::Moderate);
        assert_eq!(AqiCategory::of(101.0), AqiCategory::Sensitive);
        assert_eq!(AqiCategory::of(151.0), AqiCategory::Unhealthy);
        assert_eq!(AqiCategory::of(299.0), AqiCategory::VeryUnhealthy);
        assert_eq!(AqiCategory::of(400.0), AqiCategory::Hazardous);
        assert_eq!(
            AqiCategory::Sensitive.label(),
            "unhealthy for sensitive groups"
        );
    }

    #[test]
    fn the_current_reading_names_its_band() -> Result<(), AirError> {
        let record = record()?;
        assert_eq!(record.current.category(), Some(AqiCategory::Sensitive));
        assert_eq!(record.current.us_aqi, Some(118.0));
        Ok(())
    }

    #[test]
    fn the_chart_axis_holds_the_worst_hour_and_at_least_the_sensitive_band() -> Result<(), AirError>
    {
        let record = record()?;
        assert_eq!(
            record.peak_index().and_then(|hour| hour.us_aqi),
            Some(118.0)
        );
        assert_eq!(record.axis_top(), 150.0);
        let quiet = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "current_units": {},
            "current": {"time": "2026-11-02T01:00", "us_aqi": 12},
            "hourly": {"time": ["2026-11-02T01:00"], "pm2_5": [3.0], "us_aqi": [12]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(quiet) else {
            return Err(shape("a readable test document"));
        };
        let record = parse(&value)?;
        assert_eq!(
            record.axis_top(),
            150.0,
            "a quiet day keeps the band visible"
        );
        Ok(())
    }

    #[test]
    fn a_negative_concentration_is_refused() {
        let body = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "current": {"time": "2026-11-02T01:00", "pm2_5": -1.0},
            "hourly": {"time": ["2026-11-02T01:00"], "pm2_5": [1.0], "us_aqi": [10]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(parse(&value).is_err());
    }

    #[test]
    fn a_series_of_the_wrong_length_is_refused() {
        let body = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "current": {"time": "2026-11-02T01:00", "us_aqi": 20},
            "hourly": {"time": ["2026-11-02T01:00", "2026-11-02T02:00"], "us_aqi": [10]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(matches!(
            parse(&value),
            Err(AirError::Shape {
                detail: "one value per hour in every series"
            })
        ));
    }

    #[test]
    fn an_hour_without_seconds_is_read_as_that_hour() -> Result<(), AirError> {
        let record = record()?;
        assert_eq!(record.current.time_ms % 3_600_000, 0);
        assert_eq!(record.hours[0].time_ms + 3_600_000, record.hours[1].time_ms);
        Ok(())
    }

    #[test]
    fn the_request_names_the_point_and_the_hours_it_wants() {
        let url = request_url(39.0, -96.2345);
        assert!(url.contains("latitude=39.000"), "{url}");
        assert!(url.contains("longitude=-96.234"), "{url}");
        assert!(url.contains("us_aqi"), "{url}");
        assert!(url.contains("timezone=UTC"), "{url}");
        assert!(url.contains("past_days=1"), "{url}");
        assert!(url.contains("forecast_days=2"), "{url}");
    }

    #[test]
    fn a_failed_request_is_retried_a_bounded_number_of_times() {
        assert!(retry_due(true, 1).is_some());
        assert!(retry_due(true, MAX_ATTEMPTS).is_none());
        assert!(retry_due(false, 1).is_none(), "a shape error is final");
    }
}
