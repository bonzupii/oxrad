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
//! The forecast panel: the forecast for the searched town, its retry state
//! and the point-versus-radar explanation.

use std::time::Instant;

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::config::Config;
use crate::forecast::{Forecast, ForecastPeriod, ForecastState, ForecastView};
use crate::place::Place;
use crate::provider::timestamp_label;
use crate::stations::SiteId;
use crate::theme;

const FORECAST_WIDTH: f32 = 340.0;

pub(crate) struct ForecastPanel {
    forecast: ForecastView,
}

impl ForecastPanel {
    pub(crate) fn new(acquire: &Acquire) -> Self {
        Self {
            forecast: ForecastView::new(acquire.clone()),
        }
    }

    pub(crate) fn update(&mut self, now: Instant) {
        self.forecast.update(now);
    }

    pub(crate) fn start(&mut self, place: &Place) {
        self.forecast.start(place, 0);
    }

    pub(crate) const fn forecast(&self) -> Option<&Forecast> {
        self.forecast.forecast()
    }

    pub(crate) const fn is_busy(&self) -> bool {
        matches!(self.forecast.state(), ForecastState::Loading { .. })
    }

    /// Draws the forecast panel. Returns whether the reader asked to retry.
    pub(crate) fn panel(
        &self,
        ui: &mut egui::Ui,
        place: Option<&Place>,
        site: Option<&SiteId>,
        config: &Config,
    ) -> bool {
        let mut retry = false;
        egui::Panel::right("forecast")
            .default_size(FORECAST_WIDTH)
            .min_size(300.0)
            .max_size(460.0)
            .resizable(true)
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = config.theme.visuals();
                retry = self.footer(ui, place, site);
                ui.add_space(theme::SPACE_M);
                ui.label(RichText::new("Forecast").size(theme::TYPE_TITLE).strong());
                ui.add_space(theme::SPACE_XS);
                match self.forecast.state() {
                    ForecastState::Empty => {
                        ui.label(RichText::new("No town selected").size(14.0).strong());
                        ui.label(
                            RichText::new(
                                "Search for a town on the left to see its forecast here.",
                            )
                            .size(theme::TYPE_SMALL)
                            .weak(),
                        );
                    }
                    ForecastState::Loading { place, .. } => {
                        ui.label(RichText::new(place.label()).size(14.0).strong());
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new("Loading the forecast…")
                                    .size(theme::TYPE_LABEL)
                                    .weak(),
                            );
                        });
                    }
                    ForecastState::Failed {
                        place,
                        error,
                        last,
                        retry_at,
                        ..
                    } => {
                        ui.label(RichText::new(place.label()).size(14.0).strong());
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                        if let Some(due) = retry_at {
                            let seconds = due
                                .saturating_duration_since(Instant::now())
                                .as_secs()
                                .saturating_add(1);
                            ui.label(
                                RichText::new(format!("Automatic retry in {seconds} s."))
                                    .size(theme::TYPE_SMALL)
                                    .color(ui.visuals().warn_fg_color),
                            );
                            retry = ui.button("Retry now").clicked();
                        } else {
                            retry = ui.button("Retry forecast").clicked();
                        }
                        if let Some(forecast) = last {
                            ui.add_space(theme::SPACE_S);
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Showing the last forecast received.",
                            );
                            ui.separator();
                            forecast_body(ui, forecast, true, config.units);
                        }
                    }
                    ForecastState::Ready { forecast } => {
                        forecast_body(ui, forecast, false, config.units);
                    }
                }
                ui.add_space(theme::SPACE_S);
            });
        retry
    }

    fn footer(&self, ui: &mut egui::Ui, place: Option<&Place>, site: Option<&SiteId>) -> bool {
        let mut retry = false;
        if let Some(place) = place {
            egui::Panel::bottom("forecast_footer").show_inside(ui, |ui| {
                ui.separator();
                if matches!(self.forecast.state(), ForecastState::Ready { .. }) {
                    retry = ui.button("Refresh forecast").clicked();
                    ui.add_space(theme::SPACE_XS);
                }
                let note = site.map_or_else(
                    || {
                        format!(
                            "Forecast point {} is the town you searched. No radar station is selected.",
                            place.coordinates()
                        )
                    },
                    |site| {
                        format!(
                            "Forecast point {} is the town you searched. Radar comes from {site}. They are different places.",
                            place.coordinates()
                        )
                    },
                );
                ui.label(RichText::new(note).size(theme::TYPE_SMALL).weak());
                ui.label(
                    RichText::new(
                        "Forecast times are local to the forecast point; radar times are UTC.",
                    )
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
                ui.add_space(theme::SPACE_XS);
            });
        }
        retry
    }
}

fn forecast_body(
    ui: &mut egui::Ui,
    forecast: &Forecast,
    stale: bool,
    units: crate::units::UnitSystem,
) {
    ui.label(RichText::new(forecast.place.label()).size(15.0).strong());
    ui.label(
        RichText::new(format!("Forecast for {}", forecast.place.coordinates()))
            .size(theme::TYPE_SMALL)
            .weak(),
    );
    if stale {
        ui.colored_label(ui.visuals().warn_fg_color, "Stale forecast");
    }
    ui.add_space(theme::SPACE_XS);
    ui.label(
        RichText::new(format!(
            "{} · {} ({}) · {}",
            forecast.provider,
            forecast.timezone,
            forecast.timezone_abbreviation,
            timestamp_label(&forecast.current.observed)
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.separator();
    ui.label(RichText::new("Now").size(theme::TYPE_TITLE).strong());
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(
                crate::units::celsius_from(forecast.current.temperature, &forecast.current.unit)
                    .map_or_else(
                        || {
                            format!(
                                "{:.0} {}",
                                forecast.current.temperature, forecast.current.unit
                            )
                        },
                        |celsius| crate::units::temperature_whole(celsius, units),
                    ),
            )
            .size(20.0)
            .strong(),
        );
        ui.label(RichText::new(&forecast.current.short).size(theme::TYPE_LABEL));
    });
    let apparent = crate::units::celsius_from(forecast.current.apparent, &forecast.current.unit)
        .map_or_else(
            || format!("{:.0} {}", forecast.current.apparent, forecast.current.unit),
            |celsius| crate::units::temperature_whole(celsius, units),
        );
    ui.label(
        RichText::new(format!(
            "Feels {apparent} · humidity {:.0}% · {} · precipitation {}",
            forecast.current.humidity,
            crate::forecast::wind(
                forecast.current.wind_direction_deg,
                forecast.current.wind_speed_kmh,
                units,
            ),
            crate::units::precipitation(forecast.current.precipitation, units)
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.add_space(6.0);
    ui.separator();
    ui.label(RichText::new("Hourly").size(theme::TYPE_TITLE).strong());
    let list_height = (ui.available_height() - 8.0).max(80.0);
    egui::ScrollArea::vertical()
        .max_height(list_height)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            for period in &forecast.hourly {
                forecast_period(ui, period, units);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Daily").size(theme::TYPE_TITLE).strong());
            for period in &forecast.daily {
                forecast_period(ui, period, units);
            }
        });
}

fn forecast_period(ui: &mut egui::Ui, period: &ForecastPeriod, units: crate::units::UnitSystem) {
    ui.label(RichText::new(&period.name).size(theme::TYPE_LABEL).strong());
    ui.horizontal_wrapped(|ui| {
        let high = crate::units::celsius_from(period.temperature, &period.unit);
        let low = period
            .minimum
            .and_then(|minimum| crate::units::celsius_from(minimum, &period.unit));
        let temperature = match (high, low) {
            (Some(high), Some(low)) => format!(
                "{:.0}/{:.0} {}",
                units.temperature(high),
                units.temperature(low),
                units.temperature_unit()
            ),
            (Some(high), None) => crate::units::temperature(high, units),
            (None, _) => period.minimum.map_or_else(
                || format!("{:.0} {}", period.temperature, period.unit),
                |minimum| format!("{:.0}/{:.0} {}", period.temperature, minimum, period.unit),
            ),
        };
        ui.label(RichText::new(temperature).size(17.0).strong());
        ui.label(RichText::new(&period.short).size(theme::TYPE_LABEL));
    });
    let precipitation = period
        .precipitation
        .map_or_else(String::new, |chance| format!(" · {chance}% precipitation"));
    let wind = period.wind_speed_kmh.map_or_else(String::new, |speed| {
        format!(" · wind {}", crate::units::wind_speed(speed, units))
    });
    ui.label(
        RichText::new(format!(
            "{}{wind}{precipitation}",
            timestamp_label(&period.start)
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.add_space(6.0);
    ui.separator();
}
