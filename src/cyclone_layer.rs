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
//! The tropical-cyclone layer: the storms the global alert service is
//! carrying, wherever they are in the world.

use std::fmt::Write;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Painter, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::cyclone::{self, Alert, Cyclone, CycloneState, CycloneView, StormList};
use crate::map::MapTransform;
use crate::theme::{self, Palette};
use crate::time;

/// How long a storm list stands before it is refreshed. The service updates an
/// episode when its issuing centre posts a new advisory, which is a few times a
/// day.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
const LIST_LIMIT: usize = 8;

/// The alert colour, as the service paints it.
pub const fn colour(alert: Alert) -> Color32 {
    match alert {
        Alert::Green => Color32::from_rgb(96, 190, 112),
        Alert::Orange => Color32::from_rgb(248, 152, 48),
        Alert::Red => Color32::from_rgb(240, 64, 56),
    }
}

/// The tropical-cyclone layer.
pub struct CycloneLayer {
    view: CycloneView,
    shown: bool,
    asked_at: Option<Instant>,
    focus: Option<(f64, f64)>,
    selected: Option<u64>,
}

impl CycloneLayer {
    pub fn new() -> Self {
        Self {
            view: CycloneView::new(),
            shown: false,
            asked_at: None,
            focus: None,
            selected: None,
        }
    }

    /// Takes in the worker's answers and asks again when the list is old.
    pub fn update(&mut self, now: Instant) {
        let retry_due = self.view.update(now);
        if !self.shown {
            return;
        }
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        let retry = retry_due && self.view.may_retry();
        let first = matches!(self.view.state(), CycloneState::Idle);
        if first || stale || retry {
            let Ok(now_ms) = time::now_unix_ms() else {
                return;
            };
            self.asked_at = Some(now);
            self.view.start(now_ms, self.view.attempts());
        }
    }

    /// The list the layer holds, for anything else that reports it.
    pub const fn list(&self) -> Option<&StormList> {
        self.view.list()
    }

    /// A storm the reader clicked, so the shell can centre the map on it.
    pub const fn take_focus(&mut self) -> Option<(f64, f64)> {
        self.focus.take()
    }

    /// The rail section.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(
            RichText::new("Tropical cyclones")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!(
                "{} · worldwide, last {} days",
                cyclone::SOURCE,
                cyclone::WINDOW_DAYS
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        let label = if self.shown {
            "Cyclones: on"
        } else {
            "Cyclones: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Every tropical cyclone the global disaster alert service is carrying, with its last reported position, maximum wind, alert level and the countries it affects.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if self.shown {
                self.asked_at = Some(Instant::now());
                if let Ok(now_ms) = time::now_unix_ms() {
                    self.view.start(now_ms, 0);
                }
            }
        }
        if !self.shown {
            ui.label(
                RichText::new(
                    "Turn the layer on to read the tropical cyclones the alert service is carrying.",
                )
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(list) = self.view.list().cloned() else {
            return;
        };
        summary(ui, &list, palette);
        self.rows(ui, &list, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            CycloneState::Idle | CycloneState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the tropical cyclones…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            CycloneState::Loading { last: Some(list) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(list)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            CycloneState::Ready { list } => {
                ui.label(RichText::new(describe(list)).size(theme::TYPE_SMALL).weak());
            }
            CycloneState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Tropical cyclones failed: {error}"),
                );
                if let Some(list) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", describe(list)))
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

    fn rows(&mut self, ui: &mut egui::Ui, list: &StormList, palette: &Palette) {
        if list.cyclones.is_empty() {
            ui.label(
                RichText::new(format!(
                    "The service is carrying no tropical cyclone in the last {} days.",
                    cyclone::WINDOW_DAYS
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            return;
        }
        for storm in list.cyclones.iter().take(LIST_LIMIT) {
            let is_chosen = self.selected == Some(storm.event_id);
            let mut text = if is_chosen {
                format!("▶ {}", storm.name)
            } else {
                String::from(&storm.name)
            };
            if storm.current {
                text.push_str(" · in progress");
            }
            if let Some(alert) = storm.alert {
                let _ = write!(text, " · {} alert", alert.label());
            }
            if let Some(wind) = storm.wind {
                let _ = write!(text, " · {wind:.0} km/h");
            }
            if let Some(days) = storm.days() {
                let _ = write!(text, " · {days} d");
            }
            let response = ui
                .add(
                    egui::Label::new(
                        RichText::new(text)
                            .size(theme::TYPE_SMALL)
                            .color(storm.alert.map_or(palette.text, colour)),
                    )
                    .sense(egui::Sense::click()),
                )
                .on_hover_text(format!(
                    "{}\n{}\n{}\n{}\nClick to centre the map.",
                    storm.title,
                    storm.alert.map_or_else(
                        || format!("alert level not stated ({})", storm.alert_text),
                        |alert| String::from(alert.meaning())
                    ),
                    storm.wind_text,
                    storm.country.as_deref().unwrap_or("country not stated")
                ));
            if response.clicked() {
                self.selected = Some(storm.event_id);
                if let Some(position) = storm.position() {
                    self.focus = Some(position);
                }
            }
        }
        if list.cyclones.len() > LIST_LIMIT {
            ui.label(
                RichText::new(format!(
                    "{} more cyclones are in the window but not listed.",
                    list.cyclones.len() - LIST_LIMIT
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        ui.label(
            RichText::new(
                "The alert level is the service's judgement of expected impact, not a hurricane category, and the wind is the maximum the issuing centre published for the episode rather than the wind at the marked position now.",
            )
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }

    /// Draws a marker at every storm whose position is known.
    pub fn draw(&self, painter: &Painter, transform: &MapTransform, rect: Rect, palette: &Palette) {
        if !self.shown {
            return;
        }
        let Some(list) = self.view.list() else {
            return;
        };
        for storm in list.located() {
            let Some((latitude, longitude)) = storm.position() else {
                continue;
            };
            let Some(point) = transform.screen(latitude, longitude) else {
                continue;
            };
            if !rect.contains(point) {
                continue;
            }
            let centre = Pos2::new(point.x, point.y);
            let stroke = storm.alert.map_or(palette.text, colour);
            painter.circle_stroke(centre, 9.0, Stroke::new(2.2_f32, stroke));
            painter.circle_filled(centre, 3.5, stroke);
            painter.text(
                Pos2::new(centre.x + 12.0, centre.y),
                Align2::LEFT_CENTER,
                &storm.name,
                FontId::proportional(11.0),
                palette.canvas_text,
            );
        }
    }
}

/// The list in one line, plus the strongest storm and the highest level.
fn summary(ui: &mut egui::Ui, list: &StormList, palette: &Palette) {
    if let Some(alert) = list.highest() {
        ui.colored_label(
            colour(alert),
            format!("Highest alert {} · {}", alert.label(), alert.meaning()),
        );
    }
    if let Some(newest) = list.newest().filter(|storm| storm.current) {
        ui.label(
            RichText::new(format!("Newest in progress {}", newest.title))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    if let Some(strongest) = list
        .strongest()
        .and_then(|storm| storm.wind.map(|wind| (storm, wind)))
    {
        ui.label(
            RichText::new(format!(
                "Strongest {}",
                describe_storm(strongest.0, strongest.1)
            ))
            .size(theme::TYPE_SMALL)
            .color(palette.text),
        );
    }
}

fn describe_storm(storm: &Cyclone, wind: f64) -> String {
    format!("{} at {wind:.0} km/h", storm.name)
}

/// The list in one line.
fn describe(list: &StormList) -> String {
    let located = list.located().count();
    let active = list.active();
    let mut line = format!(
        "{} cyclones · {active} in progress · {located} with a position",
        list.cyclones.len()
    );
    if list.rejected > 0 {
        let _ = write!(line, " · {} unreadable", list.rejected);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_alert_levels_are_all_distinct() {
        let colours = [
            colour(Alert::Green),
            colour(Alert::Orange),
            colour(Alert::Red),
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
        let mut layer = CycloneLayer::new();
        assert!(!layer.shown);
        assert!(layer.list().is_none());
        assert!(layer.take_focus().is_none());
    }
}
