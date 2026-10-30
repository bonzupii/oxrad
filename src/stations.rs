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
//! The bundled WSR-88D site list, and lookup over it.

use std::fmt::{self, Display, Formatter};

use crate::error::{CatalogError, SiteIdError};

const CATALOG: &str = include_str!("../catalog/nexrad_sites.tsv");

/// A four-character radar site identifier such as `KPAH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteId {
    code: String,
}

impl SiteId {
    /// Accepts four ASCII uppercase letters or digits.
    pub fn parse(value: &str) -> Result<Self, SiteIdError> {
        if value.len() != 4 {
            return Err(SiteIdError::Length {
                value: String::from(value),
                length: value.len(),
            });
        }
        for byte in value.bytes() {
            if !(byte.is_ascii_uppercase() || byte.is_ascii_digit()) {
                return Err(SiteIdError::Character {
                    value: String::from(value),
                    byte,
                });
            }
        }
        Ok(Self {
            code: String::from(value),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.code
    }
}

impl Display for SiteId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.code)
    }
}

/// A radar site with the coordinates the source publishes for it.
#[derive(Debug, Clone)]
pub struct Station {
    pub id: SiteId,
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
}

impl Station {
    pub fn label(&self) -> String {
        format!(
            "{} · {} · {:.2}, {:.2}",
            self.id, self.name, self.lat, self.lon
        )
    }
}

fn parse_coordinate(text: &str, line: usize, longitude: bool) -> Result<f64, CatalogError> {
    let value = text
        .parse::<f64>()
        .map_err(|_| CatalogError::Number { line })?;
    if !value.is_finite() {
        return Err(CatalogError::Number { line });
    }
    if longitude {
        if !(-180.0..=180.0).contains(&value) {
            return Err(CatalogError::Longitude { line, lon: value });
        }
    } else if !(-90.0..=90.0).contains(&value) {
        return Err(CatalogError::Latitude { line, lat: value });
    }
    Ok(value)
}

fn parse_station(line: &'static str, number: usize) -> Result<Station, CatalogError> {
    let mut fields = line.split('\t');
    let (Some(id), Some(name), Some(lat), Some(lon), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return Err(CatalogError::Fields { line: number });
    };
    if name.trim().is_empty() {
        return Err(CatalogError::Name { line: number });
    }
    let id = SiteId::parse(id).map_err(|source| CatalogError::Identifier {
        line: number,
        source,
    })?;
    Ok(Station {
        id,
        name,
        lat: parse_coordinate(lat, number, false)?,
        lon: parse_coordinate(lon, number, true)?,
    })
}

/// Parses the bundled catalog. The file is fixed at build time, so a failure
/// here is a build defect that the application reports instead of guessing.
pub fn catalog() -> Result<Vec<Station>, CatalogError> {
    let mut stations: Vec<Station> = Vec::new();
    for (index, line) in CATALOG.lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let station = parse_station(trimmed, number)?;
        if stations.iter().any(|existing| existing.id == station.id) {
            return Err(CatalogError::Duplicate {
                line: number,
                id: String::from(station.id.as_str()),
            });
        }
        stations.push(station);
    }
    if stations.is_empty() {
        return Err(CatalogError::Empty);
    }
    Ok(stations)
}

/// Case-insensitive match on identifier or name.
pub fn matches(station: &Station, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    station.id.as_str().to_lowercase().contains(&query)
        || station.name.to_lowercase().contains(&query)
}

pub fn find<'a>(stations: &'a [Station], id: &SiteId) -> Option<&'a Station> {
    stations.iter().find(|station| &station.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_holds_every_bundled_site_once() -> Result<(), CatalogError> {
        let stations = catalog()?;
        assert_eq!(stations.len(), 159, "bundled WSR-88D site count");
        let mut sorted: Vec<&str> = stations.iter().map(|station| station.id.as_str()).collect();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), stations.len(), "identifiers are unique");
        Ok(())
    }

    #[test]
    fn catalog_carries_the_published_kpah_coordinates() -> Result<(), CatalogError> {
        let stations = catalog()?;
        let id =
            SiteId::parse("KPAH").map_err(|source| CatalogError::Identifier { line: 0, source })?;
        let station = find(&stations, &id).ok_or(CatalogError::Empty)?;
        assert_eq!(station.name, "Paducah");
        assert!(
            (station.lat - 37.068_33).abs() < 5e-6,
            "KPAH latitude {}",
            station.lat
        );
        assert!(
            (station.lon + 88.771_94).abs() < 5e-6,
            "KPAH longitude {}",
            station.lon
        );
        Ok(())
    }

    #[test]
    fn search_matches_identifier_and_name_without_regard_to_case() -> Result<(), CatalogError> {
        let stations = catalog()?;
        let by_name: Vec<&str> = stations
            .iter()
            .filter(|station| matches(station, "padu"))
            .map(|station| station.id.as_str())
            .collect();
        assert_eq!(by_name, vec!["KPAH"]);
        let by_id: Vec<&str> = stations
            .iter()
            .filter(|station| matches(station, "kpah"))
            .map(|station| station.id.as_str())
            .collect();
        assert_eq!(by_id, vec!["KPAH"]);
        let all = stations
            .iter()
            .filter(|station| matches(station, "  "))
            .count();
        assert_eq!(all, stations.len(), "an empty query matches every site");
        Ok(())
    }

    #[test]
    fn identifiers_outside_the_documented_shape_are_rejected() {
        assert!(matches!(
            SiteId::parse("KPA"),
            Err(SiteIdError::Length { length: 3, .. })
        ));
        assert!(matches!(
            SiteId::parse("kpah"),
            Err(SiteIdError::Character { byte: b'k', .. })
        ));
        assert!(matches!(
            SiteId::parse("KP H"),
            Err(SiteIdError::Character { byte: b' ', .. })
        ));
        assert!(SiteId::parse("KPAH").is_ok());
    }

    #[test]
    fn catalog_lines_outside_the_format_are_rejected() {
        assert!(matches!(
            parse_station("KPAH\tPaducah\t37.0", 7),
            Err(CatalogError::Fields { line: 7 })
        ));
        assert!(matches!(
            parse_station("KPAH\tPaducah\tnorth\t-88.0", 7),
            Err(CatalogError::Number { line: 7 })
        ));
        assert!(matches!(
            parse_station("KPAH\tPaducah\t91.0\t-88.0", 7),
            Err(CatalogError::Latitude { line: 7, lat }) if lat == 91.0
        ));
        assert!(matches!(
            parse_station("KP\th\t37.0\t-88.0", 7),
            Err(CatalogError::Identifier { line: 7, .. })
        ));
    }
}
