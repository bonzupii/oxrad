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
use crate::engine::{Fetched, Served};
use crate::environment;
use crate::fire;
use crate::forecast;
use crate::gauge;
use crate::global_radar;
use crate::hydro;
use crate::land;
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
use crate::snow;
use crate::sounding;
use crate::sources;
use crate::spaceweather;
use crate::stations::{self, SiteId};
use crate::sun;
use crate::terrain;
use crate::time;
use crate::uk_rivers;
use crate::volcano;
use crate::warnings;
use walkers::{Style, Tile, TileId};

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

/// One live operation: parsed values followed by the request set that produced
/// them. Every registry endpoint is run through this shape.
pub(crate) struct OperationRun {
    pub(crate) parsed: ProbeReport,
    pub(crate) requests: RequestReport,
}

/// Runs one registry operation through the shared transport and the rail's own
/// parser. The dispatch is by operation, not by source: the caller iterates
/// `source.endpoints`, so adding an endpoint to the registry adds it here.
///
/// # Errors
///
/// Returns [`ProbeError::Live`] when a request fails and [`ProbeError::Rejected`]
/// when the rail's parser rejects the answer or a prerequisite has no honest
/// value to follow.
pub(crate) fn run(
    source: &'static sources::Source,
    operation: &'static str,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    match (source.id.as_str(), operation) {
        ("openfreemap", "tile") => basemap_tile(source, acquire, supplied),
        ("rainviewer", "tile") => rainviewer_tile(source, acquire, supplied),
        ("nexrad-level-2", "list-page") => nexrad_page(source, acquire, supplied),
        ("nexrad-level-2", "object") => nexrad_object(source, acquire, supplied),
        ("usgs-water", "history") => water_history(source, acquire, supplied),
        ("usgs-volcano", "volcano") => volcano_place(source, acquire, supplied),
        ("usgs-volcano", "notice") => volcano_notice(source, acquire, supplied),
        _ => standard_operation(source, operation, acquire, supplied),
    }
}

fn live_error(stage: &'static str, detail: impl Display) -> ProbeError {
    ProbeError::Live {
        stage,
        detail: detail.to_string(),
    }
}

fn standard_operation(
    source: &'static sources::Source,
    operation: &'static str,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let endpoint = source
        .operation(operation)
        .map_err(|error| rejected("probe registry", error))?;
    let hourly = (operation == "sounding").then(sounding::fields);
    let mut values = values_for(endpoint.template, supplied);
    if let Some(fields) = hourly.as_deref() {
        values.push(("hourly", fields));
    }
    let fetched = fetch_twice(acquire, source, operation, &values)?;
    let parsed = parse(
        source,
        operation,
        &fetched.last.body,
        &values,
        &fetched.last.provenance.url,
    )?;
    drop(hourly);
    Ok(operation_run(
        source,
        operation,
        parsed,
        fetched.last.provenance,
        fetched.last.cache,
        vec![(fetched.first.provenance, fetched.first.cache)],
    ))
}

fn operation_run(
    source: &'static sources::Source,
    operation: &'static str,
    parsed: ProbeReport,
    provenance: crate::engine::Provenance,
    cache: crate::engine::CacheState,
    extra: Vec<(crate::engine::Provenance, crate::engine::CacheState)>,
) -> OperationRun {
    let mut served = Served::new((), provenance, cache);
    for (provenance, cache) in extra {
        served = served.with_request(provenance, cache);
    }
    let requests =
        RequestReport::from_served(format!("{} {operation} requests", source.name), &served);
    OperationRun { parsed, requests }
}

struct FetchedTwice {
    first: Fetched,
    last: Fetched,
}

fn fetch_twice(
    acquire: &Acquire,
    source: &'static sources::Source,
    operation: &'static str,
    values: &[(&str, &str)],
) -> Result<FetchedTwice, ProbeError> {
    let first = acquire
        .fetch_operation(source, operation, values)
        .map_err(|error| live_error(operation, error))?;
    let last = acquire
        .fetch_operation(source, operation, values)
        .map_err(|error| live_error(operation, error))?;
    Ok(FetchedTwice { first, last })
}

fn fetch_once(
    acquire: &Acquire,
    source: &'static sources::Source,
    operation: &'static str,
    values: &[(&str, &str)],
) -> Result<Fetched, ProbeError> {
    acquire
        .fetch_operation(source, operation, values)
        .map_err(|error| live_error(operation, error))
}

fn values_for<'a>(template: &str, supplied: &'a [(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let names = template_names(template);
    supplied
        .iter()
        .copied()
        .filter(|(name, _)| names.iter().any(|candidate| candidate == name))
        .collect()
}

fn template_names(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        let key = &after[..close];
        names.push(String::from(
            key.strip_prefix("raw:").map_or(key, |name| name),
        ));
        rest = &after[close + 1..];
    }
    names
}

fn set_value<'a>(values: &mut Vec<(&'a str, &'a str)>, name: &'a str, value: &'a str) {
    values.retain(|(candidate, _)| *candidate != name);
    values.push((name, value));
}

fn basemap_tile(
    source: &'static sources::Source,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let style_name = value(supplied, "style")?;
    let style_fetched = fetch_once(acquire, source, "style", &[("style", style_name)])?;
    let document = json_body(&style_fetched.body, "OpenFreeMap style")?;
    let layers = document
        .get("layers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| rejected("OpenFreeMap style", "the document has no layers array"))?;
    let style: Style =
        serde_json::from_value(document).map_err(|error| rejected("OpenFreeMap style", error))?;
    let zoom = integer_value(supplied, "zoom")?;
    let id = TileId {
        zoom: u8::try_from(zoom).map_err(|_| ProbeError::InvalidValue {
            name: "zoom",
            value: zoom.to_string(),
        })?,
        x: integer_value(supplied, "x")?,
        y: integer_value(supplied, "y")?,
    };
    let (first, warning) = basemap_tiles::load_tile(acquire, id);
    let first = first.map_err(|error| live_error("OpenFreeMap tile", error))?;
    let (fetched, warning_again) = basemap_tiles::load_tile(acquire, id);
    let fetched = fetched.map_err(|error| live_error("OpenFreeMap tile", error))?;
    let decoded = Tile::from_mvt(&fetched.bytes, &style, id.zoom)
        .map_err(|error| live_error("OpenFreeMap tile parser", error))?;
    let features = match decoded {
        Tile::Vector(shapes) => shapes.len(),
        Tile::Raster(_) => 0,
    };
    let mut parsed = ProbeReport::new(source, "tile")
        .station(format!("tile z{} x{} y{}", id.zoom, id.x, id.y))
        .product("basemap vector tile")
        .observed("no observation time; the tile is a provider-defined snapshot")
        .field("style", style_name)
        .field("layers", layers.to_string())
        .field("vector features", features.to_string())
        .field("tile bytes", fetched.bytes.len().to_string())
        .field(
            "cache",
            format!("{} after {}", fetched.cache.label(), first.cache.label()),
        );
    if let Some(warning) = warning.or(warning_again) {
        parsed = parsed.field("cache warning", warning.to_string());
    }
    Ok(operation_run(
        source,
        "tile",
        parsed,
        fetched.provenance,
        fetched.cache,
        vec![(style_fetched.provenance, style_fetched.cache)],
    ))
}

fn rainviewer_tile(
    source: &'static sources::Source,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let manifest_fetched = fetch_twice(acquire, source, "manifest", &[])?;
    let manifest = global_radar::parse_manifest(&manifest_fetched.last.body)
        .map_err(|error| rejected("RainViewer manifest", error))?;
    let latest = manifest
        .frames
        .last()
        .ok_or_else(|| rejected("RainViewer manifest", "the manifest carries no frames"))?;
    let size = supplied
        .iter()
        .find(|(name, _)| *name == "size")
        .map_or_else(
            || global_radar::TILE_SIZE.to_string(),
            |(_, value)| String::from(*value),
        );
    let scheme = supplied
        .iter()
        .find(|(name, _)| *name == "scheme")
        .map_or_else(
            || global_radar::COLOR_SCHEME.to_string(),
            |(_, value)| String::from(*value),
        );
    let zoom = integer_value(supplied, "zoom")?;
    let x = integer_value(supplied, "x")?;
    let y = integer_value(supplied, "y")?;
    let zoom_text = zoom.to_string();
    let x_text = x.to_string();
    let y_text = y.to_string();
    let fetched = fetch_twice(
        acquire,
        source,
        "tile",
        &[
            ("host", manifest.host.as_str()),
            ("path", latest.path.as_str()),
            ("size", size.as_str()),
            ("zoom", zoom_text.as_str()),
            ("x", x_text.as_str()),
            ("y", y_text.as_str()),
            ("scheme", scheme.as_str()),
        ],
    )?;
    let image = global_radar::decode_tile(&fetched.last.body, &fetched.last.provenance.url)
        .map_err(|error| rejected("RainViewer tile", error))?;
    let parsed = ProbeReport::new(source, "tile")
        .station(manifest.host.clone())
        .product("global reflectivity tile")
        .observed(utc(latest.time_unix_ms))
        .field("frame path", latest.path.clone())
        .field(
            "tile",
            format!("z{zoom} x{x} y{y} · {size} px · scheme {scheme}"),
        )
        .field("pixels", format!("{} × {} px", image.width, image.height))
        .field("rgba bytes", image.rgba.len().to_string())
        .field("palette", global_radar::PALETTE);
    Ok(operation_run(
        source,
        "tile",
        parsed,
        fetched.last.provenance,
        fetched.last.cache,
        vec![
            (fetched.first.provenance, fetched.first.cache),
            (
                manifest_fetched.last.provenance,
                manifest_fetched.last.cache,
            ),
        ],
    ))
}

fn nexrad_page(
    source: &'static sources::Source,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let endpoint = source
        .operation("list-page")
        .map_err(|error| rejected("probe registry", error))?;
    let requested_prefix = supplied
        .iter()
        .find(|(name, _)| *name == "prefix")
        .map(|(_, value)| String::from(*value));
    let mut page = None;
    let mut first = None;
    for prefix in requested_prefix
        .into_iter()
        .chain(std::iter::once(current_utc_prefix()?))
    {
        let fetched = fetch_once(acquire, source, "list", &[("prefix", prefix.as_str())])?;
        let text = String::from_utf8_lossy(&fetched.body);
        let listing = network::listing_xml(&prefix, &text)
            .map_err(|error| rejected("NEXRAD listing", error))?;
        if listing.continuation.is_some() {
            page = Some((prefix, listing));
            first = Some(fetched);
            break;
        }
    }
    let Some((prefix, listing)) = page else {
        return Err(rejected(
            "NEXRAD listing page",
            "no listing prefix in this environment returned a continuation token",
        ));
    };
    let Some(first) = first else {
        return Err(rejected("NEXRAD listing page", "no first page was served"));
    };
    let continuation = listing
        .continuation
        .clone()
        .ok_or_else(|| rejected("NEXRAD listing page", "no continuation token"))?;
    let mut values = values_for(endpoint.template, supplied);
    set_value(&mut values, "prefix", prefix.as_str());
    set_value(&mut values, "continuation", continuation.as_str());
    let fetched = fetch_twice(acquire, source, "list-page", &values)?;
    let text = String::from_utf8_lossy(&fetched.last.body);
    let page = network::listing_xml(&prefix, &text)
        .map_err(|error| rejected("NEXRAD listing page", error))?;
    let parsed = ProbeReport::new(source, "list-page")
        .station(prefix.clone())
        .product("Level II listing page")
        .observed("listing time not stated by provider")
        .field("keys", page.keys.len().to_string())
        .field("truncated", format!("{:?}", page.truncated))
        .field(
            "next continuation",
            page.continuation.unwrap_or_else(|| String::from("none")),
        )
        .field("continuation used", continuation)
        .field("first page keys", listing.keys.len().to_string());
    Ok(operation_run(
        source,
        "list-page",
        parsed,
        fetched.last.provenance,
        fetched.last.cache,
        vec![
            (fetched.first.provenance, fetched.first.cache),
            (first.provenance, first.cache),
        ],
    ))
}

fn nexrad_object(
    source: &'static sources::Source,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let prefix = value(supplied, "prefix")?;
    let site_name = prefix
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .ok_or_else(|| rejected("NEXRAD listing", "the prefix carries no site"))?;
    let site = SiteId::parse(site_name).map_err(|error| rejected("NEXRAD listing", error))?;
    let listed = network::listed_volume(acquire, prefix)
        .map_err(|error| live_error("NEXRAD discovery listing", error))?;
    let first = network::fetch_archive_cached(acquire, &listed.key, &site)
        .map_err(|error| live_error("NEXRAD object", error))?;
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
    let served = served.with_request(first.provenance, first.cache);
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
        .field("object bytes", bytes.to_string())
        .field(
            "cache",
            format!("{} after {}", archive.cache.label(), first.cache.label()),
        );
    Ok(OperationRun { parsed, requests })
}

fn water_history(
    source: &'static sources::Source,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let mut extra = Vec::new();
    let site = match value(supplied, "site") {
        Ok(site) => String::from(site),
        Err(_) => {
            let bbox = value(supplied, "bbox")?;
            let fetched = fetch_once(acquire, source, "box", &[("bbox", bbox)])?;
            let document = json_body(&fetched.body, "USGS water")?;
            let gauges = gauge::parse(&document).map_err(|error| rejected("USGS water", error))?;
            let first = gauges.gauges.first().ok_or_else(|| {
                rejected(
                    "USGS water history",
                    "the box returned no gauge whose history could be followed",
                )
            })?;
            extra.push((fetched.provenance, fetched.cache));
            first.site.clone()
        }
    };
    let fetched = fetch_twice(acquire, source, "history", &[("site", site.as_str())])?;
    let document = json_body(&fetched.last.body, "USGS water history")?;
    let history = gauge::parse_history(&document, &site)
        .map_err(|error| rejected("USGS water history", error))?;
    let newest = history.stage.last();
    let highest = history.highest_stage();
    let parsed = ProbeReport::new(source, "history")
        .station(format!("{} · {}", history.site, history.name))
        .product("seven-day gauge history")
        .observed(newest.map_or_else(|| String::from("no readings"), |reading| utc(reading.at_ms)))
        .field("stage readings", history.stage.len().to_string())
        .field("discharge readings", history.discharge.len().to_string())
        .field(
            "highest stage",
            highest.map_or_else(
                || String::from("not reported"),
                |reading| format!("{} {}", reading.value, reading.unit),
            ),
        )
        .field(
            "stage range",
            history.stage_range().map_or_else(
                || String::from("not reported"),
                |range| format!("{range:.2}"),
            ),
        );
    Ok(operation_run(
        source,
        "history",
        parsed,
        fetched.last.provenance,
        fetched.last.cache,
        {
            let mut all = vec![(fetched.first.provenance, fetched.first.cache)];
            all.extend(extra);
            all
        },
    ))
}

fn volcano_place(
    source: &'static sources::Source,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let mut extra = Vec::new();
    let vnum = match value(supplied, "vnum") {
        Ok(vnum) => String::from(vnum),
        Err(_) => {
            let fetched = fetch_once(acquire, source, "list", &[])?;
            let document = json_body(&fetched.body, "USGS volcanoes")?;
            let notices = volcano::parse_list(&document)
                .map_err(|error| rejected("USGS volcanoes", error))?;
            let first = notices.first().ok_or_else(|| {
                rejected(
                    "USGS volcano",
                    "no elevated volcano was listed to follow up",
                )
            })?;
            extra.push((fetched.provenance, fetched.cache));
            first.vnum.clone()
        }
    };
    let fetched = fetch_twice(acquire, source, "volcano", &[("vnum", vnum.as_str())])?;
    let document = json_body(&fetched.last.body, "USGS volcano")?;
    let place =
        volcano::parse_volcano(&document).map_err(|error| rejected("USGS volcano", error))?;
    let position = match (place.latitude, place.longitude) {
        (Some(latitude), Some(longitude)) => format!("{latitude:.4}, {longitude:.4}"),
        _ => String::from("not published by the observatory"),
    };
    let parsed = ProbeReport::new(source, "volcano")
        .station(vnum)
        .product("volcano reference record")
        .observed("reference record; no observation time stated")
        .field("position", position)
        .field(
            "elevation",
            place.elevation_m.map_or_else(
                || String::from("not published"),
                |value| format!("{value} m"),
            ),
        )
        .field(
            "region",
            place
                .region
                .unwrap_or_else(|| String::from("not published")),
        )
        .field(
            "summary",
            place
                .summary
                .unwrap_or_else(|| String::from("not published")),
        )
        .field(
            "record url",
            place.url.unwrap_or_else(|| String::from("not published")),
        );
    Ok(operation_run(
        source,
        "volcano",
        parsed,
        fetched.last.provenance,
        fetched.last.cache,
        {
            let mut all = vec![(fetched.first.provenance, fetched.first.cache)];
            all.extend(extra);
            all
        },
    ))
}

fn volcano_notice(
    source: &'static sources::Source,
    acquire: &Acquire,
    supplied: &[(&str, &str)],
) -> Result<OperationRun, ProbeError> {
    let mut extra = Vec::new();
    let notice = match value(supplied, "notice") {
        Ok(notice) => String::from(notice),
        Err(_) => {
            let fetched = fetch_once(acquire, source, "list", &[])?;
            let document = json_body(&fetched.body, "USGS volcanoes")?;
            let notices = volcano::parse_list(&document)
                .map_err(|error| rejected("USGS volcanoes", error))?;
            let first = notices
                .iter()
                .find(|notice| !notice.identifier.is_empty())
                .ok_or_else(|| {
                    rejected(
                        "USGS volcano notice",
                        "no elevated volcano carried a notice identifier to follow",
                    )
                })?;
            extra.push((fetched.provenance, fetched.cache));
            first.identifier.clone()
        }
    };
    let fetched = fetch_twice(acquire, source, "notice", &[("notice", notice.as_str())])?;
    let document = json_body(&fetched.last.body, "USGS volcano notice")?;
    let synopsis =
        volcano::parse_notice(&document).map_err(|error| rejected("USGS volcano notice", error))?;
    let parsed = ProbeReport::new(source, "notice")
        .station(notice)
        .product("observatory notice synopsis")
        .observed("notice time not stated in the notice body")
        .field("synopsis", synopsis);
    Ok(operation_run(
        source,
        "notice",
        parsed,
        fetched.last.provenance,
        fetched.last.cache,
        {
            let mut all = vec![(fetched.first.provenance, fetched.first.cache)];
            all.extend(extra);
            all
        },
    ))
}

fn current_utc_prefix() -> Result<String, ProbeError> {
    let now = time::now_unix_ms().map_err(|error| rejected("NEXRAD listing", error))?;
    let text = time::format_utc(now).map_err(|error| rejected("NEXRAD listing", error))?;
    let date = text
        .split(' ')
        .next()
        .ok_or_else(|| rejected("NEXRAD listing", "the clock formatted no date"))?;
    Ok(format!("{}/", date.replace('-', "/")))
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

fn integer_value(values: &[(&str, &str)], name: &'static str) -> Result<u32, ProbeError> {
    let raw = value(values, name)?;
    raw.parse::<u32>().map_err(|_| ProbeError::InvalidValue {
        name,
        value: String::from(raw),
    })
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
        ("open-meteo-forecast", "environment") => {
            let document = json_body(body, "Open-Meteo environment")?;
            let record = environment::parse(&document)
                .map_err(|error| rejected("Open-Meteo environment", error))?;
            let first = record.hours.first();
            let peak = record
                .hours
                .iter()
                .filter_map(|hour| hour.cape.map(|cape| (cape, hour.time_ms)))
                .max_by(|left, right| left.0.total_cmp(&right.0));
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("convective environment")
                .observed(first.map_or_else(|| String::from("no hours"), |hour| utc(hour.time_ms)))
                .field("hours", record.hours.len().to_string())
                .field(
                    "peak CAPE",
                    peak.map_or_else(
                        || String::from("not reported"),
                        |(cape, _)| format!("{cape} J/kg"),
                    ),
                )
                .field(
                    "first surface wind",
                    first.and_then(|hour| hour.surface).map_or_else(
                        || String::from("not reported"),
                        |wind| format!("{} km/h from {}°", wind.speed_kmh, wind.from_deg),
                    ),
                )
                .field(
                    "first shear",
                    first
                        .and_then(environment::EnvironmentHour::shear)
                        .map_or_else(
                            || String::from("not reported"),
                            |shear| format!("{} km/h from {}°", shear.speed_kmh, shear.from_deg),
                        ),
                ))
        }
        ("open-meteo-forecast", "land") => {
            let document = json_body(body, "Open-Meteo land")?;
            let record =
                land::parse(&document).map_err(|error| rejected("Open-Meteo land", error))?;
            let first = record.hours.first();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("land surface model")
                .observed(first.map_or_else(|| String::from("no hours"), |hour| utc(hour.time_ms)))
                .field("hours", record.hours.len().to_string())
                .field(
                    "first soil temperature",
                    first.map_or_else(
                        || String::from("not reported"),
                        |hour| {
                            optional(hour.soil_temperature_c, record.unit("soil_temperature_6cm"))
                        },
                    ),
                )
                .field(
                    "first topsoil moisture",
                    first.map_or_else(
                        || String::from("not reported"),
                        |hour| optional(hour.moisture_top, record.unit("soil_moisture_0_to_10cm")),
                    ),
                )
                .field(
                    "first vapour-pressure deficit",
                    first.map_or_else(
                        || String::from("not reported"),
                        |hour| optional(hour.vpd_kpa, record.unit("vapour_pressure_deficit")),
                    ),
                ))
        }
        ("open-meteo-forecast", "snow") => {
            let document = json_body(body, "Open-Meteo snow")?;
            let record =
                snow::parse(&document).map_err(|error| rejected("Open-Meteo snow", error))?;
            let first = record.hours.first();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("snow model")
                .observed(first.map_or_else(|| String::from("no hours"), |hour| utc(hour.time_ms)))
                .field("hours", record.hours.len().to_string())
                .field(
                    "first snow depth",
                    first.map_or_else(
                        || String::from("not reported"),
                        |hour| optional(hour.depth_cm(), record.unit("snow_depth")),
                    ),
                )
                .field(
                    "first snowfall",
                    first.map_or_else(
                        || String::from("not reported"),
                        |hour| optional(hour.snowfall_cm, record.unit("snowfall")),
                    ),
                ))
        }
        ("open-meteo-forecast", "sounding") => {
            let now =
                time::now_unix_ms().map_err(|error| rejected("Open-Meteo sounding", error))?;
            let document = json_body(body, "Open-Meteo sounding")?;
            let record = sounding::parse(&document, now)
                .map_err(|error| rejected("Open-Meteo sounding", error))?;
            let indices = record.indices();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("pressure-level profile")
                .observed(utc(record.time_ms))
                .field("levels", record.levels.len().to_string())
                .field(
                    "surface temperature",
                    optional(record.surface.temperature_c, "°C"),
                )
                .field(
                    "surface pressure",
                    format!("{} hPa", record.surface.pressure_hpa),
                )
                .field(
                    "precipitable water",
                    optional(indices.precipitable_water_mm, "mm"),
                )
                .field(
                    "deep shear",
                    indices.deep_shear.map_or_else(
                        || String::from("not reported"),
                        |(depth, speed)| format!("{speed:.1} m/s over {depth:.0} m"),
                    ),
                ))
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
        ("open-meteo-flood", "hydro-ensemble") => {
            let now = time::now_unix_ms().map_err(|error| rejected("Open-Meteo flood", error))?;
            let document = json_body(body, "Open-Meteo flood ensemble")?;
            let record = hydro::parse(&document, now)
                .map_err(|error| rejected("Open-Meteo flood ensemble", error))?;
            let peak = record.peak();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{:.4}, {:.4}", record.latitude, record.longitude))
                .product("GloFAS ensemble discharge")
                .observed(peak.map_or_else(|| String::from("no days"), |day| utc(day.time_ms)))
                .field("days", record.days.len().to_string())
                .field("ensemble members", record.members.len().to_string())
                .field(
                    "peak discharge",
                    peak.map_or_else(
                        || String::from("not reported"),
                        |day| optional(day.discharge, &record.units),
                    ),
                )
                .field(
                    "widest ensemble spread",
                    record.band_peak().map_or_else(
                        || String::from("not reported"),
                        |(_, day)| {
                            record
                                .days
                                .iter()
                                .position(|candidate| candidate.time_ms == day.time_ms)
                                .and_then(|index| record.spread(index))
                                .map_or_else(
                                    || format!("high only, day {}", utc(day.time_ms)),
                                    |(low, high)| format!("{low}–{high} {}", record.units),
                                )
                        },
                    ),
                ))
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
        ("nexrad-level-2", "list-page") => {
            let text = String::from_utf8_lossy(body);
            let prefix = value(values, "prefix")?;
            let page = network::listing_xml(prefix, &text)
                .map_err(|error| rejected("NEXRAD listing page", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(String::from(prefix))
                .product("Level II listing page")
                .observed("listing time not stated by provider")
                .field("keys", page.keys.len().to_string())
                .field("truncated", format!("{:?}", page.truncated))
                .field(
                    "next continuation",
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
        ("nws-api", "alerts") => {
            let document = json_body(body, "NWS alerts")?;
            let warnings = warnings::parse_collection(&document)
                .map_err(|error| rejected("NWS alerts", error))?;
            let first = warnings.warnings.first();
            Ok(ProbeReport::new(source, operation)
                .product("active warning collection")
                .station(first.map_or_else(
                    || String::from("no warnings"),
                    |warning| format!("{} · {}", warning.event, warning.area),
                ))
                .observed(first.map_or_else(
                    || String::from("no warnings"),
                    |warning| warning.sent.clone(),
                ))
                .field("warnings", warnings.warnings.len().to_string())
                .field("without geometry", warnings.without_geometry.to_string())
                .field("rejected", warnings.rejected.len().to_string())
                .field(
                    "first severity",
                    first.map_or_else(
                        || String::from("not reported"),
                        |warning| warning.severity.label().to_owned(),
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
        ("usgs-water", "history") => {
            let document = json_body(body, "USGS water history")?;
            let site = value(values, "site")?;
            let history = gauge::parse_history(&document, site)
                .map_err(|error| rejected("USGS water history", error))?;
            let newest = history.stage.last();
            let highest = history.highest_stage();
            Ok(ProbeReport::new(source, operation)
                .station(format!("{} · {}", history.site, history.name))
                .product("seven-day gauge history")
                .observed(
                    newest
                        .map_or_else(|| String::from("no readings"), |reading| utc(reading.at_ms)),
                )
                .field("stage readings", history.stage.len().to_string())
                .field("discharge readings", history.discharge.len().to_string())
                .field(
                    "highest stage",
                    highest.map_or_else(
                        || String::from("not reported"),
                        |reading| format!("{} {}", reading.value, reading.unit),
                    ),
                )
                .field(
                    "stage range",
                    history.stage_range().map_or_else(
                        || String::from("not reported"),
                        |range| format!("{range:.2}"),
                    ),
                ))
        }
        ("noaa-swpc", "wind") => {
            let document = json_body(body, "NOAA SWPC solar wind")?;
            let wind = spaceweather::parse_wind_speed(&document)
                .map_err(|error| rejected("NOAA SWPC solar wind", error))?;
            Ok(ProbeReport::new(source, operation)
                .product("solar wind speed")
                .observed(utc(wind.time_ms))
                .field("proton speed", optional(wind.speed_km_s, "km/s")))
        }
        ("noaa-swpc", "magnetic-field") => {
            let document = json_body(body, "NOAA SWPC magnetic field")?;
            let field = spaceweather::parse_magnetic_field(&document)
                .map_err(|error| rejected("NOAA SWPC magnetic field", error))?;
            Ok(ProbeReport::new(source, operation)
                .product("solar wind magnetic field")
                .observed(utc(field.time_ms))
                .field("total field", optional(field.bt_nt, "nT"))
                .field("north-south component", optional(field.bz_nt, "nT")))
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
        ("usgs-volcano", "volcano") => {
            let document = json_body(body, "USGS volcano")?;
            let place = volcano::parse_volcano(&document)
                .map_err(|error| rejected("USGS volcano", error))?;
            let position = match (place.latitude, place.longitude) {
                (Some(latitude), Some(longitude)) => format!("{latitude:.4}, {longitude:.4}"),
                _ => String::from("not published by the observatory"),
            };
            Ok(ProbeReport::new(source, operation)
                .station(value(values, "vnum")?.to_owned())
                .product("volcano reference record")
                .observed("reference record; no observation time stated")
                .field("position", position)
                .field(
                    "elevation",
                    place.elevation_m.map_or_else(
                        || String::from("not published"),
                        |value| format!("{value} m"),
                    ),
                )
                .field(
                    "region",
                    place
                        .region
                        .unwrap_or_else(|| String::from("not published")),
                )
                .field(
                    "summary",
                    place
                        .summary
                        .unwrap_or_else(|| String::from("not published")),
                )
                .field(
                    "record url",
                    place.url.unwrap_or_else(|| String::from("not published")),
                ))
        }
        ("usgs-volcano", "notice") => {
            let document = json_body(body, "USGS volcano notice")?;
            let synopsis = volcano::parse_notice(&document)
                .map_err(|error| rejected("USGS volcano notice", error))?;
            Ok(ProbeReport::new(source, operation)
                .station(value(values, "notice")?.to_owned())
                .product("observatory notice synopsis")
                .observed("notice time not stated in the notice body")
                .field("synopsis", synopsis))
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
    fn every_registry_operation_has_a_probe_path() {
        let sources = sources::SOURCES;
        assert!(!sources.is_empty());
        for source in sources {
            for endpoint in source.endpoints {
                assert!(
                    !endpoint.operation.is_empty(),
                    "{} has an unnamed operation",
                    source.id.as_str()
                );
            }
        }
    }

    #[test]
    fn every_parsed_operation_refuses_empty_and_malformed_bodies() {
        for source in sources::SOURCES {
            if source.level != sources::Level::Admitted {
                continue;
            }
            for endpoint in source.endpoints {
                if matches!(
                    (source.id.as_str(), endpoint.operation),
                    ("openfreemap", "tile") | ("rainviewer", "tile") | ("nexrad-level-2", "object")
                ) {
                    continue;
                }
                let mut owned: Vec<(String, String)> = template_names(endpoint.template)
                    .into_iter()
                    .map(|name| (name, String::from("1")))
                    .collect();
                owned.push((String::from("prefix"), String::from("2026/11/13/")));
                let values: Vec<(&str, &str)> = owned
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str()))
                    .collect();
                let empty = parse(
                    source,
                    endpoint.operation,
                    b"",
                    &values,
                    "https://example.test",
                );
                assert!(
                    empty.is_err(),
                    "{} {} accepted an empty body",
                    source.id.as_str(),
                    endpoint.operation
                );
                let malformed = parse(
                    source,
                    endpoint.operation,
                    b"{not a document",
                    &values,
                    "https://example.test",
                );
                assert!(
                    malformed.is_err(),
                    "{} {} accepted a malformed body",
                    source.id.as_str(),
                    endpoint.operation
                );
            }
        }
    }

    #[test]
    fn an_unknown_operation_is_reported_without_a_fake_value() {
        let error = parse(
            &sources::NOAA_SWPC,
            "not-a-real-operation",
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

    #[test]
    fn a_malformed_solar_wind_row_is_rejected_not_defaulted() {
        let error = parse(
            &sources::NOAA_SWPC,
            "wind",
            br#"[{"time_tag":"not a time","proton_speed":400}]"#,
            &[],
            "https://services.swpc.noaa.gov/products/summary/solar-wind-speed.json",
        );
        assert!(matches!(
            error,
            Err(ProbeError::Rejected {
                parser: "NOAA SWPC solar wind",
                ..
            })
        ));
    }

    #[test]
    fn an_empty_solar_wind_document_is_rejected_not_defaulted() {
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
                parser: "NOAA SWPC solar wind",
                ..
            })
        ));
    }
}
