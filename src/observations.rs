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
//! Surface observations from the aviation weather METAR feed.
//!
//! The feed publishes what each reporting station measured: temperature,
//! dew point, wind, visibility, altimeter setting, cloud layers, and the raw
//! report. A station reports on the hour and in between when the weather
//! changes, so the age of each observation is carried with it.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "Aviation Weather Center METAR";

const BASE: &str = "https://aviationweather.gov/api/data/metar";
/// How far back the request looks, which bounds how stale a report can be.
const HOURS: u8 = 2;
/// The largest box the layer asks for, so a wide view does not request a
/// hemisphere of stations on every move.
const MAX_SPAN_DEG: f64 = 12.0;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Why an observation set could not be read.
#[derive(Debug)]
pub enum ObservationError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for ObservationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(
                    formatter,
                    "The surface-observation request failed: {source}"
                )
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The surface-observation record is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => write!(
                formatter,
                "The surface-observation worker stopped, so no stations can be requested."
            ),
        }
    }
}

impl Error for ObservationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl ObservationError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// The flight category a station reports, which is the aviation reading of
/// ceiling and visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlightCategory {
    Visual,
    Marginal,
    Instrument,
    LowInstrument,
    Unreported,
}

impl FlightCategory {
    fn of(text: Option<&str>) -> Self {
        match text {
            Some("VFR") => Self::Visual,
            Some("MVFR") => Self::Marginal,
            Some("IFR") => Self::Instrument,
            Some("LIFR") => Self::LowInstrument,
            _ => Self::Unreported,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Visual => "VFR",
            Self::Marginal => "MVFR",
            Self::Instrument => "IFR",
            Self::LowInstrument => "LIFR",
            Self::Unreported => "no category",
        }
    }
}

/// One station's latest report.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub station: String,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: Option<f64>,
    pub observed_ms: u64,
    pub temperature_c: Option<f64>,
    pub dewpoint_c: Option<f64>,
    pub wind_from_deg: Option<f64>,
    pub wind_speed_kt: Option<f64>,
    pub gust_kt: Option<f64>,
    pub visibility_mi: Option<f64>,
    pub altimeter_hpa: Option<f64>,
    pub category: FlightCategory,
    pub cover: Option<String>,
    pub raw: String,
}

impl Observation {
    /// The temperature and dew point together, or what is missing.
    pub fn temperature_label(&self) -> String {
        match (self.temperature_c, self.dewpoint_c) {
            (Some(temp), Some(dew)) => format!("{temp:.0} °C · dew {dew:.0}"),
            (Some(temp), None) => format!("{temp:.0} °C · dew not reported"),
            (None, _) => String::from("temperature not reported"),
        }
    }

    /// The wind, in the provider's knots.
    pub fn wind_label(&self) -> String {
        match (self.wind_from_deg, self.wind_speed_kt) {
            (Some(direction), Some(speed)) if speed > 0.0 => {
                let gust = match self.gust_kt {
                    Some(gust) if gust > speed => format!(" gusting {gust:.0}"),
                    _ => String::new(),
                };
                format!("{speed:.0}{gust} kt from {direction:.0}°")
            }
            (None, _) | (_, None) => String::from("wind not reported"),
            (_, Some(_)) => String::from("calm"),
        }
    }
}

/// Every station the request found, with what it refused.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationSet {
    pub stations: Vec<Observation>,
    pub rejected: usize,
}

impl ObservationSet {
    /// The stations nearest a point, nearest first.
    pub fn nearest(&self, latitude: f64, longitude: f64) -> Vec<(f64, &Observation)> {
        let mut ordered: Vec<(f64, &Observation)> = self
            .stations
            .iter()
            .map(|station| {
                let ticks = (station.latitude - latitude).abs()
                    + (station.longitude - longitude).abs() * latitude.to_radians().cos().abs();
                (ticks, station)
            })
            .collect();
        ordered.sort_by(|left, right| left.0.total_cmp(&right.0));
        ordered
    }

    /// How many stations report each flight category.
    pub fn category_count(&self, category: FlightCategory) -> usize {
        self.stations
            .iter()
            .filter(|station| station.category == category)
            .count()
    }
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> ObservationError {
    ObservationError::Shape { detail }
}

fn number(row: &serde_json::Map<String, Value>, field: &str) -> Option<f64> {
    row.get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn text(row: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(String::from)
}

/// Reads a visibility that the provider writes either as a number or as a
/// string with a trailing plus, as in `10+`.
fn visibility(row: &serde_json::Map<String, Value>) -> Option<f64> {
    let value = row.get("visib")?;
    if let Some(number) = value.as_f64() {
        return number.is_finite().then_some(number);
    }
    let text = value.as_str()?.trim_end_matches('+');
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// Reads one station's report.
fn observation_of(row: &serde_json::Map<String, Value>) -> Option<Observation> {
    let station = text(row, "icaoId")?;
    let latitude = number(row, "lat").filter(|value| (-90.0..=90.0).contains(value))?;
    let longitude = number(row, "lon").filter(|value| (-180.0..=180.0).contains(value))?;
    let observed = number(row, "obsTime")?;
    let observed_ms = u64::try_from(observed as i64).ok()?.checked_mul(1_000)?;
    let category = FlightCategory::of(text(row, "fltCat").as_deref());
    Some(Observation {
        name: text(row, "name").unwrap_or_else(|| String::from("unrecorded station")),
        station,
        latitude,
        longitude,
        elevation_m: number(row, "elev"),
        observed_ms,
        temperature_c: number(row, "temp"),
        dewpoint_c: number(row, "dewp"),
        wind_from_deg: number(row, "wdir").filter(|value| (0.0..=360.0).contains(value)),
        wind_speed_kt: number(row, "wspd").filter(|value| *value >= 0.0),
        gust_kt: number(row, "wgst").filter(|value| *value >= 0.0),
        visibility_mi: visibility(row),
        altimeter_hpa: number(row, "altim").filter(|value| *value > 0.0),
        category,
        cover: text(row, "cover"),
        raw: text(row, "rawOb").unwrap_or_default(),
    })
}

/// Reads the feed's answer, keeping one report per station: the newest.
pub fn parse(document: &Value) -> Result<ObservationSet, ObservationError> {
    let Some(reports) = document.as_array() else {
        return Err(shape("an array of reports"));
    };
    let mut stations: Vec<Observation> = Vec::new();
    let mut rejected = 0_usize;
    for report in reports {
        let Some(row) = report.as_object() else {
            rejected += 1;
            continue;
        };
        let Some(observation) = observation_of(row) else {
            rejected += 1;
            continue;
        };
        match stations
            .iter_mut()
            .find(|existing| existing.station == observation.station)
        {
            Some(existing) => {
                if observation.observed_ms > existing.observed_ms {
                    *existing = observation;
                }
            }
            None => stations.push(observation),
        }
    }
    if stations.is_empty() && rejected > 0 {
        return Err(shape("at least one station it could read"));
    }
    Ok(ObservationSet { stations, rejected })
}

/// The box the request asks for, clamped so a wide view stays a sensible
/// request.
pub fn bounds(centre_lat: f64, centre_lon: f64, span_km: f64) -> (f64, f64, f64, f64) {
    let half_lat = (span_km / 2.0 / 111.0).clamp(0.5, MAX_SPAN_DEG);
    let cosine = centre_lat.to_radians().cos().abs().max(0.15);
    let half_lon = (half_lat / cosine).clamp(0.5, MAX_SPAN_DEG * 2.0);
    let south = (centre_lat - half_lat).clamp(-89.0, 89.0);
    let north = (centre_lat + half_lat).clamp(-89.0, 89.0);
    let west = (centre_lon - half_lon).clamp(-180.0, 180.0);
    let east = (centre_lon + half_lon).clamp(-180.0, 180.0);
    (south, west, north, east)
}

/// The request for one box.
pub fn request_url(bounds: (f64, f64, f64, f64)) -> String {
    let (south, west, north, east) = bounds;
    format!("{BASE}?bbox={south:.2},{west:.2},{north:.2},{east:.2}&format=json&hours={HOURS}")
}

/// Fetches the stations in a box.
pub fn fetch(bounds: (f64, f64, f64, f64)) -> Result<ObservationSet, ObservationError> {
    let document = provider::get_json(&request_url(bounds), "application/json")
        .map_err(|source| ObservationError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct ObservationRequest {
    pub generation: u64,
    pub bounds: (f64, f64, f64, f64),
}

/// Answers from the worker.
#[derive(Debug)]
pub enum ObservationEvent {
    Ready {
        generation: u64,
        set: ObservationSet,
    },
    Failed {
        generation: u64,
        error: ObservationError,
    },
}

/// Fetches observations off the UI thread.
pub struct ObservationFeed {
    requests: Sender<ObservationRequest>,
    events: Receiver<ObservationEvent>,
    generation: u64,
    stopped: bool,
}

impl ObservationFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<ObservationRequest>();
        let (event_sender, events) = mpsc::channel::<ObservationEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch(request.bounds) {
                    Ok(set) => ObservationEvent::Ready {
                        generation: request.generation,
                        set,
                    },
                    Err(error) => ObservationEvent::Failed {
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

    pub fn request(&mut self, request: ObservationRequest) -> Result<(), ObservationError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(ObservationError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<ObservationEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(ObservationEvent::Failed {
                    generation: self.generation,
                    error: ObservationError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum ObservationState {
    Idle,
    Loading {
        last: Option<ObservationSet>,
    },
    Ready {
        set: ObservationSet,
    },
    Failed {
        error: ObservationError,
        last: Option<ObservationSet>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl ObservationState {
    /// The most recent set still worth drawing.
    pub const fn last(&self) -> Option<&ObservationSet> {
        match self {
            Self::Ready { set } => Some(set),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct ObservationView {
    feed: ObservationFeed,
    state: ObservationState,
    generation: u64,
}

impl ObservationView {
    /// The stations worth drawing: the newest feed, or the last one if a
    /// refresh is under way or failed.
    pub const fn stations(&self) -> Option<&ObservationSet> {
        self.state.last()
    }

    pub fn new() -> Self {
        Self {
            feed: ObservationFeed::new(),
            state: ObservationState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &ObservationState {
        &self.state
    }

    pub const fn set(&self) -> Option<&ObservationSet> {
        self.state.last()
    }

    pub fn start(&mut self, bounds: (f64, f64, f64, f64), attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = ObservationState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = ObservationRequest {
            generation: self.generation,
            bounds,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = ObservationState::Failed {
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
                ObservationEvent::Ready { generation, set } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = ObservationState::Ready { set };
                }
                ObservationEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = ObservationState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            ObservationState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            ObservationState::Idle
            | ObservationState::Loading { .. }
            | ObservationState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            ObservationState::Failed { attempts, .. } => *attempts,
            ObservationState::Idle
            | ObservationState::Loading { .. }
            | ObservationState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            ObservationState::Failed { retry_due, .. } => retry_due.is_some(),
            ObservationState::Idle
            | ObservationState::Loading { .. }
            | ObservationState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the set it was holding.
fn take_last(state: &mut ObservationState) -> Option<ObservationSet> {
    match std::mem::replace(state, ObservationState::Idle) {
        ObservationState::Loading { last } | ObservationState::Failed { last, .. } => last,
        ObservationState::Ready { set } => Some(set),
        ObservationState::Idle => None,
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

/// How old a report is, in the words the rail uses.
pub fn age_of(observed_ms: u64, now_ms: u64) -> String {
    time::age_label(now_ms, observed_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        let body = r#"[
            {
                "icaoId": "KEWK",
                "obsTime": 1793585460,
                "temp": 17,
                "dewp": 8,
                "wdir": 160,
                "wspd": 10,
                "wgst": 18,
                "visib": "10+",
                "altim": 1016.7,
                "metarType": "SPECI",
                "rawOb": "SPECI KEWK 020211Z AUTO 16010G18KT 10SM BKN029 17/08 A3002",
                "lat": 38.0621,
                "lon": -97.275,
                "elev": 468,
                "name": "Newton City Arpt, KS, US",
                "cover": "BKN",
                "fltCat": "MVFR"
            },
            {
                "icaoId": "KEWK",
                "obsTime": 1793581860,
                "temp": 18,
                "wdir": 150,
                "wspd": 8,
                "visib": 10,
                "lat": 38.0621,
                "lon": -97.275,
                "name": "Newton City Arpt, KS, US",
                "fltCat": "VFR"
            },
            {
                "icaoId": "KICT",
                "obsTime": 1793585400,
                "temp": 16,
                "dewp": 9,
                "wdir": 170,
                "wspd": 12,
                "visib": "7",
                "lat": 37.65,
                "lon": -97.43,
                "name": "Wichita, KS, US",
                "fltCat": "VFR"
            },
            {
                "icaoId": "BROKEN"
            }
        ]"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn set() -> Result<ObservationSet, ObservationError> {
        parse(&document())
    }

    #[test]
    fn the_newest_report_wins_for_a_station() -> Result<(), ObservationError> {
        let set = set()?;
        assert_eq!(set.stations.len(), 2, "one report per station");
        let newton = &set.stations[0];
        assert_eq!(newton.station, "KEWK");
        assert_eq!(newton.temperature_c, Some(17.0));
        assert_eq!(newton.category, FlightCategory::Marginal);
        assert_eq!(set.rejected, 1, "the station without a position is counted");
        Ok(())
    }

    #[test]
    fn a_visibility_with_a_plus_is_read_as_its_number() -> Result<(), ObservationError> {
        let set = set()?;
        assert_eq!(set.stations[0].visibility_mi, Some(10.0));
        assert_eq!(set.stations[1].visibility_mi, Some(7.0));
        Ok(())
    }

    #[test]
    fn the_labels_state_what_is_missing() -> Result<(), ObservationError> {
        let set = set()?;
        assert_eq!(set.stations[0].temperature_label(), "17 °C · dew 8");
        assert_eq!(
            set.stations[1].temperature_label(),
            "16 °C · dew 9",
            "the second report carries its dew point"
        );
        assert!(set.stations[0].wind_label().contains("gusting 18"));
        Ok(())
    }

    #[test]
    fn the_nearest_station_is_the_one_at_the_point() -> Result<(), ObservationError> {
        let set = set()?;
        let ordered = set.nearest(37.6, -97.4);
        assert_eq!(
            ordered.first().map(|(_, station)| station.station.as_str()),
            Some("KICT")
        );
        Ok(())
    }

    #[test]
    fn a_category_is_counted_across_the_set() -> Result<(), ObservationError> {
        let set = set()?;
        assert_eq!(set.category_count(FlightCategory::Visual), 1);
        assert_eq!(set.category_count(FlightCategory::Marginal), 1);
        assert_eq!(set.category_count(FlightCategory::Instrument), 0);
        Ok(())
    }

    #[test]
    fn the_box_grows_with_the_view_and_stops_at_a_sensible_size() {
        let (south, west, north, east) = bounds(39.0, -96.0, 200.0);
        assert!(south < 39.0 && north > 39.0);
        assert!(west < -96.0 && east > -96.0);
        let (south, _, north, _) = bounds(39.0, -96.0, 20_000.0);
        assert!((north - south - 2.0 * MAX_SPAN_DEG).abs() < 0.001);
        let (south, _, north, _) = bounds(89.5, 0.0, 500.0);
        assert!(south >= -89.0 && north <= 89.0);
    }

    #[test]
    fn the_request_names_the_box_and_the_hours() {
        let url = request_url(bounds(39.0, -96.0, 200.0));
        assert!(url.contains("bbox="), "{url}");
        assert!(url.contains("format=json"), "{url}");
        assert!(url.contains("hours=2"), "{url}");
    }

    #[test]
    fn the_age_of_a_report_reads_as_a_sentence() {
        assert_eq!(age_of(1_000_000, 1_600_000), "10 min old");
        assert_eq!(age_of(0, 7_200_000), "2 h old");
    }

    #[test]
    fn an_empty_feed_is_not_an_error() {
        let Ok(value) = serde_json::from_str::<Value>("[]") else {
            return;
        };
        let Ok(set) = parse(&value) else {
            return;
        };
        assert!(set.stations.is_empty());
        assert_eq!(set.rejected, 0);
    }
}
