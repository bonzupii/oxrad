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
use geographiclib_rs::{
    DirectGeodesic, Geodesic, GeodesicLine, InverseGeodesic, geodesic_capability,
};

use crate::error::CoordinateError;

const METERS_PER_KM: f64 = 1000.0;
const MERCATOR_RADIUS_KM: f64 = 6378.137;
const MERCATOR_LIMIT_DEG: f64 = 85.051_128_779_806_6;
const MEAN_EARTH_RADIUS_KM: f64 = 6371.0;
const EFFECTIVE_RADIUS_KM: f64 = MEAN_EARTH_RADIUS_KM * 4.0 / 3.0;

pub fn ground_range(slant_km: f64, elevation_deg: f64) -> Result<f64, CoordinateError> {
    if !slant_km.is_finite() || slant_km < 0.0 {
        return Err(CoordinateError::Range { range_km: slant_km });
    }
    if !elevation_deg.is_finite() || !(-90.0..=90.0).contains(&elevation_deg) {
        return Err(CoordinateError::Elevation { elevation_deg });
    }
    let elevation = elevation_deg.to_radians();
    let height = (slant_km * slant_km
        + EFFECTIVE_RADIUS_KM * EFFECTIVE_RADIUS_KM
        + 2.0 * slant_km * EFFECTIVE_RADIUS_KM * elevation.sin())
    .sqrt()
        - EFFECTIVE_RADIUS_KM;
    let ratio = slant_km * elevation.cos() / (EFFECTIVE_RADIUS_KM + height);
    if !ratio.is_finite() {
        return Err(CoordinateError::Range { range_km: slant_km });
    }
    Ok(EFFECTIVE_RADIUS_KM * ratio.clamp(-1.0, 1.0).asin())
}

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

pub fn mercator(lat: f64, lon: f64) -> Result<(f64, f64), CoordinateError> {
    validate_lat(lat)?;
    validate_lon(lon)?;
    let lat = lat
        .clamp(-MERCATOR_LIMIT_DEG, MERCATOR_LIMIT_DEG)
        .to_radians();
    Ok((
        MERCATOR_RADIUS_KM * lon.to_radians(),
        MERCATOR_RADIUS_KM * (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln(),
    ))
}

pub fn mercator_inverse(x: f64, y: f64) -> Result<(f64, f64), CoordinateError> {
    let lon = normalize_lon((x / MERCATOR_RADIUS_KM).to_degrees());
    let lat =
        (2.0 * (y / MERCATOR_RADIUS_KM).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
    validate_lat(lat)?;
    validate_lon(lon)?;
    Ok((lat, lon))
}

pub fn mercator_wrap_delta(x: f64) -> f64 {
    let circumference = 2.0 * std::f64::consts::PI * MERCATOR_RADIUS_KM;
    (x + circumference / 2.0).rem_euclid(circumference) - circumference / 2.0
}

fn normalize_lon(lon: f64) -> f64 {
    let wrapped = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 { 180.0 } else { wrapped }
}

pub struct GeodesicPath {
    line: GeodesicLine,
}

impl GeodesicPath {
    pub fn new(lat: f64, lon: f64, azimuth_deg: f64) -> Result<Self, CoordinateError> {
        validate_lat(lat)?;
        validate_lon(lon)?;
        if !azimuth_deg.is_finite() || !(0.0..=360.0).contains(&azimuth_deg) {
            return Err(CoordinateError::Azimuth { azimuth_deg });
        }
        Ok(Self {
            line: GeodesicLine::new(&Geodesic::wgs84(), lat, lon, azimuth_deg, None, None, None),
        })
    }

    pub fn position(&self, range_km: f64) -> Result<(f64, f64), CoordinateError> {
        if !range_km.is_finite() || range_km < 0.0 {
            return Err(CoordinateError::Range { range_km });
        }
        let mask = geodesic_capability::LATITUDE | geodesic_capability::LONGITUDE;
        let (_, lat, lon, _, _, _, _, _, _) =
            self.line
                ._gen_position(false, range_km * METERS_PER_KM, mask);
        validate_lat(lat)?;
        validate_lon(lon)?;
        Ok((lat, lon))
    }
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
    let (lat, lon, _azimuth) =
        geodesic.direct(site_lat, site_lon, azimuth_deg, range_km * METERS_PER_KM);
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
    fn ground_range_matches_reference_values() -> Result<(), TestFailure> {
        for (elevation, slant, expected) in [
            (0.5, 50.0, 49.99495),
            (0.5, 100.0, 99.98130),
            (0.5, 230.0, 229.88078),
            (5.0, 50.0, 49.78363),
            (5.0, 100.0, 99.51281),
            (5.0, 230.0, 228.53021),
            (19.5, 50.0, 47.03917),
            (19.5, 100.0, 93.89135),
            (19.5, 230.0, 214.81976),
        ] {
            expect_within(
                "ground range km",
                expected,
                ground_range(slant, elevation)?,
                1e-5,
            )?;
        }
        Ok(())
    }

    #[test]
    fn mercator_matches_reference_tile_coordinates() -> Result<(), TestFailure> {
        let (x, y) = mercator(SITE_LAT, SITE_LON)?;
        for (zoom, expected_x, expected_y) in
            [(4, 4.054580, 6.223877), (10, 259.493148, 398.328116)]
        {
            let n = f64::from(1_u32 << zoom);
            let tile_x = n * (0.5 + x / (2.0 * std::f64::consts::PI * MERCATOR_RADIUS_KM));
            let tile_y = n * (0.5 - y / (2.0 * std::f64::consts::PI * MERCATOR_RADIUS_KM));
            expect_within("tile x", expected_x, tile_x, 1e-6)?;
            expect_within("tile y", expected_y, tile_y, 1e-6)?;
        }
        let (lat, lon) = mercator_inverse(x, y)?;
        expect_within("latitude", SITE_LAT, lat, 1e-9)?;
        expect_within("longitude", SITE_LON, lon, 1e-9)
    }

    #[test]
    fn geodesic_paths_match_reference_values() -> Result<(), TestFailure> {
        for (azimuth, expected_lat, expected_lon) in REFERENCE {
            let path = GeodesicPath::new(SITE_LAT, SITE_LON, azimuth)?;
            let (lat, lon) = path.position(RANGE_KM)?;
            expect_within("path latitude", expected_lat, lat, LAT_TOLERANCE_DEG)?;
            expect_within("path longitude", expected_lon, lon, LAT_TOLERANCE_DEG)?;
        }
        Ok(())
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
