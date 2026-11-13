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
//! Tropical cyclones: the storms the global disaster alert service is
//! carrying, wherever in the world they are.
//!
//! Each entry is one cyclone episode with the position its issuing centre last
//! reported, the maximum sustained wind the service published for it, the
//! countries it affects, and the dates it covers. The service's alert level is
//! its own judgement of the humanitarian impact, not a category on any
//! hurricane scale, so the layer says which is which and never renders one as
//! the other.
//!
//! The list is global: the western Pacific, the Indian Ocean, the Atlantic and
//! the eastern Pacific all arrive in the same response, so a storm that no
//! American agency is tracking still appears.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Instant;

use serde_json::Value;

use crate::acquire::Acquire;
use crate::engine::Served;
use crate::provider::FetchError;
use crate::sources;
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "GDACS global disaster alerts";
/// How far back the list reaches, in days.
pub const WINDOW_DAYS: i64 = 30;
const MS_PER_DAY: u64 = 86_400_000;

/// The service's alert level, lowest first. It measures expected humanitarian
/// impact, not wind speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Alert {
    Green,
    Orange,
    Red,
}

impl Alert {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "green" => Some(Self::Green),
            "orange" => Some(Self::Orange),
            "red" => Some(Self::Red),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Orange => "orange",
            Self::Red => "red",
        }
    }

    /// What the level means, in the service's own terms.
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Green => "a local event with little international impact expected",
            Self::Orange => "a local event with some international impact expected",
            Self::Red => "an event with a potentially severe international impact",
        }
    }
}

/// One cyclone.
#[derive(Debug, Clone, PartialEq)]
pub struct Cyclone {
    pub event_id: u64,
    /// The storm's own name, as its issuing centre gave it.
    pub name: String,
    /// The service's longer title for the event.
    pub title: String,
    pub alert: Option<Alert>,
    pub alert_text: String,
    /// The maximum sustained wind the service published, in its own unit.
    pub wind: Option<f64>,
    pub wind_text: String,
    pub country: Option<String>,
    pub source: String,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    /// Whether the service still considers the episode in progress.
    pub current: bool,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub report_url: Option<String>,
}

impl Cyclone {
    /// The position, when the service published a usable one.
    pub fn position(&self) -> Option<(f64, f64)> {
        match (self.latitude, self.longitude) {
            (Some(latitude), Some(longitude))
                if (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude) =>
            {
                Some((latitude, longitude))
            }
            (Some(_) | None, Some(_) | None) => None,
        }
    }

    /// How many days the episode covers, when both dates are known.
    pub fn days(&self) -> Option<u64> {
        let from = self.from_ms?;
        let to = self.to_ms?;
        Some(to.saturating_sub(from) / MS_PER_DAY + 1)
    }
}

/// The storms in the window.
#[derive(Debug, Clone, PartialEq)]
pub struct StormList {
    pub cyclones: Vec<Cyclone>,
    /// Events the service sent that could not be read.
    pub rejected: usize,
}

impl StormList {
    /// The storms whose position is known.
    pub fn located(&self) -> impl Iterator<Item = &Cyclone> {
        self.cyclones
            .iter()
            .filter(|cyclone| cyclone.position().is_some())
    }

    /// How many episodes the service still considers in progress.
    pub fn active(&self) -> usize {
        self.cyclones
            .iter()
            .filter(|cyclone| cyclone.current)
            .count()
    }

    /// The strongest storm by published wind.
    pub fn strongest(&self) -> Option<&Cyclone> {
        self.cyclones
            .iter()
            .filter_map(|cyclone| cyclone.wind.map(|wind| (wind, cyclone)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, cyclone)| cyclone)
    }

    /// The highest alert level in the list.
    pub fn highest(&self) -> Option<Alert> {
        self.cyclones
            .iter()
            .filter_map(|cyclone| cyclone.alert)
            .max()
    }

    /// The most recently updated episode.
    pub fn newest(&self) -> Option<&Cyclone> {
        self.cyclones
            .iter()
            .filter(|cyclone| cyclone.to_ms.is_some())
            .max_by_key(|cyclone| cyclone.to_ms)
    }
}

/// Why a storm list could not be read.
#[derive(Debug)]
pub enum CycloneError {
    Provider { source: FetchError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for CycloneError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The tropical-cyclone request failed: {source}")
            }
            Self::Shape { detail } => write!(
                formatter,
                "The tropical-cyclone list is not usable: it wants {detail}."
            ),
            Self::WorkerStopped => write!(
                formatter,
                "The cyclone worker stopped, so the storm list cannot be read."
            ),
        }
    }
}

impl Error for CycloneError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

const fn shape(detail: &'static str) -> CycloneError {
    CycloneError::Shape { detail }
}

/// Reads the service's feature collection.
pub fn parse(document: &Value) -> Result<StormList, CycloneError> {
    let Some(features) = document.get("features").and_then(Value::as_array) else {
        return Err(shape("a list of cyclone features"));
    };
    let mut cyclones = Vec::with_capacity(features.len());
    let mut rejected = 0_usize;
    for feature in features {
        match parse_feature(feature) {
            Some(cyclone) => cyclones.push(cyclone),
            None => rejected += 1,
        }
    }
    if cyclones.is_empty() && rejected > 0 {
        return Err(shape("at least one cyclone it could read"));
    }
    Ok(StormList { cyclones, rejected })
}

/// Reads one feature, or nothing when it names no storm.
fn parse_feature(feature: &Value) -> Option<Cyclone> {
    let properties = feature.get("properties")?.as_object()?;
    let event_id = properties.get("eventid")?.as_u64()?;
    let name = text(properties, "eventname")?;
    let coordinates = feature
        .get("geometry")
        .and_then(|geometry| geometry.get("coordinates"))
        .and_then(Value::as_array);
    let latitude = coordinates
        .and_then(|coordinates| coordinates.get(1))
        .and_then(Value::as_f64);
    let longitude = coordinates
        .and_then(|coordinates| coordinates.first())
        .and_then(Value::as_f64);
    let alert_text = text(properties, "alertlevel").unwrap_or_else(|| String::from("not stated"));
    let severity = properties.get("severitydata");
    let wind = severity
        .and_then(|severity| severity.get("severity"))
        .and_then(Value::as_f64)
        .filter(|wind| wind.is_finite() && *wind > 0.0);
    let unit = severity
        .and_then(|severity| severity.get("severityunit"))
        .and_then(Value::as_str)
        .unwrap_or("no unit stated");
    let wind_text = match (wind, severity.and_then(Value::as_object)) {
        (Some(_), Some(severity)) => {
            text(severity, "severitytext").unwrap_or_else(|| String::from(unit))
        }
        _ => String::from("no wind published"),
    };
    Some(Cyclone {
        event_id,
        name,
        title: text(properties, "description").unwrap_or_else(|| String::from("cyclone")),
        alert: Alert::parse(&alert_text),
        alert_text,
        wind: wind.map(|wind| match unit {
            "km/h" => wind,
            "m/s" => wind * 3.6,
            _ => wind,
        }),
        wind_text,
        country: text(properties, "country"),
        source: text(properties, "source").unwrap_or_else(|| String::from("not stated")),
        from_ms: properties
            .get("fromdate")
            .and_then(Value::as_str)
            .and_then(time::parse_iso_utc),
        to_ms: properties
            .get("todate")
            .and_then(Value::as_str)
            .and_then(time::parse_iso_utc),
        current: properties
            .get("iscurrent")
            .and_then(Value::as_str)
            .is_some_and(|current| current.trim().eq_ignore_ascii_case("true")),
        latitude,
        longitude,
        report_url: properties
            .get("url")
            .and_then(|url| url.get("report"))
            .and_then(Value::as_str)
            .map(String::from),
    })
}

/// A field's text, when the document carries one.
fn text(object: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}
/// Fetches the storms in the window.
pub fn fetch(acquire: &Acquire, now_ms: u64) -> Result<Served<StormList>, CycloneError> {
    let day = i64::try_from(now_ms / MS_PER_DAY)
        .map_err(|_| shape("a timestamp that falls inside the calendar"))?;
    let from = day - WINDOW_DAYS;
    let (from_year, from_month, from_day) = time::civil_from_days(from);
    let (to_year, to_month, to_day) = time::civil_from_days(day);
    let from_text = format!("{from_month:02}/{from_day:02}/{from_year:04}");
    let to_text = format!("{to_month:02}/{to_day:02}/{to_year:04}");
    let fetched = acquire
        .fetch_operation(
            &sources::GDACS,
            "search",
            &[
                ("from_date", from_text.as_str()),
                ("to_date", to_text.as_str()),
            ],
        )
        .map_err(|source| CycloneError::Provider { source })?;
    let document = fetched
        .json()
        .map_err(|source| CycloneError::Provider { source })?;
    let value = parse(&document)?;
    Ok(Served::new(value, fetched.provenance, fetched.cache))
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct CycloneRequest {
    pub generation: u64,
    pub now_ms: u64,
}

/// Answers from the worker.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "the served value travels whole so its provenance cannot be separated from it"
)]
pub enum CycloneEvent {
    Ready {
        generation: u64,
        list: Served<StormList>,
    },
    Failed {
        generation: u64,
        error: CycloneError,
    },
}

/// Fetches storms off the UI thread.
pub struct CycloneFeed {
    requests: Sender<CycloneRequest>,
    events: Receiver<CycloneEvent>,
    generation: u64,
    stopped: bool,
}

impl CycloneFeed {
    pub fn new(acquire: Acquire) -> Self {
        let (requests, request_receiver) = mpsc::channel::<CycloneRequest>();
        let (event_sender, events) = mpsc::channel::<CycloneEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch(&acquire, request.now_ms) {
                    Ok(list) => CycloneEvent::Ready {
                        generation: request.generation,
                        list,
                    },
                    Err(error) => CycloneEvent::Failed {
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

    pub fn request(&mut self, request: CycloneRequest) -> Result<(), CycloneError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(CycloneError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<CycloneEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(CycloneEvent::Failed {
                    generation: self.generation,
                    error: CycloneError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the list request stands.
#[derive(Debug)]
pub enum CycloneState {
    Idle,
    Loading {
        last: Option<Served<StormList>>,
    },
    Ready {
        served: Served<StormList>,
    },
    Failed {
        error: CycloneError,
        last: Option<Served<StormList>>,
    },
}

impl CycloneState {
    pub const fn last(&self) -> Option<&Served<StormList>> {
        match self {
            Self::Ready { served } => Some(served),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its state.
pub struct CycloneView {
    feed: CycloneFeed,
    state: CycloneState,
    generation: u64,
}

impl CycloneView {
    pub fn new(acquire: Acquire) -> Self {
        Self {
            feed: CycloneFeed::new(acquire),
            state: CycloneState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &CycloneState {
        &self.state
    }

    pub fn list(&self) -> Option<&StormList> {
        self.state.last().map(|served| &served.value)
    }

    pub const fn served(&self) -> Option<&Served<StormList>> {
        self.state.last()
    }

    pub fn start(&mut self, now_ms: u64) {
        let last = take_last(&mut self.state);
        self.state = CycloneState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = CycloneRequest {
            generation: self.generation,
            now_ms,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = CycloneState::Failed { error, last: None };
        }
    }

    /// Takes in the worker's answers. The engine owns retry and cache.
    pub fn update(&mut self, _now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                CycloneEvent::Ready { generation, list } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = CycloneState::Ready { served: list };
                }
                CycloneEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    self.state = CycloneState::Failed { error, last };
                }
            }
        }
        false
    }
}

/// Empties the state and returns the list it was holding.
fn take_last(state: &mut CycloneState) -> Option<Served<StormList>> {
    match std::mem::replace(state, CycloneState::Idle) {
        CycloneState::Loading { last } | CycloneState::Failed { last, .. } => last,
        CycloneState::Ready { served } => Some(served),
        CycloneState::Idle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        let body = r#"{
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [81.8, 16.4]},
                    "properties": {
                        "eventtype": "TC", "eventid": 1001345, "episodeid": 10,
                        "eventname": "ARNAB-26", "name": "Tropical Cyclone ARNAB-26",
                        "description": "Tropical Cyclone ARNAB-26",
                        "alertlevel": "Red", "alertscore": 3,
                        "country": "India", "iscurrent": "false",
                        "fromdate": "2026-10-26T00:00:00", "todate": "2026-10-28T06:00:00",
                        "source": "JTWC",
                        "severitydata": {"severity": 222.2208, "severitytext": "Tropical Storm (maximum wind speed of 222 km/h)", "severityunit": "km/h"},
                        "url": {"report": "https://www.gdacs.org/report.aspx?eventid=1001345"}
                    }
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [-84.0, 37.5]},
                    "properties": {
                        "eventtype": "TC", "eventid": 1001350, "episodeid": 4,
                        "eventname": "ISAIAS-26", "description": "Tropical Cyclone ISAIAS-26",
                        "alertlevel": "Orange", "iscurrent": "true",
                        "fromdate": "2026-10-30T00:00:00", "todate": "2026-11-01T12:00:00",
                        "source": "NOAA",
                        "severitydata": {"severity": 92.6, "severitytext": "Tropical Storm (maximum wind speed of 92 km/h)", "severityunit": "km/h"}
                    }
                },
                {"type": "Feature", "properties": {}}
            ]
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    #[test]
    fn a_storm_list_is_read_with_positions_and_winds() -> Result<(), CycloneError> {
        let list = parse(&document())?;
        assert_eq!(list.cyclones.len(), 2);
        assert_eq!(list.rejected, 1);
        let first = list.cyclones.first().ok_or(CycloneError::Shape {
            detail: "a first cyclone",
        })?;
        assert_eq!(first.name, "ARNAB-26");
        assert_eq!(first.alert, Some(Alert::Red));
        assert_eq!(first.position(), Some((16.4, 81.8)));
        assert_eq!(first.wind, Some(222.2208));
        assert_eq!(first.source, "JTWC");
        assert!(!first.current);
        Ok(())
    }

    #[test]
    fn an_active_episode_is_marked_and_counted() -> Result<(), CycloneError> {
        let list = parse(&document())?;
        assert_eq!(list.active(), 1);
        assert_eq!(list.highest(), Some(Alert::Red));
        assert_eq!(
            list.strongest().map(|cyclone| cyclone.name.as_str()),
            Some("ARNAB-26")
        );
        Ok(())
    }

    #[test]
    fn the_days_an_episode_covers_are_counted() -> Result<(), CycloneError> {
        let list = parse(&document())?;
        let first = list.cyclones.first().ok_or(CycloneError::Shape {
            detail: "a first cyclone",
        })?;
        assert_eq!(first.days(), Some(3));
        Ok(())
    }

    #[test]
    fn a_wind_in_metres_per_second_is_brought_to_kilometres_per_hour() {
        let body = r#"{"features": [{
            "geometry": {"coordinates": [10.0, 20.0]},
            "properties": {"eventid": 1, "eventname": "X-26", "alertlevel": "Green",
             "severitydata": {"severity": 30.0, "severityunit": "m/s"}}
        }]}"#;
        let Ok(document) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let list = parse(&document).unwrap_or(StormList {
            cyclones: Vec::new(),
            rejected: 0,
        });
        let wind = list.cyclones.first().and_then(|cyclone| cyclone.wind);
        assert!(
            wind.is_some_and(|wind| (wind - 108.0).abs() < 1e-9),
            "{wind:?}"
        );
    }

    #[test]
    fn an_unfamiliar_alert_level_is_kept_as_text() -> Result<(), CycloneError> {
        let body = r#"{"features": [{
            "geometry": {"coordinates": [1.0, 2.0]},
            "properties": {"eventid": 2, "eventname": "Y-26", "alertlevel": "Purple"}
        }]}"#;
        let document = serde_json::from_str::<Value>(body).map_err(|_| CycloneError::Shape {
            detail: "a document the test wrote itself",
        })?;
        let list = parse(&document)?;
        let first = list.cyclones.first().ok_or(CycloneError::Shape {
            detail: "a first cyclone",
        })?;
        assert_eq!(first.alert, None);
        assert_eq!(first.alert_text, "Purple");
        Ok(())
    }
}
