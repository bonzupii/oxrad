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
//! Sunrise and sunset for a requested map centre.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use serde_json::Value;

use crate::provider::{self, JsonError};
use crate::retry;
use crate::theme;
use crate::time;

const BASE: &str = "https://api.sunrise-sunset.org/v2";
const CREDIT: &str = "https://sunrise-sunset.org";

#[derive(Debug)]
struct SunRecord {
    latitude: f64,
    longitude: f64,
    date: String,
    timezone: String,
    status: String,
    sunrise: Option<String>,
    sunset: Option<String>,
    solar_noon: Option<String>,
}

fn required_text<'a>(document: &'a Value, key: &'static str) -> Result<&'a str, JsonError> {
    document
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| JsonError::Shape {
            url: String::from(BASE),
            field: key,
        })
}

fn event(document: &Value, key: &'static str) -> Result<Option<String>, JsonError> {
    match document.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if time::parse_iso_utc(text).is_some() => Ok(Some(text.clone())),
        Some(_) | None => Err(JsonError::Shape {
            url: String::from(BASE),
            field: key,
        }),
    }
}

fn parse(document: &Value) -> Result<SunRecord, JsonError> {
    let latitude = document
        .get("lat")
        .and_then(Value::as_f64)
        .filter(|value| (-90.0..=90.0).contains(value))
        .ok_or_else(|| JsonError::Shape {
            url: String::from(BASE),
            field: "latitude",
        })?;
    let longitude = document
        .get("lng")
        .and_then(Value::as_f64)
        .filter(|value| (-180.0..=180.0).contains(value))
        .ok_or_else(|| JsonError::Shape {
            url: String::from(BASE),
            field: "longitude",
        })?;
    let status = required_text(document, "sun_status")?;
    if !matches!(status, "normal" | "midnight_sun" | "polar_night") {
        return Err(JsonError::Shape {
            url: String::from(BASE),
            field: "sun_status",
        });
    }
    Ok(SunRecord {
        latitude,
        longitude,
        date: String::from(required_text(document, "date")?),
        timezone: String::from(required_text(document, "tzid")?),
        status: String::from(status),
        sunrise: event(document, "sunrise")?,
        sunset: event(document, "sunset")?,
        solar_noon: event(document, "solar_noon")?,
    })
}

fn fetch(latitude: f64, longitude: f64) -> Result<SunRecord, JsonError> {
    let url = format!("{BASE}?lat={latitude:.4}&lng={longitude:.4}");
    let document = provider::get_json(&url, "application/json")?;
    parse(&document)
}

/// User-initiated solar event requests and their last successful result.
pub struct SunView {
    request: Option<Receiver<Result<SunRecord, JsonError>>>,
    record: Option<SunRecord>,
    error: Option<JsonError>,
    retry_at: Option<Instant>,
}

impl SunView {
    pub const fn new() -> Self {
        Self {
            request: None,
            record: None,
            error: None,
            retry_at: None,
        }
    }

    pub fn update(&mut self, now: Instant) {
        let Some(receiver) = &self.request else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(JsonError::WorkerStopped),
        };
        self.request = None;
        match result {
            Ok(record) => {
                self.record = Some(record);
                self.error = None;
                self.retry_at = None;
            }
            Err(error) => {
                if let Some(value) = error.retry_after() {
                    let delay =
                        retry::retry_after_delay(value).unwrap_or_else(|_| Duration::from_secs(60));
                    self.retry_at = now.checked_add(delay);
                }
                self.error = Some(error);
            }
        }
    }

    fn start(&mut self, latitude: f64, longitude: f64) {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || sender.send(fetch(latitude, longitude)).is_ok());
        self.request = Some(receiver);
        self.error = None;
    }

    pub fn rail(&mut self, ui: &mut egui::Ui, centre: Option<(f64, f64)>) {
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Sunrise and sunset")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.hyperlink_to("Sun data by Sunrise-Sunset.org", CREDIT);
        let waiting = self.retry_at.is_some_and(|at| Instant::now() < at);
        if ui
            .add_enabled(
                centre.is_some() && self.request.is_none() && !waiting,
                egui::Button::new("Get sun times for map centre"),
            )
            .clicked()
            && let Some((latitude, longitude)) = centre
        {
            self.start(latitude, longitude);
        }
        if self.request.is_some() {
            ui.label("Loading sun times…");
        }
        if waiting {
            ui.label("The provider requested a wait before retrying.");
        }
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Sun times unavailable: {error}"),
            );
        }
        if let Some(record) = &self.record {
            ui.label("Requested local date and location; refresh manually.");
            if self.request.is_some() || self.error.is_some() {
                ui.label("Previous result:");
            }
            ui.label(format!(
                "{} · {} · {:.4}, {:.4}",
                record.date, record.timezone, record.latitude, record.longitude
            ));
            match record.status.as_str() {
                "midnight_sun" => {
                    ui.label("Midnight sun");
                }
                "polar_night" => {
                    ui.label("Polar night");
                }
                _ => {}
            }
            show_event(ui, "Sunrise", record.sunrise.as_deref());
            show_event(ui, "Solar noon", record.solar_noon.as_deref());
            show_event(ui, "Sunset", record.sunset.as_deref());
        }
    }
}

fn show_event(ui: &mut egui::Ui, label: &str, timestamp: Option<&str>) {
    match timestamp {
        Some(value) => {
            ui.label(format!("{label}: {}", provider::timestamp_label(value)));
        }
        None => {
            ui.label(format!("{label}: no event on this local date"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        serde_json::json!({
            "lat": 70.0, "lng": 20.0, "date": "2026-06-20", "tzid": "Europe/Oslo",
            "sun_status": "normal", "sunrise": "2026-06-20T01:00:00+02:00",
            "sunset": null, "solar_noon": "2026-06-20T12:00:00+02:00"
        })
    }

    #[test]
    fn events_are_independently_nullable() -> Result<(), JsonError> {
        let record = parse(&document())?;
        assert_eq!(record.sunrise.as_deref(), Some("2026-06-20T01:00:00+02:00"));
        assert_eq!(record.sunset, None);
        assert_eq!(record.timezone, "Europe/Oslo");
        Ok(())
    }

    #[test]
    fn invalid_solar_events_are_rejected() {
        let mut value = document();
        value["sunrise"] = serde_json::json!("not a timestamp");
        assert!(matches!(
            parse(&value),
            Err(JsonError::Shape {
                field: "sunrise",
                ..
            })
        ));
    }

    #[test]
    fn unknown_polar_status_is_rejected() {
        let mut value = document();
        value["sun_status"] = serde_json::json!("unknown");
        assert!(matches!(
            parse(&value),
            Err(JsonError::Shape {
                field: "sun_status",
                ..
            })
        ));
    }
}
