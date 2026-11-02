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
//! The river-gauge layer: measured stage and discharge around the map centre,
//! and a gauge's own week once one is chosen.

use std::fmt::Write;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, Painter, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::gauge::{self, Gauge, GaugeSet, GaugeState, GaugeView, History, HistoryState};
use crate::map::{MapTransform, MapView};
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the gauges are asked for again.
const MOVE_KM: f64 = 12.0;
/// How long a gauge set stands before it is refreshed. A gauge publishes every
/// fifteen minutes, so a quarter of an hour is the natural cadence.
const REFRESH_AFTER: Duration = Duration::from_secs(900);
const CHART_HEIGHT: f32 = 96.0;
const HOUR_GUTTER: f32 = 14.0;

/// The river-gauge layer.
pub struct GaugeLayer {
    view: GaugeView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
    selected: Option<String>,
    /// The gauge whose week is loaded, or being loaded, so a click starts one
    /// request rather than one per frame.
    asked_history: Option<String>,
    focus: Option<(f64, f64)>,
}

impl GaugeLayer {
    pub fn new() -> Self {
        Self {
            view: GaugeView::new(),
            shown: false,
            asked: None,
            asked_at: None,
            selected: None,
            asked_history: None,
            focus: None,
        }
    }

    /// Follows the map centre while the layer is on.
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
            self.asked_at = Some(now);
            self.view.start(
                gauge::bounds(lat, lon, view.span_km()),
                self.view.attempts(),
            );
        }
    }

    /// The gauges the layer holds, for anything else that reports them.
    pub const fn gauges(&self) -> Option<&GaugeSet> {
        self.view.set()
    }

    /// A gauge the reader clicked, so the shell can centre the map on it.
    pub const fn take_focus(&mut self) -> Option<(f64, f64)> {
        self.focus.take()
    }

    /// The rail section.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette, centre: Option<(f64, f64)>) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(RichText::new("River gauges").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!("{} · measured, no key", gauge::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Gauges: on"
        } else {
            "Gauges: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Stage and discharge measured at the river gauges around the map centre, with the agency's own provisional and quality marks.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if !self.shown {
                self.asked = None;
                self.view.close_history();
                self.selected = None;
                self.asked_history = None;
            }
        }
        if !self.shown {
            ui.label(
                RichText::new("Turn the layer on to read the gauges around the map centre.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(set) = self.view.set().cloned() else {
            return;
        };
        list(
            ui,
            &set,
            centre,
            palette,
            &mut self.selected,
            &mut self.focus,
        );
        if self.selected != self.asked_history
            && let Some(site) = self.selected.clone()
        {
            self.asked_history = Some(site.clone());
            self.view.start_history(&site);
        }
        self.hydrograph(ui, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            GaugeState::Idle | GaugeState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the river gauges…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            GaugeState::Loading { last: Some(set) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(set)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            GaugeState::Ready { set } => {
                ui.label(RichText::new(describe(set)).size(theme::TYPE_SMALL).weak());
            }
            GaugeState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("River gauges failed: {error}"),
                );
                if let Some(set) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", describe(set)))
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

    /// The chosen gauge's week, drawn under the list.
    fn hydrograph(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        match self.view.history() {
            HistoryState::Off => {}
            HistoryState::Loading { site } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Reading the week at gauge {site}…"))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            HistoryState::Failed { site, error } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("The week at gauge {site} could not be read: {error}"),
                );
            }
            HistoryState::Ready { history } => {
                let history = history.clone();
                draw_history(ui, &history, palette);
            }
        }
        if !matches!(self.view.history(), HistoryState::Off)
            && ui.small_button("Close the week").clicked()
        {
            self.view.close_history();
            self.selected = None;
            self.asked_history = None;
        }
    }

    /// Draws a marker at every gauge in the set.
    pub fn draw(&self, painter: &Painter, transform: &MapTransform, rect: Rect, palette: &Palette) {
        if !self.shown {
            return;
        }
        let Some(set) = self.view.set() else {
            return;
        };
        for gauge in &set.gauges {
            let Some(point) = transform.screen(gauge.latitude, gauge.longitude) else {
                continue;
            };
            if !rect.contains(point) {
                continue;
            }
            let centre = Pos2::new(point.x, point.y);
            let chosen = self.selected.as_deref() == Some(gauge.site.as_str());
            let colour = if chosen {
                palette.accent
            } else {
                let _ = palette;
                WATER
            };
            painter.circle_filled(centre, if chosen { 4.5 } else { 3.0 }, colour);
            if chosen {
                painter.circle_stroke(centre, 8.0, Stroke::new(1.2_f32, colour));
                painter.text(
                    Pos2::new(centre.x + 10.0, centre.y),
                    Align2::LEFT_CENTER,
                    &gauge.name,
                    FontId::proportional(11.0),
                    palette.canvas_text,
                );
            }
        }
    }
}

/// The gauge marker colour: a water blue that reads on the dark canvas.
const WATER: egui::Color32 = egui::Color32::from_rgb(96, 178, 236);

/// The set in one line.
fn describe(set: &GaugeSet) -> String {
    let provisional = set
        .gauges
        .iter()
        .filter(|gauge| {
            gauge
                .stage
                .as_ref()
                .is_some_and(|reading| reading.provisional)
                || gauge
                    .discharge
                    .as_ref()
                    .is_some_and(|reading| reading.provisional)
        })
        .count();
    let mut line = format!("{} gauges in view", set.gauges.len());
    if provisional > 0 {
        let _ = write!(line, " · {provisional} with provisional values");
    }
    if set.rejected > 0 {
        let _ = write!(line, " · {} series unreadable", set.rejected);
    }
    line
}

/// The gauges, nearest first.
fn list(
    ui: &mut egui::Ui,
    set: &GaugeSet,
    centre: Option<(f64, f64)>,
    palette: &Palette,
    selected: &mut Option<String>,
    focus: &mut Option<(f64, f64)>,
) {
    if set.gauges.is_empty() {
        ui.label(
            RichText::new(
                "No gauge reports stage or discharge inside this view. The service covers the United States and its territories, so a view elsewhere will stay empty.",
            )
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        return;
    }
    let Ok(now_ms) = time::now_unix_ms() else {
        ui.colored_label(
            ui.visuals().error_fg_color,
            "The system clock cannot be read, so the gauge ages cannot be shown.",
        );
        return;
    };
    let mut gauges: Vec<&Gauge> = set.gauges.iter().collect();
    if let Some((latitude, longitude)) = centre {
        gauges.sort_by(|left, right| {
            left.distance_from(latitude, longitude)
                .total_cmp(&right.distance_from(latitude, longitude))
        });
    }
    for gauge in gauges.iter().take(gauge::LIST_LIMIT) {
        let is_chosen = selected.as_deref() == Some(gauge.site.as_str());
        let mut text = String::from(&gauge.name);
        if let Some(stage) = &gauge.stage {
            let _ = write!(
                text,
                " · {:.2} {}{}",
                stage.value,
                stage.unit,
                if stage.provisional {
                    " provisional"
                } else {
                    ""
                }
            );
        }
        if let Some(discharge) = &gauge.discharge {
            let _ = write!(
                text,
                " · {:.0} {}{}",
                discharge.value,
                discharge.unit,
                if discharge.provisional {
                    " provisional"
                } else {
                    ""
                }
            );
        }
        if let Some(newest) = gauge.newest() {
            let _ = write!(text, " · {}", age_label(now_ms, newest.at_ms));
        }
        let response = ui
            .add(
                egui::Label::new(
                    RichText::new(text)
                        .size(theme::TYPE_SMALL)
                        .color(if is_chosen {
                            palette.accent
                        } else {
                            palette.text
                        }),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text(format!(
                "USGS gauge {}{}\nClick to centre the map and read its week.",
                gauge.site,
                gauge
                    .stage
                    .as_ref()
                    .and_then(|reading| reading.qualifier_label())
                    .map_or_else(String::new, |label| format!(" · {label}"))
            ));
        if response.clicked() {
            *selected = Some(gauge.site.clone());
            *focus = Some((gauge.latitude, gauge.longitude));
        }
    }
    if set.gauges.len() > gauge::LIST_LIMIT {
        ui.label(
            RichText::new(format!(
                "{} more gauges are in view but not listed.",
                set.gauges.len() - gauge::LIST_LIMIT
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    ui.label(
        RichText::new(
            "Provisional values are the agency's unapproved numbers and may still be revised. The layer shows the units the service states for each series rather than converting them.",
        )
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}

/// The chosen gauge's week as a hydrograph.
fn draw_history(ui: &mut egui::Ui, history: &History, palette: &Palette) {
    ui.label(
        RichText::new(format!("{} · gauge {}", history.name, history.site))
            .size(theme::TYPE_SMALL)
            .strong(),
    );
    let Some(highest) = history.highest_stage() else {
        ui.label(
            RichText::new("This gauge published no stage in the last week.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        return;
    };
    let unit = highest.unit.clone();
    let top = history.axis_top();
    let floor = history.axis_floor();
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, CHART_HEIGHT + HOUR_GUTTER),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 4.0, rect.top() + 6.0),
        Pos2::new(rect.right() - 4.0, rect.bottom() - HOUR_GUTTER),
    );
    let Some(first) = history.stage.first() else {
        return;
    };
    let last = history.stage.last().unwrap_or(first);
    let span = last.at_ms.saturating_sub(first.at_ms).max(1);
    let mut previous: Option<Pos2> = None;
    let height = (top - floor).max(0.1);
    for reading in &history.stage {
        let across = reading.at_ms.saturating_sub(first.at_ms) as f32 / span as f32;
        let up = ((reading.value - floor) / height).clamp(0.0, 1.0) as f32;
        let point = Pos2::new(
            plot.left() + plot.width() * across,
            plot.bottom() - plot.height() * up,
        );
        if let Some(from) = previous {
            painter.line_segment([from, point], Stroke::new(1.2_f32, WATER));
        }
        previous = Some(point);
    }
    painter.text(
        Pos2::new(plot.left() + 2.0, plot.top()),
        Align2::LEFT_TOP,
        format!("{top:.1} {unit}"),
        FontId::proportional(9.0),
        palette.text,
    );
    painter.text(
        Pos2::new(plot.left() + 2.0, plot.bottom() - 10.0),
        Align2::LEFT_TOP,
        format!("{floor:.1} {unit}"),
        FontId::proportional(9.0),
        palette.text,
    );
    for (reading, alignment, x) in [
        (history.stage.first(), Align2::LEFT_TOP, plot.left()),
        (history.stage.last(), Align2::RIGHT_TOP, plot.right()),
    ] {
        let Some(reading) = reading else {
            continue;
        };
        painter.text(
            Pos2::new(x, plot.bottom() + 1.0),
            alignment,
            stamp(reading.at_ms),
            FontId::proportional(10.0),
            palette.text,
        );
    }
    if let Some(range) = history.stage_range() {
        ui.label(
            RichText::new(format!(
                "Highest {:.2} {unit} · lowest {:.2} {unit} · range {range:.2} {unit} over {} readings",
                highest.value,
                history.lowest_stage().map_or(0.0, |reading| reading.value),
                history.stage.len()
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    response.on_hover_text(
        "Stage over the last seven days, as the gauge published it. The line joins the readings the service returned; it does not interpolate gaps it did not send.",
    );
}

/// A reading's age.
fn age_label(now_ms: u64, at_ms: u64) -> String {
    let Some(minutes) = now_ms.checked_sub(at_ms).map(|elapsed| elapsed / 60_000) else {
        return format!("stamped {} (after now)", stamp(at_ms));
    };
    if minutes < 60 {
        format!("{minutes} min old")
    } else if minutes < 1_440 {
        format!("{} h {} min old", minutes / 60, minutes % 60)
    } else {
        format!("{} d old", minutes / 1_440)
    }
}

/// A timestamp as a UTC calendar label.
fn stamp(time_ms: u64) -> String {
    let Ok(days) = i64::try_from(time_ms / time::MS_PER_DAY) else {
        return format!("<{time_ms} ms is past the calendar>");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (time_ms / 3_600_000) % 24;
    format!("{month:02}-{day:02} {hour:02}Z")
}

/// Distance between two points, from the projection's own geodesic.
fn distance_km(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    match crate::projection::inverse(from_lat, from_lon, to_lat, to_lon) {
        Ok((distance, _)) => distance,
        Err(_) => f64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_age_is_written_in_the_unit_that_reads_best() {
        let hour = 3_600_000;
        assert_eq!(age_label(10 * hour, 10 * hour - 20 * 60_000), "20 min old");
        assert_eq!(age_label(10 * hour, 10 * hour - 3 * hour), "3 h 0 min old");
        assert_eq!(age_label(100 * hour, 100 * hour - 50 * hour), "2 d old");
    }

    #[test]
    fn a_stamp_is_written_as_a_utc_day() {
        assert_eq!(
            stamp(20_758 * time::MS_PER_DAY + 7 * 3_600_000),
            "11-01 07Z"
        );
    }

    #[test]
    fn a_marker_colour_is_not_the_map_background() {
        assert_ne!(WATER.r(), 8);
    }

    #[test]
    fn a_new_layer_is_off_and_holds_nothing() {
        let mut layer = GaugeLayer::new();
        assert!(!layer.shown);
        assert!(layer.gauges().is_none());
        assert!(layer.take_focus().is_none());
    }
}
