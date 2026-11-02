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
//! Wildfire perimeters from the interagency WFIGS service.
//!
//! The service publishes the perimeter every agency has mapped for a current
//! incident, as polygons in longitude and latitude. A perimeter is what was
//! flown or drawn, not the fire's boundary at this second, and its own mapping
//! time is carried so the rail can say when it was last drawn.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "NIFC WFIGS interagency perimeters";

const BASE: &str = "https://services3.arcgis.com/T4QMspbfLg3qTGWY/arcgis/rest/services/WFIGS_Interagency_Perimeters_Current/FeatureServer/0/query";
/// The fields the layer reads, so the service does not ship every attribute it
/// keeps for an incident.
const FIELDS: &str = "poly_IncidentName,poly_GISAcres,attr_PercentContained,attr_IncidentTypeCategory,attr_POOState,poly_PolygonDateTime";
/// A perimeter is simplified to this many degrees before it is sent, which
/// keeps a national answer to a few hundred kilobytes without moving a fire.
const SIMPLIFY_DEGREES: f64 = 0.01;
/// The shortest ring worth drawing.
const MIN_RING_POINTS: usize = 4;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 3;
const RETRY_BASE: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// Why a perimeter record could not be read.
#[derive(Debug)]
pub enum FireError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for FireError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The fire-perimeter request failed: {source}")
            }
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The fire-perimeter record is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => write!(
                formatter,
                "The fire-perimeter worker stopped, so no perimeters can be requested."
            ),
        }
    }
}

impl Error for FireError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl FireError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// What kind of burn the perimeter belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireKind {
    /// A wildfire.
    Wildfire,
    /// A burn set on purpose, which is a hazard only if it escapes.
    Prescribed,
    /// A category the service publishes that this layer does not classify.
    Unclassified,
}

impl FireKind {
    fn of(category: Option<&str>) -> Self {
        match category {
            Some("WF") => Self::Wildfire,
            Some("RX") => Self::Prescribed,
            _ => Self::Unclassified,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Wildfire => "wildfire",
            Self::Prescribed => "prescribed burn",
            Self::Unclassified => "burn of unstated kind",
        }
    }
}

/// One mapped perimeter.
#[derive(Debug, Clone, PartialEq)]
pub struct FirePerimeter {
    pub name: String,
    pub kind: FireKind,
    pub acres: Option<f64>,
    pub contained: Option<f64>,
    pub state: Option<String>,
    /// When the polygon was drawn, in milliseconds since the epoch.
    pub mapped_at_ms: Option<u64>,
    /// Every ring, outer and hole alike, as latitude and longitude.
    pub rings: Vec<Vec<(f64, f64)>>,
    pub centre: (f64, f64),
}

impl FirePerimeter {
    /// The perimeter's own span, from the corners of its outer rings.
    pub fn span_km(&self) -> f64 {
        let mut north = f64::MIN;
        let mut south = f64::MAX;
        let mut east = f64::MIN;
        let mut west = f64::MAX;
        for ring in &self.rings {
            for (lat, lon) in ring {
                north = north.max(*lat);
                south = south.min(*lat);
                east = east.max(*lon);
                west = west.min(*lon);
            }
        }
        if north < south || east < west {
            return 0.0;
        }
        // At this scale a degree of latitude is 111 km and a degree of
        // longitude narrows with the cosine of the latitude.
        let height = (north - south) * 111.0;
        let middle = (north + south) / 2.0;
        let width = (east - west) * 111.0 * middle.to_radians().cos().abs();
        height.hypot(width)
    }
}

/// Every perimeter the service published, with what it refused to read.
#[derive(Debug, Clone, PartialEq)]
pub struct FireSet {
    pub perimeters: Vec<FirePerimeter>,
    /// Features the layer could not use, counted so the rail can say so.
    pub rejected: usize,
}

impl FireSet {
    /// The perimeters ordered by distance from a point, nearest first.
    pub fn nearest(&self, latitude: f64, longitude: f64) -> Vec<(f64, &FirePerimeter)> {
        let mut distances: Vec<(f64, &FirePerimeter)> = self
            .perimeters
            .iter()
            .map(|fire| {
                let ticks = (fire.centre.0 - latitude).abs()
                    + (fire.centre.1 - longitude).abs() * latitude.to_radians().cos().abs();
                // Degrees are enough for an ordering, and cheap to compare.
                (ticks, fire)
            })
            .collect();
        distances.sort_by(|left, right| left.0.total_cmp(&right.0));
        distances
    }

    /// The largest perimeter by mapped acres.
    pub fn largest(&self) -> Option<&FirePerimeter> {
        self.perimeters
            .iter()
            .filter_map(|fire| fire.acres.map(|acres| (acres, fire)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, fire)| fire)
    }

    /// The total mapped acres, counting only the perimeters that report them.
    pub fn total_acres(&self) -> f64 {
        self.perimeters.iter().filter_map(|fire| fire.acres).sum()
    }
}

/// A shape complaint naming what the document lacked.
const fn shape(detail: &'static str) -> FireError {
    FireError::Shape { detail }
}

/// Reads one coordinate pair, refusing a position that cannot exist.
fn point_of(value: &Value) -> Option<(f64, f64)> {
    let pair = value.as_array()?;
    let (Some(lon), Some(lat)) = (pair.first(), pair.get(1)) else {
        return None;
    };
    let (lon, lat) = (lon.as_f64()?, lat.as_f64()?);
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return None;
    }
    Some((lat, lon))
}

/// Reads one ring of a polygon.
fn ring_of(value: &Value) -> Option<Vec<(f64, f64)>> {
    let positions = value.as_array()?;
    let mut ring = Vec::with_capacity(positions.len());
    for position in positions {
        ring.push(point_of(position)?);
    }
    if ring.len() < MIN_RING_POINTS {
        return None;
    }
    Some(ring)
}

/// Reads every ring in a list of polygon parts.
fn rings_into(parts: &[Value], rings: &mut Vec<Vec<(f64, f64)>>) {
    for part in parts {
        if let Some(ring) = ring_of(part) {
            rings.push(ring);
        }
    }
}

/// Reads a polygon's rings, and a multipolygon's rings after them.
fn rings_of(geometry: &Value) -> Option<Vec<Vec<(f64, f64)>>> {
    let kind = geometry.get("type")?.as_str()?;
    let coordinates = geometry.get("coordinates")?.as_array()?;
    let mut rings = Vec::new();
    match kind {
        "Polygon" => rings_into(coordinates, &mut rings),
        "MultiPolygon" => {
            for polygon in coordinates {
                if let Some(parts) = polygon.as_array() {
                    rings_into(parts, &mut rings);
                }
            }
        }
        _ => return None,
    }
    if rings.is_empty() {
        return None;
    }
    Some(rings)
}

fn number_of(row: &serde_json::Map<String, Value>, field: &str) -> Option<f64> {
    row.get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn text_of(row: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(String::from)
}

/// Reads one feature into a perimeter, or counts it as refused.
fn perimeter_of(feature: &Value) -> Option<FirePerimeter> {
    let properties = feature.get("properties")?.as_object()?;
    let name = text_of(properties, "poly_IncidentName")?;
    let rings = rings_of(feature.get("geometry")?)?;
    let outer = rings.first()?;
    let count = outer.len() as f64;
    let latitude = outer.iter().map(|(lat, _)| lat).sum::<f64>() / count;
    let longitude = outer.iter().map(|(_, lon)| lon).sum::<f64>() / count;
    let mapped_at_ms = number_of(properties, "poly_PolygonDateTime")
        .and_then(|value| u64::try_from(value as i64).ok());
    Some(FirePerimeter {
        name,
        kind: FireKind::of(text_of(properties, "attr_IncidentTypeCategory").as_deref()),
        acres: number_of(properties, "poly_GISAcres").filter(|acres| *acres >= 0.0),
        contained: number_of(properties, "attr_PercentContained")
            .filter(|value| (0.0..=100.0).contains(value)),
        state: text_of(properties, "attr_POOState"),
        mapped_at_ms,
        rings,
        centre: (latitude, longitude),
    })
}

/// Reads the service's answer into a set of perimeters.
pub fn parse(document: &Value) -> Result<FireSet, FireError> {
    let Some(features) = document.get("features").and_then(Value::as_array) else {
        return Err(shape("a feature collection"));
    };
    let mut perimeters = Vec::new();
    let mut rejected = 0_usize;
    for feature in features {
        match perimeter_of(feature) {
            Some(perimeter) => perimeters.push(perimeter),
            None => rejected += 1,
        }
    }
    if perimeters.is_empty() && rejected > 0 {
        return Err(shape("at least one perimeter it could read"));
    }
    Ok(FireSet {
        perimeters,
        rejected,
    })
}

/// The request for every current perimeter.
pub fn request_url() -> String {
    format!(
        "{BASE}?where=1%3D1&outFields={FIELDS}&outSR=4326&geometryPrecision=4&maxAllowableOffset={SIMPLIFY_DEGREES}&f=geojson"
    )
}

/// Fetches every current perimeter.
pub fn fetch() -> Result<FireSet, FireError> {
    let document = provider::get_json(&request_url(), "application/json")
        .map_err(|source| FireError::Provider { source })?;
    parse(&document)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct FireRequest {
    pub generation: u64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum FireEvent {
    Ready { generation: u64, set: FireSet },
    Failed { generation: u64, error: FireError },
}

/// Fetches perimeters off the UI thread.
pub struct FireFeed {
    requests: Sender<FireRequest>,
    events: Receiver<FireEvent>,
    generation: u64,
    stopped: bool,
}

impl FireFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<FireRequest>();
        let (event_sender, events) = mpsc::channel::<FireEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch() {
                    Ok(set) => FireEvent::Ready {
                        generation: request.generation,
                        set,
                    },
                    Err(error) => FireEvent::Failed {
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

    pub fn request(&mut self, request: FireRequest) -> Result<(), FireError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(FireError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<FireEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(FireEvent::Failed {
                    generation: self.generation,
                    error: FireError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum FireState {
    Idle,
    Loading {
        last: Option<FireSet>,
    },
    Ready {
        set: FireSet,
    },
    Failed {
        error: FireError,
        last: Option<FireSet>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl FireState {
    /// The most recent set still worth drawing.
    pub const fn last(&self) -> Option<&FireSet> {
        match self {
            Self::Ready { set } => Some(set),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct FireView {
    feed: FireFeed,
    state: FireState,
    generation: u64,
}

impl FireView {
    pub fn new() -> Self {
        Self {
            feed: FireFeed::new(),
            state: FireState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &FireState {
        &self.state
    }

    pub const fn set(&self) -> Option<&FireSet> {
        self.state.last()
    }

    pub fn start(&mut self, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = FireState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = FireRequest {
            generation: self.generation,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = FireState::Failed {
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
                FireEvent::Ready { generation, set } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = FireState::Ready { set };
                }
                FireEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = FireState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            FireState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            FireState::Idle | FireState::Loading { .. } | FireState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            FireState::Failed { attempts, .. } => *attempts,
            FireState::Idle | FireState::Loading { .. } | FireState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            FireState::Failed { retry_due, .. } => retry_due.is_some(),
            FireState::Idle | FireState::Loading { .. } | FireState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the set it was holding.
fn take_last(state: &mut FireState) -> Option<FireSet> {
    match std::mem::replace(state, FireState::Idle) {
        FireState::Loading { last } | FireState::Failed { last, .. } => last,
        FireState::Ready { set } => Some(set),
        FireState::Idle => None,
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

/// How long ago a perimeter was mapped, in the words the rail uses.
pub fn mapped_label(mapped_at_ms: Option<u64>, now_ms: u64) -> String {
    mapped_at_ms.map_or_else(
        || String::from("mapping time not reported"),
        |mapped| time::age_label(now_ms, mapped),
    )
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
                    "geometry": {"type": "Polygon", "coordinates": [[
                        [-96.4, 39.1], [-96.2, 39.1], [-96.2, 39.3], [-96.4, 39.3], [-96.4, 39.1]
                    ]]},
                    "properties": {
                        "poly_IncidentName": "Prairie",
                        "poly_GISAcres": 1250,
                        "attr_PercentContained": 40,
                        "attr_IncidentTypeCategory": "WF",
                        "attr_POOState": "US-KS",
                        "poly_PolygonDateTime": 1793600000000
                    }
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "MultiPolygon", "coordinates": [
                        [[[-95.0, 38.0], [-94.9, 38.0], [-94.9, 38.1], [-95.0, 38.1], [-95.0, 38.0]]],
                        [[[-94.8, 38.0], [-94.7, 38.0], [-94.7, 38.1], [-94.8, 38.1], [-94.8, 38.0]]]
                    ]},
                    "properties": {
                        "poly_IncidentName": "Two Bits",
                        "poly_GISAcres": 8000,
                        "attr_IncidentTypeCategory": "RX",
                        "attr_POOState": "US-OK"
                    }
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [-95.0, 38.0]},
                    "properties": {"poly_IncidentName": "Not a perimeter"}
                }
            ]
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn set() -> Result<FireSet, FireError> {
        parse(&document())
    }

    #[test]
    fn a_perimeter_keeps_its_rings_and_its_attributes() -> Result<(), FireError> {
        let set = set()?;
        assert_eq!(set.perimeters.len(), 2);
        assert_eq!(set.rejected, 1, "the point is not a perimeter");
        let first = &set.perimeters[0];
        assert_eq!(first.name, "Prairie");
        assert_eq!(first.kind, FireKind::Wildfire);
        assert_eq!(first.acres, Some(1250.0));
        assert_eq!(first.contained, Some(40.0));
        assert_eq!(first.state.as_deref(), Some("US-KS"));
        assert_eq!(first.rings[0].len(), 5);
        Ok(())
    }

    #[test]
    fn a_multipolygon_keeps_every_part() -> Result<(), FireError> {
        let set = set()?;
        let second = &set.perimeters[1];
        assert_eq!(second.rings.len(), 2);
        assert_eq!(second.kind, FireKind::Prescribed);
        assert_eq!(second.contained, None);
        Ok(())
    }

    #[test]
    fn the_nearest_perimeter_is_the_one_at_the_centre() -> Result<(), FireError> {
        let set = set()?;
        let nearest = set.nearest(39.2, -96.3);
        assert_eq!(
            nearest.first().map(|(_, fire)| fire.name.as_str()),
            Some("Prairie")
        );
        assert!(nearest[0].0 < nearest[1].0);
        Ok(())
    }

    #[test]
    fn the_largest_perimeter_is_the_one_with_the_most_acres() -> Result<(), FireError> {
        let set = set()?;
        assert_eq!(
            set.largest().map(|fire| fire.name.as_str()),
            Some("Two Bits")
        );
        assert_eq!(set.total_acres(), 9250.0);
        Ok(())
    }

    #[test]
    fn a_perimeter_at_an_impossible_position_is_refused() {
        let body = r#"{
            "features": [
                {"geometry": {"type": "Polygon", "coordinates": [[[0, 91], [1, 91], [1, 92], [0, 91]]]},
                 "properties": {"poly_IncidentName": "Nowhere"}}
            ]
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(matches!(
            parse(&value),
            Err(FireError::Shape {
                detail: "at least one perimeter it could read"
            })
        ));
    }

    #[test]
    fn a_ring_too_short_to_enclose_anything_is_refused() {
        let body = r#"{
            "features": [
                {"geometry": {"type": "Polygon", "coordinates": [[[-96.0, 39.0], [-96.0, 39.1]]]},
                 "properties": {"poly_IncidentName": "A line"}}
            ]
        }"#;
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(parse(&value).is_err());
    }

    #[test]
    fn the_request_asks_for_the_fields_and_a_simplification() {
        let url = request_url();
        assert!(
            url.contains("WFIGS_Interagency_Perimeters_Current"),
            "{url}"
        );
        assert!(url.contains("f=geojson"), "{url}");
        assert!(url.contains("outSR=4326"), "{url}");
        assert!(url.contains("poly_IncidentName"), "{url}");
        assert!(url.contains("maxAllowableOffset=0.01"), "{url}");
    }

    #[test]
    fn a_perimeter_span_is_read_from_its_own_corners() -> Result<(), FireError> {
        let set = set()?;
        let span = set.perimeters[0].span_km();
        // A fifth of a degree is 22 km north to south; across, it narrows with
        // the cosine of the latitude, which puts the diagonal near 28 km.
        assert!((span - 28.1).abs() < 1.0, "the diagonal was {span} km");
        Ok(())
    }

    #[test]
    fn a_failed_request_is_retried_a_bounded_number_of_times() {
        assert!(retry_due(true, 1).is_some());
        assert!(retry_due(true, MAX_ATTEMPTS).is_none());
        assert!(retry_due(false, 1).is_none());
    }
}
