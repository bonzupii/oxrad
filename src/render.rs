// oxrad — A storm-chaser-grade NEXRAD weather radar viewer
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
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{Color32, Mesh, Pos2, Rect, Shape, Vec2};

use crate::colormap::{color_for_reflectivity, range_folded_color};
use crate::error::{CoordinateError, RenderError};
use crate::gpu::RadarGpu;
use crate::projection;
use crate::radar::{GateCorner, GateValue, RadarBin, RadarVolume};

const HOVER_CELL_PX: f32 = 8.0;
const RADAR_ALPHA: u8 = 255;
const CANONICAL_SIZE: f32 = 1080.0;

pub fn canonical_rect() -> Rect {
    Rect::from_min_size(Pos2::ZERO, Vec2::splat(CANONICAL_SIZE))
}

pub fn radar_viewport(rect: Rect) -> Rect {
    Rect::from_center_size(rect.center(), Vec2::splat(rect.width().min(rect.height())))
}

pub struct RenderRequest {
    pub volume: RadarVolume,
    pub rect: Rect,
    pub generation: u64,
}

pub struct RenderResult {
    pub volume: RadarVolume,
    pub cache: Result<(RenderCache, Shape), RenderError>,
    pub generation: u64,
}

pub struct RenderCache {
    pub rect: Rect,
    pub transform: ViewTransform,
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub index_count: u32,
    pub hover: HoverIndex,
    pub rings: Vec<(f64, Vec<Pos2>)>,
    pub grid: Vec<Vec<Pos2>>,
}

#[derive(Clone, Copy)]
pub struct ViewTransform {
    center: Pos2,
    pixels_per_km: f64,
    site_lat: f64,
    site_lon: f64,
    site_x: f64,
    site_y: f64,
}

pub struct PointerInfo {
    pub lat: f64,
    pub lon: f64,
    pub range_km: f64,
    pub azimuth_deg: f64,
}

pub struct HoverIndex {
    origin: Pos2,
    cell: f32,
    cols: usize,
    rows: usize,
    cells: Vec<Vec<usize>>,
    entries: Vec<HoverEntry>,
}

struct HoverEntry {
    points: [Pos2; 4],
    value: GateValue,
    bounds: Rect,
}

pub fn start_render_thread(
    gpu: Option<RadarGpu>,
    errors: Sender<u64>,
) -> (Sender<RenderRequest>, Receiver<RenderResult>) {
    let (request_sender, request_receiver) = mpsc::channel::<RenderRequest>();
    let (result_sender, result_receiver) = mpsc::channel::<RenderResult>();
    std::thread::spawn(move || {
        while let Ok(request) = request_receiver.recv() {
            let cache = catch_unwind(AssertUnwindSafe(|| {
                let mut cache = build_render(&request.volume, request.rect)?;
                let gpu = gpu.as_ref().ok_or(RenderError::GpuUnavailable)?;
                let shape = gpu.upload(&cache, errors.clone(), request.generation)?;
                cache.vertices = Vec::new();
                cache.indices = Vec::new();
                Ok((cache, shape))
            }))
            .map_err(|_| RenderError::Panic)
            .and_then(std::convert::identity);
            let result = RenderResult {
                volume: request.volume,
                cache,
                generation: request.generation,
            };
            if result_sender.send(result).is_err() {
                return;
            }
        }
    });
    (request_sender, result_receiver)
}

pub fn build_render(volume: &RadarVolume, rect: Rect) -> Result<RenderCache, RenderError> {
    let transform =
        ViewTransform::new(rect, volume).map_err(|source| RenderError::Coordinate { source })?;
    let mut mesh = Mesh::default();
    let mut hover = HoverIndex::new(rect);
    let mut previous = None;
    let mut paths = None;

    for bin in &volume.bins {
        let points = screen_quad(bin, &transform, &mut previous, &mut paths)?;
        let bounds = quad_bounds(&points);
        if !rect.intersects(bounds) {
            continue;
        }
        if !matches!(bin.value, GateValue::BelowThreshold) {
            let color = bin_color(bin.value)?;
            add_quad(&mut mesh, &points, color)?;
        }
        hover.insert(points, bin.value, bounds);
    }

    let index_count = u32::try_from(mesh.indices.len()).map_err(|_| RenderError::MeshSize {
        vertices: mesh.vertices.len(),
    })?;
    let mut vertices = Vec::with_capacity(mesh.vertices.len() * 12);
    for vertex in &mesh.vertices {
        let x = 2.0 * (vertex.pos.x - rect.left()) / rect.width() - 1.0;
        let y = 1.0 - 2.0 * (vertex.pos.y - rect.top()) / rect.height();
        vertices.extend_from_slice(&x.to_le_bytes());
        vertices.extend_from_slice(&y.to_le_bytes());
        vertices.extend_from_slice(&vertex.color.to_array());
    }
    let mut indices = Vec::with_capacity(mesh.indices.len() * 4);
    for index in &mesh.indices {
        indices.extend_from_slice(&index.to_le_bytes());
    }
    Ok(RenderCache {
        rect,
        transform,
        vertices,
        indices,
        index_count,
        hover,
        rings: build_rings(&transform, volume.max_range_km)?,
        grid: build_grid(&transform)?,
    })
}

fn build_grid(transform: &ViewTransform) -> Result<Vec<Vec<Pos2>>, RenderError> {
    let mut lines = Vec::new();
    for step in -2..=2 {
        for meridian in [true, false] {
            let mut line = Vec::new();
            for sample in 0..=32 {
                let delta = -2.5 + 5.0 * f64::from(sample) / 32.0;
                let (lat, lon) = if meridian {
                    (
                        transform.site_lat + delta,
                        transform.site_lon + f64::from(step),
                    )
                } else {
                    (
                        transform.site_lat + f64::from(step),
                        transform.site_lon + delta,
                    )
                };
                let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
                if (-85.051_128_779_806_6..=85.051_128_779_806_6).contains(&lat) {
                    line.push(
                        transform
                            .screen_geo(lat, lon)
                            .map_err(|source| RenderError::Coordinate { source })?,
                    );
                }
            }
            lines.push(line);
        }
    }
    Ok(lines)
}

fn build_rings(
    transform: &ViewTransform,
    max_range_km: f64,
) -> Result<Vec<(f64, Vec<Pos2>)>, RenderError> {
    let mut rings = Vec::new();
    let mut range_km = 50.0;
    while range_km <= max_range_km {
        let mut points = Vec::with_capacity(129);
        for step in 0..=128 {
            let corner = GateCorner {
                range_km,
                azimuth_deg: f64::from(step) * 360.0 / 128.0,
            };
            points.push(
                transform
                    .screen_corner(corner)
                    .map_err(|source| RenderError::Coordinate { source })?,
            );
        }
        rings.push((range_km, points));
        range_km += 50.0;
    }
    Ok(rings)
}

impl ViewTransform {
    pub fn new(rect: Rect, volume: &RadarVolume) -> Result<Self, CoordinateError> {
        let (site_x, site_y) = projection::mercator(volume.site_lat, volume.site_lon)?;
        let range_km = volume.max_range_km;
        if !range_km.is_finite() || range_km <= 0.0 {
            return Err(CoordinateError::ViewRange { range_km });
        }
        let scale = volume
            .site_lat
            .clamp(-85.051_128_779_806_6, 85.051_128_779_806_6)
            .to_radians()
            .cos();
        let pixels_per_km = f64::from(rect.width().min(rect.height())) * 0.48 * scale / range_km;
        Ok(Self {
            center: rect.center(),
            pixels_per_km,
            site_lat: volume.site_lat,
            site_lon: volume.site_lon,
            site_x,
            site_y,
        })
    }

    pub fn for_viewport(&self, viewport: Rect) -> Self {
        Self {
            center: viewport.center(),
            pixels_per_km: self.pixels_per_km * f64::from(viewport.width() / CANONICAL_SIZE),
            site_lat: self.site_lat,
            site_lon: self.site_lon,
            site_x: self.site_x,
            site_y: self.site_y,
        }
    }

    pub fn screen_local(&self, east_km: f64, north_km: f64) -> Pos2 {
        self.center
            + Vec2::new(
                (east_km * self.pixels_per_km) as f32,
                (-north_km * self.pixels_per_km) as f32,
            )
    }

    pub fn local_at(&self, position: Pos2) -> (f64, f64) {
        let east_km = f64::from(position.x - self.center.x) / self.pixels_per_km;
        let north_km = -f64::from(position.y - self.center.y) / self.pixels_per_km;
        (east_km, north_km)
    }

    pub fn screen_geo(&self, lat: f64, lon: f64) -> Result<Pos2, CoordinateError> {
        let (x, y) = projection::mercator(lat, lon)?;
        Ok(self.screen_local(
            projection::mercator_wrap_delta(x - self.site_x),
            y - self.site_y,
        ))
    }

    pub fn pointer_info(&self, position: Pos2) -> Result<PointerInfo, CoordinateError> {
        let (east_km, north_km) = self.local_at(position);
        let (lat, lon) =
            projection::mercator_inverse(self.site_x + east_km, self.site_y + north_km)?;
        let (range_km, azimuth_deg) = projection::inverse(lat, lon, self.site_lat, self.site_lon)?;
        Ok(PointerInfo {
            lat,
            lon,
            range_km,
            azimuth_deg,
        })
    }

    fn screen_corner(&self, corner: GateCorner) -> Result<Pos2, CoordinateError> {
        let (lat, lon) = projection::forward(
            corner.range_km,
            corner.azimuth_deg,
            self.site_lat,
            self.site_lon,
        )?;
        self.screen_geo(lat, lon)
    }
}

impl HoverIndex {
    fn new(rect: Rect) -> Self {
        let cols = (rect.width() / HOVER_CELL_PX).ceil().max(1.0) as usize + 1;
        let rows = (rect.height() / HOVER_CELL_PX).ceil().max(1.0) as usize + 1;
        Self {
            origin: rect.min,
            cell: HOVER_CELL_PX,
            cols,
            rows,
            cells: vec![Vec::new(); cols * rows],
            entries: Vec::new(),
        }
    }

    fn insert(&mut self, points: [Pos2; 4], value: GateValue, bounds: Rect) {
        let (min_row, min_col, max_row, max_col) = self.cell_range(bounds);
        let entry_index = self.entries.len();
        self.entries.push(HoverEntry {
            points,
            value,
            bounds,
        });
        for row in min_row..=max_row {
            for col in min_col..=max_col {
                let Some(index) = row
                    .checked_mul(self.cols)
                    .and_then(|value| value.checked_add(col))
                else {
                    continue;
                };
                if let Some(cell) = self.cells.get_mut(index) {
                    cell.push(entry_index);
                }
            }
        }
    }

    fn cell_range(&self, bounds: Rect) -> (usize, usize, usize, usize) {
        let min_col = self.col_clamped(bounds.min.x);
        let max_col = self.col_clamped(bounds.max.x);
        let min_row = self.row_clamped(bounds.min.y);
        let max_row = self.row_clamped(bounds.max.y);
        (min_row, min_col, max_row, max_col)
    }

    fn col_clamped(&self, x: f32) -> usize {
        let value = ((x - self.origin.x) / self.cell).floor();
        value.clamp(0.0, (self.cols - 1) as f32) as usize
    }

    fn row_clamped(&self, y: f32) -> usize {
        let value = ((y - self.origin.y) / self.cell).floor();
        value.clamp(0.0, (self.rows - 1) as f32) as usize
    }

    fn col(&self, x: f32) -> Option<usize> {
        let value = ((x - self.origin.x) / self.cell).floor();
        if value < 0.0 {
            return None;
        }
        let index = value as usize;
        if index >= self.cols {
            return None;
        }
        Some(index)
    }

    fn row(&self, y: f32) -> Option<usize> {
        let value = ((y - self.origin.y) / self.cell).floor();
        if value < 0.0 {
            return None;
        }
        let index = value as usize;
        if index >= self.rows {
            return None;
        }
        Some(index)
    }

    pub fn find(&self, position: Pos2) -> Option<GateValue> {
        let col = self.col(position.x)?;
        let row = self.row(position.y)?;
        let cell_index = row.checked_mul(self.cols)?.checked_add(col)?;
        let cell = self.cells.get(cell_index)?;
        let mut best: Option<&HoverEntry> = None;
        for index in cell {
            let Some(entry) = self.entries.get(*index) else {
                continue;
            };
            if !entry.bounds.contains(position) {
                continue;
            }
            if !quad_contains(position, entry.points) {
                continue;
            }
            let better = best.is_none_or(|current| {
                entry.bounds.width() * entry.bounds.height()
                    < current.bounds.width() * current.bounds.height()
            });
            if better {
                best = Some(entry);
            }
        }
        best.map(|entry| entry.value)
    }
}

struct PreviousEdge {
    left: GateCorner,
    right: GateCorner,
    left_screen: Pos2,
    right_screen: Pos2,
}

const fn same_corner(a: GateCorner, b: GateCorner) -> bool {
    a.range_km.to_bits() == b.range_km.to_bits()
        && a.azimuth_deg.to_bits() == b.azimuth_deg.to_bits()
}

struct RadialPaths {
    left_azimuth: u64,
    right_azimuth: u64,
    left: projection::GeodesicPath,
    right: projection::GeodesicPath,
}

impl RadialPaths {
    fn new(
        transform: &ViewTransform,
        left: GateCorner,
        right: GateCorner,
    ) -> Result<Self, RenderError> {
        let path = |azimuth| {
            projection::GeodesicPath::new(transform.site_lat, transform.site_lon, azimuth)
                .map_err(|source| RenderError::Coordinate { source })
        };
        Ok(Self {
            left_azimuth: left.azimuth_deg.to_bits(),
            right_azimuth: right.azimuth_deg.to_bits(),
            left: path(left.azimuth_deg)?,
            right: path(right.azimuth_deg)?,
        })
    }

    const fn matches(&self, left: GateCorner, right: GateCorner) -> bool {
        self.left_azimuth == left.azimuth_deg.to_bits()
            && self.right_azimuth == right.azimuth_deg.to_bits()
    }
}

fn project_path(
    transform: &ViewTransform,
    path: &projection::GeodesicPath,
    range: f64,
) -> Result<Pos2, RenderError> {
    let (lat, lon) = path
        .position(range)
        .map_err(|source| RenderError::Coordinate { source })?;
    transform
        .screen_geo(lat, lon)
        .map_err(|source| RenderError::Coordinate { source })
}

fn screen_quad(
    bin: &RadarBin,
    transform: &ViewTransform,
    previous: &mut Option<PreviousEdge>,
    paths: &mut Option<RadialPaths>,
) -> Result<[Pos2; 4], RenderError> {
    let [near_left, near_right, far_right, far_left] = bin.corners;
    if paths
        .as_ref()
        .is_none_or(|paths| !paths.matches(near_left, near_right))
    {
        *paths = Some(RadialPaths::new(transform, near_left, near_right)?);
    }
    let Some(paths) = paths else {
        return Err(RenderError::MissingGeodesicPaths);
    };
    let (left, right) = match previous.as_ref() {
        Some(edge) if same_corner(edge.left, near_left) && same_corner(edge.right, near_right) => {
            (edge.left_screen, edge.right_screen)
        }
        Some(_) | None => (
            project_path(transform, &paths.left, near_left.range_km)?,
            project_path(transform, &paths.right, near_right.range_km)?,
        ),
    };
    let far_left_screen = project_path(transform, &paths.left, far_left.range_km)?;
    let far_right_screen = project_path(transform, &paths.right, far_right.range_km)?;
    *previous = Some(PreviousEdge {
        left: far_left,
        right: far_right,
        left_screen: far_left_screen,
        right_screen: far_right_screen,
    });
    Ok([left, right, far_right_screen, far_left_screen])
}

const fn quad_bounds(points: &[Pos2; 4]) -> Rect {
    let [p0, p1, p2, p3] = *points;
    let min_x = p0.x.min(p1.x).min(p2.x).min(p3.x);
    let max_x = p0.x.max(p1.x).max(p2.x).max(p3.x);
    let min_y = p0.y.min(p1.y).min(p2.y).min(p3.y);
    let max_y = p0.y.max(p1.y).max(p2.y).max(p3.y);
    Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y))
}

fn add_quad(mesh: &mut Mesh, points: &[Pos2; 4], color: Color32) -> Result<(), RenderError> {
    let vertices = mesh.vertices.len();
    let start = u32::try_from(vertices).map_err(|_| RenderError::MeshSize { vertices })?;
    if start.checked_add(3).is_none() {
        return Err(RenderError::MeshSize { vertices });
    }
    for point in points {
        mesh.colored_vertex(*point, color);
    }
    mesh.add_triangle(start, start + 1, start + 2);
    mesh.add_triangle(start, start + 2, start + 3);
    Ok(())
}

fn bin_color(value: GateValue) -> Result<Color32, RenderError> {
    let rgba = match value {
        GateValue::Measured(dbz) => color_for_reflectivity(dbz),
        GateValue::BelowThreshold => color_for_reflectivity(crate::colormap::MIN_REFLECTIVITY_DBZ),
        GateValue::RangeFolded => Ok(range_folded_color()),
    }
    .map_err(|source| RenderError::Color { source })?;
    Ok(Color32::from_rgba_unmultiplied(
        rgba[0],
        rgba[1],
        rgba[2],
        RADAR_ALPHA,
    ))
}

fn quad_contains(position: Pos2, points: [Pos2; 4]) -> bool {
    let [p0, p1, p2, p3] = points;
    point_in_triangle(position, p0, p1, p2) || point_in_triangle(position, p0, p2, p3)
}

fn point_in_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    if edge(a, b, c).abs() <= f32::MIN_POSITIVE {
        return false;
    }
    let d1 = edge(p, a, b);
    let d2 = edge(p, b, c);
    let d3 = edge(p, c, a);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

fn edge(p1: Pos2, p2: Pos2, p3: Pos2) -> f32 {
    (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Pos2, Rect, Vec2};

    #[derive(Debug)]
    enum TestFailure {
        Coordinate(CoordinateError),
        Render(RenderError),
        Value {
            label: &'static str,
            expected: f64,
            actual: f64,
            tolerance: f64,
        },
        Missing {
            label: &'static str,
            expected: &'static str,
        },
        Count {
            label: &'static str,
            expected: usize,
            actual: usize,
        },
    }

    impl From<CoordinateError> for TestFailure {
        fn from(source: CoordinateError) -> Self {
            Self::Coordinate(source)
        }
    }

    impl From<RenderError> for TestFailure {
        fn from(source: RenderError) -> Self {
            Self::Render(source)
        }
    }

    impl std::fmt::Display for TestFailure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Coordinate(error) => write!(formatter, "{error}"),
                Self::Render(error) => write!(formatter, "{error}"),
                Self::Value {
                    label,
                    expected,
                    actual,
                    tolerance,
                } => write!(
                    formatter,
                    "{label}: expected {expected}, got {actual}, tolerance {tolerance}"
                ),
                Self::Missing { label, expected } => {
                    write!(formatter, "{label}: expected {expected}")
                }
                Self::Count {
                    label,
                    expected,
                    actual,
                } => write!(formatter, "{label}: expected {expected}, got {actual}"),
            }
        }
    }

    /// Screen coordinates are `f32`, so a position round trip through
    /// [`ViewTransform::screen_local`] and [`ViewTransform::local_at`] resolves to
    /// about one `f32` unit at 800 px, roughly 0.1 m at this scale.
    const SCREEN_ROUND_TRIP_KM: f64 = 1e-4;

    fn expect_within(
        label: &'static str,
        expected: f64,
        actual: f64,
        tolerance: f64,
    ) -> Result<(), TestFailure> {
        if (actual - expected).abs() <= tolerance {
            Ok(())
        } else {
            Err(TestFailure::Value {
                label,
                expected,
                actual,
                tolerance,
            })
        }
    }

    fn empty_volume() -> RadarVolume {
        RadarVolume {
            site_lat: 37.0,
            site_lon: -88.0,
            bins: Vec::new(),
            max_range_km: 100.0,
            scan_unix_ms: 0,
            below_threshold_bins: 0,
            range_folded_bins: 0,
            skipped_radials: 0,
            skipped_non_data: 0,
            moment_error: None,
        }
    }

    #[test]
    fn screen_and_local_round_trip() -> Result<(), TestFailure> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 800.0));
        let transform = ViewTransform::new(rect, &empty_volume())?;
        let point = transform.screen_local(50.0, -25.0);
        let (east_km, north_km) = transform.local_at(point);
        expect_within("east km", 50.0, east_km, SCREEN_ROUND_TRIP_KM)?;
        expect_within("north km", -25.0, north_km, SCREEN_ROUND_TRIP_KM)?;
        Ok(())
    }

    #[test]
    fn viewport_resize_preserves_gate_location_and_pointer_mapping() -> Result<(), TestFailure> {
        let canonical = ViewTransform::new(canonical_rect(), &empty_volume())?;
        let viewport = radar_viewport(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0)));
        let resized = canonical.for_viewport(viewport);
        let (lat, lon) = projection::forward(50.0, 90.0, 37.0, -88.0)?;
        let point = resized.screen_geo(lat, lon)?;
        expect_within("screen x", 1016.0, f64::from(point.x), 1.0)?;
        expect_within("screen y", 450.0, f64::from(point.y), 1.0)?;
        let info = resized.pointer_info(point)?;
        expect_within("pointer range km", 50.0, info.range_km, 0.001)?;
        expect_within("pointer azimuth deg", 90.0, info.azimuth_deg, 0.001)?;
        Ok(())
    }

    #[test]
    fn zero_range_is_rejected_instead_of_fabricating_a_view() {
        let mut volume = empty_volume();
        volume.max_range_km = 0.0;
        assert!(
            matches!(ViewTransform::new(canonical_rect(), &volume), Err(CoordinateError::ViewRange { range_km }) if range_km.abs() < f64::EPSILON)
        );
    }

    #[test]
    fn gate_corner_uses_wgs84_geodesic_on_map() -> Result<(), TestFailure> {
        let mut volume = empty_volume();
        volume.site_lat = 37.06833;
        volume.site_lon = -88.77194;
        let transform = ViewTransform::new(canonical_rect(), &volume)?;
        let corner = GateCorner {
            range_km: 75.0,
            azimuth_deg: 45.0,
        };
        let actual = transform.screen_corner(corner)?;
        let expected = transform.screen_geo(37.544662, -88.171837)?;
        expect_within(
            "gate corner x",
            f64::from(expected.x),
            f64::from(actual.x),
            0.001,
        )?;
        expect_within(
            "gate corner y",
            f64::from(expected.y),
            f64::from(actual.y),
            0.001,
        )?;
        let pointer = transform.pointer_info(actual)?;
        expect_within("gate corner range km", 75.0, pointer.range_km, 0.001)?;
        expect_within("gate corner azimuth deg", 45.0, pointer.azimuth_deg, 0.001)?;
        Ok(())
    }

    #[test]
    fn render_reports_unmapped_color_instead_of_dropping_gate() {
        let mut volume = empty_volume();
        volume.bins.push(RadarBin {
            corners: [
                GateCorner {
                    range_km: 9.0,
                    azimuth_deg: 0.0,
                },
                GateCorner {
                    range_km: 9.0,
                    azimuth_deg: 1.0,
                },
                GateCorner {
                    range_km: 10.0,
                    azimuth_deg: 1.0,
                },
                GateCorner {
                    range_km: 10.0,
                    azimuth_deg: 0.0,
                },
            ],
            value: GateValue::Measured(95.0),
        });
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 800.0));
        let result = build_render(&volume, rect);
        assert!(
            matches!(result, Err(RenderError::Color { source: crate::error::ColorMapError::OutOfRange { value, .. } }) if value == 95.0)
        );
    }

    #[test]
    fn hover_returns_the_value_at_a_rendered_gate() -> Result<(), TestFailure> {
        let mut volume = empty_volume();
        volume.bins.push(RadarBin {
            corners: [
                GateCorner {
                    range_km: 49.0,
                    azimuth_deg: 44.0,
                },
                GateCorner {
                    range_km: 49.0,
                    azimuth_deg: 46.0,
                },
                GateCorner {
                    range_km: 51.0,
                    azimuth_deg: 46.0,
                },
                GateCorner {
                    range_km: 51.0,
                    azimuth_deg: 44.0,
                },
            ],
            value: GateValue::Measured(35.0),
        });
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 800.0));
        let cache = build_render(&volume, rect)?;
        let (lat, lon) = projection::forward(50.0, 45.0, 37.0, -88.0)?;
        let position = cache.transform.screen_geo(lat, lon)?;
        match cache.hover.find(position) {
            Some(GateValue::Measured(value)) => {
                expect_within("hover value", 35.0, f64::from(value), f64::EPSILON)?;
            }
            _ => {
                return Err(TestFailure::Missing {
                    label: "hover value inside the gate",
                    expected: "a measured gate value",
                });
            }
        }
        let (outside_lat, outside_lon) = projection::forward(60.0, 45.0, 37.0, -88.0)?;
        let outside = cache.transform.screen_geo(outside_lat, outside_lon)?;
        if cache.hover.find(outside).is_some() {
            return Err(TestFailure::Missing {
                label: "hover outside every gate",
                expected: "no gate value",
            });
        }
        Ok(())
    }

    #[test]
    fn below_threshold_is_not_drawn_but_remains_identifiable() -> Result<(), TestFailure> {
        let mut volume = empty_volume();
        volume.bins.push(RadarBin {
            corners: [
                GateCorner {
                    range_km: 49.0,
                    azimuth_deg: 44.0,
                },
                GateCorner {
                    range_km: 49.0,
                    azimuth_deg: 46.0,
                },
                GateCorner {
                    range_km: 51.0,
                    azimuth_deg: 46.0,
                },
                GateCorner {
                    range_km: 51.0,
                    azimuth_deg: 44.0,
                },
            ],
            value: GateValue::BelowThreshold,
        });
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 800.0));
        let cache = build_render(&volume, rect)?;
        let (lat, lon) = projection::forward(50.0, 45.0, 37.0, -88.0)?;
        let point = cache.transform.screen_geo(lat, lon)?;
        match cache.hover.find(point) {
            Some(GateValue::BelowThreshold) => {}
            _ => {
                return Err(TestFailure::Missing {
                    label: "below-threshold gate",
                    expected: "below-threshold in the hover index",
                });
            }
        }
        if !cache.vertices.is_empty() {
            return Err(TestFailure::Count {
                label: "drawn vertices for a below-threshold gate",
                expected: 0,
                actual: cache.vertices.len(),
            });
        }
        Ok(())
    }

    #[test]
    fn degenerate_triangle_does_not_claim_outside_points() {
        let a = Pos2::new(0.0, 0.0);
        let b = Pos2::new(10.0, 10.0);
        assert!(
            !point_in_triangle(Pos2::new(5.0, 6.0), a, a, b),
            "a collapsed triangle must not contain an outside point"
        );
    }

    #[test]
    fn quad_contains_center_and_rejects_outside() {
        let points = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        assert!(
            quad_contains(Pos2::new(5.0, 5.0), points),
            "center must be inside"
        );
        assert!(
            !quad_contains(Pos2::new(15.0, 5.0), points),
            "outside point must be rejected"
        );
    }
}
