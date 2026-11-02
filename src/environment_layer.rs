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
//! The storm-environment layer: what the air over the map centre can do, and
//! how that changes over the next two days.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::environment::{
    self, CapeCategory, Environment, EnvironmentHour, EnvironmentState, EnvironmentView,
};
use crate::map::MapView;
use crate::projection;
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the fields are asked for again.
const MOVE_KM: f64 = 20.0;
/// How long a record stands before it is refreshed.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
const CHART_HEIGHT: f32 = 104.0;
/// The room under the plot for the hour labels.
const HOUR_GUTTER: f32 = 15.0;

/// The storm-environment layer.
pub struct EnvironmentLayer {
    view: EnvironmentView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl EnvironmentLayer {
    pub fn new() -> Self {
        Self {
            view: EnvironmentView::new(),
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
        ui.label(
            RichText::new("Storm environment")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · global, no key", environment::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Environment: on"
        } else {
            "Environment: off"
        };
        if ui
            .add_sized([170.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "The model's convective energy, inhibition, lifted index, freezing level, and wind difference with height at the map centre, hour by hour.",
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
                RichText::new("Turn the layer on to read the air over the map centre.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(record) = self.view.record() else {
            return;
        };
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
            EnvironmentState::Idle | EnvironmentState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the convective fields…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            EnvironmentState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            EnvironmentState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            EnvironmentState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Storm environment failed: {error}"),
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

/// A sentence for the status line: the hours and the grid point.
fn describe(record: &Environment) -> String {
    let (Some(first), Some(last)) = (record.hours.first(), record.hours.last()) else {
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

fn short_hour(time_ms: u64) -> String {
    let Ok(days) = i64::try_from(time_ms / time::MS_PER_DAY) else {
        return format!("<{time_ms} ms is past the calendar>");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (time_ms / 3_600_000) % 24;
    format!("{month:02}-{day:02} {hour:02}Z")
}

/// The compass point a direction blows from.
fn compass(from_deg: f64) -> &'static str {
    const NAMES: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let index = ((from_deg / 22.5).round() as i64).rem_euclid(16);
    NAMES
        .get(index as usize)
        .copied()
        .map_or("unknown", |name| name)
}

/// Today's air, the shear, and the peak ahead.
fn readout(ui: &mut egui::Ui, record: &Environment, now_ms: u64, palette: &Palette) {
    match record.at(now_ms) {
        Some(hour) => {
            let cape = hour.cape;
            let text = cape.map_or_else(
                || String::from("energy not modelled"),
                |cape| format!("CAPE {cape:.0} J/kg · {}", CapeCategory::of(cape).label()),
            );
            ui.label(
                RichText::new(text)
                    .size(theme::TYPE_SMALL)
                    .strong()
                    .color(cape_colour(hour.category(), palette)),
            );
            ui.label(
                RichText::new(format!(
                    "CIN {} J/kg · lifted index {} · freezing level {}",
                    amount(hour.cin, "J/kg"),
                    amount(hour.lifted_index, ""),
                    height(hour.freezing_level_m)
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            shear_line(ui, hour);
            if hour.cin.is_some_and(|cin| cin >= 100.0) {
                ui.label(
                    RichText::new(
                        "The inhibition is holding the air down; nothing fires without a trigger.",
                    )
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
        }
        None => {
            ui.label(
                RichText::new("The record begins after now, so the current hour is not in it.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }
    if let Some(peak) = record.peak() {
        let text = peak.cape.map_or_else(
            || String::from("No hour reports any energy."),
            |cape| {
                format!(
                    "Peak {cape:.0} J/kg · {} · {}",
                    CapeCategory::of(cape).label(),
                    short_hour(peak.time_ms)
                )
            },
        );
        ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
    }
    ui.label(
        RichText::new("Model fields on the forecast grid, not a launched sounding.")
            .size(theme::TYPE_SMALL)
            .weak(),
    );
}

fn shear_line(ui: &mut egui::Ui, hour: EnvironmentHour) {
    let text = hour.shear().map_or_else(
        || String::from("Wind difference with height not modelled"),
        |shear| {
            format!(
                "Surface to 500 hPa wind difference {:.0} km/h out of {}",
                shear.speed_kmh,
                compass(shear.from_deg)
            )
        },
    );
    ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
}

fn amount(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(
        || String::from("not modelled"),
        |value| {
            if unit.is_empty() {
                format!("{value:.1}")
            } else {
                format!("{value:.0} {unit}")
            }
        },
    )
}

fn height(value: Option<f64>) -> String {
    value.map_or_else(
        || String::from("not modelled"),
        |metres| format!("{metres:.0} m"),
    )
}

/// The energy chart: one bar per hour, with the storm thresholds drawn in.
fn chart(ui: &mut egui::Ui, record: &Environment, now_ms: u64, palette: &Palette) {
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
    for (level, label) in [
        (environment::CAPE_MODERATE, "1000"),
        (environment::CAPE_LARGE, "2500"),
    ] {
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
        let Some(cape) = hour.cape else {
            continue;
        };
        let share = (cape / top).clamp(0.0, 1.0) as f32;
        let x = plot.left() + slot * index as f32 + (slot - bar_width) / 2.0;
        let height = plot.height() * share;
        let bar = Rect::from_min_max(
            Pos2::new(x, plot.bottom() - height),
            Pos2::new(x + bar_width, plot.bottom()),
        );
        painter.rect_filled(
            bar,
            egui::CornerRadius::ZERO,
            cape_colour(hour.category(), palette),
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
        format!("{top:.0} J/kg"),
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
        "Convective available potential energy hour by hour, coloured by what the model's value can support. The vertical line is the current hour; the faint lines mark the energy at which thunderstorms and then organised storms become possible.",
    );
}

/// Energy in the warning range as it grows, so a capped day is not drawn as a
/// storm day.
fn cape_colour(category: Option<CapeCategory>, palette: &Palette) -> Color32 {
    match category {
        Some(CapeCategory::Weak) => palette.accent.gamma_multiply(0.75),
        Some(CapeCategory::Moderate) => palette.accent,
        Some(CapeCategory::Large) => palette.warning,
        Some(CapeCategory::Extreme) => palette.error,
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
    use crate::environment::Wind;

    fn hour(cape: f64, time_ms: u64) -> EnvironmentHour {
        EnvironmentHour {
            time_ms,
            cape: Some(cape),
            cin: Some(20.0),
            lifted_index: Some(-2.0),
            freezing_level_m: Some(4_200.0),
            surface: Some(Wind {
                speed_kmh: 20.0,
                from_deg: 180.0,
            }),
            low: Some(Wind {
                speed_kmh: 35.0,
                from_deg: 200.0,
            }),
            mid: Some(Wind {
                speed_kmh: 50.0,
                from_deg: 250.0,
            }),
        }
    }

    fn record() -> Environment {
        Environment {
            latitude: 39.0,
            longitude: -96.2,
            elevation_m: Some(347.0),
            hours: vec![
                hour(10.0, 20_758 * time::MS_PER_DAY),
                hour(2_000.0, 20_758 * time::MS_PER_DAY + 3_600_000),
            ],
        }
    }

    #[test]
    fn an_hour_label_names_its_day_and_hour() {
        assert_eq!(
            short_hour(20_758 * time::MS_PER_DAY + 6 * 3_600_000),
            "11-01 06Z"
        );
    }

    #[test]
    fn a_direction_reads_as_a_compass_point() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(180.0), "S");
        assert_eq!(compass(250.0), "WSW");
        assert_eq!(compass(359.0), "N");
        assert_eq!(compass(725.0), "N", "a direction past a full turn");
    }

    #[test]
    fn the_description_states_the_span_and_the_grid_point() {
        let text = describe(&record());
        assert!(text.contains("2 hours"), "{text}");
        assert!(text.contains("grid point 39.00, -96.20"), "{text}");
    }

    #[test]
    fn a_missing_value_is_not_shown_as_zero() {
        assert_eq!(amount(None, "J/kg"), "not modelled");
        assert_eq!(height(None), "not modelled");
        assert_eq!(amount(Some(25.0), "J/kg"), "25 J/kg");
        assert_eq!(amount(Some(-2.5), ""), "-2.5");
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = EnvironmentLayer::new();
        assert!(!layer.shown);
        assert!(matches!(layer.view.state(), EnvironmentState::Idle));
    }

    #[test]
    fn a_short_move_does_not_ask_again() {
        assert!(distance_km(39.0, -96.0, 39.0, -96.0) < 1.0);
        assert!(distance_km(39.0, -96.0, 40.0, -96.0) > MOVE_KM);
    }
}
