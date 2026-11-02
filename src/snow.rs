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
//! Snow depth and snowfall from the forecast model.
//!
//! The model carries a snow depth for its grid box and the snowfall it expects
//! hour by hour. The grid elevation comes back with the record, which is the
//! difference between a ridge and the valley beside it at the same coordinate.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Open-Meteo snow model";

const BASE: &str = "https://api.open-meteo.com/v1/forecast";
const HOURLY: &str = "snow_depth,snowfall";
const PAST_DAYS: u8 = 1;
const FORECAST_DAYS: u8 = 3;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// The depth at which a snowpack is worth calling a snowpack.
pub const DEEP_CM: f64 = 30.0;

/// Why a snow record could not be read.
#[derive(Debug)]
pub enum SnowError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for SnowError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The snowpack request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The snowpack record is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => write!(
                formatter,
                "The snowpack worker stopped, so no snow can be requested."
            ),
        }
    }
}

impl Error for SnowError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl SnowError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// One hour of the snow model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnowHour {
    pub time_ms: u64,
    /// Snow on the ground, in metres as the provider reports it.
    pub depth_m: Option<f64>,
    /// Snow that falls in this hour, in centimetres.
    pub snowfall_cm: Option<f64>,
}

impl SnowHour {
    /// The depth in centimetres, which is how a chaser reads a snowpack.
    pub fn depth_cm(&self) -> Option<f64> {
        self.depth_m.map(|metres| metres * 100.0)
    }
}

/// The snow model at one point.
#[derive(Debug, Clone, PartialEq)]
pub struct Snowpack {
    pub latitude: f64,
    pub longitude: f64,
    /// The elevation of the grid box the model answered for.
    pub elevation_m: Option<f64>,
    pub units: BTreeMap<String, String>,
    pub hours: Vec<SnowHour>,
}

impl Snowpack {
    pub fn unit(&self, field: &str) -> &str {
        self.units
            .get(field)
            .map_or("unit not stated", String::as_str)
    }

    /// The newest hour at or before `now_ms`.
    pub fn at(&self, now_ms: u64) -> Option<SnowHour> {
        self.hours
            .iter()
            .rev()
            .find(|hour| hour.time_ms <= now_ms)
            .copied()
    }

    /// The snow that falls between two instants, in centimetres.
    pub fn snowfall_between(&self, from_ms: u64, to_ms: u64) -> f64 {
        self.hours
            .iter()
            .filter(|hour| hour.time_ms > from_ms && hour.time_ms <= to_ms)
            .filter_map(|hour| hour.snowfall_cm)
            .sum()
    }

    /// The deepest hour in the record.
    pub fn deepest(&self) -> Option<SnowHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.depth_cm().map(|depth| (depth, *hour)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, hour)| hour)
    }

    /// The heaviest hour of snowfall in the record.
    pub fn heaviest_fall(&self) -> Option<SnowHour> {
        self.hours
            .iter()
            .filter_map(|hour| hour.snowfall_cm.map(|fall| (fall, *hour)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, hour)| hour)
    }

    /// The snowfall the chart's axis has to hold, never below a centimetre an
    /// hour so a light day is not drawn as a blizzard.
    pub fn axis_top(&self) -> f64 {
        let heaviest = match self.heaviest_fall().and_then(|hour| hour.snowfall_cm) {
            Some(fall) => fall,
            None => return 1.0,
        };
        // A whole centimetre is a lot of snow for one hour, so a record of
        // light flurries is scaled to tenths, where its bars are readable.
        for top in [0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0] {
            if heaviest <= top {
                return top;
            }
        }
        heaviest.ceil()
    }
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> SnowError {
    SnowError::Shape { detail }
}

/// Reads one series, checking it against the hour count.
fn series(
    row: &Map<String, Value>,
    field: &str,
    count: usize,
) -> Result<Vec<Option<f64>>, SnowError> {
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
pub fn parse(document: &Value) -> Result<Snowpack, SnowError> {
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
    let depths = series(hourly, "snow_depth", count)?;
    let falls = series(hourly, "snowfall", count)?;
    let mut hours = Vec::with_capacity(count);
    for (position, text) in times.iter().enumerate() {
        let Some(text) = text.as_str() else {
            return Err(shape("every hourly time written as text"));
        };
        let Some(time_ms) = time::parse_iso_utc(text) else {
            return Err(shape("every hourly time written as ISO 8601"));
        };
        hours.push(SnowHour {
            time_ms,
            depth_m: depths.get(position).copied().flatten(),
            snowfall_cm: falls.get(position).copied().flatten(),
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
    Ok(Snowpack {
        latitude,
        longitude,
        elevation_m: document.get("elevation").and_then(Value::as_f64),
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

/// Fetches the snow model for one point.
pub fn fetch(latitude: f64, longitude: f64) -> Result<Snowpack, SnowError> {
    let document = provider::get_json(&request_url(latitude, longitude), "application/json")
        .map_err(|source| SnowError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct SnowRequest {
    pub generation: u64,
    pub latitude: f64,
    pub longitude: f64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum SnowEvent {
    Ready { generation: u64, record: Snowpack },
    Failed { generation: u64, error: SnowError },
}

/// Fetches the snow model off the UI thread.
pub struct SnowFeed {
    requests: Sender<SnowRequest>,
    events: Receiver<SnowEvent>,
    generation: u64,
    stopped: bool,
}

impl SnowFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<SnowRequest>();
        let (event_sender, events) = mpsc::channel::<SnowEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch(request.latitude, request.longitude) {
                    Ok(record) => SnowEvent::Ready {
                        generation: request.generation,
                        record,
                    },
                    Err(error) => SnowEvent::Failed {
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

    pub fn request(&mut self, request: SnowRequest) -> Result<(), SnowError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(SnowError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<SnowEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(SnowEvent::Failed {
                    generation: self.generation,
                    error: SnowError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum SnowState {
    Idle,
    Loading {
        last: Option<Snowpack>,
    },
    Ready {
        record: Snowpack,
    },
    Failed {
        error: SnowError,
        last: Option<Snowpack>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl SnowState {
    /// The most recent record still worth drawing.
    pub const fn last(&self) -> Option<&Snowpack> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct SnowView {
    feed: SnowFeed,
    state: SnowState,
    generation: u64,
}

impl SnowView {
    pub fn new() -> Self {
        Self {
            feed: SnowFeed::new(),
            state: SnowState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &SnowState {
        &self.state
    }

    pub const fn record(&self) -> Option<&Snowpack> {
        self.state.last()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = SnowState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = SnowRequest {
            generation: self.generation,
            latitude,
            longitude,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = SnowState::Failed {
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
                SnowEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = SnowState::Ready { record };
                }
                SnowEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = SnowState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            SnowState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            SnowState::Idle | SnowState::Loading { .. } | SnowState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            SnowState::Failed { attempts, .. } => *attempts,
            SnowState::Idle | SnowState::Loading { .. } | SnowState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            SnowState::Failed { retry_due, .. } => retry_due.is_some(),
            SnowState::Idle | SnowState::Loading { .. } | SnowState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut SnowState) -> Option<Snowpack> {
    match std::mem::replace(state, SnowState::Idle) {
        SnowState::Loading { last } | SnowState::Failed { last, .. } => last,
        SnowState::Ready { record } => Some(record),
        SnowState::Idle => None,
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
            "latitude": 46.5,
            "longitude": 10.0,
            "elevation": 2380.0,
            "hourly_units": {"time": "iso8601", "snow_depth": "m", "snowfall": "cm"},
            "hourly": {
                "time": ["2026-11-01T00:00", "2026-11-01T01:00", "2026-11-01T02:00", "2026-11-01T03:00"],
                "snow_depth": [0.03, 0.05, null, 0.09],
                "snowfall": [0.0, 2.0, 1.5, 0.0]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn record() -> Result<Snowpack, SnowError> {
        parse(&document())
    }

    #[test]
    fn a_record_keeps_every_hour_and_the_grid_elevation() -> Result<(), SnowError> {
        let record = record()?;
        assert_eq!(record.hours.len(), 4);
        assert_eq!(record.hours[0].depth_cm(), Some(3.0));
        assert_eq!(record.elevation_m, Some(2380.0));
        assert_eq!(record.unit("snowfall"), "cm");
        assert_eq!(
            record.hours[2].depth_m, None,
            "a missing hour stays missing"
        );
        Ok(())
    }

    #[test]
    fn the_snowfall_of_a_window_is_the_sum_of_its_hours() -> Result<(), SnowError> {
        let record = record()?;
        let start = record.hours[0].time_ms;
        assert_eq!(record.snowfall_between(start, start + 3_600_000), 2.0);
        assert_eq!(record.snowfall_between(start, start + 5 * 3_600_000), 3.5);
        assert_eq!(record.snowfall_between(start, start), 0.0);
        Ok(())
    }

    #[test]
    fn the_deepest_and_the_heaviest_hours_are_found_separately() -> Result<(), SnowError> {
        let record = record()?;
        assert_eq!(record.deepest().and_then(|hour| hour.depth_cm()), Some(9.0));
        assert_eq!(
            record.heaviest_fall().and_then(|hour| hour.snowfall_cm),
            Some(2.0)
        );
        Ok(())
    }

    #[test]
    fn a_quiet_chart_does_not_claim_a_centimetre_an_hour() -> Result<(), SnowError> {
        let body = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "hourly": {
                "time": ["2026-11-01T00:00"],
                "snow_depth": [0.0],
                "snowfall": [0.0]
            }
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Err(shape("a readable test document"));
        };
        let record = parse(&value)?;
        assert_eq!(record.axis_top(), 0.2);
        Ok(())
    }

    #[test]
    fn a_flurry_chart_is_scaled_to_the_flurry() -> Result<(), SnowError> {
        let body = r#"{
            "latitude": 46.5, "longitude": 10.0,
            "hourly": {
                "time": ["2026-11-01T00:00", "2026-11-01T01:00"],
                "snow_depth": [0.04, 0.04],
                "snowfall": [0.0, 0.3]
            }
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return Err(shape("a readable test document"));
        };
        let record = parse(&value)?;
        assert_eq!(record.axis_top(), 0.5);
        Ok(())
    }

    #[test]
    fn a_negative_depth_is_refused() {
        let body = r#"{
            "latitude": 39.0, "longitude": -96.0,
            "hourly": {"time": ["2026-11-01T00:00"], "snow_depth": [-0.5]}
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(parse(&value).is_err());
    }

    #[test]
    fn the_request_names_the_point_and_the_window() {
        let url = request_url(46.5, 10.0);
        assert!(url.contains("latitude=46.500"), "{url}");
        assert!(url.contains("longitude=10.000"), "{url}");
        assert!(url.contains("snow_depth"), "{url}");
        assert!(url.contains("past_days=1"), "{url}");
        assert!(url.contains("forecast_days=3"), "{url}");
    }
}
