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
//! The air-quality layer: its rail section and the index chart.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::map::MapView;
use crate::projection;
use crate::quality::{self, AirQuality, AirState, AirView, AqiCategory};
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the record is asked for again.
const MOVE_KM: f64 = 25.0;
/// How long a record stands before it is refreshed even without movement.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
const CHART_HEIGHT: f32 = 104.0;
/// The room under the plot for the hour labels.
const DATE_GUTTER: f32 = 15.0;

/// The air-quality layer.
pub struct AirLayer {
    view: AirView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl AirLayer {
    pub fn new() -> Self {
        Self {
            view: AirView::new(),
            shown: false,
            asked: None,
            asked_at: None,
        }
    }

    /// Follows the map centre while the layer is on, and takes in the worker's
    /// answers.
    pub fn update(&mut self, now: Instant, view: &MapView) {
        let retry_due = self.view.update(now);
        if !self.shown {
            return;
        }
        let Ok((lat, lon)) = view.center_latlon() else {
            return;
        };
        let moved = match self.asked {
            Some((asked_lat, asked_lon)) => distance_km(asked_lat, asked_lon, lat, lon) > MOVE_KM,
            None => true,
        };
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        let retry = retry_due && self.view.may_retry();
        if moved || stale || retry {
            self.asked = Some((lat, lon));
            self.asked_at = Some(Instant::now());
            self.view.start(lat, lon, self.view.attempts());
        }
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.label(RichText::new("Air quality").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!("{} · global, no key", quality::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown { "Air: on" } else { "Air: off" };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Modelled particulate and gas concentrations at the map centre, with the United States index for the day behind and two days ahead.",
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
                RichText::new("Turn the layer on to read the air at the map centre.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(record) = self.view.record() else {
            return;
        };
        readout(ui, record, palette);
        chart(ui, record, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            AirState::Idle | AirState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the air-quality grid…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            AirState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            AirState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            AirState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Air quality failed: {error}"),
                );
                if let Some(record) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", describe(record)))
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

/// A sentence for the status line: the grid point and the record's span.
fn describe(record: &AirQuality) -> String {
    let Some(first) = record.hours.first() else {
        return String::from("The provider sent no hours.");
    };
    let Some(last) = record.hours.last() else {
        return String::from("The provider sent no hours.");
    };
    format!(
        "{} hours · {} to {} · grid point {:.2}, {:.2}",
        record.hours.len(),
        short_hour(first.time_ms),
        short_hour(last.time_ms),
        record.latitude,
        record.longitude
    )
}

fn reading(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(
        || String::from("not modelled"),
        |value| format!("{value:.1} {unit}"),
    )
}

/// The current index, its band, and the concentrations around it.
fn readout(ui: &mut egui::Ui, record: &AirQuality, palette: &Palette) {
    let current = &record.current;
    ui.horizontal(|ui| {
        ui.label(RichText::new("US index").size(theme::TYPE_SMALL).weak());
        let text = current.us_aqi.map_or_else(
            || String::from("not modelled"),
            |index| format!("{index:.0} · {}", AqiCategory::of(index).label()),
        );
        ui.label(
            RichText::new(text)
                .size(theme::TYPE_SMALL)
                .strong()
                .color(band_colour(current.category(), palette)),
        );
    });
    if let Some(european) = current.european_aqi {
        ui.label(
            RichText::new(format!("European index {european:.0} · a different scale"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    let unit = record.unit("pm2_5");
    ui.label(
        RichText::new(format!(
            "PM2.5 {} · PM10 {}",
            reading(current.pm2_5, unit),
            reading(current.pm10, record.unit("pm10"))
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.label(
        RichText::new(format!(
            "Ozone {} · NO₂ {}",
            reading(current.ozone, record.unit("ozone")),
            reading(current.nitrogen_dioxide, record.unit("nitrogen_dioxide"))
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.label(
        RichText::new(format!(
            "SO₂ {} · CO {}",
            reading(current.sulphur_dioxide, record.unit("sulphur_dioxide")),
            reading(current.carbon_monoxide, record.unit("carbon_monoxide"))
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    if let Some(peak) = record.peak_index() {
        let text = peak.us_aqi.map_or_else(
            || String::from("No hourly index was modelled."),
            |index| {
                format!(
                    "Worst hour {index:.0} · {} · {}",
                    AqiCategory::of(index).label(),
                    short_hour(peak.time_ms)
                )
            },
        );
        ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
    }
    ui.label(
        RichText::new("Modelled on a coarse grid, not a monitor's reading.")
            .size(theme::TYPE_SMALL)
            .weak(),
    );
}

/// The index chart: one bar per hour, coloured by the band it reaches.
fn chart(ui: &mut egui::Ui, record: &AirQuality, palette: &Palette) {
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, CHART_HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + theme::SPACE_S, rect.top() + theme::SPACE_M),
        Pos2::new(rect.right() - theme::SPACE_S, rect.bottom() - DATE_GUTTER),
    );
    let count = record.hours.len();
    if count == 0 {
        return;
    }
    let top = record.axis_top();
    let line = |value: f64| plot.bottom() - plot.height() * (value / top) as f32;
    // The two thresholds a chaser acts on: sensitive groups, then everyone.
    for threshold in [AqiCategory::Sensitive, AqiCategory::Unhealthy] {
        let y = line(threshold.top() - 1.0);
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(
                1.0_f32,
                band_colour(Some(threshold), palette).gamma_multiply(0.7),
            ),
        );
    }
    let slot = plot.width() / count as f32;
    let bar_width = (slot * 0.72).max(1.0);
    for (index, hour) in record.hours.iter().enumerate() {
        let Some(value) = hour.us_aqi else {
            continue;
        };
        let share = (value / top).clamp(0.0, 1.0) as f32;
        let x = plot.left() + slot * index as f32 + (slot - bar_width) / 2.0;
        let height = plot.height() * share;
        let bar = Rect::from_min_max(
            Pos2::new(x, plot.bottom() - height),
            Pos2::new(x + bar_width, plot.bottom()),
        );
        let colour = band_colour(Some(AqiCategory::of(value)), palette);
        painter.rect_filled(bar, egui::CornerRadius::ZERO, colour);
    }
    if let Some(now) = record
        .hours
        .iter()
        .position(|hour| hour.time_ms > record.current.time_ms)
        && now > 0
    {
        // Everything to the right of this line is a forecast hour.
        let x = plot.left() + slot * now as f32;
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0_f32, palette.text.gamma_multiply(0.45)),
        );
    }
    painter.text(
        Pos2::new(plot.left(), plot.top()),
        Align2::LEFT_TOP,
        format!("{top:.0}"),
        FontId::proportional(10.0),
        palette.text,
    );
    for (hour, alignment, x) in [
        (record.hours.first(), Align2::LEFT_TOP, plot.left()),
        (record.hours.last(), Align2::RIGHT_TOP, plot.right()),
    ] {
        let Some(hour) = hour else {
            continue;
        };
        painter.text(
            Pos2::new(x, plot.bottom() + 2.0),
            alignment,
            short_hour(hour.time_ms),
            FontId::proportional(10.0),
            palette.text,
        );
    }
    response.on_hover_text(
        "The United States air-quality index hour by hour, coloured by band. The vertical line marks the current hour: to its right the provider is forecasting.",
    );
}

/// The colour a band is drawn in, and the neutral one used when there is no
/// index at all.
fn band_colour(category: Option<AqiCategory>, palette: &Palette) -> Color32 {
    match category {
        Some(AqiCategory::Good) => palette.accent,
        Some(AqiCategory::Moderate) => palette.warning.gamma_multiply(0.85),
        Some(AqiCategory::Sensitive) => palette.warning,
        Some(AqiCategory::Unhealthy) => palette.error.gamma_multiply(0.8),
        Some(AqiCategory::VeryUnhealthy) => palette.error,
        Some(AqiCategory::Hazardous) => palette.error,
        None => palette.text,
    }
}
/// The day and hour a record point falls on, in UTC.
fn short_hour(time_ms: u64) -> String {
    let Ok(days) = i64::try_from(time_ms / time::MS_PER_DAY) else {
        return format!("<{time_ms} ms is past the calendar>");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (time_ms / 3_600_000) % 24;
    format!("{month:02}-{day:02} {hour:02}Z")
}

/// Distance between two points, from the projection's own geodesic.
fn distance_km(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    match projection::inverse(from_lat, from_lon, to_lat, to_lon) {
        Ok((distance, _)) => distance,
        Err(_) => f64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::{AirCurrent, AirHour};
    use std::collections::BTreeMap;

    fn record() -> AirQuality {
        AirQuality {
            latitude: 39.0,
            longitude: -96.25,
            units: BTreeMap::from([(String::from("pm2_5"), String::from("μg/m³"))]),
            current: AirCurrent {
                time_ms: 20_758 * time::MS_PER_DAY + 3_600_000,
                us_aqi: Some(118.0),
                european_aqi: Some(34.0),
                pm2_5: Some(42.5),
                pm10: Some(58.0),
                ozone: Some(62.0),
                nitrogen_dioxide: Some(3.7),
                sulphur_dioxide: Some(0.3),
                carbon_monoxide: Some(148.0),
            },
            hours: (0..6)
                .map(|index| AirHour {
                    time_ms: 20_758 * time::MS_PER_DAY + index * 3_600_000,
                    pm2_5: Some(10.0 + index as f64),
                    us_aqi: Some(40.0 + index as f64 * 20.0),
                })
                .collect(),
        }
    }

    #[test]
    fn a_hour_label_names_its_day_and_hour() {
        assert_eq!(
            short_hour(20_758 * time::MS_PER_DAY + 6 * 3_600_000),
            "11-01 06Z"
        );
    }

    #[test]
    fn the_description_states_the_span_and_the_grid_point() {
        let text = describe(&record());
        assert!(text.contains("6 hours"), "{text}");
        assert!(text.contains("grid point 39.00, -96.25"), "{text}");
    }

    #[test]
    fn a_concentration_is_shown_with_the_providers_unit() {
        assert_eq!(reading(Some(42.5), "μg/m³"), "42.5 μg/m³");
        assert_eq!(reading(None, "μg/m³"), "not modelled");
    }

    #[test]
    fn the_chart_axis_holds_the_worst_hour() {
        let record = record();
        assert_eq!(
            record.peak_index().and_then(|hour| hour.us_aqi),
            Some(140.0)
        );
        assert_eq!(record.axis_top(), 150.0);
    }

    #[test]
    fn a_new_layer_is_off_and_has_asked_nothing() {
        let layer = AirLayer::new();
        assert!(!layer.shown);
        assert!(layer.asked.is_none());
        assert!(matches!(layer.view.state(), AirState::Idle));
    }

    #[test]
    fn a_short_move_does_not_ask_again() {
        assert!(distance_km(39.0, -96.0, 39.0, -96.0) < 1.0);
        assert!(distance_km(39.0, -96.0, 39.5, -96.0) > MOVE_KM);
    }
}
