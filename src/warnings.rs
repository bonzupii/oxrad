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
//! Active warning polygons from the National Weather Service.
//!
//! The feed is authoritative for the United States and its territories. It is
//! deliberately separate from the radar source: a warning is a human-issued
//! product with its own identity, validity, and geography, not a radar gate.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use serde_json::Value;

use crate::provider::{self, JsonError};

const ALERTS_URL: &str = "https://api.weather.gov/alerts/active?status=actual&message_type=alert";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Extreme,
    Severe,
    Moderate,
    Minor,
    Unknown,
}

impl Severity {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("Extreme") => Self::Extreme,
            Some("Severe") => Self::Severe,
            Some("Moderate") => Self::Moderate,
            Some("Minor") => Self::Minor,
            Some(_) | None => Self::Unknown,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Extreme => "extreme",
            Self::Severe => "severe",
            Self::Moderate => "moderate",
            Self::Minor => "minor",
            Self::Unknown => "severity unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Immediate,
    Expected,
    Future,
    Past,
    Unknown,
}

impl Urgency {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("Immediate") => Self::Immediate,
            Some("Expected") => Self::Expected,
            Some("Future") => Self::Future,
            Some("Past") => Self::Past,
            Some(_) | None => Self::Unknown,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Expected => "expected",
            Self::Future => "future",
            Self::Past => "past",
            Self::Unknown => "urgency unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub id: String,
    pub event: String,
    pub severity: Severity,
    pub urgency: Urgency,
    pub headline: String,
    pub description: String,
    pub instruction: Option<String>,
    pub area: String,
    pub sent: String,
    pub expires: String,
    pub polygons: Vec<Vec<(f64, f64)>>,
}

#[derive(Debug)]
pub struct WarningSet {
    pub warnings: Vec<Warning>,
    pub without_geometry: usize,
    pub rejected: Vec<String>,
}

#[derive(Debug)]
pub enum WarningError {
    Json(JsonError),
    Shape { field: &'static str },
    Feature { id: String, detail: &'static str },
    WorkerStopped,
}

impl WarningError {
    fn message(&self) -> String {
        match self {
            Self::Json(source) => format!("The active-warning feed could not be read: {source}"),
            Self::Shape { field } => {
                format!("The active-warning feed has no usable {field}.")
            }
            Self::Feature { id, detail } => {
                format!("Warning {id} has an invalid {detail}.")
            }
            Self::WorkerStopped => {
                String::from("The active-warning worker stopped before answering.")
            }
        }
    }
}

impl Display for WarningError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for WarningError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            Self::Shape { .. } | Self::Feature { .. } | Self::WorkerStopped => None,
        }
    }
}

impl From<JsonError> for WarningError {
    fn from(value: JsonError) -> Self {
        Self::Json(value)
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn coordinate(pair: &Value, id: &str) -> Result<(f64, f64), WarningError> {
    let values = pair.as_array().ok_or_else(|| WarningError::Feature {
        id: String::from(id),
        detail: "coordinate pair",
    })?;
    let lon = values
        .first()
        .and_then(Value::as_f64)
        .ok_or_else(|| WarningError::Feature {
            id: String::from(id),
            detail: "longitude",
        })?;
    let lat = values
        .get(1)
        .and_then(Value::as_f64)
        .ok_or_else(|| WarningError::Feature {
            id: String::from(id),
            detail: "latitude",
        })?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(WarningError::Feature {
            id: String::from(id),
            detail: "coordinate range",
        });
    }
    Ok((lat, lon))
}

fn ring(value: &Value, id: &str) -> Result<Vec<(f64, f64)>, WarningError> {
    let points = value.as_array().ok_or_else(|| WarningError::Feature {
        id: String::from(id),
        detail: "warning ring",
    })?;
    let mut result = Vec::with_capacity(points.len());
    for pair in points {
        result.push(coordinate(pair, id)?);
    }
    if result.len() < 3 {
        return Err(WarningError::Feature {
            id: String::from(id),
            detail: "warning ring with at least three points",
        });
    }
    Ok(result)
}

fn polygons(geometry: &Value, id: &str) -> Result<Vec<Vec<(f64, f64)>>, WarningError> {
    let kind = geometry
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| WarningError::Feature {
            id: String::from(id),
            detail: "geometry type",
        })?;
    let coordinates = geometry
        .get("coordinates")
        .ok_or_else(|| WarningError::Feature {
            id: String::from(id),
            detail: "geometry coordinates",
        })?;
    match kind {
        "Polygon" => {
            let outer = coordinates
                .as_array()
                .and_then(|rings| rings.first())
                .ok_or_else(|| WarningError::Feature {
                    id: String::from(id),
                    detail: "polygon ring",
                })?;
            Ok(vec![ring(outer, id)?])
        }
        "MultiPolygon" => {
            let parts = coordinates
                .as_array()
                .ok_or_else(|| WarningError::Feature {
                    id: String::from(id),
                    detail: "multipolygon",
                })?;
            let mut result = Vec::new();
            for part in parts {
                let outer = part
                    .as_array()
                    .and_then(|rings| rings.first())
                    .ok_or_else(|| WarningError::Feature {
                        id: String::from(id),
                        detail: "polygon ring",
                    })?;
                result.push(ring(outer, id)?);
            }
            Ok(result)
        }
        _ => Err(WarningError::Feature {
            id: String::from(id),
            detail: "polygon or multipolygon geometry",
        }),
    }
}

fn parse_warning(feature: &Value) -> Result<Warning, WarningError> {
    let properties = feature.get("properties").ok_or(WarningError::Shape {
        field: "warning properties",
    })?;
    let id = text(properties, "id").ok_or(WarningError::Shape {
        field: "warning identifier",
    })?;
    let event = text(properties, "event").ok_or_else(|| WarningError::Feature {
        id: String::from(id),
        detail: "event name",
    })?;
    let severity = Severity::parse(text(properties, "severity"));
    let urgency = Urgency::parse(text(properties, "urgency"));
    let headline = text(properties, "headline").map_or_else(String::new, String::from);
    let description = text(properties, "description").map_or_else(String::new, String::from);
    let instruction = text(properties, "instruction").map(String::from);
    let area = text(properties, "areaDesc").map_or_else(String::new, String::from);
    let sent = text(properties, "sent").map_or_else(String::new, String::from);
    let expires = text(properties, "expires").map_or_else(String::new, String::from);
    let polygons = match feature.get("geometry") {
        Some(geometry) if !geometry.is_null() => polygons(geometry, id)?,
        Some(_) | None => Vec::new(),
    };
    Ok(Warning {
        id: String::from(id),
        event: String::from(event),
        severity,
        urgency,
        headline,
        description,
        instruction,
        area,
        sent,
        expires,
        polygons,
    })
}

fn parse_collection(document: &Value) -> Result<WarningSet, WarningError> {
    let features =
        document
            .get("features")
            .and_then(Value::as_array)
            .ok_or(WarningError::Shape {
                field: "warning feature collection",
            })?;
    let mut warnings = Vec::with_capacity(features.len());
    let mut without_geometry = 0_usize;
    let mut rejected = Vec::new();
    for feature in features {
        match parse_warning(feature) {
            Ok(warning) => {
                if warning.polygons.is_empty() {
                    without_geometry = without_geometry.saturating_add(1);
                }
                warnings.push(warning);
            }
            Err(error) => rejected.push(error.to_string()),
        }
    }
    Ok(WarningSet {
        warnings,
        without_geometry,
        rejected,
    })
}

/// Retrieves and parses the current active warning collection.
pub fn fetch() -> Result<WarningSet, WarningError> {
    let document = provider::get_json(ALERTS_URL, "application/geo+json")?;
    parse_collection(&document)
}

pub enum WarningEvent {
    Ready {
        generation: u64,
        set: WarningSet,
    },
    Failed {
        generation: u64,
        error: WarningError,
    },
}

pub struct WarningFeed {
    requests: Sender<u64>,
    events: Receiver<WarningEvent>,
}

impl WarningFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(generation) = request_receiver.recv() {
                let event = match fetch() {
                    Ok(set) => WarningEvent::Ready { generation, set },
                    Err(error) => WarningEvent::Failed { generation, error },
                };
                if event_sender.send(event).is_err() {
                    return;
                }
            }
        });
        Self { requests, events }
    }

    pub fn request(&self, generation: u64) -> Result<(), WarningError> {
        self.requests
            .send(generation)
            .map_err(|_| WarningError::WorkerStopped)
    }

    pub fn poll(&self) -> Result<Option<WarningEvent>, WarningError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(WarningError::WorkerStopped),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;
    use serde_json::json;

    fn warning_feature(geometry: Value) -> Value {
        json!({
            "properties": {
                "id": "urn:test:1",
                "event": "Severe Thunderstorm Warning",
                "severity": "Severe",
                "urgency": "Immediate",
                "headline": "Severe Thunderstorm Warning for test county",
                "description": "A severe thunderstorm was located near Test City and is moving east.",
                "instruction": "Move to an interior room on the lowest floor of a building.",
                "areaDesc": "Test County",
                "sent": "2026-10-31T11:55:00-05:00",
                "expires": "2026-10-31T12:00:00-05:00"
            },
            "geometry": geometry
        })
    }

    #[test]
    fn parses_polygon_geometry_and_metadata() -> Result<(), TestError> {
        let feature = warning_feature(json!({
            "type": "Polygon",
            "coordinates": [[
                [-88.0, 37.0],
                [-87.0, 37.0],
                [-87.0, 38.0],
                [-88.0, 37.0]
            ]]
        }));
        let warning = parse_warning(&feature)?;
        assert_eq!(warning.id, "urn:test:1");
        assert_eq!(warning.event, "Severe Thunderstorm Warning");
        assert!(matches!(warning.severity, Severity::Severe));
        assert!(matches!(warning.urgency, Urgency::Immediate));
        assert_eq!(
            warning.headline,
            "Severe Thunderstorm Warning for test county"
        );
        assert!(warning.description.contains("Test City"));
        assert_eq!(
            warning.instruction.as_deref(),
            Some("Move to an interior room on the lowest floor of a building.")
        );
        assert_eq!(warning.area, "Test County");
        assert_eq!(warning.sent, "2026-10-31T11:55:00-05:00");
        assert_eq!(warning.polygons.len(), 1);
        Ok(())
    }

    #[test]
    fn one_malformed_alert_does_not_discard_the_others() -> Result<(), TestError> {
        let valid = warning_feature(json!({
            "type": "Polygon",
            "coordinates": [[
                [-88.0, 37.0],
                [-87.0, 37.0],
                [-87.0, 38.0],
                [-88.0, 37.0]
            ]]
        }));
        let malformed = json!({
            "properties": { "id": "urn:test:bad" },
            "geometry": Value::Null
        });
        let set = parse_collection(&json!({ "features": [valid, malformed] }))?;
        assert_eq!(set.warnings.len(), 1);
        assert_eq!(set.rejected.len(), 1);
        assert!(
            set.rejected
                .first()
                .is_some_and(|message| message.contains("urn:test:bad")),
            "rejection message {:?}",
            set.rejected
        );
        Ok(())
    }

    #[test]
    fn a_warning_without_geometry_is_retained() -> Result<(), TestError> {
        let feature = warning_feature(Value::Null);
        let warning = parse_warning(&feature)?;
        assert!(warning.polygons.is_empty());
        assert_eq!(warning.area, "Test County");
        Ok(())
    }

    #[test]
    fn out_of_range_coordinates_are_rejected() {
        let feature = warning_feature(json!({
            "type": "Polygon",
            "coordinates": [[
                [-88.0, 97.0],
                [-87.0, 37.0],
                [-87.0, 38.0],
                [-88.0, 37.0]
            ]]
        }));
        assert!(matches!(
            parse_warning(&feature),
            Err(WarningError::Feature { .. })
        ));
    }

    #[test]
    fn missing_event_is_rejected() {
        let feature = json!({
            "properties": { "id": "urn:test:2" },
            "geometry": Value::Null
        });
        assert!(matches!(
            parse_warning(&feature),
            Err(WarningError::Feature { .. })
        ));
    }

    #[test]
    fn live_active_warning_feed_parses() -> Result<(), TestError> {
        let set = fetch()?;
        assert!(
            set.warnings.iter().all(|warning| !warning.event.is_empty()),
            "the live feed returned an empty event name"
        );
        Ok(())
    }

    #[test]
    fn multipolygon_rings_are_all_retained() -> Result<(), TestError> {
        let feature = warning_feature(json!({
            "type": "MultiPolygon",
            "coordinates": [
                [[[-88.0, 37.0], [-87.0, 37.0], [-87.0, 38.0], [-88.0, 37.0]]],
                [[[-86.0, 37.0], [-85.0, 37.0], [-85.0, 38.0], [-86.0, 37.0]]]
            ]
        }));
        let warning = parse_warning(&feature)?;
        assert_eq!(warning.polygons.len(), 2);
        Ok(())
    }
}
