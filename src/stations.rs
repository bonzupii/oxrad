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
//! The National Weather Service GeoJSON endpoint is authoritative. A parsed
//! result is cached as the provider's document so a later network failure
//! leaves usable station metadata in place.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io::ErrorKind;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use serde_json::{Value, json};

use crate::error::SiteIdError;
use crate::paths;
use crate::product::RadarProduct;
use crate::provider::{self, JsonError};

const STATIONS_URL: &str = "https://api.weather.gov/radar/stations";
const ACCEPT: &str = "application/geo+json";

/// A four-character radar site identifier such as `KPAH`.
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
    Provider {
        source: JsonError,
    },
    Shape {
        field: &'static str,
    },
    Duplicate {
        id: String,
    },
    CacheRead {
        path: String,
        source: std::io::Error,
    },
    CacheWrite {
        path: String,
        source: std::io::Error,
    },
    CachePath {
        source: crate::error::PathError,
    },
    NoStations,
    NotRadar,
    WorkerStopped,
    WorkerPanic,
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
            Self::CacheRead { path, source } => {
                format!("The cached station feed at {path} could not be read: {source}")
            }
            Self::CacheWrite { path, source } => {
                format!("The station feed could not be cached at {path}: {source}")
            }
            Self::CachePath { source } => source.to_string(),
            Self::NoStations => String::from("The station feed listed no radar stations."),
            Self::NotRadar => String::from("The station is not a supported Level II radar."),
            Self::WorkerStopped => {
                String::from("The station discovery worker stopped before answering.")
            }
            Self::WorkerPanic => {
                String::from("The station discovery worker panicked while answering.")
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
            Self::CacheRead { source, .. } | Self::CacheWrite { source, .. } => Some(source),
            Self::CachePath { source } => Some(source),
            Self::Shape { .. }
            | Self::Duplicate { .. }
            | Self::NoStations
            | Self::NotRadar
            | Self::WorkerStopped
            | Self::WorkerPanic => None,
        }
    }
}

impl From<JsonError> for StationError {
    fn from(source: JsonError) -> Self {
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

/// Retrieves the live station document without touching the cache.
pub fn fetch_document() -> Result<Value, StationError> {
    provider::get_json(STATIONS_URL, ACCEPT).map_err(|source| StationError::Provider { source })
}

fn cache_path() -> Result<std::path::PathBuf, StationError> {
    paths::station_cache_file().map_err(|source| StationError::CachePath { source })
}

/// Reads the last successfully parsed station document.
pub fn load_cache() -> Result<Option<Vec<Station>>, StationError> {
    load_cache_at(&cache_path()?)
}

fn load_cache_at(path: &Path) -> Result<Option<Vec<Station>>, StationError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StationError::CacheRead {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let stored: Value = serde_json::from_str(&text).map_err(|_| StationError::CacheRead {
        path: path.display().to_string(),
        source: std::io::Error::new(ErrorKind::InvalidData, "cached station feed is not JSON"),
    })?;
    let document = stored
        .get("document")
        .ok_or_else(|| shape("cached station document"))?;
    parse_document(document).map(Some)
}

fn save_cache(document: &Value) -> Result<(), StationError> {
    save_cache_at(&cache_path()?, document)
}

fn save_cache_at(path: &Path, document: &Value) -> Result<(), StationError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| StationError::CacheWrite {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let stored = json!({
        "saved_at_unix_s": crate::time::now_unix_ms().map_or(Value::Null, |ms| json!(ms / 1000)),
        "document": document,
    });
    let body = serde_json::to_string(&stored).map_err(|_| StationError::CacheWrite {
        path: path.display().to_string(),
        source: std::io::Error::new(ErrorKind::InvalidData, "station cache could not be encoded"),
    })?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, body).map_err(|source| StationError::CacheWrite {
        path: temporary.display().to_string(),
        source,
    })?;
    fs::rename(&temporary, path).map_err(|source| StationError::CacheWrite {
        path: path.display().to_string(),
        source,
    })
}

/// A live discovery result, with an optional cache-write warning.
pub struct Discovery {
    pub stations: Vec<Station>,
    pub cache_warning: Option<StationError>,
}

/// Fetches, parses, and caches the live station list.
pub fn discover() -> Result<Discovery, StationError> {
    let document = fetch_document()?;
    let stations = parse_document(&document)?;
    let cache_warning = save_cache(&document).err();
    Ok(Discovery {
        stations,
        cache_warning,
    })
}

pub enum StationOutcome {
    Ready(Discovery),
    Failed(StationError),
}

/// Discovers stations once on a worker thread.
pub struct StationFeed {
    results: Receiver<StationOutcome>,
    stopped: bool,
}

impl StationFeed {
    pub fn new() -> Self {
        let (sender, results) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(discover)).map_or(
                StationOutcome::Failed(StationError::WorkerPanic),
                |result| match result {
                    Ok(discovery) => StationOutcome::Ready(discovery),
                    Err(error) => StationOutcome::Failed(error),
                },
            );
            drop(sender.send(outcome));
        });
        Self {
            results,
            stopped: false,
        }
    }

    pub fn poll(&mut self) -> Option<StationOutcome> {
        match self.results.try_recv() {
            Ok(outcome) => {
                self.stopped = true;
                Some(outcome)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) if self.stopped => None,
            Err(TryRecvError::Disconnected) => {
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

    fn live_document() -> Result<Value, StationError> {
        fetch_document()
    }

    #[test]
    fn live_discovery_returns_nws_radar_stations() -> Result<(), StationError> {
        let stations = parse_document(&live_document()?)?;
        assert!(
            stations.len() >= 150,
            "live station count {}",
            stations.len()
        );
        let kpah = SiteId::parse("KPAH").map_err(|_| shape("KPAH identifier"))?;
        let station = find(&stations, &kpah).ok_or(StationError::NoStations)?;
        assert_eq!(station.name, "Paducah");
        assert!(
            (station.lat - 37.068_33).abs() < 5e-4,
            "KPAH latitude {}",
            station.lat
        );
        assert!(
            (station.lon + 88.771_94).abs() < 5e-4,
            "KPAH longitude {}",
            station.lon
        );
        assert_eq!(station.source, RadarSource::Nexrad);
        assert!(station.supports(RadarProduct::Reflectivity));
        assert!(station.supports(RadarProduct::DifferentialReflectivity));
        Ok(())
    }

    #[test]
    fn a_completed_feed_does_not_report_a_stopped_worker() {
        let (sender, results) = mpsc::channel();
        let mut feed = StationFeed {
            results,
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
    fn a_cached_document_survives_without_the_network() -> Result<(), StationError> {
        let document = live_document()?;
        let directory =
            std::env::temp_dir().join(format!("oxrad-station-cache-test-{}", std::process::id()));
        fs::create_dir(&directory).map_err(|source| StationError::CacheWrite {
            path: directory.display().to_string(),
            source,
        })?;
        let path = directory.join("stations.json");
        save_cache_at(&path, &document)?;
        let cached = load_cache_at(&path)?.ok_or(StationError::NoStations)?;
        let live = parse_document(&document)?;
        assert_eq!(cached.len(), live.len());
        assert_eq!(
            cached.first().map(|station| &station.id),
            live.first().map(|station| &station.id)
        );
        fs::remove_dir_all(&directory).map_err(|source| StationError::CacheWrite {
            path: directory.display().to_string(),
            source,
        })?;
        Ok(())
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
