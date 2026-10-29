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
use geographiclib_rs::{DirectGeodesic, Geodesic, InverseGeodesic};

use crate::error::CoordinateError;

const METERS_PER_KM: f64 = 1000.0;

fn validate_lat(lat: f64) -> Result<(), CoordinateError> {
    if (-90.0..=90.0).contains(&lat) {
        Ok(())
    } else {
        Err(CoordinateError::Latitude { lat })
    }
}

fn validate_lon(lon: f64) -> Result<(), CoordinateError> {
    if (-180.0..=180.0).contains(&lon) {
        Ok(())
    } else {
        Err(CoordinateError::Longitude { lon })
    }
}

fn normalize_lon(lon: f64) -> f64 {
    let wrapped = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 { 180.0 } else { wrapped }
}

pub fn forward(
    range_km: f64,
    azimuth_deg: f64,
    site_lat: f64,
    site_lon: f64,
) -> Result<(f64, f64), CoordinateError> {
    if !range_km.is_finite() || range_km < 0.0 {
        return Err(CoordinateError::Range { range_km });
    }
    if !azimuth_deg.is_finite() || !(0.0..=360.0).contains(&azimuth_deg) {
        return Err(CoordinateError::Azimuth { azimuth_deg });
    }
    validate_lat(site_lat)?;
    validate_lon(site_lon)?;

    let geodesic = Geodesic::wgs84();
    let (lat, lon, _azimuth) = geodesic.direct(
        site_lat,
        site_lon,
        azimuth_deg,
        range_km * METERS_PER_KM,
    );
    validate_lat(lat)?;
    Ok((lat, normalize_lon(lon)))
}

pub fn inverse(
    target_lat: f64,
    target_lon: f64,
    site_lat: f64,
    site_lon: f64,
) -> Result<(f64, f64), CoordinateError> {
    validate_lat(target_lat)?;
    validate_lon(target_lon)?;
    validate_lat(site_lat)?;
    validate_lon(site_lon)?;

    if (target_lat - site_lat).abs() < 1e-12 && (target_lon - site_lon).abs() < 1e-12 {
        return Ok((0.0, 0.0));
    }

    let geodesic = Geodesic::wgs84();
    let (distance_m, azimuth_deg, _back_azimuth, _arc_deg) =
        geodesic.inverse(site_lat, site_lon, target_lat, target_lon);
    Ok((distance_m / METERS_PER_KM, azimuth_deg.rem_euclid(360.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE_LAT: f64 = 37.06833;
    const SITE_LON: f64 = -88.77194;
    const RANGE_KM: f64 = 75.0;
    const LAT_TOLERANCE_DEG: f64 = 5e-7;
    const ROUND_TRIP_DISTANCE_TOLERANCE_M: f64 = 1e-3;
    const ROUND_TRIP_AZIMUTH_TOLERANCE_DEG: f64 = 1e-9;

    const REFERENCE: [(f64, f64, f64); 8] = [
        (0.0, 37.744095, -88.771940),
        (45.0, 37.544662, -88.171837),
        (90.0, 37.065332, -87.928621),
        (135.0, 36.588961, -88.179319),
        (180.0, 36.392487, -88.771940),
        (225.0, 36.588961, -89.364561),
        (270.0, 37.065332, -89.615259),
        (315.0, 37.544662, -89.372043),
    ];

    fn angular_difference_deg(a: f64, b: f64) -> f64 {
        let difference = (a - b).rem_euclid(360.0);
        if difference > 180.0 {
            360.0 - difference
        } else {
            difference
        }
    }

    #[derive(Debug)]
    enum TestFailure {
        Coordinate(CoordinateError),
        Value {
            label: &'static str,
            expected: f64,
            actual: f64,
            tolerance: f64,
        },
    }

    impl From<CoordinateError> for TestFailure {
        fn from(source: CoordinateError) -> Self {
            Self::Coordinate(source)
        }
    }

    impl std::fmt::Display for TestFailure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Coordinate(error) => write!(formatter, "{error}"),
                Self::Value {
                    label,
                    expected,
                    actual,
                    tolerance,
                } => write!(
                    formatter,
                    "{label}: expected {expected}, got {actual}, tolerance {tolerance}"
                ),
            }
        }
    }

    fn expect_within(
        label: &'static str,
        expected: f64,
        actual: f64,
        tolerance: f64,
    ) -> Result<(), TestFailure> {
        if (actual - expected).abs() <= tolerance {
            Ok(())
        } else {
            Err(TestFailure::Value {
                label,
                expected,
                actual,
                tolerance,
            })
        }
    }

    #[test]
    fn forward_matches_reference_values() -> Result<(), TestFailure> {
        for (azimuth_deg, expected_lat, expected_lon) in REFERENCE {
            let (lat, lon) = forward(RANGE_KM, azimuth_deg, SITE_LAT, SITE_LON)?;
            expect_within("latitude", expected_lat, lat, LAT_TOLERANCE_DEG)?;
            expect_within("longitude", expected_lon, lon, LAT_TOLERANCE_DEG)?;
        }
        Ok(())
    }

    #[test]
    fn inverse_recovers_range_and_azimuth() -> Result<(), TestFailure> {
        for (azimuth_deg, _expected_lat, _expected_lon) in REFERENCE {
            let (target_lat, target_lon) = forward(RANGE_KM, azimuth_deg, SITE_LAT, SITE_LON)?;
            let (range_km, recovered_azimuth) =
                inverse(target_lat, target_lon, SITE_LAT, SITE_LON)?;
            expect_within(
                "range m",
                RANGE_KM * METERS_PER_KM,
                range_km * METERS_PER_KM,
                ROUND_TRIP_DISTANCE_TOLERANCE_M,
            )?;
            expect_within(
                "azimuth deg",
                azimuth_deg,
                recovered_azimuth,
                ROUND_TRIP_AZIMUTH_TOLERANCE_DEG,
            )?;
            let angular_error_deg = angular_difference_deg(recovered_azimuth, azimuth_deg);
            expect_within(
                "azimuth wrap deg",
                0.0,
                angular_error_deg,
                ROUND_TRIP_AZIMUTH_TOLERANCE_DEG,
            )?;
        }
        Ok(())
    }

    #[test]
    fn forward_rejects_negative_range() {
        let result = forward(-1.0, 0.0, SITE_LAT, SITE_LON);
        assert!(
            matches!(result, Err(CoordinateError::Range { range_km }) if range_km == -1.0),
            "expected Range error carrying -1.0, got {result:?}"
        );
    }

    #[test]
    fn forward_rejects_out_of_range_azimuth() {
        let result = forward(10.0, 361.0, SITE_LAT, SITE_LON);
        assert!(
            matches!(result, Err(CoordinateError::Azimuth { azimuth_deg }) if azimuth_deg == 361.0),
            "expected Azimuth error carrying 361.0, got {result:?}"
        );
    }

    #[test]
    fn inverse_rejects_invalid_latitude() {
        let result = inverse(91.0, 0.0, SITE_LAT, SITE_LON);
        assert!(
            matches!(result, Err(CoordinateError::Latitude { lat }) if lat == 91.0),
            "expected Latitude error carrying 91.0, got {result:?}"
        );
    }

    #[test]
    fn inverse_rejects_invalid_longitude() {
        let result = inverse(0.0, 181.0, SITE_LAT, SITE_LON);
        assert!(
            matches!(result, Err(CoordinateError::Longitude { lon }) if lon == 181.0),
            "expected Longitude error carrying 181.0, got {result:?}"
        );
    }

    #[test]
    fn inverse_returns_zero_at_site() -> Result<(), TestFailure> {
        let (range_km, azimuth_deg) = inverse(SITE_LAT, SITE_LON, SITE_LAT, SITE_LON)?;
        expect_within("range km", 0.0, range_km, 0.0)?;
        expect_within("azimuth deg", 0.0, azimuth_deg, 0.0)
    }
}
