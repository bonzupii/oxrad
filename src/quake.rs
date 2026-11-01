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
//! Global earthquakes from the USGS real-time GeoJSON feeds.
//!
//! The feeds are already world-wide and need no key, so the domain is the
//! whole planet rather than a national subset. Each event keeps the
//! magnitude the source published, its magnitude scale, its origin time, its
//! depth and the provider's own event page: a depth and a magnitude mean
//! nothing without the scale and review status they came with.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::provider::{self, JsonError};

const FEEDS: &str = "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary";
const PROVIDER: &str = "USGS";
const RETRY_BASE_SECS: u64 = 5;
const RETRY_MAX_SECS: u64 = 300;

/// How often the layer is allowed to ask the provider again.
pub const REFRESH_SECS: u64 = 120;

/// A USGS summary feed, which fixes the magnitude floor and the time window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Hour,
    Day,
    Week,
}

impl Window {
    pub const ALL: [Self; 3] = [Self::Hour, Self::Day, Self::Week];

    /// The provider's file name for this window.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Hour => "all_hour",
            Self::Day => "2.5_day",
            Self::Week => "4.5_week",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Hour => "Past hour",
            Self::Day => "Past day",
            Self::Week => "Past week",
        }
    }

    /// The magnitude floor the feed applies, which the interface must state:
    /// a quiet hour under a magnitude floor is not a quiet planet.
    pub const fn floor(self) -> &'static str {
        match self {
            Self::Hour => "all magnitudes",
            Self::Day => "M2.5 and above",
            Self::Week => "M4.5 and above",
        }
    }
}

/// One earthquake, as the provider published it.
#[derive(Debug, Clone)]
pub struct Quake {
    pub id: String,
    pub magnitude: f64,
    pub magnitude_type: String,
    pub place: String,
    pub time_ms: u64,
    pub depth_km: f64,
    pub lat: f64,
    pub lon: f64,
    pub tsunami: bool,
    pub alert: Option<String>,
    pub status: String,
    pub felt: Option<u64>,
    pub url: String,
}

impl Quake {
    /// The depth band, for colouring: shallow events do the damage.
    pub const fn depth_band(&self) -> DepthBand {
        if self.depth_km < 20.0 {
            DepthBand::Shallow
        } else if self.depth_km < 70.0 {
            DepthBand::Intermediate
        } else if self.depth_km < 300.0 {
            DepthBand::Deep
        } else {
            DepthBand::VeryDeep
        }
    }
}

/// Standard seismological depth classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthBand {
    Shallow,
    Intermediate,
    Deep,
    VeryDeep,
}

impl DepthBand {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Shallow => "0–20 km",
            Self::Intermediate => "20–70 km",
            Self::Deep => "70–300 km",
            Self::VeryDeep => "deeper than 300 km",
        }
    }
}

/// A parsed feed.
#[derive(Debug, Clone)]
pub struct Quakes {
    pub window: Window,
    pub generated_ms: u64,
    pub title: String,
    pub events: Vec<Quake>,
    /// Events the provider listed without the data needed to plot them.
    pub unreadable: usize,
}

impl Quakes {
    /// The strongest event in the feed, if any survived parsing.
    pub fn strongest(&self) -> Option<&Quake> {
        self.events
            .iter()
            .max_by(|left, right| left.magnitude.total_cmp(&right.magnitude))
    }
}

#[derive(Debug)]
pub enum QuakeError {
    Provider { source: JsonError },
    Shape { field: &'static str },
    WorkerStopped,
}

impl QuakeError {
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }

    pub const fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Provider { source } => source.retry_after(),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

const fn shape(field: &'static str) -> QuakeError {
    QuakeError::Shape { field }
}

impl Display for QuakeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => write!(formatter, "{PROVIDER}: {source}"),
            Self::Shape { field } => write!(formatter, "{PROVIDER} sent no usable {field}"),
            Self::WorkerStopped => formatter.write_str("the earthquake worker stopped"),
        }
    }
}

impl Error for QuakeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

pub fn request_url(window: Window) -> String {
    format!("{FEEDS}/{}.geojson", window.slug())
}

fn number(value: &Value, field: &'static str) -> Result<f64, QuakeError> {
    value
        .get(field)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| shape(field))
}

fn text(value: &Value, field: &'static str) -> Result<String, QuakeError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(String::from)
        .ok_or_else(|| shape(field))
}

fn event(feature: &Value, index: usize) -> Result<Quake, QuakeError> {
    let properties = feature
        .get("properties")
        .filter(|value| value.is_object())
        .ok_or(QuakeError::Shape {
            field: "event properties",
        })?;
    let geometry = feature
        .get("geometry")
        .filter(|value| value.is_object())
        .ok_or(QuakeError::Shape {
            field: "event geometry",
        })?;
    let coordinates = geometry
        .get("coordinates")
        .and_then(Value::as_array)
        .ok_or(QuakeError::Shape {
            field: "event coordinates",
        })?;
    let lon = coordinates
        .first()
        .and_then(Value::as_f64)
        .ok_or(QuakeError::Shape {
            field: "event longitude",
        })?;
    let lat = coordinates
        .get(1)
        .and_then(Value::as_f64)
        .ok_or(QuakeError::Shape {
            field: "event latitude",
        })?;
    let depth_km = coordinates
        .get(2)
        .and_then(Value::as_f64)
        .ok_or(QuakeError::Shape {
            field: "event depth",
        })?;
    if !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
        || !(0.0..=800.0).contains(&depth_km)
    {
        return Err(QuakeError::Shape {
            field: "event position",
        });
    }
    let magnitude = number(properties, "mag")?;
    if !(-2.0..=10.0).contains(&magnitude) {
        return Err(shape("event magnitude"));
    }
    let time_ms = properties
        .get("time")
        .and_then(Value::as_i64)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| shape("event time"))?;
    let id = feature
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(String::from)
        .or_else(|| {
            properties
                .get("code")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .ok_or_else(|| QuakeError::Shape {
            field: "event identity",
        })?;
    let felt = properties
        .get("felt")
        .and_then(Value::as_u64)
        .filter(|felt| *felt > 0);
    let alert = properties
        .get("alert")
        .and_then(Value::as_str)
        .map(str::to_ascii_uppercase);
    let magnitude_type = properties
        .get("magType")
        .and_then(Value::as_str)
        .unwrap_or("unknown scale")
        .to_ascii_lowercase();
    let status = text(properties, "status")?;
    Ok(Quake {
        id: format!("{id}#{index}"),
        magnitude,
        magnitude_type,
        place: text(properties, "place")?,
        time_ms,
        depth_km,
        lat,
        lon,
        tsunami: properties
            .get("tsunami")
            .and_then(Value::as_u64)
            .is_some_and(|flag| flag == 1),
        alert,
        status,
        felt,
        url: text(properties, "url")?,
    })
}

pub fn parse(window: Window, document: &Value) -> Result<Quakes, QuakeError> {
    if document.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return Err(shape("feature collection"));
    }
    let metadata = document
        .get("metadata")
        .filter(|value| value.is_object())
        .ok_or_else(|| shape("feed metadata"))?;
    let generated_ms = metadata
        .get("generated")
        .and_then(Value::as_i64)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| shape("feed time"))?;
    let title = text(metadata, "title")?;
    let features = document
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| shape("feed features"))?;
    let mut events = Vec::with_capacity(features.len());
    let mut unreadable = 0;
    for (index, feature) in features.iter().enumerate() {
        match event(feature, index) {
            Ok(quake) => events.push(quake),
            Err(_) => unreadable += 1,
        }
    }
    events.sort_by_key(|quake| std::cmp::Reverse(quake.time_ms));
    Ok(Quakes {
        window,
        generated_ms,
        title,
        events,
        unreadable,
    })
}

pub fn fetch(window: Window) -> Result<Quakes, QuakeError> {
    let url = request_url(window);
    let document = provider::get_json(&url, "application/geo+json")
        .map_err(|source| QuakeError::Provider { source })?;
    parse(window, &document)
}

/// A notice in plain words, for the status line.
pub fn describe(quakes: &Quakes) -> String {
    let text = format!(
        "{} earthquakes · {} · {}",
        quakes.events.len(),
        quakes.window.label().to_ascii_lowercase(),
        quakes.window.floor()
    );
    if quakes.unreadable > 0 {
        return format!("{text} · {} without usable data", quakes.unreadable);
    }
    text
}

fn retry_delay(error: &QuakeError, attempt: u32) -> Duration {
    if let Some(value) = error.retry_after()
        && let Ok(delay) = crate::retry::retry_after_delay(value)
    {
        return delay;
    }
    let exponent = attempt.min(6);
    Duration::from_secs((RETRY_BASE_SECS << exponent).min(RETRY_MAX_SECS))
}

#[derive(Debug)]
struct QuakeRequest {
    window: Window,
    generation: u64,
}

#[derive(Debug)]
struct QuakeMessage {
    generation: u64,
    window: Window,
    result: Result<Quakes, QuakeError>,
}

pub enum QuakeOutcome {
    Ready(Quakes),
    Failed { window: Window, error: QuakeError },
}

/// Runs feed requests off the UI thread, one at a time.
pub struct QuakeFeed {
    requests: Sender<QuakeRequest>,
    results: Receiver<QuakeMessage>,
    generation: u64,
    latest: Option<Window>,
    stopped: bool,
}

impl QuakeFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<QuakeRequest>();
        let (result_sender, results) = mpsc::channel::<QuakeMessage>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let window = request.window;
                let result = fetch(window);
                let message = QuakeMessage {
                    generation: request.generation,
                    window,
                    result,
                };
                if result_sender.send(message).is_err() {
                    return;
                }
            }
        });
        Self {
            requests,
            results,
            generation: 0,
            latest: None,
            stopped: false,
        }
    }

    pub fn start(&mut self, window: Window) -> Result<(), QuakeError> {
        self.generation = self.generation.wrapping_add(1);
        self.latest = Some(window);
        let request = QuakeRequest {
            window,
            generation: self.generation,
        };
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(QuakeError::WorkerStopped);
        }
        Ok(())
    }

    /// The newest answer; a superseded request is discarded rather than
    /// overwriting a newer one.
    pub fn poll(&mut self) -> Option<QuakeOutcome> {
        loop {
            match self.results.try_recv() {
                Ok(message) => {
                    if message.generation == self.generation {
                        return Some(match message.result {
                            Ok(quakes) => QuakeOutcome::Ready(quakes),
                            Err(error) => QuakeOutcome::Failed {
                                window: message.window,
                                error,
                            },
                        });
                    }
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    if self.stopped {
                        return None;
                    }
                    self.stopped = true;
                    let window = self.latest?;
                    return Some(QuakeOutcome::Failed {
                        window,
                        error: QuakeError::WorkerStopped,
                    });
                }
            }
        }
    }
}

/// What the interface last learned about the feed.
#[derive(Debug)]
pub enum QuakeState {
    Idle,
    Loading {
        window: Window,
        last: Option<Quakes>,
        attempt: u32,
    },
    Ready {
        quakes: Quakes,
    },
    Failed {
        window: Window,
        error: QuakeError,
        last: Option<Quakes>,
        retry_at: Option<Instant>,
        attempt: u32,
    },
}

impl QuakeState {
    /// The most recent data that is still worth drawing, however old.
    pub const fn last(&self) -> Option<&Quakes> {
        match self {
            Self::Ready { quakes } => Some(quakes),
            Self::Loading { last, .. } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// Owns the feed and its request state for one window.
pub struct QuakeView {
    feed: QuakeFeed,
    state: QuakeState,
}

impl QuakeView {
    pub fn new() -> Self {
        Self {
            feed: QuakeFeed::new(),
            state: QuakeState::Idle,
        }
    }

    pub const fn state(&self) -> &QuakeState {
        &self.state
    }

    /// Data worth drawing: the newest feed, or the last one if a refresh is
    /// under way or failed.
    pub const fn events(&self) -> Option<&Quakes> {
        self.state.last()
    }

    pub fn start(&mut self, window: Window, attempt: u32) {
        let previous = std::mem::replace(&mut self.state, QuakeState::Idle);
        let last = match previous {
            QuakeState::Loading { last, .. } | QuakeState::Failed { last, .. } => last,
            QuakeState::Ready { quakes } => Some(quakes),
            QuakeState::Idle => None,
        };
        self.state = match self.feed.start(window) {
            Ok(()) => QuakeState::Loading {
                window,
                last,
                attempt,
            },
            Err(error) => QuakeState::Failed {
                window,
                error,
                last,
                retry_at: None,
                attempt,
            },
        };
    }

    pub fn update(&mut self, now: Instant) {
        if let Some(outcome) = self.feed.poll() {
            self.apply(outcome, now);
        }
        let retry = match &self.state {
            QuakeState::Failed {
                window,
                retry_at: Some(due),
                attempt,
                ..
            } if now >= *due => Some((*window, attempt.saturating_add(1))),
            QuakeState::Failed { .. }
            | QuakeState::Idle
            | QuakeState::Loading { .. }
            | QuakeState::Ready { .. } => None,
        };
        if let Some((window, attempt)) = retry {
            self.start(window, attempt);
        }
    }

    fn apply(&mut self, outcome: QuakeOutcome, now: Instant) {
        let previous = std::mem::replace(&mut self.state, QuakeState::Idle);
        let QuakeState::Loading {
            window,
            last,
            attempt,
        } = previous
        else {
            self.state = previous;
            return;
        };
        let answer_window = match &outcome {
            QuakeOutcome::Ready(quakes) => quakes.window,
            QuakeOutcome::Failed { window: failed, .. } => *failed,
        };
        if answer_window != window {
            return;
        }
        let completed = attempt;
        self.state = match outcome {
            QuakeOutcome::Ready(quakes) => QuakeState::Ready { quakes },
            QuakeOutcome::Failed { window, error } => {
                let retry_at = if error.is_transient() {
                    now.checked_add(retry_delay(&error, completed))
                } else {
                    None
                };
                QuakeState::Failed {
                    window,
                    error,
                    last,
                    retry_at,
                    attempt: completed,
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = r#"{
      "type": "FeatureCollection",
      "metadata": {
        "generated": 1793557411000,
        "url": "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/2.5_day.geojson",
        "title": "USGS Magnitude 2.5+ Earthquakes, Past Day",
        "status": 200,
        "api": "2.8.2",
        "count": 3
      },
      "features": [
        {
          "type": "Feature",
          "properties": {
            "mag": 5.1, "place": "South Sandwich Islands region",
            "time": 1793553116053, "updated": 1793554232040,
            "url": "https://earthquake.usgs.gov/eventpage/us7000tuqk",
            "detail": "https://earthquake.usgs.gov/feed/v1.0/detail/us7000tuqk.geojson",
            "felt": null, "alert": null, "status": "reviewed", "tsunami": 0,
            "sig": 400, "net": "us", "code": "7000tuqk", "magType": "mww"
          },
          "geometry": {"type": "Point", "coordinates": [-27.1985, -56.2566, 108.234]},
          "id": "us7000tuqk"
        },
        {
          "type": "Feature",
          "properties": {
            "mag": 6.4, "place": "150 km SSW of Severo-Kuril'sk, Russia",
            "time": 1793550000000, "url": "https://earthquake.usgs.gov/eventpage/us7000tup1",
            "felt": 12, "alert": "yellow", "status": "automatic", "tsunami": 1,
            "magType": "mww"
          },
          "geometry": {"type": "Point", "coordinates": [154.7, 49.1, 26.5]},
          "id": "us7000tup1"
        },
        {
          "type": "Feature",
          "properties": {
            "mag": null, "place": "Somewhere small", "time": 1793549000000,
            "url": "https://earthquake.usgs.gov/eventpage/x", "status": "automatic"
          },
          "geometry": {"type": "Point", "coordinates": [10.0, 10.0, 5.0]},
          "id": "unreadable"
        }
      ]
    }"#;

    fn fixture() -> Result<Value, QuakeError> {
        serde_json::from_str(FIXTURE).map_err(|_| shape("test fixture"))
    }

    #[test]
    fn a_feed_keeps_every_event_with_its_own_scale_and_status() -> Result<(), QuakeError> {
        let quakes = parse(Window::Day, &fixture()?)?;
        assert_eq!(quakes.events.len(), 2);
        assert_eq!(quakes.unreadable, 1);
        assert_eq!(quakes.generated_ms, 1_793_557_411_000);
        assert_eq!(quakes.title, "USGS Magnitude 2.5+ Earthquakes, Past Day");
        let newest = &quakes.events[0];
        assert_eq!(newest.magnitude, 5.1);
        assert_eq!(newest.magnitude_type, "mww");
        assert!((newest.depth_km - 108.234).abs() < 1e-9);
        assert!((newest.lat + 56.2566).abs() < 1e-9);
        assert!((newest.lon + 27.1985).abs() < 1e-9);
        assert_eq!(newest.status, "reviewed");
        assert!(!newest.tsunami);
        assert_eq!(newest.depth_band(), DepthBand::Deep);
        Ok(())
    }

    #[test]
    fn a_feed_orders_events_newest_first_and_finds_the_strongest() -> Result<(), QuakeError> {
        let quakes = parse(Window::Day, &fixture()?)?;
        assert!(quakes.events[0].time_ms >= quakes.events[1].time_ms);
        let strongest = quakes.strongest().ok_or_else(|| shape("strongest"))?;
        assert_eq!(strongest.magnitude, 6.4);
        assert!(strongest.tsunami);
        assert_eq!(strongest.alert.as_deref(), Some("YELLOW"));
        assert_eq!(strongest.felt, Some(12));
        assert_eq!(strongest.depth_band(), DepthBand::Intermediate);
        Ok(())
    }

    #[test]
    fn a_feed_without_a_feature_collection_is_refused() {
        let document = json!({"type": "Feature", "properties": {}});
        assert!(matches!(
            parse(Window::Day, &document),
            Err(QuakeError::Shape { .. })
        ));
    }

    #[test]
    fn an_event_outside_the_planet_is_counted_as_unreadable() -> Result<(), QuakeError> {
        let document = json!({
            "type": "FeatureCollection",
            "metadata": {"generated": 1, "title": "Test"},
            "features": [{
                "type": "Feature",
                "properties": {"mag": 3.0, "place": "Nowhere", "time": 2, "url": "u", "status": "reviewed"},
                "geometry": {"type": "Point", "coordinates": [0.0, 95.0, 10.0]},
                "id": "bad"
            }]
        });
        let quakes = parse(Window::Hour, &document)?;
        assert!(quakes.events.is_empty());
        assert_eq!(quakes.unreadable, 1);
        Ok(())
    }

    #[test]
    fn each_window_requests_its_own_feed() {
        assert!(request_url(Window::Hour).ends_with("/all_hour.geojson"));
        assert!(request_url(Window::Day).ends_with("/2.5_day.geojson"));
        assert!(request_url(Window::Week).ends_with("/4.5_week.geojson"));
        assert!(request_url(Window::Day).starts_with("https://earthquake.usgs.gov/"));
    }

    #[test]
    fn depth_bands_follow_the_seismological_classes() {
        let mut quake = Quake {
            id: String::from("x"),
            magnitude: 3.0,
            magnitude_type: String::from("ml"),
            place: String::from("test"),
            time_ms: 0,
            depth_km: 5.0,
            lat: 0.0,
            lon: 0.0,
            tsunami: false,
            alert: None,
            status: String::from("reviewed"),
            felt: None,
            url: String::new(),
        };
        assert_eq!(quake.depth_band(), DepthBand::Shallow);
        quake.depth_km = 35.0;
        assert_eq!(quake.depth_band(), DepthBand::Intermediate);
        quake.depth_km = 120.0;
        assert_eq!(quake.depth_band(), DepthBand::Deep);
        quake.depth_km = 400.0;
        assert_eq!(quake.depth_band(), DepthBand::VeryDeep);
    }

    #[test]
    fn a_description_states_the_window_floor_and_unreadable_events() -> Result<(), QuakeError> {
        let quakes = parse(Window::Day, &fixture()?)?;
        let text = describe(&quakes);
        assert!(text.starts_with("2 earthquakes"));
        assert!(text.contains("past day"));
        assert!(text.contains("M2.5 and above"));
        assert!(text.contains("1 without usable data"));
        Ok(())
    }
}
