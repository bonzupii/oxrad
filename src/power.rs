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
//! NASA POWER's MERRA-2 monthly climatology at one point.
//!
//! The climatology is a twenty-five year monthly mean, not a forecast and not
//! an observation for a particular day. The layer keeps the source's own units
//! and its stated period, so a monthly normal can never be read as today's
//! weather.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{Receiver, TryRecvError};

use serde_json::Value;

use crate::acquire::Acquire;
use crate::engine::Served;
use crate::provider::FetchError;
use crate::sources;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "NASA POWER, MERRA-2";

/// The calendar month names in the order the service publishes them.
const MONTHS: [(u32, &str); 12] = [
    (1, "JAN"),
    (2, "FEB"),
    (3, "MAR"),
    (4, "APR"),
    (5, "MAY"),
    (6, "JUN"),
    (7, "JUL"),
    (8, "AUG"),
    (9, "SEP"),
    (10, "OCT"),
    (11, "NOV"),
    (12, "DEC"),
];

/// One calendar month's normal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerMonth {
    pub month: u32,
    pub temperature_c: Option<f64>,
    pub precipitation_mm_per_day: Option<f64>,
}

/// The monthly and annual normals at one point.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerClimatology {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: Option<f64>,
    pub temperature_unit: String,
    pub precipitation_unit: String,
    pub period: String,
    pub sources: String,
    pub months: Vec<PowerMonth>,
    pub annual_temperature_c: Option<f64>,
    pub annual_precipitation_mm_per_day: Option<f64>,
}

impl PowerClimatology {
    /// The month with the highest normal temperature.
    pub fn warmest(&self) -> Option<PowerMonth> {
        self.months
            .iter()
            .copied()
            .filter(|month| month.temperature_c.is_some())
            .max_by(|left, right| {
                left.temperature_c
                    .unwrap_or(f64::MIN)
                    .total_cmp(&right.temperature_c.unwrap_or(f64::MIN))
            })
    }
}

/// Why a POWER response could not be read.
#[derive(Debug)]
pub enum PowerError {
    Provider { source: FetchError },
    Shape { field: &'static str },
    WorkerStopped,
}

impl Display for PowerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The NASA POWER request failed: {source}")
            }
            Self::Shape { field } => {
                write!(formatter, "The NASA POWER record has no usable {field}.")
            }
            Self::WorkerStopped => formatter
                .write_str("The NASA POWER worker stopped, so no climatology can be requested."),
        }
    }
}

impl Error for PowerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

const fn shape(field: &'static str) -> PowerError {
    PowerError::Shape { field }
}

fn point(document: &Value) -> Option<(f64, f64, Option<f64>)> {
    let coordinates = document.get("geometry")?.get("coordinates")?.as_array()?;
    let longitude = coordinates.first()?.as_f64()?;
    let latitude = coordinates.get(1)?.as_f64()?;
    if !(-180.0..=180.0).contains(&longitude) || !(-90.0..=90.0).contains(&latitude) {
        return None;
    }
    Some((
        latitude,
        longitude,
        coordinates.get(2).and_then(Value::as_f64),
    ))
}

fn value_of(
    parameters: &serde_json::Map<String, Value>,
    parameter: &str,
    key: &str,
    fill_value: f64,
) -> Option<f64> {
    parameters
        .get(parameter)?
        .get(key)?
        .as_f64()
        .filter(|value| value.is_finite() && *value != fill_value)
}

fn unit_of(document: &Value, parameter: &str) -> String {
    document
        .get("parameters")
        .and_then(|parameters| parameters.get(parameter))
        .and_then(|parameter| parameter.get("units"))
        .and_then(Value::as_str)
        .map_or_else(|| String::from("unit not stated"), String::from)
}

/// Reads the monthly climatology response.
pub fn parse(document: &Value) -> Result<PowerClimatology, PowerError> {
    let (latitude, longitude, elevation_m) = point(document).ok_or_else(|| shape("point"))?;
    let parameters = document
        .get("properties")
        .and_then(|properties| properties.get("parameter"))
        .and_then(Value::as_object)
        .ok_or_else(|| shape("parameter table"))?;
    let header = document
        .get("header")
        .and_then(Value::as_object)
        .ok_or_else(|| shape("header"))?;
    let fill_value = header
        .get("fill_value")
        .and_then(Value::as_f64)
        .ok_or_else(|| shape("fill value"))?;
    let mut months = Vec::with_capacity(MONTHS.len());
    for (month, key) in MONTHS {
        let temperature_c = value_of(parameters, "T2M", key, fill_value);
        let precipitation_mm_per_day = value_of(parameters, "PRECTOTCORR", key, fill_value);
        if temperature_c.is_none() && precipitation_mm_per_day.is_none() {
            continue;
        }
        months.push(PowerMonth {
            month,
            temperature_c,
            precipitation_mm_per_day,
        });
    }
    if months.is_empty() {
        return Err(shape("monthly normal"));
    }
    let period = header
        .get("range")
        .and_then(Value::as_str)
        .map_or_else(|| String::from("period not stated"), String::from);
    let sources = header.get("sources").and_then(Value::as_array).map_or_else(
        || String::from("source not stated"),
        |values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<&str>>()
                .join(", ")
        },
    );
    Ok(PowerClimatology {
        latitude,
        longitude,
        elevation_m,
        temperature_unit: unit_of(document, "T2M"),
        precipitation_unit: unit_of(document, "PRECTOTCORR"),
        period,
        sources,
        months,
        annual_temperature_c: value_of(parameters, "T2M", "ANN", fill_value),
        annual_precipitation_mm_per_day: value_of(parameters, "PRECTOTCORR", "ANN", fill_value),
    })
}

/// Fetches the climatology through the request engine.
pub fn fetch(
    acquire: &Acquire,
    latitude: f64,
    longitude: f64,
) -> Result<Served<PowerClimatology>, PowerError> {
    let latitude_text = format!("{latitude:.3}");
    let longitude_text = format!("{longitude:.3}");
    let fetched = acquire
        .fetch_operation(
            &sources::NASA_POWER,
            "climatology",
            &[
                ("latitude", latitude_text.as_str()),
                ("longitude", longitude_text.as_str()),
            ],
        )
        .map_err(|source| PowerError::Provider { source })?;
    let value = parse(
        &fetched
            .json()
            .map_err(|source| PowerError::Provider { source })?,
    )?;
    Ok(Served::new(value, fetched.provenance, fetched.cache))
}

/// Where the climatology request stands.
#[derive(Debug)]
pub enum PowerState {
    Idle,
    Loading {
        last: Option<Served<PowerClimatology>>,
    },
    Ready {
        served: Served<PowerClimatology>,
    },
    Failed {
        error: PowerError,
        last: Option<Served<PowerClimatology>>,
    },
}

impl PowerState {
    /// The last answer still worth drawing.
    pub const fn last(&self) -> Option<&Served<PowerClimatology>> {
        match self {
            Self::Ready { served } => Some(served),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// Owns the one in-flight climatology request.
pub struct PowerView {
    acquire: Acquire,
    pending: Option<Receiver<Result<Served<PowerClimatology>, PowerError>>>,
    state: PowerState,
}

impl PowerView {
    pub const fn new(acquire: Acquire) -> Self {
        Self {
            acquire,
            pending: None,
            state: PowerState::Idle,
        }
    }

    pub const fn state(&self) -> &PowerState {
        &self.state
    }

    pub const fn record(&self) -> Option<&Served<PowerClimatology>> {
        self.state.last()
    }

    /// Whether a request is already in flight.
    pub const fn started(&self) -> bool {
        self.pending.is_some()
    }

    pub fn start(&mut self, latitude: f64, longitude: f64) {
        let last = take_last(&mut self.state);
        self.state = PowerState::Loading { last };
        let acquire = self.acquire.clone();
        self.pending = Some(crate::engine::task(move || {
            fetch(&acquire, latitude, longitude)
        }));
    }

    pub fn update(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(served)) => {
                self.state = PowerState::Ready { served };
                self.pending = None;
            }
            Ok(Err(error)) => {
                let last = take_last(&mut self.state);
                self.state = PowerState::Failed { error, last };
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let last = take_last(&mut self.state);
                self.state = PowerState::Failed {
                    error: PowerError::WorkerStopped,
                    last,
                };
                self.pending = None;
            }
        }
    }
}

fn take_last(state: &mut PowerState) -> Option<Served<PowerClimatology>> {
    match std::mem::replace(state, PowerState::Idle) {
        PowerState::Loading { last } | PowerState::Failed { last, .. } => last,
        PowerState::Ready { served } => Some(served),
        PowerState::Idle => None,
    }
}

/// A sentence for the status line.
pub fn describe(record: &PowerClimatology) -> String {
    let warmest = record
        .warmest()
        .and_then(|month| {
            month.temperature_c.map(|value| {
                format!(
                    "warmest {} {value:.1} {}",
                    month_name(month.month),
                    record.temperature_unit
                )
            })
        })
        .unwrap_or_else(|| String::from("no monthly temperature"));
    format!(
        "{:.2}, {:.2} · {warmest} · {}",
        record.latitude, record.longitude, record.period
    )
}

/// The English name of a calendar month.
pub const fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        _ => "December",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        let body = r#"{
            "type": "Feature",
            "geometry": {"type": "Point", "coordinates": [-1.74, 51.87, 121.15]},
            "properties": {
                "parameter": {
                    "T2M": {"JAN": 3.74, "FEB": 3.83, "DEC": 4.38, "ANN": 9.7},
                    "PRECTOTCORR": {"JAN": 2.28, "FEB": 2.0, "DEC": 2.47, "ANN": 2.17}
                }
            },
            "header": {
                "title": "NASA/POWER Source Native Resolution Climatology Climatologies",
                "sources": ["MERRA2"],
                "fill_value": -999.0,
                "range": "25-year Meteorological and Solar Monthly & Annual Climatologies (January 2001 - December 2025)"
            },
            "parameters": {
                "T2M": {"units": "C", "longname": "Temperature at 2 Meters"},
                "PRECTOTCORR": {"units": "mm/day", "longname": "Precipitation Corrected"}
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    #[test]
    fn monthly_normals_keep_their_units_and_period() {
        let parsed = parse(&document());
        assert!(parsed.is_ok(), "the fixture should parse");
        if let Ok(record) = parsed {
            assert_eq!(record.months.len(), 3);
            assert_eq!(record.temperature_unit, "C");
            assert_eq!(record.precipitation_unit, "mm/day");
            assert!((record.latitude - 51.87).abs() < 1e-9);
            assert!((record.longitude + 1.74).abs() < 1e-9);
            assert_eq!(record.elevation_m, Some(121.15));
            let january = record.months[0];
            assert_eq!(january.month, 1);
            assert!((january.temperature_c.unwrap_or(0.0) - 3.74).abs() < 1e-9);
            assert!((january.precipitation_mm_per_day.unwrap_or(0.0) - 2.28).abs() < 1e-9);
            assert_eq!(record.annual_temperature_c, Some(9.7));
            assert_eq!(record.annual_precipitation_mm_per_day, Some(2.17));
            assert!(record.period.contains("2001"));
        }
    }

    #[test]
    fn a_fill_value_never_becomes_a_normal() {
        let mut document = document();
        document["properties"]["parameter"]["T2M"]["JAN"] = serde_json::json!(-999.0);
        let parsed = parse(&document);
        assert!(parsed.is_ok(), "the fixture should parse");
        if let Ok(record) = parsed {
            assert_eq!(record.months[0].temperature_c, None);
        }
    }

    #[test]
    fn a_response_without_a_parameter_table_is_refused() {
        let document = serde_json::json!({
            "geometry": {"type": "Point", "coordinates": [0.0, 0.0, 0.0]},
            "header": {"fill_value": -999.0}
        });
        assert!(matches!(
            parse(&document),
            Err(PowerError::Shape {
                field: "parameter table"
            })
        ));
    }
}
