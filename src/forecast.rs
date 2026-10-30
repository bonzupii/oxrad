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
//! Forecast for the place the user searched, from the National Weather Service.
//!
//! The request uses the place's own coordinates. The forecast carries that
//! `Place` unchanged so the UI can state exactly which location it describes,
//! independently of the radar station supplying observations.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use serde_json::Value;

use crate::place::Place;
use crate::provider::{self, JsonError};

const NWS_POINTS: &str = "https://api.weather.gov/points";
const NWS_ACCEPT: &str = "application/geo+json";
const FORECAST_URL_PREFIX: &str = "https://api.weather.gov/";

/// One forecast period as the provider published it.
#[derive(Debug, Clone)]
pub struct ForecastPeriod {
    pub name: String,
    pub start: String,
    pub temperature: f64,
    pub unit: String,
    pub wind: String,
    pub short: String,
    pub precipitation: Option<u8>,
}

/// The forecast for one place, with its provider identity and issue time.
#[derive(Debug, Clone)]
pub struct Forecast {
    pub place: Place,
    pub updated: Option<String>,
    pub grid: String,
    pub provider_location: String,
    pub periods: Vec<ForecastPeriod>,
}

/// Why a forecast could not be produced.
#[derive(Debug)]
pub enum ForecastError {
    Provider { source: JsonError },
    Unsupported { country: String },
    NoPeriods,
    Shape { field: &'static str },
    WorkerStopped,
    WorkerPanic,
}

impl ForecastError {
    /// Whether trying this forecast again may succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Unsupported { .. }
            | Self::NoPeriods
            | Self::Shape { .. }
            | Self::WorkerStopped
            | Self::WorkerPanic => false,
        }
    }

    /// The provider's requested wait, when it supplied one.
    pub const fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Provider { source } => source.retry_after(),
            Self::Unsupported { .. }
            | Self::NoPeriods
            | Self::Shape { .. }
            | Self::WorkerStopped
            | Self::WorkerPanic => None,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Provider { source } => source.to_string(),
            Self::Unsupported { country } => format!(
                "The National Weather Service forecasts the United States only; {country} is outside its coverage."
            ),
            Self::NoPeriods => {
                String::from("The National Weather Service returned no forecast periods.")
            }
            Self::Shape { field } => {
                format!("The forecast response has no usable {field}.")
            }
            Self::WorkerStopped => String::from("The forecast worker stopped before answering."),
            Self::WorkerPanic => String::from("The forecast worker panicked while answering."),
        }
    }
}

impl Display for ForecastError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for ForecastError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Unsupported { .. }
            | Self::NoPeriods
            | Self::Shape { .. }
            | Self::WorkerStopped
            | Self::WorkerPanic => None,
        }
    }
}

/// The gridpoint request for the place's own coordinates.
pub fn points_url(place: &Place) -> Result<String, ForecastError> {
    if !place.lat.is_finite() || !(-90.0..=90.0).contains(&place.lat) {
        return Err(ForecastError::Shape {
            field: "place latitude",
        });
    }
    if !place.lon.is_finite() || !(-180.0..=180.0).contains(&place.lon) {
        return Err(ForecastError::Shape {
            field: "place longitude",
        });
    }
    Ok(format!("{NWS_POINTS}/{:.4},{:.4}", place.lat, place.lon))
}

const fn shape(field: &'static str) -> ForecastError {
    ForecastError::Shape { field }
}

fn required_text(value: &Value, field: &'static str) -> Result<String, ForecastError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(String::from)
        .ok_or_else(|| shape(field))
}

fn forecast_area(value: &Value) -> String {
    let city = value
        .get("city")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty());
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty());
    match (city, state) {
        (Some(city), Some(state)) => format!("{city}, {state}"),
        (Some(city), None) => String::from(city),
        (None, _) => String::new(),
    }
}

fn parse_precipitation(period: &Value) -> Result<Option<u8>, ForecastError> {
    let Some(value) = period
        .get("probabilityOfPrecipitation")
        .and_then(|value| value.get("value"))
    else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let number = value
        .as_u64()
        .ok_or_else(|| shape("precipitation probability"))?;
    let probability = u8::try_from(number).map_err(|_| shape("precipitation probability"))?;
    Ok(Some(probability))
}

fn parse_period(period: &Value) -> Result<ForecastPeriod, ForecastError> {
    let temperature = period
        .get("temperature")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| shape("temperature"))?;
    let precipitation = parse_precipitation(period)?;
    Ok(ForecastPeriod {
        name: required_text(period, "name")?,
        start: required_text(period, "startTime")?,
        temperature,
        unit: required_text(period, "temperatureUnit")?,
        wind: required_text(period, "windSpeed")?,
        short: required_text(period, "shortForecast")?,
        precipitation,
    })
}

fn fetch_periods(url: &str) -> Result<(Option<String>, Vec<ForecastPeriod>), ForecastError> {
    let document =
        provider::get_json(url, NWS_ACCEPT).map_err(|source| ForecastError::Provider { source })?;
    let properties = document
        .get("properties")
        .ok_or_else(|| shape("forecast properties"))?;
    let updated = properties
        .get("updateTime")
        .and_then(Value::as_str)
        .or_else(|| properties.get("generatedAt").and_then(Value::as_str))
        .map(String::from);
    let periods_json = properties
        .get("periods")
        .and_then(Value::as_array)
        .ok_or_else(|| shape("forecast periods"))?;
    let mut periods = Vec::with_capacity(periods_json.len());
    for period in periods_json {
        periods.push(parse_period(period)?);
    }
    if periods.is_empty() {
        return Err(ForecastError::NoPeriods);
    }
    Ok((updated, periods))
}

/// Fetches the forecast for the place, using only the place's coordinates.
pub fn fetch(place: &Place) -> Result<Forecast, ForecastError> {
    if !place.in_united_states() {
        let country = if place.country.trim().is_empty() {
            String::from("this location")
        } else {
            place.country.clone()
        };
        return Err(ForecastError::Unsupported { country });
    }
    let points = points_url(place)?;
    let document = provider::get_json(&points, NWS_ACCEPT)
        .map_err(|source| ForecastError::Provider { source })?;
    let properties = document
        .get("properties")
        .ok_or_else(|| shape("gridpoint properties"))?;
    let forecast_url = properties
        .get("forecast")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with(FORECAST_URL_PREFIX))
        .ok_or_else(|| shape("forecast URL"))?;
    let grid_id = properties
        .get("gridId")
        .and_then(Value::as_str)
        .ok_or_else(|| shape("forecast office"))?;
    let grid_x = properties
        .get("gridX")
        .and_then(Value::as_i64)
        .ok_or_else(|| shape("grid X"))?;
    let grid_y = properties
        .get("gridY")
        .and_then(Value::as_i64)
        .ok_or_else(|| shape("grid Y"))?;
    let grid = format!("{grid_id} {grid_x},{grid_y}");
    let provider_location = properties
        .get("relativeLocation")
        .and_then(|value| value.get("properties"))
        .map_or_else(String::new, forecast_area);
    let (updated, periods) = fetch_periods(forecast_url)?;
    Ok(Forecast {
        place: place.clone(),
        updated,
        grid,
        provider_location,
        periods,
    })
}

struct ForecastRequest {
    place: Place,
    generation: u64,
}

struct ForecastMessage {
    generation: u64,
    place: Place,
    result: Result<Forecast, ForecastError>,
}

/// The answer to one forecast request.
#[derive(Debug)]
pub enum ForecastOutcome {
    Ready(Forecast),
    Failed { place: Place, error: ForecastError },
}

/// Runs forecast requests off the UI thread, one at a time.
pub struct ForecastFeed {
    requests: Sender<ForecastRequest>,
    results: Receiver<ForecastMessage>,
    generation: u64,
    latest: Option<Place>,
    stopped: bool,
}

impl ForecastFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<ForecastRequest>();
        let (result_sender, results) = mpsc::channel::<ForecastMessage>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let place = request.place;
                let result = catch_unwind(AssertUnwindSafe(|| fetch(&place)))
                    .unwrap_or_else(|_| Err(ForecastError::WorkerPanic));
                let message = ForecastMessage {
                    generation: request.generation,
                    place,
                    result,
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
            latest: None,
            stopped: false,
        }
    }

    pub fn start(&mut self, place: &Place) -> Result<(), ForecastError> {
        self.generation = self.generation.wrapping_add(1);
        self.latest = Some(place.clone());
        let request = ForecastRequest {
            place: place.clone(),
            generation: self.generation,
        };
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(ForecastError::WorkerStopped);
        }
        Ok(())
    }

    /// The newest answer, with earlier superseded requests discarded.
    pub fn poll(&mut self) -> Option<ForecastOutcome> {
        loop {
            match self.results.try_recv() {
                Ok(message) => {
                    if message.generation == self.generation {
                        return Some(match message.result {
                            Ok(forecast) => ForecastOutcome::Ready(forecast),
                            Err(error) => ForecastOutcome::Failed {
                                place: message.place,
                                error,
                            },
                        });
                    }
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    if self.stopped {
                        return None;
                    }
                    self.stopped = true;
                    let place = self.latest.clone()?;
                    return Some(ForecastOutcome::Failed {
                        place,
                        error: ForecastError::WorkerStopped,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TestError;

    fn bowling_green() -> Result<Place, TestError> {
        Ok(Place::new(
            "Bowling Green",
            "Kentucky",
            "United States",
            "us",
            36.990_3,
            -86.443_6,
        )?)
    }

    #[test]
    fn the_gridpoint_request_uses_the_town_coordinates() -> Result<(), TestError> {
        let place = bowling_green()?;
        let url = points_url(&place)?;
        assert!(url.contains("36.9903"), "request URL {url}");
        assert!(url.contains("-86.4436"), "request URL {url}");
        assert!(
            !url.contains("37.0683"),
            "the request must not use the KPAH station latitude"
        );
        assert!(
            !url.contains("-88.7719"),
            "the request must not use the KPAH station longitude"
        );
        Ok(())
    }

    #[test]
    fn an_invalid_precipitation_probability_is_not_silently_dropped() {
        assert!(matches!(
            parse_precipitation(&serde_json::json!({
                "probabilityOfPrecipitation": { "value": 300 }
            })),
            Err(ForecastError::Shape {
                field: "precipitation probability"
            })
        ));
        assert!(matches!(
            parse_precipitation(&serde_json::json!({
                "probabilityOfPrecipitation": { "value": null }
            })),
            Ok(None)
        ));
    }

    #[test]
    fn a_non_united_states_place_reports_outside_coverage() -> Result<(), TestError> {
        let place = Place::new("Dublin", "Leinster", "Ireland", "ie", 53.349_8, -6.260_3)?;
        match fetch(&place) {
            Err(ForecastError::Unsupported { country }) => {
                assert_eq!(country, "Ireland");
                Ok(())
            }
            other => Err(TestError::from(format!(
                "expected Unsupported, got {other:?}"
            ))),
        }
    }

    /// Exercises the real provider with the searched town's coordinates and
    /// verifies the returned forecast describes that town, not the radar site.
    #[test]
    fn the_live_forecast_belongs_to_the_searched_town() -> Result<(), TestError> {
        let place = bowling_green()?;
        let forecast = fetch(&place)?;
        assert_eq!(forecast.place, place, "the forecast carries its place");
        assert!(
            !forecast.periods.is_empty(),
            "the provider returned no periods"
        );
        let first = forecast.periods.first().ok_or("no first period")?;
        assert!(!first.name.is_empty(), "period name");
        assert!(
            (first.temperature > -80.0 && first.temperature < 140.0),
            "temperature {} {}",
            first.temperature,
            first.unit
        );
        assert!(
            forecast.grid.split_whitespace().count() == 2,
            "grid {}",
            forecast.grid
        );
        assert!(
            !forecast.provider_location.is_empty(),
            "the provider named no forecast area"
        );
        assert!(
            forecast
                .updated
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
            "the provider supplied no issue time"
        );
        Ok(())
    }
}
