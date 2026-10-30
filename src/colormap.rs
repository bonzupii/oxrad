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
//! The reflectivity palette.

use crate::error::ColorMapError;

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
    fn an_out_of_range_value_carries_itself() {
        let result = color_for_reflectivity(95.0);
        assert!(
            matches!(result, Err(ColorMapError::OutOfRange { product, value }) if product == PRODUCT && value == 95.0),
            "expected OutOfRange carrying 95.0, got {result:?}"
        );
    }
}
