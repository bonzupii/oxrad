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
//! The gate raster a frame uploads to the GPU.
//!
//! The raster holds one RGBA texel per gate in the same order a texture reads
//! it: gates run along a row and radials stack down the rows. The displayed
//! image is this raster sampled through a lattice whose vertices carry geodesic
//! positions and matching texture coordinates, so gate geometry stays correct
//! while the GPU does the filtering. Building a quad per gate instead cost tens
//! of megabytes per frame and forced every zoom level through the same fixed
//! tessellation.
//!
//! The axis names are deliberately `gates` and `radials` rather than width and
//! height. When they were called width and height it was easy to write a buffer
//! of rows-per-radial into a texture of columns-per-radial, which re-cut every
//! radial against the next one and sheared the whole sweep into spirals.

use crate::colormap::{ColorTable, MappedColor, map_display_color, range_folded_color};
use crate::derived;
use crate::error::DerivedError;
use crate::product::RadarProduct;
use crate::radar::{GateValue, MomentBins, RadarVolume};

/// The gate geometry a raster row is indexed on.
///
/// Radials of one sweep normally share it. A radial that disagrees is not
/// stretched into the sweep's grid: it is counted in
/// [`GateCounts::mismatched_radials`] and left transparent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateGrid {
    /// Range to the near edge of the first gate, in kilometres of slant range.
    pub first_gate_km: f64,
    /// Gate spacing in kilometres of slant range.
    pub gate_width_km: f64,
    /// Gates per radial.
    pub gate_count: usize,
}

impl GateGrid {
    const fn of(moment: &MomentBins) -> Self {
        Self {
            first_gate_km: moment.first_gate_km,
            gate_width_km: moment.gate_width_km,
            gate_count: moment.values.len(),
        }
    }

    /// Slant range of the near edge of the gate at `index`.
    pub fn near_slant_km(&self, index: f64) -> f64 {
        self.first_gate_km + (index - 0.5) * self.gate_width_km
    }

    /// Whether two moments are indexed on the same gate spacing.
    fn shares_spacing(&self, moment: &MomentBins) -> bool {
        (self.first_gate_km - moment.first_gate_km).abs() < 1e-9
            && (self.gate_width_km - moment.gate_width_km).abs() < 1e-9
    }
}

/// What the raster did with each gate it considered.
///
/// Every field is surfaced in the interface. A product that silently dropped
/// half its gates would look like a product with no echo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateCounts {
    /// Gates drawn with a colour from the product scale.
    pub drawn: u32,
    /// Gates the radar reported as below the signal threshold.
    pub below_threshold: u32,
    /// Gates the radar reported as range folded.
    pub range_folded: u32,
    /// Measured gates above the top of the product scale.
    pub above_scale: u32,
    /// Measured gates below the bottom of the product scale, which is its
    /// noise floor, and therefore not drawn.
    pub below_scale: u32,
    /// Measured gates whose value was not a finite number.
    pub not_finite: u32,
    /// Radials whose gate spacing did not match the sweep's.
    pub mismatched_radials: u32,
}

/// One product of one sweep as RGBA texels.
#[derive(Debug)]
pub struct RadarRaster {
    /// Gates per radial, which is the texture's width and the fastest axis.
    pub gates: u32,
    /// Radials, which is the texture's height and the slowest axis.
    pub radials: u32,
    /// The gate grid every row is indexed on.
    pub grid: GateGrid,
    /// RGBA bytes, row-major, one row per radial.
    pub texels: Vec<u8>,
    /// What happened to the gates behind those texels.
    pub counts: GateCounts,
}

impl RadarRaster {
    /// Whether any gate carries a colour.
    pub const fn is_empty(&self) -> bool {
        self.counts.drawn == 0
    }
}

/// Chooses the gate grid for the selected product.
///
/// A sweep that declares a product it has no radials for still needs a grid so
/// the frame can draw nothing instead of failing: the raster is then empty and
/// the interface says the sweep has no measured echo.
fn grid_for(volume: &RadarVolume, product: RadarProduct) -> GateGrid {
    if let Some(moment) = volume
        .sweep
        .radials
        .iter()
        .find_map(|radial| radial.moment(product))
    {
        return GateGrid::of(moment);
    }
    volume.sweep.radials.first().map_or(
        GateGrid {
            first_gate_km: 0.0,
            gate_width_km: 1.0,
            gate_count: 0,
        },
        |radial| GateGrid {
            first_gate_km: radial.first_gate_km,
            gate_width_km: radial.gate_width_km,
            gate_count: 0,
        },
    )
}

/// Builds the raster for one product of the selected sweep.
pub fn build(volume: &RadarVolume, product: RadarProduct, table: ColorTable) -> RadarRaster {
    let grid = grid_for(volume, product);
    let gates = volume
        .sweep
        .radials
        .iter()
        .filter_map(|radial| radial.moment(product))
        .filter(|moment| grid.shares_spacing(moment))
        .map(|moment| moment.values.len())
        .fold(grid.gate_count, usize::max);
    let radials = volume.sweep.radials.len();
    // A texture needs at least one row; an all-transparent row draws nothing.
    let row_length = gates.max(1);
    let (Ok(gates_u32), Ok(radials_u32)) = (u32::try_from(row_length), u32::try_from(radials))
    else {
        return RadarRaster {
            gates: 0,
            radials: 0,
            grid,
            texels: Vec::new(),
            counts: GateCounts::default(),
        };
    };
    let mut raster = RadarRaster {
        gates: gates_u32,
        radials: radials_u32,
        grid: GateGrid {
            first_gate_km: grid.first_gate_km,
            gate_width_km: grid.gate_width_km,
            gate_count: gates,
        },
        texels: vec![0; radials * row_length * 4],
        counts: GateCounts::default(),
    };
    fill_rows(&mut raster, volume, product, table);
    raster
}

fn fill_rows(
    raster: &mut RadarRaster,
    volume: &RadarVolume,
    product: RadarProduct,
    table: ColorTable,
) {
    let row_bytes = raster.gates as usize * 4;
    for (index, row) in raster.texels.chunks_exact_mut(row_bytes).enumerate() {
        let Some(radial) = volume.sweep.radials.get(index) else {
            continue;
        };
        let Some(moment) = radial.moment(product) else {
            continue;
        };
        if !raster.grid.shares_spacing(moment) {
            raster.counts.mismatched_radials += 1;
            continue;
        }
        fill_row(row, moment, product, table, &mut raster.counts);
    }
}

/// Builds the raster for a product computed from the whole volume.
///
/// The texels land on the gate grid of the moment the product is computed from,
/// so the lattice that places them and the pointer lookup over them stay the
/// same as for a measured moment.
///
/// # Errors
///
/// [`DerivedError`] from the computation, which the caller reports rather than
/// drawing an empty product as though the sky were clear.
pub fn build_derived(
    volume: &RadarVolume,
    product: RadarProduct,
    table: ColorTable,
    motion: derived::StormMotion,
) -> Result<RadarRaster, DerivedError> {
    let field = derived::evaluate(volume, product, motion)?;
    let Some(source) = product.source_moment() else {
        return Err(DerivedError::NotDerived { product });
    };
    let grid = grid_for(volume, source);
    let gates = grid.gate_count.max(field.gates);
    let radials = field.radials;
    let (Ok(gates_u32), Ok(radials_u32)) = (u32::try_from(gates), u32::try_from(radials)) else {
        return Err(DerivedError::GridSize { gates, radials });
    };
    let row_length = gates.max(1);
    let mut raster = RadarRaster {
        gates: gates_u32,
        radials: radials_u32,
        grid: GateGrid {
            first_gate_km: grid.first_gate_km,
            gate_width_km: grid.gate_width_km,
            gate_count: gates,
        },
        texels: vec![0; radials.saturating_mul(row_length).saturating_mul(4)],
        counts: GateCounts::default(),
    };
    let row_bytes = row_length * 4;
    for (index, row) in raster.texels.chunks_exact_mut(row_bytes).enumerate() {
        let (texels, _) = row.as_chunks_mut::<4>();
        for (gate, texel) in texels.iter_mut().enumerate() {
            let Some(value) = field.value(index, gate) else {
                continue;
            };
            if !value.is_finite() || value.abs() > f64::from(f32::MAX) {
                raster.counts.not_finite += 1;
                continue;
            }
            let value = value as f32;
            if let Some(colour) = measured_colour(table, product, value, &mut raster.counts) {
                texel.copy_from_slice(&colour);
            }
        }
    }
    Ok(raster)
}

fn fill_row(
    row: &mut [u8],
    moment: &MomentBins,
    product: RadarProduct,
    table: ColorTable,
    counts: &mut GateCounts,
) {
    let (texels, _) = row.as_chunks_mut::<4>();
    for (gate, texel) in texels.iter_mut().enumerate() {
        let Some(value) = moment.values.get(gate).copied() else {
            continue;
        };
        match value {
            GateValue::BelowThreshold => counts.below_threshold += 1,
            GateValue::RangeFolded => {
                let colour = range_folded_color();
                texel.copy_from_slice(&colour);
                counts.range_folded += 1;
            }
            GateValue::Measured(value) => {
                if let Some(colour) = measured_colour(table, product, value, counts) {
                    texel.copy_from_slice(&colour);
                }
            }
        }
    }
}

fn measured_colour(
    table: ColorTable,
    product: RadarProduct,
    value: f32,
    counts: &mut GateCounts,
) -> Option<[u8; 4]> {
    match map_display_color(table, product, value) {
        Ok(MappedColor::BelowScale) => {
            counts.below_scale += 1;
            None
        }
        Ok(mapped) => {
            let colour = mapped.colour();
            if let Some(colour) = colour {
                counts.drawn += 1;
                if matches!(mapped, MappedColor::ClippedAbove(_)) {
                    counts.above_scale += 1;
                }
                Some(colour)
            } else {
                None
            }
        }
        Err(_) => {
            counts.not_finite += 1;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::radar::{RadarSweep, RadialBins};
    use crate::stations::SiteId;
    use crate::test_support::TestError;

    fn radial(values: Vec<GateValue>) -> RadialBins {
        RadialBins {
            azimuth_number: 1,
            azimuth_left_deg: 0.0,
            azimuth_right_deg: 1.0,
            elevation_deg: 0.5,
            first_gate_km: 2.0,
            gate_width_km: 0.25,
            moments: vec![MomentBins {
                product: RadarProduct::Reflectivity,
                first_gate_km: 2.0,
                gate_width_km: 0.25,
                values,
            }],
        }
    }

    fn volume(radials: Vec<RadialBins>) -> Result<RadarVolume, TestError> {
        Ok(RadarVolume {
            site: SiteId::parse("KTWX")?,
            site_lat: 38.996_94,
            site_lon: -96.232_49,
            scan_unix_ms: 1_793_327_729_000,
            sweep: RadarSweep {
                elevation_deg: 0.5,
                elevation_number: 1,
                radials,
                products: vec![RadarProduct::Reflectivity],
                below_threshold: 0,
                range_folded: 0,
                max_ground_range_km: 100.0,
            },
            sweeps: Vec::new(),
            skipped_radials: 0,
            skipped_non_data: 0,
            moment_error: None,
        })
    }

    fn build_ok(radials: Vec<RadialBins>) -> Result<RadarRaster, TestError> {
        let volume = volume(radials)?;
        Ok(build(
            &volume,
            RadarProduct::Reflectivity,
            ColorTable::Standard,
        ))
    }

    /// Reads a texel the way a texture row-major over gates reads it, so a
    /// buffer written in the wrong order fails the test instead of the display.
    fn texel_at(raster: &RadarRaster, radial: usize, gate: usize) -> Result<[u8; 4], TestError> {
        let start = radial * raster.gates as usize + gate;
        let start = start * 4;
        let bytes = raster
            .texels
            .get(start..start + 4)
            .ok_or_else(|| TestError::from(format!("texel {radial}/{gate} outside the raster")))?;
        bytes
            .try_into()
            .map_err(|_| TestError::from(format!("texel {radial}/{gate} is truncated")))
    }

    #[test]
    fn a_measured_gate_becomes_an_opaque_texel() -> Result<(), TestError> {
        let raster = build_ok(vec![radial(vec![GateValue::Measured(20.0)])])?;
        assert_eq!(raster.gates, 1);
        assert_eq!(raster.radials, 1);
        let texel = texel_at(&raster, 0, 0)?;
        assert_eq!(texel[3], 255, "alpha");
        assert_eq!(raster.counts.drawn, 1);
        Ok(())
    }

    #[test]
    fn a_below_threshold_gate_stays_transparent_and_is_counted() -> Result<(), TestError> {
        let raster = build_ok(vec![radial(vec![GateValue::BelowThreshold])])?;
        assert_eq!(texel_at(&raster, 0, 0)?[3], 0, "alpha");
        assert_eq!(raster.counts.below_threshold, 1);
        assert_eq!(raster.counts.drawn, 0);
        assert!(raster.is_empty());
        Ok(())
    }

    #[test]
    fn a_gate_above_the_scale_is_drawn_at_the_top_colour_and_counted() -> Result<(), TestError> {
        let raster = build_ok(vec![radial(vec![GateValue::Measured(95.0)])])?;
        let (minimum, maximum) = RadarProduct::Reflectivity.value_range();
        let top = map_display_color(ColorTable::Standard, RadarProduct::Reflectivity, maximum)?;
        let MappedColor::Inside(top) = top else {
            return Err(TestError::from(String::from(
                "the top of the scale clipped",
            )));
        };
        assert_eq!(texel_at(&raster, 0, 0)?, top);
        assert_eq!(raster.counts.above_scale, 1);
        assert_eq!(raster.counts.drawn, 1);
        assert!(maximum > minimum);
        Ok(())
    }

    #[test]
    fn a_gate_below_the_noise_floor_is_counted_and_not_drawn() -> Result<(), TestError> {
        let volume = volume(vec![radial(vec![GateValue::Measured(-40.0)])])?;
        let raster = build(&volume, RadarProduct::Reflectivity, ColorTable::Standard);
        assert_eq!(raster.counts.below_scale, 1);
        assert_eq!(raster.counts.drawn, 0);
        assert_eq!(texel_at(&raster, 0, 0)?[3], 0, "alpha");
        Ok(())
    }

    #[test]
    fn a_non_finite_gate_is_counted_and_left_transparent() -> Result<(), TestError> {
        let raster = build_ok(vec![radial(vec![GateValue::Measured(f32::NAN)])])?;
        assert_eq!(raster.counts.not_finite, 1);
        assert_eq!(texel_at(&raster, 0, 0)?[3], 0, "alpha");
        Ok(())
    }

    #[test]
    fn a_radial_with_other_gate_spacing_is_not_stretched_into_the_grid() -> Result<(), TestError> {
        let mut wide = radial(vec![GateValue::Measured(10.0)]);
        if let Some(moment) = wide.moments.first_mut() {
            moment.gate_width_km = 1.0;
        }
        let raster = build_ok(vec![radial(vec![GateValue::Measured(20.0)]), wide])?;
        assert_eq!(raster.counts.mismatched_radials, 1);
        assert_eq!(raster.counts.drawn, 1);
        assert_eq!(texel_at(&raster, 1, 0)?[3], 0, "alpha");
        Ok(())
    }

    #[test]
    fn a_product_with_no_radials_builds_an_empty_raster() -> Result<(), TestError> {
        let volume = volume(vec![radial(vec![GateValue::Measured(20.0)])])?;
        let raster = build(&volume, RadarProduct::Velocity, ColorTable::Standard);
        assert!(raster.is_empty(), "no velocity gate is drawn");
        assert_eq!(raster.radials, 1, "one transparent row");
        assert_eq!(raster.counts, GateCounts::default());
        Ok(())
    }
}
