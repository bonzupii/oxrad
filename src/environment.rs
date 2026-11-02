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
//! The storm environment: convective available energy, its inhibition, the
//! lifted index, the freezing level, and the wind difference with height.
//!
//! The values are hourly model fields on the forecast grid, not a sounding
//! launched at the point. They say what the model thinks the air over that grid
//! box can do, which is the question a chaser asks before a storm exists.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo convective fields";

const BASE: &str = "https://api.open-meteo.com/v1/forecast";
const HOURLY: &str = "cape,convective_inhibition,lifted_index,freezing_level_height,wind_speed_10m,wind_direction_10m,wind_speed_850hPa,wind_direction_850hPa,wind_speed_500hPa,wind_direction_500hPa";
const FORECAST_DAYS: u8 = 2;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// The energy at which a sounding is worth a chaser's attention.
pub const CAPE_MODERATE: f64 = 1_000.0;
pub const CAPE_LARGE: f64 = 2_500.0;

/// Why a convective record could not be read.
#[derive(Debug)]
pub enum EnvironmentError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for EnvironmentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The convective-field request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The convective record is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => write!(
                formatter,
                "The convective worker stopped, so no storm environment can be requested."
            ),
        }
    }
}

impl Error for EnvironmentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl EnvironmentError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// A wind as the provider reports it: speed and the direction it blows from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wind {
    pub speed_kmh: f64,
    pub from_deg: f64,
}

impl Wind {
    /// The eastward and northward components, in the provider's speed unit.
    pub fn components(self) -> (f64, f64) {
        let radians = self.from_deg.to_radians();
        (
            -self.speed_kmh * radians.sin(),
            -self.speed_kmh * radians.cos(),
        )
    }
}

/// The wind difference between the surface and the 500 hPa level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shear {
    pub speed_kmh: f64,
    pub from_deg: f64,
}

impl Shear {
    /// The hole a storm's updraught tilts into, from the two winds.
    pub fn between(surface: Wind, mid: Wind) -> Option<Self> {
        let (su, sv) = surface.components();
        let (mu, mv) = mid.components();
        let (du, dv) = (mu - su, mv - sv);
        let speed = du.hypot(dv);
        if !speed.is_finite() || speed <= 0.0 {
            return None;
        }
        // The direction the difference blows from, which is the opposite of
        // the direction the difference points towards.
        let from_deg = (-du).atan2(-dv).to_degrees().rem_euclid(360.0);
        Some(Self {
            speed_kmh: speed,
            from_deg,
        })
    }
}

/// How much energy the model gives the air.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapeCategory {
    /// Too little to support deep convection.
    Weak,
    /// Enough for thunderstorms if something lifts the air.
    Moderate,
    /// Enough for organised storms.
    Large,
    /// Enough for explosive updraughts.
    Extreme,
}

impl CapeCategory {
    pub fn of(cape: f64) -> Self {
        if cape < CAPE_MODERATE {
            Self::Weak
        } else if cape < CAPE_LARGE {
            Self::Moderate
        } else if cape < 4_000.0 {
            Self::Large
        } else {
            Self::Extreme
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Weak => "little support",
            Self::Moderate => "thunderstorms possible",
            Self::Large => "organised storms possible",
            Self::Extreme => "explosive updraughts",
        }
    }
}

/// One hour of the model's convective fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvironmentHour {
    pub time_ms: u64,
    pub cape: Option<f64>,
    pub cin: Option<f64>,
    pub lifted_index: Option<f64>,
    pub freezing_level_m: Option<f64>,
    pub surface: Option<Wind>,
    pub low: Option<Wind>,
    pub mid: Option<Wind>,
}

impl EnvironmentHour {
    /// The surface-to-500 hPa wind difference, which stands in for the 0–6 km
    /// shear the model does not publish directly.
    pub fn shear(&self) -> Option<Shear> {
        Shear::between(self.surface?, self.mid?)
    }

    /// The energy category of the hour, when it reports any.
    pub fn category(&self) -> Option<CapeCategory> {
        self.cape.map(CapeCategory::of)
    }
}

/// The convective fields at one point.
#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: Option<f64>,
    pub hours: Vec<EnvironmentHour>,
}

impl Environment {
    /// The newest hour at or before `now_ms`, which is what the air is doing.
    pub fn at(&self, now_ms: u64) -> Option<EnvironmentHour> {
        self.hours
            .iter()
            .rev()
            .find(|hour| hour.time_ms <= now_ms)
            .copied()
    }

    /// The hour with the most energy in the record.
    pub fn peak(&self) -> Option<EnvironmentHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.cape.map(|cape| (cape, *hour)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, hour)| hour)
    }

    /// The energy the chart's axis has to hold, never below the level where
    /// storms become possible.
    pub fn axis_top(&self) -> f64 {
        let peak = match self.peak().and_then(|hour| hour.cape) {
            Some(peak) => peak,
            None => return CAPE_LARGE,
        };
        CapeCategory::of(peak).top().max(CAPE_LARGE)
    }
}

impl CapeCategory {
    /// The energy at which the category ends.
    pub const fn top(self) -> f64 {
        match self {
            Self::Weak => CAPE_MODERATE,
            Self::Moderate => CAPE_LARGE,
            Self::Large => 4_000.0,
            Self::Extreme => 8_000.0,
        }
    }
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> EnvironmentError {
    EnvironmentError::Shape { detail }
}

/// Reads one series into a vector of optional values, refusing a value below
/// the floor the quantity can physically have.
fn series(
    row: &Map<String, Value>,
    field: &str,
    count: usize,
    floor: f64,
) -> Result<Vec<Option<f64>>, EnvironmentError> {
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
            if !number.is_finite() || number < floor {
                return Err(shape("a finite hourly value above its floor"));
            }
            Ok(Some(number))
        })
        .collect()
}

/// A wind from its two series, when both are present.
fn wind(speed: Option<f64>, direction: Option<f64>) -> Option<Wind> {
    let (speed, from_deg) = (speed?, direction?);
    if !(0.0..=360.0).contains(&from_deg) {
        return None;
    }
    Some(Wind {
        speed_kmh: speed,
        from_deg,
    })
}

/// Reads the provider's document into a record.
pub fn parse(document: &Value) -> Result<Environment, EnvironmentError> {
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
    let cape = series(hourly, "cape", count, 0.0)?;
    let cin = series(hourly, "convective_inhibition", count, 0.0)?;
    let lifted = series(hourly, "lifted_index", count, f64::NEG_INFINITY)?;
    let freezing = series(hourly, "freezing_level_height", count, 0.0)?;
    let surface_speed = series(hourly, "wind_speed_10m", count, 0.0)?;
    let surface_dir = series(hourly, "wind_direction_10m", count, 0.0)?;
    let low_speed = series(hourly, "wind_speed_850hPa", count, 0.0)?;
    let low_dir = series(hourly, "wind_direction_850hPa", count, 0.0)?;
    let mid_speed = series(hourly, "wind_speed_500hPa", count, 0.0)?;
    let mid_dir = series(hourly, "wind_direction_500hPa", count, 0.0)?;
    let mut hours = Vec::with_capacity(count);
    for (position, text) in times.iter().enumerate() {
        let Some(text) = text.as_str() else {
            return Err(shape("every hourly time written as text"));
        };
        let Some(time_ms) = time::parse_iso_utc(text) else {
            return Err(shape("every hourly time written as ISO 8601"));
        };
        let at = |values: &Vec<Option<f64>>| values.get(position).copied().flatten();
        hours.push(EnvironmentHour {
            time_ms,
            cape: at(&cape),
            cin: at(&cin),
            lifted_index: at(&lifted),
            freezing_level_m: at(&freezing),
            surface: wind(at(&surface_speed), at(&surface_dir)),
            low: wind(at(&low_speed), at(&low_dir)),
            mid: wind(at(&mid_speed), at(&mid_dir)),
        });
    }
    if hours.is_empty() {
        return Err(shape("at least one hour of record"));
    }
    Ok(Environment {
        latitude,
        longitude,
        elevation_m: document.get("elevation").and_then(Value::as_f64),
        hours,
    })
}

/// The request for one point.
pub fn request_url(latitude: f64, longitude: f64) -> String {
    format!(
        "{BASE}?latitude={latitude:.3}&longitude={longitude:.3}&hourly={HOURLY}&forecast_days={FORECAST_DAYS}&timezone=UTC"
    )
}

/// Fetches the convective fields for one point.
pub fn fetch(latitude: f64, longitude: f64) -> Result<Environment, EnvironmentError> {
    let document = provider::get_json(&request_url(latitude, longitude), "application/json")
        .map_err(|source| EnvironmentError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct EnvironmentRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum EnvironmentEvent {
    Ready {
        generation: u64,
        record: Environment,
    },
    Failed {
        generation: u64,
        error: EnvironmentError,
    },
}

/// Fetches convective fields off the UI thread.
pub struct EnvironmentFeed {
    requests: Sender<EnvironmentRequest>,
    events: Receiver<EnvironmentEvent>,
    generation: u64,
    stopped: bool,
}

impl EnvironmentFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<EnvironmentRequest>();
        let (event_sender, events) = mpsc::channel::<EnvironmentEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch(request.latitude, request.longitude) {
                    Ok(record) => EnvironmentEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => EnvironmentEvent::Failed {
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

    pub fn request(&mut self, request: EnvironmentRequest) -> Result<(), EnvironmentError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(EnvironmentError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<EnvironmentEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(EnvironmentEvent::Failed {
                    generation: self.generation,
                    error: EnvironmentError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum EnvironmentState {
    Idle,
    Loading {
        last: Option<Environment>,
    },
    Ready {
        record: Environment,
    },
    Failed {
        error: EnvironmentError,
        last: Option<Environment>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl EnvironmentState {
    /// The most recent record still worth drawing.
    pub const fn last(&self) -> Option<&Environment> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct EnvironmentView {
    feed: EnvironmentFeed,
    state: EnvironmentState,
    generation: u64,
}

impl EnvironmentView {
    pub fn new() -> Self {
        Self {
            feed: EnvironmentFeed::new(),
            state: EnvironmentState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &EnvironmentState {
        &self.state
    }

    pub const fn record(&self) -> Option<&Environment> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = EnvironmentState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = EnvironmentRequest {
            generation: self.generation,
            latitude,
            longitude,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = EnvironmentState::Failed {
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
                EnvironmentEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = EnvironmentState::Ready { record };
                }
                EnvironmentEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = EnvironmentState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            EnvironmentState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            EnvironmentState::Idle
            | EnvironmentState::Loading { .. }
            | EnvironmentState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            EnvironmentState::Failed { attempts, .. } => *attempts,
            EnvironmentState::Idle
            | EnvironmentState::Loading { .. }
            | EnvironmentState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            EnvironmentState::Failed { retry_due, .. } => retry_due.is_some(),
            EnvironmentState::Idle
            | EnvironmentState::Loading { .. }
            | EnvironmentState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut EnvironmentState) -> Option<Environment> {
    match std::mem::replace(state, EnvironmentState::Idle) {
        EnvironmentState::Loading { last } | EnvironmentState::Failed { last, .. } => last,
        EnvironmentState::Ready { record } => Some(record),
        EnvironmentState::Idle => None,
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
            "longitude": -96.2,
            "elevation": 347.0,
            "hourly_units": {"time": "iso8601", "cape": "J/kg", "wind_speed_10m": "km/h"},
            "hourly": {
                "time": ["2026-11-02T00:00", "2026-11-02T01:00", "2026-11-02T02:00"],
                "cape": [10.0, 1800.0, 3200.0],
                "convective_inhibition": [1.0, 25.0, 60.0],
                "lifted_index": [5.0, -2.0, -5.5],
                "freezing_level_height": [4300.0, 4200.0, 4100.0],
                "wind_speed_10m": [15.0, 20.0, 25.0],
                "wind_direction_10m": [180.0, 180.0, 180.0],
                "wind_speed_850hPa": [30.0, 35.0, 40.0],
                "wind_direction_850hPa": [190.0, 195.0, 200.0],
                "wind_speed_500hPa": [45.0, 50.0, 55.0],
                "wind_direction_500hPa": [240.0, 250.0, 260.0]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn record() -> Result<Environment, EnvironmentError> {
        parse(&document())
    }

    #[test]
    fn a_record_keeps_every_hour_and_its_winds() -> Result<(), EnvironmentError> {
        let record = record()?;
        assert_eq!(record.hours.len(), 3);
        assert_eq!(record.hours[1].cape, Some(1800.0));
        assert_eq!(record.hours[1].cin, Some(25.0));
        assert_eq!(record.elevation_m, Some(347.0));
        assert_eq!(record.hours[2].mid.map(|wind| wind.speed_kmh), Some(55.0));
        Ok(())
    }

    #[test]
    fn the_energy_categories_follow_the_thresholds() {
        assert_eq!(CapeCategory::of(999.0), CapeCategory::Weak);
        assert_eq!(CapeCategory::of(1000.0), CapeCategory::Moderate);
        assert_eq!(CapeCategory::of(2500.0), CapeCategory::Large);
        assert_eq!(CapeCategory::of(4200.0), CapeCategory::Extreme);
    }

    #[test]
    fn the_newest_past_hour_is_what_the_air_is_doing() -> Result<(), EnvironmentError> {
        let record = record()?;
        let now = record.hours[1].time_ms + 1800_000;
        let current = record.at(now).ok_or(shape("an hour at that time"))?;
        assert_eq!(current.cape, Some(1800.0));
        assert_eq!(record.at(0), None, "no hour is before the first one");
        Ok(())
    }

    #[test]
    fn the_peak_is_the_hour_with_the_most_energy() -> Result<(), EnvironmentError> {
        let record = record()?;
        assert_eq!(record.peak().and_then(|hour| hour.cape), Some(3200.0));
        assert_eq!(record.axis_top(), 4000.0);
        Ok(())
    }

    #[test]
    fn a_quiet_record_keeps_the_axis_at_the_storm_level() -> Result<(), EnvironmentError> {
        let body = r#"{
            "latitude": 1.0, "longitude": 2.0,
            "hourly": {"time": ["2026-11-02T00:00"], "cape": [12.0]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Err(shape("a readable test document"));
        };
        let record = parse(&value)?;
        assert_eq!(record.axis_top(), CAPE_LARGE);
        Ok(())
    }

    #[test]
    fn shear_is_the_wind_difference_from_the_surface_to_the_mid_levels() {
        let surface = Wind {
            speed_kmh: 20.0,
            from_deg: 180.0,
        };
        let mid = Wind {
            speed_kmh: 20.0,
            from_deg: 270.0,
        };
        let shear = Shear::between(surface, mid);
        assert!(shear.is_some());
        if let Some(shear) = shear {
            // A southerly surface wind and a westerly mid-level wind are at
            // right angles, so the difference is the hypotenuse of two 20 km/h
            // legs, and it blows from the north-west.
            assert!((shear.speed_kmh - 28.284).abs() < 0.001, "{shear:?}");
            assert!((shear.from_deg - 315.0).abs() < 0.001, "{shear:?}");
        }
    }

    #[test]
    fn a_calm_surface_under_a_calm_mid_level_has_no_shear() {
        let still = Wind {
            speed_kmh: 0.0,
            from_deg: 0.0,
        };
        assert!(Shear::between(still, still).is_none());
    }

    #[test]
    fn a_negative_energy_is_refused() {
        let body = r#"{
            "latitude": 1.0, "longitude": 2.0,
            "hourly": {"time": ["2026-11-02T00:00"], "cape": [-5.0]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(parse(&value).is_err());
    }

    #[test]
    fn the_request_names_the_point_and_the_fields() {
        let url = request_url(39.0, -96.2345);
        assert!(url.contains("latitude=39.000"), "{url}");
        assert!(url.contains("longitude=-96.234"), "{url}");
        assert!(url.contains("cape"), "{url}");
        assert!(url.contains("lifted_index"), "{url}");
        assert!(url.contains("wind_speed_500hPa"), "{url}");
        assert!(url.contains("timezone=UTC"), "{url}");
    }
}
