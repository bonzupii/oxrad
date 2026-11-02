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
//! The snowpack layer: the snow on the ground at the map centre and what the
//! model expects to add to it.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::map::MapView;
use crate::projection;
use crate::snow::{self, SnowState, SnowView, Snowpack};
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the snowpack is asked for again.
const MOVE_KM: f64 = 20.0;
/// How long a record stands before it is refreshed.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
const CHART_HEIGHT: f32 = 104.0;
/// The room under the plot for the hour labels.
const HOUR_GUTTER: f32 = 15.0;
const DAY_MS: u64 = 86_400_000;

/// The snowpack layer.
pub struct SnowLayer {
    view: SnowView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl SnowLayer {
    pub fn new() -> Self {
        Self {
            view: SnowView::new(),
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
        ui.label(RichText::new("Snowpack").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!("{} · global, no key", snow::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown { "Snow: on" } else { "Snow: off" };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Snow depth at the map centre with the snowfall the model expects hour by hour for the next three days.",
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
                RichText::new("Turn the layer on to read the snow at the map centre.")
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
                "The system clock cannot be read, so the snow's hours have no anchor.",
            );
            return;
        };
        readout(ui, record, now_ms, palette);
        chart(ui, record, now_ms, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            SnowState::Idle | SnowState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the snow model…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            SnowState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            SnowState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            SnowState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Snowpack failed: {error}"),
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

/// A sentence for the status line: the hours, the grid point, and the elevation
/// the model answered for.
fn describe(record: &Snowpack) -> String {
    let (Some(first), Some(last)) = (record.hours.first(), record.hours.last()) else {
        return String::from("The provider sent no hours.");
    };
    let elevation = record.elevation_m.map_or_else(
        || String::from("elevation not stated"),
        |metres| format!("grid elevation {metres:.0} m"),
    );
    format!(
        "{} hours · {} to {} · {elevation}",
        record.hours.len(),
        short_hour(first.time_ms),
        short_hour(last.time_ms)
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

/// The depth now, what fell yesterday, and what the model expects.
fn readout(ui: &mut egui::Ui, record: &Snowpack, now_ms: u64, palette: &Palette) {
    match record.at(now_ms) {
        Some(hour) => {
            let text = hour.depth_cm().map_or_else(
                || String::from("snow depth not modelled"),
                |depth| {
                    let state = if depth >= snow::DEEP_CM {
                        "a deep pack"
                    } else if depth > 0.0 {
                        "a pack on the ground"
                    } else {
                        "bare ground"
                    };
                    format!("Depth {depth:.0} cm · {state}")
                },
            );
            ui.label(
                RichText::new(text)
                    .size(theme::TYPE_SMALL)
                    .strong()
                    .color(depth_colour(hour.depth_cm(), palette)),
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
    let day_ago = now_ms.saturating_sub(DAY_MS);
    let fell = record.snowfall_between(day_ago, now_ms);
    let ahead = record.snowfall_between(now_ms, now_ms.saturating_add(3 * DAY_MS));
    ui.label(
        RichText::new(format!(
            "Fell in the last day {fell:.1} {} · next three days {ahead:.1} {}",
            record.unit("snowfall"),
            record.unit("snowfall")
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    if let Some(heaviest) = record
        .heaviest_fall()
        .filter(|hour| hour.snowfall_cm.is_some_and(|fall| fall > 0.0))
    {
        let text = heaviest.snowfall_cm.map_or_else(
            || String::from("No hour reports any snowfall."),
            |fall| {
                format!(
                    "Heaviest {fall:.1} {}/h · {}",
                    record.unit("snowfall"),
                    short_hour(heaviest.time_ms)
                )
            },
        );
        ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
    }
    if let Some(peak) = record.deepest() {
        let text = peak.depth_cm().map_or_else(
            || String::from("No hour reports a depth."),
            |depth| {
                format!(
                    "Deepest in the record {depth:.0} cm · {}",
                    short_hour(peak.time_ms)
                )
            },
        );
        ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
    }
    ui.label(
        RichText::new(
            "A model grid value for the whole box, which a valley and a ridge can share.",
        )
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}

/// The snowfall chart: one bar per hour, with the current hour marked.
fn chart(ui: &mut egui::Ui, record: &Snowpack, now_ms: u64, palette: &Palette) {
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
    let slot = plot.width() / count as f32;
    let bar_width = (slot * 0.72).max(1.0);
    for (index, hour) in record.hours.iter().enumerate() {
        let Some(fall) = hour.snowfall_cm else {
            continue;
        };
        let share = (fall / top).clamp(0.0, 1.0) as f32;
        let x = plot.left() + slot * index as f32 + (slot - bar_width) / 2.0;
        let bar = Rect::from_min_max(
            Pos2::new(x, plot.bottom() - plot.height() * share),
            Pos2::new(x + bar_width, plot.bottom()),
        );
        painter.rect_filled(bar, egui::CornerRadius::ZERO, fall_colour(fall, palette));
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
        format!("{} {}/h", axis_label(top), record.unit("snowfall")),
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
        "Snowfall hour by hour from the model. The vertical line is the current hour: to its left the model has already run, to its right it is forecasting.",
    );
}

/// The chart's top value, in tenths while the rates are light enough that a
/// whole number would read as zero.
fn axis_label(top: f64) -> String {
    if top < 1.0 {
        format!("{top:.1}")
    } else {
        format!("{top:.0}")
    }
}

/// The colour a snow depth is shown in, from bare ground to a deep pack.
fn depth_colour(depth_cm: Option<f64>, palette: &Palette) -> Color32 {
    match depth_cm {
        Some(depth) if depth >= snow::DEEP_CM => palette.text,
        Some(depth) if depth > 0.0 => palette.accent,
        Some(_) => palette.warning.gamma_multiply(0.8),
        None => palette.text.gamma_multiply(0.5),
    }
}

/// The colour an hour of snowfall is drawn in.
fn fall_colour(fall_cm: f64, palette: &Palette) -> Color32 {
    if fall_cm >= 2.0 {
        palette.accent
    } else if fall_cm > 0.0 {
        palette.accent.gamma_multiply(0.75)
    } else {
        palette.text.gamma_multiply(0.35)
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
    use crate::snow::SnowHour;
    use std::collections::BTreeMap;

    fn record() -> Snowpack {
        Snowpack {
            latitude: 46.5,
            longitude: 10.0,
            elevation_m: Some(2380.0),
            units: BTreeMap::from([(String::from("snowfall"), String::from("cm"))]),
            hours: vec![
                SnowHour {
                    time_ms: 20_758 * time::MS_PER_DAY,
                    depth_m: Some(0.03),
                    snowfall_cm: Some(0.0),
                },
                SnowHour {
                    time_ms: 20_758 * time::MS_PER_DAY + 3_600_000,
                    depth_m: Some(0.05),
                    snowfall_cm: Some(2.0),
                },
            ],
        }
    }

    #[test]
    fn the_description_names_the_grid_elevation() {
        let text = describe(&record());
        assert!(text.contains("grid elevation 2380 m"), "{text}");
        assert!(text.contains("2 hours"), "{text}");
    }

    #[test]
    fn an_hour_label_names_its_day_and_hour() {
        assert_eq!(
            short_hour(20_758 * time::MS_PER_DAY + 6 * 3_600_000),
            "11-01 06Z"
        );
    }

    #[test]
    fn a_light_axis_never_reads_as_zero() {
        assert_eq!(axis_label(0.2), "0.2");
        assert_eq!(axis_label(0.5), "0.5");
        assert_eq!(axis_label(2.0), "2");
    }

    #[test]
    fn a_bare_and_a_deep_pack_are_drawn_differently() {
        let palette = theme::Theme::Dark.palette();
        let bare = depth_colour(Some(0.0), &palette);
        let pack = depth_colour(Some(12.0), &palette);
        let deep = depth_colour(Some(snow::DEEP_CM), &palette);
        assert_ne!(bare, pack);
        assert_ne!(pack, deep);
    }

    #[test]
    fn a_heavy_hour_of_snowfall_is_not_drawn_as_a_trace() {
        let palette = theme::Theme::Dark.palette();
        assert_ne!(fall_colour(0.0, &palette), fall_colour(2.5, &palette));
        assert_ne!(fall_colour(0.5, &palette), fall_colour(2.5, &palette));
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = SnowLayer::new();
        assert!(!layer.shown);
        assert!(matches!(layer.view.state(), SnowState::Idle));
    }

    #[test]
    fn a_short_move_does_not_ask_again() {
        assert!(distance_km(46.5, 10.0, 46.5, 10.0) < 1.0);
        assert!(distance_km(46.5, 10.0, 47.5, 10.0) > MOVE_KM);
    }
}
