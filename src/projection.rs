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
use geographiclib_rs::{
    DirectGeodesic, Geodesic, GeodesicLine, InverseGeodesic, PolygonArea, Winding,
    geodesic_capability,
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
    if !(-1.0..=1.0).contains(&ratio) {
        return Err(CoordinateError::GroundRange {
            slant_km,
            elevation_deg,
        });
    }
    Ok(EFFECTIVE_RADIUS_KM * ratio.asin())
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
    if !(-MERCATOR_LIMIT_DEG..=MERCATOR_LIMIT_DEG).contains(&lat) {
        return Err(CoordinateError::MercatorLatitude { lat });
    }
    let lon = normalize_lon(lon).to_radians();
    let lat = lat.to_radians();
    Ok((
        MERCATOR_RADIUS_KM * lon,
        MERCATOR_RADIUS_KM * (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln(),
    ))
}

pub fn mercator_inverse(x: f64, y: f64) -> Result<(f64, f64), CoordinateError> {
    if !x.is_finite() || !y.is_finite() {
        return Err(CoordinateError::MercatorLatitude { lat: y });
    }
    let lon = normalize_lon((x / MERCATOR_RADIUS_KM).to_degrees());
    let lat =
        (2.0 * (y / MERCATOR_RADIUS_KM).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
    validate_lat(lat)?;
    validate_lon(lon)?;
    if !(-MERCATOR_LIMIT_DEG..=MERCATOR_LIMIT_DEG).contains(&lat) {
        return Err(CoordinateError::MercatorLatitude { lat });
    }
    Ok((lat, lon))
}

pub fn mercator_wrap_delta(x: f64) -> f64 {
    let circumference = 2.0 * std::f64::consts::PI * MERCATOR_RADIUS_KM;
    (x + circumference / 2.0).rem_euclid(circumference) - circumference / 2.0
}

fn normalize_lon(lon: f64) -> f64 {
    // Web Mercator puts -180 and +180 on opposite edges. The western edge is
    // canonical so an antimeridian crossing never jumps a full world width.
    (lon + 180.0).rem_euclid(360.0) - 180.0
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
        let lon = normalize_lon(lon);
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

/// Distance and initial bearing from `(origin_lat, origin_lon)` to
/// `(target_lat, target_lon)` on the WGS84 ellipsoid.
///
/// The origin is first so a caller cannot accidentally reverse a bearing while
/// still getting the same distance. Compass labels depend on this order.
pub fn inverse(
    origin_lat: f64,
    origin_lon: f64,
    target_lat: f64,
    target_lon: f64,
) -> Result<(f64, f64), CoordinateError> {
    validate_lat(origin_lat)?;
    validate_lon(origin_lon)?;
    validate_lat(target_lat)?;
    validate_lon(target_lon)?;

    if (target_lat - origin_lat).abs() < 1e-12 && (target_lon - origin_lon).abs() < 1e-12 {
        return Ok((0.0, 0.0));
    }

    let geodesic = Geodesic::wgs84();
    let (distance_m, azimuth_deg, _back_azimuth, _arc_deg) =
        geodesic.inverse(origin_lat, origin_lon, target_lat, target_lon);
    Ok((distance_m / METERS_PER_KM, azimuth_deg.rem_euclid(360.0)))
}

/// Area and perimeter of the closed geodesic polygon through `points`.
///
/// The path closes from the last corner back to the first. The area is the
/// area enclosed on the WGS84 ellipsoid, in square kilometres, and the
/// perimeter is the length of the boundary, in kilometres. Fewer than three
/// corners do not enclose anything.
pub fn polygon_area_km2(points: &[(f64, f64)]) -> Result<(f64, f64), CoordinateError> {
    if points.len() < 3 {
        return Err(CoordinateError::PolygonPoints {
            count: points.len(),
        });
    }
    let counter_clockwise = solved_area(points, Winding::CounterClockwise)?;
    let clockwise = solved_area(points, Winding::Clockwise)?;
    // Both windings describe the same boundary: one encloses the shape the
    // corners trace and the other the rest of the planet. A measured area is a
    // shape drawn on a map, so it is the smaller of the two.
    let (perimeter_m, area_m2) = if counter_clockwise.1 <= clockwise.1 {
        counter_clockwise
    } else {
        clockwise
    };
    let per_km = METERS_PER_KM * METERS_PER_KM;
    Ok((area_m2.abs() / per_km, perimeter_m / METERS_PER_KM))
}

fn solved_area(points: &[(f64, f64)], winding: Winding) -> Result<(f64, f64), CoordinateError> {
    let geodesic = Geodesic::wgs84();
    let mut area = PolygonArea::new(&geodesic, winding);
    for (lat, lon) in points {
        validate_lat(*lat)?;
        validate_lon(*lon)?;
        area.add_point(*lat, *lon);
    }
    let (perimeter_m, area_m2, _points) = area.compute(false);
    Ok((perimeter_m, area_m2))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE_LAT: f64 = 38.99694;
    const SITE_LON: f64 = -96.23249;
    const RANGE_KM: f64 = 75.0;
    const LAT_TOLERANCE_DEG: f64 = 5e-7;
    const ROUND_TRIP_DISTANCE_TOLERANCE_M: f64 = 1e-3;
    const ROUND_TRIP_AZIMUTH_TOLERANCE_DEG: f64 = 1e-9;

    const REFERENCE: [(f64, f64, f64); 8] = [
        (0.0, 39.672483, -96.232490),
        (45.0, 39.473007, -95.616164),
        (90.0, 38.993728, -95.366767),
        (135.0, 38.517622, -95.624396),
        (180.0, 38.321319, -96.232490),
        (225.0, 38.517622, -96.840584),
        (270.0, 38.993728, -97.098213),
        (315.0, 39.473007, -96.848816),
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
        Message(String),
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
                Self::Message(message) => formatter.write_str(message),
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

    /// A one-degree square on the equator, against the geodesic library's
    /// published reference area and perimeter.
    #[test]
    fn an_equatorial_degree_square_has_its_reference_area() -> Result<(), TestFailure> {
        let corners = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
        let (area_km2, perimeter_km) = polygon_area_km2(&corners)?;
        expect_within("area km2", 12_308.778_361, area_km2, 1e-3)?;
        expect_within("perimeter km", 443.770_917, perimeter_km, 1e-3)
    }

    /// The corner order a user clicks is arbitrary, so walking the same square
    /// clockwise must report the same small area rather than flipping to the
    /// rest of the planet.
    #[test]
    fn corner_order_does_not_change_the_enclosed_area() -> Result<(), TestFailure> {
        let counter_clockwise = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
        let clockwise = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let (first_area, first_perimeter) = polygon_area_km2(&counter_clockwise)?;
        let (second_area, second_perimeter) = polygon_area_km2(&clockwise)?;
        expect_within("area km2", first_area, second_area, 1e-3)?;
        expect_within("perimeter km", first_perimeter, second_perimeter, 1e-3)
    }

    /// A polygon that walks east then back west encloses nothing, so the area
    /// must be zero rather than a negative or doubled value.
    #[test]
    fn a_path_that_does_not_enclose_anything() -> Result<(), TestFailure> {
        let out_and_back = [(0.0, 0.0), (0.0, 1.0), (0.0, 0.0)];
        let (area_km2, _perimeter) = polygon_area_km2(&out_and_back)?;
        assert!(area_km2.abs() < 1e-6, "area was {area_km2} km2");
        Ok(())
    }

    #[test]
    fn an_area_of_two_corners_is_reported() -> Result<(), TestFailure> {
        let two = [(0.0, 0.0), (0.0, 1.0)];
        match polygon_area_km2(&two) {
            Err(CoordinateError::PolygonPoints { count }) => {
                assert_eq!(count, 2);
                Ok(())
            }
            other => Err(TestFailure::Message(format!(
                "expected a polygon point error, got {other:?}"
            ))),
        }
    }

    #[test]
    fn mercator_matches_reference_tile_coordinates() -> Result<(), TestFailure> {
        let (x, y) = mercator(SITE_LAT, SITE_LON)?;
        for (zoom, expected_x, expected_y) in
            [(4, 3.723000, 6.115042), (10, 238.272028, 391.362674)]
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
                inverse(SITE_LAT, SITE_LON, target_lat, target_lon)?;
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
    fn antimeridian_uses_the_western_edge() -> Result<(), TestFailure> {
        let (west, _) = mercator(0.0, -180.0)?;
        let (east, _) = mercator(0.0, 180.0)?;
        let world_width = 2.0 * std::f64::consts::PI * MERCATOR_RADIUS_KM;
        expect_within("west x", -world_width / 2.0, west, 1e-9)?;
        expect_within("east x", -world_width / 2.0, east, 1e-9)?;
        let (_, lon) = mercator_inverse(west, 0.0)?;
        expect_within("inverse west longitude", -180.0, lon, 1e-12)
    }

    #[test]
    fn mercator_rejects_coordinates_outside_its_domain() {
        assert!(matches!(
            mercator(86.0, 0.0),
            Err(CoordinateError::MercatorLatitude { lat }) if lat == 86.0
        ));
        let outside = MERCATOR_RADIUS_KM * std::f64::consts::PI * 2.0;
        assert!(matches!(
            mercator_inverse(0.0, outside),
            Err(CoordinateError::MercatorLatitude { .. })
        ));
        assert!(matches!(
            mercator_inverse(f64::INFINITY, 0.0),
            Err(CoordinateError::MercatorLatitude { .. })
        ));
    }

    #[test]
    fn polar_geodesics_are_finite() -> Result<(), TestFailure> {
        let (range_km, azimuth_deg) = inverse(0.0, 0.0, 0.0, 180.0)?;
        expect_within("antipodal range km", 20_003.93, range_km, 1.0)?;
        expect_within("antipodal bearing range", 0.0, azimuth_deg, 360.0)?;
        let (lat, lon) = forward(0.0, 0.0, SITE_LAT, SITE_LON)?;
        expect_within("zero range latitude", SITE_LAT, lat, 1e-12)?;
        expect_within("zero range longitude", SITE_LON, lon, 1e-12)?;
        let (lat, lon) = forward(1.0e-4, 90.0, SITE_LAT, SITE_LON)?;
        assert!(
            lat.is_finite() && lon.is_finite(),
            "near point {lat}, {lon}"
        );
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

    /// A bearing depends on direction: east from the site is 90 degrees, and
    /// the reciprocal bearing back to the site is 270 degrees.
    #[test]
    fn inverse_bearing_is_relative_to_the_origin() -> Result<(), TestFailure> {
        let (east_lat, east_lon) = forward(RANGE_KM, 90.0, SITE_LAT, SITE_LON)?;
        let (_, outward) = inverse(SITE_LAT, SITE_LON, east_lat, east_lon)?;
        let (_, inward) = inverse(east_lat, east_lon, SITE_LAT, SITE_LON)?;
        expect_within("outward bearing", 90.0, outward, 1e-9)?;
        // A geodesic's return bearing is not the exact reciprocal of its
        // outgoing bearing: meridians converge toward the pole. At this
        // latitude the 75 km return bearing is about 270.5 degrees, but it
        // must still point west rather than east.
        if !(270.0..271.0).contains(&inward) {
            return Err(TestFailure::Value {
                label: "inward bearing direction",
                expected: 270.5,
                actual: inward,
                tolerance: 0.5,
            });
        }
        Ok(())
    }

    #[test]
    fn inverse_rejects_invalid_latitude() {
        let result = inverse(SITE_LAT, SITE_LON, 91.0, 0.0);
        assert!(
            matches!(result, Err(CoordinateError::Latitude { lat }) if lat == 91.0),
            "expected Latitude error carrying 91.0, got {result:?}"
        );
    }

    #[test]
    fn inverse_rejects_invalid_longitude() {
        let result = inverse(SITE_LAT, SITE_LON, 0.0, 181.0);
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
