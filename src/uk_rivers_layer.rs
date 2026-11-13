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
//! The England river-level rail section.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::canvas::{action_button, provenance, toggle_button};
use crate::theme::{self, Palette};
use crate::uk_rivers::{self, UkReading, UkRiverState, UkRiverView};

/// How long a latest-reading answer stands before it is requested again.
const REFRESH_AFTER: Duration = Duration::from_secs(900);

/// The Environment Agency's latest river levels.
pub struct UkRiverLayer {
    view: UkRiverView,
    shown: bool,
    asked_at: Option<Instant>,
}

impl UkRiverLayer {
    pub const fn new(acquire: Acquire) -> Self {
        Self {
            view: UkRiverView::new(acquire),
            shown: false,
            asked_at: None,
        }
    }

    /// Polls the worker and refreshes while the layer is on.
    pub fn update(&mut self, now: Instant) {
        self.view.update();
        if !self.shown {
            return;
        }
        let stale = self
            .asked_at
            .is_none_or(|at| now.duration_since(at) >= REFRESH_AFTER);
        if stale && !self.view.started() {
            self.view.start();
            self.asked_at = Some(now);
        }
    }

    /// The rail section.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(
            RichText::new("UK river levels")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · England, no key", uk_rivers::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "UK rivers: on"
        } else {
            "UK rivers: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "The latest level at Environment Agency stations, with the station reference, qualifier and datum unit exactly as published.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if self.shown {
                self.view.start();
                self.asked_at = Some(Instant::now());
            }
        }
        if !self.shown {
            ui.label(
                RichText::new("Turn the layer on to read the latest Environment Agency levels.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui, palette);
        let Some(served) = self.view.readings() else {
            return;
        };
        ui.label(
            RichText::new(uk_rivers::describe(&served.value))
                .size(theme::TYPE_SMALL)
                .strong()
                .color(palette.text),
        );
        provenance(ui, served);
        egui::ScrollArea::vertical()
            .max_height(230.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for reading in &served.value.readings {
                    reading_row(ui, reading, palette);
                }
            });
    }

    fn status(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        match self.view.state() {
            UkRiverState::Idle | UkRiverState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the Environment Agency latest levels…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            UkRiverState::Loading { last: Some(served) } => {
                ui.label(
                    RichText::new(format!(
                        "Refreshing · still showing {}",
                        uk_rivers::describe(&served.value)
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
            UkRiverState::Ready { .. } => {}

            UkRiverState::Failed { error, last } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("UK river levels failed: {error}"),
                );
                if let Some(served) = last {
                    ui.label(
                        RichText::new(format!(
                            "Still showing {}",
                            uk_rivers::describe(&served.value)
                        ))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                    );
                }
            }
        }
        if ui
            .add_sized([110.0, 28.0], action_button(palette, "Refresh levels"))
            .clicked()
        {
            self.asked_at = Some(Instant::now());
            self.view.start();
        }
    }
}

fn reading_row(ui: &mut egui::Ui, reading: &UkReading, palette: &Palette) {
    ui.label(
        RichText::new(format!(
            "{} · {:.3} {}",
            reading.station, reading.value, reading.unit
        ))
        .size(theme::TYPE_SMALL)
        .color(palette.text),
    );
    let qualifier = reading
        .qualifier
        .as_deref()
        .map_or("no qualifier", |value| value);
    ui.label(
        RichText::new(format!(
            "{} · {} · station {}",
            uk_rivers::time_label(reading),
            qualifier,
            reading.station_reference
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    )
    .on_hover_text(&reading.measure);
}
