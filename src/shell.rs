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
//! Window chrome: configuration, theme, the scene switch, notices and the
//! header. Everything below the header is owned by a panel.

use eframe::egui::{self, Align, Layout, RichText};

use crate::canvas::color_table_selector;
use crate::config::{self, Config};
use crate::place::Place;
use crate::stations::SiteId;
use crate::theme::{self, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SceneMode {
    Radar,
    Map,
}

#[derive(Debug, Default)]
pub(crate) struct ShellActions {
    pub(crate) switched_to_map: bool,
    pub(crate) apply_color_table: bool,
}

pub(crate) struct Shell {
    config: Config,
    applied_theme: Option<Theme>,
    config_writable: bool,
    notice: Option<String>,
    scene_mode: SceneMode,
}

impl Shell {
    pub(crate) const fn new(config: Config, config_writable: bool, notice: Option<String>) -> Self {
        Self {
            config,
            applied_theme: None,
            config_writable,
            notice,
            scene_mode: SceneMode::Radar,
        }
    }

    pub(crate) const fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) const fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    pub(crate) const fn scene_mode(&self) -> SceneMode {
        self.scene_mode
    }

    pub(crate) const fn set_scene(&mut self, mode: SceneMode) {
        self.scene_mode = mode;
    }

    pub(crate) const fn notice(&self) -> Option<&String> {
        self.notice.as_ref()
    }

    pub(crate) fn set_notice(&mut self, message: Option<String>) {
        self.notice = message;
    }

    pub(crate) const fn notice_mut(&mut self) -> &mut Option<String> {
        &mut self.notice
    }

    /// Configuration and the notice together, for panels that read settings
    /// and report what happened in one call.
    pub(crate) const fn config_and_notice_mut(&mut self) -> (&Config, &mut Option<String>) {
        (&self.config, &mut self.notice)
    }

    pub(crate) const fn config_mut_and_notice(&mut self) -> (&mut Config, &mut Option<String>) {
        (&mut self.config, &mut self.notice)
    }

    pub(crate) fn save_config(&mut self) {
        if !self.config_writable {
            return;
        }
        if let Err(error) = config::save_default(&self.config) {
            self.notice = Some(format!("Could not save your location: {error}"));
        }
    }

    /// Applies the configured theme once per change.
    pub(crate) fn apply_theme(&mut self, context: &egui::Context) {
        if self.applied_theme != Some(self.config.theme) {
            context.set_visuals(self.config.theme.visuals());
            self.applied_theme = Some(self.config.theme);
        }
    }

    pub(crate) fn header(
        &mut self,
        ui: &mut egui::Ui,
        place: Option<&Place>,
        site: Option<&SiteId>,
        station_name: Option<&str>,
    ) -> ShellActions {
        let mut actions = ShellActions::default();
        let mut units = self.config.units;
        let mut theme = self.config.theme;
        let mut color_table = self.config.color_table;
        egui::Panel::top("header")
            .exact_size(56.0)
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = self.config.theme.visuals();
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("oxrad").size(theme::TYPE_DISPLAY).strong());
                    ui.label(
                        RichText::new("weather radar")
                            .size(theme::TYPE_LABEL)
                            .weak(),
                    );
                    ui.add_space(theme::SPACE_L);
                    if ui
                        .selectable_label(self.scene_mode == SceneMode::Radar, "Radar")
                        .clicked()
                    {
                        self.scene_mode = SceneMode::Radar;
                    }
                    if ui
                        .selectable_label(self.scene_mode == SceneMode::Map, "Map")
                        .clicked()
                    {
                        self.scene_mode = SceneMode::Map;
                        actions.switched_to_map = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.selectable_value(&mut theme, Theme::Light, Theme::Light.label());
                        ui.selectable_value(&mut theme, Theme::Dark, Theme::Dark.label());
                        ui.separator();
                        color_table = color_table_selector(ui, color_table);
                        ui.separator();
                        ui.selectable_value(
                            &mut units,
                            crate::units::UnitSystem::Imperial,
                            crate::units::UnitSystem::Imperial.label(),
                        );
                        ui.selectable_value(
                            &mut units,
                            crate::units::UnitSystem::Metric,
                            crate::units::UnitSystem::Metric.label(),
                        );
                        ui.separator();
                        if let Some(place) = place {
                            ui.label(
                                RichText::new(format!("Weather for {}", place.label()))
                                    .size(theme::TYPE_BODY),
                            );
                        } else {
                            ui.label(
                                RichText::new("No town selected")
                                    .size(theme::TYPE_BODY)
                                    .weak(),
                            );
                        }
                        ui.separator();
                        match site {
                            Some(site) => {
                                let label = station_name.map_or_else(
                                    || format!("Radar {site}"),
                                    |name| format!("Radar {site} · {name}"),
                                );
                                ui.label(RichText::new(label).size(theme::TYPE_BODY));
                            }
                            None => {
                                ui.label(
                                    RichText::new("No site selected")
                                        .size(theme::TYPE_BODY)
                                        .weak(),
                                );
                            }
                        }
                    });
                });
            });
        if units != self.config.units
            || theme != self.config.theme
            || color_table != self.config.color_table
        {
            actions.apply_color_table = color_table != self.config.color_table;
            self.config.units = units;
            self.config.theme = theme;
            self.config.color_table = color_table;
            self.save_config();
        }
        actions
    }
}
