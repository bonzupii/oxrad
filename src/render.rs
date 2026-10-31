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

use crate::colormap::{color_for_product, range_folded_color};
use crate::error::{CoordinateError, RenderError};
use crate::product::RadarProduct;
use crate::projection;
use crate::radar::{GateValue, RadarVolume, RadialBins, RetrievedVolume};

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
    fn gate_at(&self, range_km: f64) -> Result<Option<usize>, CoordinateError> {
        let mut low = 0_usize;
        let mut high = self.gate_count;
        while low < high {
            let middle = low + (high - low) / 2;
            let centre = self.first_gate_km + middle as f64 * self.gate_width_km;
            let near =
                projection::ground_range(centre - self.gate_width_km / 2.0, self.elevation_deg)?;
            if near <= range_km {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let Some(gate) = low.checked_sub(1) else {
            return Ok(None);
        };
        let centre = self.first_gate_km + gate as f64 * self.gate_width_km;
        let far = projection::ground_range(centre + self.gate_width_km / 2.0, self.elevation_deg)?;
        Ok((range_km < far).then_some(gate))
    }
}

/// Azimuth and gate lookup over the sweep that was drawn.
pub struct SweepLookup {
    radials: Vec<RadialLookup>,
    product: RadarProduct,
}

impl SweepLookup {
    fn new(volume: &RadarVolume, product: RadarProduct) -> Self {
        let mut radials: Vec<RadialLookup> = Vec::with_capacity(volume.sweep.radials.len());
        let mut previous: Option<f64> = None;
        for radial in &volume.sweep.radials {
            let span = (radial.azimuth_right_deg - radial.azimuth_left_deg).rem_euclid(360.0);
            let left = previous.map_or(radial.azimuth_left_deg, |value| {
                value + (radial.azimuth_left_deg - value).rem_euclid(360.0)
            });
            let moment = radial.moment(product);
            radials.push(RadialLookup {
                left,
                span,
                elevation_deg: radial.elevation_deg,
                first_gate_km: moment.map_or(radial.first_gate_km, |moment| moment.first_gate_km),
                gate_width_km: moment.map_or(radial.gate_width_km, |moment| moment.gate_width_km),
                gate_count: moment.map_or(0, |moment| moment.values.len()),
            });
            previous = Some(left);
        }
        Self { radials, product }
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
    pub fn gate(
        &self,
        volume: &RadarVolume,
        range_km: f64,
        azimuth_deg: f64,
    ) -> Result<Option<GateHit>, CoordinateError> {
        let Some(index) = self.radial_at(azimuth_deg) else {
            return Ok(None);
        };
        let Some(lookup) = self.radials.get(index) else {
            return Ok(None);
        };
        let Some(gate) = lookup.gate_at(range_km)? else {
            return Ok(None);
        };
        let Some(radial) = volume.sweep.radials.get(index) else {
            return Ok(None);
        };
        let Some(value) = radial
            .moment(self.product)
            .and_then(|moment| moment.values.get(gate))
            .copied()
        else {
            return Ok(None);
        };
        Ok(Some(GateHit {
            value,
            elevation_deg: radial.elevation_deg,
        }))
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
    pub product: RadarProduct,
    pub site_lat: f64,
    pub site_lon: f64,
    pub site_x: f64,
    pub site_y: f64,
}

pub struct BuildRequest {
    pub volume: RetrievedVolume,
    pub product: RadarProduct,
    pub generation: u64,
}

pub struct BuildResult {
    pub volume: RetrievedVolume,
    pub geometry: Result<RadarGeometry, RenderError>,
    pub generation: u64,
}

/// Runs geometry construction on its own thread; a panic is reported, not fatal.
pub fn start_build_thread() -> (Sender<BuildRequest>, Receiver<BuildResult>) {
    let (request_sender, request_receiver) = mpsc::channel::<BuildRequest>();
    let (result_sender, result_receiver) = mpsc::channel::<BuildResult>();
    std::thread::spawn(move || {
        while let Ok(request) = request_receiver.recv() {
            let geometry = catch_unwind(AssertUnwindSafe(|| {
                build_geometry_for(&request.volume.volume, request.product)
            }))
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

/// The site-relative kilometre offset of a place, in the mesh's frame.
pub fn place_offset(lat: f64, lon: f64, site_x: f64, site_y: f64) -> Result<Pos2, CoordinateError> {
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
    place_offset(lat, lon, site_x, site_y).map_err(|source| RenderError::Coordinate { source })
}

fn bin_color(product: RadarProduct, value: GateValue) -> Result<Option<Color32>, RenderError> {
    let rgba = match value {
        GateValue::Measured(value) => color_for_product(product, value),
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
    product: RadarProduct,
    left_path: &projection::GeodesicPath,
    right_path: &projection::GeodesicPath,
    build: &mut BuildState,
) -> Result<(), RenderError> {
    let Some(moment) = radial.moment(product) else {
        return Ok(());
    };
    let width = moment.gate_width_km;
    for (gate, value) in moment.values.iter().enumerate() {
        let centre = moment.first_gate_km + gate as f64 * width;
        let colour = bin_color(product, *value)?;
        let near_slant = centre - width / 2.0;
        let far_slant = centre + width / 2.0;
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
                place_offset(lat, lon, site_x, site_y)
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
                place_offset(lat, lon, site_x, site_y)
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
/// Builds the mesh for one product from the selected sweep.
pub fn build_geometry_for(
    volume: &RadarVolume,
    product: RadarProduct,
) -> Result<RadarGeometry, RenderError> {
    let (site_x, site_y) = projection::mercator(volume.site_lat, volume.site_lon)
        .map_err(|source| RenderError::Coordinate { source })?;
    let gate_capacity = volume
        .sweep
        .radials
        .iter()
        .filter_map(|radial| radial.moment(product))
        .map(|moment| moment.values.len())
        .sum::<usize>();
    let mut build = BuildState {
        vertices: Vec::with_capacity(gate_capacity.saturating_mul(4 * VERTEX_BYTES)),
        indices: Vec::with_capacity(gate_capacity.saturating_mul(6 * 4)),
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
        radial_quads(radial, product, &left_path, &right_path, &mut build)?;
    }
    let index_count =
        u32::try_from(build.indices.len() / 4).map_err(|_| RenderError::MeshSize {
            vertices: build.vertices.len() / VERTEX_BYTES,
        })?;
    let extent_km = if build.extent_km > 0.0 {
        build.extent_km
    } else {
        volume.sweep.max_ground_range_km.max(1.0)
    };
    Ok(RadarGeometry {
        extent_km,
        vertices: build.vertices,
        indices: build.indices,
        index_count,
        rings: build_rings(volume, site_x, site_y)?,
        grid: build_grid(volume, site_x, site_y)?,
        lookup: SweepLookup::new(volume, product),
        product,
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
        let (range_km, azimuth_deg) = projection::inverse(self.site_lat, self.site_lon, lat, lon)?;
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
    use crate::test_support::TestError;

    const SITE_LAT: f64 = 37.068_33;
    const SITE_LON: f64 = -88.771_94;

    fn site() -> Result<SiteId, TestError> {
        Ok(SiteId::parse("KPAH")?)
    }

    fn radial(left: f64, right: f64, elevation_deg: f64, values: Vec<GateValue>) -> RadialBins {
        RadialBins {
            azimuth_number: 1,
            azimuth_left_deg: left,
            azimuth_right_deg: right,
            elevation_deg,
            first_gate_km: 74.75,
            gate_width_km: 0.5,
            moments: vec![crate::radar::MomentBins {
                product: RadarProduct::Reflectivity,
                first_gate_km: 74.75,
                gate_width_km: 0.5,
                values,
                below_threshold: 0,
                range_folded: 0,
            }],
        }
    }

    fn volume(
        radials: Vec<RadialBins>,
        max_ground_range_km: f64,
    ) -> Result<RadarVolume, TestError> {
        Ok(RadarVolume {
            site: site()?,
            site_lat: SITE_LAT,
            site_lon: SITE_LON,
            scan_unix_ms: 1_793_327_729_000,
            sweep: RadarSweep {
                elevation_deg: 0.0,
                elevation_number: 1,
                radials,
                products: vec![RadarProduct::Reflectivity],
                below_threshold: 0,
                range_folded: 0,
                max_ground_range_km,
            },
            sweeps: Vec::new(),
            skipped_radials: 0,
            skipped_non_data: 0,
            moment_error: None,
        })
    }

    fn build(
        radials: Vec<RadialBins>,
        max_ground_range_km: f64,
    ) -> Result<(RadarVolume, RadarGeometry), TestError> {
        let volume = volume(radials, max_ground_range_km)?;
        let geometry = build_geometry_for(&volume, RadarProduct::Reflectivity)?;
        Ok((volume, geometry))
    }

    #[test]
    fn render_worker_returns_the_origin_with_the_matching_volume() -> Result<(), TestError> {
        let (sender, receiver) = start_build_thread();
        let retrieved = RetrievedVolume {
            volume: volume(
                vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(2.0)])],
                75.0,
            )?,
            origin: crate::radar::ArchiveOrigin::Cached,
        };
        sender
            .send(BuildRequest {
                volume: retrieved,
                product: RadarProduct::Reflectivity,
                generation: 42,
            })
            .map_err(|error| TestError::from(format!("render request failed: {error}")))?;
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|error| TestError::from(format!("render result failed: {error}")))?;
        assert_eq!(result.generation, 42);
        assert_eq!(result.volume.origin, crate::radar::ArchiveOrigin::Cached);
        assert_eq!(result.volume.volume.site.as_str(), "KPAH");
        assert!(result.geometry?.index_count > 0);
        Ok(())
    }

    fn vertex(geometry: &RadarGeometry, index: usize) -> Result<Pos2, TestError> {
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
    fn rendered_corner_lands_on_the_reference_geodesic_point() -> Result<(), TestError> {
        let (_, geometry) = build(
            vec![radial(90.0, 90.0, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        let corner = vertex(&geometry, 2)?;
        let (lat, lon) = projection::mercator_inverse(
            geometry.site_x + f64::from(corner.x),
            geometry.site_y + f64::from(corner.y),
        )?;
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
    fn lookup_returns_the_gate_under_a_reference_point() -> Result<(), TestError> {
        let (volume, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(41.0)])],
            75.0,
        )?;
        let ground = projection::ground_range(74.75, 0.0)?;
        let hit = geometry.lookup.gate(&volume, ground, 90.0)?;
        match hit {
            Some(GateHit {
                value: GateValue::Measured(dbz),
                elevation_deg,
            }) => {
                assert!((dbz - 41.0).abs() < 1e-6, "gate value {dbz}");
                assert!(elevation_deg.abs() < 1e-9, "elevation {elevation_deg}");
                Ok(())
            }
            other => Err(TestError::from(format!(
                "expected the measured gate, got {other:?}"
            ))),
        }
    }

    #[test]
    fn lookup_reports_no_data_outside_the_drawn_sweep() -> Result<(), TestError> {
        let (volume, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(41.0)])],
            75.0,
        )?;
        assert!(
            geometry.lookup.gate(&volume, 10.0, 90.0)?.is_none(),
            "a point nearer than the first gate reports no data"
        );
        assert!(
            geometry.lookup.gate(&volume, 75.0, 180.0)?.is_none(),
            "a point outside the drawn azimuth sector reports no data"
        );
        Ok(())
    }

    #[test]
    fn below_threshold_gates_are_identifiable_and_not_drawn() -> Result<(), TestError> {
        let (volume, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::BelowThreshold])],
            75.0,
        )?;
        assert_eq!(
            geometry.index_count, 0,
            "below-threshold gates are excluded from the mesh"
        );
        let ground = projection::ground_range(74.75, 0.0)?;
        let hit = geometry.lookup.gate(&volume, ground, 90.0)?;
        match hit {
            Some(GateHit {
                value: GateValue::BelowThreshold,
                ..
            }) => Ok(()),
            other => Err(TestError::from(format!(
                "expected below threshold, got {other:?}"
            ))),
        }
    }

    #[test]
    fn range_folded_gates_are_drawn_in_the_sentinel_colour() -> Result<(), TestError> {
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
    fn an_unmappable_reflectivity_value_is_an_error() -> Result<(), TestError> {
        let volume = volume(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(95.0)])],
            75.0,
        )?;
        match build_geometry_for(&volume, RadarProduct::Reflectivity) {
            Err(RenderError::Color {
                source: ColorMapError::OutOfRange { value, .. },
            }) => {
                assert!((value - 95.0).abs() < f32::EPSILON, "carried value {value}");
                Ok(())
            }
            Err(error) => Err(TestError::from(format!(
                "expected a colour error, got {error}"
            ))),
            Ok(_) => Err(TestError::from(String::from(
                "expected a colour error, got a geometry",
            ))),
        }
    }

    #[test]
    fn viewport_transform_maps_offsets_and_pointer_positions() -> Result<(), TestError> {
        let (_, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        let viewport = radar_viewport(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0)));
        let transform = ViewTransform::new(viewport, &geometry)?;
        let offset = Pos2::new(40.0, -25.0);
        let screen = transform.screen(offset);
        let recovered = transform.offset_at(screen);
        assert!(
            (recovered.x - offset.x).abs() < 1e-2 && (recovered.y - offset.y).abs() < 1e-2,
            "offset round trip {recovered:?}"
        );
        let info = transform.pointer_info(screen)?;
        let (expected_lat, expected_lon) =
            projection::mercator_inverse(geometry.site_x + 40.0, geometry.site_y - 25.0)?;
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
    fn a_viewport_without_pixels_is_rejected() -> Result<(), TestError> {
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
            other => Err(TestError::from(format!(
                "expected a viewport error, got {other:?}"
            ))),
        }
    }

    #[test]
    fn range_rings_are_geodesic_circles_at_fifty_kilometre_intervals() -> Result<(), TestError> {
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
        )?;
        let (range_km, _) = projection::inverse(SITE_LAT, SITE_LON, lat, lon)?;
        assert!(
            (range_km - 50.0).abs() < 1e-3,
            "ring point range {range_km}"
        );
        Ok(())
    }

    /// North is positive Mercator y and must draw above the site; east is
    /// positive Mercator x and must draw to the right. Both expectations come
    /// from the map, not from the transform under test.
    #[test]
    fn north_and_east_plot_on_the_expected_sides_of_the_site() -> Result<(), TestError> {
        let (_, geometry) = build(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        let viewport = radar_viewport(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0)));
        let transform = ViewTransform::new(viewport, &geometry)?;
        let (north_lat, north_lon) = projection::forward(75.0, 0.0, SITE_LAT, SITE_LON)?;
        let north = place_offset(north_lat, north_lon, geometry.site_x, geometry.site_y)?;
        assert!(north.y > 0.0, "north offset y {}", north.y);
        assert!(
            transform.screen(north).y < viewport.center().y,
            "north draws below the site"
        );
        let (east_lat, east_lon) = projection::forward(75.0, 90.0, SITE_LAT, SITE_LON)?;
        let east = place_offset(east_lat, east_lon, geometry.site_x, geometry.site_y)?;
        assert!(east.x > 0.0, "east offset x {}", east.x);
        assert!(
            transform.screen(east).x > viewport.center().x,
            "east draws left of the site"
        );
        Ok(())
    }

    /// Bowling Green is east and north of KHPX; Paducah is east and north of
    /// KPAH. Both are independent geographic facts about the station catalog.
    #[test]
    fn a_place_marker_sits_on_the_expected_bearing_from_the_station() -> Result<(), TestError> {
        let cases = [
            (
                36.736_9,
                -87.285,
                36.990_3,
                -86.443_6,
                "Bowling Green from KHPX",
            ),
            (37.068_33, -88.771_94, 37.083_4, -88.6, "Paducah from KPAH"),
        ];
        for (site_lat, site_lon, place_lat, place_lon, label) in cases {
            let (site_x, site_y) = projection::mercator(site_lat, site_lon)?;
            let offset = place_offset(place_lat, place_lon, site_x, site_y)?;
            assert!(offset.x > 0.0, "{label}: east offset {}", offset.x);
            assert!(offset.y > 0.0, "{label}: north offset {}", offset.y);
        }
        Ok(())
    }
}
