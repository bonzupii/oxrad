// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! The request set behind one readout: every URL, operation, cache state and
//! attribution, with the tile or listing page each request served.
//!
//! A report is built only from `Provenance` records the engine produced and
//! templates read from the registry. It never composes a URL of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::engine::{CacheState, Provenance, Served};
use crate::sources;

/// One request and the displayed part it served.
#[derive(Debug, Clone)]
pub(crate) struct ReportedRequest {
    pub(crate) label: Option<String>,
    pub(crate) provenance: Provenance,
    pub(crate) cache: CacheState,
}

impl ReportedRequest {
    pub(crate) const fn new(
        label: Option<String>,
        provenance: Provenance,
        cache: CacheState,
    ) -> Self {
        Self {
            label,
            provenance,
            cache,
        }
    }
}

/// The complete set of requests behind one displayed value.
#[derive(Debug, Clone)]
pub(crate) struct RequestReport {
    title: String,
    requests: Vec<ReportedRequest>,
}

impl RequestReport {
    pub(crate) fn new(title: impl Into<String>, requests: Vec<ReportedRequest>) -> Self {
        Self {
            title: title.into(),
            requests,
        }
    }

    pub(crate) fn from_served<T>(title: impl Into<String>, served: &Served<T>) -> Self {
        let requests = served
            .entries()
            .map(|entry| {
                ReportedRequest::new(
                    entry.label.map(String::from),
                    entry.provenance.clone(),
                    entry.cache,
                )
            })
            .collect();
        Self::new(title, requests)
    }

    pub(crate) const fn count(&self) -> usize {
        self.requests.len()
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn requests(&self) -> &[ReportedRequest] {
        &self.requests
    }

    /// The first request's attribution, for the report heading.
    pub(crate) fn primary(&self) -> Option<&ReportedRequest> {
        self.requests.first()
    }

    /// Hosts actually contacted, in stable order.
    pub(crate) fn hosts(&self) -> Vec<String> {
        self.requests
            .iter()
            .filter_map(|request| host_of(&request.provenance.url))
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect()
    }

    /// Registry templates for every operation in the set, in stable order.
    pub(crate) fn templates(&self) -> Vec<&'static str> {
        self.requests
            .iter()
            .filter_map(|request| template_of(&request.provenance))
            .collect::<BTreeSet<&'static str>>()
            .into_iter()
            .collect()
    }

    /// Cache states with how many requests ended in each state.
    pub(crate) fn cache_counts(&self) -> Vec<(&'static str, usize)> {
        let mut counts = BTreeMap::new();
        for request in &self.requests {
            *counts.entry(request.cache.label()).or_insert(0_usize) += 1;
        }
        counts.into_iter().collect()
    }

    pub(crate) fn cache_summary(&self) -> String {
        self.cache_counts()
            .into_iter()
            .map(|(state, count)| format!("{count} {state}"))
            .collect::<Vec<String>>()
            .join(" · ")
    }

    /// A short set summary: count, hosts, templates and cache states.
    pub(crate) fn summary(&self) -> Vec<String> {
        let hosts = self.hosts();
        let host_text = if hosts.is_empty() {
            String::from("host not stated")
        } else {
            hosts.join(", ")
        };
        let mut lines = vec![format!(
            "{} requests · {host_text} · cache {}",
            self.requests.len(),
            self.cache_summary()
        )];
        let templates = self.templates();
        if templates.is_empty() {
            lines.push(String::from(
                "registry template unavailable for this request",
            ));
        } else {
            for template in templates {
                lines.push(format!("template {template}"));
            }
        }
        lines
    }

    /// Up to `limit` labelled request URLs, with a plain count of the rest.
    pub(crate) fn samples(&self, limit: usize) -> Vec<String> {
        self.requests
            .iter()
            .take(limit)
            .map(|request| {
                let label = request
                    .label
                    .as_deref()
                    .unwrap_or(request.provenance.subject.as_str());
                format!(
                    "{label} · cache {} · {}",
                    request.cache.label(),
                    request.provenance.url
                )
            })
            .collect()
    }

    /// Every request as one JSON object, suitable for export.
    pub(crate) fn json(&self) -> Value {
        let requests: Vec<Value> = self
            .requests
            .iter()
            .map(|request| {
                json!({
                    "label": request.label,
                    "source": request.provenance.source,
                    "source_name": request.provenance.source_name,
                    "operation": request.provenance.operation,
                    "subject": request.provenance.subject,
                    "station": request.provenance.station,
                    "product": request.provenance.product,
                    "scan_unix_ms": request.provenance.scan_unix_ms,
                    "url": request.provenance.url,
                    "cache": request.cache.label(),
                    "credit": request.provenance.credit,
                    "licence": request.provenance.licence,
                    "licence_url": request.provenance.licence_url,
                    "rate_limit_seconds": request.provenance.rate_limit_seconds,
                })
            })
            .collect();
        json!({
            "title": self.title,
            "count": self.requests.len(),
            "hosts": self.hosts(),
            "templates": self.templates(),
            "cache": self.cache_summary(),
            "requests": requests,
        })
    }

    /// One line per request with every attribution field, for headless output
    /// and deterministic formatter tests.
    pub(crate) fn text(&self) -> String {
        let mut lines = vec![format!("{}: {}", self.title, self.summary().join(" · "))];
        for request in &self.requests {
            let label = request
                .label
                .as_deref()
                .unwrap_or(request.provenance.subject.as_str());
            lines.push(format!(
                "{label} · source {} · operation {} · cache {} · url {} · credit {} · licence {} · licence_url {}",
                request.provenance.source_name,
                request.provenance.operation.map_or("not named", |value| value),
                request.cache.label(),
                request.provenance.url,
                request.provenance.credit,
                request.provenance.licence,
                request.provenance.licence_url
            ));
        }
        lines.join("\n")
    }

    /// Writes the raw list to the cache directory and returns the path.
    pub(crate) fn write(&self) -> Result<PathBuf, String> {
        let directory = crate::paths::cache_directory()
            .map_err(|error| error.to_string())?
            .join("requests");
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let source = self
            .primary()
            .map_or("unattributed", |request| request.provenance.source);
        let operation = self
            .primary()
            .and_then(|request| request.provenance.operation)
            .map_or("operation", |value| value);
        let path = directory.join(format!("{source}-{operation}.json"));
        let encoded = serde_json::to_vec_pretty(&self.json()).map_err(|error| error.to_string())?;
        std::fs::write(&path, encoded).map_err(|error| error.to_string())?;
        Ok(path)
    }
}

/// The host of an absolute URL, without inventing one for a relative value.
fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix(sources::HTTPS_SCHEME)?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    Some(String::from(host))
}

/// The registry's template for the operation a request used.
fn template_of(provenance: &Provenance) -> Option<&'static str> {
    let source = sources::find(provenance.source)?;
    let operation = provenance.operation?;
    source
        .operation(operation)
        .ok()
        .map(|endpoint| endpoint.template)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::engine::RequestSpec;
    use crate::test_support::TestError;

    fn request(
        source: &'static sources::Source,
        operation: &'static str,
        values: &[(&str, &str)],
        cache: CacheState,
    ) -> Result<ReportedRequest, TestError> {
        let spec = RequestSpec::for_operation(source, operation, values)
            .map_err(|error| TestError::from(error.to_string()))?;
        Ok(ReportedRequest::new(None, spec.provenance(), cache))
    }

    #[test]
    fn a_tile_set_names_every_request_from_registry_data() -> Result<(), TestError> {
        let first = request(
            &sources::NASA_GIBS,
            "tile",
            &[
                ("layer", "MODIS_Terra_CorrectedReflectance_TrueColor"),
                ("level", "9"),
                ("format", "image%2Fjpeg"),
                ("time", "2026-11-10"),
                ("zoom", "4"),
                ("x", "3"),
                ("y", "6"),
            ],
            CacheState::Cached,
        )?;
        let second = request(
            &sources::NASA_GIBS,
            "tile",
            &[
                ("layer", "MODIS_Terra_CorrectedReflectance_TrueColor"),
                ("level", "9"),
                ("format", "image%2Fjpeg"),
                ("time", "2026-11-10"),
                ("zoom", "4"),
                ("x", "4"),
                ("y", "6"),
            ],
            CacheState::Network,
        )?;
        let report = RequestReport::new(
            "Satellite tile requests",
            vec![
                ReportedRequest {
                    label: Some(String::from("tile z4 x3 y6")),
                    ..first
                },
                ReportedRequest {
                    label: Some(String::from("tile z4 x4 y6")),
                    ..second
                },
            ],
        );
        assert_eq!(report.count(), 2);
        assert_eq!(
            report.hosts(),
            vec![String::from("gibs.earthdata.nasa.gov")]
        );
        assert_eq!(
            report.templates(),
            vec![
                "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/wmts.cgi?SERVICE=WMTS&REQUEST=GetTile&VERSION=1.0.0&LAYER={layer}&STYLE=default&TILEMATRIXSET=GoogleMapsCompatible_Level{level}&TILEMATRIX={zoom}&TILEROW={y}&TILECOL={x}&FORMAT={raw:format}&TIME={raw:time}"
            ]
        );
        assert_eq!(report.cache_counts(), vec![("cached", 1), ("live", 1)]);
        let text = report.text();
        assert!(text.contains("tile z4 x3 y6"));
        assert!(text.contains("credit NASA EOSDIS GIBS"));
        assert!(text.contains("licence NASA open data, no key required"));
        assert!(text.contains("licence_url https://nasa-gibs.github.io/gibs-api-docs/"));
        assert!(text.contains("cache cached"));
        assert!(text.contains("cache live"));
        Ok(())
    }

    #[test]
    fn a_listing_request_names_its_registry_operation() -> Result<(), TestError> {
        let listing = request(
            &sources::NEXRAD_LEVEL_TWO,
            "list",
            &[("prefix", "2026/11/13/KTWX/")],
            CacheState::Network,
        )?;
        let object = request(
            &sources::NEXRAD_LEVEL_TWO,
            "object",
            &[("key", "2026/11/13/KTWX/KTWX20261113_000000_V06")],
            CacheState::Cached,
        )?;
        let report = RequestReport::new(
            "NEXRAD volume requests",
            vec![
                ReportedRequest {
                    label: Some(String::from("discovery listing 2026/11/13/KTWX/")),
                    ..listing
                },
                ReportedRequest {
                    label: Some(String::from("Level II object")),
                    ..object
                },
            ],
        );
        assert_eq!(report.count(), 2);
        let text = report.text();
        assert!(text.contains("operation list"));
        assert!(text.contains("operation object"));
        assert!(text.contains(
            "url https://unidata-nexrad-level2.s3.amazonaws.com?list-type=2&prefix=2026"
        ));
        assert!(text.contains(
            "url https://unidata-nexrad-level2.s3.amazonaws.com/2026/11/13/KTWX/KTWX20261113_000000_V06"
        ));
        assert!(text.contains("credit NOAA NEXRAD, distributed by Unidata"));
        assert!(text.contains("licence US Government work, public domain"));
        assert!(text.contains("licence_url https://www.usa.gov/government-copyright"));
        assert!(text.contains("cache live"));
        assert!(text.contains("cache cached"));
        Ok(())
    }

    #[test]
    fn a_basemap_tile_set_reports_each_vector_tile() -> Result<(), TestError> {
        let first = request(
            &sources::OPENFREEMAP,
            "tile",
            &[("zoom", "4"), ("x", "3"), ("y", "7")],
            CacheState::Cached,
        )?;
        let second = request(
            &sources::OPENFREEMAP,
            "tile",
            &[("zoom", "4"), ("x", "4"), ("y", "7")],
            CacheState::Network,
        )?;
        let report = RequestReport::new(
            "Basemap vector tile requests",
            vec![
                ReportedRequest {
                    label: Some(String::from("tile z4 x3 y7")),
                    ..first
                },
                ReportedRequest {
                    label: Some(String::from("tile z4 x4 y7")),
                    ..second
                },
            ],
        );
        let text = report.text();
        assert!(text.contains("tile z4 x3 y7"));
        assert!(text.contains("tile z4 x4 y7"));
        assert!(text.contains("operation tile"));
        assert!(text.contains("url https://tiles.openfreemap.org/planet/latest/4/3/7.pbf"));
        assert!(text.contains("url https://tiles.openfreemap.org/planet/latest/4/4/7.pbf"));
        assert!(text.contains("credit OpenFreeMap, OpenMapTiles, OpenStreetMap contributors"));
        assert!(text.contains("licence OpenStreetMap data, ODbL 1.0"));
        assert!(text.contains("licence_url https://www.openstreetmap.org/copyright"));
        assert!(text.contains("cache cached"));
        assert!(text.contains("cache live"));
        Ok(())
    }

    #[test]
    fn a_rainviewer_composite_reports_manifest_and_tiles() -> Result<(), TestError> {
        let manifest = request(&sources::RAINVIEWER, "manifest", &[], CacheState::Cached)?;
        let tile = request(
            &sources::RAINVIEWER,
            "tile",
            &[
                ("host", "https://tilecache.rainviewer.com"),
                ("path", "/v2/radar/abc"),
                ("size", "512"),
                ("zoom", "4"),
                ("x", "3"),
                ("y", "6"),
                ("scheme", "2"),
            ],
            CacheState::Network,
        )?;
        let report = RequestReport::new(
            "RainViewer composite requests",
            vec![
                ReportedRequest {
                    label: Some(String::from("composite manifest")),
                    ..manifest
                },
                ReportedRequest {
                    label: Some(String::from(
                        "frame 2026-11-13 00:00:00 UTC · tile z4 x3 y6",
                    )),
                    ..tile
                },
            ],
        );
        assert_eq!(report.count(), 2);
        assert_eq!(
            report.hosts(),
            vec![
                String::from("api.rainviewer.com"),
                String::from("tilecache.rainviewer.com")
            ]
        );
        let text = report.text();
        assert!(text.contains("operation manifest"));
        assert!(text.contains("operation tile"));
        assert!(text.contains("url https://api.rainviewer.com/public/weather-maps.json"));
        assert!(
            text.contains("url https://tilecache.rainviewer.com/v2/radar/abc/512/4/3/6/2/1_1.png")
        );
        assert!(text.contains("credit Weather data by RainViewer"));
        assert!(text.contains("licence RainViewer free terms, attribution required"));
        assert!(text.contains("licence_url https://www.rainviewer.com/api.html"));
        assert!(text.contains("cache cached"));
        assert!(text.contains("cache live"));
        Ok(())
    }

    #[test]
    fn no_request_is_hidden_from_the_export() -> Result<(), TestError> {
        let coordinates: Vec<String> = (0..12).map(|value| value.to_string()).collect();
        let requests = coordinates
            .iter()
            .map(|x| {
                request(
                    &sources::OPENFREEMAP,
                    "tile",
                    &[("zoom", "4"), ("x", x.as_str()), ("y", "7")],
                    CacheState::Cached,
                )
            })
            .collect::<Result<Vec<ReportedRequest>, TestError>>()?;
        let report = RequestReport::new("Basemap tile requests", requests);
        let json = report.json();
        assert_eq!(json.get("count").and_then(Value::as_u64), Some(12));
        assert_eq!(
            json.get("requests").and_then(Value::as_array).map(Vec::len),
            Some(12)
        );
        assert_eq!(report.samples(3).len(), 3);
        assert!(report.samples(3)[0].contains("cache cached"));
        assert_eq!(report.hosts(), vec![String::from("tiles.openfreemap.org")]);
        Ok(())
    }
}
