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
//! Sea state from the Open-Meteo marine service.
//!
//! The service combines the MeteoFrance and ECMWF wave models with a global
//! current and sea-surface temperature analysis. Its wave directions are the
//! direction the waves come from, while its current direction is the direction
//! the water is heading towards, and the rail words each of them accordingly.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo marine, MeteoFrance and ECMWF waves";

const BASE: &str = "https://marine-api.open-meteo.com/v1/marine";
const HOURLY: &str = "wave_height,wave_direction,wave_period,swell_wave_height,sea_surface_temperature,ocean_current_velocity,ocean_current_direction";
const FORECAST_DAYS: u8 = 3;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// The wave height at which the sea becomes rough under the Douglas scale.
pub const ROUGH: f64 = 2.5;
/// The wave height at which it becomes moderate.
pub const MODERATE: f64 = 1.25;

/// The Douglas sea state a wave height falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeaState {
    Calm,
    Smooth,
    Slight,
    Moderate,
    Rough,
    VeryRough,
    High,
    VeryHigh,
    Phenomenal,
}

impl SeaState {
    /// The state a significant wave height falls in.
    pub fn of(height_m: f64) -> Self {
        if height_m < 0.1 {
            Self::Calm
        } else if height_m < 0.5 {
            Self::Smooth
        } else if height_m < MODERATE {
            Self::Slight
        } else if height_m < ROUGH {
            Self::Moderate
        } else if height_m < 4.0 {
            Self::Rough
        } else if height_m < 6.0 {
            Self::VeryRough
        } else if height_m < 9.0 {
            Self::High
        } else if height_m < 14.0 {
            Self::VeryHigh
        } else {
            Self::Phenomenal
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Calm => "calm",
            Self::Smooth => "smooth",
            Self::Slight => "slight",
            Self::Moderate => "moderate",
            Self::Rough => "rough",
            Self::VeryRough => "very rough",
            Self::High => "high",
            Self::VeryHigh => "very high",
            Self::Phenomenal => "phenomenal",
        }
    }

    /// The height at which the state ends.
    pub const fn top(self) -> f64 {
        match self {
            Self::Calm => 0.1,
            Self::Smooth => 0.5,
            Self::Slight => MODERATE,
            Self::Moderate => ROUGH,
            Self::Rough => 4.0,
            Self::VeryRough => 6.0,
            Self::High => 9.0,
            Self::VeryHigh => 14.0,
            Self::Phenomenal => 20.0,
        }
    }
}

/// Why a marine record could not be read.
#[derive(Debug)]
pub enum OceanError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for OceanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The marine request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The marine record is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => write!(
                formatter,
                "The marine worker stopped, so no sea state can be requested."
            ),
        }
    }
}

impl Error for OceanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl OceanError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// One hour of the marine forecast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OceanHour {
    pub time_ms: u64,
    pub wave_height: Option<f64>,
    /// The direction the waves come from.
    pub wave_from_deg: Option<f64>,
    pub wave_period_s: Option<f64>,
    pub swell_height: Option<f64>,
    pub sea_temperature: Option<f64>,
    pub current_kmh: Option<f64>,
    /// The direction the water is heading towards.
    pub current_toward_deg: Option<f64>,
}

impl OceanHour {
    /// The Douglas state of the hour, when it reports a wave height.
    pub fn sea_state(&self) -> Option<SeaState> {
        self.wave_height.map(SeaState::of)
    }
}

/// The marine forecast at one point.
#[derive(Debug, Clone, PartialEq)]
pub struct Ocean {
    pub latitude: f64,
    pub longitude: f64,
    /// The provider's own unit string for each value.
    pub units: BTreeMap<String, String>,
    pub hours: Vec<OceanHour>,
}

impl Ocean {
    /// The provider's unit for a field, or a message saying it sent none.
    pub fn unit(&self, field: &str) -> &str {
        self.units
            .get(field)
            .map_or("unit not stated", String::as_str)
    }

    /// Whether the grid point carries any sea value at all. A point inland
    /// answers with hours and nothing in them.
    pub fn has_water(&self) -> bool {
        self.hours.iter().any(|hour| {
            hour.wave_height.is_some()
                || hour.sea_temperature.is_some()
                || hour.current_kmh.is_some()
        })
    }

    /// The newest hour at or before `now_ms`.
    pub fn at(&self, now_ms: u64) -> Option<OceanHour> {
        self.hours
            .iter()
            .rev()
            .find(|hour| hour.time_ms <= now_ms)
            .copied()
    }

    /// The roughest hour in the record.
    pub fn roughest(&self) -> Option<OceanHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.wave_height.map(|height| (height, *hour)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, hour)| hour)
    }

    /// The height the chart's axis has to hold, never below the rough mark.
    pub fn axis_top(&self) -> f64 {
        let Some(height) = self.roughest().and_then(|hour| hour.wave_height) else {
            return 4.0;
        };
        SeaState::of(height).top().max(4.0)
    }
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> OceanError {
    OceanError::Shape { detail }
}

/// Reads one series, checking it against the hour count.
fn series(
    row: &Map<String, Value>,
    field: &str,
    count: usize,
) -> Result<Vec<Option<f64>>, OceanError> {
    let Some(values) = row.get(field) else {
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

/// Reads the provider's document into a record.
pub fn parse(document: &Value) -> Result<Ocean, OceanError> {
    let Some(latitude) = document.get("latitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let Some(longitude) = document.get("longitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let Some(hourly) = document.get("hourly").and_then(Value::as_object) else {
        return Err(shape("an hourly record"));
    };
    let Some(times) = hourly.get("time").and_then(Value::as_array) else {
        return Err(shape("an array of hourly times"));
    };
    let count = times.len();
    let wave_height = series(hourly, "wave_height", count)?;
    let wave_direction = series(hourly, "wave_direction", count)?;
    let wave_period = series(hourly, "wave_period", count)?;
    let swell_height = series(hourly, "swell_wave_height", count)?;
    let sea_temperature = series(hourly, "sea_surface_temperature", count)?;
    let current = series(hourly, "ocean_current_velocity", count)?;
    let current_direction = series(hourly, "ocean_current_direction", count)?;
    let mut hours = Vec::with_capacity(count);
    for (position, text) in times.iter().enumerate() {
        let Some(text) = text.as_str() else {
            return Err(shape("every hourly time written as text"));
        };
        let Some(time_ms) = time::parse_iso_utc(text) else {
            return Err(shape("every hourly time written as ISO 8601"));
        };
        let at = |values: &Vec<Option<f64>>| values.get(position).copied().flatten();
        hours.push(OceanHour {
            time_ms,
            wave_height: at(&wave_height),
            wave_from_deg: at(&wave_direction).filter(|value| *value <= 360.0),
            wave_period_s: at(&wave_period),
            swell_height: at(&swell_height),
            sea_temperature: at(&sea_temperature),
            current_kmh: at(&current),
            current_toward_deg: at(&current_direction).filter(|value| *value <= 360.0),
        });
    }
    if hours.is_empty() {
        return Err(shape("at least one hour of record"));
    }
    let mut units = BTreeMap::new();
    if let Some(object) = document.get("hourly_units").and_then(Value::as_object) {
        for (field, value) in object {
            if let Some(text) = value.as_str() {
                units.insert(field.clone(), String::from(text));
            }
        }
    }
    Ok(Ocean {
        latitude,
        longitude,
        units,
        hours,
    })
}

/// The request for one point.
pub fn request_url(latitude: f64, longitude: f64) -> String {
    format!(
        "{BASE}?latitude={latitude:.3}&longitude={longitude:.3}&hourly={HOURLY}&forecast_days={FORECAST_DAYS}&timezone=UTC&cell_selection=sea"
    )
}

/// Fetches the marine forecast for one point.
pub fn fetch(latitude: f64, longitude: f64) -> Result<Ocean, OceanError> {
    let document = provider::get_json(&request_url(latitude, longitude), "application/json")
        .map_err(|source| OceanError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct OceanRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum OceanEvent {
    Ready { generation: u64, record: Ocean },
    Failed { generation: u64, error: OceanError },
}

/// Fetches the sea state off the UI thread.
pub struct OceanFeed {
    requests: Sender<OceanRequest>,
    events: Receiver<OceanEvent>,
    generation: u64,
    stopped: bool,
}

impl OceanFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<OceanRequest>();
        let (event_sender, events) = mpsc::channel::<OceanEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch(request.latitude, request.longitude) {
                    Ok(record) => OceanEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => OceanEvent::Failed {
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

    pub fn request(&mut self, request: OceanRequest) -> Result<(), OceanError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(OceanError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<OceanEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(OceanEvent::Failed {
                    generation: self.generation,
                    error: OceanError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum OceanState {
    Idle,
    Loading {
        last: Option<Ocean>,
    },
    Ready {
        record: Ocean,
    },
    Failed {
        error: OceanError,
        last: Option<Ocean>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl OceanState {
    /// The most recent record still worth drawing.
    pub const fn last(&self) -> Option<&Ocean> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct OceanView {
    feed: OceanFeed,
    state: OceanState,
    generation: u64,
}

impl OceanView {
    pub fn new() -> Self {
        Self {
            feed: OceanFeed::new(),
            state: OceanState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &OceanState {
        &self.state
    }

    pub const fn record(&self) -> Option<&Ocean> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = OceanState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = OceanRequest {
            generation: self.generation,
            latitude,
            longitude,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = OceanState::Failed {
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
                OceanEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = OceanState::Ready { record };
                }
                OceanEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = OceanState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            OceanState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            OceanState::Idle | OceanState::Loading { .. } | OceanState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            OceanState::Failed { attempts, .. } => *attempts,
            OceanState::Idle | OceanState::Loading { .. } | OceanState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            OceanState::Failed { retry_due, .. } => retry_due.is_some(),
            OceanState::Idle | OceanState::Loading { .. } | OceanState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut OceanState) -> Option<Ocean> {
    match std::mem::replace(state, OceanState::Idle) {
        OceanState::Loading { last } | OceanState::Failed { last, .. } => last,
        OceanState::Ready { record } => Some(record),
        OceanState::Idle => None,
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
            "latitude": 25.0,
            "longitude": -80.0,
            "hourly_units": {"time": "iso8601", "wave_height": "m", "sea_surface_temperature": "°C", "ocean_current_velocity": "km/h"},
            "hourly": {
                "time": ["2026-11-02T00:00", "2026-11-02T01:00", "2026-11-02T02:00"],
                "wave_height": [1.3, 2.9, null],
                "wave_direction": [70.0, 80.0, null],
                "wave_period": [5.1, 7.4, null],
                "swell_wave_height": [0.56, 1.1, null],
                "sea_surface_temperature": [28.5, 28.4, null],
                "ocean_current_velocity": [1.5, 1.6, null],
                "ocean_current_direction": [353.0, 354.0, null]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn record() -> Result<Ocean, OceanError> {
        parse(&document())
    }

    #[test]
    fn a_record_keeps_every_hour_and_the_providers_units() -> Result<(), OceanError> {
        let record = record()?;
        assert_eq!(record.hours.len(), 3);
        assert_eq!(record.hours[0].wave_height, Some(1.3));
        assert_eq!(record.hours[0].sea_temperature, Some(28.5));
        assert_eq!(
            record.hours[2].wave_height, None,
            "a missing hour stays missing"
        );
        assert_eq!(record.unit("wave_height"), "m");
        assert!(record.has_water());
        Ok(())
    }

    #[test]
    fn the_sea_state_follows_the_douglas_scale() {
        assert_eq!(SeaState::of(0.05), SeaState::Calm);
        assert_eq!(SeaState::of(0.3), SeaState::Smooth);
        assert_eq!(SeaState::of(0.9), SeaState::Slight);
        assert_eq!(SeaState::of(1.3), SeaState::Moderate);
        assert_eq!(SeaState::of(2.9), SeaState::Rough);
        assert_eq!(SeaState::of(5.0), SeaState::VeryRough);
        assert_eq!(SeaState::of(15.0), SeaState::Phenomenal);
        assert_eq!(SeaState::Rough.label(), "rough");
    }

    #[test]
    fn the_roughest_hour_is_the_highest_wave() -> Result<(), OceanError> {
        let record = record()?;
        assert_eq!(
            record.roughest().and_then(|hour| hour.wave_height),
            Some(2.9)
        );
        assert_eq!(record.axis_top(), 4.0, "the chart holds the rough mark");
        Ok(())
    }

    #[test]
    fn a_land_point_carries_no_water() -> Result<(), OceanError> {
        let body = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "hourly": {
                "time": ["2026-11-02T00:00"],
                "wave_height": [null],
                "sea_surface_temperature": [null],
                "ocean_current_velocity": [null]
            }
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Err(shape("a readable test document"));
        };
        let record = parse(&value)?;
        assert!(!record.has_water(), "a grid point on land has no sea state");
        Ok(())
    }

    #[test]
    fn the_current_hour_is_the_newest_one_before_now() -> Result<(), OceanError> {
        let record = record()?;
        let now = record.hours[1].time_ms + 60_000;
        assert_eq!(record.at(now).and_then(|hour| hour.wave_height), Some(2.9));
        assert!(record.at(0).is_none());
        Ok(())
    }

    #[test]
    fn a_direction_beyond_a_full_turn_is_not_a_direction() {
        let body = r#"{
            "latitude": 1.0, "longitude": 2.0,
            "hourly": {
                "time": ["2026-11-02T00:00"],
                "wave_height": [1.0],
                "wave_direction": [400.0]
            }
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let Ok(record) = parse(&value) else {
            return;
        };
        assert_eq!(record.hours[0].wave_from_deg, None);
    }

    #[test]
    fn the_request_names_the_point_and_asks_for_sea_cells() {
        let url = request_url(25.0, -80.0);
        assert!(url.contains("latitude=25.000"), "{url}");
        assert!(url.contains("wave_height"), "{url}");
        assert!(url.contains("ocean_current_direction"), "{url}");
        assert!(url.contains("cell_selection=sea"), "{url}");
        assert!(url.contains("timezone=UTC"), "{url}");
    }
}
