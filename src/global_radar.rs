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
//! The global radar composite from RainViewer.
//!
//! This is a separate source from NEXRAD Level II. It is already a
//! reflectivity composite, not a single site's sweep. The worker keeps the
//! two-hour, ten-minute frame sequence so the map can scrub and loop it.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::RadarError;
use crate::network;

const ENDPOINT: &str = "https://api.rainviewer.com/public/weather-maps.json";
const HOST_PREFIX: &str = "https://";
const TILE_SIZE: u32 = 512;
const COLOR_SCHEME: u8 = 2;
const MAX_ZOOM: u8 = 7;
const MAX_FRAMES: usize = 24;
const MAX_CACHED_TILES: usize = 120;
const MAX_VIEW_TILES: usize = 8;
const MANIFEST_LIFETIME: Duration = Duration::from_secs(90);
const EARTH_KM_PER_DEGREE: f64 = 111.32;

/// Attribution required by the RainViewer public API terms.
pub const ATTRIBUTION: &str = "Weather data by RainViewer";

/// The colour scheme requested for the global composite.
pub const PALETTE: &str = "Universal Blue";

/// A global composite frame available from the manifest.
#[derive(Debug, Clone)]
pub struct GlobalFrame {
    pub time_unix_ms: u64,
    pub path: String,
}

/// The Web Mercator tiles covering the current map view.
#[derive(Debug, Clone, PartialEq)]
pub struct TileGrid {
    pub zoom: u8,
    pub tiles: Vec<TileBounds>,
}

/// One Web Mercator tile in the viewport grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileBounds {
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

/// One decoded tile ready to upload to egui.
#[derive(Debug)]
pub struct DecodedTile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One decoded tile with the geographic bounds it occupies.
#[derive(Debug)]
pub struct TileImage {
    pub bounds: TileBounds,
    pub image: DecodedTile,
}

/// Messages sent from the global radar worker to the UI.
#[derive(Debug)]
pub enum GlobalEvent {
    Manifest {
        generation: u64,
    },
    Frame {
        generation: u64,
        frame: GlobalFrame,
        tiles: Vec<TileImage>,
    },
    Failed {
        generation: u64,
        error: GlobalRadarError,
    },
}

/// Why the global composite could not be acquired or decoded.
#[derive(Debug)]
pub enum GlobalRadarError {
    Network {
        source: RadarError,
    },
    Json {
        source: serde_json::Error,
    },
    Image {
        url: String,
        source: image::ImageError,
    },
    Shape {
        detail: &'static str,
    },
    InvalidTime {
        value: i64,
    },
    InvalidHost {
        host: String,
    },
    InvalidPath {
        path: String,
    },
    Pixels {
        width: u32,
        height: u32,
        bytes: usize,
    },
    WorkerStopped,
}

impl GlobalRadarError {
    fn message(&self) -> String {
        match self {
            Self::Network { source } => {
                format!("The global radar request failed: {source}")
            }
            Self::Json { source } => {
                format!("The global radar manifest is not valid JSON: {source}")
            }
            Self::Image { url, source } => {
                format!("The global radar tile {url} is not a readable PNG: {source}")
            }
            Self::Shape { detail } => {
                format!("The global radar manifest is missing {detail}.")
            }
            Self::InvalidTime { value } => {
                format!("The global radar manifest has an invalid Unix time {value}.")
            }
            Self::InvalidHost { host } => {
                format!("The global radar manifest names an unusable tile host {host:?}.")
            }
            Self::InvalidPath { path } => {
                format!("The global radar manifest names an unusable frame path {path:?}.")
            }
            Self::Pixels {
                width,
                height,
                bytes,
            } => format!(
                "The global radar tile is {width} by {height} with {bytes} bytes of RGBA data."
            ),
            Self::WorkerStopped => {
                String::from("The global radar worker stopped; updates stopped.")
            }
        }
    }
}

impl Display for GlobalRadarError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for GlobalRadarError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Network { source } => Some(source),
            Self::Json { source } => Some(source),
            Self::Image { source, .. } => Some(source),
            Self::Shape { .. }
            | Self::InvalidTime { .. }
            | Self::InvalidHost { .. }
            | Self::InvalidPath { .. }
            | Self::Pixels { .. }
            | Self::WorkerStopped => None,
        }
    }
}

impl From<RadarError> for GlobalRadarError {
    fn from(source: RadarError) -> Self {
        Self::Network { source }
    }
}

#[derive(Debug, Clone)]
struct ManifestSnapshot {
    host: String,
    frames: Vec<GlobalFrame>,
}

struct CachedManifest {
    fetched: Instant,
    snapshot: ManifestSnapshot,
}

struct RadarRequest {
    center_lat: f64,
    center_lon: f64,
    span_km: f64,
    generation: u64,
}

/// Runs global radar requests off the UI thread.
pub struct GlobalRadarFeed {
    requests: Sender<RadarRequest>,
    events: Receiver<GlobalEvent>,
    generation: u64,
    stopped: bool,
}

impl GlobalRadarFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<RadarRequest>();
        let (event_sender, events) = mpsc::channel::<GlobalEvent>();
        std::thread::spawn(move || {
            Worker {
                requests: request_receiver,
                events: event_sender,
                cached: None,
                tiles: HashMap::new(),
            }
            .run();
        });
        Self {
            requests,
            events,
            generation: 0,
            stopped: false,
        }
    }

    /// Requests the frames covering a map view.
    ///
    /// # Errors
    ///
    /// Returns [`GlobalRadarError::WorkerStopped`] when the worker has exited.
    pub fn request(
        &mut self,
        center_lat: f64,
        center_lon: f64,
        span_km: f64,
    ) -> Result<u64, GlobalRadarError> {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
        if self
            .requests
            .send(RadarRequest {
                center_lat,
                center_lon,
                span_km,
                generation: self.generation,
            })
            .is_err()
        {
            self.stopped = true;
            return Err(GlobalRadarError::WorkerStopped);
        }
        Ok(self.generation)
    }

    /// Returns the next event from the current request.
    pub fn poll(&mut self) -> Option<GlobalEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(GlobalEvent::Failed {
                    generation: self.generation,
                    error: GlobalRadarError::WorkerStopped,
                })
            }
        }
    }
}

struct Worker {
    requests: Receiver<RadarRequest>,
    events: Sender<GlobalEvent>,
    cached: Option<CachedManifest>,
    tiles: HashMap<String, Vec<u8>>,
}

impl Worker {
    fn run(mut self) {
        let Ok(mut request) = self.requests.recv() else {
            return;
        };
        loop {
            match self.refresh(&request) {
                Ok(Some(next)) => {
                    request = next;
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    if self
                        .events
                        .send(GlobalEvent::Failed {
                            generation: request.generation,
                            error,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
            match self.requests.recv() {
                Ok(next) => request = next,
                Err(_) => return,
            }
        }
    }

    fn refresh(
        &mut self,
        request: &RadarRequest,
    ) -> Result<Option<RadarRequest>, GlobalRadarError> {
        let snapshot = self.manifest()?;
        let grid = tile_grid_for_view(request.center_lat, request.center_lon, request.span_km)?;
        if self
            .events
            .send(GlobalEvent::Manifest {
                generation: request.generation,
            })
            .is_err()
        {
            return Err(GlobalRadarError::WorkerStopped);
        }
        for frame in snapshot.frames.iter().rev() {
            let mut decoded = Vec::with_capacity(grid.tiles.len());
            for bounds in &grid.tiles {
                if let Some(next) = self.next_request()? {
                    return Ok(Some(next));
                }
                let url = tile_url(&snapshot.host, frame, *bounds)?;
                let key = cache_key(frame, *bounds);
                let png = match self.tiles.remove(&key) {
                    Some(cached) => cached,
                    None => network::http_get(&url)?,
                };
                let image = decode_tile(&png, &url)?;
                if self.tiles.len() >= MAX_CACHED_TILES {
                    self.tiles.clear();
                }
                self.tiles.insert(key, png);
                decoded.push(TileImage {
                    bounds: *bounds,
                    image,
                });
            }
            if self
                .events
                .send(GlobalEvent::Frame {
                    generation: request.generation,
                    frame: frame.clone(),
                    tiles: decoded,
                })
                .is_err()
            {
                return Err(GlobalRadarError::WorkerStopped);
            }
        }
        self.next_request()
    }

    fn next_request(&self) -> Result<Option<RadarRequest>, GlobalRadarError> {
        match self.requests.try_recv() {
            Ok(request) => Ok(Some(request)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(GlobalRadarError::WorkerStopped),
        }
    }

    fn manifest(&mut self) -> Result<ManifestSnapshot, GlobalRadarError> {
        if let Some(cached) = &self.cached
            && cached.fetched.elapsed() < MANIFEST_LIFETIME
        {
            return Ok(cached.snapshot.clone());
        }
        let bytes = network::http_get(ENDPOINT)?;
        let snapshot = parse_manifest(&bytes)?;
        let result = snapshot.clone();
        self.cached = Some(CachedManifest {
            fetched: Instant::now(),
            snapshot,
        });
        Ok(result)
    }
}

fn parse_time(value: &Value, field: &'static str) -> Result<u64, GlobalRadarError> {
    let seconds = value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or(GlobalRadarError::Shape { detail: field })?;
    let seconds =
        u64::try_from(seconds).map_err(|_| GlobalRadarError::InvalidTime { value: seconds })?;
    seconds.checked_mul(1000).ok_or(GlobalRadarError::Shape {
        detail: "a representable frame time",
    })
}

fn valid_host(host: &str) -> bool {
    let Some(name) = host.strip_prefix(HOST_PREFIX) else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 160
        && !path.contains("..")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
}

fn parse_manifest(bytes: &[u8]) -> Result<ManifestSnapshot, GlobalRadarError> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|source| GlobalRadarError::Json { source })?;
    let host = root
        .get("host")
        .and_then(Value::as_str)
        .filter(|host| valid_host(host))
        .ok_or_else(|| GlobalRadarError::InvalidHost {
            host: root
                .get("host")
                .and_then(Value::as_str)
                .map_or_else(|| String::from("missing"), String::from),
        })?;
    let past = root
        .get("radar")
        .and_then(|radar| radar.get("past"))
        .and_then(Value::as_array)
        .ok_or(GlobalRadarError::Shape {
            detail: "radar.past",
        })?;
    if past.is_empty() {
        return Err(GlobalRadarError::Shape {
            detail: "at least one radar frame",
        });
    }
    let mut frames = Vec::with_capacity(past.len().min(MAX_FRAMES));
    for value in past.iter().take(MAX_FRAMES) {
        let time_unix_ms = parse_time(value, "time")?;
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| valid_path(path))
            .ok_or_else(|| GlobalRadarError::InvalidPath {
                path: value
                    .get("path")
                    .and_then(Value::as_str)
                    .map_or_else(|| String::from("missing"), String::from),
            })?;
        frames.push(GlobalFrame {
            time_unix_ms,
            path: String::from(path),
        });
    }
    frames.sort_by_key(|frame| frame.time_unix_ms);
    frames.dedup_by_key(|frame| frame.time_unix_ms);
    Ok(ManifestSnapshot {
        host: String::from(host),
        frames,
    })
}

fn tile_latitude(y: f64, count: f64) -> f64 {
    (std::f64::consts::PI * (1.0 - 2.0 * y / count))
        .sinh()
        .atan()
        .to_degrees()
}

/// Selects a bounded grid of Web Mercator tiles that covers the square view.
///
/// The zoom is the highest level that keeps the grid at or below eight tiles,
/// so local views keep detail and wide views remain bounded.
///
/// # Errors
///
/// Returns [`GlobalRadarError::Shape`] for non-finite or out-of-range views.
pub fn tile_grid_for_view(
    center_lat: f64,
    center_lon: f64,
    span_km: f64,
) -> Result<TileGrid, GlobalRadarError> {
    if !(center_lat.is_finite()
        && center_lon.is_finite()
        && span_km.is_finite()
        && (-85.0..=85.0).contains(&center_lat)
        && (-180.0..=180.0).contains(&center_lon)
        && span_km > 0.0)
    {
        return Err(GlobalRadarError::Shape {
            detail: "a finite map centre inside Web Mercator limits and a positive span",
        });
    }
    let latitude_radians = center_lat.to_radians();
    let longitude_km = EARTH_KM_PER_DEGREE * latitude_radians.cos().max(0.05);
    let lat_extent = (span_km / EARTH_KM_PER_DEGREE).min(170.0);
    let lon_extent = (span_km / longitude_km).min(360.0);
    for zoom in (0..=MAX_ZOOM).rev() {
        let grid = grid_at_zoom(center_lat, center_lon, lat_extent, lon_extent, zoom)?;
        if !grid.tiles.is_empty() && grid.tiles.len() <= MAX_VIEW_TILES {
            return Ok(grid);
        }
    }
    Err(GlobalRadarError::Shape {
        detail: "a viewport grid within the tile budget",
    })
}

fn mercator_fraction(lat: f64) -> f64 {
    let radians = lat.to_radians();
    (1.0 - (radians.tan() + 1.0 / radians.cos()).ln() / std::f64::consts::PI) / 2.0
}

fn grid_at_zoom(
    center_lat: f64,
    center_lon: f64,
    lat_extent: f64,
    lon_extent: f64,
    zoom: u8,
) -> Result<TileGrid, GlobalRadarError> {
    let count = 1_u32 << zoom;
    let count_f = f64::from(count);
    let count_i = i64::from(count);
    let north = (center_lat + lat_extent / 2.0).min(85.0);
    let south = (center_lat - lat_extent / 2.0).max(-85.0);
    let west = center_lon - lon_extent / 2.0;
    let east = center_lon + lon_extent / 2.0;
    let x_start = ((west + 180.0) / 360.0 * count_f).floor() as i64;
    let x_end = ((east + 180.0) / 360.0 * count_f).floor() as i64;
    let y_start = (mercator_fraction(north) * count_f)
        .floor()
        .clamp(0.0, f64::from(count.saturating_sub(1))) as u32;
    let y_end = (mercator_fraction(south) * count_f)
        .floor()
        .clamp(0.0, f64::from(count.saturating_sub(1))) as u32;
    if x_end < x_start {
        return Err(GlobalRadarError::Shape {
            detail: "a finite tile grid",
        });
    }
    let x_count = x_end.saturating_sub(x_start).saturating_add(1);
    let y_count = i64::from(y_end)
        .saturating_sub(i64::from(y_start))
        .saturating_add(1);
    if x_count.saturating_mul(y_count) > MAX_VIEW_TILES as i64 {
        return Ok(TileGrid {
            zoom,
            tiles: Vec::new(),
        });
    }
    let mut tiles = Vec::new();
    for x in x_start..=x_end {
        let wrapped = x.rem_euclid(count_i) as u32;
        for y in y_start..=y_end {
            if tiles
                .iter()
                .any(|tile: &TileBounds| tile.x == wrapped && tile.y == y)
            {
                continue;
            }
            tiles.push(TileBounds {
                zoom,
                x: wrapped,
                y,
                west: 360.0 * f64::from(wrapped) / count_f - 180.0,
                east: 360.0 * f64::from(wrapped.saturating_add(1)) / count_f - 180.0,
                north: tile_latitude(f64::from(y), count_f),
                south: tile_latitude(f64::from(y.saturating_add(1)), count_f),
            });
        }
    }
    if tiles.is_empty() {
        return Err(GlobalRadarError::Shape {
            detail: "at least one viewport tile",
        });
    }
    Ok(TileGrid { zoom, tiles })
}

fn tile_url(
    host: &str,
    frame: &GlobalFrame,
    bounds: TileBounds,
) -> Result<String, GlobalRadarError> {
    if !valid_host(host) || !valid_path(&frame.path) {
        return Err(GlobalRadarError::Shape {
            detail: "a valid tile host and frame path",
        });
    }
    Ok(format!(
        "{host}{}/{TILE_SIZE}/{}/{}/{}/{COLOR_SCHEME}/1_1.png",
        frame.path, bounds.zoom, bounds.x, bounds.y
    ))
}

fn cache_key(frame: &GlobalFrame, bounds: TileBounds) -> String {
    format!(
        "{}:{}:{}:{}",
        frame.time_unix_ms, bounds.zoom, bounds.x, bounds.y
    )
}

fn decode_tile(bytes: &[u8], url: &str) -> Result<DecodedTile, GlobalRadarError> {
    let image =
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png).map_err(|source| {
            GlobalRadarError::Image {
                url: String::from(url),
                source,
            }
        })?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let expected = match (usize::try_from(width), usize::try_from(height)) {
        (Ok(width), Ok(height)) => width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4)),
        (Err(_), _) | (_, Err(_)) => None,
    };
    if width == 0 || height == 0 || expected != Some(rgba.as_raw().len()) {
        return Err(GlobalRadarError::Pixels {
            width,
            height,
            bytes: rgba.as_raw().len(),
        });
    }
    Ok(DecodedTile {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TestError;

    #[test]
    fn tile_grid_covers_the_centre_and_uses_web_mercator_bounds() -> Result<(), TestError> {
        let grid = tile_grid_for_view(40.71, -74.01, 950.0)?;
        let covered = grid.tiles.iter().any(|bounds| {
            bounds.west <= -74.01
                && -74.01 <= bounds.east
                && bounds.south <= 40.71
                && 40.71 <= bounds.north
        });
        assert!(covered, "grid does not contain the map centre");
        assert!(!grid.tiles.is_empty());
        Ok(())
    }

    #[test]
    fn wide_view_uses_multiple_bounded_tiles() -> Result<(), TestError> {
        let grid = tile_grid_for_view(40.0, -74.0, 18_000.0)?;
        assert!(grid.tiles.len() > 1, "wide view used one tile");
        assert!(grid.tiles.len() <= MAX_VIEW_TILES);
        Ok(())
    }

    #[test]
    fn world_view_uses_a_bounded_grid() -> Result<(), TestError> {
        let grid = tile_grid_for_view(0.0, 0.0, 40_000.0)?;
        assert!(grid.tiles.len() > 1);
        assert!(grid.tiles.len() <= MAX_VIEW_TILES);
        assert!(grid.tiles.iter().all(|tile| tile.west >= -180.0));
        Ok(())
    }

    #[test]
    fn manifest_rejects_a_foreign_host() {
        let document = br#"{"generated":1609402525,"host":"http://example.com","radar":{"past":[{"time":1609401600,"path":"/v2/radar/a"}]}}"#;
        assert!(matches!(
            parse_manifest(document),
            Err(GlobalRadarError::InvalidHost { .. })
        ));
    }

    #[test]
    fn manifest_sorts_frames_and_keeps_paths() -> Result<(), TestError> {
        let document = br#"{"generated":1609402525,"host":"https://tilecache.rainviewer.com","radar":{"past":[{"time":1609402200,"path":"/v2/radar/b"},{"time":1609401600,"path":"/v2/radar/a"}]}}"#;
        let manifest = parse_manifest(document)?;
        assert_eq!(manifest.frames.len(), 2);
        assert_eq!(
            manifest.frames.first().map(|frame| frame.time_unix_ms),
            Some(1_609_401_600_000)
        );
        assert_eq!(
            manifest.frames.last().map(|frame| frame.path.as_str()),
            Some("/v2/radar/b")
        );
        Ok(())
    }

    /// Exercises the live global source, including a real PNG tile.
    #[test]
    fn live_global_manifest_and_tile_decode() -> Result<(), TestError> {
        let bytes = network::http_get(ENDPOINT)?;
        let manifest = parse_manifest(&bytes)?;
        assert!(!manifest.frames.is_empty());
        let frame = manifest
            .frames
            .last()
            .ok_or("global manifest has no frames")?;
        let grid = tile_grid_for_view(40.71, -74.01, 950.0)?;
        let bounds = grid.tiles.first().ok_or("global viewport has no tile")?;
        let url = tile_url(&manifest.host, frame, *bounds)?;
        let png = network::http_get(&url)?;
        let tile = decode_tile(&png, &url)?;
        assert_eq!(tile.width, TILE_SIZE);
        assert_eq!(tile.height, TILE_SIZE);
        assert!(tile.rgba.len() > 1000);
        Ok(())
    }
}
