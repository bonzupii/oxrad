// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//! The offline vector basemap and its geographic view.
//!
//! Natural Earth 1:110m country geometry is compiled into the binary. It is
//! public-domain data, so the map has no tile account, network dependency, or
//! attribution-to-a-tile-server problem. Country rings are filled with a
//! small ear-clipping triangulator and drawn in Web Mercator.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Shape, Stroke, Vec2,
};

use crate::error::CoordinateError;
use crate::projection;

const COUNTRIES: &str = include_str!("../assets/ne_110m_admin_0_countries.geojson");
const OCEAN: Color32 = Color32::from_rgb(8, 18, 27);
const LAND: Color32 = Color32::from_rgb(26, 39, 35);
const COAST: Color32 = Color32::from_rgb(72, 102, 91);
const GRID: Color32 = Color32::from_rgb(43, 60, 69);
const LABEL: Color32 = Color32::from_rgb(170, 188, 184);
const LABEL_SHADOW: Color32 = Color32::from_rgb(8, 18, 27);
const MIN_SPAN_KM: f64 = 80.0;
const MAX_SPAN_KM: f64 = 40_075.0;
const MERCATOR_LIMIT_DEG: f64 = 85.0;

#[derive(Debug)]
pub enum MapError {
    Json { source: serde_json::Error },
    Shape { detail: &'static str },
    Feature { index: usize, detail: &'static str },
    Coordinate { source: CoordinateError },
}

impl MapError {
    fn message(&self) -> String {
        match self {
            Self::Json { source } => format!("The embedded basemap is not valid JSON: {source}"),
            Self::Shape { detail } => format!("The embedded basemap is missing {detail}."),
            Self::Feature { index, detail } => {
                format!("Basemap feature {index} has an invalid {detail}.")
            }
            Self::Coordinate { source } => format!("The basemap could not be projected: {source}"),
        }
    }
}

impl Display for MapError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for MapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json { source } => Some(source),
            Self::Coordinate { source } => Some(source),
            Self::Shape { .. } | Self::Feature { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Country {
    name: String,
    label: Option<(f64, f64)>,
    label_rank: u32,
    rings: Vec<Vec<(f64, f64)>>,
}

#[derive(Debug)]
pub struct WorldMap {
    countries: Vec<Country>,
}

fn coordinate(pair: Option<&serde_json::Value>, index: usize) -> Result<(f64, f64), MapError> {
    let values = pair
        .and_then(serde_json::Value::as_array)
        .ok_or(MapError::Feature {
            index,
            detail: "coordinate pair",
        })?;
    let lon = values
        .first()
        .and_then(serde_json::Value::as_f64)
        .ok_or(MapError::Feature {
            index,
            detail: "longitude",
        })?;
    let lat = values
        .get(1)
        .and_then(serde_json::Value::as_f64)
        .ok_or(MapError::Feature {
            index,
            detail: "latitude",
        })?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(MapError::Feature {
            index,
            detail: "coordinate range",
        });
    }
    Ok((lat, lon))
}

fn ring(value: &serde_json::Value, feature: usize) -> Result<Vec<(f64, f64)>, MapError> {
    let points = value.as_array().ok_or(MapError::Feature {
        index: feature,
        detail: "ring",
    })?;
    let mut result = Vec::with_capacity(points.len());
    for pair in points {
        result.push(coordinate(Some(pair), feature)?);
    }
    if result.len() < 3 {
        return Err(MapError::Feature {
            index: feature,
            detail: "ring with at least three points",
        });
    }
    Ok(result)
}

fn polygons(value: &serde_json::Value, feature: usize) -> Result<Vec<Vec<(f64, f64)>>, MapError> {
    let kind = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(MapError::Feature {
            index: feature,
            detail: "geometry type",
        })?;
    let coordinates = value.get("coordinates").ok_or(MapError::Feature {
        index: feature,
        detail: "geometry coordinates",
    })?;
    match kind {
        "Polygon" => {
            let outer = coordinates
                .as_array()
                .and_then(|rings| rings.first())
                .ok_or(MapError::Feature {
                    index: feature,
                    detail: "polygon ring",
                })?;
            Ok(vec![ring(outer, feature)?])
        }
        "MultiPolygon" => {
            let parts = coordinates.as_array().ok_or(MapError::Feature {
                index: feature,
                detail: "multipolygon",
            })?;
            let mut rings = Vec::new();
            for part in parts {
                let first =
                    part.as_array()
                        .and_then(|value| value.first())
                        .ok_or(MapError::Feature {
                            index: feature,
                            detail: "polygon",
                        })?;
                rings.push(ring(first, feature)?);
            }
            Ok(rings)
        }
        _ => Ok(Vec::new()),
    }
}

fn property_text(properties: &serde_json::Value, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(String::from)
}

impl WorldMap {
    /// Parses the compiled-in Natural Earth country collection.
    pub fn load() -> Result<Self, MapError> {
        let document: serde_json::Value =
            serde_json::from_str(COUNTRIES).map_err(|source| MapError::Json { source })?;
        let features = document
            .get("features")
            .and_then(serde_json::Value::as_array)
            .ok_or(MapError::Shape {
                detail: "the feature collection",
            })?;
        let mut countries = Vec::with_capacity(features.len());
        for (index, feature) in features.iter().enumerate() {
            let properties = feature.get("properties").ok_or(MapError::Feature {
                index,
                detail: "properties",
            })?;
            let Some(name) = property_text(properties, "NAME")
                .or_else(|| property_text(properties, "ADMIN"))
                .or_else(|| property_text(properties, "SOVEREIGNT"))
            else {
                continue;
            };
            // Natural Earth names these fields by axis: LABEL_X is longitude.
            // Storing them as (latitude, longitude) requires Y first.
            let label = properties
                .get("LABEL_Y")
                .and_then(serde_json::Value::as_f64)
                .zip(
                    properties
                        .get("LABEL_X")
                        .and_then(serde_json::Value::as_f64),
                );
            let label_rank = properties
                .get("LABELRANK")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(8);
            let geometry = feature.get("geometry").ok_or(MapError::Feature {
                index,
                detail: "geometry",
            })?;
            let rings = polygons(geometry, index)?;
            if !rings.is_empty() {
                countries.push(Country {
                    name,
                    label,
                    label_rank,
                    rings,
                });
            }
        }
        if countries.is_empty() {
            return Err(MapError::Shape {
                detail: "usable country features",
            });
        }
        Ok(Self { countries })
    }

    pub const fn country_count(&self) -> usize {
        self.countries.len()
    }

    pub fn draw(
        &self,
        painter: &egui::Painter,
        view: &MapView,
        rect: Rect,
    ) -> Result<(), MapError> {
        let transform = view.transform(rect)?;
        painter.rect_filled(rect, CornerRadius::ZERO, OCEAN);
        draw_graticule(painter, &transform, rect);
        let mut labels = Vec::new();
        for country in &self.countries {
            for ring in &country.rings {
                let points: Vec<Pos2> = ring
                    .iter()
                    .filter_map(|(lat, lon)| transform.screen(*lat, *lon))
                    .collect();
                if points.len() < 3 {
                    continue;
                }
                let Some(first) = points.first() else {
                    continue;
                };
                let width = points
                    .iter()
                    .fold(0.0_f32, |width, point| width.max((point.x - first.x).abs()));
                if width > rect.width() * 3.0 {
                    continue;
                }
                if let Some(mesh) = filled_mesh(&points, LAND) {
                    painter.add(Shape::mesh(mesh));
                }
                painter.add(Shape::closed_line(points, Stroke::new(0.7_f32, COAST)));
            }
            if let Some((lat, lon)) = country.label {
                labels.push((country.name.as_str(), country.label_rank, lat, lon));
            }
        }
        draw_labels(painter, &transform, rect, &labels, view.span_km);
        Ok(())
    }
}

fn filled_mesh(points: &[Pos2], colour: Color32) -> Option<Mesh> {
    let triangles = triangulate(points)?;
    let mut mesh = Mesh::default();
    for point in points {
        mesh.colored_vertex(*point, colour);
    }
    for triangle in triangles {
        mesh.add_triangle(triangle[0], triangle[1], triangle[2]);
    }
    Some(mesh)
}

fn cross(a: Pos2, b: Pos2, c: Pos2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn in_triangle(point: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let ab = cross(a, b, point);
    let bc = cross(b, c, point);
    let ca = cross(c, a, point);
    (ab >= 0.0 && bc >= 0.0 && ca >= 0.0) || (ab <= 0.0 && bc <= 0.0 && ca <= 0.0)
}

/// Ear clipping for a simple, hole-free Geographic ring. Returns `None` when
/// the ring is degenerate or self-intersecting instead of filling nonsense.
fn triangulate(points: &[Pos2]) -> Option<Vec<[u32; 3]>> {
    if points.len() < 3 {
        return None;
    }
    let area: f32 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| a.x * b.y - b.x * a.y)
        .sum();
    if area.abs() < 1e-6 {
        return None;
    }
    let mut indices: Vec<usize> = (0..points.len()).collect();
    if area < 0.0 {
        indices.reverse();
    }
    let mut triangles = Vec::with_capacity(indices.len().saturating_sub(2));
    let mut attempts = 0_usize;
    while indices.len() > 3 {
        if attempts > indices.len() * indices.len() {
            return None;
        }
        attempts += 1;
        let mut ear = None;
        for position in 0..indices.len() {
            let previous = position.checked_sub(1).unwrap_or(indices.len() - 1);
            let next = (position + 1) % indices.len();
            let (Some(a_index), Some(b_index), Some(c_index)) = (
                indices.get(previous),
                indices.get(position),
                indices.get(next),
            ) else {
                return None;
            };
            let (Some(a), Some(b), Some(c)) = (
                points.get(*a_index),
                points.get(*b_index),
                points.get(*c_index),
            ) else {
                return None;
            };
            if cross(*a, *b, *c) <= 0.0 {
                continue;
            }
            let contains = indices.iter().enumerate().any(|(other_position, index)| {
                if other_position == previous
                    || other_position == position
                    || other_position == next
                {
                    return false;
                }
                points
                    .get(*index)
                    .is_some_and(|point| in_triangle(*point, *a, *b, *c))
            });
            if !contains {
                ear = Some((position, *a_index, *b_index, *c_index));
                break;
            }
        }
        let (position, a, b, c) = ear?;
        triangles.push([
            u32::try_from(a).ok()?,
            u32::try_from(b).ok()?,
            u32::try_from(c).ok()?,
        ]);
        indices.remove(position);
    }
    if let [a, b, c] = indices.as_slice() {
        triangles.push([
            u32::try_from(*a).ok()?,
            u32::try_from(*b).ok()?,
            u32::try_from(*c).ok()?,
        ]);
    }
    Some(triangles)
}

fn draw_graticule(painter: &egui::Painter, transform: &MapTransform, rect: Rect) {
    let stroke = Stroke::new(0.5_f32, GRID);
    let (north, _) = transform.latlon_at(Pos2::new(rect.center().x, rect.top()));
    let (south, _) = transform.latlon_at(Pos2::new(rect.center().x, rect.bottom()));
    let mut latitude = (south / 15.0).floor() * 15.0;
    while latitude <= north {
        let mut points = Vec::new();
        for step in 0..=72 {
            let lon = -180.0 + f64::from(step) * 5.0;
            if let Some(point) = transform.screen(latitude, lon) {
                points.push(point);
            }
        }
        if points.len() > 1 {
            painter.add(Shape::line(points, stroke));
        }
        latitude += 15.0;
    }
    let mut longitude = -180.0;
    while longitude <= 180.0 {
        let mut points = Vec::new();
        for step in 0..=36 {
            let lat = -80.0 + f64::from(step) * 80.0 / 18.0;
            if let Some(point) = transform.screen(lat, longitude) {
                points.push(point);
            }
        }
        if points.len() > 1 {
            painter.add(Shape::line(points, stroke));
        }
        longitude += 15.0;
    }
}

fn draw_labels(
    painter: &egui::Painter,
    transform: &MapTransform,
    rect: Rect,
    labels: &[(&str, u32, f64, f64)],
    span_km: f64,
) {
    let rank_limit = if span_km > 12_000.0 {
        2
    } else if span_km > 5_000.0 {
        4
    } else if span_km > 1_500.0 {
        6
    } else {
        10
    };
    let mut used: Vec<Rect> = Vec::new();
    for (name, rank, lat, lon) in labels {
        if *rank > rank_limit {
            continue;
        }
        let Some(position) = transform.screen(*lat, *lon) else {
            continue;
        };
        if !rect.shrink(8.0).contains(position) {
            continue;
        }
        let galley = painter.layout_no_wrap(String::from(*name), FontId::proportional(11.0), LABEL);
        let box_rect = Rect::from_center_size(position, galley.size() + Vec2::new(6.0, 2.0));
        if used.iter().any(|other| other.intersects(box_rect)) {
            continue;
        }
        used.push(box_rect);
        painter.text(
            position + Vec2::new(0.7, 0.7),
            Align2::CENTER_CENTER,
            String::from(*name),
            FontId::proportional(11.0),
            LABEL_SHADOW,
        );
        painter.galley(position - galley.size() / 2.0, galley, LABEL);
        if used.len() >= 70 {
            break;
        }
    }
}

/// Immutable geographic view. The centre is kept in Mercator kilometres so
/// zooming under the pointer is exact and cannot drift at high latitudes.
#[derive(Debug, Clone, Copy)]
pub struct MapView {
    center_x: f64,
    center_y: f64,
    span_km: f64,
}

impl Default for MapView {
    fn default() -> Self {
        let (center_x, center_y) = projection::mercator(25.0, -20.0).unwrap_or((0.0, 0.0));
        Self {
            center_x,
            center_y,
            span_km: 18_000.0,
        }
    }
}

impl MapView {
    pub fn focus(lat: f64, lon: f64, span_km: f64) -> Result<Self, MapError> {
        let (center_x, center_y) =
            projection::mercator(lat, lon).map_err(|source| MapError::Coordinate { source })?;
        Ok(Self {
            center_x,
            center_y,
            span_km: span_km.clamp(MIN_SPAN_KM, MAX_SPAN_KM),
        })
    }

    pub fn center_latlon(&self) -> Result<(f64, f64), MapError> {
        projection::mercator_inverse(self.center_x, self.center_y)
            .map_err(|source| MapError::Coordinate { source })
    }

    pub const fn span_km(&self) -> f64 {
        self.span_km
    }

    pub fn transform(&self, rect: Rect) -> Result<MapTransform, MapError> {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(MapError::Coordinate {
                source: CoordinateError::Viewport {
                    width: rect.width(),
                    height: rect.height(),
                },
            });
        }
        Ok(MapTransform {
            rect,
            center_x: self.center_x,
            center_y: self.center_y,
            km_per_pixel: self.span_km / f64::from(rect.width()),
        })
    }

    pub fn pan(&mut self, rect: Rect, pixels: Vec2) -> Result<(), MapError> {
        let transform = self.transform(rect)?;
        self.center_x -= f64::from(pixels.x) * transform.km_per_pixel;
        self.center_y += f64::from(pixels.y) * transform.km_per_pixel;
        self.clamp_center()?;
        Ok(())
    }

    pub fn zoom_at(&mut self, rect: Rect, pointer: Pos2, factor: f64) -> Result<(), MapError> {
        if !factor.is_finite() || factor <= 0.0 {
            return Ok(());
        }
        let before = self.transform(rect)?.latlon_at(pointer);
        let Ok((target_x, target_y)) = projection::mercator(before.0, before.1) else {
            return Ok(());
        };
        self.span_km = (self.span_km / factor).clamp(MIN_SPAN_KM, MAX_SPAN_KM);
        let transform = self.transform(rect)?;
        let offset = pointer - rect.center();
        self.center_x = target_x - f64::from(offset.x) * transform.km_per_pixel;
        self.center_y = target_y + f64::from(offset.y) * transform.km_per_pixel;
        self.clamp_center()
    }

    fn clamp_center(&mut self) -> Result<(), MapError> {
        self.center_x = projection::mercator_wrap_delta(self.center_x);
        let (_, limit_y) = projection::mercator(MERCATOR_LIMIT_DEG, 0.0)
            .map_err(|source| MapError::Coordinate { source })?;
        self.center_y = self.center_y.clamp(-limit_y, limit_y);
        if !(self.center_x.is_finite() && self.center_y.is_finite() && self.span_km.is_finite()) {
            self.center_x = 0.0;
            self.center_y = 0.0;
            self.span_km = 9_000.0;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MapTransform {
    rect: Rect,
    center_x: f64,
    center_y: f64,
    km_per_pixel: f64,
}

impl MapTransform {
    pub fn screen(&self, lat: f64, lon: f64) -> Option<Pos2> {
        let lat = lat.clamp(-MERCATOR_LIMIT_DEG, MERCATOR_LIMIT_DEG);
        let (x, y) = projection::mercator(lat, lon).ok()?;
        let dx = projection::mercator_wrap_delta(x - self.center_x) / self.km_per_pixel;
        let dy = (y - self.center_y) / self.km_per_pixel;
        Some(Pos2::new(
            self.rect.center().x + dx as f32,
            self.rect.center().y - dy as f32,
        ))
    }

    pub fn latlon_at(&self, position: Pos2) -> (f64, f64) {
        let x = self.center_x + f64::from(position.x - self.rect.center().x) * self.km_per_pixel;
        let y = self.center_y - f64::from(position.y - self.rect.center().y) * self.km_per_pixel;
        projection::mercator_inverse(x, y).unwrap_or((0.0, 0.0))
    }

    pub const fn kilometers_per_pixel(&self) -> f64 {
        self.km_per_pixel
    }
}

pub fn draw_scale(painter: &egui::Painter, rect: Rect, transform: &MapTransform) {
    let target_pixels = 110.0_f32;
    let raw_km = f64::from(target_pixels) * transform.kilometers_per_pixel();
    let power = 10_f64.powf(raw_km.log10().floor());
    let candidates = [1.0, 2.0, 5.0, 10.0];
    let kilometers = candidates
        .iter()
        .map(|factor| factor * power)
        .find(|value| *value >= raw_km * 0.5)
        .unwrap_or(power * 10.0);
    let pixels = (kilometers / transform.kilometers_per_pixel()) as f32;
    let start = Pos2::new(rect.left() + 16.0, rect.bottom() - 30.0);
    painter.rect_filled(
        Rect::from_min_size(start - Vec2::new(6.0, 20.0), Vec2::new(pixels + 18.0, 32.0)),
        CornerRadius::same(3),
        Color32::from_black_alpha(150),
    );
    painter.line_segment(
        [start, start + Vec2::new(pixels, 0.0)],
        Stroke::new(2.0_f32, LABEL),
    );
    painter.line_segment(
        [start + Vec2::new(0.0, -4.0), start + Vec2::new(0.0, 4.0)],
        Stroke::new(1.0_f32, LABEL),
    );
    painter.line_segment(
        [
            start + Vec2::new(pixels, -4.0),
            start + Vec2::new(pixels, 4.0),
        ],
        Stroke::new(1.0_f32, LABEL),
    );
    let label = if kilometers >= 1000.0 {
        format!("{:.0} 000 km", kilometers / 1000.0)
    } else {
        format!("{kilometers:.0} km")
    };
    painter.text(
        start + Vec2::new(pixels / 2.0, -8.0),
        Align2::CENTER_BOTTOM,
        label,
        FontId::monospace(10.0),
        LABEL,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn embedded_world_has_the_natural_earth_countries() -> Result<(), TestError> {
        let map = WorldMap::load()?;
        assert_eq!(map.country_count(), 177);
        Ok(())
    }

    #[test]
    fn country_labels_use_latitude_before_longitude() -> Result<(), TestError> {
        let map = WorldMap::load()?;
        let france = map
            .countries
            .iter()
            .find(|country| country.name == "France")
            .ok_or("France missing")?;
        let (lat, lon) = france.label.ok_or("France label missing")?;
        assert!((40.0..=53.0).contains(&lat), "France label latitude {lat}");
        assert!((-6.0..=10.0).contains(&lon), "France label longitude {lon}");
        Ok(())
    }

    #[test]
    fn triangulates_a_concave_square() -> Result<(), TestError> {
        let points = [
            Pos2::new(0.0, 0.0),
            Pos2::new(4.0, 0.0),
            Pos2::new(4.0, 4.0),
            Pos2::new(2.0, 2.0),
            Pos2::new(0.0, 4.0),
        ];
        let triangles = triangulate(&points).ok_or("concave ring failed")?;
        assert_eq!(triangles.len(), 3);
        Ok(())
    }

    #[test]
    fn map_zoom_keeps_the_point_under_the_pointer() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut view = MapView::focus(37.0, -88.0, 2_000.0)?;
        let pointer = Pos2::new(620.0, 210.0);
        let before = view.transform(rect)?.latlon_at(pointer);
        view.zoom_at(rect, pointer, 2.0)?;
        let after = view.transform(rect)?.latlon_at(pointer);
        assert!((before.0 - after.0).abs() < 1e-6, "latitude moved");
        assert!((before.1 - after.1).abs() < 1e-6, "longitude moved");
        Ok(())
    }

    #[test]
    fn map_pan_moves_the_centre_west_when_dragged_east() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut view = MapView::focus(37.0, -88.0, 2_000.0)?;
        let before = view.center_latlon()?;
        view.pan(rect, Vec2::new(100.0, 0.0))?;
        let after = view.center_latlon()?;
        assert!(after.1 < before.1, "drag east must move the map west");
        Ok(())
    }
}
