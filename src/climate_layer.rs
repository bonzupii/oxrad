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
//! The climate layer: this month against the reanalysis normals, and the
//! history of that month as a diverging chart.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::climate::{self, ClimateRecord, ClimateState, ClimateView, MonthValue, Normal};
use crate::map::MapView;
use crate::projection;
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the record is asked for again.
const MOVE_KM: f64 = 50.0;
/// The reanalysis gains a day at a time, so a record stands for a while.
const REFRESH_AFTER: Duration = Duration::from_secs(6 * 3600);
const CHART_HEIGHT: f32 = 108.0;
/// The room under the plot for the year labels.
const YEAR_GUTTER: f32 = 15.0;

/// The climate layer.
pub struct ClimateLayer {
    view: ClimateView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl ClimateLayer {
    pub fn new() -> Self {
        Self {
            view: ClimateView::new(),
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
        ui.label(RichText::new("Climate").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!("{} · global, no key", climate::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Climate: on"
        } else {
            "Climate: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "This calendar month at the map centre against the same month in 1991 to 2020, with every year of the record behind it.",
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
                RichText::new("Turn the layer on to compare this month with the baseline.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(record) = self.view.record() else {
            return;
        };
        let Some(month) = record.latest_complete_month() else {
            ui.label(
                RichText::new("The archive holds no complete month for this point yet.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        };
        readout(ui, record, month, palette);
        chart(ui, record, month, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            ClimateState::Idle | ClimateState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the reanalysis — this asks for decades of days…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            ClimateState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            ClimateState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            ClimateState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Climate failed: {error}"),
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

/// A sentence for the status line: how many days the record holds and where it
/// starts and ends.
fn describe(record: &ClimateRecord) -> String {
    let (Some(first), Some(last)) = (record.days.first(), record.days.last()) else {
        return String::from("The archive sent no days.");
    };
    let (start_year, _, _) = time::civil_from_days(first.day);
    let (end_year, end_month, end_day) = time::civil_from_days(last.day);
    format!(
        "{} days · {start_year} to {end_year}-{end_month:02}-{end_day:02} · 1991–2020 baseline",
        record.days.len()
    )
}

const fn month_name(month: u32) -> &'static str {
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

/// The month's mean, its anomaly, and how it ranks against the record.
fn readout(ui: &mut egui::Ui, record: &ClimateRecord, month: MonthValue, palette: &Palette) {
    let Some(value) = month.temperature else {
        ui.label(
            RichText::new(format!(
                "{} {} has no mean temperature in the archive.",
                month_name(month.month),
                month.year
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        return;
    };
    ui.label(
        RichText::new(format!(
            "{} {} · {value:.1} {}",
            month_name(month.month),
            month.year,
            record.temperature_unit
        ))
        .size(theme::TYPE_SMALL)
        .strong()
        .color(palette.text),
    );
    match record.normal(month.month) {
        Some(normal) => anomaly_lines(ui, record, month, &normal, palette),
        None => {
            ui.label(
                RichText::new("The archive holds no baseline months to compare against.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }
}

fn anomaly_lines(
    ui: &mut egui::Ui,
    record: &ClimateRecord,
    month: MonthValue,
    normal: &Normal,
    palette: &Palette,
) {
    let Some(value) = month.temperature else {
        return;
    };
    let anomaly = value - normal.temperature;
    ui.label(
        RichText::new(format!(
            "{anomaly:+.1} vs the {} normal of {:.1} · {:.1} to {:.1} across {} years",
            month_name(normal.month),
            normal.temperature,
            normal.low,
            normal.high,
            normal.years
        ))
        .size(theme::TYPE_SMALL)
        .color(anomaly_colour(anomaly, palette)),
    );
    if let Some((rank, total)) = record.rank(month) {
        let name = month_name(month.month);
        let text = if rank == 1 {
            format!("The warmest of {total} {name} values in the record")
        } else {
            format!("{rank} of {total} {name} values, warmest first")
        };
        ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
    }
    if let (Some(wet), Some(wet_normal)) = (month.precipitation, normal.precipitation) {
        let share = if wet_normal > 0.0 {
            wet / wet_normal * 100.0
        } else {
            0.0
        };
        ui.label(
            RichText::new(format!(
                "{wet:.1} {} of precipitation · {share:.0}% of the normal {wet_normal:.1}",
                record.precipitation_unit
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
}

/// The chart: each year's value for this calendar month, drawn as a departure
/// from the baseline so the sign is visible at a glance.
fn chart(ui: &mut egui::Ui, record: &ClimateRecord, month: MonthValue, palette: &Palette) {
    let Some(normal) = record.normal(month.month) else {
        return;
    };
    let values: Vec<(i32, f64)> = record
        .same_month(month.month)
        .into_iter()
        .filter_map(|value| Some((value.year, value.temperature? - normal.temperature)))
        .collect();
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, CHART_HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    if values.is_empty() {
        return;
    }
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + theme::SPACE_S, rect.top() + theme::SPACE_M),
        Pos2::new(rect.right() - theme::SPACE_S, rect.bottom() - YEAR_GUTTER),
    );
    let extent = values
        .iter()
        .map(|(_, anomaly)| anomaly.abs())
        .fold(1.0_f64, f64::max);
    let zero = plot.center().y;
    let scale = (plot.height() / 2.0) / extent as f32;
    painter.line_segment(
        [Pos2::new(plot.left(), zero), Pos2::new(plot.right(), zero)],
        Stroke::new(1.0_f32, palette.text.gamma_multiply(0.6)),
    );
    let slot = plot.width() / values.len() as f32;
    let bar_width = (slot * 0.7).max(1.0);
    for (index, (_, anomaly)) in values.iter().enumerate() {
        let height = (*anomaly as f32) * scale;
        let x = plot.left() + slot * index as f32 + (slot - bar_width) / 2.0;
        let bar = if height >= 0.0 {
            Rect::from_min_max(Pos2::new(x, zero - height), Pos2::new(x + bar_width, zero))
        } else {
            Rect::from_min_max(Pos2::new(x, zero), Pos2::new(x + bar_width, zero - height))
        };
        painter.rect_filled(
            bar,
            egui::CornerRadius::ZERO,
            anomaly_colour(*anomaly, palette),
        );
        if index + 1 == values.len() {
            // The bar the readout is describing.
            painter.rect_stroke(
                bar,
                egui::CornerRadius::ZERO,
                Stroke::new(1.0_f32, palette.text),
                egui::StrokeKind::Inside,
            );
        }
    }
    painter.text(
        Pos2::new(plot.left(), plot.top()),
        Align2::LEFT_TOP,
        format!("+{extent:.1}"),
        FontId::proportional(10.0),
        palette.text,
    );
    painter.text(
        Pos2::new(plot.left(), plot.bottom()),
        Align2::LEFT_BOTTOM,
        format!("-{extent:.1}"),
        FontId::proportional(10.0),
        palette.text,
    );
    for (index, alignment, x) in [
        (0_usize, Align2::LEFT_TOP, plot.left()),
        (
            values.len().saturating_sub(1),
            Align2::RIGHT_TOP,
            plot.right(),
        ),
    ] {
        let Some((year, _)) = values.get(index) else {
            continue;
        };
        painter.text(
            Pos2::new(x, plot.bottom() + 2.0),
            alignment,
            format!("{year}"),
            FontId::proportional(10.0),
            palette.text,
        );
    }
    response.on_hover_text(
        "Each year's mean for this calendar month, as a departure from the 1991–2020 baseline in the same reanalysis. Bars above the line were warmer than the baseline, bars below were cooler.",
    );
}

/// Warm departures in the warning colour, cool ones in the cool accent.
fn anomaly_colour(anomaly: f64, palette: &Palette) -> Color32 {
    if anomaly >= 0.0 {
        palette.warning
    } else {
        palette.accent
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
    use crate::climate::DayValue;

    /// Forty baseline Octobers plus a warm one, which is enough for the
    /// baseline and the chart to have something real to draw.
    fn record() -> ClimateRecord {
        let mut days = Vec::new();
        for year in 1991..=2020 {
            for day in 1..=31 {
                days.push(DayValue {
                    day: time::days_from_civil(year, 10, day),
                    temperature: Some(10.0),
                    precipitation: Some(1.0),
                });
            }
        }
        for day in 1..=31 {
            days.push(DayValue {
                day: time::days_from_civil(2026, 10, day),
                temperature: Some(13.0),
                precipitation: Some(3.0),
            });
        }
        ClimateRecord {
            latitude: 39.0,
            longitude: -96.2,
            elevation_m: Some(347.0),
            temperature_unit: String::from("°C"),
            precipitation_unit: String::from("mm"),
            days,
        }
    }

    #[test]
    fn the_baseline_is_the_mean_of_its_years() {
        let record = record();
        let normal = record.normal(10);
        assert!(normal.is_some());
        if let Some(normal) = normal {
            assert!((normal.temperature - 10.0).abs() < 1e-9);
            assert_eq!(normal.years, 30);
            assert_eq!(normal.low, 10.0);
            assert_eq!(normal.high, 10.0);
        }
    }

    #[test]
    fn the_latest_month_is_the_warm_one() {
        let record = record();
        let Some(month) = record.latest_complete_month() else {
            return;
        };
        assert_eq!(month.year, 2026);
        assert_eq!(record.rank(month), Some((1, 31)));
    }

    #[test]
    fn a_month_name_reads_as_a_word() {
        assert_eq!(month_name(1), "January");
        assert_eq!(month_name(10), "October");
        assert_eq!(month_name(12), "December");
    }

    #[test]
    fn a_short_move_does_not_ask_again() {
        assert!(distance_km(39.0, -96.0, 39.0, -96.0) < 1.0);
        assert!(distance_km(39.0, -96.0, 41.0, -96.0) > MOVE_KM);
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = ClimateLayer::new();
        assert!(!layer.shown);
        assert!(matches!(layer.view.state(), ClimateState::Idle));
    }
}
