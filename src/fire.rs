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
use crate::engine::Served;
use crate::provider::FetchError;
use crate::sources;
use crate::time;

/// The wildfire catalogues the layer can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireProvider {
    Nifc,
    Cwfis,
}

impl FireProvider {
    pub const ALL: [Self; 2] = [Self::Nifc, Self::Cwfis];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Nifc => "NIFC WFIGS",
            Self::Cwfis => "CWFIS",
        }
    }

    pub const fn coverage(self) -> &'static str {
        match self {
            Self::Nifc => "United States",
            Self::Cwfis => "Canada",
        }
    }

    /// Whether the catalogue is the national one for this position. The
    /// envelope is coarse near a shared border, so the selector stays available
    /// and the rail names the catalogue it is reading.
    pub fn covers(self, lat: f64, lon: f64) -> bool {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return false;
        }
        match self {
            Self::Nifc => !canada(lat, lon) && united_states(lat, lon),
            Self::Cwfis => canada(lat, lon),
        }
    }

    /// The catalogue that covers a position, if one does.
    pub fn for_point(lat: f64, lon: f64) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.covers(lat, lon))
    }
}

/// A coarse national envelope, enough to choose a default catalogue rather
/// than a survey boundary. Longitude bands follow the southern border west to
/// east; the caller still names the catalogue it reads.
fn canada(lat: f64, lon: f64) -> bool {
    if !(41.0..=84.0).contains(&lat) || !(-141.0..=-52.0).contains(&lon) {
        return false;
    }
    let border = if lon < -95.0 {
        49.0
    } else if lon < -79.0 {
        43.0
    } else if lon < -67.0 {
        45.0
    } else {
        44.0
    };
    lat >= border
}

fn united_states(lat: f64, lon: f64) -> bool {
    let contiguous = (-125.0..=-66.0).contains(&lon)
        && lat <= 49.0
        && lat
            >= if lon < -117.0 {
                32.5
            } else if lon < -106.0 {
                31.3
            } else if lon < -103.0 {
                29.5
            } else if lon < -97.0 {
                25.8
            } else {
                24.5
            };
    contiguous
        || ((51.0..=72.0).contains(&lat) && (-170.0..=-129.0).contains(&lon))
        || ((18.0..=23.0).contains(&lat) && (-161.0..=-154.0).contains(&lon))
        || ((17.0..=19.0).contains(&lat) && (-68.0..=-64.0).contains(&lon))
}

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
    /// The mapped area in hectares, the source's metric area or a converted
    /// acreage from a source that publishes acres.
    pub area_hectares: Option<f64>,
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
    pub provider: FireProvider,
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

    /// The largest perimeter by mapped area.
    pub fn largest(&self) -> Option<&FirePerimeter> {
        self.perimeters
            .iter()
            .filter_map(|fire| fire.area_hectares.map(|hectares| (hectares, fire)))
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, fire)| fire)
    }

    /// The total mapped area in hectares, counting only the perimeters that
    /// report it.
    pub fn total_hectares(&self) -> f64 {
        self.perimeters
            .iter()
            .filter_map(|fire| fire.area_hectares)
            .sum()
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
        area_hectares: number_of(properties, "poly_GISAcres")
            .filter(|acres| *acres >= 0.0)
            .map(|acres| acres * ACRES_TO_HECTARES),
        contained: number_of(properties, "attr_PercentContained")
            .filter(|value| (0.0..=100.0).contains(value)),
        state: text_of(properties, "attr_POOState"),
        mapped_at_ms,
        rings,
        centre: (latitude, longitude),
    })
}

/// One acre in hectares, the exact international definition.
pub const ACRES_TO_HECTARES: f64 = 0.404_685_642_24;

/// Reads the NIFC service's answer into a set of perimeters.
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
        provider: FireProvider::Nifc,
        perimeters,
        rejected,
    })
}

/// Reads a CWFIS WFS response. Its perimeter estimates carry an area in
/// hectares and a last mapping time but no incident name.
pub fn parse_cwfis(document: &Value) -> Result<FireSet, FireError> {
    let Some(features) = document.get("features").and_then(Value::as_array) else {
        return Err(shape("a feature collection"));
    };
    let mut perimeters = Vec::new();
    let mut rejected = 0_usize;
    for feature in features {
        let Some(properties) = feature.get("properties").and_then(Value::as_object) else {
            rejected += 1;
            continue;
        };
        let Some(rings) = feature.get("geometry").and_then(rings_of) else {
            rejected += 1;
            continue;
        };
        let Some(outer) = rings.first() else {
            rejected += 1;
            continue;
        };
        let count = outer.len() as f64;
        let latitude = outer.iter().map(|(lat, _)| lat).sum::<f64>() / count;
        let longitude = outer.iter().map(|(_, lon)| lon).sum::<f64>() / count;
        let mapped_at_ms = properties
            .get("lastdate")
            .and_then(Value::as_str)
            .and_then(time::parse_iso_utc);
        perimeters.push(FirePerimeter {
            name: String::from("unnamed perimeter estimate"),
            kind: FireKind::Unclassified,
            area_hectares: number_of(properties, "area").filter(|area| *area >= 0.0),
            contained: None,
            state: None,
            mapped_at_ms,
            rings,
            centre: (latitude, longitude),
        });
    }
    if perimeters.is_empty() && rejected > 0 {
        return Err(shape("at least one perimeter it could read"));
    }
    Ok(FireSet {
        provider: FireProvider::Cwfis,
        perimeters,
        rejected,
    })
}

/// Half the widest box a map-centred wildfire request may cover, in degrees.
const MAX_HALF_DEG: f64 = 10.0;

/// The box the wildfire request asks for, from the map view.
pub fn bounds(centre_lat: f64, centre_lon: f64, span_km: f64) -> (f64, f64, f64, f64) {
    let half_lat = (span_km / 2.0 / 111.0).clamp(0.5, MAX_HALF_DEG);
    let cosine = centre_lat.to_radians().cos().abs().max(0.2);
    let half_lon = (half_lat / cosine).clamp(0.5, MAX_HALF_DEG);
    let south = (centre_lat - half_lat).clamp(-89.0, 89.0);
    let north = (centre_lat + half_lat).clamp(-89.0, 89.0);
    let west = (centre_lon - half_lon).clamp(-180.0, 180.0);
    let east = (centre_lon + half_lon).clamp(-180.0, 180.0);
    (south, west, north, east)
}

/// A provider's request operation, its values and the parser for its body.
type FireRequestPlan = (
    &'static crate::sources::Source,
    &'static str,
    Vec<(&'static str, String)>,
    fn(&Value) -> Result<FireSet, FireError>,
);

/// Fetches current perimeters through the request engine.
pub fn fetch(
    acquire: &Acquire,
    provider: FireProvider,
    bounds: (f64, f64, f64, f64),
) -> Result<Served<FireSet>, FireError> {
    let (south, west, north, east) = bounds;
    let (source, operation, values, parse_fn): FireRequestPlan = match provider {
        FireProvider::Nifc => (&sources::NIFC_WFIGS, "perimeters", Vec::new(), parse),
        FireProvider::Cwfis => {
            let bbox = format!("{west:.4},{south:.4},{east:.4},{north:.4},EPSG:4326");
            (
                &sources::CWFIS,
                "perimeters",
                vec![("bbox", bbox)],
                parse_cwfis,
            )
        }
    };
    let borrowed: Vec<(&str, &str)> = values
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let fetched = acquire
        .fetch_operation(source, operation, &borrowed)
        .map_err(|source| FireError::Provider { source })?;
    let value = parse_fn(
        &fetched
            .json()
            .map_err(|source| FireError::Provider { source })?,
    )?;
    Ok(Served {
        value,
        provenance: fetched.provenance,
        cache: fetched.cache,
    })
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum FireState {
    Idle,
    Loading {
        last: Option<Served<FireSet>>,
    },
    Ready {
        served: Served<FireSet>,
    },
    Failed {
        error: FireError,
        last: Option<Served<FireSet>>,
    },
}

impl FireState {
    /// The most recent set still worth drawing.
    pub const fn last(&self) -> Option<&Served<FireSet>> {
        match self {
            Self::Ready { served } => Some(served),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The engine read and its current state. The engine owns retry and cache;
/// this view only holds what the layer draws.
pub struct FireView {
    acquire: Acquire,
    pending: Option<Receiver<Result<Served<FireSet>, FireError>>>,
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

    pub fn set(&self) -> Option<&FireSet> {
        self.state.last().map(|served| &served.value)
    }

    /// Whether a read is already in flight.
    pub const fn started(&self) -> bool {
        self.pending.is_some()
    }

    /// Drops the last answer and any read in flight, for a catalogue change.
    pub fn reset(&mut self) {
        self.pending = None;
        self.state = FireState::Idle;
    }

    /// Starts one engine read, keeping a set from the same provider on screen
    /// while a new provider's request is in flight.
    pub fn start(&mut self, provider: FireProvider, bounds: (f64, f64, f64, f64)) {
        let last = take_last(&mut self.state).filter(|served| served.value.provider == provider);
        self.state = FireState::Loading { last };
        let acquire = self.acquire.clone();
        self.pending = Some(crate::engine::task(move || {
            fetch(&acquire, provider, bounds)
        }));
    }

    /// Takes in the read's single answer.
    pub fn update(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(served)) => {
                self.state = FireState::Ready { served };
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
fn take_last(state: &mut FireState) -> Option<Served<FireSet>> {
    match std::mem::replace(state, FireState::Idle) {
        FireState::Loading { last } | FireState::Failed { last, .. } => last,
        FireState::Ready { served } => Some(served),
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
    fn the_catalogue_follows_the_map_centre() {
        assert_eq!(
            FireProvider::for_point(39.0, -98.0),
            Some(FireProvider::Nifc)
        );
        assert_eq!(
            FireProvider::for_point(43.65, -79.38),
            Some(FireProvider::Cwfis)
        );
        assert_eq!(
            FireProvider::for_point(49.28, -123.12),
            Some(FireProvider::Cwfis)
        );
        assert_eq!(
            FireProvider::for_point(45.50, -73.57),
            Some(FireProvider::Cwfis)
        );
        assert_eq!(
            FireProvider::for_point(47.60, -122.33),
            Some(FireProvider::Nifc)
        );
        assert_eq!(
            FireProvider::for_point(61.20, -149.90),
            Some(FireProvider::Nifc)
        );
        assert_eq!(FireProvider::for_point(19.43, -99.13), None);
        assert_eq!(FireProvider::for_point(51.50, -0.12), None);
    }

    #[test]
    fn a_perimeter_keeps_its_rings_and_its_attributes() -> Result<(), FireError> {
        let set = set()?;
        assert_eq!(set.perimeters.len(), 2);
        assert_eq!(set.rejected, 1, "the point is not a perimeter");
        let first = &set.perimeters[0];
        assert_eq!(first.name, "Prairie");
        assert_eq!(first.kind, FireKind::Wildfire);
        assert!(
            first
                .area_hectares
                .is_some_and(|hectares| (hectares - 505.857).abs() < 0.01)
        );
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
    fn the_largest_perimeter_is_the_one_with_the_most_hectares() -> Result<(), FireError> {
        let set = set()?;
        assert_eq!(
            set.largest().map(|fire| fire.name.as_str()),
            Some("Two Bits")
        );
        assert!((set.total_hectares() - 3_743.34).abs() < 0.01);
        Ok(())
    }

    #[test]
    fn a_cwfis_perimeter_keeps_its_hectares_and_mapping_time() {
        let body = r#"{
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Polygon", "coordinates": [[
                    [-96.4, 39.1], [-96.2, 39.1], [-96.2, 39.3], [-96.4, 39.3], [-96.4, 39.1]
                ]]},
                "properties": {
                    "hcount": 227,
                    "firstdate": "2026-07-14T07:17:00Z",
                    "lastdate": "2026-07-19T17:59:00Z",
                    "area": 1517.6168
                }
            }]
        }"#;
        let Ok(document) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let Ok(set) = parse_cwfis(&document) else {
            return;
        };
        assert_eq!(set.provider, FireProvider::Cwfis);
        assert_eq!(set.perimeters.len(), 1);
        let fire = &set.perimeters[0];
        assert!(
            fire.area_hectares
                .is_some_and(|hectares| (hectares - 1517.6168).abs() < 1e-6)
        );
        assert!(fire.mapped_at_ms.is_some());
        assert_eq!(fire.kind, FireKind::Unclassified);
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
