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
const MIN_DBZ: f32 = -32.0;
const MAX_DBZ: f32 = 94.5;

const fn with_alpha(rgb: [u8; 3]) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], 255]
}

fn interpolate(low: [u8; 3], high: [u8; 3], fraction: f32) -> [u8; 4] {
    let channel = |low_channel: u8, high_channel: u8| {
        let value = f32::from(low_channel)
            + (f32::from(high_channel) - f32::from(low_channel)) * fraction;
        value.round().clamp(0.0, 255.0) as u8
    };
    [
        channel(low[0], high[0]),
        channel(low[1], high[1]),
        channel(low[2], high[2]),
        255,
    ]
}

fn reflectivity_color(dbz: f32) -> Result<[u8; 4], ColorMapError> {
    if dbz.is_nan() {
        return Ok(RANGE_FOLDED);
    }
    if !(MIN_DBZ..=MAX_DBZ).contains(&dbz) {
        return Err(ColorMapError::OutOfRange {
            product: PRODUCT,
            value: dbz,
        });
    }

    let lowest = KEYPOINTS
        .first()
        .ok_or(ColorMapError::Unmapped {
            product: PRODUCT,
            value: dbz,
        })?;
    let highest = KEYPOINTS
        .last()
        .ok_or(ColorMapError::Unmapped {
            product: PRODUCT,
            value: dbz,
        })?;

    if dbz < lowest.0 {
        return Ok(with_alpha(lowest.1));
    }
    if dbz >= highest.0 {
        return Ok(with_alpha(highest.1));
    }

    for pair in KEYPOINTS.windows(2) {
        let [low, high] = pair else { continue };
        if dbz >= low.0 && dbz < high.0 {
            let fraction = (dbz - low.0) / (high.0 - low.0);
            return Ok(interpolate(low.1, high.1, fraction));
        }
    }

    Err(ColorMapError::Unmapped {
        product: PRODUCT,
        value: dbz,
    })
}

pub fn color_for_reflectivity(dbz: f32) -> Result<[u8; 4], ColorMapError> {
    reflectivity_color(dbz)
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
                Self::Color { dbz, expected, actual } => write!(
                    formatter,
                    "reflectivity {dbz} dBZ: expected {expected:?}, got {actual:?}"
                ),
            }
        }
    }

    fn expect_color(dbz: f32, expected: [u8; 4]) -> Result<(), TestFailure> {
        let actual = reflectivity_color(dbz)?;
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
        let cases = [
            (-15.0, [102, 139, 139, 255]),
            (0.0, [187, 255, 255, 255]),
            (20.0, [0, 255, 0, 255]),
            (40.0, [231, 192, 0, 255]),
            (55.0, [214, 0, 0, 255]),
        ];
        for (dbz, expected) in cases {
            expect_color(dbz, expected)?;
        }
        Ok(())
    }

    #[test]
    fn interpolation_between_keypoints() -> Result<(), TestFailure> {
        expect_color(32.0, [102, 188, 0, 255])
    }

    #[test]
    fn below_palette_clamps_to_lowest_keypoint() -> Result<(), TestFailure> {
        expect_color(-31.0, [50, 79, 79, 255])
    }

    #[test]
    fn range_folded_maps_to_sentinel_color() -> Result<(), TestFailure> {
        expect_color(f32::NAN, RANGE_FOLDED)
    }

    #[test]
    fn out_of_range_carries_value() {
        let result = reflectivity_color(95.0);
        assert!(
            matches!(result, Err(ColorMapError::OutOfRange { product, value }) if product == PRODUCT && value == 95.0),
            "expected OutOfRange carrying 95.0, got {result:?}"
        );
    }
}
