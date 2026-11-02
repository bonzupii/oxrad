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
//! The ocean layer: the sea state at the map centre and its rise over three
//! days.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::map::MapView;
use crate::ocean::{self, Ocean, OceanState, OceanView, SeaState};
use crate::projection;
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the sea state is asked for again.
const MOVE_KM: f64 = 15.0;
/// How long a record stands before it is refreshed.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
const CHART_HEIGHT: f32 = 104.0;
/// The room under the plot for the hour labels.
const HOUR_GUTTER: f32 = 15.0;

/// The ocean layer.
pub struct OceanLayer {
    view: OceanView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl OceanLayer {
    pub fn new() -> Self {
        Self {
            view: OceanView::new(),
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
        ui.label(RichText::new("Ocean").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!("{} · global, no key", ocean::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Ocean: on"
        } else {
            "Ocean: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Significant wave height, swell, sea-surface temperature, and the surface current at the nearest sea point, hour by hour for three days.",
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
                RichText::new("Turn the layer on to read the sea at the map centre.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(record) = self.view.record() else {
            return;
        };
        if !record.has_water() {
            ui.label(
                RichText::new(format!(
                    "The nearest sea point to {:.2}, {:.2} carries no wave or current value; inland points have no sea state.",
                    record.latitude, record.longitude
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            return;
        }
        let Ok(now_ms) = time::now_unix_ms() else {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "The system clock cannot be read, so the current hour is unknown.",
            );
            return;
        };
        readout(ui, record, now_ms, palette);
        chart(ui, record, now_ms, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            OceanState::Idle | OceanState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the sea state…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            OceanState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            OceanState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            OceanState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Ocean failed: {error}"),
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

/// A sentence for the status line: the hours, the span, and the sea point.
fn describe(record: &Ocean) -> String {
    let (Some(first), Some(last)) = (record.hours.first(), record.hours.last()) else {
        return String::from("The provider sent no hours.");
    };
    format!(
        "{} hours · {} to {} · sea point {:.2}, {:.2}",
        record.hours.len(),
        short_hour(first.time_ms),
        short_hour(last.time_ms),
        record.latitude,
        record.longitude
    )
}

fn short_hour(time_ms: u64) -> String {
    let Ok(days) = i64::try_from(time_ms / time::MS_PER_DAY) else {
        return format!("<{time_ms} ms is past the calendar>");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (time_ms / 3_600_000) % 24;
    format!("{month:02}-{day:02} {hour:02}Z")
}

/// The compass point a direction names.
fn compass(degrees: f64) -> &'static str {
    const NAMES: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let index = ((degrees / 22.5).round() as i64).rem_euclid(16);
    NAMES
        .get(index as usize)
        .copied()
        .map_or("unknown", |name| name)
}

/// The wave height, its state, the swell, the water temperature, and the
/// current, each with the direction that belongs to it.
fn readout(ui: &mut egui::Ui, record: &Ocean, now_ms: u64, palette: &Palette) {
    match record.at(now_ms) {
        Some(hour) => {
            let text = hour.wave_height.map_or_else(
                || String::from("wave height not modelled"),
                |height| {
                    format!(
                        "Waves {height:.1} {} · sea state {}",
                        record.unit("wave_height"),
                        SeaState::of(height).label()
                    )
                },
            );
            ui.label(
                RichText::new(text)
                    .size(theme::TYPE_SMALL)
                    .strong()
                    .color(state_colour(hour.sea_state(), palette)),
            );
            ui.label(
                RichText::new(format!(
                    "Coming from {} · period {}",
                    direction(hour.wave_from_deg),
                    seconds(hour.wave_period_s)
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            ui.label(
                RichText::new(format!(
                    "Swell {} · sea surface {}",
                    metres(hour.swell_height, record.unit("swell_wave_height")),
                    temperature(hour.sea_temperature, record.unit("sea_surface_temperature"))
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            ui.label(
                RichText::new(format!(
                    "Current {} toward {}",
                    speed(hour.current_kmh, record.unit("ocean_current_velocity")),
                    direction(hour.current_toward_deg)
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        None => {
            ui.label(
                RichText::new("The record begins after now, so the current hour is not in it.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }
    if let Some(roughest) = record.roughest() {
        let text = roughest.wave_height.map_or_else(
            || String::from("No hour reports a wave height."),
            |height| {
                format!(
                    "Roughest {height:.1} {} · {} · {}",
                    record.unit("wave_height"),
                    SeaState::of(height).label(),
                    short_hour(roughest.time_ms)
                )
            },
        );
        ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
    }
    ui.label(
        RichText::new(
            "Wave directions are where the waves come from; the current is where the water goes.",
        )
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}

fn direction(degrees: Option<f64>) -> String {
    degrees.map_or_else(
        || String::from("a direction not modelled"),
        |degrees| format!("{} ({degrees:.0}°)", compass(degrees)),
    )
}

fn seconds(value: Option<f64>) -> String {
    value.map_or_else(
        || String::from("not modelled"),
        |seconds| format!("{seconds:.1} s"),
    )
}

fn metres(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(
        || String::from("not modelled"),
        |metres| format!("{metres:.1} {unit}"),
    )
}

fn temperature(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(
        || String::from("not modelled"),
        |value| format!("{value:.1} {unit}"),
    )
}

fn speed(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(
        || String::from("not modelled"),
        |value| format!("{value:.1} {unit}"),
    )
}

/// The wave chart: one bar per hour, coloured by sea state.
fn chart(ui: &mut egui::Ui, record: &Ocean, now_ms: u64, palette: &Palette) {
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, CHART_HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + theme::SPACE_S, rect.top() + theme::SPACE_M),
        Pos2::new(rect.right() - theme::SPACE_S, rect.bottom() - HOUR_GUTTER),
    );
    let count = record.hours.len();
    if count == 0 {
        return;
    }
    let top = record.axis_top();
    for (level, label) in [(ocean::MODERATE, "moderate"), (ocean::ROUGH, "rough")] {
        if level >= top {
            continue;
        }
        let y = plot.bottom() - plot.height() * (level / top) as f32;
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0_f32, palette.text.gamma_multiply(0.35)),
        );
        painter.text(
            Pos2::new(plot.right(), y - 1.0),
            Align2::RIGHT_BOTTOM,
            label,
            FontId::proportional(9.0),
            palette.text.gamma_multiply(0.7),
        );
    }
    let slot = plot.width() / count as f32;
    let bar_width = (slot * 0.72).max(1.0);
    for (index, hour) in record.hours.iter().enumerate() {
        let Some(height) = hour.wave_height else {
            continue;
        };
        let share = (height / top).clamp(0.0, 1.0) as f32;
        let x = plot.left() + slot * index as f32 + (slot - bar_width) / 2.0;
        let bar = Rect::from_min_max(
            Pos2::new(x, plot.bottom() - plot.height() * share),
            Pos2::new(x + bar_width, plot.bottom()),
        );
        painter.rect_filled(
            bar,
            egui::CornerRadius::ZERO,
            state_colour(hour.sea_state(), palette),
        );
    }
    if let Some(now) = record.hours.iter().position(|hour| hour.time_ms > now_ms)
        && now > 0
    {
        let x = plot.left() + slot * now as f32;
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0_f32, palette.text.gamma_multiply(0.45)),
        );
    }
    painter.text(
        Pos2::new(plot.left(), plot.top()),
        Align2::LEFT_TOP,
        format!("{top:.0} {}", record.unit("wave_height")),
        FontId::proportional(9.0),
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
        "Significant wave height hour by hour, coloured by the Douglas sea state. The vertical line is the current hour, and the faint lines mark the heights at which the sea becomes moderate and then rough.",
    );
}

/// Sea state colours, calm to phenomenal.
fn state_colour(state: Option<SeaState>, palette: &Palette) -> Color32 {
    match state {
        Some(SeaState::Calm | SeaState::Smooth) => palette.accent.gamma_multiply(0.7),
        Some(SeaState::Slight) => palette.accent,
        Some(SeaState::Moderate) => palette.warning.gamma_multiply(0.85),
        Some(SeaState::Rough) => palette.warning,
        Some(SeaState::VeryRough | SeaState::High) => palette.error.gamma_multiply(0.85),
        Some(SeaState::VeryHigh | SeaState::Phenomenal) => palette.error,
        None => palette.text.gamma_multiply(0.5),
    }
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
    use crate::ocean::OceanHour;
    use std::collections::BTreeMap;

    fn hour(height: f64, time_ms: u64) -> OceanHour {
        OceanHour {
            time_ms,
            wave_height: Some(height),
            wave_from_deg: Some(70.0),
            wave_period_s: Some(5.1),
            swell_height: Some(0.6),
            sea_temperature: Some(28.5),
            current_kmh: Some(1.5),
            current_toward_deg: Some(353.0),
        }
    }

    fn record() -> Ocean {
        Ocean {
            latitude: 25.0,
            longitude: -80.0,
            units: BTreeMap::from([
                (String::from("wave_height"), String::from("m")),
                (String::from("sea_surface_temperature"), String::from("°C")),
                (String::from("ocean_current_velocity"), String::from("km/h")),
            ]),
            hours: vec![
                hour(1.3, 20_758 * time::MS_PER_DAY),
                hour(2.9, 20_758 * time::MS_PER_DAY + 3_600_000),
            ],
        }
    }

    #[test]
    fn a_direction_reads_as_a_compass_point() {
        assert_eq!(compass(70.0), "ENE");
        assert_eq!(compass(353.0), "N");
        assert_eq!(compass(180.0), "S");
    }

    #[test]
    fn a_missing_reading_is_not_shown_as_zero() {
        assert_eq!(seconds(None), "not modelled");
        assert_eq!(metres(None, "m"), "not modelled");
        assert_eq!(direction(None), "a direction not modelled");
        assert_eq!(speed(Some(1.5), "km/h"), "1.5 km/h");
    }

    #[test]
    fn the_description_names_the_sea_point() {
        let text = describe(&record());
        assert!(text.contains("2 hours"), "{text}");
        assert!(text.contains("sea point 25.00, -80.00"), "{text}");
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = OceanLayer::new();
        assert!(!layer.shown);
        assert!(matches!(layer.view.state(), OceanState::Idle));
    }

    #[test]
    fn a_short_move_does_not_ask_again() {
        assert!(distance_km(25.0, -80.0, 25.0, -80.0) < 1.0);
        assert!(distance_km(25.0, -80.0, 26.0, -80.0) > MOVE_KM);
    }
}
