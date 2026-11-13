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
//! Runtime radar-station discovery and the source catalog it produces.
//!
//! The National Weather Service GeoJSON endpoint is authoritative. The shared
//! request engine owns its cache, so a later network failure leaves the last
//! station document usable through the served value.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use serde_json::Value;

use crate::acquire::Acquire;
use crate::dispatch;
use crate::engine::Served;
use crate::error::SiteIdError;
use crate::product::RadarProduct;
use crate::provider::FetchError;
use crate::sources;

/// A four-character radar site identifier such as `KTWX`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteId {
    code: String,
}

impl SiteId {
    /// Accepts four ASCII uppercase letters or digits.
    pub fn parse(value: &str) -> Result<Self, SiteIdError> {
        if value.len() != 4 {
            return Err(SiteIdError::Length {
                value: String::from(value),
                length: value.len(),
            });
        }
        for byte in value.bytes() {
            if !(byte.is_ascii_uppercase() || byte.is_ascii_digit()) {
                return Err(SiteIdError::Character {
                    value: String::from(value),
                    byte,
                });
            }
        }
        Ok(Self {
            code: String::from(value),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.code
    }
}

impl Display for SiteId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.code)
    }
}

/// The radar network that publishes a station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarSource {
    Nexrad,
}

impl RadarSource {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Nexrad => "nexrad",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Nexrad => "United States NEXRAD",
        }
    }

    pub const fn provider(self) -> &'static str {
        match self {
            Self::Nexrad => "NOAA National Weather Service",
        }
    }
}

/// The hardware family reported by the station feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StationKind {
    Wsr88d,
    Tdwr,
    Other(String),
}

impl StationKind {
    fn parse(text: &str) -> Self {
        match text {
            "WSR-88D" => Self::Wsr88d,
            "TDWR" => Self::Tdwr,
            other => Self::Other(String::from(other)),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Wsr88d => "WSR-88D",
            Self::Tdwr => "TDWR",
            Self::Other(name) => name,
        }
    }

    pub const fn capabilities(&self) -> &'static [RadarProduct] {
        match self {
            Self::Wsr88d => &RadarProduct::ALL,
            Self::Tdwr => &[
                RadarProduct::Reflectivity,
                RadarProduct::Velocity,
                RadarProduct::SpectrumWidth,
            ],
            Self::Other(_) => &[],
        }
    }
}

/// A radar site discovered from its publishing network.
#[derive(Debug, Clone)]
pub struct Station {
    pub id: SiteId,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation_m: Option<f64>,
    pub kind: StationKind,
    pub source: RadarSource,
    pub status: Option<String>,
    pub last_data: Option<String>,
}

impl Station {
    pub const fn capabilities(&self) -> &'static [RadarProduct] {
        self.kind.capabilities()
    }

    pub fn supports(&self, product: RadarProduct) -> bool {
        self.capabilities().contains(&product)
    }
}

/// A failure discovering, parsing, or caching station metadata.
#[derive(Debug)]
pub enum StationError {
    Provider { source: FetchError },
    Shape { field: &'static str },
    Duplicate { id: String },
    NoStations,
    NotRadar,
    WorkerStopped,
}

impl StationError {
    fn message(&self) -> String {
        match self {
            Self::Provider { source } => source.to_string(),
            Self::Shape { field } => {
                format!("The station feed has no usable {field}.")
            }
            Self::Duplicate { id } => {
                format!("The station feed lists {id} more than once.")
            }
            Self::NoStations => String::from("The station feed listed no radar stations."),
            Self::NotRadar => String::from("The station is not a supported Level II radar."),
            Self::WorkerStopped => {
                String::from("The station discovery worker stopped before answering.")
            }
        }
    }
}

impl Display for StationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for StationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. }
            | Self::Duplicate { .. }
            | Self::NoStations
            | Self::NotRadar
            | Self::WorkerStopped => None,
        }
    }
}

impl From<FetchError> for StationError {
    fn from(source: FetchError) -> Self {
        Self::Provider { source }
    }
}

const fn shape(field: &'static str) -> StationError {
    StationError::Shape { field }
}

fn object<'a>(value: &'a Value, field: &'static str) -> Result<&'a Value, StationError> {
    value
        .get(field)
        .filter(|value| value.is_object())
        .ok_or_else(|| shape(field))
}

fn text(value: &Value, field: &'static str) -> Result<String, StationError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(String::from)
        .ok_or_else(|| shape(field))
}

fn coordinates(feature: &Value) -> Result<(f64, f64), StationError> {
    let geometry = object(feature, "geometry")?;
    let values = geometry
        .get("coordinates")
        .and_then(Value::as_array)
        .ok_or_else(|| shape("station coordinates"))?;
    let lon = values
        .first()
        .and_then(Value::as_f64)
        .filter(|lon| lon.is_finite() && (-180.0..=180.0).contains(lon))
        .ok_or_else(|| shape("station longitude"))?;
    let lat = values
        .get(1)
        .and_then(Value::as_f64)
        .filter(|lat| lat.is_finite() && (-90.0..=90.0).contains(lat))
        .ok_or_else(|| shape("station latitude"))?;
    Ok((lat, lon))
}

fn elevation(properties: &Value) -> Option<f64> {
    properties
        .get("elevation")
        .and_then(|value| value.get("value"))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn receive_time(properties: &Value) -> Option<String> {
    properties
        .get("latency")
        .and_then(|value| value.get("levelTwoLastReceivedTime"))
        .and_then(Value::as_str)
        .map(String::from)
}

fn station_status(properties: &Value) -> Option<String> {
    properties
        .get("rda")
        .and_then(|value| value.get("properties"))
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(String::from)
}

fn parse_feature(feature: &Value) -> Result<Station, StationError> {
    let properties = object(feature, "properties")?;
    let kind = StationKind::parse(&text(properties, "stationType")?);
    if !matches!(kind, StationKind::Wsr88d) {
        return Err(StationError::NotRadar);
    }
    let id = SiteId::parse(&text(properties, "id")?).map_err(|_| shape("station identifier"))?;
    let name = text(properties, "name")?;
    let (lat, lon) = coordinates(feature)?;
    Ok(Station {
        id,
        name,
        lat,
        lon,
        elevation_m: elevation(properties),
        kind,
        source: RadarSource::Nexrad,
        status: station_status(properties),
        last_data: receive_time(properties),
    })
}

/// Parses the NWS GeoJSON document, rejecting duplicate identifiers.
pub fn parse_document(document: &Value) -> Result<Vec<Station>, StationError> {
    let features = document
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| shape("feature collection"))?;
    let mut stations = Vec::with_capacity(features.len());
    for feature in features {
        let station = match parse_feature(feature) {
            Ok(station) => station,
            Err(StationError::NotRadar) => continue,
            Err(error) => return Err(error),
        };
        if stations
            .iter()
            .any(|existing: &Station| existing.id == station.id)
        {
            return Err(StationError::Duplicate {
                id: String::from(station.id.as_str()),
            });
        }
        stations.push(station);
    }
    if stations.is_empty() {
        return Err(StationError::NoStations);
    }
    Ok(stations)
}

/// Retrieves and parses the live station list with its provenance.
pub fn discover(acquire: &Acquire) -> Result<Served<Vec<Station>>, StationError> {
    let fetched = acquire
        .fetch_operation(&sources::NWS_API, "stations", &[])
        .map_err(|source| StationError::Provider { source })?;
    let document = fetched
        .json()
        .map_err(|source| StationError::Provider { source })?;
    let value = parse_document(&document)?;
    Ok(Served::new(value, fetched.provenance, fetched.cache))
}

pub enum StationOutcome {
    Ready(Box<Served<Vec<Station>>>),
    Failed(StationError),
}

/// Discovers stations once on the shared job queue.
pub struct StationFeed {
    feed: dispatch::Feed<StationOutcome>,
    stopped: bool,
}

impl StationFeed {
    pub fn new(acquire: Acquire) -> Self {
        let feed = dispatch::Feed::new(acquire);
        // Discovery either answers or reports why it could not. A queue that
        // has stopped is reported as a stopped feed.
        let stopped = feed
            .request(|acquire| match discover(acquire) {
                Ok(discovery) => StationOutcome::Ready(Box::new(discovery)),
                Err(error) => StationOutcome::Failed(error),
            })
            .is_err();
        Self { feed, stopped }
    }

    pub fn poll(&mut self) -> Option<StationOutcome> {
        match self.feed.poll() {
            Ok(Some(outcome)) => {
                self.stopped = true;
                Some(outcome)
            }
            Ok(None) => None,
            Err(dispatch::Disconnected) if self.stopped => None,
            Err(dispatch::Disconnected) => {
                self.stopped = true;
                Some(StationOutcome::Failed(StationError::WorkerStopped))
            }
        }
    }

    pub const fn is_finished(&self) -> bool {
        self.stopped
    }
}

/// Case-insensitive match on identifier or name.
pub fn matches(station: &Station, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    station.id.as_str().to_lowercase().contains(&query)
        || station.name.to_lowercase().contains(&query)
}

pub fn find<'a>(stations: &'a [Station], id: &SiteId) -> Option<&'a Station> {
    stations.iter().find(|station| &station.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn station_document() -> Value {
        json!({
            "features": [{
                "properties": {
                    "id": "KTWX", "name": "Topeka", "stationType": "WSR-88D",
                    "elevation": 411.0, "rda": {"properties": {"status": "Online"}},
                    "time": {"last_received": "2026-11-03T12:00:00+00:00"}
                },
                "geometry": {"type": "Point", "coordinates": [-96.23249, 38.99694]}
            }]
        })
    }

    #[test]
    fn a_nws_station_document_retains_identity_location_and_capabilities()
    -> Result<(), StationError> {
        let stations = parse_document(&station_document())?;
        assert_eq!(stations.len(), 1);
        let kpah = SiteId::parse("KTWX").map_err(|_| shape("KTWX identifier"))?;
        let station = find(&stations, &kpah).ok_or(StationError::NoStations)?;
        assert_eq!(station.name, "Topeka");
        assert!(
            (station.lat - 38.996_94).abs() < 5e-4,
            "KTWX latitude {}",
            station.lat
        );
        assert!(
            (station.lon + 96.232_49).abs() < 5e-4,
            "KTWX longitude {}",
            station.lon
        );
        assert_eq!(station.source, RadarSource::Nexrad);
        assert!(station.supports(RadarProduct::Reflectivity));
        assert!(station.supports(RadarProduct::DifferentialReflectivity));
        Ok(())
    }

    #[test]
    fn a_completed_feed_does_not_report_a_stopped_worker() {
        let (queued, sender) = dispatch::Feed::<StationOutcome>::test_pair();
        let mut feed = StationFeed {
            feed: queued,
            stopped: false,
        };
        assert!(
            sender
                .send(StationOutcome::Failed(StationError::NoStations))
                .is_ok()
        );
        drop(sender);
        assert!(matches!(
            feed.poll(),
            Some(StationOutcome::Failed(StationError::NoStations))
        ));
        assert!(feed.poll().is_none());
        assert!(feed.is_finished());
    }

    #[test]
    fn malformed_documents_name_the_missing_field() {
        assert!(matches!(
            parse_document(&json!({})),
            Err(StationError::Shape {
                field: "feature collection"
            })
        ));
        assert!(matches!(
            parse_document(&json!({"features": []})),
            Err(StationError::NoStations)
        ));
    }

    #[test]
    fn tdwr_capabilities_exclude_the_dual_polarization_suite() {
        let station = Station {
            id: SiteId::parse("TDF").unwrap_or_else(|_| SiteId {
                code: String::from("TDF"),
            }),
            name: String::from("Test"),
            lat: 0.0,
            lon: 0.0,
            elevation_m: None,
            kind: StationKind::Tdwr,
            source: RadarSource::Nexrad,
            status: None,
            last_data: None,
        };
        assert!(station.supports(RadarProduct::Reflectivity));
        assert!(!station.supports(RadarProduct::DifferentialReflectivity));
    }
}
