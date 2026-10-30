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
//! Radar gate geometry, overlay geometry, and the screen transform.
//!
//! Every gate corner is placed with the WGS84 geodesic direct solution from the
//! site after its slant range is converted to ground range, then projected to
//! Web Mercator. Vertices carry kilometre offsets from the site, which the GPU
//! scales into the viewport, so one mesh serves every window size. The pointer
//! lookup inverts the same model.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{Color32, Pos2, Rect, Vec2};

use crate::colormap::{color_for_reflectivity, range_folded_color};
use crate::error::{CoordinateError, RenderError};
use crate::projection;
use crate::radar::{GateValue, RadarVolume, RadialBins};

const RADAR_ALPHA: u8 = 255;
const VERTEX_BYTES: usize = 12;
/// Half-width of the radar disc as a fraction of the viewport side.
const VIEW_MARGIN: f64 = 0.48;
const RING_STEP_KM: f64 = 50.0;
const RING_SAMPLES: u32 = 128;
const GRID_SAMPLES: u32 = 64;
const GRID_DEGREES_EACH_SIDE: i32 = 3;

pub struct RangeRing {
    pub range_km: f64,
    pub points: Vec<Pos2>,
}

/// The gate under a map point.
#[derive(Debug, Clone, Copy)]
pub struct GateHit {
    pub value: GateValue,
    pub elevation_deg: f64,
}

struct RadialLookup {
    left: f64,
    span: f64,
    elevation_deg: f64,
    first_gate_km: f64,
    gate_width_km: f64,
    gate_count: usize,
}

impl RadialLookup {
    fn gate_at(&self, range_km: f64) -> Option<usize> {
        let mut low = 0_usize;
        let mut high = self.gate_count;
        while low < high {
            let middle = low + (high - low) / 2;
            let centre = self.first_gate_km + middle as f64 * self.gate_width_km;
            let near =
                projection::ground_range(centre - self.gate_width_km / 2.0, self.elevation_deg)
                    .ok()?;
            if near <= range_km {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let gate = low.checked_sub(1)?;
        let centre = self.first_gate_km + gate as f64 * self.gate_width_km;
        let far =
            projection::ground_range(centre + self.gate_width_km / 2.0, self.elevation_deg).ok()?;
        if range_km < far { Some(gate) } else { None }
    }
}

/// Azimuth and gate lookup over the sweep that was drawn.
pub struct SweepLookup {
    radials: Vec<RadialLookup>,
}

impl SweepLookup {
    fn new(volume: &RadarVolume) -> Self {
        let mut radials: Vec<RadialLookup> = Vec::with_capacity(volume.sweep.radials.len());
        let mut previous: Option<f64> = None;
        for radial in &volume.sweep.radials {
            let span = (radial.azimuth_right_deg - radial.azimuth_left_deg).rem_euclid(360.0);
            let left = previous.map_or(radial.azimuth_left_deg, |value| {
                value + (radial.azimuth_left_deg - value).rem_euclid(360.0)
            });
            radials.push(RadialLookup {
                left,
                span,
                elevation_deg: radial.elevation_deg,
                first_gate_km: radial.first_gate_km,
                gate_width_km: radial.gate_width_km,
                gate_count: radial.values.len(),
            });
            previous = Some(left);
        }
        Self { radials }
    }

    fn radial_at(&self, azimuth_deg: f64) -> Option<usize> {
        for turn in [-360.0, 0.0, 360.0] {
            let target = azimuth_deg + turn;
            let after = self.radials.partition_point(|radial| radial.left <= target);
            if let Some(index) = after.checked_sub(1)
                && let Some(radial) = self.radials.get(index)
                && target < radial.left + radial.span
            {
                return Some(index);
            }
        }
        None
    }

    /// The gate whose drawn quad covers this ground range and azimuth.
    pub fn gate(&self, volume: &RadarVolume, range_km: f64, azimuth_deg: f64) -> Option<GateHit> {
        let index = self.radial_at(azimuth_deg)?;
        let lookup = self.radials.get(index)?;
        let gate = lookup.gate_at(range_km)?;
        let radial = volume.sweep.radials.get(index)?;
        let value = radial.values.get(gate).copied()?;
        Some(GateHit {
            value,
            elevation_deg: radial.elevation_deg,
        })
    }
}

/// Everything a frame draws, built once per volume off the UI thread.
pub struct RadarGeometry {
    pub extent_km: f64,
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub index_count: u32,
    pub rings: Vec<RangeRing>,
    pub grid: Vec<Vec<Pos2>>,
    pub lookup: SweepLookup,
    pub site_lat: f64,
    pub site_lon: f64,
    pub site_x: f64,
    pub site_y: f64,
}

pub struct BuildRequest {
    pub volume: RadarVolume,
    pub generation: u64,
}

pub struct BuildResult {
    pub volume: RadarVolume,
    pub geometry: Result<RadarGeometry, RenderError>,
    pub generation: u64,
}

/// Runs geometry construction on its own thread; a panic is reported, not fatal.
pub fn start_build_thread() -> (Sender<BuildRequest>, Receiver<BuildResult>) {
    let (request_sender, request_receiver) = mpsc::channel::<BuildRequest>();
    let (result_sender, result_receiver) = mpsc::channel::<BuildResult>();
    std::thread::spawn(move || {
        while let Ok(request) = request_receiver.recv() {
            let geometry = catch_unwind(AssertUnwindSafe(|| build_geometry(&request.volume)))
                .map_err(|_| RenderError::Panic)
                .and_then(std::convert::identity);
            let result = BuildResult {
                volume: request.volume,
                geometry,
                generation: request.generation,
            };
            if result_sender.send(result).is_err() {
                return;
            }
        }
    });
    (request_sender, result_receiver)
}

fn place_geo(lat: f64, lon: f64, site_x: f64, site_y: f64) -> Result<Pos2, CoordinateError> {
    let (x, y) = projection::mercator(lat, lon)?;
    Ok(Pos2::new(
        projection::mercator_wrap_delta(x - site_x) as f32,
        (y - site_y) as f32,
    ))
}

fn place_corner(
    path: &projection::GeodesicPath,
    range_km: f64,
    site_x: f64,
    site_y: f64,
) -> Result<Pos2, RenderError> {
    let (lat, lon) = path
        .position(range_km)
        .map_err(|source| RenderError::Coordinate { source })?;
    place_geo(lat, lon, site_x, site_y).map_err(|source| RenderError::Coordinate { source })
}

fn bin_color(value: GateValue) -> Result<Option<Color32>, RenderError> {
    let rgba = match value {
        GateValue::Measured(dbz) => color_for_reflectivity(dbz),
        GateValue::BelowThreshold => return Ok(None),
        GateValue::RangeFolded => Ok(range_folded_color()),
    }
    .map_err(|source| RenderError::Color { source })?;
    Ok(Some(Color32::from_rgba_unmultiplied(
        rgba[0],
        rgba[1],
        rgba[2],
        RADAR_ALPHA,
    )))
}

fn push_quad(
    vertices: &mut Vec<u8>,
    indices: &mut Vec<u8>,
    corners: [Pos2; 4],
    color: Color32,
) -> Result<(), RenderError> {
    let count = vertices.len() / VERTEX_BYTES;
    let base = u32::try_from(count).map_err(|_| RenderError::MeshSize { vertices: count })?;
    if base.checked_add(4).is_none() {
        return Err(RenderError::MeshSize { vertices: count });
    }
    let channels = color.to_array();
    for corner in corners {
        vertices.extend_from_slice(&corner.x.to_le_bytes());
        vertices.extend_from_slice(&corner.y.to_le_bytes());
        vertices.extend_from_slice(&channels);
    }
    for index in [base, base + 1, base + 2, base, base + 2, base + 3] {
        indices.extend_from_slice(&index.to_le_bytes());
    }
    Ok(())
}

struct BuildState {
    vertices: Vec<u8>,
    indices: Vec<u8>,
    extent_km: f64,
    site_x: f64,
    site_y: f64,
    near: [Pos2; 2],
    near_slant: Option<f64>,
}

impl BuildState {
    fn corners_at(
        &mut self,
        slant_km: f64,
        elevation_deg: f64,
        left: &projection::GeodesicPath,
        right: &projection::GeodesicPath,
    ) -> Result<[Pos2; 2], RenderError> {
        let ground = projection::ground_range(slant_km, elevation_deg)
            .map_err(|source| RenderError::Coordinate { source })?;
        let left_corner = place_corner(left, ground, self.site_x, self.site_y)?;
        let right_corner = place_corner(right, ground, self.site_x, self.site_y)?;
        self.extent_km = self
            .extent_km
            .max(f64::from(left_corner.x.hypot(left_corner.y)))
            .max(f64::from(right_corner.x.hypot(right_corner.y)));
        Ok([left_corner, right_corner])
    }
}

fn radial_quads(
    radial: &RadialBins,
    left_path: &projection::GeodesicPath,
    right_path: &projection::GeodesicPath,
    build: &mut BuildState,
) -> Result<(), RenderError> {
    let width = radial.gate_width_km;
    for (gate, value) in radial.values.iter().enumerate() {
        let centre = radial.first_gate_km + gate as f64 * width;
        let near_slant = centre - width / 2.0;
        let far_slant = centre + width / 2.0;
        let colour = bin_color(*value)?;
        if build.near_slant != Some(near_slant) {
            build.near =
                build.corners_at(near_slant, radial.elevation_deg, left_path, right_path)?;
        }
        let far = build.corners_at(far_slant, radial.elevation_deg, left_path, right_path)?;
        if let Some(colour) = colour {
            push_quad(
                &mut build.vertices,
                &mut build.indices,
                [build.near[0], build.near[1], far[1], far[0]],
                colour,
            )?;
        }
        build.near = far;
        build.near_slant = Some(far_slant);
    }
    Ok(())
}

fn build_rings(
    volume: &RadarVolume,
    site_x: f64,
    site_y: f64,
) -> Result<Vec<RangeRing>, RenderError> {
    let mut rings = Vec::new();
    let mut range_km = RING_STEP_KM;
    while range_km <= volume.sweep.max_ground_range_km {
        let mut points = Vec::with_capacity(RING_SAMPLES as usize + 1);
        for step in 0..=RING_SAMPLES {
            let azimuth_deg = f64::from(step) * 360.0 / f64::from(RING_SAMPLES);
            let (lat, lon) =
                projection::forward(range_km, azimuth_deg, volume.site_lat, volume.site_lon)
                    .map_err(|source| RenderError::Coordinate { source })?;
            points.push(
                place_geo(lat, lon, site_x, site_y)
                    .map_err(|source| RenderError::Coordinate { source })?,
            );
        }
        rings.push(RangeRing { range_km, points });
        range_km += RING_STEP_KM;
    }
    Ok(rings)
}

fn grid_line(
    start: (f64, f64),
    step: (f64, f64),
    site_x: f64,
    site_y: f64,
) -> Result<Vec<Pos2>, RenderError> {
    let mut points = Vec::with_capacity(GRID_SAMPLES as usize + 1);
    for sample in 0..=GRID_SAMPLES {
        let fraction = f64::from(sample) / f64::from(GRID_SAMPLES);
        let lat = start.0 + step.0 * fraction;
        let lon = start.1 + step.1 * fraction;
        if (-85.051_128_779_806_6..=85.051_128_779_806_6).contains(&lat) {
            points.push(
                place_geo(lat, lon, site_x, site_y)
                    .map_err(|source| RenderError::Coordinate { source })?,
            );
        }
    }
    Ok(points)
}

fn build_grid(
    volume: &RadarVolume,
    site_x: f64,
    site_y: f64,
) -> Result<Vec<Vec<Pos2>>, RenderError> {
    let mut lines = Vec::new();
    let span = f64::from(GRID_DEGREES_EACH_SIDE) + 0.5;
    let first_latitude = volume.site_lat.floor() as i32 - GRID_DEGREES_EACH_SIDE;
    let last_latitude = volume.site_lat.ceil() as i32 + GRID_DEGREES_EACH_SIDE;
    let first_longitude = volume.site_lon.floor() as i32 - GRID_DEGREES_EACH_SIDE;
    let last_longitude = volume.site_lon.ceil() as i32 + GRID_DEGREES_EACH_SIDE;
    for latitude in first_latitude..=last_latitude {
        lines.push(grid_line(
            (f64::from(latitude), volume.site_lon - span),
            (0.0, 2.0 * span),
            site_x,
            site_y,
        )?);
    }
    for longitude in first_longitude..=last_longitude {
        lines.push(grid_line(
            (volume.site_lat - span, f64::from(longitude)),
            (2.0 * span, 0.0),
            site_x,
            site_y,
        )?);
    }
    Ok(lines)
}

/// Builds the mesh, overlays, and pointer lookup for one volume.
pub fn build_geometry(volume: &RadarVolume) -> Result<RadarGeometry, RenderError> {
    let (site_x, site_y) = projection::mercator(volume.site_lat, volume.site_lon)
        .map_err(|source| RenderError::Coordinate { source })?;
    let mut build = BuildState {
        vertices: Vec::new(),
        indices: Vec::new(),
        extent_km: 0.0,
        site_x,
        site_y,
        near: [Pos2::ZERO; 2],
        near_slant: None,
    };
    for radial in &volume.sweep.radials {
        let left_path = projection::GeodesicPath::new(
            volume.site_lat,
            volume.site_lon,
            radial.azimuth_left_deg,
        )
        .map_err(|source| RenderError::Coordinate { source })?;
        let right_path = projection::GeodesicPath::new(
            volume.site_lat,
            volume.site_lon,
            radial.azimuth_right_deg,
        )
        .map_err(|source| RenderError::Coordinate { source })?;
        build.near_slant = None;
        radial_quads(radial, &left_path, &right_path, &mut build)?;
    }
    let index_count =
        u32::try_from(build.indices.len() / 4).map_err(|_| RenderError::MeshSize {
            vertices: build.vertices.len() / VERTEX_BYTES,
        })?;
    Ok(RadarGeometry {
        extent_km: build.extent_km,
        vertices: build.vertices,
        indices: build.indices,
        index_count,
        rings: build_rings(volume, site_x, site_y)?,
        grid: build_grid(volume, site_x, site_y)?,
        lookup: SweepLookup::new(volume),
        site_lat: volume.site_lat,
        site_lon: volume.site_lon,
        site_x,
        site_y,
    })
}

/// Clip-space units per kilometre of offset, shared by the mesh shader and the
/// screen transform so both place a gate in the same spot.
pub fn projection_scale(extent_km: f64) -> Result<f64, CoordinateError> {
    if !(extent_km.is_finite() && extent_km > 0.0) {
        return Err(CoordinateError::ViewExtent { extent_km });
    }
    Ok(2.0 * VIEW_MARGIN / extent_km)
}

/// The square of the window the radar disc is drawn into.
pub fn radar_viewport(rect: Rect) -> Rect {
    Rect::from_center_size(rect.center(), Vec2::splat(rect.width().min(rect.height())))
}

#[derive(Debug, Clone, Copy)]
pub struct PointerInfo {
    pub lat: f64,
    pub lon: f64,
    pub range_km: f64,
    pub azimuth_deg: f64,
}

/// Screen placement for one geometry at the current viewport size.
#[derive(Debug, Clone, Copy)]
pub struct ViewTransform {
    center: Pos2,
    pixels_per_km: f64,
    site_lat: f64,
    site_lon: f64,
    site_x: f64,
    site_y: f64,
}

impl ViewTransform {
    pub fn new(viewport: Rect, geometry: &RadarGeometry) -> Result<Self, CoordinateError> {
        let side = f64::from(viewport.width().min(viewport.height()));
        if !(side.is_finite() && side > 0.0) {
            return Err(CoordinateError::Viewport {
                width: viewport.width(),
                height: viewport.height(),
            });
        }
        let scale = projection_scale(geometry.extent_km)?;
        Ok(Self {
            center: viewport.center(),
            pixels_per_km: 0.5 * side * scale,
            site_lat: geometry.site_lat,
            site_lon: geometry.site_lon,
            site_x: geometry.site_x,
            site_y: geometry.site_y,
        })
    }

    /// Places a site-relative kilometre offset in the window.
    pub fn screen(&self, offset: Pos2) -> Pos2 {
        self.center
            + Vec2::new(
                (f64::from(offset.x) * self.pixels_per_km) as f32,
                (-f64::from(offset.y) * self.pixels_per_km) as f32,
            )
    }

    /// The site-relative kilometre offset under a window position.
    pub fn offset_at(&self, position: Pos2) -> Pos2 {
        Pos2::new(
            (f64::from(position.x - self.center.x) / self.pixels_per_km) as f32,
            (-f64::from(position.y - self.center.y) / self.pixels_per_km) as f32,
        )
    }

    pub fn pointer_info(&self, position: Pos2) -> Result<PointerInfo, CoordinateError> {
        let offset = self.offset_at(position);
        let (lat, lon) = projection::mercator_inverse(
            self.site_x + f64::from(offset.x),
            self.site_y + f64::from(offset.y),
        )?;
        let (range_km, azimuth_deg) = projection::inverse(lat, lon, self.site_lat, self.site_lon)?;
        Ok(PointerInfo {
            lat,
            lon,
            range_km,
            azimuth_deg,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ColorMapError;
    use crate::radar::RadarSweep;
    use crate::stations::SiteId;

    const SITE_LAT: f64 = 37.068_33;
    const SITE_LON: f64 = -88.771_94;

    fn site() -> Result<SiteId, String> {
        SiteId::parse("KPAH").map_err(|error| error.to_string())
    }

    fn radial(left: f64, right: f64, elevation_deg: f64, values: Vec<GateValue>) -> RadialBins {
        RadialBins {
            azimuth_number: 1,
            azimuth_left_deg: left,
            azimuth_right_deg: right,
            elevation_deg,
            first_gate_km: 74.75,
            gate_width_km: 0.5,
            values,
        }
    }

    fn volume(radials: Vec<RadialBins>, max_ground_range_km: f64) -> Result<RadarVolume, String> {
        Ok(RadarVolume {
            site: site()?,
            site_lat: SITE_LAT,
            site_lon: SITE_LON,
            scan_unix_ms: 1_793_327_729_000,
            sweep: RadarSweep {
                elevation_deg: 0.0,
                elevation_number: 1,
                radials,
                below_threshold: 0,
                range_folded: 0,
                max_ground_range_km,
            },
            skipped_radials: 0,
            skipped_non_data: 0,
            moment_error: None,
        })
    }

    fn build(
        radials: Vec<RadialBins>,
        max_ground_range_km: f64,
    ) -> Result<(RadarVolume, RadarGeometry), String> {
        let volume = volume(radials, max_ground_range_km)?;
        let geometry = build_geometry(&volume).map_err(|error| error.to_string())?;
        Ok((volume, geometry))
    }

    fn vertex(geometry: &RadarGeometry, index: usize) -> Result<Pos2, String> {
        let start = index * VERTEX_BYTES;
        let x: [u8; 4] = geometry
            .vertices
            .get(start..start + 4)
            .ok_or("vertex x")?
            .try_into()
            .map_err(|_| String::from("vertex x"))?;
        let y: [u8; 4] = geometry
            .vertices
            .get(start + 4..start + 8)
            .ok_or("vertex y")?
            .try_into()
            .map_err(|_| String::from("vertex y"))?;
        Ok(Pos2::new(f32::from_le_bytes(x), f32::from_le_bytes(y)))
    }

    /// At zero elevation the 75 km slant range converts to 74.998 km of ground
    /// range, so the corner sits 2 m inside the roadmap's 75 km reference point;
    /// 5e-5 degrees is 4.4 m at this latitude.
    #[test]
    fn rendered_corner_lands_on_the_reference_geodesic_point() -> Result<(), String> {
        let (_, geometry) = build(
            vec![radial(90.0, 90.0, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        let corner = vertex(&geometry, 2)?;
        let (lat, lon) = projection::mercator_inverse(
            geometry.site_x + f64::from(corner.x),
            geometry.site_y + f64::from(corner.y),
        )
        .map_err(|error| error.to_string())?;
        assert!(
            (lat - 37.065_332).abs() < 5e-5,
            "rendered corner latitude {lat}"
        );
        assert!(
            (lon + 87.928_621).abs() < 5e-5,
            "rendered corner longitude {lon}"
        );
        Ok(())
    }

    #[test]
    fn lookup_returns_the_gate_under_a_reference_point() -> Result<(), String> {
        let (volume, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(41.0)])],
            75.0,
        )?;
        let ground = projection::ground_range(74.75, 0.0).map_err(|error| error.to_string())?;
        match geometry.lookup.gate(&volume, ground, 90.0) {
            Some(GateHit {
                value: GateValue::Measured(dbz),
                elevation_deg,
            }) => {
                assert!((dbz - 41.0).abs() < 1e-6, "gate value {dbz}");
                assert!(elevation_deg.abs() < 1e-9, "elevation {elevation_deg}");
                Ok(())
            }
            other => Err(format!("expected the measured gate, got {other:?}")),
        }
    }

    #[test]
    fn lookup_reports_no_data_outside_the_drawn_sweep() -> Result<(), String> {
        let (volume, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(41.0)])],
            75.0,
        )?;
        assert!(
            geometry.lookup.gate(&volume, 10.0, 90.0).is_none(),
            "a point nearer than the first gate reports no data"
        );
        assert!(
            geometry.lookup.gate(&volume, 75.0, 180.0).is_none(),
            "a point outside the drawn azimuth sector reports no data"
        );
        Ok(())
    }

    #[test]
    fn below_threshold_gates_are_identifiable_and_not_drawn() -> Result<(), String> {
        let (volume, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::BelowThreshold])],
            75.0,
        )?;
        assert_eq!(
            geometry.index_count, 0,
            "below-threshold gates are excluded from the mesh"
        );
        let ground = projection::ground_range(74.75, 0.0).map_err(|error| error.to_string())?;
        match geometry.lookup.gate(&volume, ground, 90.0) {
            Some(GateHit {
                value: GateValue::BelowThreshold,
                ..
            }) => Ok(()),
            other => Err(format!("expected below threshold, got {other:?}")),
        }
    }

    #[test]
    fn range_folded_gates_are_drawn_in_the_sentinel_colour() -> Result<(), String> {
        let (_, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::RangeFolded])],
            75.0,
        )?;
        let colour: [u8; 4] = geometry
            .vertices
            .get(8..12)
            .ok_or("vertex colour")?
            .try_into()
            .map_err(|_| String::from("vertex colour"))?;
        assert_eq!(colour, range_folded_color(), "range folded colour");
        Ok(())
    }

    #[test]
    fn an_unmappable_reflectivity_value_is_an_error() -> Result<(), String> {
        let volume = volume(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(95.0)])],
            75.0,
        )?;
        match build_geometry(&volume) {
            Err(RenderError::Color {
                source: ColorMapError::OutOfRange { value, .. },
            }) => {
                assert!((value - 95.0).abs() < f32::EPSILON, "carried value {value}");
                Ok(())
            }
            Err(error) => Err(format!("expected a colour error, got {error}")),
            Ok(_) => Err(String::from("expected a colour error, got a geometry")),
        }
    }

    #[test]
    fn viewport_transform_maps_offsets_and_pointer_positions() -> Result<(), String> {
        let (_, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        let viewport = radar_viewport(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0)));
        let transform =
            ViewTransform::new(viewport, &geometry).map_err(|error| error.to_string())?;
        let offset = Pos2::new(40.0, -25.0);
        let screen = transform.screen(offset);
        let recovered = transform.offset_at(screen);
        assert!(
            (recovered.x - offset.x).abs() < 1e-2 && (recovered.y - offset.y).abs() < 1e-2,
            "offset round trip {recovered:?}"
        );
        let info = transform
            .pointer_info(screen)
            .map_err(|error| error.to_string())?;
        let (expected_lat, expected_lon) =
            projection::mercator_inverse(geometry.site_x + 40.0, geometry.site_y - 25.0)
                .map_err(|error| error.to_string())?;
        assert!(
            (info.lat - expected_lat).abs() < 1e-6,
            "latitude {}",
            info.lat
        );
        assert!(
            (info.lon - expected_lon).abs() < 1e-6,
            "longitude {}",
            info.lon
        );
        Ok(())
    }

    #[test]
    fn a_viewport_without_pixels_is_rejected() -> Result<(), String> {
        let (_, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        let empty = Rect::from_min_size(Pos2::ZERO, Vec2::ZERO);
        match ViewTransform::new(empty, &geometry) {
            Err(CoordinateError::Viewport { width, height }) => {
                assert!(width.abs() < f32::EPSILON && height.abs() < f32::EPSILON);
                Ok(())
            }
            other => Err(format!("expected a viewport error, got {other:?}")),
        }
    }

    #[test]
    fn range_rings_are_geodesic_circles_at_fifty_kilometre_intervals() -> Result<(), String> {
        let (_, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        assert_eq!(geometry.rings.len(), 1, "one ring inside a 75 km extent");
        let ring = geometry.rings.first().ok_or("no ring")?;
        assert!((ring.range_km - 50.0).abs() < 1e-9, "ring radius");
        let first = ring.points.first().ok_or("no ring point")?;
        let (lat, lon) = projection::mercator_inverse(
            geometry.site_x + f64::from(first.x),
            geometry.site_y + f64::from(first.y),
        )
        .map_err(|error| error.to_string())?;
        let (range_km, _) =
            projection::inverse(lat, lon, SITE_LAT, SITE_LON).map_err(|error| error.to_string())?;
        assert!(
            (range_km - 50.0).abs() < 1e-3,
            "ring point range {range_km}"
        );
        Ok(())
    }
}
