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
//! Radar products independent of any one source's data format.

/// A radar product.
///
/// The first seven are the moments the radar measures and sends; the last two
/// are computed from a whole volume by [`crate::derived`] and are never read
/// out of a message. They share this type because they share everything a
/// product needs to be selected, drawn, labelled and read out; [`Self::is_derived`]
/// keeps the two kinds apart wherever that difference matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RadarProduct {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    CorrelationCoefficient,
    DifferentialPhase,
    SpecificDifferentialPhase,
    VerticallyIntegratedLiquid,
    EchoTops,
    StormRelativeVelocity,
}

impl RadarProduct {
    /// The moments a radar measures, in the order the suite is listed.
    pub const ALL: [Self; 7] = [
        Self::Reflectivity,
        Self::Velocity,
        Self::SpectrumWidth,
        Self::DifferentialReflectivity,
        Self::CorrelationCoefficient,
        Self::DifferentialPhase,
        Self::SpecificDifferentialPhase,
    ];

    /// The products computed from decoded data rather than decoded themselves.
    pub const DERIVED: [Self; 3] = [
        Self::VerticallyIntegratedLiquid,
        Self::EchoTops,
        Self::StormRelativeVelocity,
    ];

    /// Whether this product is computed instead of decoded.
    pub const fn is_derived(self) -> bool {
        matches!(
            self,
            Self::VerticallyIntegratedLiquid | Self::EchoTops | Self::StormRelativeVelocity
        )
    }

    /// The measured moment a computed product is built from.
    ///
    /// Liquid and echo tops stack the elevations' reflectivity; storm-relative
    /// velocity removes the storm's own motion from the drawn sweep's velocity.
    pub const fn source_moment(self) -> Option<Self> {
        match self {
            Self::VerticallyIntegratedLiquid | Self::EchoTops => Some(Self::Reflectivity),
            Self::StormRelativeVelocity => Some(Self::Velocity),
            _ => None,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Reflectivity => "DREF",
            Self::Velocity => "DVEL",
            Self::SpectrumWidth => "DSW",
            Self::DifferentialReflectivity => "DZDR",
            Self::CorrelationCoefficient => "DRHO",
            Self::DifferentialPhase => "DPHI",
            Self::SpecificDifferentialPhase => "DKDP",
            Self::VerticallyIntegratedLiquid => "VIL",
            Self::EchoTops => "ETOP",
            Self::StormRelativeVelocity => "SRV",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Reflectivity => "Base reflectivity",
            Self::Velocity => "Base velocity",
            Self::SpectrumWidth => "Spectrum width",
            Self::DifferentialReflectivity => "Differential reflectivity",
            Self::CorrelationCoefficient => "Correlation coefficient",
            Self::DifferentialPhase => "Differential phase",
            Self::SpecificDifferentialPhase => "Specific differential phase",
            Self::VerticallyIntegratedLiquid => "Vertically integrated liquid",
            Self::EchoTops => "Echo tops above the radar",
            Self::StormRelativeVelocity => "Storm-relative velocity",
        }
    }

    pub const fn unit(self) -> &'static str {
        match self {
            Self::Reflectivity => "dBZ",
            Self::Velocity | Self::SpectrumWidth => "m/s",
            Self::DifferentialReflectivity => "dB",
            Self::CorrelationCoefficient => "ratio",
            Self::DifferentialPhase => "deg",
            Self::SpecificDifferentialPhase => "deg/km",
            Self::VerticallyIntegratedLiquid => "kg/m²",
            Self::EchoTops => "km",
            Self::StormRelativeVelocity => "m/s",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Reflectivity => "Precipitation intensity from returned power.",
            Self::Velocity => "Motion toward or away from the radar along each beam.",
            Self::SpectrumWidth => "Spread of velocities inside each gate.",
            Self::DifferentialReflectivity => "Horizontal minus vertical reflectivity.",
            Self::CorrelationCoefficient => "Similarity of horizontal and vertical returns.",
            Self::DifferentialPhase => "Phase shift between horizontal and vertical returns.",
            Self::SpecificDifferentialPhase => "Range derivative of differential phase.",
            Self::VerticallyIntegratedLiquid => {
                "Liquid water in the column, integrated from every elevation of this volume."
            }
            Self::EchoTops => "Height above the radar of the highest gate at or above 18.5 dBZ.",
            Self::StormRelativeVelocity => {
                "Velocity with the storm's own motion removed, so rotation shows as a couplet."
            }
        }
    }

    pub const fn is_dual_polarization(self) -> bool {
        matches!(
            self,
            Self::DifferentialReflectivity
                | Self::CorrelationCoefficient
                | Self::DifferentialPhase
                | Self::SpecificDifferentialPhase
        )
    }

    /// The scale this product is drawn and labelled on.
    ///
    /// Each end is the limit of what the source format can encode for the
    /// product, so ordinary data is never classified as off-scale. A
    /// measurement beyond an end is drawn at that end's colour and counted in
    /// the interface; the scale is a display range, not a filter on the data.
    /// The reflectivity floor is the five decibels the NWS uses for base
    /// reflectivity: below it the receiver reports noise and ground clutter
    /// rather than precipitation, so those gates are left out and counted. The
    /// top is the end of the two-decibel code. Velocity and spectrum width
    /// cover the unambiguous-plus-folded span of a two-decibel code at a 129
    /// offset, and the dual-polarization products use their meteorological
    /// ranges. Vertically integrated liquid starts at one kilogram per square
    /// metre: below that a column holds less water than a millimetre of rain
    /// spread over a square metre, which is not worth painting over the map.
    /// Echo tops start at the ground and stop at eighteen kilometres, above
    /// which a thunderstorm top no longer exists.
    pub const fn value_range(self) -> (f32, f32) {
        match self {
            Self::Reflectivity => (5.0, 94.5),
            Self::Velocity => (-64.0, 64.0),
            Self::SpectrumWidth => (0.0, 64.0),
            Self::DifferentialReflectivity => (-8.0, 8.0),
            Self::CorrelationCoefficient => (0.0, 1.1),
            Self::DifferentialPhase => (0.0, 360.0),
            Self::SpecificDifferentialPhase => (-32.0, 32.0),
            Self::VerticallyIntegratedLiquid => (1.0, 80.0),
            Self::EchoTops => (0.0, 18.0),
            Self::StormRelativeVelocity => (-64.0, 64.0),
        }
    }

    /// The scale in the notation the legend prints.
    pub const fn scale_label(self) -> &'static str {
        match self {
            Self::CorrelationCoefficient => "0.00 to 1.10",
            Self::Reflectivity => "5 to 94.5",
            Self::Velocity => "-64 to 64",
            Self::SpectrumWidth => "0 to 64",
            Self::DifferentialReflectivity => "-8 to 8",
            Self::DifferentialPhase => "0 to 360",
            Self::SpecificDifferentialPhase => "-32 to 32",
            Self::VerticallyIntegratedLiquid => "1 to 80",
            Self::EchoTops => "0 to 18",
            Self::StormRelativeVelocity => "-64 to 64",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_cover_the_dual_polarization_suite() {
        let identifiers: Vec<&str> = RadarProduct::ALL
            .iter()
            .map(|product| product.id())
            .collect();
        assert_eq!(
            identifiers,
            vec!["DREF", "DVEL", "DSW", "DZDR", "DRHO", "DPHI", "DKDP"]
        );
        assert!(RadarProduct::DifferentialReflectivity.is_dual_polarization());
        assert!(!RadarProduct::Velocity.is_dual_polarization());
    }

    #[test]
    fn every_product_has_a_usable_range() {
        for product in RadarProduct::ALL.iter().chain(RadarProduct::DERIVED.iter()) {
            let (minimum, maximum) = product.value_range();
            assert!(minimum < maximum, "{} range", product.id());
            assert!(!product.unit().is_empty(), "{} unit", product.id());
        }
    }

    #[test]
    fn derived_products_are_not_moments() {
        let identifiers: Vec<&str> = RadarProduct::DERIVED.iter().map(|p| p.id()).collect();
        assert_eq!(identifiers, vec!["VIL", "ETOP", "SRV"]);
        assert!(RadarProduct::VerticallyIntegratedLiquid.is_derived());
        assert!(RadarProduct::EchoTops.is_derived());
        for product in RadarProduct::ALL {
            assert!(!product.is_derived(), "{}", product.id());
        }
    }
}
