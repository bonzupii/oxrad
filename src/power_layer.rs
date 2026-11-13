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
//! The NASA POWER monthly-climatology rail section.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::canvas::{provenance, toggle_button};
use crate::map::MapView;
use crate::power::{self, PowerClimatology, PowerMonth, PowerState, PowerView};
use crate::projection;
use crate::theme::{self, Palette};

/// How far the map may move before the climatology is requested again.
const MOVE_KM: f64 = 50.0;
/// The monthly climatology changes once a year, so one request a day is enough
/// to notice a provider revision while the engine keeps its thirty-day cache.
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 3600);

/// NASA POWER's monthly normals at the map centre.
pub struct PowerLayer {
    view: PowerView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl PowerLayer {
    pub const fn new(acquire: Acquire) -> Self {
        Self {
            view: PowerView::new(acquire),
            shown: false,
            asked: None,
            asked_at: None,
        }
    }

    /// Follows the map centre while the layer is on.
    pub fn update(&mut self, now: Instant, view: &MapView) {
        self.view.update();
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
        if (moved || stale) && !self.view.started() {
            self.asked = Some((lat, lon));
            self.asked_at = Some(now);
            self.view.start(lat, lon);
        }
    }

    /// The rail section.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(
            RichText::new("NASA POWER climatology")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · global, no key", power::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "POWER normals: on"
        } else {
            "POWER normals: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "The 2001 to 2025 monthly mean temperature and precipitation at the map centre, from MERRA-2.",
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
                RichText::new("Turn the layer on to read the monthly normals from NASA POWER.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(served) = self.view.record() else {
            return;
        };
        let record = &served.value;
        ui.label(
            RichText::new(power::describe(record))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        provenance(ui, &served.provenance, served.cache);
        summary(ui, record, palette);
        table(ui, record, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            PowerState::Idle | PowerState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the MERRA-2 monthly climatology…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            PowerState::Loading { last: Some(served) } => {
                ui.label(
                    RichText::new(format!(
                        "Refreshing · still showing {}",
                        power::describe(&served.value)
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
            PowerState::Ready { served } => {
                ui.label(
                    RichText::new(format!(
                        "Source period {} · {}",
                        served.value.period, served.value.sources
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
            PowerState::Failed { error, last } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("NASA POWER failed: {error}"),
                );
                if let Some(served) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", power::describe(&served.value)))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
        }
    }
}

fn distance_km(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    match projection::inverse(from_lat, from_lon, to_lat, to_lon) {
        Ok((distance, _)) => distance,
        Err(_) => f64::MAX,
    }
}

fn summary(ui: &mut egui::Ui, record: &PowerClimatology, palette: &Palette) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Annual").size(theme::TYPE_SMALL).weak());
        ui.label(
            RichText::new(record.annual_temperature_c.map_or_else(
                || String::from("temperature not stated"),
                |value| format!("{value:.1} {}", record.temperature_unit),
            ))
            .size(theme::TYPE_SMALL)
            .strong()
            .color(palette.text),
        );
        ui.label(
            RichText::new(record.annual_precipitation_mm_per_day.map_or_else(
                || String::from("precipitation not stated"),
                |value| format!("{value:.2} {}", record.precipitation_unit),
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    });
}

fn table(ui: &mut egui::Ui, record: &PowerClimatology, palette: &Palette) {
    egui::Grid::new("nasa_power_months")
        .num_columns(3)
        .spacing([10.0, 2.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Month").size(theme::TYPE_SMALL).weak());
            ui.label(
                RichText::new(format!("T2M {}", record.temperature_unit))
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            ui.label(
                RichText::new(format!("Precip. {}", record.precipitation_unit))
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            ui.end_row();
            for month in &record.months {
                month_row(ui, month, record, palette);
            }
        });
}

fn month_row(ui: &mut egui::Ui, month: &PowerMonth, record: &PowerClimatology, palette: &Palette) {
    ui.label(
        RichText::new(power::month_name(month.month))
            .size(theme::TYPE_SMALL)
            .weak(),
    );
    ui.label(
        RichText::new(
            month
                .temperature_c
                .map_or_else(|| String::from("missing"), |value| format!("{value:.1}")),
        )
        .size(theme::TYPE_SMALL)
        .color(palette.text),
    );
    ui.label(
        RichText::new(
            month
                .precipitation_mm_per_day
                .map_or_else(|| String::from("missing"), |value| format!("{value:.2}")),
        )
        .size(theme::TYPE_SMALL)
        .color(palette.text),
    );
    ui.end_row();
    if month.month == 12 {
        ui.label(
            RichText::new(format!("Annual mean, {}", record.period))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        ui.label(
            RichText::new(
                record
                    .annual_temperature_c
                    .map_or_else(|| String::from("missing"), |value| format!("{value:.1}")),
            )
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        ui.label(
            RichText::new(
                record
                    .annual_precipitation_mm_per_day
                    .map_or_else(|| String::from("missing"), |value| format!("{value:.2}")),
            )
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        ui.end_row();
    }
}
