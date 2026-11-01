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
//! Supplemental Natural Earth layers for the geographic map.
//!
//! The country outlines in `map` answer "where is the border". These layers
//! answer "where is the town, the state line, and the river" without a tile
//! server or an account. They are compiled into the binary and filtered by map
//! span, so a world view does not draw every settlement on Earth.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::map::MapTransform;
use crate::theme::Palette;

const PLACES: &str = include_str!("../assets/ne_10m_populated_places_simple.geojson");
const ADMIN_LINES: &str = include_str!("../assets/ne_50m_admin_1_states_provinces_lines.geojson");
const RIVERS: &str = include_str!("../assets/ne_50m_rivers_lake_centerlines.geojson");
const MAX_PLACE_LABELS: usize = 70;
const ADMIN_SPAN_LIMIT_KM: f64 = 6_000.0;
const RIVER_SPAN_LIMIT_KM: f64 = 3_000.0;
const LABEL_GAP: f32 = 4.0;
const CULL_MARGIN: f32 = 80.0;

#[derive(Debug)]
pub enum BasemapError {
    Json {
        layer: &'static str,
        source: serde_json::Error,
    },
    Shape {
        layer: &'static str,
        detail: &'static str,
    },
    Feature {
        layer: &'static str,
        index: usize,
        detail: &'static str,
    },
}

impl BasemapError {
    fn message(&self) -> String {
        match self {
            Self::Json { layer, source } => {
                format!("The embedded {layer} layer is not valid JSON: {source}")
            }
            Self::Shape { layer, detail } => {
                format!("The embedded {layer} layer is missing {detail}.")
            }
            Self::Feature {
                layer,
                index,
                detail,
            } => format!("{layer} feature {index} has an invalid {detail}."),
        }
    }
}

impl Display for BasemapError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for BasemapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json { source, .. } => Some(source),
            Self::Shape { .. } | Self::Feature { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Place {
    name: String,
    lat: f64,
    lon: f64,
    population: u64,
    rank: u32,
}

#[derive(Debug, Clone)]
struct Polyline {
    points: Vec<(f64, f64)>,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
}

/// The supplemental map layers, parsed once at startup.
#[derive(Debug)]
pub struct BasemapLayers {
    places: Vec<Place>,
    admin: Vec<Polyline>,
    rivers: Vec<Polyline>,
}

impl BasemapLayers {
    /// Parses the compiled-in Natural Earth place, boundary, and river files.
    ///
    /// # Errors
    ///
    /// Returns [`BasemapError`] when an embedded document is malformed or a
    /// coordinate is outside the geographic range.
    pub fn load() -> Result<Self, BasemapError> {
        let places = parse_places()?;
        let admin = parse_lines(ADMIN_LINES, "admin-1 boundaries")?;
        let rivers = parse_lines(RIVERS, "rivers")?;
        Ok(Self {
            places,
            admin,
            rivers,
        })
    }

    /// Draws boundaries and rivers under the radar and warning layers.
    pub fn draw_lines(
        &self,
        painter: &egui::Painter,
        transform: &MapTransform,
        rect: Rect,
        span_km: f64,
        palette: &Palette,
    ) {
        if span_km <= RIVER_SPAN_LIMIT_KM {
            let stroke = Stroke::new(0.8_f32, with_alpha(palette.accent, 160));
            for line in &self.rivers {
                if visible(line, transform, rect) {
                    draw_polyline(painter, transform, rect, line, stroke);
                }
            }
        }
        if span_km <= ADMIN_SPAN_LIMIT_KM {
            let stroke = Stroke::new(0.7_f32, with_alpha(palette.coast, 190));
            for line in &self.admin {
                if visible(line, transform, rect) {
                    draw_polyline(painter, transform, rect, line, stroke);
                }
            }
        }
    }

    /// Draws settlement dots and decluttered labels above the data layers.
    pub fn draw_places(
        &self,
        painter: &egui::Painter,
        transform: &MapTransform,
        rect: Rect,
        span_km: f64,
        palette: &Palette,
    ) {
        let mut labels: Vec<Rect> = Vec::new();
        for place in &self.places {
            if !place_visible(place, span_km) {
                continue;
            }
            let Some(position) = transform.screen(place.lat, place.lon) else {
                continue;
            };
            if !rect.contains(position) {
                continue;
            }
            painter.circle_filled(position, 2.5_f32, palette.map_text);
            if labels.len() >= MAX_PLACE_LABELS {
                continue;
            }
            let galley = painter.layout_no_wrap(
                place.name.clone(),
                FontId::proportional(11.0),
                palette.map_text,
            );
            let origin = position + Vec2::new(6.0, -galley.size().y / 2.0);
            let label = Rect::from_min_size(origin, galley.size() + Vec2::splat(LABEL_GAP));
            if labels.iter().any(|other| other.intersects(label)) {
                continue;
            }
            labels.push(label);
            painter.rect_filled(
                label,
                egui::CornerRadius::same(2),
                with_alpha(palette.surface, 210),
            );
            painter.galley(origin + Vec2::splat(2.0), galley, palette.map_text);
        }
    }
}

fn with_alpha(colour: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

fn place_visible(place: &Place, span_km: f64) -> bool {
    if span_km > 5_000.0 {
        place.population >= 2_000_000 || place.rank <= 2
    } else if span_km > 2_000.0 {
        place.population >= 500_000 || place.rank <= 4
    } else if span_km > 800.0 {
        place.population >= 25_000 || place.rank <= 6
    } else if span_km > 300.0 {
        place.population >= 10_000 || place.rank <= 7
    } else {
        place.population >= 1_000 || place.rank <= 8
    }
}

fn visible(line: &Polyline, transform: &MapTransform, rect: Rect) -> bool {
    let corners = [
        transform.screen(line.min_lat, line.min_lon),
        transform.screen(line.min_lat, line.max_lon),
        transform.screen(line.max_lat, line.min_lon),
        transform.screen(line.max_lat, line.max_lon),
    ];
    let mut bounds: Option<Rect> = None;
    for corner in corners.into_iter().flatten() {
        bounds = Some(bounds.map_or_else(
            || Rect::from_min_max(corner, corner),
            |current| {
                Rect::from_min_max(
                    Pos2::new(current.min.x.min(corner.x), current.min.y.min(corner.y)),
                    Pos2::new(current.max.x.max(corner.x), current.max.y.max(corner.y)),
                )
            },
        ));
    }
    bounds.is_some_and(|bounds| bounds.intersects(rect.expand(CULL_MARGIN)))
}

fn draw_polyline(
    painter: &egui::Painter,
    transform: &MapTransform,
    rect: Rect,
    line: &Polyline,
    stroke: Stroke,
) {
    let max_jump = rect.width().max(rect.height());
    let mut previous: Option<Pos2> = None;
    for (lat, lon) in &line.points {
        match transform.screen(*lat, *lon) {
            Some(point) => {
                if let Some(previous) = previous
                    && previous.distance(point) <= max_jump
                {
                    painter.line_segment([previous, point], stroke);
                }
                previous = Some(point);
            }
            None => previous = None,
        }
    }
}

fn parse_places() -> Result<Vec<Place>, BasemapError> {
    let document: Value = serde_json::from_str(PLACES).map_err(|source| BasemapError::Json {
        layer: "populated places",
        source,
    })?;
    let features = features(&document, "populated places")?;
    let mut places = Vec::new();
    for (index, feature) in features.iter().enumerate() {
        let Some(properties) = feature.get("properties") else {
            return Err(BasemapError::Feature {
                layer: "populated places",
                index,
                detail: "properties",
            });
        };
        let Some(name) = property_text(properties, "nameascii")
            .or_else(|| property_text(properties, "name"))
            .filter(|name| !name.trim().is_empty())
        else {
            continue;
        };
        let coordinates = feature
            .get("geometry")
            .and_then(|geometry| geometry.get("coordinates"))
            .and_then(Value::as_array)
            .ok_or(BasemapError::Feature {
                layer: "populated places",
                index,
                detail: "point coordinates",
            })?;
        let lon = coordinates
            .first()
            .and_then(Value::as_f64)
            .ok_or(BasemapError::Feature {
                layer: "populated places",
                index,
                detail: "longitude",
            })?;
        let lat = coordinates
            .get(1)
            .and_then(Value::as_f64)
            .ok_or(BasemapError::Feature {
                layer: "populated places",
                index,
                detail: "latitude",
            })?;
        validate(lat, lon, "populated places", index)?;
        let population = properties
            .get("pop_max")
            .and_then(Value::as_f64)
            .map_or(0, |value| value.max(0.0).round() as u64);
        let rank = properties
            .get("labelrank")
            .and_then(Value::as_u64)
            .map_or(8, |value| u32::try_from(value).unwrap_or(8));
        places.push(Place {
            name: String::from(name),
            lat,
            lon,
            population,
            rank,
        });
    }
    places.sort_by(|left, right| {
        right
            .population
            .cmp(&left.population)
            .then(left.rank.cmp(&right.rank))
    });
    if places.is_empty() {
        return Err(BasemapError::Shape {
            layer: "populated places",
            detail: "at least one populated place",
        });
    }
    Ok(places)
}

fn parse_lines(source: &str, layer: &'static str) -> Result<Vec<Polyline>, BasemapError> {
    let document: Value =
        serde_json::from_str(source).map_err(|source| BasemapError::Json { layer, source })?;
    let features = features(&document, layer)?;
    let mut lines = Vec::new();
    for (index, feature) in features.iter().enumerate() {
        let Some(geometry) = feature.get("geometry") else {
            return Err(BasemapError::Feature {
                layer,
                index,
                detail: "geometry",
            });
        };
        let kind = geometry
            .get("type")
            .and_then(Value::as_str)
            .ok_or(BasemapError::Feature {
                layer,
                index,
                detail: "geometry type",
            })?;
        let coordinates = geometry.get("coordinates").ok_or(BasemapError::Feature {
            layer,
            index,
            detail: "geometry coordinates",
        })?;
        match kind {
            "LineString" => lines.push(polyline(coordinates, layer, index)?),
            "MultiLineString" => {
                let parts = coordinates.as_array().ok_or(BasemapError::Feature {
                    layer,
                    index,
                    detail: "line parts",
                })?;
                for part in parts {
                    lines.push(polyline(part, layer, index)?);
                }
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        return Err(BasemapError::Shape {
            layer,
            detail: "at least one line",
        });
    }
    Ok(lines)
}

fn polyline(value: &Value, layer: &'static str, index: usize) -> Result<Polyline, BasemapError> {
    let pairs = value.as_array().ok_or(BasemapError::Feature {
        layer,
        index,
        detail: "line coordinates",
    })?;
    let mut points = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let values = pair.as_array().ok_or(BasemapError::Feature {
            layer,
            index,
            detail: "coordinate pair",
        })?;
        let lon = values
            .first()
            .and_then(Value::as_f64)
            .ok_or(BasemapError::Feature {
                layer,
                index,
                detail: "longitude",
            })?;
        let lat = values
            .get(1)
            .and_then(Value::as_f64)
            .ok_or(BasemapError::Feature {
                layer,
                index,
                detail: "latitude",
            })?;
        validate(lat, lon, layer, index)?;
        points.push((lat, lon));
    }
    if points.len() < 2 {
        return Err(BasemapError::Feature {
            layer,
            index,
            detail: "line with at least two points",
        });
    }
    let mut min_lat = f64::INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    for (lat, lon) in &points {
        min_lat = min_lat.min(*lat);
        min_lon = min_lon.min(*lon);
        max_lat = max_lat.max(*lat);
        max_lon = max_lon.max(*lon);
    }
    Ok(Polyline {
        points,
        min_lat,
        min_lon,
        max_lat,
        max_lon,
    })
}

fn validate(lat: f64, lon: f64, layer: &'static str, index: usize) -> Result<(), BasemapError> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(BasemapError::Feature {
            layer,
            index,
            detail: "coordinate range",
        });
    }
    Ok(())
}

fn features<'a>(document: &'a Value, layer: &'static str) -> Result<&'a [Value], BasemapError> {
    document
        .get("features")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(BasemapError::Shape {
            layer,
            detail: "a feature collection",
        })
}

fn property_text<'a>(properties: &'a Value, key: &str) -> Option<&'a str> {
    properties
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn loads_global_places_boundaries_and_rivers() -> Result<(), TestError> {
        let layers = BasemapLayers::load()?;
        assert!(
            layers.places.len() > 5_000,
            "places {}",
            layers.places.len()
        );
        assert!(
            layers.admin.len() > 500,
            "admin lines {}",
            layers.admin.len()
        );
        assert!(layers.rivers.len() > 300, "rivers {}", layers.rivers.len());
        Ok(())
    }

    #[test]
    fn settlement_visibility_follows_the_map_span() {
        let town = Place {
            name: String::from("Test"),
            lat: 0.0,
            lon: 0.0,
            population: 30_000,
            rank: 7,
        };
        assert!(place_visible(&town, 950.0));
        assert!(!place_visible(&town, 5_000.0));
    }

    #[test]
    fn polyline_bounds_contain_every_point() -> Result<(), TestError> {
        let value = serde_json::json!([[-88.0, 37.0], [-87.0, 38.0]]);
        let line = polyline(&value, "test", 0)?;
        assert_eq!(line.min_lat, 37.0);
        assert_eq!(line.min_lon, -88.0);
        assert_eq!(line.max_lat, 38.0);
        assert_eq!(line.max_lon, -87.0);
        Ok(())
    }
}
