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
//! Town and place identity, place search, and nearest-station selection.
//!
//! A `Place` is the location the user asked about, and it is the only input to
//! the forecast. The radar station is selected separately from that place.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::CoordinateError;
use crate::projection;
use crate::provider::{self, JsonError};
use crate::stations::Station;

const PHOTON_SEARCH: &str = "https://photon.komoot.io/api/";
const SEARCH_LIMIT: usize = 10;

/// The town or place whose weather the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    pub region: String,
    pub country: String,
    pub country_code: String,
    pub lat: f64,
    pub lon: f64,
}

impl Place {
    /// Validates the identity a provider returned before it is stored.
    pub fn new(
        name: &str,
        region: &str,
        country: &str,
        country_code: &str,
        lat: f64,
        lon: f64,
    ) -> Result<Self, PlaceError> {
        let name = valid_text(name, "name")?;
        let region = valid_text(region, "region")?;
        let country = valid_text(country, "country")?;
        if name.is_empty() {
            return Err(PlaceError::Name);
        }
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err(PlaceError::Latitude { lat });
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return Err(PlaceError::Longitude { lon });
        }
        Ok(Self {
            name,
            region,
            country,
            country_code: country_code.trim().to_lowercase(),
            lat,
            lon,
        })
    }

    /// The name with the administrative region when the provider supplied one.
    pub fn label(&self) -> String {
        if self.region.trim().is_empty() {
            format!("{}, {}", self.name, self.country)
        } else {
            format!("{}, {}", self.name, self.region)
        }
    }

    pub fn coordinates(&self) -> String {
        format!("{:.4}, {:.4}", self.lat, self.lon)
    }

    pub fn same_place(&self, other: &Self) -> bool {
        self.name.eq_ignore_ascii_case(&other.name)
            && self.country_code == other.country_code
            && (self.lat - other.lat).abs() < 1e-4
            && (self.lon - other.lon).abs() < 1e-4
    }

    pub fn in_united_states(&self) -> bool {
        self.country_code == "us"
    }
}

fn valid_text(text: &str, field: &'static str) -> Result<String, PlaceError> {
    let trimmed = text.trim();
    if trimmed
        .chars()
        .any(|character| character == '"' || character.is_control())
    {
        return Err(PlaceError::Text { field });
    }
    Ok(trimmed.to_owned())
}

/// Why a place identity or nearest-station query was rejected.
#[derive(Debug)]
pub enum PlaceError {
    Name,
    Text { field: &'static str },
    Latitude { lat: f64 },
    Longitude { lon: f64 },
    Coordinate { source: CoordinateError },
}

impl PlaceError {
    fn message(&self) -> String {
        match self {
            Self::Name => String::from("The place has no name."),
            Self::Text { field } => {
                format!("The place {field} contains a quote or control character.")
            }
            Self::Latitude { lat } => format!("Latitude {lat} is outside -90 to 90."),
            Self::Longitude { lon } => format!("Longitude {lon} is outside -180 to 180."),
            Self::Coordinate { source } => format!("Place geometry failed: {source}"),
        }
    }
}

impl Display for PlaceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for PlaceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Coordinate { source } => Some(source),
            Self::Name | Self::Text { .. } | Self::Latitude { .. } | Self::Longitude { .. } => None,
        }
    }
}

/// The station closest to a place, with the distance and bearing from it.
#[derive(Debug, Clone)]
pub struct NearestStation {
    pub station: Station,
    pub distance_km: f64,
    pub bearing_deg: f64,
}

/// Selects the nearest station by geodesic distance from the place.
pub fn nearest_station(
    stations: &[Station],
    lat: f64,
    lon: f64,
) -> Result<Option<NearestStation>, PlaceError> {
    let mut best: Option<NearestStation> = None;
    for station in stations {
        let (distance_km, bearing_deg) = projection::inverse(lat, lon, station.lat, station.lon)
            .map_err(|source| PlaceError::Coordinate { source })?;
        if best
            .as_ref()
            .is_none_or(|current| distance_km < current.distance_km)
        {
            best = Some(NearestStation {
                station: station.clone(),
                distance_km,
                bearing_deg,
            });
        }
    }
    Ok(best)
}

/// A sixteen-point compass label for a bearing.
pub const fn compass(azimuth_deg: f64) -> &'static str {
    let index = ((azimuth_deg + 11.25) / 22.5) as usize % 16;
    match index {
        0 => "north",
        1 => "north-northeast",
        2 => "northeast",
        3 => "east-northeast",
        4 => "east",
        5 => "east-southeast",
        6 => "southeast",
        7 => "south-southeast",
        8 => "south",
        9 => "south-southwest",
        10 => "southwest",
        11 => "west-southwest",
        12 => "west",
        13 => "west-northwest",
        14 => "northwest",
        _ => "north-northwest",
    }
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(String::from)
}

fn address_field(address: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = text_field(address, key) {
            return Some(text);
        }
    }
    None
}

fn parse_feature(feature: &Value, url: &str) -> Result<Option<Place>, JsonError> {
    let Some(properties) = feature.get("properties") else {
        return Ok(None);
    };
    let Some(coordinates) = feature
        .get("geometry")
        .and_then(|geometry| geometry.get("coordinates"))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };
    let (Some(lon), Some(lat)) = (
        coordinates.first().and_then(Value::as_f64),
        coordinates.get(1).and_then(Value::as_f64),
    ) else {
        return Ok(None);
    };
    let name = text_field(properties, "name").or_else(|| {
        address_field(
            properties,
            &[
                "city", "town", "village", "hamlet", "locality", "district", "county",
            ],
        )
    });
    let Some(name) = name else {
        return Ok(None);
    };
    let region = address_field(properties, &["state", "county"]);
    let country = text_field(properties, "country").ok_or_else(|| JsonError::Shape {
        url: String::from(url),
        field: "place country",
    })?;
    let country_code = text_field(properties, "countrycode").unwrap_or_default();
    Place::new(
        &name,
        region.as_deref().unwrap_or(""),
        &country,
        &country_code,
        lat,
        lon,
    )
    .map(Some)
    .map_err(|source| JsonError::Shape {
        url: String::from(url),
        field: match source {
            PlaceError::Name => "place name",
            PlaceError::Text { .. } => "place text",
            PlaceError::Latitude { .. } => "place latitude",
            PlaceError::Longitude { .. } => "place longitude",
            PlaceError::Coordinate { .. } => "place coordinates",
        },
    })
}

/// Searches the geocoder for towns and places by name.
///
/// When the first answer contains no exact, prefix, or word-prefix match, one
/// adjacent transposition of the query is retried (for example `cihcago` for
/// `chicago`). The original answer is kept if the correction does not produce
/// a closer match, so a failed retry never hides usable results.
pub fn search(query: &str) -> Result<Vec<Place>, JsonError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let places = fetch_places(trimmed)?;
    if places.iter().any(|place| match_rank(place, trimmed) <= 2) {
        return Ok(places);
    }
    let Some(corrected) = first_inversion_variant(trimmed) else {
        return Ok(places);
    };
    let Ok(mut corrected_places) = fetch_places(&corrected) else {
        return Ok(places);
    };
    if corrected_places
        .iter()
        .any(|place| match_rank(place, &corrected) <= 2)
    {
        rank_places(&mut corrected_places, &corrected);
        Ok(corrected_places)
    } else {
        Ok(places)
    }
}

fn fetch_places(query: &str) -> Result<Vec<Place>, JsonError> {
    let url = format!(
        "{PHOTON_SEARCH}?q={}&limit={SEARCH_LIMIT}",
        provider::encode_query(query)
    );
    let document = provider::get_json(&url, "application/json")?;
    let features = document
        .get("features")
        .and_then(Value::as_array)
        .ok_or_else(|| JsonError::Shape {
            url: url.clone(),
            field: "place results",
        })?;
    let mut places: Vec<Place> = Vec::new();
    for feature in features {
        if let Some(place) = parse_feature(feature, &url)? {
            merge_place(&mut places, place);
        }
    }
    rank_places(&mut places, query);
    Ok(places)
}

/// The query with its first descending letter pair swapped.
fn first_inversion_variant(query: &str) -> Option<String> {
    if query.chars().count() < 4 || !query.is_ascii() {
        return None;
    }
    let mut characters: Vec<char> = query.chars().collect();
    let index = characters
        .windows(2)
        .position(|pair| match (pair.first(), pair.get(1)) {
            (Some(left), Some(right)) => left.to_ascii_lowercase() > right.to_ascii_lowercase(),
            _ => false,
        })?;
    characters.swap(index, index + 1);
    Some(characters.into_iter().collect())
}

/// Orders results so exact names precede prefix matches and unrelated names.
fn rank_places(places: &mut [Place], query: &str) {
    places.sort_by_key(|place| match_rank(place, query));
}

fn match_rank(place: &Place, query: &str) -> u8 {
    let query = query.trim().to_lowercase();
    let name = place.name.to_lowercase();
    if name == query {
        0
    } else if name.starts_with(&query) {
        1
    } else if name
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.starts_with(&query))
    {
        2
    } else {
        3
    }
}

/// Drops results with the same name, country, and region so the picker does
/// not repeat indistinguishable labels.
fn merge_place(places: &mut Vec<Place>, place: Place) {
    let duplicate = places.iter().any(|existing| {
        existing.same_place(&place)
            || (existing.name.eq_ignore_ascii_case(&place.name)
                && existing.country_code == place.country_code
                && existing.region.eq_ignore_ascii_case(&place.region))
    });
    if !duplicate {
        places.push(place);
    }
}

struct SearchRequest {
    query: String,
    generation: u64,
}

struct SearchMessage {
    generation: u64,
    outcome: SearchOutcome,
}

/// The answer to one place search.
#[derive(Debug)]
pub enum SearchOutcome {
    Results { places: Vec<Place> },
    Failed { query: String, error: JsonError },
}

/// Runs place searches off the UI thread, one at a time.
///
/// The worker spaces requests to stay within the public geocoder's fair-use
/// expectations and caches successful answers for this session. Results from
/// superseded queries are delivered with their generation and discarded by
/// `poll`.
pub struct PlaceSearch {
    requests: Sender<SearchRequest>,
    results: Receiver<SearchMessage>,
    generation: u64,
    latest_query: String,
    stopped: bool,
}

impl PlaceSearch {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<SearchRequest>();
        let (result_sender, results) = mpsc::channel::<SearchMessage>();
        std::thread::spawn(move || {
            let mut cache: HashMap<String, Vec<Place>> = HashMap::new();
            let mut last_request: Option<Instant> = None;
            while let Ok(request) = request_receiver.recv() {
                let key = request.query.trim().to_lowercase();
                if let Some(places) = cache.get(&key) {
                    let message = SearchMessage {
                        generation: request.generation,
                        outcome: SearchOutcome::Results {
                            places: places.clone(),
                        },
                    };
                    if result_sender.send(message).is_err() {
                        return;
                    }
                    continue;
                }
                if let Some(last) = last_request {
                    let elapsed = last.elapsed();
                    let policy = Duration::from_secs(1);
                    if elapsed < policy {
                        std::thread::sleep(policy - elapsed);
                    }
                }
                let outcome = match catch_unwind(AssertUnwindSafe(|| search(&request.query))) {
                    Ok(Ok(places)) => SearchOutcome::Results { places },
                    Ok(Err(error)) => SearchOutcome::Failed {
                        query: request.query,
                        error,
                    },
                    Err(_) => SearchOutcome::Failed {
                        query: request.query,
                        error: JsonError::WorkerPanic,
                    },
                };
                last_request = Some(Instant::now());
                if let SearchOutcome::Results { places } = &outcome {
                    cache.insert(key, places.clone());
                }
                let message = SearchMessage {
                    generation: request.generation,
                    outcome,
                };
                if result_sender.send(message).is_err() {
                    return;
                }
            }
        });
        Self {
            requests,
            results,
            generation: 0,
            latest_query: String::new(),
            stopped: false,
        }
    }

    pub fn start(&mut self, query: String) -> Result<(), JsonError> {
        self.generation = self.generation.wrapping_add(1);
        self.latest_query = query.clone();
        let request = SearchRequest {
            query,
            generation: self.generation,
        };
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(JsonError::WorkerStopped);
        }
        Ok(())
    }

    /// Discards any answer still in flight after the user chooses a station.
    pub const fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// The newest answer, with earlier superseded searches discarded.
    pub fn poll(&mut self) -> Option<SearchOutcome> {
        loop {
            match self.results.try_recv() {
                Ok(message) => {
                    if message.generation == self.generation {
                        return Some(message.outcome);
                    }
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    if self.stopped {
                        return None;
                    }
                    self.stopped = true;
                    return Some(SearchOutcome::Failed {
                        query: self.latest_query.clone(),
                        error: JsonError::WorkerStopped,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::product::RadarProduct;
    use crate::stations::{RadarSource, SiteId, Station, StationKind};
    use crate::test_support::TestError;

    const PADUCAH_LAT: f64 = 37.083_4;
    const PADUCAH_LON: f64 = -88.6;
    const BOWLING_GREEN_LAT: f64 = 36.990_3;
    const BOWLING_GREEN_LON: f64 = -86.443_6;

    fn station(id: &str, name: &str, lat: f64, lon: f64) -> Result<Station, TestError> {
        Ok(Station {
            id: SiteId::parse(id)?,
            name: String::from(name),
            lat,
            lon,
            elevation_m: None,
            kind: StationKind::Wsr88d,
            source: RadarSource::Nexrad,
            status: None,
            last_data: None,
        })
    }

    fn catalog() -> Result<Vec<Station>, TestError> {
        assert_eq!(RadarProduct::ALL.len(), 7);
        Ok(vec![
            station("KPAH", "Paducah", 37.068_33, -88.771_94)?,
            station("KHPX", "Fort Campbell", 36.736_9, -87.285_0)?,
            station("KOHX", "Nashville", 36.247_2, -86.562_5)?,
        ])
    }

    fn id(code: &str) -> Result<SiteId, TestError> {
        Ok(SiteId::parse(code)?)
    }

    #[test]
    fn compass_names_every_octant_and_quadrant() {
        for (azimuth, expected) in [
            (0.0, "north"),
            (45.0, "northeast"),
            (90.0, "east"),
            (135.0, "southeast"),
            (180.0, "south"),
            (225.0, "southwest"),
            (270.0, "west"),
            (315.0, "northwest"),
            (359.0, "north"),
        ] {
            assert_eq!(compass(azimuth), expected, "azimuth {azimuth}");
        }
    }

    #[test]
    fn cancelling_invalidates_a_pending_search_generation() {
        let mut search = PlaceSearch::new();
        let generation = search.generation;
        search.cancel();
        assert_ne!(search.generation, generation);
    }

    #[test]
    fn a_town_with_a_station_selects_that_station() -> Result<(), TestError> {
        let stations = catalog()?;
        let nearest =
            nearest_station(&stations, PADUCAH_LAT, PADUCAH_LON)?.ok_or("no station selected")?;
        assert_eq!(nearest.station.id, id("KPAH")?);
        assert!(
            (10.0..20.0).contains(&nearest.distance_km),
            "KPAH distance {} km",
            nearest.distance_km
        );
        Ok(())
    }

    /// Bowling Green has no radar of its own. Fort Campbell's KHPX is at
    /// 36.7369 N 87.2850 W and Old Hickory's KOHX is farther away.
    #[test]
    fn a_town_without_a_station_selects_the_nearest_one_outside_it() -> Result<(), TestError> {
        let stations = catalog()?;
        let nearest = nearest_station(&stations, BOWLING_GREEN_LAT, BOWLING_GREEN_LON)?
            .ok_or("no station selected")?;
        assert_eq!(nearest.station.id, id("KHPX")?);
        assert!(
            (60.0..100.0).contains(&nearest.distance_km),
            "KHPX distance {} km",
            nearest.distance_km
        );
        Ok(())
    }

    #[test]
    fn an_empty_catalog_selects_no_station() -> Result<(), TestError> {
        assert!(nearest_station(&[], BOWLING_GREEN_LAT, BOWLING_GREEN_LON)?.is_none());
        Ok(())
    }

    #[test]
    fn place_identity_normalizes_country_and_rejects_config_breaking_text() -> Result<(), TestError>
    {
        let place = Place::new(
            "  Bowling Green  ",
            " Kentucky ",
            "United States",
            "US",
            36.99,
            -86.44,
        )?;
        assert_eq!(place.name, "Bowling Green");
        assert_eq!(place.country_code, "us");
        assert!(matches!(
            Place::new("Bad\"Name", "", "", "us", 0.0, 0.0),
            Err(PlaceError::Text { field: "name" })
        ));
        Ok(())
    }

    #[test]
    fn features_without_usable_coordinates_are_skipped() {
        let feature = serde_json::json!({
            "properties": {"name": "Nowhere", "country": "United States"},
            "geometry": {"type": "Point", "coordinates": ["not-a-number"]}
        });
        assert!(matches!(
            parse_feature(&feature, "https://example.invalid"),
            Ok(None)
        ));
    }

    #[test]
    fn photon_features_carry_identity_and_region() -> Result<(), TestError> {
        let feature = serde_json::json!({
            "properties": {
                "name": "Paducah",
                "state": "Kentucky",
                "country": "United States",
                "countrycode": "US"
            },
            "geometry": {"type": "Point", "coordinates": [-88.6, 37.08]}
        });
        let place = parse_feature(&feature, "https://example.invalid")?.ok_or("no place")?;
        assert_eq!(place.name, "Paducah");
        assert_eq!(place.region, "Kentucky");
        assert!(place.in_united_states());
        Ok(())
    }

    #[test]
    fn ranking_prefers_exact_then_prefix_matches() {
        let place = |name: &str| Place {
            name: String::from(name),
            region: String::new(),
            country: String::new(),
            country_code: String::new(),
            lat: 0.0,
            lon: 0.0,
        };
        let mut places = vec![place("Chic"), place("Cislago"), place("Chicago")];
        rank_places(&mut places, "chic");
        assert_eq!(places[0].name, "Chic");
        assert_eq!(places[1].name, "Chicago");
        assert_eq!(places[2].name, "Cislago");
    }

    #[test]
    fn a_transposed_query_gains_one_corrected_variant() {
        assert_eq!(
            first_inversion_variant("cihcago").as_deref(),
            Some("chicago")
        );
        assert_eq!(first_inversion_variant("abcdef"), None);
        assert_eq!(first_inversion_variant("abc"), None);
    }

    #[test]
    fn duplicate_display_labels_collapse() {
        let place = |region: &str, lat: f64| Place {
            name: String::from("Chic"),
            region: String::from(region),
            country: String::from("France"),
            country_code: String::from("fr"),
            lat,
            lon: 1.0,
        };
        let mut places = Vec::new();
        merge_place(&mut places, place("Occitanie", 43.0));
        merge_place(&mut places, place("Occitanie", 43.0));
        merge_place(&mut places, place("Nouvelle-Aquitaine", 44.0));
        assert_eq!(places.len(), 2);
    }

    #[test]
    fn place_identity_rejects_impossible_coordinates() {
        assert!(matches!(
            Place::new("Nowhere", "", "", "us", 91.0, 0.0),
            Err(PlaceError::Latitude { lat }) if lat == 91.0
        ));
        assert!(matches!(
            Place::new("Nowhere", "", "", "us", 0.0, 181.0),
            Err(PlaceError::Longitude { lon }) if lon == 181.0
        ));
        assert!(matches!(
            Place::new("  ", "", "", "us", 0.0, 0.0),
            Err(PlaceError::Name)
        ));
    }

    /// Exercises the real geocoder: a town with its own radar, a town without
    /// one, an ambiguous name, and a query with no match. Requests are spaced
    /// by the geocoder's one-per-second policy, so this test is serial.
    #[test]
    fn place_search_resolves_towns_and_reports_ambiguity() -> Result<(), TestError> {
        let stations = catalog()?;

        let paducah = search("Paducah, Kentucky")?;
        let first = paducah.first().ok_or("Paducah returned no place")?;
        assert!(
            first.name.to_lowercase().contains("paducah"),
            "resolved {}",
            first.name
        );
        assert!(
            first.in_united_states(),
            "country code {}",
            first.country_code
        );
        let nearest = nearest_station(&stations, first.lat, first.lon)?
            .ok_or("Paducah has no nearest station")?;
        assert_eq!(nearest.station.id, id("KPAH")?);

        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let bowling_green = search("Bowling Green, Kentucky")?;
        let town = bowling_green
            .first()
            .ok_or("Bowling Green returned no place")?;
        assert!(
            town.name.to_lowercase().contains("bowling green"),
            "resolved {}",
            town.name
        );
        let nearest = nearest_station(&stations, town.lat, town.lon)?
            .ok_or("Bowling Green has no nearest station")?;
        assert_eq!(nearest.station.id, id("KHPX")?);
        assert!(
            nearest.distance_km > 30.0,
            "KHPX is outside the town: {} km",
            nearest.distance_km
        );

        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let springfield = search("Springfield")?;
        let regions: Vec<&str> = springfield
            .iter()
            .map(|place| place.region.as_str())
            .collect();
        let mut unique: Vec<&str> = regions.clone();
        unique.sort_unstable();
        unique.dedup();
        assert!(
            unique.len() >= 2,
            "Springfield resolved to one region: {regions:?}"
        );

        std::thread::sleep(std::time::Duration::from_millis(1_100));
        let none = search("zzzzqqqqxxyy")?;
        assert!(none.is_empty(), "unexpected match {none:?}");
        Ok(())
    }
}
