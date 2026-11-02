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
//! The volcanic-activity layer: which volcanoes are raised, how high, and
//! where they are on the map.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Painter, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::map::{MapTransform, MapView};
use crate::theme::{self, Palette};
use crate::time;
use crate::volcano::{self, Colour, Report, Volcano, VolcanoState, VolcanoView};

/// How long a report stands before it is refreshed. The observatories publish
/// a daily update, so there is nothing to gain from asking more often.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
const LIST_LIMIT: usize = 10;

/// The colour code, drawn the way aviation charts draw it.
pub const fn colour(colour: Colour) -> Color32 {
    match colour {
        Colour::Green => Color32::from_rgb(84, 196, 108),
        Colour::Yellow => Color32::from_rgb(240, 208, 64),
        Colour::Orange => Color32::from_rgb(248, 148, 44),
        Colour::Red => Color32::from_rgb(248, 72, 60),
    }
}

/// The volcanic-activity layer.
pub struct VolcanoLayer {
    view: VolcanoView,
    shown: bool,
    asked_at: Option<Instant>,
    focus: Option<(f64, f64)>,
    selected: Option<usize>,
}

impl VolcanoLayer {
    pub fn new() -> Self {
        Self {
            view: VolcanoView::new(),
            shown: false,
            asked_at: None,
            focus: None,
            selected: None,
        }
    }

    /// Takes in the worker's answers and asks again when the report is old.
    pub fn update(&mut self, now: Instant) {
        let retry_due = self.view.update(now);
        if !self.shown {
            return;
        }
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        let retry = retry_due && self.view.may_retry();
        // A report already in hand is not asked for again until it is old: the
        // observatories publish a daily update, and a request per frame would
        // spend the reader's connection on nothing.
        let first = matches!(self.view.state(), VolcanoState::Idle);
        if first || stale || retry {
            self.asked_at = Some(now);
            self.view.start(self.view.attempts());
        }
    }

    /// The report the layer holds, for anything else that reports it.
    pub const fn report(&self) -> Option<&Report> {
        self.view.report()
    }

    /// A volcano the reader clicked, so the shell can centre the map on it.
    pub const fn take_focus(&mut self) -> Option<(f64, f64)> {
        self.focus.take()
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(
            RichText::new("Volcanic activity")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · no key", volcano::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Volcanoes: on"
        } else {
            "Volcanoes: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Which volcanoes an observatory has raised, at what alert level and aviation colour code, and where they are.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if self.shown {
                self.asked_at = Some(Instant::now());
                self.view.start(0);
            }
        }
        if !self.shown {
            ui.label(
                RichText::new(
                    "Turn the layer on to read the volcanic activity the observatories are reporting.",
                )
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(report) = self.view.report().cloned() else {
            return;
        };
        legend(ui, &report, palette);
        self.list(ui, &report, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            VolcanoState::Idle | VolcanoState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the volcano notices…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            VolcanoState::Loading { last: Some(report) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(report)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            VolcanoState::Ready { report } => {
                ui.label(
                    RichText::new(describe(report))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            VolcanoState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Volcanic activity failed: {error}"),
                );
                if let Some(report) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", describe(report)))
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

    fn list(&mut self, ui: &mut egui::Ui, report: &Report, palette: &Palette) {
        if report.volcanoes.is_empty() {
            ui.label(
                RichText::new(
                    "No observatory has raised a volcano: every monitored volcano is at its normal background level.",
                )
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            return;
        }
        let Ok(now_ms) = time::now_unix_ms() else {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "The system clock cannot be read, so the notice ages cannot be shown.",
            );
            return;
        };
        for (index, volcano) in report.volcanoes.iter().take(LIST_LIMIT).enumerate() {
            row(
                ui,
                index,
                volcano,
                now_ms,
                palette,
                &mut self.selected,
                &mut self.focus,
            );
        }
        if report.volcanoes.len() > LIST_LIMIT {
            ui.label(
                RichText::new(format!(
                    "{} more raised volcanoes are in the report but not listed.",
                    report.volcanoes.len() - LIST_LIMIT
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        if report.not_followed > 0 {
            ui.label(
                RichText::new(format!(
                    "{} raised volcanoes were not followed up, because the list is longer than the {} this layer reads positions for.",
                    report.not_followed,
                    volcano::MAX_VOLCANOES
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
    }

    /// Draws a marker at every raised volcano whose position is published.
    pub fn draw(&self, painter: &Painter, transform: &MapTransform, rect: Rect, palette: &Palette) {
        let Some(report) = self.view.report() else {
            return;
        };
        if !self.shown {
            return;
        }
        for volcano in report.located() {
            let Some((latitude, longitude)) = volcano.position() else {
                continue;
            };
            let Some(point) = transform.screen(latitude, longitude) else {
                continue;
            };
            if !rect.contains(point) {
                continue;
            }
            let Some(code) = volcano.notice.colour else {
                continue;
            };
            let centre = Pos2::new(point.x, point.y);
            painter.circle_filled(centre, 6.0, colour(code));
            painter.circle_stroke(
                centre,
                10.0,
                Stroke::new(1.5_f32, colour(code).gamma_multiply(0.75)),
            );
            painter.text(
                Pos2::new(centre.x + 10.0, centre.y),
                Align2::LEFT_CENTER,
                &volcano.notice.volcano,
                FontId::proportional(11.0),
                palette.canvas_text,
            );
        }
    }
}

/// The colours in the report, highest first.
fn legend(ui: &mut egui::Ui, report: &Report, palette: &Palette) {
    let rows = report.by_colour();
    if rows.is_empty() {
        ui.label(
            RichText::new("No colour code was published for the raised volcanoes.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (code, count) in rows {
            ui.colored_label(colour(code), format!("{count} × {}", code.label()))
                .on_hover_text(code.meaning());
        }
    });
    if let Some(highest) = report.highest() {
        ui.label(
            RichText::new(format!(
                "Highest code {} · {}",
                highest.label(),
                highest.meaning()
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    if let Some(newest) = report.newest().and_then(|volcano| volcano.notice.sent_ms) {
        ui.label(
            RichText::new(format!("Newest notice {}", stamp(newest)))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    let missing = report
        .volcanoes
        .iter()
        .filter(|volcano| volcano.position().is_none())
        .count();
    if missing > 0 {
        ui.label(
            RichText::new(format!(
                "{missing} raised volcanoes have no published position, so they are listed without a map marker."
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    let _ = palette;
}

/// One raised volcano, clickable to centre the map.
fn row(
    ui: &mut egui::Ui,
    index: usize,
    volcano: &Volcano,
    now_ms: u64,
    palette: &Palette,
    selected: &mut Option<usize>,
    focus: &mut Option<(f64, f64)>,
) {
    let notice = &volcano.notice;
    let code = notice.colour;
    let age = notice.sent_ms.map_or_else(
        || String::from("age not stated"),
        |sent| age_label(now_ms, sent),
    );
    let position = volcano.position().map_or_else(
        || String::from("position not published"),
        |(latitude, longitude)| format!("{latitude:.2}, {longitude:.2}"),
    );
    let is_selected = *selected == Some(index);
    let mut response = ui.add(
        egui::Label::new(
            RichText::new(format!(
                "{} · {} · {} · {age}",
                notice.volcano,
                notice.alert_level.to_lowercase(),
                code.map_or_else(
                    || notice.colour_text.to_lowercase(),
                    |code| String::from(code.label())
                )
            ))
            .size(theme::TYPE_SMALL)
            .color(code.map_or(palette.text, colour)),
        )
        .sense(egui::Sense::click()),
    );
    if let Some(code) = code {
        response = response.on_hover_text(format!(
            "{} · {} · {position} · {}\n{}\nClick to centre the map.",
            notice.observatory,
            notice.alert_level.to_lowercase(),
            code.meaning(),
            volcano
                .place
                .summary
                .as_deref()
                .unwrap_or("No synopsis was published with the notice.")
        ));
    }
    if response.clicked() {
        *selected = Some(index);
        if let Some(position) = volcano.position() {
            *focus = Some(position);
        }
    }
    if is_selected && let Some(summary) = &volcano.place.summary {
        ui.label(
            RichText::new(summary.clone())
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
}

/// The report in one line.
fn describe(report: &Report) -> String {
    let located = report.located().count();
    format!(
        "{} raised · {located} located · {} observator{}",
        report.volcanoes.len(),
        observatory_count(report),
        if observatory_count(report) == 1 {
            "y"
        } else {
            "ies"
        }
    )
}

/// How many distinct observatories are named in the report.
fn observatory_count(report: &Report) -> usize {
    let mut names: Vec<&str> = report
        .volcanoes
        .iter()
        .map(|volcano| volcano.notice.observatory.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.len()
}

/// A notice's age, in hours and minutes.
fn age_label(now_ms: u64, sent_ms: u64) -> String {
    let Some(minutes) = now_ms.checked_sub(sent_ms).map(|elapsed| elapsed / 60_000) else {
        return format!("sent {} (after now)", stamp(sent_ms));
    };
    match minutes {
        0..=59 => format!("{minutes} min old"),
        60..=1_439 => format!("{} h {} min old", minutes / 60, minutes % 60),
        _ => format!("{} d old", minutes / 1_440),
    }
}

/// A timestamp as a UTC calendar label.
fn stamp(time_ms: u64) -> String {
    let Ok(days) = i64::try_from(time_ms / time::MS_PER_DAY) else {
        return format!("<{time_ms} ms is past the calendar>");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (time_ms / 3_600_000) % 24;
    let minute = (time_ms / 60_000) % 60;
    format!("{month:02}-{day:02} {hour:02}:{minute:02}Z")
}

/// Where the map should point when a volcano is clicked.
pub fn view_for(latitude: f64, longitude: f64) -> Result<MapView, crate::map::MapError> {
    MapView::focus(latitude, longitude, 600.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_age_is_written_in_the_unit_that_reads_best() {
        let hour = 3_600_000;
        assert_eq!(
            age_label(100 * hour, 100 * hour - 12 * 60_000),
            "12 min old"
        );
        assert_eq!(
            age_label(100 * hour, 100 * hour - 5 * hour),
            "5 h 0 min old"
        );
        assert_eq!(age_label(100 * hour, 100 * hour - 72 * hour), "3 d old");
    }

    #[test]
    fn a_notice_after_now_is_named_rather_than_shown_as_negative() {
        let label = age_label(10, 20);
        assert!(label.contains("after now"), "{label}");
    }

    #[test]
    fn a_timestamp_is_written_as_a_utc_day() {
        assert_eq!(
            stamp(20_758 * time::MS_PER_DAY + 20 * 3_600_000 + 5 * 60_000),
            "11-01 20:05Z"
        );
    }

    #[test]
    fn a_volcano_opens_a_close_view() {
        let Ok(view) = view_for(52.0765, -176.1109) else {
            return;
        };
        assert!(view.span_km() <= 600.0);
    }

    #[test]
    fn the_colour_codes_are_all_distinct() {
        let colours = [
            colour(Colour::Green),
            colour(Colour::Yellow),
            colour(Colour::Orange),
            colour(Colour::Red),
        ];
        let mut seen: Vec<(u8, u8, u8)> = colours
            .iter()
            .map(|colour| (colour.r(), colour.g(), colour.b()))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), colours.len());
    }

    #[test]
    fn a_new_layer_is_off_and_holds_nothing() {
        let mut layer = VolcanoLayer::new();
        assert!(!layer.shown);
        assert!(layer.report().is_none());
        assert!(layer.take_focus().is_none());
    }
}
