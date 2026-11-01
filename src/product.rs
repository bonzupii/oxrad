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

/// A base or dual-polarization radar product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RadarProduct {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    CorrelationCoefficient,
    DifferentialPhase,
    SpecificDifferentialPhase,
}

impl RadarProduct {
    pub const ALL: [Self; 7] = [
        Self::Reflectivity,
        Self::Velocity,
        Self::SpectrumWidth,
        Self::DifferentialReflectivity,
        Self::CorrelationCoefficient,
        Self::DifferentialPhase,
        Self::SpecificDifferentialPhase,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Reflectivity => "DREF",
            Self::Velocity => "DVEL",
            Self::SpectrumWidth => "DSW",
            Self::DifferentialReflectivity => "DZDR",
            Self::CorrelationCoefficient => "DRHO",
            Self::DifferentialPhase => "DPHI",
            Self::SpecificDifferentialPhase => "DKDP",
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
    /// ranges.
    pub const fn value_range(self) -> (f32, f32) {
        match self {
            Self::Reflectivity => (5.0, 94.5),
            Self::Velocity => (-64.0, 64.0),
            Self::SpectrumWidth => (0.0, 64.0),
            Self::DifferentialReflectivity => (-8.0, 8.0),
            Self::CorrelationCoefficient => (0.0, 1.1),
            Self::DifferentialPhase => (0.0, 360.0),
            Self::SpecificDifferentialPhase => (-32.0, 32.0),
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
        for product in RadarProduct::ALL {
            let (minimum, maximum) = product.value_range();
            assert!(minimum < maximum, "{} range", product.id());
            assert!(!product.unit().is_empty(), "{} unit", product.id());
        }
    }
}
