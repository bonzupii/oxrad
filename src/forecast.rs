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
//! Global weather and forecast from Open-Meteo.
//!
//! The request uses the searched place's coordinates. The forecast carries
//! that `Place` unchanged so the UI can state exactly which location it
//! describes, independently of the radar station supplying observations.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use serde_json::Value;

use crate::place::Place;
use crate::provider::{self, JsonError};

const OPEN_METEO: &str = "https://api.open-meteo.com/v1/forecast";
const FORECAST_DAYS: u8 = 7;
const HOURLY_LIMIT: usize = 24;
const PROVIDER: &str = "Open-Meteo";

/// Current conditions at the selected place.
#[derive(Debug, Clone)]
pub struct CurrentWeather {
    pub observed: String,
    pub temperature: f64,
    pub apparent: f64,
    pub humidity: f64,
    pub precipitation: f64,
    pub wind: String,
    pub short: String,
    pub unit: String,
}

/// One hourly or daily forecast period.
#[derive(Debug, Clone)]
pub struct ForecastPeriod {
    pub name: String,
    pub start: String,
    pub temperature: f64,
    pub minimum: Option<f64>,
    pub unit: String,
    pub wind: String,
    pub short: String,
    pub precipitation: Option<u8>,
}

/// The forecast for one place, with its provider identity and local timezone.
#[derive(Debug, Clone)]
pub struct Forecast {
    pub place: Place,
    pub provider: &'static str,
    pub timezone: String,
    pub timezone_abbreviation: String,
    pub current: CurrentWeather,
    pub hourly: Vec<ForecastPeriod>,
    pub daily: Vec<ForecastPeriod>,
}

/// Why a forecast could not be produced.
#[derive(Debug)]
pub enum ForecastError {
    Provider { source: JsonError },
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
            Self::NoPeriods | Self::Shape { .. } | Self::WorkerStopped | Self::WorkerPanic => false,
        }
    }

    /// The provider's requested wait, when it supplied one.
    pub const fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Provider { source } => source.retry_after(),
            Self::NoPeriods | Self::Shape { .. } | Self::WorkerStopped | Self::WorkerPanic => None,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Provider { source } => source.to_string(),
            Self::NoPeriods => String::from("Open-Meteo returned no forecast periods."),
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
            Self::NoPeriods | Self::Shape { .. } | Self::WorkerStopped | Self::WorkerPanic => None,
        }
    }
}

const fn shape(field: &'static str) -> ForecastError {
    ForecastError::Shape { field }
}

/// The forecast request for the place's own coordinates.
pub fn request_url(place: &Place) -> Result<String, ForecastError> {
    if !place.lat.is_finite() || !(-90.0..=90.0).contains(&place.lat) {
        return Err(shape("place latitude"));
    }
    if !place.lon.is_finite() || !(-180.0..=180.0).contains(&place.lon) {
        return Err(shape("place longitude"));
    }
    Ok(format!(
        "{OPEN_METEO}?latitude={:.4}&longitude={:.4}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m,wind_direction_10m\
         &hourly=temperature_2m,precipitation_probability,weather_code,wind_speed_10m\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max\
         &timezone=auto&forecast_days={FORECAST_DAYS}",
        place.lat, place.lon
    ))
}

fn object<'a>(value: &'a Value, field: &'static str) -> Result<&'a Value, ForecastError> {
    value
        .get(field)
        .filter(|value| value.is_object())
        .ok_or_else(|| shape(field))
}

fn text(value: &Value, field: &'static str) -> Result<String, ForecastError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(String::from)
        .ok_or_else(|| shape(field))
}

fn number(value: &Value, field: &'static str) -> Result<f64, ForecastError> {
    value
        .get(field)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| shape(field))
}

fn array<'a>(value: &'a Value, field: &'static str) -> Result<&'a [Value], ForecastError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| shape(field))
}

fn array_text(array: &[Value], index: usize, field: &'static str) -> Result<String, ForecastError> {
    array
        .get(index)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(String::from)
        .ok_or_else(|| shape(field))
}

fn array_number(array: &[Value], index: usize, field: &'static str) -> Result<f64, ForecastError> {
    array
        .get(index)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| shape(field))
}

fn array_probability(
    array: &[Value],
    index: usize,
    field: &'static str,
) -> Result<Option<u8>, ForecastError> {
    let Some(value) = array.get(index) else {
        return Err(shape(field));
    };
    if value.is_null() {
        return Ok(None);
    }
    let number = value.as_f64().ok_or_else(|| shape(field))?;
    if !number.is_finite() || !(0.0..=100.0).contains(&number) {
        return Err(shape(field));
    }
    let rounded = number.round();
    u8::try_from(rounded as u32)
        .map(Some)
        .map_err(|_| shape(field))
}

fn wind(degrees: f64, speed: f64, unit: &str) -> String {
    let direction = if !degrees.is_finite() {
        String::from("direction unavailable")
    } else {
        let headings = [
            "north",
            "northeast",
            "east",
            "southeast",
            "south",
            "southwest",
            "west",
            "northwest",
        ];
        let index = ((degrees.rem_euclid(360.0) + 22.5) / 45.0).floor() as usize % headings.len();
        headings
            .get(index)
            .map_or_else(|| String::from("unknown"), |heading| String::from(*heading))
    };
    format!("{speed:.0} {unit} from the {direction}")
}

const fn weather_text(code: u64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 => "snow",
        77 => "snow grains",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "weather code not supplied",
    }
}

fn weather_code(codes: &[Value], index: usize, field: &'static str) -> Result<u64, ForecastError> {
    codes
        .get(index)
        .and_then(Value::as_u64)
        .ok_or_else(|| shape(field))
}

fn current(document: &Value, unit: &str, wind_unit: &str) -> Result<CurrentWeather, ForecastError> {
    let current = object(document, "current")?;
    let degrees = number(current, "wind_direction_10m")?;
    let speed = number(current, "wind_speed_10m")?;
    Ok(CurrentWeather {
        observed: text(current, "time")?,
        temperature: number(current, "temperature_2m")?,
        apparent: number(current, "apparent_temperature")?,
        humidity: number(current, "relative_humidity_2m")?,
        precipitation: number(current, "precipitation")?,
        wind: wind(degrees, speed, wind_unit),
        short: String::from(weather_text(
            current
                .get("weather_code")
                .and_then(Value::as_u64)
                .ok_or_else(|| shape("current weather code"))?,
        )),
        unit: String::from(unit),
    })
}

fn hourly(
    document: &Value,
    unit: &str,
    wind_unit: &str,
) -> Result<Vec<ForecastPeriod>, ForecastError> {
    let hourly = object(document, "hourly")?;
    let times = array(hourly, "time")?;
    let temperatures = array(hourly, "temperature_2m")?;
    let winds = array(hourly, "wind_speed_10m")?;
    let codes = array(hourly, "weather_code")?;
    let probabilities = hourly
        .get("precipitation_probability")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let count = times.len().min(HOURLY_LIMIT);
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut periods = Vec::with_capacity(count);
    for index in 0..count {
        let start = array_text(times, index, "hourly time")?;
        let name = start
            .get(11..16)
            .map_or_else(|| start.clone(), String::from);
        periods.push(ForecastPeriod {
            name,
            precipitation: probabilities
                .map(|values| array_probability(values, index, "hourly precipitation probability"))
                .transpose()?
                .flatten(),
            start,
            temperature: array_number(temperatures, index, "hourly temperature")?,
            minimum: None,
            unit: String::from(unit),
            wind: format!(
                "{:.0} {wind_unit}",
                array_number(winds, index, "hourly wind")?
            ),
            short: String::from(weather_text(weather_code(
                codes,
                index,
                "hourly weather code",
            )?)),
        });
    }
    Ok(periods)
}

fn daily(document: &Value, unit: &str) -> Result<Vec<ForecastPeriod>, ForecastError> {
    let daily = object(document, "daily")?;
    let times = array(daily, "time")?;
    let maximums = array(daily, "temperature_2m_max")?;
    let minimums = array(daily, "temperature_2m_min")?;
    let codes = array(daily, "weather_code")?;
    let probabilities = daily
        .get("precipitation_probability_max")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let count = times.len().min(usize::from(FORECAST_DAYS));
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut periods = Vec::with_capacity(count);
    for index in 0..count {
        let start = array_text(times, index, "daily time")?;
        let name = match index {
            0 => String::from("Today"),
            1 => String::from("Tomorrow"),
            _ => start.clone(),
        };
        periods.push(ForecastPeriod {
            name,
            precipitation: probabilities
                .map(|values| array_probability(values, index, "daily precipitation probability"))
                .transpose()?
                .flatten(),
            start,
            temperature: array_number(maximums, index, "daily high")?,
            minimum: Some(array_number(minimums, index, "daily low")?),
            unit: String::from(unit),
            wind: String::new(),
            short: String::from(weather_text(weather_code(
                codes,
                index,
                "daily weather code",
            )?)),
        });
    }
    Ok(periods)
}

fn parse_document(place: &Place, document: &Value) -> Result<Forecast, ForecastError> {
    let timezone = text(document, "timezone")?;
    let timezone_abbreviation = text(document, "timezone_abbreviation")?;
    let current_units = object(document, "current_units")?;
    let hourly_units = object(document, "hourly_units")?;
    let unit = current_units
        .get("temperature_2m")
        .and_then(Value::as_str)
        .ok_or_else(|| shape("temperature unit"))?;
    let wind_unit = hourly_units
        .get("wind_speed_10m")
        .and_then(Value::as_str)
        .ok_or_else(|| shape("wind unit"))?;
    let current = current(document, unit, wind_unit)?;
    let hourly = hourly(document, unit, wind_unit)?;
    let daily = daily(document, unit)?;
    if hourly.is_empty() && daily.is_empty() {
        return Err(ForecastError::NoPeriods);
    }
    Ok(Forecast {
        place: place.clone(),
        provider: PROVIDER,
        timezone,
        timezone_abbreviation,
        current,
        hourly,
        daily,
    })
}

/// Fetches the global forecast for the place, using only its coordinates.
pub fn fetch(place: &Place) -> Result<Forecast, ForecastError> {
    let url = request_url(place)?;
    let document = provider::get_json(&url, "application/json")
        .map_err(|source| ForecastError::Provider { source })?;
    parse_document(place, &document)
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
    fn the_forecast_request_uses_the_town_coordinates() -> Result<(), TestError> {
        let place = bowling_green()?;
        let url = request_url(&place)?;
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
    fn weather_codes_and_wind_directions_are_readable() {
        assert_eq!(weather_text(0), "clear sky");
        assert_eq!(weather_text(95), "thunderstorm");
        assert_eq!(wind(0.0, 10.0, "km/h"), "10 km/h from the north");
        assert_eq!(wind(90.0, 10.0, "km/h"), "10 km/h from the east");
    }

    #[test]
    fn malformed_numbers_are_an_error_not_a_default() {
        assert!(matches!(
            array_probability(&[serde_json::json!(101)], 0, "probability"),
            Err(ForecastError::Shape {
                field: "probability"
            })
        ));
        assert!(matches!(
            array_probability(&[serde_json::json!(null)], 0, "probability"),
            Ok(None)
        ));
    }

    /// Exercises the real global provider with the searched town's coordinates.
    #[test]
    fn the_live_forecast_belongs_to_the_searched_town() -> Result<(), TestError> {
        let place = bowling_green()?;
        let forecast = fetch(&place)?;
        assert_eq!(forecast.place, place, "the forecast carries its place");
        assert_eq!(forecast.provider, "Open-Meteo");
        assert!(
            !forecast.hourly.is_empty(),
            "the provider returned no hourly periods"
        );
        assert!(
            !forecast.daily.is_empty(),
            "the provider returned no daily periods"
        );
        assert!(
            forecast.timezone.contains("Chicago"),
            "forecast timezone {}",
            forecast.timezone
        );
        assert!(
            (forecast.current.temperature > -80.0 && forecast.current.temperature < 140.0),
            "current temperature {} {}",
            forecast.current.temperature,
            forecast.current.unit
        );
        assert!(
            forecast
                .daily
                .iter()
                .all(|period| period.minimum.is_some_and(|low| low <= period.temperature)),
            "every daily low must not exceed its high"
        );
        Ok(())
    }

    /// The same provider serves places outside the United States.
    #[test]
    fn a_non_united_states_place_gets_a_live_forecast() -> Result<(), TestError> {
        let place = Place::new("Dublin", "Leinster", "Ireland", "ie", 53.349_8, -6.260_3)?;
        let forecast = fetch(&place)?;
        assert_eq!(forecast.provider, "Open-Meteo");
        assert!(
            !forecast.daily.is_empty(),
            "Dublin received no daily periods"
        );
        Ok(())
    }
}
