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
//! The geographic map workspace: terrain, environmental domains, warnings,
//! measurement, the global composite and the map's own layer controls.

use std::fmt::Write;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Shape,
    Stroke, Vec2,
};

use crate::acquire::Acquire;
use crate::basemap::Basemap;
use crate::canvas::{self, Canvas};
use crate::config::Config;
use crate::domains::Domains;
use crate::forecast_panel::ForecastPanel;
use crate::global_panel::GlobalPanel;
use crate::layer_catalog::{LayerCatalog, LayerGroup, LayerId};
use crate::map::{self, MapView};
use crate::overlay;
use crate::overlay_select::warning_intersects;
use crate::place::Place;
use crate::quake_layer::QuakeLayer;
use crate::radar_panel::RadarPanel;
use crate::radio::Input as RadioInput;
use crate::radio_layer::Radio;
use crate::station_panel::StationPanel;
use crate::stations::SiteId;
use crate::theme;
use crate::time;
use crate::warning_view::{WarningScope, WarningState, WarningView};
use crate::warnings::Warning;
use crate::workspaces::{MapWorkspace, MeasureColors};

const VIEW_RADAR_TIME_WINDOW_MS: u64 = 6 * 60 * 1000;

/// What the map's controls asked the shell to do.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these are independent one-way requests, not a state machine"
)]
pub(crate) struct MapActions {
    pub(crate) site: Option<SiteId>,
    pub(crate) switch_to_map: bool,
    pub(crate) reload_radars: bool,
    pub(crate) request_global: bool,
    pub(crate) stop_global: bool,
    pub(crate) save_config: bool,
    pub(crate) focus_context: bool,
}

pub(crate) struct MapPanel {
    map: MapWorkspace,
    basemap: Basemap,
    layers: LayerCatalog,
    layer_query: String,
    domains: Domains,
    quakes: QuakeLayer,
    warning_view: WarningView,
    radio: Radio,
}

impl MapPanel {
    pub(crate) fn new(style: crate::basemap::MapStyle, acquire: &Acquire) -> Self {
        Self {
            map: MapWorkspace::new(),
            basemap: Basemap::new(style, acquire.clone()),
            layers: LayerCatalog::new(),
            layer_query: String::new(),
            domains: Domains::new(acquire),
            quakes: QuakeLayer::new(acquire.clone()),
            warning_view: WarningView::new(acquire.clone()),
            radio: Radio::new(),
        }
    }

    pub(crate) const fn view(&self) -> MapView {
        self.map.view
    }

    pub(crate) const fn set_view(&mut self, view: MapView) {
        self.map.view = view;
    }

    pub(crate) fn global_visible(&self) -> bool {
        self.layers.visible(LayerId::GlobalComposite)
    }

    pub(crate) fn update(&mut self, now: Instant, context: &egui::Context) {
        self.quakes.update(now);
        self.domains.update(now, context, &self.map.view);
    }

    pub(crate) fn take_due_reload(&mut self, now: Instant) -> bool {
        self.map.take_due_reload(now)
    }

    /// Polls the warning feed, concerts its message into the shell notice.
    pub(crate) fn update_warnings(&mut self, active: bool, notice: &mut Option<String>) {
        if let Some(message) = self.warning_view.update(active) {
            *notice = Some(message);
        }
    }

    pub(crate) fn draw_basemap(&mut self, ui: &mut egui::Ui, rect: Rect, view: &MapView) {
        self.basemap.draw(ui, rect, view);
    }

    pub(crate) fn draw_attribution(
        &self,
        ui: &mut egui::Ui,
        rect: Rect,
        providers: &crate::config::EnabledProviders,
    ) {
        self.basemap.draw_attribution(ui, rect, providers);
    }

    pub(crate) fn draw_warnings(&self, ui: &egui::Ui, rect: Rect, transform: &map::MapTransform) {
        if !self.layers.visible(LayerId::Warnings) {
            return;
        }
        for warning in self.warning_view.list() {
            if self.warning_view.scope == WarningScope::Viewport
                && !warning_intersects(warning, transform, rect)
            {
                continue;
            }
            let colour = canvas::warning_colour(warning.severity);
            for ring in &warning.polygons {
                let points: Vec<Pos2> = ring
                    .iter()
                    .filter_map(|(lat, lon)| transform.screen(*lat, *lon))
                    .collect();
                if points.len() < 3 {
                    continue;
                }
                let width = points
                    .iter()
                    .map(|point| point.x)
                    .fold(f32::NEG_INFINITY, f32::max)
                    - points
                        .iter()
                        .map(|point| point.x)
                        .fold(f32::INFINITY, f32::min);
                if width > rect.width() * 4.0 {
                    continue;
                }
                let alpha = self.layers.opacity(LayerId::Warnings);
                let (width, colour) =
                    if self.warning_view.selected.as_deref() == Some(warning.id.as_str()) {
                        (4.0_f32, canvas::TEXT.gamma_multiply(alpha))
                    } else {
                        (2.0_f32, colour.gamma_multiply(alpha))
                    };
                ui.painter()
                    .add(Shape::closed_line(points, Stroke::new(width, colour)));
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "station drawing needs the canvas, transform, selection and catalog"
    )]
    pub(crate) fn draw_stations(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        interaction: &egui::Response,
        selected_site: Option<&SiteId>,
        stations: &StationPanel,
        config: &Config,
    ) -> Option<SiteId> {
        let pointer = ui.input(|input| input.pointer.hover_pos());
        let palette = config.theme.palette();
        let mut nearest: Option<(f32, SiteId)> = None;
        if let Ok(list) = stations.result() {
            for station in list {
                let Some(position) = transform.screen(station.lat, station.lon) else {
                    continue;
                };
                if !rect.shrink(2.0).contains(position) {
                    continue;
                }
                let selected = selected_site == Some(&station.id);
                if selected {
                    ui.painter().circle_stroke(
                        position,
                        8.0,
                        Stroke::new(2.0_f32, palette.warning),
                    );
                }
                ui.painter().circle_filled(
                    position,
                    if selected { 4.0 } else { 2.4 },
                    if selected {
                        palette.warning
                    } else {
                        Color32::from_rgb(105, 202, 214)
                    },
                );
                if selected || self.map.view.span_km() < 1_800.0 {
                    ui.painter().text(
                        position + Vec2::new(7.0, -7.0),
                        Align2::LEFT_BOTTOM,
                        station.id.as_str(),
                        FontId::monospace(10.0),
                        if selected {
                            palette.warning
                        } else {
                            palette.map_text
                        },
                    );
                }
                if let Some(pointer) = pointer {
                    let distance = position.distance(pointer);
                    if distance <= 12.0 && nearest.as_ref().is_none_or(|(best, _)| distance < *best)
                    {
                        nearest = Some((distance, station.id.clone()));
                    }
                }
            }
        }
        if interaction.clicked() {
            nearest.map(|(_, id)| id)
        } else {
            None
        }
    }

    pub(crate) fn draw_place(
        &self,
        ui: &egui::Ui,
        transform: &map::MapTransform,
        place: Option<&Place>,
        config: &Config,
    ) {
        let Some(place) = place else {
            return;
        };
        let palette = config.theme.palette();
        let Some(position) = transform.screen(place.lat, place.lon) else {
            return;
        };
        ui.painter()
            .circle_filled(position, 5.0, Color32::from_rgb(245, 245, 240));
        ui.painter().text(
            position + Vec2::new(8.0, 8.0),
            Align2::LEFT_TOP,
            place.label(),
            FontId::proportional(theme::TYPE_SMALL),
            palette.map_text,
        );
    }

    /// The geographic canvas.
    #[expect(
        clippy::too_many_arguments,
        reason = "the canvas reads one panel each for radar, global, stations and settings"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "the map is one ordered drawing pass; splitting it would hide the layer order"
    )]
    pub(crate) fn draw(
        &mut self,
        ui: &mut egui::Ui,
        radar: &RadarPanel,
        global: &mut GlobalPanel,
        stations: &StationPanel,
        config: &Config,
        place: Option<&Place>,
        notice: &mut Option<String>,
    ) -> Option<SiteId> {
        let full_rect = ui.max_rect();
        ui.painter().rect_filled(
            full_rect,
            CornerRadius::ZERO,
            config.theme.palette().background,
        );
        if self.layers.visible(LayerId::GlobalComposite) && !global.is_empty() {
            global.history_bar(ui, config);
        }
        let rect = ui.available_rect_before_wrap();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return None;
        }
        let palette = config.theme.palette();
        let (rect, legend_rect) = canvas::legend_split(rect, radar.legend_expected());
        self.map.viewport = Some(rect);
        let transform = match self.map.view.transform(rect) {
            Ok(transform) => transform,
            Err(error) => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("The map is unavailable: {error}"),
                    FontId::proportional(15.0),
                    canvas::ERROR,
                );
                return None;
            }
        };
        let view = self.map.view;
        self.basemap.draw(ui, rect, &view);
        let interaction = ui.interact(
            rect,
            ui.id().with("geographic-map"),
            egui::Sense::click_and_drag(),
        );
        if self.apply_canvas_input(ui, rect, &interaction, notice) {
            self.schedule_viewport_reload();
        }
        if let Err(error) = map::draw_graticule(ui.painter(), &transform, rect, view.span_km()) {
            *notice = Some(format!("Map grid unavailable: {error}"));
        }
        self.domains.draw_satellite(ui, &transform, rect);
        self.domains
            .draw_fires(ui.painter(), &transform, rect, &palette);
        self.domains
            .draw_volcanoes(ui.painter(), &transform, rect, &palette);
        self.domains
            .draw_gauges(ui.painter(), &transform, rect, &palette);
        self.domains
            .draw_cyclones(ui.painter(), &transform, rect, &palette);
        self.domains
            .draw_stations(ui.painter(), &transform, &palette);
        if self.layers.visible(LayerId::GlobalComposite) {
            global.draw(
                ui.painter(),
                &transform,
                self.layers.alpha(LayerId::GlobalComposite),
            );
        }
        let radar_visible =
            radar.draw_viewport_radars(ui, rect, &transform, view.span_km(), notice);
        let ring_labels = radar.draw_range_rings(ui, rect, &transform, stations, config, notice);
        self.draw_warnings(ui, rect, &transform);
        let mut occupied = canvas::chrome_obstacles(rect, Canvas::Map).to_vec();
        radar.draw_range_labels(
            ui,
            rect,
            &transform,
            stations,
            config,
            &ring_labels,
            &mut occupied,
        );
        self.draw_measure(ui, rect, &transform, &interaction, config, notice);
        let clicked_site = if self.map.measure.is_on() {
            None
        } else {
            self.draw_stations(
                ui,
                rect,
                &transform,
                &interaction,
                radar.site(),
                stations,
                config,
            )
        };
        self.draw_place(ui, &transform, place, config);
        map::draw_scale(ui.painter(), rect, &transform, &palette);
        canvas::radar_readout(
            ui,
            rect,
            &transform,
            radar.display(),
            radar.product(),
            radar_visible,
            Some(canvas::map_status_area(rect)),
        );
        if radar_visible && let Some(legend) = legend_rect {
            ui.painter()
                .rect_filled(legend, CornerRadius::ZERO, palette.canvas);
            overlay::draw_legend(
                ui.painter(),
                legend,
                radar.product(),
                config.color_table,
                &palette,
            );
        }
        let pointer = ui.input(|input| input.pointer.hover_pos());
        self.quakes.draw(
            ui.painter(),
            rect,
            &transform,
            pointer,
            interaction.clicked(),
        );
        self.draw_status(ui, rect, radar, global, config);
        self.basemap.draw_attribution(ui, rect, &config.providers);
        clicked_site
    }

    fn apply_canvas_input(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        interaction: &egui::Response,
        notice: &mut Option<String>,
    ) -> bool {
        match canvas::canvas_input(ui, rect, interaction, &mut self.map.view) {
            Ok(changed) => changed,
            Err(error) => {
                *notice = Some(format!("The view could not be moved: {error}"));
                false
            }
        }
    }

    pub(crate) fn schedule_viewport_reload(&mut self) {
        self.map.schedule_reload(Duration::from_millis(700));
    }

    fn draw_measure(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        interaction: &egui::Response,
        config: &Config,
        notice: &mut Option<String>,
    ) {
        if !self.map.measure.is_on() {
            return;
        }
        if interaction.clicked()
            && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
            && rect.contains(pointer)
        {
            match transform.latlon_at(pointer) {
                Ok(point) => {
                    if let Err(error) = self.map.measure.add(point) {
                        *notice = Some(error.to_string());
                        return;
                    }
                }
                Err(error) => {
                    *notice = Some(format!("Measurement point unavailable: {error}"));
                    return;
                }
            }
        }
        let palette = config.theme.palette();
        self.map.measure.draw(
            ui.painter(),
            rect,
            transform,
            config.units,
            MeasureColors {
                accent: canvas::STALE,
                text: canvas::TEXT,
                outline: palette.map_text,
            },
        );
    }

    fn draw_status(
        &self,
        ui: &mut egui::Ui,
        rect: Rect,
        radar: &RadarPanel,
        global: &GlobalPanel,
        config: &Config,
    ) {
        self.draw_status_details(ui, rect, radar, global, config);
        let palette = config.theme.palette();
        let secondary = Color32::from_rgba_unmultiplied(
            palette.map_text.r(),
            palette.map_text.g(),
            palette.map_text.b(),
            200,
        );
        let centre = self.map.view.center_latlon().map_or_else(
            |_| String::from("unavailable"),
            |(lat, lon)| format!("{lat:.2}, {lon:.2}"),
        );
        let panel = canvas::map_header_rect(rect);
        ui.painter().rect_filled(
            panel,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(
                palette.surface.r(),
                palette.surface.g(),
                palette.surface.b(),
                214,
            ),
        );
        ui.painter().text(
            panel.left_top() + Vec2::new(10.0, 6.0),
            Align2::LEFT_TOP,
            "Geographic map",
            FontId::proportional(theme::TYPE_TITLE),
            palette.map_text,
        );
        ui.painter().text(
            panel.left_top() + Vec2::new(10.0, 29.0),
            Align2::LEFT_TOP,
            format!(
                "Centre {centre} · {:.0} km wide · {} viewport radars",
                self.map.view.span_km(),
                radar.view_radars().len()
            ),
            FontId::proportional(theme::TYPE_SMALL),
            secondary,
        );
        ui.painter().text(
            panel.left_top() + Vec2::new(10.0, 44.0),
            Align2::LEFT_TOP,
            "Drag to pan · scroll to zoom · click a radar site",
            FontId::proportional(theme::TYPE_SMALL),
            secondary,
        );
    }

    fn draw_status_details(
        &self,
        ui: &mut egui::Ui,
        rect: Rect,
        radar: &RadarPanel,
        global: &GlobalPanel,
        config: &Config,
    ) {
        let area = canvas::map_status_area(rect);
        if area.width() < 12.0 || area.height() < 12.0 {
            return;
        }
        let palette = config.theme.palette();
        let width = area.width();
        let height = area.height();
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(area)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(
                        palette.surface.r(),
                        palette.surface.g(),
                        palette.surface.b(),
                        232,
                    ))
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.set_width(width - 12.0);
                        egui::ScrollArea::vertical()
                            .max_height((height - 12.0).max(0.0))
                            .show(ui, |ui| {
                                if let Some(layers) = self.layer_line(radar, global) {
                                    ui.label(
                                        RichText::new(layers)
                                            .monospace()
                                            .size(theme::TYPE_SMALL)
                                            .weak(),
                                    );
                                }
                                if self.layers.visible(LayerId::Warnings)
                                    && let Some(warning) = self.most_severe_warning()
                                {
                                    ui.label(
                                        RichText::new(format!(
                                            "Most severe: {} · {} · {} · expires {}",
                                            warning.event,
                                            warning.area,
                                            warning.urgency.label(),
                                            warning.expires
                                        ))
                                        .monospace()
                                        .size(theme::TYPE_SMALL)
                                        .color(ui.visuals().error_fg_color),
                                    );
                                    if !warning.sent.is_empty() {
                                        ui.label(
                                            RichText::new(format!("Issued {}", warning.sent))
                                                .monospace()
                                                .size(theme::TYPE_SMALL)
                                                .weak(),
                                        );
                                    }
                                }
                            });
                    });
            },
        );
    }

    fn layer_line(&self, radar: &RadarPanel, global: &GlobalPanel) -> Option<String> {
        let reference = radar.scan_reference();
        let mut contributing = Vec::new();
        let mut excluded = Vec::new();
        if self.layers.visible(LayerId::Radar) {
            if radar
                .display()
                .frame
                .as_ref()
                .is_some_and(|frame| frame.geometry.product == radar.product())
                && let Some(site) = radar.site()
            {
                contributing.push(format!("{site} selected"));
            }
            for layer in radar.view_radars() {
                let Some(frame) = layer.display.frame.as_ref() else {
                    continue;
                };
                if frame.geometry.product != radar.product() {
                    continue;
                }
                if reference.is_none_or(|reference| {
                    reference.abs_diff(frame.volume.scan_unix_ms) <= VIEW_RADAR_TIME_WINDOW_MS
                }) {
                    contributing.push(layer.site.to_string());
                } else {
                    excluded.push(layer.site.to_string());
                }
            }
        }
        let contributing = contributing.join(", ");
        let time_note = if excluded.is_empty() {
            String::new()
        } else {
            format!(" · time-excluded: {}", excluded.join(", "))
        };
        let global_note = if self.layers.visible(LayerId::GlobalComposite) {
            global.layer_summary().to_owned()
        } else {
            String::from(" · global off")
        };
        if contributing.is_empty()
            && !self.layers.visible(LayerId::GlobalComposite)
            && !self.layers.visible(LayerId::Warnings)
        {
            return None;
        }
        let warnings = self.warning_layer_summary();
        let scope = if self.warning_view.scope == WarningScope::Viewport {
            " · warnings in view"
        } else {
            ""
        };
        Some(format!(
            "Layers: {contributing}{global_note}{warnings}{scope}{time_note}"
        ))
    }

    fn warning_layer_summary(&self) -> String {
        if !self.layers.visible(LayerId::Warnings) {
            return String::from(" · warnings off");
        }
        match self.warning_view.state {
            WarningState::Idle => String::new(),
            WarningState::Loading => String::from(" · warnings loading"),
            WarningState::Ready => {
                let mut text = format!(" · {} warnings", self.warning_view.list().len());
                if (self.warning_view.without_geometry() > 0 || self.warning_view.rejected() > 0)
                    && write!(
                        text,
                        " · {} without polygons · {} unreadable",
                        self.warning_view.without_geometry(),
                        self.warning_view.rejected()
                    )
                    .is_err()
                {
                    text.push_str(" · warning counts unavailable");
                }
                text
            }
            WarningState::Failed => String::from(" · warnings error"),
        }
    }

    fn most_severe_warning(&self) -> Option<&Warning> {
        self.warning_view
            .list()
            .iter()
            .max_by_key(|warning| canvas::severity_rank(warning.severity))
    }

    /// The map's controls in the rail.
    pub(crate) fn controls(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut Config,
        forecast: &ForecastPanel,
        place: Option<&Place>,
        notice: &mut Option<String>,
    ) -> MapActions {
        let mut actions = MapActions::default();
        self.zoom_controls(ui, config, &mut actions, notice);
        self.overlay_controls(ui, config);
        self.layer_list(ui, &mut actions);
        ui.add_space(theme::SPACE_M);
        let palette = config.theme.palette();
        let centre = self.map.view.center_latlon().ok();
        ui.label("Basemap style");
        if let Some(style) = self.basemap.controls(ui) {
            config.map_style = style;
            actions.save_config = true;
        }
        self.domains.rail(ui, &palette, centre);
        self.quake_rail(ui, &palette, &mut actions, notice);
        self.domain_focus(&mut actions, notice);
        let clock = time::now_unix_ms();
        let radio_input = RadioInput {
            now_ms: clock.as_ref().copied().unwrap_or(0),
            clock_read: clock.is_ok(),
            place: place.map(|place| place.name.as_str()),
            warnings: self.warning_view.list(),
            quakes: self.quakes.events(),
            stations: self.domains.stations(),
            forecast: forecast.forecast(),
            air: self.domains.air(),
            space: self.domains.space(),
            sounding: self.domains.sounding(),
            volcanoes: self.domains.volcanoes(),
            gauges: self.domains.gauges(),
            cyclones: self.domains.cyclones(),
        };
        self.radio.update(Instant::now(), &radio_input);
        self.radio.rail(ui, &palette, &radio_input);
        actions
    }

    fn zoom_controls(
        &mut self,
        ui: &mut egui::Ui,
        config: &Config,
        actions: &mut MapActions,
        notice: &mut Option<String>,
    ) {
        let palette = config.theme.palette();
        ui.horizontal(|ui| {
            if ui
                .add_sized([34.0, 30.0], canvas::action_button(&palette, "+"))
                .clicked()
            {
                match self.map.view.zoom_center(1.6) {
                    Ok(()) => {
                        self.schedule_viewport_reload();
                        actions.request_global = true;
                    }
                    Err(error) => *notice = Some(format!("Map zoom failed: {error}")),
                }
            }
            if ui
                .add_sized([34.0, 30.0], canvas::action_button(&palette, "−"))
                .clicked()
            {
                match self.map.view.zoom_center(1.0 / 1.6) {
                    Ok(()) => {
                        self.schedule_viewport_reload();
                        actions.request_global = true;
                    }
                    Err(error) => *notice = Some(format!("Map zoom failed: {error}")),
                }
            }
            if ui
                .add_sized([64.0, 30.0], canvas::action_button(&palette, "Focus"))
                .clicked()
            {
                actions.focus_context = true;
            }
        });
        if ui
            .add_sized(
                [116.0, 30.0],
                canvas::action_button(&palette, "Reload nearby"),
            )
            .on_hover_text("Find up to three stations around the current map centre.")
            .clicked()
        {
            actions.reload_radars = true;
        }
    }

    fn overlay_controls(&mut self, ui: &mut egui::Ui, config: &Config) {
        let palette = config.theme.palette();
        ui.add_space(theme::SPACE_M);
        if ui
            .add_sized(
                [116.0, 30.0],
                canvas::toggle_button(
                    &palette,
                    if self.map.measure.is_on() {
                        "Measure: on"
                    } else {
                        "Measure: off"
                    },
                    self.map.measure.is_on(),
                ),
            )
            .on_hover_text(
                "Click map corners to measure geodesic distance and, from three corners, enclosed area.",
            )
            .clicked()
        {
            self.map.measure.toggle();
        }
    }

    fn layer_list(&mut self, ui: &mut egui::Ui, actions: &mut MapActions) {
        ui.add_space(theme::SPACE_M);
        ui.label(RichText::new("Layers").size(theme::TYPE_LABEL).strong());
        ui.add(
            egui::TextEdit::singleline(&mut self.layer_query)
                .hint_text("Filter layers")
                .desired_width(f32::INFINITY)
                .margin(Vec2::new(8.0, 5.0)),
        );
        let matching = self.layers.search(&self.layer_query);
        let mut changed = Vec::new();
        for group in LayerGroup::ALL {
            let ids: Vec<LayerId> = self
                .layers
                .in_group(group)
                .into_iter()
                .filter(|id| matching.contains(id))
                .collect();
            if ids.is_empty() {
                continue;
            }
            ui.add_space(theme::SPACE_S);
            ui.label(RichText::new(group.label()).size(theme::TYPE_SMALL).weak());
            for id in ids {
                let mut visible = self.layers.visible(id);
                let mut opacity = self.layers.opacity(id);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut visible, id.label())
                        .on_hover_text(id.detail());
                    if visible {
                        ui.add(
                            egui::Slider::new(&mut opacity, 0.1..=1.0)
                                .show_value(false)
                                .trailing_fill(true),
                        )
                        .on_hover_text("Layer opacity");
                    }
                });
                if visible != self.layers.visible(id) {
                    changed.push((id, visible));
                }
                if (self.layers.opacity(id) - opacity).abs() > 1e-6 {
                    self.layers.set_opacity(id, opacity);
                }
            }
        }
        for (id, visible) in changed {
            self.set_layer_visible(id, visible, actions);
        }
    }

    fn domain_focus(&mut self, actions: &mut MapActions, notice: &mut Option<String>) {
        let (lat, lon, span) = if let Some((lat, lon)) = self.domains.take_volcano_focus() {
            (lat, lon, 600.0)
        } else if let Some((lat, lon)) = self.domains.take_gauge_focus() {
            (lat, lon, 120.0)
        } else if let Some((lat, lon)) = self.domains.take_cyclone_focus() {
            (lat, lon, 2_500.0)
        } else {
            return;
        };
        self.focus(lat, lon, span, actions, notice);
    }

    fn quake_rail(
        &mut self,
        ui: &mut egui::Ui,
        palette: &crate::theme::Palette,
        actions: &mut MapActions,
        notice: &mut Option<String>,
    ) {
        let Some((lat, lon)) = self.quakes.rail(ui, palette) else {
            return;
        };
        self.focus(lat, lon, 1_200.0, actions, notice);
    }

    fn focus(
        &mut self,
        lat: f64,
        lon: f64,
        span: f64,
        actions: &mut MapActions,
        notice: &mut Option<String>,
    ) {
        match MapView::focus(lat, lon, span) {
            Ok(view) => {
                self.map.view = view;
                self.schedule_viewport_reload();
                actions.reload_radars = true;
                actions.request_global = true;
            }
            Err(error) => *notice = Some(format!("Map centre unavailable: {error}")),
        }
    }

    /// The active warnings rail. Drawing it can move the map and switch scene.
    pub(crate) fn warning_section(
        &mut self,
        ui: &mut egui::Ui,
        config: &Config,
        actions: &mut MapActions,
        notice: &mut Option<String>,
    ) {
        if !self.layers.visible(LayerId::Warnings) || self.warning_view.list().is_empty() {
            return;
        }
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Active warnings")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new("National Weather Service · United States and territories")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        if let Some(served) = self.warning_view.served() {
            canvas::provenance(ui, served);
        }
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.warning_view.scope,
                WarningScope::All,
                "All active",
            );
            ui.selectable_value(
                &mut self.warning_view.scope,
                WarningScope::Viewport,
                "Visible map",
            );
        });
        let mut order: Vec<usize> = (0..self.warning_view.list().len()).collect();
        order.sort_by_key(|index| {
            self.warning_view
                .list()
                .get(*index)
                .map_or(0, |warning| canvas::severity_rank(warning.severity))
        });
        order.reverse();
        if self.warning_view.scope == WarningScope::Viewport {
            order.retain(|index| {
                self.warning_view
                    .list()
                    .get(*index)
                    .is_some_and(|warning| self.warning_visible(warning))
            });
        }
        let visible = order.len();
        let mut chosen = None;
        let palette = config.theme.palette();
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for index in order.into_iter().take(40) {
                    let Some(warning) = self.warning_view.list().get(index) else {
                        continue;
                    };
                    let selected =
                        self.warning_view.selected.as_deref() == Some(warning.id.as_str());
                    let label = format!(
                        "{} · {}\n{} · expires {}",
                        warning.event,
                        warning.area,
                        warning.urgency.label(),
                        warning.expires
                    );
                    if ui
                        .add_sized(
                            [ui.available_width(), 52.0],
                            canvas::toggle_button(&palette, &label, selected),
                        )
                        .clicked()
                    {
                        chosen = Some(index);
                    }
                }
            });
        if visible == 0 {
            ui.label(
                RichText::new("No active warnings intersect the visible map.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
        if self.warning_view.list().len() > 40 {
            ui.label(
                RichText::new(format!(
                    "Showing 40 most severe of {} active alerts.",
                    self.warning_view.list().len()
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        if let Some(index) = chosen {
            self.focus_warning(index, actions, notice);
        }
        self.warning_detail(ui);
    }

    fn warning_visible(&self, warning: &Warning) -> bool {
        if self.warning_view.scope == WarningScope::All {
            return true;
        }
        let Some(rect) = self.map.viewport else {
            return true;
        };
        let Ok(transform) = self.map.view.transform(rect) else {
            return true;
        };
        warning_intersects(warning, &transform, rect)
    }

    fn warning_detail(&self, ui: &mut egui::Ui) {
        let Some(warning) = self.warning_view.selected_warning() else {
            return;
        };
        ui.add_space(10.0);
        ui.separator();
        ui.label(
            RichText::new("Selected alert")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        let title = if warning.headline.is_empty() {
            format!("{} for {}", warning.event, warning.area)
        } else {
            warning.headline.clone()
        };
        ui.label(RichText::new(title).size(theme::TYPE_LABEL).strong());
        ui.label(
            RichText::new(format!(
                "{} · {} · {}",
                warning.severity.label(),
                warning.urgency.label(),
                warning.area
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        ui.label(
            RichText::new(format!(
                "Issued {} · expires {}",
                warning.sent, warning.expires
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        if warning.description.is_empty() {
            ui.label(
                RichText::new("This alert carries no description text.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        } else {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.label(RichText::new(&warning.description).size(theme::TYPE_SMALL));
                    if let Some(instruction) = &warning.instruction {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Instructions")
                                .size(theme::TYPE_SMALL)
                                .strong()
                                .weak(),
                        );
                        ui.label(RichText::new(instruction).size(theme::TYPE_SMALL));
                    }
                });
        }
        ui.label(
            RichText::new(format!("Source: NWS active alerts · {}", warning.id))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }

    fn focus_warning(
        &mut self,
        index: usize,
        actions: &mut MapActions,
        notice: &mut Option<String>,
    ) {
        let Some(id) = self
            .warning_view
            .list()
            .get(index)
            .map(|warning| warning.id.clone())
        else {
            *notice = Some(String::from("That warning is no longer available."));
            return;
        };
        self.warning_view.selected = Some(id);
        let Some(warning) = self.warning_view.selected_warning() else {
            return;
        };
        let Some(ring) = warning.polygons.first() else {
            *notice = Some(format!(
                "Warning {} has no mapped polygon; its area is {}.",
                warning.event, warning.area
            ));
            return;
        };
        let mut lat = 0.0_f64;
        let mut lon = 0.0_f64;
        for (point_lat, point_lon) in ring {
            lat += *point_lat;
            lon += *point_lon;
        }
        let count = ring.len() as f64;
        if count <= 0.0 {
            *notice = Some(String::from("That warning polygon has no points."));
            return;
        }
        lat /= count;
        lon /= count;
        let label = format!("{} for {}", warning.event, warning.area);
        match MapView::focus(lat, lon, 700.0) {
            Ok(view) => {
                self.map.view = view;
                self.schedule_viewport_reload();
                actions.switch_to_map = true;
                actions.reload_radars = true;
                actions.request_global = true;
                *notice = Some(format!(
                    "Selected {label}; polygon centred near {lat:.2}, {lon:.2}."
                ));
            }
            Err(error) => {
                *notice = Some(format!("Warning map centre unavailable: {error}"));
            }
        }
    }

    /// Applies a layer switch and reports what it means for the other panels.
    /// The rail calls this; tests drive it without a window.
    pub(crate) fn set_layer_visible(
        &mut self,
        id: LayerId,
        visible: bool,
        actions: &mut MapActions,
    ) {
        if self.layers.visible(id) == visible {
            return;
        }
        self.layers.set_visible(id, visible);
        match id {
            LayerId::Radar => {}
            LayerId::Warnings => {
                if visible && matches!(self.warning_view.state, WarningState::Idle) {
                    self.warning_view.retry_now();
                }
            }
            LayerId::GlobalComposite => {
                if visible {
                    actions.request_global = true;
                } else {
                    actions.stop_global = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basemap::MapStyle;

    fn panel() -> MapPanel {
        let acquire = Acquire::new(Config::default().providers);
        MapPanel::new(MapStyle::Liberty, &acquire)
    }

    #[test]
    fn hiding_and_restoring_the_global_layer_crosses_to_the_composite_actions() {
        let mut map = panel();
        let mut actions = MapActions::default();
        map.set_layer_visible(LayerId::GlobalComposite, false, &mut actions);
        assert!(actions.stop_global);
        assert!(!actions.request_global);

        let mut actions = MapActions::default();
        map.set_layer_visible(LayerId::GlobalComposite, true, &mut actions);
        assert!(actions.request_global);
        assert!(!actions.stop_global);
    }
}
