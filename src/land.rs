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
//! The land surface: how wet the soil is, how much water the sky is pulling
//! out of it, and how much sun and ultraviolet reach it.
//!
//! Every value here is a forecast-model field for the grid box under the map
//! centre. Soil moisture is volumetric water content, in cubic metres of water
//! per cubic metre of soil, so it is a fraction, not a depth, and it is not
//! comparable between a sand and a clay without the soil's own properties.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo land surface";
const BASE: &str = "https://api.open-meteo.com/v1/forecast";
const HOURLY: &str = "soil_moisture_0_to_10cm,soil_moisture_10_to_40cm,soil_temperature_6cm,et0_fao_evapotranspiration,vapour_pressure_deficit,shortwave_radiation,direct_normal_irradiance,uv_index";
const PAST_DAYS: u8 = 1;
const FORECAST_DAYS: u8 = 2;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// The volumetric water content at which a soil is treated as dry, as a rule
/// of thumb for a loam. It is not a drought index.
pub const DRY: f64 = 0.18;
pub const VERY_DRY: f64 = 0.10;
pub const WET: f64 = 0.30;

/// How the soil's water content reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SoilBand {
    VeryDry,
    Dry,
    Moist,
    Wet,
}

impl SoilBand {
    /// The band a volumetric water content falls in.
    pub fn of(fraction: f64) -> Self {
        if fraction < VERY_DRY {
            Self::VeryDry
        } else if fraction < DRY {
            Self::Dry
        } else if fraction < WET {
            Self::Moist
        } else {
            Self::Wet
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::VeryDry => "very dry for a loam",
            Self::Dry => "dry for a loam",
            Self::Moist => "moist",
            Self::Wet => "wet",
        }
    }
}

/// Why a land-surface record could not be read.
#[derive(Debug)]
pub enum LandError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for LandError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The land-surface request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The land-surface record is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => write!(
                formatter,
                "The land-surface worker stopped, so no land record can be requested."
            ),
        }
    }
}

impl Error for LandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl LandError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// One hour of the land surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LandHour {
    pub time_ms: u64,
    /// Volumetric water content in the top ten centimetres.
    pub moisture_top: Option<f64>,
    /// Volumetric water content between ten and forty centimetres.
    pub moisture_deep: Option<f64>,
    pub soil_temperature_c: Option<f64>,
    /// Reference evapotranspiration for the hour, millimetres.
    pub et0_mm: Option<f64>,
    /// Vapour-pressure deficit, kilopascals.
    pub vpd_kpa: Option<f64>,
    pub shortwave_wm2: Option<f64>,
    pub direct_normal_wm2: Option<f64>,
    pub uv_index: Option<f64>,
}

/// The land surface at one point.
#[derive(Debug, Clone, PartialEq)]
pub struct Land {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: Option<f64>,
    /// The provider's own unit string for each field.
    pub units: BTreeMap<String, String>,
    pub hours: Vec<LandHour>,
}

impl Land {
    /// The provider's unit for a field, treating its placeholder as no answer.
    pub fn unit(&self, field: &str) -> &str {
        match self.units.get(field).map(String::as_str) {
            Some(text) if !text.is_empty() && text != "undefined" => text,
            Some(_) | None => "no unit stated",
        }
    }

    /// The newest hour at or before `now_ms`.
    pub fn at(&self, now_ms: u64) -> Option<LandHour> {
        self.hours
            .iter()
            .rev()
            .find(|hour| hour.time_ms <= now_ms)
            .copied()
    }

    /// Reference evapotranspiration summed between two times, so the rail can
    /// say what the atmosphere will pull out of the ground.
    pub fn et0_between(&self, from_ms: u64, to_ms: u64) -> f64 {
        self.hours
            .iter()
            .filter(|hour| hour.time_ms > from_ms && hour.time_ms <= to_ms)
            .filter_map(|hour| hour.et0_mm)
            .sum()
    }

    /// The hour with the strongest ultraviolet index in the record.
    pub fn peak_uv(&self) -> Option<LandHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.uv_index.map(|uv| (uv, *hour)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, hour)| hour)
    }

    /// The hour with the most shortwave radiation, which is when the sun was
    /// or will be highest over the point.
    pub fn sunniest(&self) -> Option<LandHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.shortwave_wm2.map(|sun| (sun, *hour)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, hour)| hour)
    }

    /// The wettest soil in the record, by the top layer.
    pub fn wettest(&self) -> Option<LandHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.moisture_top.map(|water| (water, *hour)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, hour)| hour)
    }

    /// The top of the chart's moisture axis.
    pub fn axis_top(&self) -> f64 {
        let highest = self
            .hours
            .iter()
            .filter_map(|hour| hour.moisture_top)
            .fold(0.0_f64, f64::max);
        (highest * 1.15).clamp(WET, 1.0)
    }
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> LandError {
    LandError::Shape { detail }
}

/// Reads one series, checking it against the hour count.
fn series(
    row: &Map<String, Value>,
    field: &str,
    count: usize,
) -> Result<Vec<Option<f64>>, LandError> {
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
            if !number.is_finite() {
                return Err(shape("a finite hourly value"));
            }
            Ok(Some(number))
        })
        .collect()
}

/// Reads the provider's document into a record.
pub fn parse(document: &Value) -> Result<Land, LandError> {
    let Some(latitude) = document.get("latitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let Some(longitude) = document.get("longitude").and_then(Value::as_f64) else {
        return Err(shape("the point it answered for"));
    };
    let elevation_m = document.get("elevation").and_then(Value::as_f64);
    let Some(hourly) = document.get("hourly").and_then(Value::as_object) else {
        return Err(shape("an hourly record"));
    };
    let Some(times) = hourly.get("time").and_then(Value::as_array) else {
        return Err(shape("an array of hourly times"));
    };
    let count = times.len();
    if count == 0 {
        return Err(shape("at least one hour of record"));
    }
    let moisture_top = series(hourly, "soil_moisture_0_to_10cm", count)?;
    let moisture_deep = series(hourly, "soil_moisture_10_to_40cm", count)?;
    let soil_temperature = series(hourly, "soil_temperature_6cm", count)?;
    let et0 = series(hourly, "et0_fao_evapotranspiration", count)?;
    let vpd = series(hourly, "vapour_pressure_deficit", count)?;
    let shortwave = series(hourly, "shortwave_radiation", count)?;
    let direct = series(hourly, "direct_normal_irradiance", count)?;
    let uv = series(hourly, "uv_index", count)?;
    let mut hours = Vec::with_capacity(count);
    for (position, text) in times.iter().enumerate() {
        let Some(text) = text.as_str() else {
            return Err(shape("every hourly time written as text"));
        };
        let Some(time_ms) = time::parse_iso_utc(text) else {
            return Err(shape("every hourly time written as ISO 8601"));
        };
        let at = |values: &Vec<Option<f64>>| values.get(position).copied().flatten();
        hours.push(LandHour {
            time_ms,
            moisture_top: at(&moisture_top).filter(|value| (0.0..=1.0).contains(value)),
            moisture_deep: at(&moisture_deep).filter(|value| (0.0..=1.0).contains(value)),
            soil_temperature_c: at(&soil_temperature),
            et0_mm: at(&et0).filter(|value| *value >= 0.0),
            vpd_kpa: at(&vpd).filter(|value| *value >= 0.0),
            shortwave_wm2: at(&shortwave).filter(|value| *value >= 0.0),
            direct_normal_wm2: at(&direct).filter(|value| *value >= 0.0),
            uv_index: at(&uv).filter(|value| *value >= 0.0),
        });
    }
    let mut units = BTreeMap::new();
    if let Some(object) = document.get("hourly_units").and_then(Value::as_object) {
        for (field, value) in object {
            if let Some(text) = value.as_str() {
                units.insert(field.clone(), String::from(text));
            }
        }
    }
    Ok(Land {
        latitude,
        longitude,
        elevation_m,
        units,
        hours,
    })
}

/// The request for one point.
pub fn request_url(latitude: f64, longitude: f64) -> String {
    format!(
        "{BASE}?latitude={latitude:.3}&longitude={longitude:.3}&hourly={HOURLY}&past_days={PAST_DAYS}&forecast_days={FORECAST_DAYS}&timezone=UTC"
    )
}

/// Fetches the land surface for one point.
pub fn fetch(latitude: f64, longitude: f64) -> Result<Land, LandError> {
    let document = provider::get_json(&request_url(latitude, longitude), "application/json")
        .map_err(|source| LandError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct LandRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum LandEvent {
    Ready { generation: u64, record: Land },
    Failed { generation: u64, error: LandError },
}

/// Fetches the land surface off the UI thread.
pub struct LandFeed {
    requests: Sender<LandRequest>,
    events: Receiver<LandEvent>,
    generation: u64,
    stopped: bool,
}

impl LandFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<LandRequest>();
        let (event_sender, events) = mpsc::channel::<LandEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch(request.latitude, request.longitude) {
                    Ok(record) => LandEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => LandEvent::Failed {
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

    pub fn request(&mut self, request: LandRequest) -> Result<(), LandError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(LandError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<LandEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(LandEvent::Failed {
                    generation: self.generation,
                    error: LandError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum LandState {
    Idle,
    Loading {
        last: Option<Land>,
    },
    Ready {
        record: Land,
    },
    Failed {
        error: LandError,
        last: Option<Land>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl LandState {
    /// The most recent record still worth drawing.
    pub const fn last(&self) -> Option<&Land> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct LandView {
    feed: LandFeed,
    state: LandState,
    generation: u64,
}

impl LandView {
    pub fn new() -> Self {
        Self {
            feed: LandFeed::new(),
            state: LandState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &LandState {
        &self.state
    }

    pub const fn record(&self) -> Option<&Land> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = LandState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = LandRequest {
            generation: self.generation,
            latitude,
            longitude,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = LandState::Failed {
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
                LandEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = LandState::Ready { record };
                }
                LandEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = LandState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            LandState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            LandState::Idle | LandState::Loading { .. } | LandState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            LandState::Failed { attempts, .. } => *attempts,
            LandState::Idle | LandState::Loading { .. } | LandState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            LandState::Failed { retry_due, .. } => retry_due.is_some(),
            LandState::Idle | LandState::Loading { .. } | LandState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut LandState) -> Option<Land> {
    match std::mem::replace(state, LandState::Idle) {
        LandState::Loading { last } | LandState::Failed { last, .. } => last,
        LandState::Ready { record } => Some(record),
        LandState::Idle => None,
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
            "latitude": 37.2, "longitude": -97.5, "elevation": 373.0,
            "hourly_units": {
                "time": "iso8601",
                "soil_moisture_0_to_10cm": "m³/m³",
                "soil_moisture_10_to_40cm": "m³/m³",
                "soil_temperature_6cm": "°C",
                "et0_fao_evapotranspiration": "mm",
                "vapour_pressure_deficit": "kPa",
                "shortwave_radiation": "W/m²",
                "direct_normal_irradiance": "W/m²",
                "uv_index": ""
            },
            "hourly": {
                "time": ["2026-11-02T00:00", "2026-11-02T01:00", "2026-11-02T02:00"],
                "soil_moisture_0_to_10cm": [0.21, 0.205, 0.2],
                "soil_moisture_10_to_40cm": [0.186, 0.185, 0.184],
                "soil_temperature_6cm": [17.0, 17.4, 17.9],
                "et0_fao_evapotranspiration": [0.02, 0.03, 0.04],
                "vapour_pressure_deficit": [0.1, 0.12, 0.13],
                "shortwave_radiation": [0.0, 4.0, 28.0],
                "direct_normal_irradiance": [0.0, 0.0, 0.0],
                "uv_index": [0.0, 0.25, 0.3]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn record() -> Result<Land, LandError> {
        parse(&document())
    }

    #[test]
    fn a_record_is_read_with_its_units() -> Result<(), LandError> {
        let record = record()?;
        assert_eq!(record.hours.len(), 3);
        assert_eq!(record.unit("soil_moisture_0_to_10cm"), "m³/m³");
        assert_eq!(record.unit("et0_fao_evapotranspiration"), "mm");
        // The provider sends an empty string for a field with no unit.
        assert_eq!(record.unit("uv_index"), "no unit stated");
        assert_eq!(record.unit("a_field_it_never_sent"), "no unit stated");
        Ok(())
    }

    #[test]
    fn a_soil_band_follows_the_water_content() {
        assert_eq!(SoilBand::of(0.05), SoilBand::VeryDry);
        assert_eq!(SoilBand::of(0.15), SoilBand::Dry);
        assert_eq!(SoilBand::of(0.21), SoilBand::Moist);
        assert_eq!(SoilBand::of(0.35), SoilBand::Wet);
    }

    #[test]
    fn the_newest_hour_at_or_before_now_is_the_one_read() -> Result<(), LandError> {
        let record = record()?;
        let first = time::parse_iso_utc("2026-11-02T00:00").unwrap_or_default();
        let second = time::parse_iso_utc("2026-11-02T01:00").unwrap_or_default();
        assert_eq!(
            record.at(second).and_then(|hour| hour.soil_temperature_c),
            Some(17.4)
        );
        assert_eq!(record.at(first - 1).map(|hour| hour.time_ms), None);
        Ok(())
    }

    #[test]
    fn evapotranspiration_is_summed_over_the_hours_asked_for() -> Result<(), LandError> {
        let record = record()?;
        let start = time::parse_iso_utc("2026-11-02T00:00").unwrap_or_default();
        let end = time::parse_iso_utc("2026-11-02T02:00").unwrap_or_default();
        let total = record.et0_between(start, end);
        assert!((total - 0.07).abs() < 1e-9, "{total}");
        Ok(())
    }

    #[test]
    fn the_peak_ultraviolet_hour_is_found() -> Result<(), LandError> {
        let record = record()?;
        assert_eq!(record.peak_uv().and_then(|hour| hour.uv_index), Some(0.3));
        Ok(())
    }

    #[test]
    fn a_moisture_outside_the_physical_range_is_dropped() {
        let body = r#"{
            "latitude": 0.0, "longitude": 0.0,
            "hourly": {"time": ["2026-11-02T00:00"], "soil_moisture_0_to_10cm": [1.8]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let Ok(record) = parse(&value) else {
            return;
        };
        assert_eq!(
            record.hours.first().and_then(|hour| hour.moisture_top),
            None
        );
    }

    #[test]
    fn the_request_asks_for_the_soil_and_the_sun() {
        let url = request_url(37.2, -97.5);
        assert!(url.contains("soil_moisture_0_to_10cm"), "{url}");
        assert!(url.contains("soil_moisture_10_to_40cm"), "{url}");
        assert!(url.contains("et0_fao_evapotranspiration"), "{url}");
        assert!(url.contains("uv_index"), "{url}");
        assert!(url.contains("past_days=1"), "{url}");
    }
}
