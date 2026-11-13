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
//! Application construction and per-frame composition.
//!
//! Every panel owns its state and draws itself. This module wires them
//! together, resolves cross-panel gestures into actions and applies the
//! actions in one place.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::config::{self, Config};
use crate::engine::EngineStatus;
use crate::forecast_panel::ForecastPanel;
use crate::global_panel::GlobalPanel;
use crate::gpu::RadarGpu;
use crate::history_panel::HistoryPanel;
use crate::map::MapView;
use crate::map_panel::{MapActions, MapPanel};
use crate::place::{self, Place};
use crate::places_panel::PlacesPanel;
use crate::radar_panel::RadarPanel;
use crate::registry_panel::RegistryPanel;
use crate::shell::{SceneMode, Shell};
use crate::station_panel::StationPanel;
use crate::stations::SiteId;
use crate::status_bar;
use crate::theme;

const BUSY_REPAINT_MS: u64 = 100;
const IDLE_REPAINT_MS: u64 = 1000;
const RAIL_WIDTH: f32 = 320.0;

pub struct OxradApp {
    acquire: Acquire,
    engine_status: EngineStatus,
    shell: Shell,
    stations: StationPanel,
    places: PlacesPanel,
    forecast: ForecastPanel,
    radar: RadarPanel,
    history: HistoryPanel,
    global: GlobalPanel,
    map: MapPanel,
    registry: RegistryPanel,
}

/// Opens the site the configuration remembered, or says why it did not.
fn restore_site(app: &mut OxradApp, restored: Option<SiteId>) {
    let Some(id) = restored else {
        return;
    };
    if app.stations.find(&id).is_some() {
        app.open_site(id, false);
        app.stations.set_pending(None);
    } else {
        app.shell.set_notice(Some(format!(
            "Site {id} is not in the discovered radar source; choose a station."
        )));
    }
}

impl OxradApp {
    pub fn new(gpu: Option<RadarGpu>, initial: Option<SiteId>, resolved: Option<Config>) -> Self {
        let (config, config_writable, notice) = resolved.map_or_else(
            || {
                let mut config = Config::default();
                let mut writable = true;
                let mut notice = None::<String>;
                match config::load_default() {
                    Ok(None) => {}
                    Ok(Some(loaded)) => config = loaded,
                    Err(error) => {
                        writable = false;
                        notice = Some(format!("Configuration ignored: {error}"));
                    }
                }
                (config, writable, notice)
            },
            |config| (config, true, None::<String>),
        );
        let map_style = config.map_style;
        let (acquire, engine_status) = Acquire::new_with_status(config.providers.clone());
        let restored_site = initial.or_else(|| config.site.clone());
        let restored_place = config.place.clone();
        let stations = StationPanel::new(&acquire, restored_site.as_ref());
        let places = PlacesPanel::new(&acquire, restored_place.clone());
        let forecast = ForecastPanel::new(&acquire);
        let radar = RadarPanel::new(gpu, &config);
        let history = HistoryPanel::new(&acquire);
        let global = GlobalPanel::new(&acquire);
        let map = MapPanel::new(map_style, &acquire);
        let registry = RegistryPanel::new(acquire.clone());
        let shell = Shell::new(config, config_writable, notice);
        let mut app = Self {
            acquire,
            engine_status,
            shell,
            stations,
            places,
            forecast,
            radar,
            history,
            global,
            map,
            registry,
        };
        if let Some(place) = &restored_place {
            app.focus_place(place);
            app.forecast.start(place);
        }
        restore_site(&mut app, restored_site);
        app
    }

    fn open_site(&mut self, id: SiteId, persist: bool) {
        self.history.clear();
        self.radar.open(id.clone(), &self.acquire);
        self.stations.close_picker();
        self.shell.config_mut().site = Some(id);
        if persist {
            self.shell.save_config();
        }
        if self.shell.scene_mode() == SceneMode::Map {
            self.ensure_viewport_radars();
        }
    }

    fn choose_site(&mut self, id: SiteId) {
        self.places.clear_search();
        self.open_site(id, true);
    }

    fn clear_site(&mut self) {
        self.radar.clear();
        self.history.clear();
        self.shell.config_mut().site = None;
    }

    fn focus_place(&mut self, place: &Place) {
        match MapView::focus(place.lat, place.lon, 2_200.0) {
            Ok(view) => {
                self.map.set_view(view);
                self.map.schedule_viewport_reload();
            }
            Err(error) => self
                .shell
                .set_notice(Some(format!("Map centre unavailable: {error}"))),
        }
    }

    fn focus_map_context(&mut self) {
        let place = self
            .places
            .place()
            .map(|place| (place.lat, place.lon, 2_200.0));
        let station = self
            .radar
            .selected_station(&self.stations)
            .map(|station| (station.lat, station.lon, 950.0));
        let Some((lat, lon, span)) = place.or(station) else {
            return;
        };
        match MapView::focus(lat, lon, span) {
            Ok(view) => {
                self.map.set_view(view);
                self.map.schedule_viewport_reload();
            }
            Err(error) => self
                .shell
                .set_notice(Some(format!("Map centre unavailable: {error}"))),
        }
    }

    fn ensure_viewport_radars(&mut self) {
        let view = self.map.view();
        let (config, notice) = self.shell.config_and_notice_mut();
        self.radar
            .ensure_viewport_radars(&view, &self.stations, config, &self.acquire, notice);
    }

    fn choose_place(&mut self, place: &Place) {
        self.places.clear_search();
        self.focus_place(place);
        self.places.set_place(Some(place.clone()));
        self.shell.config_mut().place = Some(place.clone());
        self.forecast.start(place);
        if !place.in_united_states() {
            self.clear_site();
            self.shell.set_scene(SceneMode::Map);
            self.ensure_viewport_radars();
            self.global.request_now();
            self.shell.set_notice(Some(format!(
                "Showing the global RainViewer composite for {}. No Level II site is selected outside the United States.",
                place.label()
            )));
            self.shell.save_config();
            return;
        }
        let nearest = match self.stations.result() {
            Ok(list) => place::nearest_station(list, place.lat, place.lon),
            Err(error) => {
                let message = format!("Radar station catalog unavailable: {error}");
                self.clear_site();
                self.shell.set_notice(Some(message));
                self.shell.save_config();
                return;
            }
        };
        match nearest {
            Ok(Some(nearest)) => {
                self.open_site(nearest.station.id, true);
                self.shell.set_notice(None);
            }
            Ok(None) => {
                self.clear_site();
                self.shell.set_notice(Some(format!(
                    "No radar station is available for {}.",
                    place.label()
                )));
                self.shell.save_config();
            }
            Err(error) => {
                self.clear_site();
                self.shell
                    .set_notice(Some(format!("Nearest radar station unavailable: {error}")));
                self.shell.save_config();
            }
        }
    }

    fn apply_color_table(&mut self) {
        let table = self.shell.config().color_table;
        self.radar.apply_color_table(table);
    }

    fn apply_map_actions(&mut self, actions: MapActions) {
        if let Some(site) = actions.site {
            self.choose_site(site);
        }
        if actions.focus_context {
            self.focus_map_context();
        }
        if actions.save_config {
            self.shell.save_config();
        }
        if actions.request_global {
            self.global.request_now();
        }
        if actions.stop_global {
            self.global.stop();
        }
        if actions.switch_to_map {
            self.shell.set_scene(SceneMode::Map);
        }
        if actions.reload_radars {
            let view = self.map.view();
            let (config, notice) = self.shell.config_and_notice_mut();
            self.radar
                .reload_viewport_radars(&view, &self.stations, config, &self.acquire, notice);
        }
    }

    fn update_workers(&mut self, context: &egui::Context) {
        let now = Instant::now();
        self.radar.poll(self.shell.notice_mut());
        self.history
            .update(&mut self.radar, self.shell.notice_mut());
        let site = self.radar.site().cloned();
        self.history
            .advance(site.as_ref(), &mut self.radar, self.shell.notice_mut());
        if let Some(report) = self.stations.update(now, &self.acquire) {
            if let Some(message) = report.notice {
                self.shell.set_notice(Some(message));
            }
            if let Some(pending) = report.pending
                && self.radar.site().is_none()
                && self.stations.find(&pending).is_some()
            {
                self.open_site(pending, true);
            }
        }
        self.places.update(now);
        self.forecast.update(now);
        self.registry.poll();
        let active = self.shell.scene_mode() == SceneMode::Map;
        self.map.update_warnings(active, self.shell.notice_mut());
        self.map.update(now, context);
        let view = self.map.view();
        let visible = self.map.global_visible();
        self.global
            .update(context, &view, visible, self.shell.notice_mut());
        if self.map.take_due_reload(now) {
            let (config, notice) = self.shell.config_and_notice_mut();
            self.radar
                .reload_viewport_radars(&view, &self.stations, config, &self.acquire, notice);
        }
    }

    fn busy(&self) -> bool {
        if self.radar.failed() {
            return false;
        }
        self.places.is_busy()
            || self.forecast.is_busy()
            || self.global.is_loading()
            || self.global.is_playing()
            || self.history.is_loading()
            || self.history.is_playing()
            || self.registry.is_loading()
            || self.radar.is_busy()
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let site = self.radar.site().cloned();
        let name = site
            .as_ref()
            .and_then(|id| self.stations.find(id))
            .map(|station| station.name.clone());
        let actions = self
            .shell
            .header(ui, self.places.place(), site.as_ref(), name.as_deref());
        if actions.switched_to_map {
            self.focus_map_context();
            self.ensure_viewport_radars();
            self.global.request_now();
        }
        if actions.apply_color_table {
            self.apply_color_table();
        }
    }

    fn rail(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("rail")
            .default_size(RAIL_WIDTH)
            .min_size(280.0)
            .max_size(440.0)
            .resizable(true)
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = self.shell.config().theme.visuals();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(theme::SPACE_XL);
                        ui.label(
                            RichText::new("Weather for")
                                .size(theme::TYPE_TITLE)
                                .strong(),
                        );
                        let chosen =
                            self.places
                                .search_section(ui, &self.stations, self.shell.notice_mut());
                        if let Some(place) = chosen {
                            self.choose_place(&place);
                        }
                        if self.places.place().is_some() {
                            ui.add_space(14.0);
                            self.places.place_card(ui);
                            ui.add_space(theme::SPACE_L);
                        }
                        ui.separator();
                        ui.add_space(theme::SPACE_M);
                        // Each workspace owns its own controls: the radar
                        // workspace is for the selected radar, the geographic
                        // one is for layers over the terrain.
                        let mut actions = MapActions::default();
                        match self.shell.scene_mode() {
                            SceneMode::Radar => {
                                self.radar_controls(ui);
                            }
                            SceneMode::Map => {
                                let (config, notice) = self.shell.config_mut_and_notice();
                                actions = self.map.controls(
                                    ui,
                                    config,
                                    &self.forecast,
                                    self.places.place(),
                                    notice,
                                );
                            }
                            SceneMode::Registry => self.registry.rail(ui),
                        }
                        if self.shell.scene_mode() != SceneMode::Registry {
                            let (config, notice) = self.shell.config_and_notice_mut();
                            self.map.warning_section(ui, config, &mut actions, notice);
                        }
                        self.apply_map_actions(actions);
                        if let Some(notice) = self.shell.notice() {
                            ui.add_space(theme::SPACE_M);
                            ui.colored_label(ui.visuals().warn_fg_color, notice);
                        }
                    });
            });
    }

    fn radar_controls(&mut self, ui: &mut egui::Ui) {
        let site = self.radar.site().cloned();
        let chosen = self
            .stations
            .section(ui, site.as_ref(), self.places.place());
        if let Some(id) = chosen {
            self.choose_site(id);
        }
        if let Some(site) = site.as_ref()
            && let Some(station) = self.stations.find(site)
        {
            let (config, notice) = self.shell.config_mut_and_notice();
            if self.radar.product_section(ui, station, config, notice) {
                self.shell.save_config();
            }
        }
        let actions =
            self.history
                .section(ui, site.as_ref(), &mut self.radar, self.shell.notice_mut());
        if actions.return_to_live
            && let Some(site) = site
        {
            self.open_site(site, false);
            self.shell
                .set_notice(Some(String::from("Returned to the live radar feed.")));
        }
        if actions.switch_to_radar {
            self.shell.set_scene(SceneMode::Radar);
        }
    }

    fn forecast_panel(&mut self, ui: &mut egui::Ui) {
        let retry = self.forecast.panel(
            ui,
            self.places.place(),
            self.radar.site(),
            self.shell.config(),
        );
        if retry && let Some(place) = self.places.place().cloned() {
            self.forecast.start(&place);
        }
    }

    fn scene(&mut self, ui: &mut egui::Ui) {
        match self.shell.scene_mode() {
            SceneMode::Map => {
                let (config, notice) = self.shell.config_and_notice_mut();
                let clicked = self.map.draw(
                    ui,
                    &self.radar,
                    &mut self.global,
                    &self.stations,
                    config,
                    self.places.place(),
                    notice,
                );
                if let Some(id) = clicked {
                    self.choose_site(id);
                }
            }
            SceneMode::Radar => {
                let reviewing = self.history.is_reviewing();
                let (config, notice) = self.shell.config_and_notice_mut();
                let clicked = self.radar.draw(
                    ui,
                    &mut self.map,
                    &self.stations,
                    self.places.place(),
                    config,
                    reviewing,
                    notice,
                );
                if let Some(id) = clicked {
                    self.choose_site(id);
                }
            }
            SceneMode::Registry => {
                let palette = self.shell.config().theme.palette();
                self.registry.ui(ui, &palette);
            }
        }
    }
}

impl eframe::App for OxradApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.apply_theme(ui.ctx());
        self.update_workers(ui.ctx());
        self.header(ui);
        {
            let (config, notice) = self.shell.config_and_notice_mut();
            status_bar::status(
                ui,
                config,
                &mut self.radar,
                &self.stations,
                &self.history,
                status_bar::EngineView {
                    acquire: &self.acquire,
                    status: &mut self.engine_status,
                },
                notice,
            );
        }
        self.rail(ui);
        self.forecast_panel(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.shell.config().theme.palette().background))
            .show_inside(ui, |ui| self.scene(ui));
        let interval = if self.busy() {
            BUSY_REPAINT_MS
        } else {
            IDLE_REPAINT_MS
        };
        ui.ctx()
            .request_repaint_after(Duration::from_millis(interval));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scene_switch_is_shell_state_not_map_state() {
        let mut shell = Shell::new(Config::default(), false, None);
        assert_eq!(shell.scene_mode(), SceneMode::Radar);
        shell.set_scene(SceneMode::Map);
        assert_eq!(shell.scene_mode(), SceneMode::Map);
    }
}
