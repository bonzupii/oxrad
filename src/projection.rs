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
use crate::error::CoordinateError;

const EARTH_RADIUS_KM: f64 = 6371.0;
const PI: f64 = std::f64::consts::PI;

pub fn forward(
    range_km: f64,
    azimuth_deg: f64,
    site_lat: f64,
    site_lon: f64,
) -> Result<(f64, f64), CoordinateError> {
    if range_km < 0.0 {
        return Err(CoordinateError::Range { range_km });
    }
    if !(0.0..=360.0).contains(&azimuth_deg) {
        return Err(CoordinateError::Azimuth { azimuth_deg });
    }
    if !(-90.0..=90.0).contains(&site_lat) {
        return Err(CoordinateError::Latitude { lat: site_lat });
    }
    if !(-180.0..=180.0).contains(&site_lon) {
        return Err(CoordinateError::Longitude { lon: site_lon });
    }

    let lat1_rad = site_lat * PI / 180.0;
    let lon1_rad = site_lon * PI / 180.0;
    let az_rad = azimuth_deg * PI / 180.0;
    let delta = range_km / EARTH_RADIUS_KM;

    let sin_lat1 = lat1_rad.sin();
    let cos_lat1 = lat1_rad.cos();
    let sin_delta = delta.sin();
    let cos_delta = delta.cos();
    let sin_az = az_rad.sin();
    let cos_az = az_rad.cos();

    let lat2_raw = sin_lat1 * cos_delta + cos_lat1 * sin_delta * cos_az;
    let lat2_raw = lat2_raw.clamp(-1.0, 1.0);
    let lat2_rad = lat2_raw.asin();

    let lon2_rad =
        lon1_rad + (sin_az * sin_delta * cos_lat1).atan2(cos_delta - sin_lat1 * lat2_rad.sin());

    let lat2_deg = lat2_rad * 180.0 / PI;
    let lon2_deg = lon2_rad * 180.0 / PI;
    let lon2_deg = ((lon2_deg + 540.0) % 360.0) - 180.0;

    Ok((lat2_deg, lon2_deg))
}

pub fn inverse(
    target_lat: f64,
    target_lon: f64,
    site_lat: f64,
    site_lon: f64,
) -> Result<(f64, f64), CoordinateError> {
    if !(-90.0..=90.0).contains(&target_lat) {
        return Err(CoordinateError::Latitude { lat: target_lat });
    }
    if !(-180.0..=180.0).contains(&target_lon) {
        return Err(CoordinateError::Longitude { lon: target_lon });
    }
    if !(-90.0..=90.0).contains(&site_lat) {
        return Err(CoordinateError::Latitude { lat: site_lat });
    }
    if !(-180.0..=180.0).contains(&site_lon) {
        return Err(CoordinateError::Longitude { lon: site_lon });
    }

    if (target_lat - site_lat).abs() < 1e-12 && (target_lon - site_lon).abs() < 1e-12 {
        return Ok((0.0, 0.0));
    }

    let lat1_rad = site_lat * PI / 180.0;
    let lon1_rad = site_lon * PI / 180.0;
    let lat2_rad = target_lat * PI / 180.0;
    let lon2_rad = target_lon * PI / 180.0;

    let d_lat = lat2_rad - lat1_rad;
    let d_lon = lon2_rad - lon1_rad;

    let a =
        (d_lat / 2.0).sin().powi(2) + lat1_rad.cos() * lat2_rad.cos() * (d_lon / 2.0).sin().powi(2);
    let a = a.clamp(0.0, 1.0);

    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    let range_km = EARTH_RADIUS_KM * c;

    let y = d_lon.sin() * lat2_rad.cos();
    let x = lat1_rad.cos() * lat2_rad.sin() - lat1_rad.sin() * lat2_rad.cos() * d_lon.cos();

    let az_rad = y.atan2(x);
    let az_deg = ((az_rad * 180.0 / PI) + 360.0) % 360.0;

    Ok((range_km, az_deg))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE_LAT: f64 = 37.0683;
    const SITE_LON: f64 = -88.7720;

    #[test]
    fn forward_zero_range_returns_site_coordinates() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(0.0, 0.0, SITE_LAT, SITE_LON)?;
        assert!(
            (lat - SITE_LAT).abs() < 1e-6,
            "forward lat {lat:.6} differs from site {SITE_LAT} by {:.6} (tolerance 1e-6°)",
            (lat - SITE_LAT).abs()
        );
        assert!(
            (lon - SITE_LON).abs() < 1e-6,
            "forward lon {lon:.6} differs from site {SITE_LON} by {:.6} (tolerance 1e-6°)",
            (lon - SITE_LON).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_due_north_at_100km() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(100.0, 0.0, SITE_LAT, SITE_LON)?;
        assert!(
            lat > SITE_LAT,
            "forward lat {lat:.6} should be north of site {SITE_LAT}"
        );
        assert!(
            (lon - SITE_LON).abs() < 0.01,
            "forward lon {lon:.6} should match site lon {SITE_LON} for due-north azimuth, diff {:.6}",
            (lon - SITE_LON).abs()
        );
        assert!(
            (lat - SITE_LAT - 0.9).abs() < 0.1,
            "forward lat {lat:.6} should be ~0.9° north of site, actual diff {:.4}°",
            lat - SITE_LAT
        );
        Ok(())
    }

    #[test]
    fn forward_due_east_at_100km() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(100.0, 90.0, SITE_LAT, SITE_LON)?;
        assert!(
            lon > SITE_LON,
            "forward lon {lon:.6} should be east of site {SITE_LON}"
        );
        assert!(
            (lat - SITE_LAT).abs() < 0.05,
            "forward lat {lat:.6} should be approximately equal to site lat {SITE_LAT} for due-east azimuth, diff {:.6}",
            (lat - SITE_LAT).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_due_south_at_100km() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(100.0, 180.0, SITE_LAT, SITE_LON)?;
        assert!(
            lat < SITE_LAT,
            "forward lat {lat:.6} should be south of site {SITE_LAT}"
        );
        assert!(
            (lon - SITE_LON).abs() < 0.01,
            "forward lon {lon:.6} should match site lon {SITE_LON} for due-south azimuth, diff {:.6}",
            (lon - SITE_LON).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_due_west_at_100km() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(100.0, 270.0, SITE_LAT, SITE_LON)?;
        assert!(
            lon < SITE_LON,
            "forward lon {lon:.6} should be west of site {SITE_LON}"
        );
        assert!(
            (lat - SITE_LAT).abs() < 0.05,
            "forward lat {lat:.6} should be approximately equal to site lat {SITE_LAT} for due-west azimuth, diff {:.6}",
            (lat - SITE_LAT).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_reference_north_10km() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(10.0, 0.0, SITE_LAT, SITE_LON)?;
        assert!(
            (lat - 37.158148).abs() < 0.001,
            "forward lat {lat:.6} differs from reference 37.158148 by {:.6} (tolerance 0.001°)",
            (lat - 37.158148).abs()
        );
        assert!(
            (lon - (-88.772000)).abs() < 0.001,
            "forward lon {lon:.6} differs from reference -88.772000 by {:.6} (tolerance 0.001°)",
            (lon - (-88.772000)).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_reference_east_10km() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(10.0, 90.0, SITE_LAT, SITE_LON)?;
        assert!(
            (lat - 37.068202).abs() < 0.001,
            "forward lat {lat:.6} differs from reference 37.068202 by {:.6} (tolerance 0.001°)",
            (lat - 37.068202).abs()
        );
        assert!(
            (lon - (-88.659459)).abs() < 0.001,
            "forward lon {lon:.6} differs from reference -88.659459 by {:.6} (tolerance 0.001°)",
            (lon - (-88.659459)).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_reference_sw_100km() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(100.0, 225.0, SITE_LAT, SITE_LON)?;
        assert!(
            (lat - 36.429753).abs() < 0.001,
            "forward lat {lat:.6} differs from reference 36.429753 by {:.6} (tolerance 0.001°)",
            (lat - 36.429753).abs()
        );
        assert!(
            (lon - (-89.562358)).abs() < 0.001,
            "forward lon {lon:.6} differs from reference -89.562358 by {:.6} (tolerance 0.001°)",
            (lon - (-89.562358)).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_reference_zero_range() -> Result<(), Box<dyn std::error::Error>> {
        let (lat, lon) = forward(0.0, 0.0, SITE_LAT, SITE_LON)?;
        assert!(
            (lat - 37.068300).abs() < 0.001,
            "forward lat {lat:.6} differs from reference 37.068300 by {:.6} (tolerance 0.001°)",
            (lat - 37.068300).abs()
        );
        assert!(
            (lon - (-88.772000)).abs() < 0.001,
            "forward lon {lon:.6} differs from reference -88.772000 by {:.6} (tolerance 0.001°)",
            (lon - (-88.772000)).abs()
        );
        Ok(())
    }

    #[test]
    fn forward_negative_range_returns_invalid_range() -> Result<(), Box<dyn std::error::Error>> {
        let result = forward(-1.0, 0.0, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Range { range_km }) => {
                assert_eq!(
                    range_km, -1.0,
                    "Range error should carry the invalid value -1.0, got {range_km}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Range), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Range), got Err({e})").into()),
        }
    }

    #[test]
    fn forward_azimuth_above_360_returns_invalid_azimuth() -> Result<(), Box<dyn std::error::Error>>
    {
        let result = forward(10.0, 361.0, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Azimuth { azimuth_deg }) => {
                assert_eq!(
                    azimuth_deg, 361.0,
                    "Azimuth error should carry the invalid value 361.0, got {azimuth_deg}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Azimuth), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Azimuth), got Err({e})").into()),
        }
    }

    #[test]
    fn forward_site_lat_above_90_returns_invalid_latitude() -> Result<(), Box<dyn std::error::Error>>
    {
        let result = forward(10.0, 0.0, 91.0, SITE_LON);
        match result {
            Err(CoordinateError::Latitude { lat }) => {
                assert_eq!(
                    lat, 91.0,
                    "Latitude error should carry the invalid value 91.0, got {lat}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Latitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Latitude), got Err({e})").into()),
        }
    }

    #[test]
    fn forward_site_lat_below_minus_90_returns_invalid_latitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = forward(10.0, 0.0, -91.0, SITE_LON);
        match result {
            Err(CoordinateError::Latitude { lat }) => {
                assert_eq!(
                    lat, -91.0,
                    "Latitude error should carry the invalid value -91.0, got {lat}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Latitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Latitude), got Err({e})").into()),
        }
    }

    #[test]
    fn forward_site_lon_above_180_returns_invalid_longitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = forward(10.0, 0.0, SITE_LAT, 181.0);
        match result {
            Err(CoordinateError::Longitude { lon }) => {
                assert_eq!(
                    lon, 181.0,
                    "Longitude error should carry the invalid value 181.0, got {lon}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Longitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Longitude), got Err({e})").into()),
        }
    }

    #[test]
    fn forward_site_lon_below_minus_180_returns_invalid_longitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = forward(10.0, 0.0, SITE_LAT, -181.0);
        match result {
            Err(CoordinateError::Longitude { lon }) => {
                assert_eq!(
                    lon, -181.0,
                    "Longitude error should carry the invalid value -181.0, got {lon}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Longitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Longitude), got Err({e})").into()),
        }
    }

    #[test]
    fn inverse_reference_north_10km() -> Result<(), Box<dyn std::error::Error>> {
        let (range_km, az_deg) = inverse(37.158148, -88.772000, SITE_LAT, SITE_LON)?;
        assert!(
            (range_km - 10.0).abs() < 0.1,
            "inverse range {range_km:.6} differs from reference 10.000000 by {:.6} (tolerance 0.1 km)",
            (range_km - 10.0).abs()
        );
        assert!(
            (az_deg - 0.0).abs() < 0.1,
            "inverse azimuth {az_deg:.6} differs from reference 0.000000 by {:.6} (tolerance 0.1°)",
            (az_deg - 0.0).abs()
        );
        Ok(())
    }

    #[test]
    fn inverse_reference_east_10km() -> Result<(), Box<dyn std::error::Error>> {
        let (range_km, az_deg) = inverse(37.068202, -88.659459, SITE_LAT, SITE_LON)?;
        assert!(
            (range_km - 10.0).abs() < 0.1,
            "inverse range {range_km:.6} differs from reference 10.000000 by {:.6} (tolerance 0.1 km)",
            (range_km - 10.0).abs()
        );
        assert!(
            (az_deg - 90.0).abs() < 0.1,
            "inverse azimuth {az_deg:.6} differs from reference 90.000000 by {:.6} (tolerance 0.1°)",
            (az_deg - 90.0).abs()
        );
        Ok(())
    }

    #[test]
    fn inverse_reference_sw_100km() -> Result<(), Box<dyn std::error::Error>> {
        let (range_km, az_deg) = inverse(36.429753, -89.562358, SITE_LAT, SITE_LON)?;
        assert!(
            (range_km - 100.0).abs() < 0.1,
            "inverse range {range_km:.6} differs from reference 100.000000 by {:.6} (tolerance 0.1 km)",
            (range_km - 100.0).abs()
        );
        assert!(
            (az_deg - 225.0).abs() < 0.1,
            "inverse azimuth {az_deg:.6} differs from reference 225.000000 by {:.6} (tolerance 0.1°)",
            (az_deg - 225.0).abs()
        );
        Ok(())
    }

    #[test]
    fn inverse_same_point_returns_zero_range() -> Result<(), Box<dyn std::error::Error>> {
        let (range_km, az_deg) = inverse(SITE_LAT, SITE_LON, SITE_LAT, SITE_LON)?;
        assert!(
            (range_km - 0.0).abs() < 1e-12,
            "inverse range {range_km:.6} should be 0.0 for same-point input, got {range_km}"
        );
        assert!(
            (az_deg - 0.0).abs() < 1e-12,
            "inverse azimuth {az_deg:.6} should be 0.0 for same-point input, got {az_deg}"
        );
        Ok(())
    }

    #[test]
    fn inverse_azimuth_always_in_0_360() -> Result<(), Box<dyn std::error::Error>> {
        for az in [0.0_f64, 30.0, 90.0, 135.0, 180.0, 225.0, 270.0, 359.9] {
            let (lat, lon) = forward(75.0, az, SITE_LAT, SITE_LON)?;
            let (_, rt_az) = inverse(lat, lon, SITE_LAT, SITE_LON)?;
            assert!(
                rt_az >= 0.0 && rt_az < 360.0,
                "inverse azimuth {rt_az:.6} for forward az={az} is outside [0, 360)"
            );
        }
        Ok(())
    }

    #[test]
    fn inverse_target_lat_above_90_returns_invalid_latitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = inverse(91.0, SITE_LON, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Latitude { lat }) => {
                assert_eq!(
                    lat, 91.0,
                    "Latitude error should carry the invalid value 91.0, got {lat}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Latitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Latitude), got Err({e})").into()),
        }
    }

    #[test]
    fn inverse_target_lat_below_minus_90_returns_invalid_latitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = inverse(-91.0, SITE_LON, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Latitude { lat }) => {
                assert_eq!(
                    lat, -91.0,
                    "Latitude error should carry the invalid value -91.0, got {lat}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Latitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Latitude), got Err({e})").into()),
        }
    }

    #[test]
    fn inverse_target_lon_above_180_returns_invalid_longitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = inverse(SITE_LAT, 181.0, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Longitude { lon }) => {
                assert_eq!(
                    lon, 181.0,
                    "Longitude error should carry the invalid value 181.0, got {lon}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Longitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Longitude), got Err({e})").into()),
        }
    }

    #[test]
    fn inverse_target_lon_below_minus_180_returns_invalid_longitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = inverse(SITE_LAT, -181.0, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Longitude { lon }) => {
                assert_eq!(
                    lon, -181.0,
                    "Longitude error should carry the invalid value -181.0, got {lon}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Longitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Longitude), got Err({e})").into()),
        }
    }

    #[test]
    fn inverse_site_lat_above_90_returns_invalid_latitude() -> Result<(), Box<dyn std::error::Error>>
    {
        let result = inverse(SITE_LAT, SITE_LON, 91.0, SITE_LON);
        match result {
            Err(CoordinateError::Latitude { lat }) => {
                assert_eq!(
                    lat, 91.0,
                    "Latitude error should carry the invalid value 91.0, got {lat}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Latitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Latitude), got Err({e})").into()),
        }
    }

    #[test]
    fn inverse_site_lon_above_180_returns_invalid_longitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = inverse(SITE_LAT, SITE_LON, SITE_LAT, 181.0);
        match result {
            Err(CoordinateError::Longitude { lon }) => {
                assert_eq!(
                    lon, 181.0,
                    "Longitude error should carry the invalid value 181.0, got {lon}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Longitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Longitude), got Err({e})").into()),
        }
    }

    #[test]
    fn inverse_site_lon_below_minus_180_returns_invalid_longitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = inverse(SITE_LAT, SITE_LON, SITE_LAT, -181.0);
        match result {
            Err(CoordinateError::Longitude { lon }) => {
                assert_eq!(
                    lon, -181.0,
                    "Longitude error should carry the invalid value -181.0, got {lon}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Longitude), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Longitude), got Err({e})").into()),
        }
    }

    #[test]
    fn round_trip_all_azimuths() -> Result<(), Box<dyn std::error::Error>> {
        let mut failures: Vec<String> = Vec::new();

        for az in [0.0_f64, 30.0, 90.0, 135.0, 180.0, 225.0, 270.0, 359.9] {
            let (lat, lon) = forward(75.0, az, SITE_LAT, SITE_LON)?;
            let (rt_range, rt_az) = inverse(lat, lon, SITE_LAT, SITE_LON)?;

            if (rt_range - 75.0).abs() >= 0.01 {
                failures.push(format!(
                    "az={az}: range round-trip {rt_range:.4} != 75.0000 (diff {:.4})",
                    (rt_range - 75.0).abs()
                ));
            }
            let mut az_diff = (rt_az - az).abs();
            az_diff = az_diff.min(360.0 - az_diff);
            if az_diff >= 0.01 {
                failures.push(format!(
                    "az={az}: azimuth round-trip {rt_az:.4} != {az:.4} (diff {az_diff:.4})"
                ));
            }
        }

        if !failures.is_empty() {
            return Err(failures.join("\n").into());
        }
        Ok(())
    }

    #[test]
    fn forward_rejects_small_negative_azimuth() -> Result<(), Box<dyn std::error::Error>> {
        let negative_az = -0.0028076171875_f64;
        let result = forward(10.0, negative_az, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Azimuth { azimuth_deg }) => {
                assert_eq!(
                    azimuth_deg, negative_az,
                    "Azimuth error should carry the invalid value {negative_az}, got {azimuth_deg}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Azimuth), got Ok".into()),
            Err(e) => Err(format!("Expected Err(Azimuth), got Err({e})").into()),
        }
    }

    #[test]
    fn forward_accepts_normalized_azimuth_near_zero() -> Result<(), Box<dyn std::error::Error>> {
        let negative_az = -0.0028076171875_f64;
        let normalized_az = (negative_az % 360.0 + 360.0) % 360.0;
        assert!(
            (0.0..=360.0).contains(&normalized_az),
            "Normalized azimuth {normalized_az} must be in [0, 360]"
        );
        let result = forward(10.0, normalized_az, SITE_LAT, SITE_LON);
        match result {
            Ok((lat, lon)) => {
                let (lat_north, _) = forward(10.0, 0.0, SITE_LAT, SITE_LON)?;
                assert!(
                    (lat - lat_north).abs() < 0.01,
                    "Normalized azimuth {normalized_az} produced lat {lat:.6}, expected near north lat {lat_north:.6}, diff {:.6}",
                    (lat - lat_north).abs()
                );
                assert!(
                    lon > SITE_LON - 0.01 && lon < SITE_LON + 0.01,
                    "Normalized azimuth {normalized_az} produced lon {lon:.6}, expected near site lon {SITE_LON}"
                );
                Ok(())
            }
            Err(e) => Err(format!(
                "Expected Ok for normalized azimuth {normalized_az}, got Err({e})"
            )
            .into()),
        }
    }

    #[test]
    fn forward_rejects_azimuth_above_360_wrapped() -> Result<(), Box<dyn std::error::Error>> {
        let az = 500.0_f64;
        let result = forward(10.0, az, SITE_LAT, SITE_LON);
        match result {
            Err(CoordinateError::Azimuth { azimuth_deg }) => {
                assert_eq!(
                    azimuth_deg, az,
                    "Azimuth error should carry the invalid value {az}, got {azimuth_deg}"
                );
                Ok(())
            }
            Ok(_) => Err("Expected Err(Azimuth) for azimuth=500.0, got Ok — forward must not silently normalize".into()),
            Err(e) => Err(format!("Expected Err(Azimuth), got Err({e})").into()),
        }
    }
}
