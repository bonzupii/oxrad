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
//! Terrain elevation from Copernicus DEM GLO-90, through Open-Meteo.
//!
//! The endpoint returns one value per requested coordinate. This module
//! accepts exactly one value because the registry operation asks for one
//! point; a longer array would describe a different request and is rejected
//! rather than silently taking its first member.

use serde_json::Value;

use crate::provider::FetchError;

/// The dataset a returned height belongs to.
pub const DATASET: &str = "Copernicus DEM 2021 release GLO-90";

/// One terrain height with the point it was sampled at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainRecord {
    pub latitude: f64,
    pub longitude: f64,
    /// Height above sea level in metres, as the provider returned it.
    pub elevation_m: f64,
}

impl TerrainRecord {
    pub const fn dataset(self) -> &'static str {
        DATASET
    }
}

fn shape(url: &str, field: &'static str) -> FetchError {
    FetchError::Shape {
        url: String::from(url),
        field,
    }
}

/// Parses the provider's JSON body for one requested coordinate.
///
/// # Errors
///
/// Returns [`FetchError::Json`] when the body is not JSON, and
/// [`FetchError::Shape`] when the coordinates are invalid, the elevation array
/// is absent, empty, has more than one member, or carries a non-finite number.
pub fn parse_body(
    body: &[u8],
    latitude: f64,
    longitude: f64,
    url: &str,
) -> Result<TerrainRecord, FetchError> {
    let document: Value = serde_json::from_slice(body).map_err(|source| FetchError::Json {
        url: String::from(url),
        source,
    })?;
    parse_document(&document, latitude, longitude, url)
}

/// Parses an already decoded elevation document.
///
/// # Errors
///
/// Returns [`FetchError::Shape`] under the same conditions as [`parse_body`].
pub fn parse_document(
    document: &Value,
    latitude: f64,
    longitude: f64,
    url: &str,
) -> Result<TerrainRecord, FetchError> {
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(shape(url, "latitude"));
    }
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        return Err(shape(url, "longitude"));
    }
    let values = document
        .get("elevation")
        .and_then(Value::as_array)
        .ok_or_else(|| shape(url, "elevation"))?;
    if values.len() != 1 {
        return Err(shape(url, "elevation"));
    }
    let elevation_m = values
        .first()
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| shape(url, "elevation"))?;
    Ok(TerrainRecord {
        latitude,
        longitude,
        elevation_m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://api.open-meteo.com/v1/elevation?latitude=52.52&longitude=13.41";

    #[test]
    fn a_point_elevation_keeps_its_precision_and_unit() -> Result<(), FetchError> {
        let record = parse_body(br#"{"elevation":[38.0]}"#, 52.52, 13.41, URL)?;
        assert!((record.elevation_m - 38.0).abs() < f64::EPSILON);
        assert!((record.latitude - 52.52).abs() < f64::EPSILON);
        assert!((record.longitude - 13.41).abs() < f64::EPSILON);
        assert_eq!(record.dataset(), "Copernicus DEM 2021 release GLO-90");
        Ok(())
    }

    #[test]
    fn a_negative_height_is_kept_rather_than_clamped() -> Result<(), FetchError> {
        let record = parse_body(br#"{"elevation":[-430.25]}"#, 31.5, 35.5, URL)?;
        assert!((record.elevation_m + 430.25).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn an_empty_elevation_array_is_rejected() {
        assert!(matches!(
            parse_body(br#"{"elevation":[]}"#, 52.52, 13.41, URL),
            Err(FetchError::Shape {
                field: "elevation",
                ..
            })
        ));
    }

    #[test]
    fn a_malformed_body_is_rejected() {
        assert!(matches!(
            parse_body(b"{not json", 52.52, 13.41, URL),
            Err(FetchError::Json { .. })
        ));
    }

    #[test]
    fn an_empty_body_is_rejected() {
        assert!(matches!(
            parse_body(b"", 52.52, 13.41, URL),
            Err(FetchError::Json { .. })
        ));
    }

    #[test]
    fn a_missing_elevation_is_rejected() {
        assert!(matches!(
            parse_body(br#"{"latitude":52.52}"#, 52.52, 13.41, URL),
            Err(FetchError::Shape {
                field: "elevation",
                ..
            })
        ));
    }

    #[test]
    fn a_non_numeric_elevation_is_rejected() {
        assert!(matches!(
            parse_body(br#"{"elevation":["38"]}"#, 52.52, 13.41, URL),
            Err(FetchError::Shape {
                field: "elevation",
                ..
            })
        ));
    }

    #[test]
    fn more_than_one_value_is_rejected_rather_than_truncated() {
        assert!(matches!(
            parse_body(br#"{"elevation":[38.0,39.0]}"#, 52.52, 13.41, URL),
            Err(FetchError::Shape {
                field: "elevation",
                ..
            })
        ));
    }

    #[test]
    fn an_invalid_request_coordinate_is_rejected() {
        assert!(matches!(
            parse_body(br#"{"elevation":[38.0]}"#, 91.0, 13.41, URL),
            Err(FetchError::Shape {
                field: "latitude",
                ..
            })
        ));
        assert!(matches!(
            parse_body(br#"{"elevation":[38.0]}"#, 52.52, 181.0, URL),
            Err(FetchError::Shape {
                field: "longitude",
                ..
            })
        ));
    }
}
