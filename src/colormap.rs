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

#[derive(Clone, Copy, Debug)]
pub enum Product {
    BaseReflectivity,
}

impl Product {
    fn name(self) -> &'static str {
        match self {
            Product::BaseReflectivity => "BaseReflectivity",
        }
    }
}

fn reflectivity_color(dbz: f32) -> Result<[u8; 4], ColorMapError> {
    if dbz.is_nan() {
        return Ok([128, 0, 255, 255]);
    }

    if !(-32.0..=94.5).contains(&dbz) {
        return Err(ColorMapError::OutOfRange {
            product: Product::BaseReflectivity.name(),
            value: dbz,
        });
    }

    // DO NOT MODIFY — NWS Level-3 product code 32 reflectivity palette.
    // Source: supercell-wx/scwx-qt/res/palettes/wct/DR.pal
    // Keypoints at 5 dBZ intervals from -20 to 75.
    // Values between keypoints are linearly interpolated per NWS convention.
    // Changing these values is a safety defect — storm chasers read color
    // gradients to estimate storm intensity at a glance.
    let keypoints: [(f32, u8, u8, u8); 20] = [
        (-20.0, 50, 79, 79),
        (-15.0, 102, 139, 139),
        (-10.0, 150, 205, 205),
        (-5.0, 174, 238, 238),
        (0.0, 187, 255, 255),
        (5.0, 0, 236, 236),
        (10.0, 1, 160, 246),
        (15.0, 0, 0, 246),
        (20.0, 0, 255, 0),
        (25.0, 0, 200, 0),
        (30.0, 0, 144, 0),
        (35.0, 255, 255, 0),
        (40.0, 231, 192, 0),
        (45.0, 255, 144, 0),
        (50.0, 255, 0, 0),
        (55.0, 214, 0, 0),
        (60.0, 192, 0, 0),
        (65.0, 255, 0, 255),
        (70.0, 153, 85, 201),
        (75.0, 235, 235, 235),
    ];

    if dbz < keypoints[0].0 {
        let (_, r, g, b) = keypoints[0];
        return Ok([r, g, b, 255]);
    }
    if dbz >= keypoints[19].0 {
        let (_, r, g, b) = keypoints[19];
        return Ok([r, g, b, 255]);
    }

    let mut i = 0;
    while i < keypoints.len() - 1 && keypoints[i + 1].0 <= dbz {
        i += 1;
    }

    if i >= keypoints.len() - 1 {
        let (_, r, g, b) = keypoints[keypoints.len() - 1];
        return Ok([r, g, b, 255]);
    }

    let (key0, r0, g0, b0) = keypoints[i];
    let (key1, r1, g1, b1) = keypoints[i + 1];
    let t = (dbz - key0) / (key1 - key0);
    let r = (r0 as f32 + (r1 as f32 - r0 as f32) * t).round() as u8;
    let g = (g0 as f32 + (g1 as f32 - g0 as f32) * t).round() as u8;
    let b = (b0 as f32 + (b1 as f32 - b0 as f32) * t).round() as u8;
    Ok([r, g, b, 255])
}

pub fn map_value(product: Product, value: f32) -> Result<[u8; 4], ColorMapError> {
    match product {
        Product::BaseReflectivity => reflectivity_color(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // DO NOT MODIFY expected color values in any test below.
    // These values are derived from the NWS Level-3 product code 32
    // reflectivity palette (supercell-wx/scwx-qt/res/palettes/wct/DR.pal).
    // Keypoint values are taken directly from DR.pal.
    // Interpolated values are computed by linear interpolation between
    // adjacent keypoints per NWS convention.
    // Changing these expected values without changing the palette source
    // is a test fraud that produces a silent safety defect. A wrong color
    // scale causes storm chasers to misread storm intensity.
    // If you believe a value is wrong, verify against DR.pal first.

    #[test]
    fn reflectivity_below_threshold_clamps_to_lowest_keypoint()
    -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(-25.0)?;
        assert_eq!(
            color,
            [50, 79, 79, 255],
            "reflectivity -25.0 dBZ should clamp to NWS -20.0 keypoint [50,79,79], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_minus_15() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(-15.0)?;
        assert_eq!(
            color,
            [102, 139, 139, 255],
            "reflectivity -15.0 dBZ should map to NWS keypoint [102,139,139], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_0() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(0.0)?;
        assert_eq!(
            color,
            [187, 255, 255, 255],
            "reflectivity 0.0 dBZ should map to NWS keypoint [187,255,255], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_5() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(5.0)?;
        assert_eq!(
            color,
            [0, 236, 236, 255],
            "reflectivity 5.0 dBZ should map to NWS keypoint [0,236,236], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_10() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(10.0)?;
        assert_eq!(
            color,
            [1, 160, 246, 255],
            "reflectivity 10.0 dBZ should map to NWS keypoint [1,160,246], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_15() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(15.0)?;
        assert_eq!(
            color,
            [0, 0, 246, 255],
            "reflectivity 15.0 dBZ should map to NWS keypoint [0,0,246], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_20() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(20.0)?;
        assert_eq!(
            color,
            [0, 255, 0, 255],
            "reflectivity 20.0 dBZ should map to NWS keypoint [0,255,0], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_30() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(30.0)?;
        assert_eq!(
            color,
            [0, 144, 0, 255],
            "reflectivity 30.0 dBZ should map to NWS keypoint [0,144,0], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_40() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(40.0)?;
        assert_eq!(
            color,
            [231, 192, 0, 255],
            "reflectivity 40.0 dBZ should map to NWS keypoint [231,192,0], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_50() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(50.0)?;
        assert_eq!(
            color,
            [255, 0, 0, 255],
            "reflectivity 50.0 dBZ should map to NWS keypoint [255,0,0], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_keypoint_65() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(65.0)?;
        assert_eq!(
            color,
            [255, 0, 255, 255],
            "reflectivity 65.0 dBZ should map to NWS keypoint [255,0,255], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_above_max_clamps_to_highest_keypoint() -> Result<(), Box<dyn std::error::Error>>
    {
        let color = reflectivity_color(90.0)?;
        assert_eq!(
            color,
            [235, 235, 235, 255],
            "reflectivity 90.0 dBZ should clamp to NWS 75.0 keypoint [235,235,235], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_nan_maps_range_folded_purple() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(f32::NAN)?;
        assert_eq!(
            color,
            [128, 0, 255, 255],
            "reflectivity NaN should map to range-folded purple [128,0,255], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_interpolated_32() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(32.0)?;
        // Interpolated between keypoint 30 [0,144,0] and keypoint 35 [255,255,0]
        // t = (32 - 30) / (35 - 30) = 0.4
        // r = 0 + (255 - 0) * 0.4 = 102
        // g = 144 + (255 - 144) * 0.4 = 144 + 44.4 = 188.4 → 188
        // b = 0 + (0 - 0) * 0.4 = 0
        assert_eq!(
            color,
            [102, 188, 0, 255],
            "reflectivity 32.0 dBZ should interpolate to [102,188,0], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_interpolated_55() -> Result<(), Box<dyn std::error::Error>> {
        let color = reflectivity_color(55.0)?;
        // Exact keypoint 55 [214,0,0]
        assert_eq!(
            color,
            [214, 0, 0, 255],
            "reflectivity 55.0 dBZ should map to NWS keypoint [214,0,0], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn reflectivity_out_of_range_low() -> Result<(), Box<dyn std::error::Error>> {
        let result = reflectivity_color(-33.0);
        match result {
            Err(ColorMapError::OutOfRange { product, value }) => {
                assert_eq!(
                    product, "BaseReflectivity",
                    "OutOfRange product should be BaseReflectivity, got {product}"
                );
                assert_eq!(
                    value, -33.0,
                    "OutOfRange value should be -33.0, got {value}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(OutOfRange), got Ok".into()),
        }
    }

    #[test]
    fn reflectivity_out_of_range_high() -> Result<(), Box<dyn std::error::Error>> {
        let result = reflectivity_color(95.0);
        match result {
            Err(ColorMapError::OutOfRange { product, value }) => {
                assert_eq!(
                    product, "BaseReflectivity",
                    "OutOfRange product should be BaseReflectivity, got {product}"
                );
                assert_eq!(value, 95.0, "OutOfRange value should be 95.0, got {value}");
                Ok(())
            }
            Ok(_) => Err("Expected Err(OutOfRange), got Ok".into()),
        }
    }

    #[test]
    fn map_value_delegates_to_reflectivity() -> Result<(), Box<dyn std::error::Error>> {
        let color = map_value(Product::BaseReflectivity, 40.0)?;
        assert_eq!(
            color,
            [231, 192, 0, 255],
            "map_value BaseReflectivity 40.0 should return NWS keypoint [231,192,0], got {:?}",
            color
        );
        Ok(())
    }

    #[test]
    fn map_value_out_of_range_returns_error() -> Result<(), Box<dyn std::error::Error>> {
        let result = map_value(Product::BaseReflectivity, -33.0);
        match result {
            Err(ColorMapError::OutOfRange { product, value }) => {
                assert_eq!(
                    product, "BaseReflectivity",
                    "OutOfRange product should be BaseReflectivity, got {product}"
                );
                assert_eq!(
                    value, -33.0,
                    "OutOfRange value should be -33.0, got {value}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(OutOfRange), got Ok".into()),
        }
    }
}
