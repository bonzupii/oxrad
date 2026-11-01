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
use crate::theme::Palette;
use crate::units::{self, UnitSystem};

const COUNTRIES: &str = include_str!("../assets/ne_110m_admin_0_countries.geojson");
const MIN_SPAN_KM: f64 = 80.0;
const MAX_SPAN_KM: f64 = 40_075.0;
const MERCATOR_LIMIT_DEG: f64 = 85.0;
/// The coarsest spacing, used when even that is dense for the view.
const RING_COARSEST_KM: f64 = 300.0;
/// Ring spacings offered, finest first.
const RING_STEPS_KM: [f64; 6] = [10.0, 25.0, 50.0, 100.0, 200.0, RING_COARSEST_KM];
/// Rings to aim for before the spacing is coarsened.
const RINGS_PER_VIEW: f64 = 5.0;
/// The most rings any view may carry.
const RING_LIMIT: u32 = 6;
const MAP_RING_SAMPLES: u32 = 128;
const MAP_SPOKES: u32 = 8;

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
        palette: &Palette,
    ) -> Result<(), MapError> {
        let transform = view.transform(rect)?;
        painter.rect_filled(rect, CornerRadius::ZERO, palette.ocean);
        draw_graticule(painter, &transform, rect, view.span_km, palette)?;
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
                if let Some(mesh) = filled_mesh(&points, palette.land) {
                    painter.add(Shape::mesh(mesh));
                }
                painter.add(Shape::closed_line(
                    points,
                    Stroke::new(0.7_f32, palette.coast),
                ));
            }
            if let Some((lat, lon)) = country.label {
                labels.push((country.name.as_str(), country.label_rank, lat, lon));
            }
        }
        if view.span_km >= 2_000.0 {
            draw_labels(painter, &transform, rect, &labels, view.span_km, palette);
        }
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

fn graticule_step_degrees(span_km: f64) -> f64 {
    if span_km < 1_500.0 {
        1.0
    } else if span_km < 5_000.0 {
        5.0
    } else {
        15.0
    }
}

fn draw_graticule(
    painter: &egui::Painter,
    transform: &MapTransform,
    rect: Rect,
    span_km: f64,
    palette: &Palette,
) -> Result<(), MapError> {
    let stroke = Stroke::new(0.5_f32, palette.graticule);
    let step = graticule_step_degrees(span_km);
    let (north, _) = transform.clipped_latlon_at(Pos2::new(rect.center().x, rect.top()))?;
    let (south, _) = transform.clipped_latlon_at(Pos2::new(rect.center().x, rect.bottom()))?;
    let mut latitude = (south / step).floor() * step;
    while latitude <= north {
        let mut points = Vec::new();
        for sample in 0..=72 {
            let lon = -180.0 + f64::from(sample) * 5.0;
            if let Some(point) = transform.screen(latitude, lon) {
                points.push(point);
            }
        }
        if points.len() > 1 {
            painter.add(Shape::line(points, stroke));
        }
        latitude += step;
    }
    let mut longitude = -180.0;
    while longitude <= 180.0 {
        let visible = transform
            .screen(0.0, longitude)
            .is_some_and(|point| point.x >= rect.left() - 1.0 && point.x <= rect.right() + 1.0);
        if visible {
            let mut points = Vec::new();
            for sample in 0..=36 {
                let lat = -80.0 + f64::from(sample) * 80.0 / 18.0;
                if let Some(point) = transform.screen(lat, longitude) {
                    points.push(point);
                }
            }
            if points.len() > 1 {
                painter.add(Shape::line(points, stroke));
            }
        }
        longitude += step;
    }
    Ok(())
}

fn ring_points(
    transform: &MapTransform,
    site_lat: f64,
    site_lon: f64,
    range_km: f64,
) -> Result<Vec<Pos2>, MapError> {
    let mut points = Vec::with_capacity(MAP_RING_SAMPLES as usize + 1);
    for sample in 0..=MAP_RING_SAMPLES {
        let azimuth_deg = f64::from(sample) * 360.0 / f64::from(MAP_RING_SAMPLES);
        let (lat, lon) = projection::forward(range_km, azimuth_deg, site_lat, site_lon)
            .map_err(|source| MapError::Coordinate { source })?;
        if let Some(point) = transform.screen(lat, lon) {
            points.push(point);
        }
    }
    Ok(points)
}

/// Which rings a view carries, from the ground it shows and the range the radar
/// product actually reaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingPlan {
    pub step_km: f64,
    pub count: u32,
}

impl RingPlan {
    /// Rings for a view span and a radar coverage.
    ///
    /// A fixed 50 km ladder drew seven rings whether the view was a 100 km
    /// street window or a whole continent, and carried on to 400 km for a
    /// product whose range stops well short of that. Scale the spacing to the
    /// ground actually on screen and stop at the coverage the product has.
    pub fn for_view(span_km: f64, coverage_km: f64) -> Self {
        let span_km = if span_km.is_finite() && span_km > 0.0 {
            span_km
        } else {
            RING_STEPS_KM[0]
        };
        let coverage_km = if coverage_km.is_finite() && coverage_km > 0.0 {
            coverage_km
        } else {
            span_km
        };
        let reach_km = coverage_km.min(span_km * 0.75);
        let mut step_km = RING_COARSEST_KM;
        for candidate in RING_STEPS_KM {
            if reach_km / candidate <= RINGS_PER_VIEW {
                step_km = candidate;
                break;
            }
        }
        let count = (reach_km / step_km)
            .floor()
            .max(1.0)
            .min(f64::from(RING_LIMIT)) as u32;
        Self { step_km, count }
    }

    pub fn outermost_km(&self) -> f64 {
        self.step_km * f64::from(self.count)
    }
}

/// Place one ring's distance label.
///
/// Labels alternate between the south-east and north-west diagonals of the
/// site, so successive rings never stack their labels down one spoke through
/// the middle of the echo, and consecutive labels are separated vertically as
/// well as horizontally.
fn ring_label(context: &RingContext<'_>, range_km: f64, index: u32) -> Result<(), MapError> {
    // Odd rings carry their label south-east of the site and even rings
    // north-west, so no two labels share a line through the echo.
    let azimuth_deg = if index.is_multiple_of(2) {
        315.0
    } else {
        135.0
    };
    let (lat, lon) = projection::forward(range_km, azimuth_deg, context.site_lat, context.site_lon)
        .map_err(|source| MapError::Coordinate { source })?;
    let Some(point) = context.transform.screen(lat, lon) else {
        return Ok(());
    };
    if !context.rect.shrink(14.0).contains(point) {
        return Ok(());
    }
    let galley = context.painter.layout_no_wrap(
        units::distance_whole(range_km, context.units),
        FontId::monospace(10.0),
        context.palette.map_text,
    );
    let palette = context.palette;
    let origin = Pos2::new(point.x - galley.size().x / 2.0, point.y + 4.0);
    context.painter.rect_filled(
        Rect::from_min_size(origin - Vec2::splat(2.0), galley.size() + Vec2::splat(4.0)),
        CornerRadius::same(2),
        Color32::from_rgba_unmultiplied(
            palette.surface.r(),
            palette.surface.g(),
            palette.surface.b(),
            215,
        ),
    );
    context.painter.galley(origin, galley, palette.map_text);
    Ok(())
}

/// One ring pass: the painter, the view it lands in, the site it centres on,
/// the range the product reaches, and the presentation preferences.
pub struct RingContext<'a> {
    pub painter: &'a egui::Painter,
    pub rect: Rect,
    pub transform: &'a MapTransform,
    pub site_lat: f64,
    pub site_lon: f64,
    pub coverage_km: f64,
    pub units: UnitSystem,
    pub palette: &'a Palette,
}

pub fn draw_range_rings(context: &RingContext<'_>) -> Result<(), MapError> {
    let Some(site) = context.transform.screen(context.site_lat, context.site_lon) else {
        return Ok(());
    };
    let span_km = context.transform.kilometers_per_pixel()
        * f64::from(context.rect.width().max(context.rect.height()));
    let plan = RingPlan::for_view(span_km, context.coverage_km);
    let max_jump = context.rect.width().max(context.rect.height()) * 0.5;
    let palette = context.palette;
    let ring_stroke = Stroke::new(
        1.0_f32,
        Color32::from_rgba_unmultiplied(palette.ring.r(), palette.ring.g(), palette.ring.b(), 185),
    );
    let spoke_stroke = Stroke::new(
        1.0_f32,
        Color32::from_rgba_unmultiplied(palette.ring.r(), palette.ring.g(), palette.ring.b(), 95),
    );
    for sample in 0..MAP_SPOKES {
        let azimuth_deg = f64::from(sample) * 360.0 / f64::from(MAP_SPOKES);
        let (lat, lon) = projection::forward(
            plan.outermost_km(),
            azimuth_deg,
            context.site_lat,
            context.site_lon,
        )
        .map_err(|source| MapError::Coordinate { source })?;
        if let Some(point) = context.transform.screen(lat, lon)
            && site.distance(point) <= max_jump
        {
            context.painter.line_segment([site, point], spoke_stroke);
        }
    }
    for index in 1..=plan.count {
        let range_km = plan.step_km * f64::from(index);
        let points = ring_points(
            context.transform,
            context.site_lat,
            context.site_lon,
            range_km,
        )?;
        for pair in points.windows(2) {
            if let [a, b] = pair
                && a.distance(*b) <= max_jump
            {
                context.painter.line_segment([*a, *b], ring_stroke);
            }
        }
        ring_label(context, range_km, index)?;
    }
    Ok(())
}

fn draw_labels(
    painter: &egui::Painter,
    transform: &MapTransform,
    rect: Rect,
    labels: &[(&str, u32, f64, f64)],
    span_km: f64,
    palette: &Palette,
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
        let galley = painter.layout_no_wrap(
            String::from(*name),
            FontId::proportional(11.0),
            palette.map_text,
        );
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
            palette.surface,
        );
        painter.galley(position - galley.size() / 2.0, galley, palette.map_text);
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
        // The Mercator origin is zero by definition: zero longitude and the
        // equator both project to zero, so the default view needs no fallible
        // projection and no invented centre. The application focuses the view
        // on a searched place or a radar site as soon as it has one.
        Self {
            center_x: 0.0,
            center_y: 0.0,
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
        if !rect.width().is_finite()
            || !rect.height().is_finite()
            || rect.width() <= 0.0
            || rect.height() <= 0.0
        {
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
        let mut next = *self;
        next.center_x -= f64::from(pixels.x) * transform.km_per_pixel;
        next.center_y += f64::from(pixels.y) * transform.km_per_pixel;
        next.clamp_center()?;
        *self = next;
        Ok(())
    }

    pub fn zoom_at(&mut self, rect: Rect, pointer: Pos2, factor: f64) -> Result<(), MapError> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(MapError::Coordinate {
                source: CoordinateError::ZoomFactor { factor },
            });
        }
        let before = self.transform(rect)?.latlon_at(pointer)?;
        let (target_x, target_y) = projection::mercator(before.0, before.1)
            .map_err(|source| MapError::Coordinate { source })?;
        let mut next = *self;
        next.span_km = (next.span_km / factor).clamp(MIN_SPAN_KM, MAX_SPAN_KM);
        let transform = next.transform(rect)?;
        let offset = pointer - rect.center();
        next.center_x = target_x - f64::from(offset.x) * transform.km_per_pixel;
        next.center_y = target_y + f64::from(offset.y) * transform.km_per_pixel;
        next.clamp_center()?;
        *self = next;
        Ok(())
    }

    fn clamp_center(&mut self) -> Result<(), MapError> {
        if !(self.center_x.is_finite() && self.center_y.is_finite()) {
            return Err(MapError::Coordinate {
                source: CoordinateError::MapPosition {
                    x: self.center_x,
                    y: self.center_y,
                },
            });
        }
        if !self.span_km.is_finite() || self.span_km <= 0.0 {
            return Err(MapError::Coordinate {
                source: CoordinateError::ViewExtent {
                    extent_km: self.span_km,
                },
            });
        }
        self.center_x = projection::mercator_wrap_delta(self.center_x);
        let (_, limit_y) = projection::mercator(MERCATOR_LIMIT_DEG, 0.0)
            .map_err(|source| MapError::Coordinate { source })?;
        self.center_y = self.center_y.clamp(-limit_y, limit_y);
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
    /// Where a coordinate lands in this view, or nothing when it cannot be
    /// placed at all.
    ///
    /// The latitude is clamped into the Mercator domain first, so the only
    /// rejection left is a longitude the projection refuses — and there is no
    /// position to report for one. That is an unplottable coordinate, not a
    /// failure of this view, and callers treat it like a point off screen.
    pub fn screen(&self, lat: f64, lon: f64) -> Option<Pos2> {
        let lat = lat.clamp(-MERCATOR_LIMIT_DEG, MERCATOR_LIMIT_DEG);
        let (x, y) = match projection::mercator(lat, lon) {
            Ok(point) => point,
            Err(_) => return None,
        };
        let dx = projection::mercator_wrap_delta(x - self.center_x) / self.km_per_pixel;
        let dy = (y - self.center_y) / self.km_per_pixel;
        Some(Pos2::new(
            self.rect.center().x + dx as f32,
            self.rect.center().y - dy as f32,
        ))
    }

    pub fn latlon_at(&self, position: Pos2) -> Result<(f64, f64), MapError> {
        let x = self.center_x + f64::from(position.x - self.rect.center().x) * self.km_per_pixel;
        let y = self.center_y - f64::from(position.y - self.rect.center().y) * self.km_per_pixel;
        projection::mercator_inverse(x, y).map_err(|source| MapError::Coordinate { source })
    }

    fn clipped_latlon_at(&self, position: Pos2) -> Result<(f64, f64), MapError> {
        let x = self.center_x + f64::from(position.x - self.rect.center().x) * self.km_per_pixel;
        let y = self.center_y - f64::from(position.y - self.rect.center().y) * self.km_per_pixel;
        let (_, limit) = projection::mercator(MERCATOR_LIMIT_DEG, 0.0)
            .map_err(|source| MapError::Coordinate { source })?;
        projection::mercator_inverse(x, y.clamp(-limit, limit))
            .map_err(|source| MapError::Coordinate { source })
    }

    pub const fn kilometers_per_pixel(&self) -> f64 {
        self.km_per_pixel
    }
}

pub fn draw_scale(
    painter: &egui::Painter,
    rect: Rect,
    transform: &MapTransform,
    palette: &Palette,
) {
    let target_pixels = 110.0_f32;
    let raw_km = f64::from(target_pixels) * transform.kilometers_per_pixel();
    let power = 10_f64.powf(raw_km.log10().floor());
    // A 1-2-5 ladder: the first rung that fills at least half the target is the
    // roundest number that still reaches a usable length on screen. The coarsest
    // rung always qualifies, because power is within a decade of raw_km.
    let mut kilometers = 10.0 * power;
    for factor in [1.0, 2.0, 5.0, 10.0] {
        if factor * power >= raw_km * 0.5 {
            kilometers = factor * power;
            break;
        }
    }
    let pixels = (kilometers / transform.kilometers_per_pixel()) as f32;
    let start = Pos2::new(rect.left() + 16.0, rect.bottom() - 30.0);
    painter.rect_filled(
        Rect::from_min_size(start - Vec2::new(6.0, 20.0), Vec2::new(pixels + 18.0, 32.0)),
        CornerRadius::same(3),
        Color32::from_rgba_unmultiplied(
            palette.surface.r(),
            palette.surface.g(),
            palette.surface.b(),
            210,
        ),
    );
    painter.line_segment(
        [start, start + Vec2::new(pixels, 0.0)],
        Stroke::new(2.0_f32, palette.map_text),
    );
    painter.line_segment(
        [start + Vec2::new(0.0, -4.0), start + Vec2::new(0.0, 4.0)],
        Stroke::new(1.0_f32, palette.map_text),
    );
    painter.line_segment(
        [
            start + Vec2::new(pixels, -4.0),
            start + Vec2::new(pixels, 4.0),
        ],
        Stroke::new(1.0_f32, palette.map_text),
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
        palette.map_text,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn rings_stop_where_the_product_stops() {
        // KTWX base reflectivity reaches about 460 km. A view fitted to the
        // whole sweep must not carry a ring past that, whatever the view holds.
        let plan = RingPlan::for_view(1_250.0, 460.0);
        assert!(
            plan.outermost_km() <= 460.0,
            "rings reached {} km on a 460 km product",
            plan.outermost_km()
        );
        assert!(
            plan.count >= 2,
            "a fitted radar view needs more than one ring"
        );
    }

    #[test]
    fn rings_keep_five_or_fewer_on_screen() {
        for span_km in [40.0, 120.0, 400.0, 1_250.0, 6_000.0, 18_000.0] {
            let plan = RingPlan::for_view(span_km, 460.0);
            assert!(
                plan.count <= RING_LIMIT,
                "{span_km} km view drew {} rings",
                plan.count
            );
            assert!(plan.count >= 1, "{span_km} km view drew no rings");
            assert!(
                plan.outermost_km() <= span_km * 0.75 + plan.step_km,
                "{span_km} km view drew rings out to {} km",
                plan.outermost_km()
            );
        }
    }

    #[test]
    fn zooming_in_tightens_the_ring_spacing() {
        let wide = RingPlan::for_view(1_250.0, 460.0);
        let close = RingPlan::for_view(60.0, 460.0);
        assert!(
            close.step_km < wide.step_km,
            "a 60 km view used {} km rings, a 1,250 km view used {} km",
            close.step_km,
            wide.step_km
        );
    }

    #[test]
    fn a_short_range_radar_gets_rings_inside_its_own_reach() {
        // Terminal and short-range products can stop at 150 km; a 400 km ring
        // would draw coverage that does not exist.
        let plan = RingPlan::for_view(1_250.0, 150.0);
        assert!(plan.outermost_km() <= 150.0);
    }

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
        let before = view.transform(rect)?.latlon_at(pointer)?;
        view.zoom_at(rect, pointer, 2.0)?;
        let after = view.transform(rect)?.latlon_at(pointer)?;
        assert!((before.0 - after.0).abs() < 1e-6, "latitude moved");
        assert!((before.1 - after.1).abs() < 1e-6, "longitude moved");
        Ok(())
    }

    #[test]
    fn map_readout_rejects_coordinates_outside_mercator() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let view = MapView::focus(80.0, 0.0, 18_000.0)?;
        let transform = view.transform(rect)?;
        assert!(matches!(
            transform.latlon_at(rect.left_top()),
            Err(MapError::Coordinate {
                source: CoordinateError::MercatorLatitude { .. }
            })
        ));
        let (latitude, _) = transform.clipped_latlon_at(rect.left_top())?;
        assert!((latitude - MERCATOR_LIMIT_DEG).abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn invalid_pan_preserves_the_previous_view() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut view = MapView::focus(37.0, -88.0, 2_000.0)?;
        let before = view.center_latlon()?;
        assert!(matches!(
            view.pan(rect, Vec2::new(f32::NAN, 0.0)),
            Err(MapError::Coordinate {
                source: CoordinateError::MapPosition { .. }
            })
        ));
        let after = view.center_latlon()?;
        assert!((before.0 - after.0).abs() < 1e-12);
        assert!((before.1 - after.1).abs() < 1e-12);
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

    #[test]
    fn graticule_densifies_at_radar_zoom() {
        assert_eq!(graticule_step_degrees(950.0), 1.0);
        assert_eq!(graticule_step_degrees(2_200.0), 5.0);
        assert_eq!(graticule_step_degrees(18_000.0), 15.0);
    }

    #[test]
    fn range_ring_is_closed_and_scaled_to_the_map() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let view = MapView::focus(38.997, -96.232, 950.0)?;
        let transform = view.transform(rect)?;
        let points = ring_points(&transform, 38.997, -96.232, 50.0)?;
        assert_eq!(points.len(), MAP_RING_SAMPLES as usize + 1);
        let first = points.first().ok_or("ring has no first point")?;
        let last = points.last().ok_or("ring has no last point")?;
        assert!(first.distance(*last) < 0.01, "ring is not closed");
        let site = transform
            .screen(38.997, -96.232)
            .ok_or("site is outside the projection")?;
        for point in &points {
            let radius = point.distance(site);
            assert!((40.0..=65.0).contains(&radius), "ring radius {radius}");
        }
        Ok(())
    }
}
