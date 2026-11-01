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
//! Products computed from a whole volume instead of decoded from one sweep.
//!
//! Two quantities are only meaningful when the elevations are stacked: how much
//! liquid a column holds (vertically integrated liquid) and how high its echo
//! reaches (echo tops). Both are computed here from the reflectivity gates of
//! every sweep in the volume, at the geometry of the sweep being drawn, so the
//! raster and lattice code keeps working on one gate grid.
//!
//! # Beam height
//!
//! A gate sits at a slant range along a beam that is bending with the Earth's
//! curvature and the standard atmosphere. The height above the radar is
//!
//! ```text
//! h = sqrt(r² + R'² + 2 r R' sin θ) − R'
//! ```
//!
//! with `R'` the four-thirds Earth radius (8494.667 km) and `r` the slant range
//! of the gate centre. The height is above the radar, not above sea level: the
//! archive record this program decodes does not carry the site elevation, and
//! inventing one would place the cloud deck at the wrong altitude. The unit is
//! labelled accordingly wherever the product reaches the interface.
//!
//! # Vertical resolution
//!
//! Levels are matched by the radial's azimuth number and by the slant range of
//! the gate centre, so volumes with mixed super-resolution and legacy gate
//! spacings still line up. A level whose nearest gate is more than half a gate
//! away is left out rather than stretched across a gap. Heights are integrated
//! between consecutive valid levels only, so a column with a single valid level
//! reports no liquid rather than a guess.

use crate::error::DerivedError;
use crate::product::RadarProduct;
use crate::projection;
use crate::radar::{GateValue, RadarSweep, RadarVolume, RadialBins};

/// The four-thirds Earth radius used for refraction, in kilometres.
///
/// This is the standard atmosphere's effective radius: the beam bends as though
/// the Earth were a third larger than it is, which is what the 4/3 convention
/// encodes.
const EFFECTIVE_EARTH_RADIUS_KM: f64 = 8_494.666_667;

/// The gate reflectivity at or above which an echo top is counted, in dBZ.
///
/// This is the threshold the weather service uses for its echo-top product.
const ECHO_TOP_THRESHOLD_DBZ: f64 = 18.5;

/// Converts reflectivity to liquid water content, in grams per cubic metre.
///
/// The relation is `M = 3.44e-3 Z^(4/7)` with `Z` the linear reflectivity, the
/// one used by the operational vertically integrated liquid algorithm.
const LIQUID_COEFFICIENT: f64 = 3.44e-3;

/// How the storm itself is moving, as the user estimates it.
///
/// Storm-relative velocity is measured velocity minus the component of this
/// motion along each beam. The default is a still storm, which makes the
/// product identical to base velocity and is therefore honest about the fact
/// that no motion has been supplied.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StormMotion {
    /// Speed over the ground, in kilometres per hour.
    pub speed_kmh: f64,
    /// Direction the storm is moving toward, in degrees clockwise from north.
    pub bearing_deg: f64,
}

impl StormMotion {
    /// The speed along a beam, in metres per second, positive away from the
    /// radar.
    ///
    /// A storm moving directly away along a beam adds to the measured velocity,
    /// and one moving across the beam adds nothing.
    pub fn along_beam_m_s(self, azimuth_deg: f64) -> f64 {
        let offset = (self.bearing_deg - azimuth_deg).to_radians();
        self.speed_kmh / 3.6 * offset.cos()
    }

    /// Whether the setting is a usable motion vector.
    pub fn is_usable(self) -> bool {
        self.speed_kmh.is_finite()
            && self.speed_kmh >= 0.0
            && self.speed_kmh <= MAX_STORM_SPEED_KMH
            && self.bearing_deg.is_finite()
            && (0.0..360.0).contains(&self.bearing_deg)
    }
}

/// The fastest storm motion the interface accepts, in kilometres per hour.
///
/// Faster than any translating storm on Earth, so a larger value is a mistyped
/// setting rather than a chaser report.
pub const MAX_STORM_SPEED_KMH: f64 = 200.0;

/// A volume product on the gate grid of the sweep being drawn.
#[derive(Debug)]
pub struct DerivedField {
    /// One row per radial of the drawn sweep, in its order.
    pub radials: usize,
    /// Gates per radial.
    pub gates: usize,
    /// Row-major values; `None` where the column has nothing to report.
    pub values: Vec<Option<f64>>,
}

impl DerivedField {
    /// The value at a radial and gate, when the column produced one.
    pub fn value(&self, radial: usize, gate: usize) -> Option<f64> {
        let index = radial.checked_mul(self.gates)?.checked_add(gate)?;
        self.values.get(index).copied().flatten()
    }

    /// A field with no values, used when there is nothing to compute.
    pub fn empty(radials: usize, gates: usize) -> Self {
        Self {
            radials,
            gates,
            values: vec![None; radials.saturating_mul(gates)],
        }
    }
}

/// The height above the radar of a gate, in kilometres.
///
/// # Errors
///
/// [`DerivedError::BeamHeight`] when the slant range or elevation is not a
/// finite number, and [`DerivedError::NonPhysicalHeight`] when the result is
/// not a real height, which means the input was not a usable beam geometry.
pub fn beam_height_km(slant_km: f64, elevation_deg: f64) -> Result<f64, DerivedError> {
    if !slant_km.is_finite() || slant_km < 0.0 || !elevation_deg.is_finite() {
        return Err(DerivedError::BeamHeight {
            slant_km,
            elevation_deg,
        });
    }
    let sine = elevation_deg.to_radians().sin();
    let radius = EFFECTIVE_EARTH_RADIUS_KM;
    let height =
        (slant_km * slant_km + radius * radius + 2.0 * slant_km * radius * sine).sqrt() - radius;
    if !height.is_finite() {
        return Err(DerivedError::BeamHeight {
            slant_km,
            elevation_deg,
        });
    }
    Ok(height)
}

/// The slant range at which a beam reaches a ground range, in kilometres.
///
/// This inverts the beam geometry used for [`beam_height_km`]: with `R'` the
/// four-thirds Earth radius, a central angle `a = g / R'` and elevation `e`, the
/// sine rule on the radar–gate–centre triangle gives `s = R' sin a / cos(a + e)`.
/// A beam elevated so far that `a + e` reaches ninety degrees never meets that
/// ground range at all, which is `None` rather than a made-up range.
pub fn slant_for_ground_km(ground_km: f64, elevation_deg: f64) -> Option<f64> {
    if !ground_km.is_finite() || ground_km < 0.0 || !elevation_deg.is_finite() {
        return None;
    }
    let radius = EFFECTIVE_EARTH_RADIUS_KM;
    let central = ground_km / radius;
    let cosine = (central + elevation_deg.to_radians()).cos();
    if !cosine.is_finite() || cosine <= 0.0 {
        return None;
    }
    let slant = radius * central.sin() / cosine;
    slant.is_finite().then_some(slant)
}

/// The liquid water content of a reflectivity value, in grams per cubic metre.
fn liquid_content(dbz: f64) -> f64 {
    let linear = 10_f64.powf(dbz / 10.0);
    LIQUID_COEFFICIENT * linear.powf(4.0 / 7.0)
}

/// One valid reflectivity gate in a column.
#[derive(Debug, Clone, Copy)]
struct Level {
    height_km: f64,
    dbz: f64,
}

/// The views of a column: every sweep that carries reflectivity at this
/// azimuth and range.
///
/// The radial of each sweep at this azimuth is resolved once per row rather
/// than once per gate: a volume has fourteen elevations and nearly two thousand
/// gates per radial, so searching per gate would multiply a row's work by that
/// factor for no reason.
fn column(levels: &[&RadialBins], ground_km: f64) -> Vec<Level> {
    let mut collected: Vec<Level> = Vec::with_capacity(levels.len());
    for radial in levels {
        let Some(moment) = radial.moment(RadarProduct::Reflectivity) else {
            continue;
        };
        if moment.values.is_empty()
            || !moment.gate_width_km.is_finite()
            || moment.gate_width_km <= 0.0
        {
            continue;
        }
        // The gate at this ground range on this beam. A higher elevation reaches
        // the same ground point at a shorter slant range, so the beam's own
        // geometry decides which gate the column reads.
        let Some(slant_km) = slant_for_ground_km(ground_km, radial.elevation_deg) else {
            continue;
        };
        let offset = (slant_km - moment.first_gate_km) / moment.gate_width_km;
        let nearest = offset.round();
        if (offset - nearest).abs() > 0.5 || nearest < 0.0 {
            continue;
        }
        let Ok(index) = usize::try_from(nearest as i64) else {
            continue;
        };
        let Some(GateValue::Measured(dbz)) = moment.values.get(index).copied() else {
            continue;
        };
        let Ok(height_km) = beam_height_km(slant_km, radial.elevation_deg) else {
            continue;
        };
        collected.push(Level {
            height_km,
            dbz: f64::from(dbz),
        });
    }
    collected.sort_by(|left, right| left.height_km.total_cmp(&right.height_km));
    collected
}

/// The radial a sweep collected at an azimuth number.
///
/// The number is the message's own identity for the radial, so two sweeps of
/// one volume agree on it even when their radial counts differ. A sweep that
/// did not collect this azimuth is simply absent from the column.
fn radial_at(sweep: &RadarSweep, azimuth: u16) -> Option<&RadialBins> {
    sweep
        .radials
        .iter()
        .find(|radial| radial.azimuth_number == azimuth)
}

/// Integrates liquid water through the column, in kilograms per square metre.
///
/// Each pair of neighbouring levels contributes a trapezoid; the highest level
/// contributes nothing above itself because the volume says nothing there.
fn vertically_integrated_liquid(levels: &[Level]) -> Option<f64> {
    let mut total_g_m2 = 0.0;
    let mut previous: Option<&Level> = None;
    let mut pairs = 0_usize;
    for level in levels {
        if let Some(low) = previous {
            let thickness_m = (level.height_km - low.height_km) * 1000.0;
            if thickness_m > 0.0 {
                let mean = 0.5 * (liquid_content(low.dbz) + liquid_content(level.dbz));
                total_g_m2 += mean * thickness_m;
                pairs += 1;
            }
        }
        previous = Some(level);
    }
    if pairs == 0 {
        return None;
    }
    Some(total_g_m2 / 1000.0)
}

/// The velocity of each gate with the storm's motion subtracted.
///
/// Only the drawn sweep is used: storm-relative velocity is a single-tilt
/// product, and stacking tilts into it would mix times and heights that no
/// chaser asked for.
fn storm_relative_velocity(
    volume: &RadarVolume,
    motion: StormMotion,
) -> Result<DerivedField, DerivedError> {
    let mut gates = 0_usize;
    for radial in &volume.sweep.radials {
        if let Some(moment) = radial.moment(RadarProduct::Velocity) {
            gates = gates.max(moment.values.len());
        }
    }
    if gates == 0 {
        return Err(DerivedError::NoVelocity {
            elevation: volume.sweep.elevation_number,
        });
    }
    let mut field = DerivedField::empty(volume.sweep.radials.len(), gates);
    for (row, radial) in volume.sweep.radials.iter().enumerate() {
        let Some(moment) = radial.moment(RadarProduct::Velocity) else {
            continue;
        };
        let span = (radial.azimuth_right_deg - radial.azimuth_left_deg).rem_euclid(360.0);
        let azimuth = (radial.azimuth_left_deg + span / 2.0).rem_euclid(360.0);
        let along = motion.along_beam_m_s(azimuth);
        for (gate, value) in moment.values.iter().enumerate() {
            let GateValue::Measured(velocity) = *value else {
                continue;
            };
            if let Some(slot) = row
                .checked_mul(field.gates)
                .and_then(|base| base.checked_add(gate))
                && let Some(entry) = field.values.get_mut(slot)
            {
                *entry = Some(f64::from(velocity) - along);
            }
        }
    }
    Ok(field)
}

/// The height of the highest level at or above the echo-top threshold.
fn echo_top(levels: &[Level]) -> Option<f64> {
    levels
        .iter()
        .filter(|level| level.dbz >= ECHO_TOP_THRESHOLD_DBZ)
        .map(|level| level.height_km)
        .reduce(f64::max)
}

/// Computes one derived product for the volume on the drawn sweep's grid.
///
/// # Errors
///
/// [`DerivedError::NotDerived`] for a measured moment, and
/// [`DerivedError::NoReflectivity`] when no sweep carries reflectivity to work
/// from.
pub fn evaluate(
    volume: &RadarVolume,
    product: RadarProduct,
    motion: StormMotion,
) -> Result<DerivedField, DerivedError> {
    if !product.is_derived() {
        return Err(DerivedError::NotDerived { product });
    }
    let Some(source) = product.source_moment() else {
        return Err(DerivedError::NotDerived { product });
    };
    if product == RadarProduct::StormRelativeVelocity {
        if !motion.is_usable() {
            return Err(DerivedError::StormMotion {
                speed_kmh: motion.speed_kmh,
                bearing_deg: motion.bearing_deg,
            });
        }
        return storm_relative_velocity(volume, motion);
    }
    let mut levels: Vec<&RadarSweep> = Vec::with_capacity(volume.sweeps.len() + 1);
    levels.push(&volume.sweep);
    for sweep in &volume.sweeps {
        levels.push(sweep);
    }
    levels.retain(|sweep| sweep.products.contains(&source));
    if levels.is_empty() {
        return Err(DerivedError::NoReflectivity);
    }
    let mut gates = 0_usize;
    for radial in &volume.sweep.radials {
        if let Some(moment) = radial.moment(source) {
            gates = gates.max(moment.values.len());
        }
    }
    if gates == 0 {
        return Err(DerivedError::NoReflectivity);
    }
    let mut field = DerivedField::empty(volume.sweep.radials.len(), gates);
    for (row, radial) in volume.sweep.radials.iter().enumerate() {
        let Some(moment) = radial.moment(source) else {
            continue;
        };
        let tilted: Vec<&RadialBins> = levels
            .iter()
            .filter_map(|sweep| radial_at(sweep, radial.azimuth_number))
            .collect();
        for gate in 0..moment.values.len() {
            let slant_km = moment.first_gate_km + gate as f64 * moment.gate_width_km;
            let Ok(ground_km) = projection::ground_range(slant_km, radial.elevation_deg) else {
                continue;
            };
            let levels = column(&tilted, ground_km);
            let value = match product {
                RadarProduct::VerticallyIntegratedLiquid => vertically_integrated_liquid(&levels),
                RadarProduct::EchoTops => echo_top(&levels),
                _ => None,
            };
            if let Some(value) = value
                && let Some(slot) = row
                    .checked_mul(field.gates)
                    .and_then(|base| base.checked_add(gate))
                && let Some(entry) = field.values.get_mut(slot)
            {
                *entry = Some(value);
            }
        }
    }
    Ok(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::radar::{MomentBins, RadialBins};
    use crate::stations::SiteId;
    use crate::test_support::TestError;

    /// Beam heights computed by hand from the documented formula with
    /// `R' = 8494.667 km`, which is the four-thirds Earth radius.
    #[test]
    fn beam_height_matches_the_four_thirds_earth_formula() -> Result<(), TestError> {
        for (slant_km, elevation_deg, expected_km) in [
            (100.0, 0.0, 0.588_61),
            (230.0, 0.5, 5.119_28),
            (50.0, 19.5, 16.820_84),
        ] {
            let height = beam_height_km(slant_km, elevation_deg)?;
            assert!(
                (height - expected_km).abs() < 1e-4,
                "{slant_km} km at {elevation_deg} degrees gave {height} km, expected {expected_km} km"
            );
        }
        Ok(())
    }

    #[test]
    fn a_negative_slant_range_is_rejected() {
        assert!(matches!(
            beam_height_km(-1.0, 0.5),
            Err(DerivedError::BeamHeight { .. })
        ));
    }

    fn radial(azimuth: u16, elevation_deg: f64, values: Vec<GateValue>) -> RadialBins {
        RadialBins {
            azimuth_number: azimuth,
            azimuth_left_deg: 0.0,
            azimuth_right_deg: 1.0,
            elevation_deg,
            first_gate_km: 10.0,
            gate_width_km: 1.0,
            moments: vec![MomentBins {
                product: RadarProduct::Reflectivity,
                first_gate_km: 10.0,
                gate_width_km: 1.0,
                values,
            }],
        }
    }

    fn sweep(elevation_number: u8, elevation_deg: f64, dbz: f32) -> RadarSweep {
        let values = vec![GateValue::Measured(dbz); 3];
        RadarSweep {
            elevation_deg,
            elevation_number,
            radials: vec![radial(1, elevation_deg, values)],
            products: vec![RadarProduct::Reflectivity],
            below_threshold: 0,
            range_folded: 0,
            max_ground_range_km: 12.0,
        }
    }

    fn volume(base: RadarSweep, others: Vec<RadarSweep>) -> Result<RadarVolume, TestError> {
        Ok(RadarVolume {
            site: SiteId::parse("KTWX")?,
            site_lat: 38.996_94,
            site_lon: -96.232_49,
            scan_unix_ms: 1_793_327_729_000,
            sweep: base,
            sweeps: others,
            skipped_radials: 0,
            skipped_non_data: 0,
            moment_error: None,
        })
    }

    /// Uniform 40 dBZ from 0.1 to 0.3 degrees is about two kilometres of
    /// column; `M = 3.44e-3 * 10^(40*4/70) = 0.6642 g/m3`, so a two kilometre
    /// trapezoid holds 1.328 kg of water per square metre.
    #[test]
    fn vertically_integrated_liquid_matches_the_liquid_content_formula() -> Result<(), TestError> {
        let volume = volume(sweep(1, 0.1, 40.0), vec![sweep(2, 0.3, 40.0)])?;
        let field = evaluate(
            &volume,
            RadarProduct::VerticallyIntegratedLiquid,
            StormMotion::default(),
        )?;
        let value = field
            .value(0, 0)
            .ok_or_else(|| TestError::from("the column produced no liquid"))?;
        let low = beam_height_km(10.0, 0.1)?;
        let ground = projection::ground_range(10.0, 0.1)?;
        let upper_slant = slant_for_ground_km(ground, 0.3)
            .ok_or_else(|| TestError::from("the upper beam never reaches this ground range"))?;
        let high = beam_height_km(upper_slant, 0.3)?;
        let thickness_m = (high - low) * 1000.0;
        let expected = 0.5 * liquid_content(40.0) * 2.0 * thickness_m / 1000.0;
        assert!(
            (value - expected).abs() < 1e-4,
            "VIL was {value} kg/m2, expected {expected} kg/m2"
        );
        Ok(())
    }

    /// One elevation cannot define a layer, so it must not report liquid.
    #[test]
    fn a_single_elevation_reports_no_liquid() -> Result<(), TestError> {
        let volume = volume(sweep(1, 0.5, 50.0), Vec::new())?;
        let field = evaluate(
            &volume,
            RadarProduct::VerticallyIntegratedLiquid,
            StormMotion::default(),
        )?;
        assert_eq!(field.value(0, 0), None);
        Ok(())
    }

    /// The 18.5 dBZ threshold is the point of the product: a strong low gate
    /// does not win over a weaker high one that still crosses it.
    #[test]
    fn echo_tops_report_the_highest_gate_above_the_threshold() -> Result<(), TestError> {
        let volume = volume(
            sweep(1, 0.5, 50.0),
            vec![sweep(2, 3.0, 20.0), sweep(3, 9.0, 10.0)],
        )?;
        let field = evaluate(&volume, RadarProduct::EchoTops, StormMotion::default())?;
        let value = field
            .value(0, 0)
            .ok_or_else(|| TestError::from("the column produced no echo top"))?;
        let ground = projection::ground_range(10.0, 0.5)?;
        let upper_slant =
            slant_for_ground_km(ground, 3.0).ok_or_else(|| TestError::from("no slant"))?;
        let expected = beam_height_km(upper_slant, 3.0)?;
        assert!(
            (value - expected).abs() < 1e-9,
            "echo top was {value} km, expected {expected} km"
        );
        Ok(())
    }

    #[test]
    fn a_measured_moment_is_not_a_derived_product() -> Result<(), TestError> {
        let volume = volume(sweep(1, 0.5, 50.0), Vec::new())?;
        match evaluate(&volume, RadarProduct::Reflectivity, StormMotion::default()) {
            Err(DerivedError::NotDerived { product }) => {
                assert_eq!(product, RadarProduct::Reflectivity);
                Ok(())
            }
            other => Err(TestError::from(format!(
                "expected a not-derived error, got {other:?}"
            ))),
        }
    }

    #[test]
    fn a_column_only_sees_the_elevations_that_reach_it() -> Result<(), TestError> {
        let low = sweep(1, 0.5, 50.0);
        let high = sweep(2, 9.0, 50.0);
        let levels: Vec<&RadialBins> = [radial_at(&low, 1), radial_at(&high, 1)]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(levels.len(), 2, "both elevations carry the azimuth");
        let near = column(&levels, 10.0);
        assert_eq!(near.len(), 2, "a gate both elevations reach has two levels");
        let far = column(&levels, 400.0);
        assert_eq!(far.len(), 0, "a gate no elevation reaches has none");
        assert_eq!(radial_at(&low, 42).map(|r| r.azimuth_number), None);
        Ok(())
    }

    /// A storm moving due east at 36 km/h is ten metres per second away along
    /// an east beam, across a north beam, and ten metres per second toward along
    /// a west beam.
    #[test]
    fn storm_motion_projects_onto_each_beam() -> Result<(), TestError> {
        let east = StormMotion {
            speed_kmh: 36.0,
            bearing_deg: 90.0,
        };
        assert!((east.along_beam_m_s(90.0) - 10.0).abs() < 1e-9);
        assert!(east.along_beam_m_s(0.0).abs() < 1e-9);
        assert!((east.along_beam_m_s(270.0) + 10.0).abs() < 1e-9);
        let back = StormMotion {
            speed_kmh: 36.0,
            bearing_deg: 270.0,
        };
        assert!((back.along_beam_m_s(90.0) + 10.0).abs() < 1e-9);
        assert!(east.is_usable());
        Ok(())
    }

    /// A storm motion the interface cannot use is refused, not silently
    /// approximated.
    #[test]
    fn unusable_storm_motions_are_rejected() {
        for motion in [
            StormMotion {
                speed_kmh: -5.0,
                bearing_deg: 90.0,
            },
            StormMotion {
                speed_kmh: 5.0,
                bearing_deg: 360.0,
            },
            StormMotion {
                speed_kmh: f64::NAN,
                bearing_deg: 90.0,
            },
            StormMotion {
                speed_kmh: MAX_STORM_SPEED_KMH + 1.0,
                bearing_deg: 90.0,
            },
        ] {
            assert!(!motion.is_usable(), "{motion:?} should be refused");
        }
        assert!(StormMotion::default().is_usable());
    }

    fn velocity_radial(azimuth_deg: f64, velocity: f32) -> RadialBins {
        let mut radial = radial(1, 0.5, vec![GateValue::Measured(50.0); 3]);
        radial.azimuth_left_deg = azimuth_deg - 0.5;
        radial.azimuth_right_deg = azimuth_deg + 0.5;
        radial.moments.push(MomentBins {
            product: RadarProduct::Velocity,
            first_gate_km: 10.0,
            gate_width_km: 1.0,
            values: vec![GateValue::Measured(velocity); 3],
        });
        radial
    }

    /// Measured velocity is relative to the radar; subtracting the storm's own
    /// motion along each beam is what turns a broad inbound/outbound field into
    /// a couplet a chaser can read.
    #[test]
    fn storm_relative_velocity_removes_the_storm_motion() -> Result<(), TestError> {
        let mut base = sweep(1, 0.5, 50.0);
        base.radials = vec![velocity_radial(90.0, 10.0)];
        base.products = vec![RadarProduct::Reflectivity, RadarProduct::Velocity];
        let volume = volume(base, Vec::new())?;
        let help = StormMotion {
            speed_kmh: 36.0,
            bearing_deg: 90.0,
        };
        let field = evaluate(&volume, RadarProduct::StormRelativeVelocity, help)?;
        let value = field
            .value(0, 0)
            .ok_or_else(|| TestError::from("the column produced no storm-relative value"))?;
        assert!((value - 0.0).abs() < 1e-9, "storm-relative was {value} m/s");

        let still = evaluate(
            &volume,
            RadarProduct::StormRelativeVelocity,
            StormMotion::default(),
        )?;
        let measured = still
            .value(0, 0)
            .ok_or_else(|| TestError::from("a still storm should report the measured value"))?;
        assert!((measured - 10.0).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn storm_relative_velocity_needs_a_velocity_sweep() -> Result<(), TestError> {
        let volume = volume(sweep(1, 0.5, 50.0), Vec::new())?;
        match evaluate(
            &volume,
            RadarProduct::StormRelativeVelocity,
            StormMotion::default(),
        ) {
            Err(DerivedError::NoVelocity { elevation }) => {
                assert_eq!(elevation, 1);
                Ok(())
            }
            other => Err(TestError::from(format!(
                "expected a missing velocity error, got {other:?}"
            ))),
        }
    }

    #[test]
    fn storm_relative_velocity_refuses_an_unusable_motion() -> Result<(), TestError> {
        let mut base = sweep(1, 0.5, 50.0);
        base.radials = vec![velocity_radial(90.0, 10.0)];
        base.products = vec![RadarProduct::Reflectivity, RadarProduct::Velocity];
        let volume = volume(base, Vec::new())?;
        let bad = StormMotion {
            speed_kmh: 400.0,
            bearing_deg: 90.0,
        };
        match evaluate(&volume, RadarProduct::StormRelativeVelocity, bad) {
            Err(DerivedError::StormMotion { speed_kmh, .. }) => {
                assert!((speed_kmh - 400.0).abs() < 1e-9);
                Ok(())
            }
            other => Err(TestError::from(format!(
                "expected a motion error, got {other:?}"
            ))),
        }
    }

    /// The same ground range on a higher beam takes a longer path along the
    /// beam, and arrives most of a kilometre higher: at ten kilometres out, a
    /// nine-degree beam is 10.13 km along its own path.
    #[test]
    fn a_higher_beam_reaches_a_ground_range_farther_in_slant() -> Result<(), TestError> {
        let flatter = slant_for_ground_km(10.0, 0.5).ok_or_else(|| TestError::from("no slant"))?;
        let steeper = slant_for_ground_km(10.0, 9.0).ok_or_else(|| TestError::from("no slant"))?;
        assert!((flatter - 10.0004).abs() < 1e-3, "flat slant {flatter}");
        assert!((steeper - 10.1266).abs() < 1e-3, "steep slant {steeper}");
        assert!(steeper > flatter, "{steeper} should be over {flatter}");
        let flat_height = beam_height_km(flatter, 0.5)?;
        let steep_height = beam_height_km(steeper, 9.0)?;
        assert!(steep_height > flat_height + 1.0);
        // A ground range that is not a distance has no beam that reaches it, and
        // the inverse refuses rather than inventing a slant range.
        assert_eq!(slant_for_ground_km(-5.0, 0.5), None);
        assert_eq!(slant_for_ground_km(f64::NAN, 0.5), None);
        assert_eq!(slant_for_ground_km(10.0, f64::INFINITY), None);
        Ok(())
    }

    #[test]
    fn an_impossible_height_is_an_error() {
        assert!(matches!(
            beam_height_km(f64::NAN, 0.5),
            Err(DerivedError::BeamHeight { .. })
        ));
        assert!(matches!(
            beam_height_km(100.0, f64::INFINITY),
            Err(DerivedError::BeamHeight { .. })
        ));
    }

    #[test]
    fn a_missing_reflectivity_sweep_is_reported() -> Result<(), TestError> {
        let mut base = sweep(1, 0.5, 50.0);
        base.products.clear();
        base.radials.clear();
        let volume = volume(base, Vec::new())?;
        match evaluate(&volume, RadarProduct::EchoTops, StormMotion::default()) {
            Err(DerivedError::NoReflectivity) => Ok(()),
            other => Err(TestError::from(format!(
                "expected a missing reflectivity error, got {other:?}"
            ))),
        }
    }

    #[test]
    fn a_field_index_outside_the_grid_has_no_value() {
        let field = DerivedField::empty(2, 3);
        assert_eq!(field.value(1, 2), None);
        assert_eq!(field.value(2, 0), None);
        assert_eq!(field.value(usize::MAX, usize::MAX), None);
    }
}
