// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! The headless parser behind `--fetch-source`: the fetched body goes through
//! the same parser the rail uses and comes back as labelled values, units,
//! station, product and observation time.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use serde_json::Value;

use crate::acquire::Acquire;
use crate::basemap_tiles;
use crate::climate;
use crate::cyclone;
use crate::engine::{Fetched, RequestSpec, Served};
use crate::fire;
use crate::forecast;
use crate::gauge;
use crate::global_radar;
use crate::hydro;
use crate::network;
use crate::observations;
use crate::ocean;
use crate::pipeline;
use crate::place;
use crate::power;
use crate::quake;
use crate::quality;
use crate::radar;
use crate::request_report::RequestReport;
use crate::satellite;
use crate::seaice::{self, Hemisphere};
use crate::sources;
use crate::spaceweather;
use crate::stations::{self, SiteId};
use crate::sun;
use crate::terrain;
use crate::time;
use crate::uk_rivers;
use crate::volcano;
use walkers::{Style, Tile, TileId};

/// The unchanged view used by the three composite headless observations: the
/// application's default 18,000 km world view centred on the origin.
const PROBE_CENTER_LAT: f64 = 0.0;
const PROBE_CENTER_LON: f64 = 0.0;
const PROBE_SPAN_KM: f64 = 18_000.0;

/// Four vector tiles at the default world view's scale, around the origin.
const BASEMAP_PROBE_TILES: [TileId; 4] = [
    TileId {
        zoom: 2,
        x: 1,
        y: 1,
    },
    TileId {
        zoom: 2,
        x: 2,
        y: 1,
    },
    TileId {
        zoom: 2,
        x: 1,
        y: 2,
    },
    TileId {
        zoom: 2,
        x: 2,
        y: 2,
    },
];

/// A parsed value with the unit the provider stated.
#[derive(Debug, Clone)]
pub(crate) struct ProbeField {
    pub(crate) label: String,
    pub(crate) value: String,
}

/// What one parsed source produced, ready to print.
#[derive(Debug, Clone)]
pub(crate) struct ProbeReport {
    pub(crate) source: &'static str,
    pub(crate) operation: &'static str,
    pub(crate) fields: Vec<ProbeField>,
    pub(crate) station: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) observed: Option<String>,
}

impl ProbeReport {
    const fn new(source: &sources::Source, operation: &'static str) -> Self {
        Self {
            source: source.id.as_str(),
            operation,
            fields: Vec::new(),
            station: None,
            product: None,
            observed: None,
        }
    }

    fn field(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.push(ProbeField {
            label: label.into(),
            value: value.into(),
        });
        self
    }

    fn station(mut self, value: impl Into<String>) -> Self {
        self.station = Some(value.into());
        self
    }

    fn product(mut self, value: impl Into<String>) -> Self {
        self.product = Some(value.into());
        self
    }

    fn observed(mut self, value: impl Into<String>) -> Self {
        self.observed = Some(value.into());
        self
    }

    /// The human-readable report printed by the probe.
    pub(crate) fn render(&self) -> String {
        let mut lines = vec![format!(
            "parsed {} · operation {}",
            self.source, self.operation
        )];
        lines.push(format!(
            "station: {}",
            self.station
                .as_deref()
                .unwrap_or("not stated by this request")
        ));
        lines.push(format!(
            "product: {}",
            self.product
                .as_deref()
                .unwrap_or("not stated by this request")
        ));
        lines.push(format!(
            "observed: {}",
            self.observed
                .as_deref()
                .unwrap_or("not stated by this request")
        ));
        for field in &self.fields {
            lines.push(format!("{}: {}", field.label, field.value));
        }
        lines.join("\n")
    }
}

/// One live composite observation: parsed values followed by the request set
/// that produced them.
pub(crate) struct CompositeRun {
    pub(crate) parsed: ProbeReport,
    pub(crate) requests: RequestReport,
}

/// Whether this source needs the live composite path rather than the first
/// endpoint's parser alone.
pub(crate) fn is_composite(id: &str) -> bool {
    matches!(id, "rainviewer" | "openfreemap" | "nexrad-level-2")
}

/// Drives the three composite rails through the same registry operations and
/// parsers their display paths use. Every request is fetched, never composed
/// into a summary URL.
///
/// # Errors
///
/// Returns [`ProbeError::Live`] when a required placeholder is absent, a request
/// fails, or a fetched body is not renderable. Returns the parser failure from
/// the rail's own parser when it rejects the response.
pub(crate) fn live(
    source: &'static sources::Source,
    acquire: &Acquire,
    values: &[(&str, &str)],
) -> Result<CompositeRun, ProbeError> {
    match source.id.as_str() {
        "rainviewer" => rainviewer_live(source, acquire),
        "openfreemap" => basemap_live(source, acquire, values),
        "nexrad-level-2" => nexrad_live(source, acquire, values),
        _ => Err(live_error(
            "composite probe",
            "no live composite path is registered for this source",
        )),
    }
}

fn live_error(stage: &'static str, detail: impl Display) -> ProbeError {
    ProbeError::Live {
        stage,
        detail: detail.to_string(),
    }
}

fn fetch_twice(
    acquire: &Acquire,
    source: &'static sources::Source,
    operation: &'static str,
    values: &[(&str, &str)],
) -> Result<Fetched, ProbeError> {
    let mut last = None;
    for _ in 0..2 {
        last = Some(
            acquire
                .fetch_operation(source, operation, values)
                .map_err(|error| live_error(operation, error))?,
        );
    }
    last.ok_or_else(|| live_error(operation, "the request produced no body"))
}

fn rainviewer_live(
    source: &'static sources::Source,
    acquire: &Acquire,
) -> Result<CompositeRun, ProbeError> {
    let fetched = fetch_twice(acquire, source, "manifest", &[])?;
    let manifest = global_radar::parse_manifest(&fetched.body)
        .map_err(|error| rejected("RainViewer manifest", error))?;
    let latest = manifest
        .frames
        .last()
        .cloned()
        .ok_or_else(|| rejected("RainViewer manifest", "the manifest carries no frames"))?;
    let grid = global_radar::tile_grid_for_view(PROBE_CENTER_LAT, PROBE_CENTER_LON, PROBE_SPAN_KM)
        .map_err(|error| rejected("RainViewer tile grid", error))?;
    let frame_count = manifest.frames.len();
    let host = manifest.host.clone();
    let mut served = Served::new(manifest, fetched.provenance, fetched.cache);
    let mut pixels = String::from("not decoded");
    let mut rgba_bytes = 0_usize;
    for bounds in &grid.tiles {
        let size = global_radar::TILE_SIZE.to_string();
        let zoom = bounds.zoom.to_string();
        let x = bounds.x.to_string();
        let y = bounds.y.to_string();
        let scheme = global_radar::COLOR_SCHEME.to_string();
        let spec = RequestSpec::for_operation(
            &sources::RAINVIEWER,
            "tile",
            &[
                ("host", host.as_str()),
                ("path", latest.path.as_str()),
                ("size", size.as_str()),
                ("zoom", zoom.as_str()),
                ("x", x.as_str()),
                ("y", y.as_str()),
                ("scheme", scheme.as_str()),
            ],
        )
        .map_err(|error| live_error("RainViewer tile", error))?;
        let fetched = acquire
            .fetch(&spec)
            .map_err(|error| live_error("RainViewer tile", error))?;
        let image = global_radar::decode_tile(&fetched.body, &fetched.provenance.url)
            .map_err(|error| rejected("RainViewer tile", error))?;
        pixels = format!("{} × {} px", image.width, image.height);
        rgba_bytes = rgba_bytes.saturating_add(image.rgba.len());
        served = served.with_labelled_request(
            format!("tile z{} x{} y{}", bounds.zoom, bounds.x, bounds.y),
            fetched.provenance,
            fetched.cache,
        );
    }
    let requests = RequestReport::from_served("RainViewer composite requests", &served);
    let parsed = ProbeReport::new(source, "manifest")
        .station(host)
        .product("global reflectivity composite")
        .observed(utc(latest.time_unix_ms))
        .field("frames", frame_count.to_string())
        .field("latest frame path", latest.path)
        .field("palette", global_radar::PALETTE)
        .field(
            "probe view",
            format!("{PROBE_CENTER_LAT:.4}, {PROBE_CENTER_LON:.4} · {PROBE_SPAN_KM:.0} km"),
        )
        .field("tiles", grid.tiles.len().to_string())
        .field("tile size", pixels)
        .field("rgba bytes", rgba_bytes.to_string());
    Ok(CompositeRun { parsed, requests })
}

fn basemap_live(
    source: &'static sources::Source,
    acquire: &Acquire,
    values: &[(&str, &str)],
) -> Result<CompositeRun, ProbeError> {
    let style_name = value(values, "style")?;
    let fetched = fetch_twice(acquire, source, "style", &[("style", style_name)])?;
    let document = json_body(&fetched.body, "OpenFreeMap style")?;
    let layers = document
        .get("layers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| rejected("OpenFreeMap style", "the document has no layers array"))?;
    let style: Style =
        serde_json::from_value(document).map_err(|error| rejected("OpenFreeMap style", error))?;
    let mut requests = Vec::with_capacity(BASEMAP_PROBE_TILES.len());
    let mut features = 0_usize;
    let mut bytes = 0_usize;
    let mut cache_warning = None;
    for id in BASEMAP_PROBE_TILES {
        let (result, warning) = basemap_tiles::load_tile(acquire, id);
        let tile = result.map_err(|error| live_error("OpenFreeMap tile", error))?;
        let decoded = Tile::from_mvt(&tile.bytes, &style, id.zoom)
            .map_err(|error| live_error("OpenFreeMap tile parser", error))?;
        features = features.saturating_add(match decoded {
            Tile::Vector(shapes) => shapes.len(),
            Tile::Raster(_) => 0,
        });
        if let Some(warning) = warning {
            cache_warning = Some(warning.to_string());
        }
        bytes = bytes.saturating_add(tile.bytes.len());
        requests.push((
            format!("tile z{} x{} y{}", id.zoom, id.x, id.y),
            tile.provenance,
            tile.cache,
        ));
    }
    let mut served = Served::new(style, fetched.provenance, fetched.cache);
    for (label, provenance, cache) in requests {
        served = served.with_labelled_request(label, provenance, cache);
    }
    let request_report =
        RequestReport::from_served("Basemap style and vector tile requests", &served);
    let mut parsed = ProbeReport::new(source, "style")
        .product("basemap vector style")
        .field("layers", layers.to_string())
        .field(
            "probe view",
            format!("{PROBE_CENTER_LAT:.4}, {PROBE_CENTER_LON:.4} · {PROBE_SPAN_KM:.0} km"),
        )
        .field("tiles", BASEMAP_PROBE_TILES.len().to_string())
        .field("vector features", features.to_string())
        .field("tile bytes", bytes.to_string());
    if let Some(warning) = cache_warning {
        parsed = parsed.field("cache warning", warning);
    }
    Ok(CompositeRun {
        parsed,
        requests: request_report,
    })
}

fn nexrad_live(
    source: &'static sources::Source,
    acquire: &Acquire,
    values: &[(&str, &str)],
) -> Result<CompositeRun, ProbeError> {
    let prefix = value(values, "prefix")?;
    let site_name = prefix
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .ok_or_else(|| rejected("NEXRAD listing", "the prefix carries no site"))?;
    let site = SiteId::parse(site_name).map_err(|error| rejected("NEXRAD listing", error))?;
    network::listed_volume(acquire, prefix)
        .map_err(|error| live_error("NEXRAD discovery listing", error))?;
    let listed = network::listed_volume(acquire, prefix)
        .map_err(|error| live_error("NEXRAD discovery listing", error))?;
    let archive = network::fetch_archive_cached(acquire, &listed.key, &site)
        .map_err(|error| live_error("NEXRAD object", error))?;
    let bytes = archive.bytes.len();
    let volume = radar::decode_volume(&site, &listed.key, &archive.bytes)
        .map_err(|error| rejected("NEXRAD object", error))?;
    let station = String::from(volume.site.as_str());
    let scan = volume.scan_unix_ms;
    let sweeps = volume.sweeps.len().saturating_add(1);
    let elevation = volume.sweep.elevation_deg;
    let radials = volume.sweep.radials.len();
    let products = volume
        .sweep
        .products
        .iter()
        .map(|product| product.label())
        .collect::<Vec<&str>>()
        .join(", ");
    let range = volume.sweep.max_ground_range_km;
    let pages = listed.requests.len();
    let key = listed.key;
    let served = pipeline::served_with_listing(
        Served::new(volume, archive.provenance, archive.cache),
        listed.requests,
    );
    let requests =
        RequestReport::from_served("NEXRAD discovery listing and volume requests", &served);
    let parsed = ProbeReport::new(source, "object")
        .station(station)
        .product("Level II base data")
        .observed(utc(scan))
        .field("discovery pages", pages.to_string())
        .field("object key", key)
        .field("sweeps", sweeps.to_string())
        .field("selected elevation", format!("{elevation:.2}°"))
        .field("radials", radials.to_string())
        .field("products", products)
        .field("max ground range", format!("{range:.1} km"))
        .field("object bytes", bytes.to_string());
    Ok(CompositeRun { parsed, requests })
}

#[derive(Debug)]
pub(crate) enum ProbeError {
    MissingValue {
        name: &'static str,
    },
    InvalidValue {
        name: &'static str,
        value: String,
    },
    Rejected {
        parser: &'static str,
        detail: String,
    },
    Live {
        stage: &'static str,
        detail: String,
    },
}

impl Display for ProbeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue { name } => {
                write!(
                    formatter,
                    "the request needs --value {name}=VALUE to parse its answer"
                )
            }
            Self::InvalidValue { name, value } => {
                write!(
                    formatter,
                    "--value {name}={value} is not a number this parser accepts"
                )
            }
            Self::Rejected { parser, detail } => {
                write!(
                    formatter,
                    "the {parser} parser rejected the answer: {detail}"
                )
            }
            Self::Live { stage, detail } => {
                write!(formatter, "the {stage} live probe failed: {detail}")
            }
        }
    }
}

impl Error for ProbeError {}

fn rejected(parser: &'static str, detail: impl Display) -> ProbeError {
    ProbeError::Rejected {
        parser,
        detail: detail.to_string(),
    }
}

fn value<'a>(values: &'a [(&str, &str)], name: &'static str) -> Result<&'a str, ProbeError> {
    values
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
        .ok_or(ProbeError::MissingValue { name })
}

fn number(values: &[(&str, &str)], name: &'static str) -> Result<f64, ProbeError> {
    let raw = value(values, name)?;
    raw.parse::<f64>().map_err(|_| ProbeError::InvalidValue {
        name,
        value: String::from(raw),
    })
}

fn utc(milliseconds: u64) -> String {
    match time::format_utc(milliseconds) {
        Ok(text) => text,
        Err(error) => format!("time unavailable: {error}"),
    }
}

fn optional(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(
        || String::from("not reported"),
        |value| format!("{value} {unit}"),
    )
}

fn json_body(bytes: &[u8], parser: &'static str) -> Result<Value, ProbeError> {
    serde_json::from_slice(bytes).map_err(|error| rejected(parser, error))
}

/// Parses one fetched first-operation answer with the rail's own parser.
///
/// # Errors
///
/// Returns [`ProbeError`] when a placeholder is missing or the parser rejects
/// the body. A successful return means the parser produced typed data.
#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive dispatch over the admitted sources' first operations"
)]
pub(crate) fn parse(
    source: &'static sources::Source,
    operation: &'static str,
    body: &[u8],
    values: &[(&str, &str)],
    request_url: &str,
) -> Result<ProbeReport, ProbeError> {
    match (source.id.as_str(), operation) {
        ("openfreemap", "style") => {
            let document = json_body(body, "OpenFreeMap style")?;
            let layers = document
                .get("layers")
                .and_then(Value::as_array)
                .map(Vec::len)
                .ok_or_else(|| rejected("OpenFreeMap style", "the document has no layers array"))?;
            serde_json::from_value::<walkers::Style>(document)
                .map_err(|error| rejected("OpenFreeMap style", error))?;
            Ok(ProbeReport::new(source, operation)
                .field("layers", layers.to_string())
                .field("document", "accepted by the rail's style parser"))
        }
        ("open-meteo-forecast", "forecast") => {
            let place = probe_place(values)?;
            let document = json_body(body, "Open-Meteo forecast")?;
            let record = forecast::parse_document(&place, &document)
                .map_err(|error| rejected("Open-Meteo forecast", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(format!("probe point at {:.4}, {:.4}", place.lat, place.lon))
                .product("point forecast")
                .observed(format!("{} ({})", record.current.observed, record.timezone))
                .field(
                    "current temperature",
                    format!("{} {}", record.current.temperature, record.current.unit),
                )
                .field(
                    "apparent temperature",
                    format!("{} {}", record.current.apparent, record.current.unit),
                )
                .field(
                    "wind",
                    format!(
                        "{} km/h from {}°",
                        record.current.wind_speed_kmh, record.current.wind_direction_deg
                    ),
                )
                .field("conditions", record.current.short)
                .field(
                    "periods",
                    format!(
                        "{} hourly · {} daily",
                        record.hourly.len(),
                        record.daily.len()
                    ),
                )
                .field("timezone", record.timezone))
        }
        ("open-meteo-archive", "climate") => {
            let document = json_body(body, "Open-Meteo archive")?;
            let record =
                climate::parse(&document).map_err(|error| rejected("Open-Meteo archive", error))?;
            let first = record.days.first();
            let last = record.days.last();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("ERA5 daily archive")
                .observed(last.map_or_else(
                    || String::from("no days"),
                    |day| {
                        utc(u64::try_from(day.day)
                            .map_or(0, |days| days.saturating_mul(86_400_000)))
                    },
                ))
                .field("days", record.days.len().to_string())
                .field(
                    "first day temperature",
                    first.map_or_else(
                        || String::from("not reported"),
                        |day| optional(day.temperature, &record.temperature_unit),
                    ),
                )
                .field(
                    "latest day precipitation",
                    last.map_or_else(
                        || String::from("not reported"),
                        |day| optional(day.precipitation, &record.precipitation_unit),
                    ),
                ))
        }
        ("open-meteo-air-quality", "quality") => {
            let document = json_body(body, "Open-Meteo air quality")?;
            let record = quality::parse(&document)
                .map_err(|error| rejected("Open-Meteo air quality", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("CAMS air quality")
                .observed(utc(record.current.time_ms))
                .field(
                    "US AQI",
                    optional(record.current.us_aqi, record.unit("us_aqi")),
                )
                .field(
                    "PM2.5",
                    optional(record.current.pm2_5, record.unit("pm2_5")),
                )
                .field("PM10", optional(record.current.pm10, record.unit("pm10")))
                .field("hours", record.hours.len().to_string()))
        }
        ("open-meteo-marine", "ocean") => {
            let document = json_body(body, "Open-Meteo marine")?;
            let record =
                ocean::parse(&document).map_err(|error| rejected("Open-Meteo marine", error))?;
            let first = record.hours.first().copied();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("marine forecast")
                .observed(first.map_or_else(|| String::from("no hours"), |hour| utc(hour.time_ms)))
                .field(
                    "wave height",
                    first.map_or_else(
                        || String::from("not reported"),
                        |hour| optional(hour.wave_height, record.unit("wave_height")),
                    ),
                )
                .field(
                    "sea-surface temperature",
                    first.map_or_else(
                        || String::from("not reported"),
                        |hour| {
                            optional(hour.sea_temperature, record.unit("sea_surface_temperature"))
                        },
                    ),
                )
                .field("hours", record.hours.len().to_string()))
        }
        ("open-meteo-elevation", "elevation") => {
            let latitude = number(values, "latitude")?;
            let longitude = number(values, "longitude")?;
            let record = terrain::parse_body(body, latitude, longitude, request_url)
                .map_err(|error| rejected("Open-Meteo elevation", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("terrain elevation")
                .observed("reference 2021 DEM release; no observation time")
                .field("elevation", format!("{} m", record.elevation_m))
                .field("dataset", record.dataset())
                .field("vertical datum", "not stated by provider"))
        }
        ("open-meteo-flood", "hydro") => {
            let now = time::now_unix_ms().map_err(|error| rejected("Open-Meteo flood", error))?;
            let document = json_body(body, "Open-Meteo flood")?;
            let record = hydro::parse(&document, now)
                .map_err(|error| rejected("Open-Meteo flood", error))?;
            let peak = record.peak();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("GloFAS river discharge")
                .observed(peak.map_or_else(|| String::from("no days"), |day| utc(day.time_ms)))
                .field(
                    "peak discharge",
                    peak.map_or_else(
                        || String::from("not reported"),
                        |day| optional(day.discharge, &record.units),
                    ),
                )
                .field("days", record.days.len().to_string()))
        }
        ("rainviewer", "manifest") => {
            let manifest = global_radar::parse_manifest(body)
                .map_err(|error| rejected("RainViewer manifest", error))?;
            let first = manifest.frames.first().map(|frame| frame.time_unix_ms);
            let last = manifest.frames.last().map(|frame| frame.time_unix_ms);
            Ok(ProbeReport::new(source, operation)
                .product("global reflectivity composite")
                .station(manifest.host.clone())
                .observed(last.map_or_else(|| String::from("no frames"), utc))
                .field("frames", manifest.frames.len().to_string())
                .field(
                    "first frame",
                    first.map_or_else(|| String::from("no frames"), utc),
                ))
        }
        ("nexrad-level-2", "list") => {
            let text = String::from_utf8_lossy(body);
            let prefix = value(values, "prefix")?;
            let page = network::listing_xml(prefix, &text)
                .map_err(|error| rejected("NEXRAD listing", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(String::from(prefix))
                .product("Level II object listing")
                .field("keys", page.keys.len().to_string())
                .field("truncated", format!("{:?}", page.truncated))
                .field(
                    "continuation",
                    page.continuation.unwrap_or_else(|| String::from("none")),
                ))
        }
        ("nws-api", "stations") => {
            let document = json_body(body, "NWS stations")?;
            let stations = stations::parse_document(&document)
                .map_err(|error| rejected("NWS stations", error))?;
            let first = stations.first();
            let observed = first
                .and_then(|station| station.last_data.as_deref())
                .map(|text| {
                    time::parse_iso_utc(text).map_or_else(|| format!("provider text {text}"), utc)
                });
            Ok(ProbeReport::new(source, operation)
                .product("radar station catalogue")
                .station(first.map_or_else(
                    || String::from("no stations"),
                    |station| format!("{} · {}", station.id, station.name),
                ))
                .observed(observed.unwrap_or_else(|| String::from("not reported")))
                .field("stations", stations.len().to_string())
                .field(
                    "first position",
                    first.map_or_else(
                        || String::from("not reported"),
                        |station| format!("{:.4}, {:.4}", station.lat, station.lon),
                    ),
                ))
        }
        ("usgs-earthquakes", "feed") => {
            let window = match value(values, "window")? {
                "all_hour" | "hour" => quake::Window::Hour,
                "4.5_week" | "all_week" | "week" => quake::Window::Week,
                _ => quake::Window::Day,
            };
            let document = json_body(body, "USGS earthquakes")?;
            let quakes = quake::parse(window, &document)
                .map_err(|error| rejected("USGS earthquakes", error))?;
            let strongest = quakes.strongest();
            Ok(ProbeReport::new(source, operation)
                .product("earthquake catalogue")
                .station(quakes.title.clone())
                .observed(
                    strongest.map_or_else(|| String::from("no events"), |quake| utc(quake.time_ms)),
                )
                .field("events", quakes.events.len().to_string())
                .field(
                    "strongest magnitude",
                    strongest.map_or_else(
                        || String::from("none"),
                        |quake| {
                            format!(
                                "{} {} at {}",
                                quake.magnitude, quake.magnitude_type, quake.place
                            )
                        },
                    ),
                )
                .field("unreadable", quakes.unreadable.to_string()))
        }
        ("usgs-water", "box") => {
            let document = json_body(body, "USGS water")?;
            let gauges = gauge::parse(&document).map_err(|error| rejected("USGS water", error))?;
            let first = gauges.gauges.first();
            let reading = first.and_then(gauge::Gauge::newest);
            Ok(ProbeReport::new(source, operation)
                .product("river gauge observation")
                .station(first.map_or_else(
                    || String::from("no gauges"),
                    |gauge| format!("{} · {}", gauge.site, gauge.name),
                ))
                .observed(
                    reading
                        .map_or_else(|| String::from("no reading"), |reading| utc(reading.at_ms)),
                )
                .field("gauges", gauges.gauges.len().to_string())
                .field(
                    "newest value",
                    reading.map_or_else(
                        || String::from("not reported"),
                        |reading| {
                            format!(
                                "{} {} (provisional {})",
                                reading.value, reading.unit, reading.provisional
                            )
                        },
                    ),
                )
                .field("rejected", gauges.rejected.to_string()))
        }
        ("nasa-gibs", "tile") => {
            let (width, height, rgba) = satellite::decode("probe tile", body)
                .map_err(|error| rejected("NASA GIBS tile", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(value(values, "layer")?.to_owned())
                .product(format!(
                    "GIBS tile z{} x{} y{}",
                    value(values, "zoom")?,
                    value(values, "x")?,
                    value(values, "y")?
                ))
                .observed(value(values, "time")?.to_owned())
                .field("pixels", format!("{width} × {height}"))
                .field("rgba bytes", rgba.len().to_string()))
        }
        ("noaa-swpc", "kp") => {
            let document = json_body(body, "NOAA SWPC Kp")?;
            let points = spaceweather::parse_kp(&document)
                .map_err(|error| rejected("NOAA SWPC Kp", error))?;
            let latest = points
                .iter()
                .rev()
                .find(|point| !point.status.is_forecast())
                .copied()
                .or_else(|| points.last().copied());
            Ok(ProbeReport::new(source, operation)
                .product("planetary K index")
                .observed(
                    latest.map_or_else(|| String::from("no points"), |point| utc(point.time_ms)),
                )
                .field("points", points.len().to_string())
                .field(
                    "latest Kp",
                    latest.map_or_else(
                        || String::from("not reported"),
                        |point| format!("{} ({})", point.kp, point.status.label()),
                    ),
                ))
        }
        ("sunrise-sunset", "day") => {
            let document = json_body(body, "Sunrise-Sunset")?;
            let record = sun::parse(&document, request_url)
                .map_err(|error| rejected("Sunrise-Sunset", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("solar events")
                .observed(record.date)
                .field("timezone", record.timezone)
                .field("status", record.status)
                .field(
                    "sunrise",
                    record
                        .sunrise
                        .unwrap_or_else(|| String::from("not reported")),
                )
                .field(
                    "sunset",
                    record
                        .sunset
                        .unwrap_or_else(|| String::from("not reported")),
                )
                .field(
                    "solar noon",
                    record
                        .solar_noon
                        .unwrap_or_else(|| String::from("not reported")),
                ))
        }
        ("open-meteo-geocoding", "search") => {
            let document = json_body(body, "Open-Meteo geocoding")?;
            let query = value(values, "query")?;
            let places = place::places_from_document(&document, request_url, query)
                .map_err(|error| rejected("Open-Meteo geocoding", error))?;
            let first = places.first();
            Ok(ProbeReport::new(source, operation)
                .product("place search")
                .station(first.map_or_else(|| String::from("no places"), |place| place.label()))
                .field("query", query)
                .field("places", places.len().to_string())
                .field(
                    "first position",
                    first.map_or_else(
                        || String::from("not reported"),
                        |place| format!("{:.4}, {:.4}", place.lat, place.lon),
                    ),
                ))
        }
        ("gdacs", "search") => {
            let document = json_body(body, "GDACS")?;
            let storms = cyclone::parse(&document).map_err(|error| rejected("GDACS", error))?;
            let strongest = storms.strongest();
            Ok(ProbeReport::new(source, operation)
                .product("tropical cyclone episodes")
                .station(
                    strongest.map_or_else(|| String::from("no storms"), |storm| storm.name.clone()),
                )
                .observed(
                    strongest
                        .and_then(|storm| storm.to_ms)
                        .map_or_else(|| String::from("not reported"), utc),
                )
                .field("episodes", storms.cyclones.len().to_string())
                .field("active", storms.active().to_string())
                .field(
                    "strongest wind",
                    strongest.map_or_else(
                        || String::from("not reported"),
                        |storm| format!("{} {}", storm.wind_text, storm.alert_text),
                    ),
                ))
        }
        ("aviationweather-metar", "box") => {
            let document = json_body(body, "Aviation Weather METAR")?;
            let reports = observations::parse(&document)
                .map_err(|error| rejected("Aviation Weather METAR", error))?;
            let first = reports.stations.first();
            Ok(ProbeReport::new(source, operation)
                .product("surface observation")
                .station(first.map_or_else(
                    || String::from("no stations"),
                    |station| format!("{} · {}", station.station, station.name),
                ))
                .observed(first.map_or_else(
                    || String::from("no time"),
                    |station| utc(station.observed_ms),
                ))
                .field("stations", reports.stations.len().to_string())
                .field(
                    "first report",
                    first.map_or_else(
                        || String::from("not reported"),
                        |station| station.raw.clone(),
                    ),
                )
                .field("rejected", reports.rejected.to_string()))
        }
        ("nifc-wfigs", "perimeters") => {
            let document = json_body(body, "NIFC perimeters")?;
            let perimeters =
                fire::parse(&document).map_err(|error| rejected("NIFC perimeters", error))?;
            let first = perimeters.perimeters.first();
            Ok(ProbeReport::new(source, operation)
                .product("wildfire perimeter")
                .station(
                    first.map_or_else(|| String::from("no perimeters"), |fire| fire.name.clone()),
                )
                .observed(
                    first
                        .and_then(|fire| fire.mapped_at_ms)
                        .map_or_else(|| String::from("not reported"), utc),
                )
                .field("perimeters", perimeters.perimeters.len().to_string())
                .field(
                    "first area",
                    first.map_or_else(
                        || String::from("not reported"),
                        |fire| optional(fire.area_hectares, "ha"),
                    ),
                )
                .field("rejected", perimeters.rejected.to_string()))
        }
        ("nsidc-sea-ice", "month") => {
            let text = String::from_utf8_lossy(body);
            let month = seaice::parse(&text, Hemisphere::North)
                .map_err(|error| rejected("NSIDC sea ice", error))?;
            let latest = month.latest();
            Ok(ProbeReport::new(source, operation)
                .product(format!(
                    "{} monthly sea-ice extent",
                    month.hemisphere.label()
                ))
                .station(value(values, "hemisphere")?.to_owned())
                .observed(latest.map_or_else(
                    || String::from("no years"),
                    |year| format!("{}-{:02}", year.year, month.month),
                ))
                .field("years", month.years.len().to_string())
                .field(
                    "latest extent",
                    latest.map_or_else(
                        || String::from("not reported"),
                        |year| format!("{} million km²", year.extent_mkm2),
                    ),
                )
                .field("unreadable rows", month.unreadable.to_string()))
        }
        ("usgs-volcano", "list") => {
            let document = json_body(body, "USGS volcanoes")?;
            let notices = volcano::parse_list(&document)
                .map_err(|error| rejected("USGS volcanoes", error))?;
            let first = notices.first();
            Ok(ProbeReport::new(source, operation)
                .product("elevated volcano notice")
                .station(first.map_or_else(
                    || String::from("no volcanoes"),
                    |notice| notice.volcano.clone(),
                ))
                .observed(
                    first
                        .and_then(|notice| notice.sent_ms)
                        .map_or_else(|| String::from("not reported"), utc),
                )
                .field("elevated volcanoes", notices.len().to_string())
                .field(
                    "first colour",
                    first.map_or_else(
                        || String::from("not reported"),
                        |notice| notice.colour_text.clone(),
                    ),
                ))
        }
        ("emsc", "query") => {
            let document = json_body(body, "EMSC")?;
            let quakes = quake::parse_emsc(quake::Window::Day, &document)
                .map_err(|error| rejected("EMSC", error))?;
            let strongest = quakes.strongest();
            Ok(ProbeReport::new(source, operation)
                .product("EMSC earthquake catalogue")
                .station(quakes.title.clone())
                .observed(
                    strongest.map_or_else(|| String::from("no events"), |quake| utc(quake.time_ms)),
                )
                .field("events", quakes.events.len().to_string())
                .field(
                    "strongest magnitude",
                    strongest.map_or_else(
                        || String::from("none"),
                        |quake| {
                            format!(
                                "{} {} at {}",
                                quake.magnitude, quake.magnitude_type, quake.place
                            )
                        },
                    ),
                )
                .field("unreadable", quakes.unreadable.to_string()))
        }
        ("uk-ea-flood", "latest-readings") => {
            let document = json_body(body, "UK Environment Agency")?;
            let readings = uk_rivers::parse(&document)
                .map_err(|error| rejected("UK Environment Agency", error))?;
            let newest = readings.newest();
            Ok(ProbeReport::new(source, operation)
                .product("river level reading")
                .station(newest.map_or_else(
                    || String::from("no readings"),
                    |reading| format!("{} · {}", reading.station, reading.station_reference),
                ))
                .observed(
                    newest.map_or_else(|| String::from("no reading"), |reading| utc(reading.at_ms)),
                )
                .field("readings", readings.readings.len().to_string())
                .field(
                    "newest level",
                    newest.map_or_else(
                        || String::from("not reported"),
                        |reading| {
                            format!(
                                "{} {} ({})",
                                reading.value,
                                reading.unit,
                                reading
                                    .qualifier
                                    .clone()
                                    .unwrap_or_else(|| String::from("qualifier not reported"))
                            )
                        },
                    ),
                )
                .field("rejected", readings.rejected.to_string()))
        }
        ("nasa-power", "climatology") => {
            let document = json_body(body, "NASA POWER")?;
            let record = power::parse(&document).map_err(|error| rejected("NASA POWER", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("MERRA-2 climatology")
                .observed(record.period.clone())
                .field(
                    "annual temperature",
                    optional(record.annual_temperature_c, &record.temperature_unit),
                )
                .field(
                    "annual precipitation",
                    optional(
                        record.annual_precipitation_mm_per_day,
                        &record.precipitation_unit,
                    ),
                )
                .field("months", record.months.len().to_string()))
        }
        ("cwfis", "perimeters") => {
            let document = json_body(body, "CWFIS")?;
            let perimeters =
                fire::parse_cwfis(&document).map_err(|error| rejected("CWFIS", error))?;
            let first = perimeters.perimeters.first();
            Ok(ProbeReport::new(source, operation)
                .product("wildfire perimeter")
                .station(
                    first.map_or_else(|| String::from("no perimeters"), |fire| fire.name.clone()),
                )
                .observed(
                    first
                        .and_then(|fire| fire.mapped_at_ms)
                        .map_or_else(|| String::from("not reported"), utc),
                )
                .field("perimeters", perimeters.perimeters.len().to_string())
                .field(
                    "first area",
                    first.map_or_else(
                        || String::from("not reported"),
                        |fire| optional(fire.area_hectares, "ha"),
                    ),
                )
                .field("rejected", perimeters.rejected.to_string()))
        }
        _ => Err(rejected(
            "probe",
            format!(
                "no parse path is registered for {} operation {operation}",
                source.id.as_str()
            ),
        )),
    }
}

fn probe_place(values: &[(&str, &str)]) -> Result<place::Place, ProbeError> {
    let latitude = number(values, "latitude")?;
    let longitude = number(values, "longitude")?;
    place::Place::new("probe point", "", "", "", latitude, longitude)
        .map_err(|error| rejected("probe place", error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parsed_report_prints_units_station_product_and_observation() {
        let report = ProbeReport::new(&sources::NOAA_SWPC, "kp")
            .station("planetary network")
            .product("planetary K index")
            .observed("2026-11-13 00:00:00 UTC")
            .field("latest Kp", "4.33 (observed)");
        let rendered = report.render();
        assert!(rendered.contains("station: planetary network"));
        assert!(rendered.contains("product: planetary K index"));
        assert!(rendered.contains("observed: 2026-11-13 00:00:00 UTC"));
        assert!(rendered.contains("latest Kp: 4.33 (observed)"));
    }

    #[test]
    fn the_probe_parses_through_the_rail_parser() -> Result<(), ProbeError> {
        let body = br#"[{"time_tag":"2026-11-13T00:00:00Z","kp":4.33,"observed":"observed"}]"#;
        let report = parse(
            &sources::NOAA_SWPC,
            "kp",
            body,
            &[],
            "https://services.swpc.noaa.gov/products/noaa-planetary-k-index-forecast.json",
        )?;
        assert_eq!(report.source, "noaa-swpc");
        assert_eq!(report.operation, "kp");
        assert_eq!(report.observed.as_deref(), Some("2026-11-13 00:00:00 UTC"));
        assert!(
            report
                .fields
                .iter()
                .any(|field| field.value == "4.33 (observed)")
        );
        Ok(())
    }

    #[test]
    fn the_geocoding_probe_reports_a_place_and_its_provenance() -> Result<(), ProbeError> {
        let body = br#"{"results":[{"name":"Berlin","admin1":"State of Berlin","country":"Germany","country_code":"DE","latitude":52.52437,"longitude":13.41053}]}"#;
        let report = parse(
            &sources::OPEN_METEO_GEOCODING,
            "search",
            body,
            &[("query", "Berlin"), ("limit", "8")],
            "https://geocoding-api.open-meteo.com/v1/search?name=Berlin&count=8&language=en&format=json",
        )?;
        assert_eq!(report.source, "open-meteo-geocoding");
        assert_eq!(report.station.as_deref(), Some("Berlin, State of Berlin"));
        assert!(
            report
                .fields
                .iter()
                .any(|field| field.label == "places" && field.value == "1")
        );
        Ok(())
    }

    #[test]
    fn the_elevation_probe_reports_metres_and_its_dataset() -> Result<(), ProbeError> {
        let report = parse(
            &sources::OPEN_METEO_ELEVATION,
            "elevation",
            br#"{"elevation":[38.0]}"#,
            &[("latitude", "52.52"), ("longitude", "13.41")],
            "https://api.open-meteo.com/v1/elevation?latitude=52.52&longitude=13.41",
        )?;
        assert_eq!(report.source, "open-meteo-elevation");
        assert_eq!(report.product.as_deref(), Some("terrain elevation"));
        assert!(
            report
                .fields
                .iter()
                .any(|field| field.label == "elevation" && field.value == "38 m")
        );
        assert!(
            report.fields.iter().any(|field| field.label == "dataset"
                && field.value == "Copernicus DEM 2021 release GLO-90")
        );
        Ok(())
    }

    #[test]
    fn only_the_three_composite_rails_take_the_live_path() {
        assert!(is_composite("rainviewer"));
        assert!(is_composite("openfreemap"));
        assert!(is_composite("nexrad-level-2"));
        assert!(!is_composite("nws-api"));
        assert!(!is_composite("open-meteo-forecast"));
    }

    #[test]
    fn an_unknown_operation_is_reported_without_a_fake_value() {
        let error = parse(
            &sources::NOAA_SWPC,
            "wind",
            b"[]",
            &[],
            "https://services.swpc.noaa.gov/products/summary/solar-wind-speed.json",
        );
        assert!(matches!(
            error,
            Err(ProbeError::Rejected {
                parser: "probe",
                ..
            })
        ));
    }
}
