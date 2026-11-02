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
//! The surface-observation layer: the stations reporting in view, drawn as
//! markers and listed by distance.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::app::toggle_button;
use crate::map;
use crate::observations::{
    self, FlightCategory, Observation, ObservationSet, ObservationState, ObservationView,
};
use crate::projection;
use crate::theme::{self, Palette};
use crate::time;

/// Reports come on the hour, so a set stands for a while.
const REFRESH_AFTER: Duration = Duration::from_secs(600);
/// How far the map may move before the box is asked for again.
const MOVE_FRACTION: f64 = 0.4;
/// How many stations the rail lists.
const LISTED: usize = 6;
const MARKER_RADIUS: f32 = 2.6;

/// The surface-observation layer.
pub struct ObservationLayer {
    view: ObservationView,
    shown: bool,
    asked: Option<(f64, f64, f64)>,
    asked_at: Option<Instant>,
}

impl ObservationLayer {
    pub fn new() -> Self {
        Self {
            view: ObservationView::new(),
            shown: false,
            asked: None,
            asked_at: None,
        }
    }

    /// Keeps the observations current while the layer is on.
    pub fn update(&mut self, now: Instant, view: &map::MapView) {
        let retry_due = self.view.update(now);
        if !self.shown {
            return;
        }
        let Ok((lat, lon)) = view.center_latlon() else {
            return;
        };
        let span = view.span_km();
        let moved = match self.asked {
            Some((asked_lat, asked_lon, asked_span)) => {
                projection::inverse(asked_lat, asked_lon, lat, lon)
                    .map_or(true, |(distance, _)| distance > asked_span * MOVE_FRACTION)
                    || (asked_span - span).abs() > asked_span * MOVE_FRACTION
            }
            None => true,
        };
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        let retry = retry_due && self.view.may_retry();
        if moved || stale || retry {
            self.asked = Some((lat, lon, span));
            self.asked_at = Some(Instant::now());
            let box_of_stations = observations::bounds(lat, lon, span);
            self.view.start(box_of_stations, self.view.attempts());
        }
    }

    /// Whether the stations should be drawn.
    pub const fn is_shown(&self) -> bool {
        self.shown
    }

    /// Draws a marker for every station the projection can place.
    pub fn draw(&self, painter: &egui::Painter, transform: &map::MapTransform, palette: &Palette) {
        if !self.is_shown() {
            return;
        }
        let Some(set) = self.view.set() else {
            return;
        };
        for station in &set.stations {
            let Some(point) = transform.screen(station.latitude, station.longitude) else {
                continue;
            };
            painter.circle_filled(
                point,
                MARKER_RADIUS,
                category_colour(station.category, palette),
            );
        }
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette, centre: Option<(f64, f64)>) {
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Surface observations")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · worldwide, no key", observations::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Stations: on"
        } else {
            "Stations: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Reporting stations in view, drawn by flight category and listed with their temperature, dew point, wind, and visibility.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if !self.shown {
                self.asked = None;
            }
        }
        if !self.shown {
            ui.label(
                RichText::new("Turn the layer on to plot the stations reporting in view.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(set) = self.view.set() else {
            return;
        };
        summary(ui, set, palette);
        if let Some((lat, lon)) = centre {
            nearest(ui, set, lat, lon, palette);
        }
        ui.label(
            RichText::new("A station reports what it measures at its own point, not over the map.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            ObservationState::Idle | ObservationState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the surface reports…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            ObservationState::Loading { last: Some(_) } => {
                ui.label(
                    RichText::new("Refreshing · still showing the last reports")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            ObservationState::Ready { .. } => {}
            ObservationState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Surface observations failed: {error}"),
                );
                if last.is_some() {
                    ui.label(
                        RichText::new("Still showing the last reports.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
                if self.view.retry_waiting() {
                    ui.label(
                        RichText::new("Asking again shortly.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
        }
    }
}

/// The count and the spread of flight categories.
fn summary(ui: &mut egui::Ui, set: &ObservationSet, palette: &Palette) {
    let counts = [
        (FlightCategory::Visual, "VFR"),
        (FlightCategory::Marginal, "MVFR"),
        (FlightCategory::Instrument, "IFR"),
        (FlightCategory::LowInstrument, "LIFR"),
    ];
    let mut parts = Vec::new();
    for (category, name) in counts {
        let count = set.category_count(category);
        if count > 0 {
            parts.push(format!("{count} {name}"));
        }
    }
    let spread = if parts.is_empty() {
        String::from("no flight category reported")
    } else {
        parts.join(" · ")
    };
    let text = if set.rejected == 0 {
        format!("{} stations · {spread}", set.stations.len())
    } else {
        format!(
            "{} stations · {spread} · {} unreadable",
            set.stations.len(),
            set.rejected
        )
    };
    ui.label(
        RichText::new(text)
            .size(theme::TYPE_SMALL)
            .strong()
            .color(palette.text),
    );
}

/// The nearest reporting stations, with their readings.
fn nearest(
    ui: &mut egui::Ui,
    set: &ObservationSet,
    latitude: f64,
    longitude: f64,
    palette: &Palette,
) {
    let ordered = set.nearest(latitude, longitude);
    ui.add_space(theme::SPACE_S);
    ui.label(
        RichText::new("Nearest to the map centre")
            .size(theme::TYPE_SMALL)
            .weak(),
    );
    let now_ms = match time::now_unix_ms() {
        Ok(now) => now,
        Err(error) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("The system clock cannot be read, so report ages are unknown: {error}"),
            );
            0
        }
    };
    for (_, station) in ordered.iter().take(LISTED) {
        station_row(ui, station, latitude, longitude, now_ms, palette);
    }
    if ordered.len() > LISTED {
        ui.label(
            RichText::new(format!(
                "{} more in view",
                ordered.len().saturating_sub(LISTED)
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
}

fn station_row(
    ui: &mut egui::Ui,
    station: &Observation,
    latitude: f64,
    longitude: f64,
    now_ms: u64,
    palette: &Palette,
) {
    let distance = projection::inverse(latitude, longitude, station.latitude, station.longitude)
        .map_or_else(
            |_| String::from("distance unavailable"),
            |(distance, _)| format!("{distance:.0} km"),
        );
    let age = if now_ms == 0 {
        String::new()
    } else {
        format!(" · {}", observations::age_of(station.observed_ms, now_ms))
    };
    ui.label(
        RichText::new(format!(
            "{} · {distance} · {}{age}",
            station.station,
            station.category.label()
        ))
        .size(theme::TYPE_SMALL)
        .color(category_colour(station.category, palette)),
    );
    ui.label(
        RichText::new(format!(
            "{} · {}",
            station.name,
            station.temperature_label()
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    let visibility = station.visibility_mi.map_or_else(
        || String::from("visibility not reported"),
        |miles| format!("{miles:.0} mi"),
    );
    let altimeter = station.altimeter_hpa.map_or_else(
        || String::from("altimeter not reported"),
        |hpa| format!("{hpa:.1} hPa"),
    );
    ui.label(
        RichText::new(format!(
            "{} · {visibility} · {altimeter}",
            station.wind_label()
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}

/// The colour a flight category is marked in.
fn category_colour(category: FlightCategory, palette: &Palette) -> Color32 {
    match category {
        FlightCategory::Visual => palette.accent,
        FlightCategory::Marginal => palette.warning.gamma_multiply(0.75),
        FlightCategory::Instrument => palette.error,
        FlightCategory::LowInstrument => palette.error.gamma_multiply(0.65),
        FlightCategory::Unreported => palette.text.gamma_multiply(0.6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(station: &str, latitude: f64, longitude: f64) -> Observation {
        Observation {
            station: String::from(station),
            name: String::from("A field, KS, US"),
            latitude,
            longitude,
            elevation_m: Some(400.0),
            observed_ms: 1_793_585_400_000,
            temperature_c: Some(16.0),
            dewpoint_c: Some(9.0),
            wind_from_deg: Some(170.0),
            wind_speed_kt: Some(12.0),
            gust_kt: None,
            visibility_mi: Some(10.0),
            altimeter_hpa: Some(1016.0),
            category: FlightCategory::Visual,
            cover: Some(String::from("SCT")),
            raw: String::from("METAR KICT 020200Z 17012KT 10SM SCT035 16/09 A3002"),
        }
    }

    fn set() -> ObservationSet {
        ObservationSet {
            stations: vec![
                station("KICT", 37.65, -97.43),
                station("KEWK", 38.06, -97.28),
            ],
            rejected: 0,
        }
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = ObservationLayer::new();
        assert!(!layer.is_shown());
        assert!(matches!(layer.view.state(), ObservationState::Idle));
    }

    #[test]
    fn the_nearest_station_comes_first() {
        let set = set();
        let ordered = set.nearest(37.7, -97.4);
        assert_eq!(
            ordered.first().map(|(_, station)| station.station.as_str()),
            Some("KICT")
        );
    }

    #[test]
    fn each_flight_category_has_its_own_colour() {
        let palette = theme::Theme::Dark.palette();
        let visual = category_colour(FlightCategory::Visual, &palette);
        let instrument = category_colour(FlightCategory::Instrument, &palette);
        let unreported = category_colour(FlightCategory::Unreported, &palette);
        assert_ne!(visual, instrument);
        assert_ne!(instrument, unreported);
    }
}
