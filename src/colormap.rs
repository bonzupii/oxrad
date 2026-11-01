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
//! Product color scales.

use crate::error::ColorMapError;
use crate::product::RadarProduct;

const KEYPOINTS: [(f32, [u8; 3]); 20] = [
    (-20.0, [50, 79, 79]),
    (-15.0, [102, 139, 139]),
    (-10.0, [150, 205, 205]),
    (-5.0, [174, 238, 238]),
    (0.0, [187, 255, 255]),
    (5.0, [0, 236, 236]),
    (10.0, [1, 160, 246]),
    (15.0, [0, 0, 246]),
    (20.0, [0, 255, 0]),
    (25.0, [0, 200, 0]),
    (30.0, [0, 144, 0]),
    (35.0, [255, 255, 0]),
    (40.0, [231, 192, 0]),
    (45.0, [255, 144, 0]),
    (50.0, [255, 0, 0]),
    (55.0, [214, 0, 0]),
    (60.0, [192, 0, 0]),
    (65.0, [255, 0, 255]),
    (70.0, [153, 85, 201]),
    (75.0, [235, 235, 235]),
];

const RANGE_FOLDED: [u8; 4] = [128, 0, 255, 255];
const PRODUCT: &str = "BaseReflectivity";
pub const MIN_REFLECTIVITY_DBZ: f32 = -32.0;
pub const MAX_REFLECTIVITY_DBZ: f32 = 94.5;

/// A radar color table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorTable {
    /// The NWS-style table used by default.
    #[default]
    Standard,
    /// A blue-to-yellow ramp that does not rely on the red-green axis.
    Deuteranopia,
    /// A blue-to-yellow ramp tuned for reduced red sensitivity.
    Protanopia,
    /// A yellow-to-red ramp that avoids the blue-yellow axis.
    Tritanopia,
    /// A lightness-only ramp for severe color-vision loss and grayscale output.
    HighContrast,
}

impl ColorTable {
    /// Every selectable table, in menu order.
    pub const ALL: [Self; 5] = [
        Self::Standard,
        Self::Deuteranopia,
        Self::Protanopia,
        Self::Tritanopia,
        Self::HighContrast,
    ];

    /// The control label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Deuteranopia => "Deuteranopia",
            Self::Protanopia => "Protanopia",
            Self::Tritanopia => "Tritanopia",
            Self::HighContrast => "High contrast",
        }
    }

    /// The configuration token written to disk.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Deuteranopia => "deuteranopia",
            Self::Protanopia => "protanopia",
            Self::Tritanopia => "tritanopia",
            Self::HighContrast => "high-contrast",
        }
    }

    /// Parses a configuration token.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "standard" => Some(Self::Standard),
            "deuteranopia" => Some(Self::Deuteranopia),
            "protanopia" => Some(Self::Protanopia),
            "tritanopia" => Some(Self::Tritanopia),
            "high-contrast" => Some(Self::HighContrast),
            _ => None,
        }
    }

    /// The accessibility use this table addresses.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Standard => "Familiar NWS-style reflectivity colors.",
            Self::Deuteranopia => "Blue-to-yellow ramp that stays ordered with deuteranopia.",
            Self::Protanopia => "Blue-to-yellow ramp that stays ordered with protanopia.",
            Self::Tritanopia => "Yellow-to-red ramp that stays ordered with tritanopia.",
            Self::HighContrast => {
                "Lightness-only ramp for severe color-vision loss and grayscale output."
            }
        }
    }
}

const DEUTERANOPIA_RAMP: [(f32, [u8; 3]); 9] = [
    (0.0, [16, 38, 84]),
    (0.125, [22, 64, 130]),
    (0.25, [28, 96, 165]),
    (0.375, [40, 130, 180]),
    (0.5, [70, 165, 170]),
    (0.625, [125, 195, 120]),
    (0.75, [195, 215, 60]),
    (0.875, [240, 235, 80]),
    (1.0, [255, 252, 200]),
];
const PROTANOPIA_RAMP: [(f32, [u8; 3]); 9] = [
    (0.0, [18, 26, 52]),
    (0.125, [24, 58, 100]),
    (0.25, [30, 94, 140]),
    (0.375, [50, 130, 158]),
    (0.5, [90, 165, 150]),
    (0.625, [150, 196, 110]),
    (0.75, [212, 220, 80]),
    (0.875, [246, 238, 110]),
    (1.0, [255, 253, 220]),
];
const TRITANOPIA_RAMP: [(f32, [u8; 3]); 9] = [
    (0.0, [30, 25, 30]),
    (0.125, [70, 30, 38]),
    (0.25, [110, 40, 45]),
    (0.375, [145, 55, 42]),
    (0.5, [180, 70, 45]),
    (0.625, [210, 105, 42]),
    (0.75, [240, 150, 50]),
    (0.875, [252, 205, 80]),
    (1.0, [255, 245, 190]),
];
const HIGH_CONTRAST_RAMP: [(f32, [u8; 3]); 9] = [
    (0.0, [72, 72, 72]),
    (0.125, [88, 88, 88]),
    (0.25, [108, 108, 108]),
    (0.375, [128, 128, 128]),
    (0.5, [150, 150, 150]),
    (0.625, [172, 172, 172]),
    (0.75, [198, 198, 198]),
    (0.875, [224, 224, 224]),
    (1.0, [255, 255, 255]),
];

const fn with_alpha(rgb: [u8; 3]) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], 255]
}

fn interpolate(low: [u8; 3], high: [u8; 3], fraction: f32) -> [u8; 4] {
    let channel = |low_channel: u8, high_channel: u8| {
        let value =
            f32::from(low_channel) + (f32::from(high_channel) - f32::from(low_channel)) * fraction;
        value.round().clamp(0.0, 255.0) as u8
    };
    [
        channel(low[0], high[0]),
        channel(low[1], high[1]),
        channel(low[2], high[2]),
        255,
    ]
}

pub fn color_for_reflectivity(dbz: f32) -> Result<[u8; 4], ColorMapError> {
    if !(MIN_REFLECTIVITY_DBZ..=MAX_REFLECTIVITY_DBZ).contains(&dbz) {
        return Err(ColorMapError::OutOfRange {
            product: PRODUCT,
            value: dbz,
        });
    }
    let mut previous: Option<(f32, [u8; 3])> = None;
    for (value, rgb) in KEYPOINTS {
        match previous {
            None => {
                if dbz < value {
                    return Ok(with_alpha(rgb));
                }
            }
            Some((low, low_rgb)) => {
                if dbz < value {
                    let fraction = (dbz - low) / (value - low);
                    return Ok(interpolate(low_rgb, rgb, fraction));
                }
            }
        }
        previous = Some((value, rgb));
    }
    previous.map_or_else(
        || {
            Err(ColorMapError::OutOfRange {
                product: PRODUCT,
                value: dbz,
            })
        },
        |(_, rgb)| Ok(with_alpha(rgb)),
    )
}

const VELOCITY_RAMP: [(f32, [u8; 3]); 5] = [
    (-64.0, [24, 48, 120]),
    (-20.0, [40, 110, 210]),
    (0.0, [235, 235, 235]),
    (20.0, [214, 126, 24]),
    (64.0, [150, 24, 24]),
];
const ACCESSIBLE_VELOCITY_RAMP: [(f32, [u8; 3]); 5] = [
    (-64.0, [10, 25, 70]),
    (-20.0, [30, 100, 180]),
    (0.0, [240, 240, 240]),
    (20.0, [220, 130, 20]),
    (64.0, [120, 40, 10]),
];
const SPECTRUM_WIDTH_RAMP: [(f32, [u8; 3]); 4] = [
    (0.0, [30, 30, 38]),
    (10.0, [90, 70, 130]),
    (30.0, [190, 130, 90]),
    (64.0, [245, 245, 220]),
];
const ZDR_RAMP: [(f32, [u8; 3]); 5] = [
    (-8.0, [50, 60, 160]),
    (-2.0, [110, 150, 220]),
    (0.0, [210, 210, 210]),
    (3.0, [240, 180, 80]),
    (8.0, [210, 60, 40]),
];
const CC_RAMP: [(f32, [u8; 3]); 4] = [
    (0.0, [28, 28, 32]),
    (0.5, [100, 70, 150]),
    (0.9, [180, 180, 210]),
    (1.1, [250, 250, 250]),
];
const PHI_RAMP: [(f32, [u8; 3]); 3] = [
    (0.0, [35, 70, 170]),
    (180.0, [235, 235, 235]),
    (360.0, [150, 40, 160]),
];
/// Vertically integrated liquid, in kilograms per square metre. The stops
/// follow the operational scale: light green for a shallow column, magenta for
/// the eighty-plus columns that carry large hail.
const VIL_RAMP: [(f32, [u8; 3]); 7] = [
    (1.0, [30, 122, 70]),
    (5.0, [64, 176, 88]),
    (15.0, [196, 200, 60]),
    (30.0, [226, 138, 40]),
    (45.0, [206, 52, 40]),
    (60.0, [176, 44, 128]),
    (80.0, [238, 232, 250]),
];
/// Echo tops, in kilometres above the radar: low cloud stays blue, and the
/// deep convection that carries hail and tornadoes reaches white and red.
const ECHO_TOP_RAMP: [(f32, [u8; 3]); 6] = [
    (0.0, [54, 112, 176]),
    (3.0, [86, 168, 226]),
    (6.0, [92, 196, 196]),
    (9.0, [196, 214, 120]),
    (12.0, [226, 140, 60]),
    (18.0, [236, 236, 244]),
];

const KDP_RAMP: [(f32, [u8; 3]); 3] = [
    (-32.0, [40, 80, 170]),
    (0.0, [225, 225, 225]),
    (32.0, [200, 40, 40]),
];

fn ramp_color(
    ramp: &[(f32, [u8; 3])],
    product: &'static str,
    value: f32,
    minimum: f32,
    maximum: f32,
) -> Result<[u8; 4], ColorMapError> {
    if !value.is_finite() || !(minimum..=maximum).contains(&value) {
        return Err(ColorMapError::OutOfRange { product, value });
    }
    let mut previous: Option<(f32, [u8; 3])> = None;
    for (key, rgb) in ramp {
        match previous {
            None => {
                if value < *key {
                    return Ok(with_alpha(*rgb));
                }
            }
            Some((low, low_rgb)) => {
                if value < *key {
                    let fraction = (value - low) / (*key - low);
                    return Ok(interpolate(low_rgb, *rgb, fraction));
                }
            }
        }
        previous = Some((*key, *rgb));
    }
    previous.map_or_else(
        || Err(ColorMapError::OutOfRange { product, value }),
        |(_, rgb)| Ok(with_alpha(rgb)),
    )
}

/// The color for a value of the selected product in the standard table.
pub fn color_for_product(product: RadarProduct, value: f32) -> Result<[u8; 4], ColorMapError> {
    if product == RadarProduct::Reflectivity {
        return color_for_reflectivity(value);
    }
    let (minimum, maximum) = product.value_range();
    let ramp: &[(f32, [u8; 3])] = match product {
        RadarProduct::Velocity => &VELOCITY_RAMP,
        RadarProduct::SpectrumWidth => &SPECTRUM_WIDTH_RAMP,
        RadarProduct::DifferentialReflectivity => &ZDR_RAMP,
        RadarProduct::CorrelationCoefficient => &CC_RAMP,
        RadarProduct::DifferentialPhase => &PHI_RAMP,
        RadarProduct::SpecificDifferentialPhase => &KDP_RAMP,
        RadarProduct::VerticallyIntegratedLiquid => &VIL_RAMP,
        RadarProduct::EchoTops => &ECHO_TOP_RAMP,
        // Storm-relative velocity keeps the velocity palette: the colours mean
        // toward and away from the radar there too.
        RadarProduct::StormRelativeVelocity => &VELOCITY_RAMP,
        RadarProduct::Reflectivity => &KEYPOINTS,
    };
    ramp_color(ramp, product.id(), value, minimum, maximum)
}

const fn table_ramp(table: ColorTable) -> &'static [(f32, [u8; 3])] {
    match table {
        ColorTable::Standard => &KEYPOINTS,
        ColorTable::Deuteranopia => &DEUTERANOPIA_RAMP,
        ColorTable::Protanopia => &PROTANOPIA_RAMP,
        ColorTable::Tritanopia => &TRITANOPIA_RAMP,
        ColorTable::HighContrast => &HIGH_CONTRAST_RAMP,
    }
}

fn ramp_at(ramp: &[(f32, [u8; 3])], fraction: f32) -> [u8; 4] {
    let fraction = fraction.clamp(0.0, 1.0);
    let mut previous: Option<(f32, [u8; 3])> = None;
    for (key, rgb) in ramp {
        match previous {
            None => {
                if fraction <= *key {
                    return with_alpha(*rgb);
                }
            }
            Some((low, low_rgb)) => {
                if fraction <= *key {
                    let span = *key - low;
                    let local = if span > 0.0 {
                        (fraction - low) / span
                    } else {
                        0.0
                    };
                    return interpolate(low_rgb, *rgb, local);
                }
            }
        }
        previous = Some((*key, *rgb));
    }
    ramp.last()
        .map_or(RANGE_FOLDED, |(_, rgb)| with_alpha(*rgb))
}

/// The color for a value of the selected product in a selected table.
pub fn color_for_product_with(
    table: ColorTable,
    product: RadarProduct,
    value: f32,
) -> Result<[u8; 4], ColorMapError> {
    if table == ColorTable::Standard {
        return color_for_product(product, value);
    }
    let (minimum, maximum) = product.value_range();
    if !value.is_finite() || !(minimum..=maximum).contains(&value) {
        return Err(ColorMapError::OutOfRange {
            product: product.id(),
            value,
        });
    }
    if product == RadarProduct::Velocity {
        return ramp_color(
            &ACCESSIBLE_VELOCITY_RAMP,
            product.id(),
            value,
            minimum,
            maximum,
        );
    }
    let fraction = (value - minimum) / (maximum - minimum);
    Ok(ramp_at(table_ramp(table), fraction))
}

/// A display colour, and whether the measurement fell outside the scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappedColor {
    /// The value sits inside the product's scale.
    Inside([u8; 4]),
    /// The value is above the top of the scale and takes its top colour.
    ClippedAbove([u8; 4]),
    /// The value is below the bottom of the scale and has no colour.
    ///
    /// The bottom of a product's scale is where its noise floor sits: a
    /// reflectivity return weaker than five decibels is receiver noise and
    /// ground clutter, not precipitation, and painting it would bury the storm
    /// under a wash of speckle. The gates are counted so the interface can say
    /// how many were left out.
    BelowScale,
}

impl MappedColor {
    /// The colour to draw, or nothing when the value is below the scale.
    pub const fn colour(self) -> Option<[u8; 4]> {
        match self {
            Self::Inside(colour) | Self::ClippedAbove(colour) => Some(colour),
            Self::BelowScale => None,
        }
    }
}

/// The colour for a measurement that must be drawn.
///
/// A measurement outside the product scale is drawn at the scale's end colour
/// and reported as clipped. That is a scale limit, not missing data: a fifty
/// decibel echo above a fifty decibel scale is still a fifty decibel echo. A
/// value that is not a finite number cannot be drawn at all and is an error.
///
/// # Errors
///
/// [`ColorMapError::OutOfRange`] when the value is NaN or infinite.
pub fn map_display_color(
    table: ColorTable,
    product: RadarProduct,
    value: f32,
) -> Result<MappedColor, ColorMapError> {
    if !value.is_finite() {
        return Err(ColorMapError::OutOfRange {
            product: product.id(),
            value,
        });
    }
    let (minimum, maximum) = product.value_range();
    if value > maximum {
        return Ok(MappedColor::ClippedAbove(color_for_product_with(
            table, product, maximum,
        )?));
    }
    if value < minimum {
        return Ok(MappedColor::BelowScale);
    }
    Ok(MappedColor::Inside(color_for_product_with(
        table, product, value,
    )?))
}

pub const fn range_folded_color() -> [u8; 4] {
    RANGE_FOLDED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    enum TestFailure {
        ColorMap(ColorMapError),
        Color {
            dbz: f32,
            expected: [u8; 4],
            actual: [u8; 4],
        },
    }

    impl From<ColorMapError> for TestFailure {
        fn from(source: ColorMapError) -> Self {
            Self::ColorMap(source)
        }
    }

    impl std::fmt::Display for TestFailure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::ColorMap(error) => write!(formatter, "{error}"),
                Self::Color {
                    dbz,
                    expected,
                    actual,
                } => write!(
                    formatter,
                    "reflectivity {dbz} dBZ: expected {expected:?}, got {actual:?}"
                ),
            }
        }
    }

    fn expect_color(dbz: f32, expected: [u8; 4]) -> Result<(), TestFailure> {
        let actual = color_for_reflectivity(dbz)?;
        if actual == expected {
            Ok(())
        } else {
            Err(TestFailure::Color {
                dbz,
                expected,
                actual,
            })
        }
    }

    #[test]
    fn keypoints_map_to_palette_values() -> Result<(), TestFailure> {
        for (dbz, expected) in [
            (-15.0, [102, 139, 139, 255]),
            (0.0, [187, 255, 255, 255]),
            (20.0, [0, 255, 0, 255]),
            (40.0, [231, 192, 0, 255]),
            (55.0, [214, 0, 0, 255]),
        ] {
            expect_color(dbz, expected)?;
        }
        Ok(())
    }

    #[test]
    fn interpolation_between_keypoints() -> Result<(), TestFailure> {
        expect_color(32.0, [102, 188, 0, 255])
    }

    #[test]
    fn values_outside_the_keypoints_take_the_end_colours() -> Result<(), TestFailure> {
        expect_color(-31.0, [50, 79, 79, 255])?;
        expect_color(80.0, [235, 235, 235, 255])
    }

    #[test]
    fn range_folded_maps_to_its_sentinel_colour() {
        assert_eq!(range_folded_color(), RANGE_FOLDED);
    }

    #[test]
    fn a_nan_measurement_is_out_of_range_not_range_folded() {
        assert!(matches!(
            color_for_reflectivity(f32::NAN),
            Err(ColorMapError::OutOfRange { product, value }) if product == PRODUCT && value.is_nan()
        ));
    }

    #[test]
    fn every_product_produces_a_color_inside_its_range() -> Result<(), TestFailure> {
        for product in RadarProduct::ALL {
            let (minimum, maximum) = product.value_range();
            let middle = minimum + (maximum - minimum) / 2.0;
            let color = color_for_product(product, middle)?;
            assert_eq!(color[3], 255, "{} alpha", product.id());
        }
        Ok(())
    }

    #[test]
    fn a_product_out_of_range_carries_its_name() {
        assert!(matches!(
            color_for_product(RadarProduct::Velocity, 200.0),
            Err(ColorMapError::OutOfRange { product: "DVEL", value }) if value == 200.0
        ));
    }

    #[test]
    fn an_out_of_range_value_carries_itself() {
        let result = color_for_reflectivity(95.0);
        assert!(
            matches!(result, Err(ColorMapError::OutOfRange { product, value }) if product == PRODUCT && value == 95.0),
            "expected OutOfRange carrying 95.0, got {result:?}"
        );
    }

    #[test]
    fn table_tokens_round_trip_and_reject_unknown_values() {
        for table in ColorTable::ALL {
            assert_eq!(ColorTable::parse(table.token()), Some(table));
        }
        assert_eq!(ColorTable::parse("viridis"), None);
    }

    #[test]
    fn every_accessible_table_keeps_reflectivity_steps_distinct() -> Result<(), TestFailure> {
        for table in ColorTable::ALL {
            let mut previous: Option<[u8; 4]> = None;
            for dbz in [5.0_f32, 15.0, 30.0, 45.0, 60.0, 85.0] {
                let color = color_for_product_with(table, RadarProduct::Reflectivity, dbz)?;
                if let Some(previous) = previous {
                    assert_ne!(previous, color, "{} at {dbz} dBZ", table.label());
                }
                previous = Some(color);
            }
        }
        Ok(())
    }

    #[test]
    fn high_contrast_uses_equal_channels() -> Result<(), TestFailure> {
        for dbz in [5.0_f32, 20.0, 40.0, 60.0, 85.0] {
            let color =
                color_for_product_with(ColorTable::HighContrast, RadarProduct::Reflectivity, dbz)?;
            assert!(
                color[0] == color[1] && color[1] == color[2],
                "{dbz} dBZ is not gray: {color:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn accessible_tables_reject_out_of_range_values() {
        assert!(matches!(
            color_for_product_with(ColorTable::Deuteranopia, RadarProduct::Reflectivity, 95.0),
            Err(ColorMapError::OutOfRange { product, value })
                if product == RadarProduct::Reflectivity.id() && value == 95.0
        ));
    }
}
