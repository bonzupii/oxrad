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
use std::sync::mpsc::Receiver;

use serde_json::Value;

use crate::acquire::Acquire;
use crate::provider::FetchError;
use crate::sources;
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "NIFC WFIGS interagency perimeters";

/// The fields the layer reads, so the service does not ship every attribute it
/// keeps for an incident.
/// A perimeter is simplified to this many degrees before it is sent, which
/// keeps a national answer to a few hundred kilobytes without moving a fire.
/// The shortest ring worth drawing.
const MIN_RING_POINTS: usize = 4;

/// Why a perimeter record could not be read.
#[derive(Debug)]
pub enum FireError {
    Provider { source: FetchError },
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

impl FireError {}

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
/// Fetches every current perimeter through the request engine.
pub fn fetch(acquire: &Acquire) -> Result<FireSet, FireError> {
    let fetched = acquire
        .fetch_operation(&sources::NIFC_WFIGS, "perimeters", &[])
        .map_err(|source| FireError::Provider { source })?;
    let document = fetched
        .json()
        .map_err(|source| FireError::Provider { source })?;
    parse(&document)
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

/// The engine read and its current state. The engine owns retry and cache;
/// this view only holds what the layer draws.
pub struct FireView {
    acquire: Acquire,
    pending: Option<Receiver<Result<FireSet, FireError>>>,
    state: FireState,
}

impl FireView {
    pub const fn new(acquire: Acquire) -> Self {
        Self {
            acquire,
            pending: None,
            state: FireState::Idle,
        }
    }

    pub const fn state(&self) -> &FireState {
        &self.state
    }

    pub const fn set(&self) -> Option<&FireSet> {
        self.state.last()
    }

    /// Whether a read is already in flight.
    pub const fn started(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts one engine read, keeping the last set on screen meanwhile.
    pub fn start(&mut self) {
        let last = take_last(&mut self.state);
        self.state = FireState::Loading { last };
        let acquire = self.acquire.clone();
        self.pending = Some(crate::engine::task(move || fetch(&acquire)));
    }

    /// Takes in the read's single answer.
    pub fn update(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(set)) => {
                self.state = FireState::Ready { set };
                self.pending = None;
            }
            Ok(Err(error)) => {
                let last = take_last(&mut self.state);
                self.state = FireState::Failed { error, last };
                self.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let last = take_last(&mut self.state);
                self.state = FireState::Failed {
                    error: FireError::WorkerStopped,
                    last,
                };
                self.pending = None;
            }
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
    fn a_perimeter_span_is_read_from_its_own_corners() -> Result<(), FireError> {
        let set = set()?;
        let span = set.perimeters[0].span_km();
        // A fifth of a degree is 22 km north to south; across, it narrows with
        // the cosine of the latitude, which puts the diagonal near 28 km.
        assert!((span - 28.1).abs() < 1.0, "the diagonal was {span} km");
        Ok(())
    }
}
