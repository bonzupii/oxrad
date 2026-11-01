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
//! A frame is a texel per gate plus a lattice that carries both the geodesic
//! position of every lattice corner and its coordinate in the raster. The GPU
//! interpolates the lattice and filters the raster, so gate placement is the
//! WGS84 direct solution while the image itself is drawn in one pass. Building
//! a textured quad per gate instead cost tens of megabytes per frame and made
//! every redraw and zoom pay for a million triangles.
//!
//! Lattice vertices carry kilometre offsets from the site, which the shader
//! scales into the viewport, so one frame serves every window size. The pointer
//! lookup inverts the same model.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{Pos2, Rect};

use crate::colormap::ColorTable;
use crate::derived;
use crate::error::DerivedError;
use crate::error::{CoordinateError, RenderError};
use crate::product::RadarProduct;
use crate::projection;
use crate::radar::{GateValue, RadarVolume, RadialBins, RetrievedVolume};
use crate::raster::{self, GateGrid, RadarRaster};

/// Bytes per lattice vertex: position and texture coordinate.
pub const LATTICE_STRIDE: usize = 16;
/// Range subdivisions between the site and the farthest gate.
///
/// Gate spacing is uniform along a radial, but ground range is not linear in
/// slant range. Sixty-four subdivisions keep the lattice's linear interpolation
/// of ground range inside a fraction of a metre at every elevation angle the
/// radar scans.
const LATTICE_RANGE_STEPS: usize = 64;

/// How a frame's raster is sampled onto the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sampling {
    /// Each screen pixel takes the colour of the gate that covers it.
    #[default]
    Nearest,
    /// Colours are blended between neighbouring gates.
    Bilinear,
}

impl Sampling {
    /// Every selectable sampling mode, in menu order.
    pub const ALL: [Self; 2] = [Self::Nearest, Self::Bilinear];

    /// The control label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Nearest => "Nearest gate",
            Self::Bilinear => "Smoothed",
        }
    }

    /// The configuration token written to disk.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Bilinear => "bilinear",
        }
    }

    /// Parses a configuration token.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "nearest" => Some(Self::Nearest),
            "bilinear" => Some(Self::Bilinear),
            _ => None,
        }
    }

    /// What the mode does to the measured data.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Nearest => "Shows each measured gate as its own block.",
            Self::Bilinear => "Blends between gates for display only; no new measurements.",
        }
    }
}

/// Where a frame's vertices land inside a canvas.
///
/// The lattice stores site-relative offsets in projected kilometres, so one
/// scale and one origin place it in any window, at any zoom. Both come from the
/// geographic transform that also places the rings, the warnings, and the
/// pointer readout, so the raster cannot drift away from the map under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Clip-space units per projected kilometre, one per axis.
    pub scale: [f32; 2],
    /// The station's position in clip space.
    pub origin: [f32; 2],
}

impl Placement {
    /// Places a station drawn at `site` inside `rect` for a view `span_km` wide.
    ///
    /// The geographic transform draws a projected kilometre at the same number
    /// of pixels on both axes, so the view's width fixes the scale and the
    /// station's own screen position fixes the origin.
    pub fn new(site: Pos2, rect: Rect, span_km: f64) -> Result<Self, CoordinateError> {
        if !(span_km.is_finite() && span_km > 0.0) {
            return Err(CoordinateError::ViewExtent { extent_km: span_km });
        }
        let width = f64::from(rect.width());
        let height = f64::from(rect.height());
        if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
            return Err(CoordinateError::Viewport {
                width: rect.width(),
                height: rect.height(),
            });
        }
        let center = rect.center();
        let clip_per_km = 2.0 / span_km;
        Ok(Self {
            scale: [clip_per_km as f32, (clip_per_km * width / height) as f32],
            origin: [
                (2.0 * f64::from(site.x - center.x) / width) as f32,
                (-2.0 * f64::from(site.y - center.y) / height) as f32,
            ],
        })
    }
}

/// What a gate holds, as the interface reads it back.
#[derive(Debug, Clone, Copy)]
pub enum Reading {
    /// A gate the radar measured, including its own "nothing here" reports.
    Measured(GateValue),
    /// A value computed from the column at this gate.
    Derived(f64),
}

/// The gate under a map point.
#[derive(Debug, Clone, Copy)]
pub struct GateHit {
    /// What the gate holds.
    pub reading: Reading,
    /// The elevation angle of the radial that carries it.
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
            reading: Reading::Measured(value),
            elevation_deg: radial.elevation_deg,
        }))
    }

    /// The radials the lookup was built from, for a derived reader that has to
    /// apply the same azimuth and gate geometry.
    fn geometry(&self) -> &[RadialLookup] {
        &self.radials
    }
}

/// Azimuth and gate lookup for a product computed from the volume.
///
/// The column values are held on the same grid the raster was built on, so the
/// gate a pointer lands on is the gate the readout reports.
pub struct DerivedLookup {
    sweep: SweepLookup,
    field: derived::DerivedField,
}

impl DerivedLookup {
    fn new(volume: &RadarVolume, product: RadarProduct) -> Result<Self, DerivedError> {
        Ok(Self {
            sweep: SweepLookup::new(volume, RadarProduct::Reflectivity),
            field: derived::evaluate(volume, product)?,
        })
    }

    /// The gate under a ground range and azimuth, with its column value.
    pub fn gate(
        &self,
        range_km: f64,
        azimuth_deg: f64,
    ) -> Result<Option<GateHit>, CoordinateError> {
        let Some(index) = self.sweep.radial_at(azimuth_deg) else {
            return Ok(None);
        };
        let Some(lookup) = self.sweep.geometry().get(index) else {
            return Ok(None);
        };
        let Some(gate) = lookup.gate_at(range_km)? else {
            return Ok(None);
        };
        let Some(value) = self.field.value(index, gate) else {
            return Ok(None);
        };
        Ok(Some(GateHit {
            reading: Reading::Derived(value),
            elevation_deg: lookup.elevation_deg,
        }))
    }
}

/// The gate lookup for whichever kind of product is drawn.
pub enum Lookup {
    Measured(SweepLookup),
    Derived(DerivedLookup),
}

impl Lookup {
    /// The gate under a map point, for the product that was drawn.
    pub fn gate(
        &self,
        volume: &RadarVolume,
        range_km: f64,
        azimuth_deg: f64,
    ) -> Result<Option<GateHit>, CoordinateError> {
        match self {
            Self::Measured(lookup) => lookup.gate(volume, range_km, azimuth_deg),
            Self::Derived(lookup) => lookup.gate(range_km, azimuth_deg),
        }
    }
}

/// The mesh that places raster texels on the map.
pub struct Lattice {
    /// Interleaved `x, y, u, v` vertices, site-relative and north-positive.
    pub vertices: Vec<u8>,
    /// Triangle indices into `vertices`.
    pub indices: Vec<u8>,
    /// Index count the draw call submits.
    pub index_count: u32,
}

impl Lattice {
    /// The site-relative kilometre offset of a vertex.
    #[cfg(test)]
    pub fn position(&self, index: usize) -> Option<Pos2> {
        let start = index.checked_mul(LATTICE_STRIDE)?;
        Some(Pos2::new(
            read_f32(&self.vertices, start)?,
            read_f32(&self.vertices, start + 4)?,
        ))
    }

    /// The raster coordinate of a vertex, from zero to one on each axis.
    #[cfg(test)]
    pub fn texcoord(&self, index: usize) -> Option<Pos2> {
        let start = index.checked_mul(LATTICE_STRIDE)?;
        Some(Pos2::new(
            read_f32(&self.vertices, start + 8)?,
            read_f32(&self.vertices, start + 12)?,
        ))
    }

    /// Whether the lattice has nothing to draw.
    pub const fn is_empty(&self) -> bool {
        self.index_count == 0
    }

    fn reserved(vertex_count: usize, quad_count: usize) -> Self {
        Self {
            vertices: Vec::with_capacity(vertex_count.saturating_mul(LATTICE_STRIDE)),
            indices: Vec::with_capacity(quad_count.saturating_mul(6 * 4)),
            index_count: 0,
        }
    }

    fn push_vertex(&mut self, offset: Pos2, texcoord: Pos2) {
        for value in [offset.x, offset.y, texcoord.x, texcoord.y] {
            self.vertices.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// Adds the two triangles of the lattice cell whose near-left corner is
    /// `base`, with `row_stride` vertices between one row and the next.
    fn push_quad(&mut self, base: u32, row_stride: u32) -> Result<(), RenderError> {
        let below = base.checked_add(row_stride).ok_or(RenderError::MeshSize {
            vertices: self.vertices.len() / LATTICE_STRIDE,
        })?;
        for index in [base, base + 1, below + 1, base, below + 1, below] {
            self.indices.extend_from_slice(&index.to_le_bytes());
        }
        self.index_count =
            u32::try_from(self.indices.len() / 4).map_err(|_| RenderError::MeshSize {
                vertices: self.vertices.len() / LATTICE_STRIDE,
            })?;
        Ok(())
    }
}

#[cfg(test)]
fn read_f32(buffer: &[u8], start: usize) -> Option<f32> {
    let bytes: [u8; 4] = buffer.get(start..start + 4)?.try_into().ok()?;
    Some(f32::from_le_bytes(bytes))
}

/// Everything a frame draws, built once per volume off the UI thread.
pub struct RadarGeometry {
    /// Radius the view is scaled to, in kilometres.
    pub extent_km: f64,
    /// Gate colours, one texel per gate.
    pub raster: RadarRaster,
    /// The mesh that samples the raster.
    pub lattice: Lattice,
    /// Pointer lookup into what was drawn.
    pub lookup: Lookup,
    /// The product the raster was coloured for.
    pub product: RadarProduct,
    /// The table the raster was coloured with.
    pub color_table: ColorTable,
    /// Site latitude.
    pub site_lat: f64,
    /// Site longitude.
    pub site_lon: f64,
}

pub struct BuildRequest {
    pub volume: RetrievedVolume,
    pub product: RadarProduct,
    pub color_table: ColorTable,
    pub generation: u64,
}

pub struct BuildResult {
    pub volume: RetrievedVolume,
    pub geometry: Result<RadarGeometry, RenderError>,
    pub generation: u64,
    /// How long the raster and lattice took to build, off the UI thread.
    pub elapsed: Duration,
}

/// Runs geometry construction on its own thread.
///
/// A thread that dies without answering leaves the receiver disconnected, which
/// the display reports as `WorkerStopped`. Nothing catches a panic here: a panic
/// in this code is a defect, and dressing it as a frame error would let a broken
/// build keep drawing stale frames as though it were healthy.
pub fn start_build_thread() -> (Sender<BuildRequest>, Receiver<BuildResult>) {
    let (request_sender, request_receiver) = mpsc::channel::<BuildRequest>();
    let (result_sender, result_receiver) = mpsc::channel::<BuildResult>();
    std::thread::spawn(move || {
        while let Ok(request) = request_receiver.recv() {
            let started = Instant::now();
            let geometry =
                build_geometry_for(&request.volume.volume, request.product, request.color_table);
            let result = BuildResult {
                volume: request.volume,
                geometry,
                generation: request.generation,
                elapsed: started.elapsed(),
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

/// The azimuth and elevation of the lattice column at `column`.
fn column_angles(radials: &[RadialBins], column: usize) -> Option<(f64, f64)> {
    radials.get(column).map_or_else(
        || {
            radials
                .last()
                .map(|last| (last.azimuth_right_deg, last.elevation_deg))
        },
        |radial| Some((radial.azimuth_left_deg, radial.elevation_deg)),
    )
}

/// The vertex count of a lattice with `columns` radial boundaries and `rows`
/// range subdivisions.
fn lattice_shape(radials: usize, grid: GateGrid) -> (usize, usize) {
    let rows = LATTICE_RANGE_STEPS.min(grid.gate_count.max(1));
    (radials.saturating_add(1), rows)
}

fn build_lattice(
    volume: &RadarVolume,
    grid: GateGrid,
    site_x: f64,
    site_y: f64,
) -> Result<(Lattice, f64), RenderError> {
    let radials = &volume.sweep.radials;
    let (columns, rows) = lattice_shape(radials.len(), grid);
    let mut lattice = Lattice::reserved(
        columns.saturating_mul(rows.saturating_add(1)),
        columns.saturating_mul(rows),
    );
    if radials.is_empty() {
        return Ok((lattice, 0.0));
    }
    let mut extent_km: f64 = 0.0;
    for column in 0..columns {
        let Some((azimuth_deg, elevation_deg)) = column_angles(radials, column) else {
            continue;
        };
        let path = projection::GeodesicPath::new(volume.site_lat, volume.site_lon, azimuth_deg)
            .map_err(|source| RenderError::Coordinate { source })?;
        let v = column as f32 / columns.saturating_sub(1) as f32;
        for row in 0..=rows {
            let fraction = row as f32 / rows as f32;
            let gate = f64::from(fraction) * grid.gate_count as f64;
            let slant_km = grid.near_slant_km(gate).max(0.0);
            let ground_km = projection::ground_range(slant_km, elevation_deg)
                .map_err(|source| RenderError::Coordinate { source })?;
            let (lat, lon) = path
                .position(ground_km)
                .map_err(|source| RenderError::Coordinate { source })?;
            let offset = place_offset(lat, lon, site_x, site_y)
                .map_err(|source| RenderError::Coordinate { source })?;
            extent_km = extent_km.max(f64::from(offset.x.hypot(offset.y)));
            // A raster row runs along the gates and its rows stack along the
            // radials, so the range sets u and the radial sets v.
            lattice.push_vertex(offset, Pos2::new(fraction, v));
        }
    }
    let row_stride = u32::try_from(rows.saturating_add(1)).map_err(|_| RenderError::MeshSize {
        vertices: lattice.vertices.len() / LATTICE_STRIDE,
    })?;
    for column in 0..columns.saturating_sub(1) {
        for row in 0..rows {
            let base = column.saturating_mul(rows + 1).saturating_add(row);
            let base = u32::try_from(base).map_err(|_| RenderError::MeshSize {
                vertices: lattice.vertices.len() / LATTICE_STRIDE,
            })?;
            lattice.push_quad(base, row_stride)?;
        }
    }
    Ok((lattice, extent_km))
}

/// Builds one product's raster and the lattice that samples it, plus the
/// overlays and pointer lookup shared by both workspaces.
pub fn build_geometry_for(
    volume: &RadarVolume,
    product: RadarProduct,
    color_table: ColorTable,
) -> Result<RadarGeometry, RenderError> {
    if product.is_derived() {
        return build_derived_geometry(volume, product, color_table);
    }
    if !volume.sweep.products.contains(&product) {
        return Err(RenderError::ProductUnavailable {
            product,
            elevation: volume.sweep.elevation_number,
        });
    }
    let (site_x, site_y) = projection::mercator(volume.site_lat, volume.site_lon)
        .map_err(|source| RenderError::Coordinate { source })?;
    let raster = raster::build(volume, product, color_table);
    let (lattice, extent_km) = build_lattice(volume, raster.grid, site_x, site_y)?;
    let extent_km = if extent_km > 0.0 {
        extent_km
    } else {
        volume.sweep.max_ground_range_km.max(1.0)
    };
    Ok(RadarGeometry {
        extent_km,
        lookup: Lookup::Measured(SweepLookup::new(volume, product)),
        raster,
        lattice,
        product,
        color_table,
        site_lat: volume.site_lat,
        site_lon: volume.site_lon,
    })
}

/// Builds a volume product's raster on the drawn sweep's grid, with the lookup
/// that reads the same computed columns back.
fn build_derived_geometry(
    volume: &RadarVolume,
    product: RadarProduct,
    color_table: ColorTable,
) -> Result<RadarGeometry, RenderError> {
    let (site_x, site_y) = projection::mercator(volume.site_lat, volume.site_lon)
        .map_err(|source| RenderError::Coordinate { source })?;
    let raster = raster::build_derived(volume, product, color_table)
        .map_err(|source| RenderError::Derived { source })?;
    let lookup =
        DerivedLookup::new(volume, product).map_err(|source| RenderError::Derived { source })?;
    let (lattice, extent_km) = build_lattice(volume, raster.grid, site_x, site_y)?;
    let extent_km = if extent_km > 0.0 {
        extent_km
    } else {
        volume.sweep.max_ground_range_km.max(1.0)
    };
    Ok(RadarGeometry {
        extent_km,
        lookup: Lookup::Derived(lookup),
        raster,
        lattice,
        product,
        color_table,
        site_lat: volume.site_lat,
        site_lon: volume.site_lon,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use eframe::egui::Vec2;

    use crate::radar::{MomentBins, RadarSweep, RadialBins};
    use crate::stations::SiteId;
    use crate::test_support::TestError;

    const SITE_LAT: f64 = 38.996_94;
    const SITE_LON: f64 = -96.232_49;

    fn site() -> Result<SiteId, TestError> {
        Ok(SiteId::parse("KTWX")?)
    }

    fn radial(left: f64, right: f64, elevation_deg: f64, values: Vec<GateValue>) -> RadialBins {
        RadialBins {
            azimuth_number: 1,
            azimuth_left_deg: left,
            azimuth_right_deg: right,
            elevation_deg,
            first_gate_km: 74.75,
            gate_width_km: 0.5,
            moments: vec![MomentBins {
                product: RadarProduct::Reflectivity,
                first_gate_km: 74.75,
                gate_width_km: 0.5,
                values,
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
        let geometry =
            build_geometry_for(&volume, RadarProduct::Reflectivity, ColorTable::Standard)?;
        Ok((volume, geometry))
    }

    /// A two-elevation volume of uniform echo, so a computed product has a
    /// layer to integrate and a gate to read back.
    fn derived_volume() -> Result<RadarVolume, TestError> {
        let base = RadarSweep {
            elevation_deg: 0.5,
            elevation_number: 1,
            radials: vec![radial(89.5, 90.5, 0.5, vec![GateValue::Measured(45.0); 4])],
            products: vec![RadarProduct::Reflectivity],
            below_threshold: 0,
            range_folded: 0,
            max_ground_range_km: 76.0,
        };
        let upper = RadarSweep {
            elevation_deg: 5.0,
            elevation_number: 2,
            radials: vec![radial(89.5, 90.5, 5.0, vec![GateValue::Measured(30.0); 4])],
            products: vec![RadarProduct::Reflectivity],
            below_threshold: 0,
            range_folded: 0,
            max_ground_range_km: 76.0,
        };
        let mut volume = volume(
            vec![radial(89.5, 90.5, 0.5, vec![GateValue::Measured(45.0); 4])],
            76.0,
        )?;
        volume.sweep = base;
        volume.sweeps = vec![upper];
        Ok(volume)
    }

    #[test]
    fn a_derived_product_draws_and_reads_back_its_column() -> Result<(), TestError> {
        for product in RadarProduct::DERIVED {
            let volume = derived_volume()?;
            let geometry = build_geometry_for(&volume, product, ColorTable::Standard)?;
            assert!(
                !geometry.raster.is_empty(),
                "{} drew no gates",
                product.id()
            );
            let ground = projection::ground_range(74.75, 0.5)?;
            let hit = geometry.lookup.gate(&volume, ground, 90.0)?;
            match hit {
                Some(GateHit {
                    reading: Reading::Derived(value),
                    ..
                }) => {
                    assert!(value > 0.0, "{} read back {value}", product.id());
                }
                other => {
                    return Err(TestError::from(format!(
                        "{} read back nothing at a gate with echo: {other:?}",
                        product.id()
                    )));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn render_worker_returns_the_origin_with_the_matching_volume() -> Result<(), TestError> {
        let (sender, receiver) = start_build_thread();
        let retrieved = RetrievedVolume {
            volume: volume(
                vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(20.0)])],
                75.0,
            )?,
            origin: crate::radar::ArchiveOrigin::Cached,
            history_key: Some(String::from("2026/10/31/KTWX/KTWX20261031_003109_V06")),
        };
        sender
            .send(BuildRequest {
                volume: retrieved,
                product: RadarProduct::Reflectivity,
                color_table: ColorTable::Standard,
                generation: 42,
            })
            .map_err(|error| TestError::from(format!("render request failed: {error}")))?;
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|error| TestError::from(format!("render result failed: {error}")))?;
        assert_eq!(result.generation, 42);
        assert_eq!(result.volume.origin, crate::radar::ArchiveOrigin::Cached);
        assert_eq!(
            result.volume.history_key.as_deref(),
            Some("2026/10/31/KTWX/KTWX20261031_003109_V06")
        );
        assert_eq!(result.volume.volume.site.as_str(), "KTWX");
        assert!(result.geometry?.raster.counts.drawn > 0);
        Ok(())
    }

    /// At zero elevation the 75 km slant range converts to 74.998 km of ground
    /// range, so the far lattice corner sits 2 m inside the roadmap's 75 km
    /// reference point; 5e-5 degrees is 4.4 m at this latitude.
    #[test]
    fn rendered_corner_lands_on_the_reference_geodesic_point() -> Result<(), TestError> {
        let (_, geometry) = build(
            vec![radial(90.0, 90.0, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        assert!(geometry.lattice.index_count > 0, "lattice is empty");
        let (site_x, site_y) = projection::mercator(SITE_LAT, SITE_LON)?;
        let far = geometry.lattice.vertices.len() / LATTICE_STRIDE - 1;
        let corner = geometry
            .lattice
            .position(far)
            .ok_or("no far corner vertex")?;
        let texcoord = geometry
            .lattice
            .texcoord(far)
            .ok_or("no far corner texture coordinate")?;
        assert!((texcoord.y - 1.0).abs() < 1e-6, "far row {texcoord:?}");
        let (lat, lon) = projection::mercator_inverse(
            site_x + f64::from(corner.x),
            site_y + f64::from(corner.y),
        )?;
        assert!(
            (lat - 38.993_728).abs() < 5e-5,
            "rendered corner latitude {lat}"
        );
        assert!(
            (lon + 95.366_767).abs() < 5e-5,
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
                reading: Reading::Measured(GateValue::Measured(dbz)),
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
        assert!(
            geometry.raster.is_empty(),
            "below-threshold gates are excluded from the raster"
        );
        let ground = projection::ground_range(74.75, 0.0)?;
        let hit = geometry.lookup.gate(&volume, ground, 90.0)?;
        match hit {
            Some(GateHit {
                reading: Reading::Measured(GateValue::BelowThreshold),
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
        let start = 0;
        let texel: [u8; 4] = geometry
            .raster
            .texels
            .get(start..start + 4)
            .ok_or("missing texel")?
            .try_into()
            .map_err(|_| TestError::from(String::from("texel is truncated")))?;
        assert_eq!(texel, crate::colormap::range_folded_color());
        Ok(())
    }

    #[test]
    fn missing_product_is_not_rendered_as_an_empty_sweep() -> Result<(), TestError> {
        let volume = volume(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(30.0)])],
            75.0,
        )?;
        assert!(matches!(
            build_geometry_for(&volume, RadarProduct::Velocity, ColorTable::Standard),
            Err(RenderError::ProductUnavailable {
                product: RadarProduct::Velocity,
                elevation: 1,
            })
        ));
        Ok(())
    }

    /// A value above the top of the scale is a gate to draw, not a reason to
    /// abandon the whole image: the frame keeps every other gate and reports
    /// how many were clipped.
    #[test]
    fn a_gate_above_the_scale_does_not_fail_the_frame() -> Result<(), TestError> {
        let volume = volume(
            vec![radial(89.5, 90.5, 0.0, vec![GateValue::Measured(95.0)])],
            75.0,
        )?;
        let geometry =
            build_geometry_for(&volume, RadarProduct::Reflectivity, ColorTable::Standard)?;
        assert_eq!(geometry.raster.counts.above_scale, 1);
        assert_eq!(geometry.raster.counts.drawn, 1);
        Ok(())
    }

    /// The GPU reads a texel as a texture does: gates along `u`, radials along
    /// `v`. Following a mesh corner's texture coordinate back to the texel it
    /// lands on must recover the radial and gate that vertex stands on; when the
    /// raster was laid out rows-per-radial, this walked into a neighbouring
    /// radial and the sweep came out sheared.
    #[test]
    fn every_mesh_corner_samples_the_gate_it_stands_on() -> Result<(), TestError> {
        let gates = 6;
        let radials = vec![
            radial(0.0, 1.0, 0.5, vec![GateValue::Measured(10.0); gates]),
            radial(1.0, 2.0, 0.5, vec![GateValue::Measured(20.0); gates]),
            radial(2.0, 3.0, 0.5, vec![GateValue::Measured(30.0); gates]),
        ];
        let (_, geometry) = build(radials, 75.0)?;
        let raster = &geometry.raster;
        assert_eq!(raster.gates as usize, gates, "raster width");
        assert_eq!(raster.radials, 3, "raster height");
        let columns = 4;
        let rows = LATTICE_RANGE_STEPS.min(gates);
        for column in 0..columns {
            for row in 0..=rows {
                let index = column * (rows + 1) + row;
                let texcoord = geometry.lattice.texcoord(index).ok_or("missing texcoord")?;
                let gate = (texcoord.x * raster.gates as f32).clamp(0.0, (raster.gates - 1) as f32)
                    as usize;
                let radial = (texcoord.y * raster.radials as f32)
                    .clamp(0.0, (raster.radials - 1) as f32) as usize;
                assert_eq!(
                    radial,
                    column.min(raster.radials as usize - 1),
                    "column {column} sampled radial {radial}"
                );
                let start = (radial * gates + gate) * 4;
                let alpha = raster.texels.get(start + 3).copied().ok_or("texel")?;
                let expected = if gate < gates { 255 } else { 0 };
                assert_eq!(alpha, expected, "column {column} row {row}");
            }
        }
        Ok(())
    }

    #[test]
    fn the_lattice_samples_every_corner_of_the_raster() -> Result<(), TestError> {
        let values = vec![GateValue::Measured(30.0); 8];
        let (_, geometry) = build(
            vec![
                radial(89.5, 90.5, 0.0, values.clone()),
                radial(90.5, 91.5, 0.0, values),
            ],
            75.0,
        )?;
        let columns = 3;
        let rows = LATTICE_RANGE_STEPS.min(8);
        let vertices = (columns) * (rows + 1);
        assert_eq!(geometry.lattice.vertices.len() / LATTICE_STRIDE, vertices);
        let last = geometry
            .lattice
            .texcoord(vertices - 1)
            .ok_or("no last vertex")?;
        assert!((last.x - 1.0).abs() < 1e-6, "last gate {last:?}");
        assert!((last.y - 1.0).abs() < 1e-6, "last radial {last:?}");
        let second = geometry.lattice.texcoord(1).ok_or("no second vertex")?;
        assert!(second.y.abs() < 1e-6, "first radial {second:?}");
        assert!(second.x > 0.0, "second vertex is past the near gate");
        Ok(())
    }

    /// A 1,600-pixel view spanning 8,000 km draws a kilometre in 0.2 pixels, so
    /// 800 km east of a station that already sits 80 px right of the centre
    /// must land 240 px right of it. The height covers 4,500 km, so 400 km south
    /// of a station 40 px above the centre lands 40 px below it. Both
    /// expectations are arithmetic on the view, not the placement formula.
    #[test]
    fn a_gate_draws_at_the_distance_the_view_says_it_should() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0));
        let site = rect.center() + Vec2::new(80.0, -40.0);
        let placement = Placement::new(site, rect, 8_000.0)?;
        let east_km = 800.0_f32;
        let clip_x = placement.origin[0] + east_km * placement.scale[0];
        let pixels_right = f64::from(clip_x) * 0.5 * f64::from(rect.width());
        assert!(
            (pixels_right - 240.0).abs() < 0.01,
            "gate drew {pixels_right} px right of the centre"
        );
        let south_km = 400.0_f32;
        let clip_y = placement.origin[1] - south_km * placement.scale[1];
        let pixels_up = f64::from(clip_y) * 0.5 * f64::from(rect.height());
        assert!(
            (pixels_up + 40.0).abs() < 0.01,
            "gate drew {pixels_up} px above the centre"
        );
        Ok(())
    }

    /// The placement and the geographic transform draw a kilometre with the
    /// same number of pixels, so a station plotted through one lands on the
    /// station plotted through the other.
    #[test]
    fn placement_agrees_with_the_geographic_transform() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0));
        let view = crate::map::MapView::focus(SITE_LAT, SITE_LON, 600.0)?;
        let transform = view.transform(rect)?;
        let site = transform
            .screen(SITE_LAT, SITE_LON)
            .ok_or("station does not project")?;
        let placement = Placement::new(site, rect, view.span_km())?;
        let (lat, lon) = projection::forward(120.0, 45.0, SITE_LAT, SITE_LON)?;
        let drawn = transform.screen(lat, lon).ok_or("gate does not project")?;
        let (site_x, site_y) = projection::mercator(SITE_LAT, SITE_LON)?;
        let offset = place_offset(lat, lon, site_x, site_y)?;
        let clip_x = placement.origin[0] + offset.x * placement.scale[0];
        let clip_y = placement.origin[1] + offset.y * placement.scale[1];
        let placed = Pos2::new(
            rect.center().x + clip_x * 0.5 * rect.width(),
            rect.center().y - clip_y * 0.5 * rect.height(),
        );
        assert!(
            (placed - drawn).length() < 0.5,
            "placement drew {placed:?} but the map drew {drawn:?}"
        );
        Ok(())
    }

    #[test]
    fn a_viewport_without_pixels_is_rejected() -> Result<(), TestError> {
        let empty = Rect::from_min_size(Pos2::ZERO, Vec2::ZERO);
        match Placement::new(Pos2::ZERO, empty, 600.0) {
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
    fn a_view_span_without_kilometres_is_rejected() -> Result<(), TestError> {
        match Placement::new(
            Pos2::ZERO,
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0)),
            0.0,
        ) {
            Err(CoordinateError::ViewExtent { extent_km }) => {
                assert!(extent_km.abs() < f64::EPSILON);
                Ok(())
            }
            other => Err(TestError::from(format!(
                "expected a view extent error, got {other:?}"
            ))),
        }
    }

    /// North is positive Mercator y and must draw above the site; east is
    /// positive Mercator x and must draw to the right. Both expectations come
    /// from the projection, not from the placement under test.
    #[test]
    fn north_and_east_plot_on_the_expected_sides_of_the_site() -> Result<(), TestError> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 900.0));
        let site = rect.center();
        let placement = Placement::new(site, rect, 600.0)?;
        let (site_x, site_y) = projection::mercator(SITE_LAT, SITE_LON)?;
        let (north_lat, north_lon) = projection::forward(75.0, 0.0, SITE_LAT, SITE_LON)?;
        let north = place_offset(north_lat, north_lon, site_x, site_y)?;
        assert!(north.y > 0.0, "north offset y {}", north.y);
        let north_clip = placement.origin[1] + north.y * placement.scale[1];
        assert!(north_clip > 0.0, "north drew below the site: {north_clip}");
        let (east_lat, east_lon) = projection::forward(75.0, 90.0, SITE_LAT, SITE_LON)?;
        let east = place_offset(east_lat, east_lon, site_x, site_y)?;
        assert!(east.x > 0.0, "east offset x {}", east.x);
        let east_clip = placement.origin[0] + east.x * placement.scale[0];
        assert!(east_clip > 0.0, "east drew left of the site: {east_clip}");
        Ok(())
    }

    /// Emporia is east and south of KTWX; Topeka is east and north of KTWX.
    /// Both are independent geographic facts about the station catalog.
    #[test]
    fn a_place_marker_sits_on_the_expected_bearing_from_the_station() -> Result<(), TestError> {
        let cases = [
            (
                38.996_94,
                -96.232_49,
                39.047_3,
                -95.675_2,
                "Topeka from KTWX",
            ),
            (
                37.654_44,
                -97.443_05,
                37.687_2,
                -97.330_1,
                "Wichita from KICT",
            ),
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
