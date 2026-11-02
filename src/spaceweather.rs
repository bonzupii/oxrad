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
//! Geomagnetic activity and the solar wind, from the NOAA space weather
//! prediction centre.
//!
//! The Kp index is a three-hour planetary average of geomagnetic disturbance,
//! reported with the storm scale that follows from it. The solar-wind numbers
//! are the current measurement at L1, which is what the aurora will be made of
//! an hour later, not a local measurement.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "NOAA Space Weather Prediction Center";

const KP_URL: &str = "https://services.swpc.noaa.gov/products/noaa-planetary-k-index-forecast.json";
const WIND_URL: &str = "https://services.swpc.noaa.gov/products/summary/solar-wind-speed.json";
const MAG_URL: &str = "https://services.swpc.noaa.gov/products/summary/solar-wind-mag-field.json";
const REQUEST_EVERY: Duration = Duration::from_secs(600);

/// Why the space-weather record could not be read.
#[derive(Debug)]
pub enum SpaceWeatherError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for SpaceWeatherError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The space-weather request failed: {source}")
            }
            Self::Shape { detail } => write!(
                formatter,
                "The space-weather record is not usable: it wants {detail}."
            ),
            Self::WorkerStopped => write!(
                formatter,
                "The space-weather worker stopped, so no geomagnetic data can be requested."
            ),
        }
    }
}

impl Error for SpaceWeatherError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl SpaceWeatherError {
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// Where a Kp value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KpStatus {
    /// Measured by the ground magnetometer network.
    Observed,
    /// Derived from an incomplete set of stations, as the newest value often is.
    Estimated,
    /// A model prediction for a time that has not happened yet.
    Predicted,
}

impl KpStatus {
    pub const fn is_forecast(self) -> bool {
        matches!(self, Self::Predicted)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Estimated => "estimated",
            Self::Predicted => "predicted",
        }
    }
}

/// One three-hour Kp value, observed, estimated, or predicted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KpPoint {
    pub time_ms: u64,
    pub kp: f64,
    pub status: KpStatus,
}

/// The current measurement at L1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolarWind {
    pub speed_km_s: Option<f64>,
    pub bt_nt: Option<f64>,
    pub bz_nt: Option<f64>,
    pub time_ms: u64,
}

/// Geomagnetic activity and the solar wind, as one record.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceWeather {
    pub points: Vec<KpPoint>,
    pub wind: Option<SolarWind>,
    /// Why the wind is missing, when it is: the Kp index is still usable.
    pub wind_note: Option<String>,
}

impl SpaceWeather {
    /// The newest measured or estimated Kp, which is what the ground did.
    pub fn latest_observed(&self) -> Option<KpPoint> {
        self.points
            .iter()
            .rev()
            .find(|point| !point.status.is_forecast())
            .copied()
    }

    /// How many points are estimates rather than full measurements.
    pub fn estimated_count(&self) -> usize {
        self.points
            .iter()
            .filter(|point| point.status == KpStatus::Estimated)
            .count()
    }

    /// The newest value in the record, observed or predicted.
    pub fn latest(&self) -> Option<KpPoint> {
        self.points.last().copied()
    }

    /// The strongest value in the record.
    pub fn peak(&self) -> Option<KpPoint> {
        self.points.iter().copied().fold(None, |best, point| {
            best.map_or(Some(point), |held| {
                if point.kp > held.kp {
                    Some(point)
                } else {
                    Some(held)
                }
            })
        })
    }

    /// How many points are still to come.
    pub fn predicted_count(&self) -> usize {
        self.points
            .iter()
            .filter(|point| point.status.is_forecast())
            .count()
    }
}

/// The NOAA storm scale for a Kp value, or the quiet label below the first.
pub const fn storm_scale(kp: f64) -> &'static str {
    if kp >= 9.0 {
        "G5 extreme"
    } else if kp >= 8.0 {
        "G4 severe"
    } else if kp >= 7.0 {
        "G3 strong"
    } else if kp >= 6.0 {
        "G2 moderate"
    } else if kp >= 5.0 {
        "G1 minor"
    } else {
        "below storm level"
    }
}

const fn shape(detail: &'static str) -> SpaceWeatherError {
    SpaceWeatherError::Shape { detail }
}

/// Reads the Kp forecast product: a list of three-hour values, each marked
/// observed or predicted.
pub fn parse_kp(document: &Value) -> Result<Vec<KpPoint>, SpaceWeatherError> {
    let Some(rows) = document.as_array() else {
        return Err(shape("a list of Kp rows"));
    };
    let mut points = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(stamp) = row.get("time_tag").and_then(Value::as_str) else {
            return Err(shape("a time on every Kp row"));
        };
        let Some(kp) = row.get("kp").and_then(Value::as_f64) else {
            return Err(shape("a Kp value on every row"));
        };
        if !kp.is_finite() || !(0.0..=9.0).contains(&kp) {
            return Err(shape("Kp values between zero and nine"));
        }
        let Some(time_ms) = time::parse_iso_utc(stamp) else {
            return Err(shape("timestamps this program can order"));
        };
        let status = match row.get("observed").and_then(Value::as_str) {
            Some("observed") => KpStatus::Observed,
            Some("estimated") => KpStatus::Estimated,
            Some("predicted") => KpStatus::Predicted,
            _ => return Err(shape("every row marked observed, estimated, or predicted")),
        };
        points.push(KpPoint {
            time_ms,
            kp,
            status,
        });
    }
    points.sort_by_key(|point| point.time_ms);
    if points.is_empty() {
        return Err(shape("at least one Kp value"));
    }
    Ok(points)
}

/// Reads the solar-wind speed summary, whose one row carries the latest value.
pub fn parse_wind(speed: &Value, magnetic: &Value) -> Result<SolarWind, SpaceWeatherError> {
    let row = speed
        .as_array()
        .and_then(|rows| rows.first())
        .ok_or(shape("a solar-wind speed row"))?;
    let Some(stamp) = row.get("time_tag").and_then(Value::as_str) else {
        return Err(shape("a time on the solar-wind row"));
    };
    let Some(time_ms) = time::parse_iso_utc(stamp) else {
        return Err(shape("timestamps this program can order"));
    };
    let speed_km_s = row
        .get("proton_speed")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0);
    let magnetic_row = magnetic.as_array().and_then(|rows| rows.first());
    let bt_nt = magnetic_row
        .and_then(|row| row.get("bt"))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0);
    let bz_nt = magnetic_row
        .and_then(|row| row.get("bz_gsm"))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite());
    if speed_km_s.is_none() && bt_nt.is_none() && bz_nt.is_none() {
        return Err(shape("at least one solar-wind measurement"));
    }
    Ok(SolarWind {
        speed_km_s,
        bt_nt,
        bz_nt,
        time_ms,
    })
}

/// Fetches the whole record. The Kp index is required; a missing wind is
/// reported as a note beside it rather than taking the record down.
pub fn fetch() -> Result<SpaceWeather, SpaceWeatherError> {
    let kp_document = provider::get_json(KP_URL, "application/json")
        .map_err(|source| SpaceWeatherError::Provider { source })?;
    let points = parse_kp(&kp_document)?;
    let (wind, wind_note) = match fetch_wind() {
        Ok(wind) => (Some(wind), None),
        Err(error) => (None, Some(format!("{error}"))),
    };
    Ok(SpaceWeather {
        points,
        wind,
        wind_note,
    })
}

fn fetch_wind() -> Result<SolarWind, SpaceWeatherError> {
    let speed = provider::get_json(WIND_URL, "application/json")
        .map_err(|source| SpaceWeatherError::Provider { source })?;
    let magnetic = provider::get_json(MAG_URL, "application/json")
        .map_err(|source| SpaceWeatherError::Provider { source })?;
    parse_wind(&speed, &magnetic)
}

#[derive(Debug)]
pub enum SpaceWeatherEvent {
    Ready {
        generation: u64,
        record: SpaceWeather,
    },
    Failed {
        generation: u64,
        error: SpaceWeatherError,
    },
}

/// Fetches the record off the UI thread.
pub struct SpaceWeatherFeed {
    requests: Sender<u64>,
    events: Receiver<SpaceWeatherEvent>,
    generation: u64,
    stopped: bool,
}

impl SpaceWeatherFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<u64>();
        let (event_sender, events) = mpsc::channel::<SpaceWeatherEvent>();
        std::thread::spawn(move || {
            while let Ok(generation) = request_receiver.recv() {
                let event = match fetch() {
                    Ok(record) => SpaceWeatherEvent::Ready { generation, record },
                    Err(error) => SpaceWeatherEvent::Failed { generation, error },
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

    pub fn request(&mut self, generation: u64) -> Result<(), SpaceWeatherError> {
        self.generation = generation;
        if self.requests.send(generation).is_err() {
            self.stopped = true;
            return Err(SpaceWeatherError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<SpaceWeatherEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(SpaceWeatherEvent::Failed {
                    generation: self.generation,
                    error: SpaceWeatherError::WorkerStopped,
                })
            }
        }
    }
}

#[derive(Debug)]
pub enum SpaceWeatherState {
    Idle,
    Loading {
        last: Option<SpaceWeather>,
    },
    Ready {
        record: SpaceWeather,
    },
    Failed {
        error: SpaceWeatherError,
        last: Option<SpaceWeather>,
        retry_due: Option<Instant>,
    },
}

impl SpaceWeatherState {
    pub const fn last(&self) -> Option<&SpaceWeather> {
        match self {
            Self::Ready { record } => Some(record),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker, its state, and when the next request is due.
pub struct SpaceWeatherView {
    feed: SpaceWeatherFeed,
    state: SpaceWeatherState,
    generation: u64,
    next_request: Option<Instant>,
    failures: u32,
}

impl SpaceWeatherView {
    pub fn new() -> Self {
        Self {
            feed: SpaceWeatherFeed::new(),
            state: SpaceWeatherState::Idle,
            generation: 0,
            next_request: None,
            failures: 0,
        }
    }

    pub const fn state(&self) -> &SpaceWeatherState {
        &self.state
    }

    pub const fn record(&self) -> Option<&SpaceWeather> {
        self.state.last()
    }

    pub fn start(&mut self, now: Instant) {
        let previous = std::mem::replace(&mut self.state, SpaceWeatherState::Idle);
        let last = match previous {
            SpaceWeatherState::Loading { last } | SpaceWeatherState::Failed { last, .. } => last,
            SpaceWeatherState::Ready { record } => Some(record),
            SpaceWeatherState::Idle => None,
        };
        self.state = SpaceWeatherState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        self.next_request = now.checked_add(REQUEST_EVERY);
        if let Err(error) = self.feed.request(self.generation) {
            let last = match std::mem::replace(&mut self.state, SpaceWeatherState::Idle) {
                SpaceWeatherState::Loading { last } => last,
                other => other.last().cloned(),
            };
            self.failed(error, last, now);
        }
    }

    /// Takes in the worker's answer and asks again when the record is due.
    pub fn update(&mut self, now: Instant) {
        while let Some(event) = self.feed.poll() {
            match event {
                SpaceWeatherEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.failures = 0;
                    self.state = SpaceWeatherState::Ready { record };
                }
                SpaceWeatherEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = match std::mem::replace(&mut self.state, SpaceWeatherState::Idle) {
                        SpaceWeatherState::Loading { last }
                        | SpaceWeatherState::Failed { last, .. } => last,
                        SpaceWeatherState::Ready { record } => Some(record),
                        SpaceWeatherState::Idle => None,
                    };
                    self.failed(error, last, now);
                }
            }
        }
        let due = self.next_request.is_some_and(|at| now >= at);
        let ready = matches!(self.state, SpaceWeatherState::Ready { .. });
        if due && ready {
            self.start(now);
        }
    }

    fn failed(&mut self, error: SpaceWeatherError, last: Option<SpaceWeather>, now: Instant) {
        let transient = error.is_transient();
        self.failures = self.failures.saturating_add(1);
        let retry_due = if transient && self.failures <= 4 {
            now.checked_add(Duration::from_secs(u64::from(self.failures) * 20))
        } else {
            None
        };
        self.next_request = retry_due.or_else(|| now.checked_add(REQUEST_EVERY));
        self.state = SpaceWeatherState::Failed {
            error,
            last,
            retry_due,
        };
    }

    /// Whether a retry is waiting.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            SpaceWeatherState::Failed { retry_due, .. } => retry_due.is_some(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kp_document() -> Value {
        let body = r#"[
            {"time_tag": "2026-11-01T00:00:00", "kp": 1.67, "observed": "observed", "noaa_scale": null},
            {"time_tag": "2026-11-01T03:00:00", "kp": 5.33, "observed": "observed", "noaa_scale": "G1"},
            {"time_tag": "2026-11-01T04:30:00", "kp": 5.67, "observed": "estimated", "noaa_scale": "G1"},
            {"time_tag": "2026-11-01T06:00:00", "kp": 6.00, "observed": "predicted", "noaa_scale": "G2"},
            {"time_tag": "2026-11-01T09:00:00", "kp": 2.00, "observed": "predicted", "noaa_scale": null}
        ]"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn wind_documents() -> (Value, Value) {
        let speed = r#"[{"proton_speed": 492, "time_tag": "2026-11-01T20:24:00Z"}]"#;
        let magnetic = r#"[{"bt": 4, "bz_gsm": -2, "time_tag": "2026-11-01T20:24:00Z"}]"#;
        (
            serde_json::from_str(speed).unwrap_or(Value::Null),
            serde_json::from_str(magnetic).unwrap_or(Value::Null),
        )
    }

    #[test]
    fn the_kp_rows_keep_their_order_and_their_status() -> Result<(), SpaceWeatherError> {
        let points = parse_kp(&kp_document())?;
        assert_eq!(points.len(), 5);
        assert_eq!(points[0].status, KpStatus::Observed);
        assert_eq!(points[2].status, KpStatus::Estimated);
        assert_eq!(points[4].status, KpStatus::Predicted);
        assert!(points[0].time_ms < points[1].time_ms);
        Ok(())
    }

    #[test]
    fn the_newest_observed_value_is_not_a_prediction() -> Result<(), SpaceWeatherError> {
        let record = SpaceWeather {
            points: parse_kp(&kp_document())?,
            wind: None,
            wind_note: None,
        };
        let observed = record.latest_observed().ok_or(SpaceWeatherError::Shape {
            detail: "an observed value",
        })?;
        assert_eq!(
            observed.kp, 5.67,
            "an estimate is still what the ground did"
        );
        assert_eq!(record.estimated_count(), 1);
        assert_eq!(
            record.latest().map(|point| point.kp),
            Some(2.0),
            "the newest value in the record is the last prediction"
        );
        assert_eq!(record.peak().map(|point| point.kp), Some(6.0));
        assert_eq!(record.predicted_count(), 2);
        Ok(())
    }

    #[test]
    fn the_storm_scale_follows_the_index() {
        assert_eq!(storm_scale(4.99), "below storm level");
        assert_eq!(storm_scale(5.0), "G1 minor");
        assert_eq!(storm_scale(6.0), "G2 moderate");
        assert_eq!(storm_scale(7.0), "G3 strong");
        assert_eq!(storm_scale(8.0), "G4 severe");
        assert_eq!(storm_scale(9.0), "G5 extreme");
        assert_eq!(storm_scale(12.0), "G5 extreme");
    }

    #[test]
    fn a_kp_value_outside_the_scale_is_refused() {
        let body = r#"[{"time_tag": "2026-11-01T00:00:00", "kp": 11.0, "observed": "observed"}]"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(parse_kp(&value).is_err());
    }

    #[test]
    fn a_row_without_a_status_is_refused() {
        let body = r#"[{"time_tag": "2026-11-01T00:00:00", "kp": 2.0}]"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(parse_kp(&value).is_err());
    }

    #[test]
    fn the_wind_summary_keeps_its_measurements() -> Result<(), SpaceWeatherError> {
        let (speed, magnetic) = wind_documents();
        let wind = parse_wind(&speed, &magnetic)?;
        assert_eq!(wind.speed_km_s, Some(492.0));
        assert_eq!(wind.bt_nt, Some(4.0));
        assert_eq!(wind.bz_nt, Some(-2.0));
        assert_eq!(
            wind.time_ms % time::MS_PER_DAY,
            20 * 3_600_000 + 24 * 60_000
        );
        Ok(())
    }

    #[test]
    fn a_wind_summary_without_measurements_is_refused() {
        let Ok(empty) = serde_json::from_str::<Value>("[]") else {
            return;
        };
        assert!(parse_wind(&empty, &empty).is_err());
    }
}
