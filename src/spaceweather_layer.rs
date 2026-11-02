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
//! The geomagnetic layer: its rail section and the Kp chart.

use std::time::Instant;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::spaceweather::{self, KpStatus, SpaceWeather, SpaceWeatherState, SpaceWeatherView};
use crate::theme::{self, Palette};
use crate::time;

const CHART_HEIGHT: f32 = 104.0;
/// The room under the plot for the date labels.
const DATE_GUTTER: f32 = 15.0;
/// The Kp value at which the geomagnetic storm scale starts.
const STORM_KP: f64 = 5.0;

/// The geomagnetic activity layer.
pub struct SpaceWeatherLayer {
    view: SpaceWeatherView,
    shown: bool,
}

impl SpaceWeatherLayer {
    pub fn new() -> Self {
        Self {
            view: SpaceWeatherView::new(),
            shown: false,
        }
    }

    pub fn update(&mut self, now: Instant) {
        if !self.shown {
            return;
        }
        if matches!(self.view.state(), SpaceWeatherState::Idle) {
            self.view.start(now);
            return;
        }
        self.view.update(now);
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Geomagnetic activity")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · global, no key", spaceweather::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Aurora weather: on"
        } else {
            "Aurora weather: off"
        };
        if ui
            .add_sized([170.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "The planetary K index for the past two days and the next two, with the current solar wind at L1.",
            )
            .clicked()
        {
            self.shown = !self.shown;
        }
        if !self.shown {
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
            SpaceWeatherState::Idle | SpaceWeatherState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the geomagnetic index…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            SpaceWeatherState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            SpaceWeatherState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            SpaceWeatherState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Space weather failed: {error}"),
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

/// A sentence for the status line: the record's span and its predictions.
fn describe(record: &SpaceWeather) -> String {
    let Some(first) = record.points.first() else {
        return String::from("The provider sent no Kp values.");
    };
    let Some(last) = record.points.last() else {
        return String::from("The provider sent no Kp values.");
    };
    format!(
        "{} Kp values · {} to {} · {} predicted · {} estimated",
        record.points.len(),
        short_day(first.time_ms),
        short_day(last.time_ms),
        record.predicted_count(),
        record.estimated_count()
    )
}

/// The current index, the peak, and the solar wind.
fn readout(ui: &mut egui::Ui, record: &SpaceWeather, palette: &Palette) {
    let observed = record.latest_observed();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Now").size(theme::TYPE_SMALL).weak());
        ui.label(
            RichText::new(observed.map_or_else(
                || String::from("no observed value"),
                |point| {
                    format!(
                        "Kp {:.2} · {} · {}",
                        point.kp,
                        kp_label(point.kp),
                        point.status.label()
                    )
                },
            ))
            .size(theme::TYPE_SMALL)
            .strong()
            .color(palette.text),
        );
    });
    if let Some(next) = record.latest().filter(|point| point.status.is_forecast()) {
        ui.label(
            RichText::new(format!(
                "Last prediction {:.2} · {} · {}",
                next.kp,
                kp_label(next.kp),
                short_day(next.time_ms)
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    if let Some(peak) = record.peak() {
        ui.label(
            RichText::new(format!(
                "Peak {:.2} · {} · {}{}",
                peak.kp,
                kp_label(peak.kp),
                short_day(peak.time_ms),
                if peak.status.is_forecast() {
                    " (predicted)"
                } else {
                    ""
                }
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    match &record.wind {
        Some(wind) => {
            let mut parts = Vec::new();
            if let Some(speed) = wind.speed_km_s {
                parts.push(format!("solar wind {speed:.0} km/s"));
            }
            if let Some(bz) = wind.bz_nt {
                parts.push(format!("Bz {bz:+.1} nT"));
            }
            if let Some(bt) = wind.bt_nt {
                parts.push(format!("Bt {bt:.1} nT"));
            }
            ui.label(
                RichText::new(format!(
                    "{} · {}",
                    parts.join(" · "),
                    short_day(wind.time_ms)
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            ui.label(
                RichText::new("Measured at L1, an hour upstream of the aurora.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
        None => {
            let note = record
                .wind_note
                .as_deref()
                .map_or("The provider sent no solar-wind measurement.", |note| note);
            ui.label(
                RichText::new(format!("Solar wind unavailable: {note}"))
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }
}

/// The Kp chart: one bar per three-hour value, predicted bars drawn hollow.
fn chart(ui: &mut egui::Ui, record: &SpaceWeather, palette: &Palette) {
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, CHART_HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + theme::SPACE_S, rect.top() + theme::SPACE_M),
        Pos2::new(rect.right() - theme::SPACE_S, rect.bottom() - DATE_GUTTER),
    );
    let count = record.points.len();
    if count == 0 {
        return;
    }
    // The scale is the storm scale, not the data: a quiet day should look quiet.
    let top = 9.0_f64;
    let storm_y = plot.bottom() - plot.height() * (STORM_KP / top) as f32;
    painter.line_segment(
        [
            Pos2::new(plot.left(), storm_y),
            Pos2::new(plot.right(), storm_y),
        ],
        Stroke::new(1.0_f32, palette.warning),
    );
    painter.text(
        Pos2::new(plot.right(), storm_y - 2.0),
        Align2::RIGHT_BOTTOM,
        "G1",
        FontId::proportional(10.0),
        palette.warning,
    );
    let slot = plot.width() / count as f32;
    let bar_width = (slot * 0.72).max(1.0);
    for (index, point) in record.points.iter().enumerate() {
        let share = (point.kp / top).clamp(0.0, 1.0) as f32;
        let x = plot.left() + slot * index as f32 + (slot - bar_width) / 2.0;
        let height = plot.height() * share;
        let bar = Rect::from_min_max(
            Pos2::new(x, plot.bottom() - height),
            Pos2::new(x + bar_width, plot.bottom()),
        );
        let colour = kp_colour(point.kp, point.status, palette);
        if point.status.is_forecast() {
            painter.rect_stroke(
                bar,
                egui::CornerRadius::ZERO,
                Stroke::new(1.0_f32, colour),
                egui::StrokeKind::Inside,
            );
        } else {
            painter.rect_filled(bar, egui::CornerRadius::ZERO, colour);
        }
    }
    if let Some(now) = record
        .points
        .iter()
        .position(|point| point.status.is_forecast())
        && now > 0
    {
        // The forecast starts here: measured to the left, model output to the right.
        let x = plot.left() + slot * now as f32;
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0_f32, palette.text.gamma_multiply(0.45)),
        );
    }
    painter.text(
        Pos2::new(plot.left(), plot.top()),
        Align2::LEFT_TOP,
        "Kp 9",
        FontId::proportional(10.0),
        palette.text,
    );
    painter.text(
        Pos2::new(plot.left(), plot.bottom() + 2.0),
        Align2::LEFT_TOP,
        record
            .points
            .first()
            .map_or_else(String::new, |point| short_day(point.time_ms)),
        FontId::proportional(10.0),
        palette.text,
    );
    painter.text(
        Pos2::new(plot.right(), plot.bottom() + 2.0),
        Align2::RIGHT_TOP,
        record
            .points
            .last()
            .map_or_else(String::new, |point| short_day(point.time_ms)),
        FontId::proportional(10.0),
        palette.text,
    );
    response.on_hover_text(
        "Three-hour planetary K index values. Filled bars are measured, outlined bars are the forecast, and the amber line is the start of the geomagnetic storm scale at Kp 5.",
    );
}

/// Fill colour for a Kp value: quiet, storm, or severe, and dimmed for a
/// prediction so a forecast bar is never read as a measurement.
fn kp_colour(kp: f64, status: KpStatus, palette: &Palette) -> Color32 {
    let base = if kp >= 7.0 {
        palette.error
    } else if kp >= STORM_KP {
        palette.warning
    } else {
        palette.accent
    };
    if status.is_forecast() {
        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 190)
    } else {
        base
    }
}

/// The storm scale wording for a value, in the rail's own words.
const fn kp_label(kp: f64) -> &'static str {
    spaceweather::storm_scale(kp)
}

fn short_day(time_ms: u64) -> String {
    match time::format_utc(time_ms) {
        Ok(text) => String::from(text.get(5..16).unwrap_or(&text)),
        Err(error) => format!("<{error}>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaceweather::{KpPoint, SolarWind, SpaceWeather};

    fn record(kp: &[(f64, bool)]) -> SpaceWeather {
        SpaceWeather {
            points: kp
                .iter()
                .enumerate()
                .map(|(index, (value, predicted))| KpPoint {
                    time_ms: 20_758 * time::MS_PER_DAY + index as u64 * 10_800_000,
                    kp: *value,
                    status: if *predicted {
                        KpStatus::Predicted
                    } else {
                        KpStatus::Observed
                    },
                })
                .collect(),
            wind: Some(SolarWind {
                speed_km_s: Some(492.0),
                bt_nt: Some(4.0),
                bz_nt: Some(-2.0),
                time_ms: 20_758 * time::MS_PER_DAY + 73_440_000,
            }),
            wind_note: None,
        }
    }

    #[test]
    fn a_quiet_chart_is_not_drawn_as_a_quiet_day_of_storms() {
        assert_eq!(kp_label(4.3), "below storm level");
        assert_eq!(kp_label(6.0), "G2 moderate");
    }

    #[test]
    fn the_description_states_the_span_and_the_predictions() {
        let text = describe(&record(&[(1.0, false), (2.0, false), (6.0, true)]));
        assert!(text.contains("3 Kp values"), "{text}");
        assert!(text.contains("1 predicted"), "{text}");
    }

    #[test]
    fn a_record_without_wind_still_describes_itself() {
        let mut weather = record(&[(3.0, false)]);
        weather.wind = None;
        weather.wind_note = Some(String::from("the request timed out"));
        assert!(describe(&weather).contains("1 Kp values"));
        assert!(weather.wind.is_none());
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = SpaceWeatherLayer::new();
        assert!(!layer.shown);
        assert!(matches!(layer.view.state(), SpaceWeatherState::Idle));
    }
}
