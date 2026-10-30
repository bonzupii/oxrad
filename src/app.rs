// oxrad — A storm-chaser-grade NEXRAD weather radar viewer
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
//! The application window: site selection, map, readout, and status.

use std::time::Duration;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, Vec2};

use crate::config::{self, Config, Loaded};
use crate::display::{Display, DisplayState, Frame};
use crate::gpu::RadarGpu;
use crate::overlay;
use crate::pipeline::{Pipeline, PipelineState};
use crate::radar::GateValue;
use crate::render::{self, ViewTransform};
use crate::stations::{self, SiteId, Station};
use crate::time;

const BACKGROUND: Color32 = Color32::from_rgb(8, 10, 14);
const TEXT: Color32 = Color32::from_rgb(206, 214, 226);
const MUTED: Color32 = Color32::from_rgb(150, 160, 175);
const ERROR: Color32 = Color32::from_rgb(255, 122, 122);
const STALE: Color32 = Color32::from_rgb(255, 196, 64);
const HIGHLIGHT: Color32 = Color32::from_rgb(38, 60, 90);
const STALE_AFTER_MS: u64 = 15 * 60 * 1000;
const PICKER_WIDTH: f32 = 620.0;
const PICKER_ROW_HEIGHT: f32 = 30.0;
const PICKER_LIST_HEIGHT: f32 = 330.0;
const BUSY_REPAINT_MS: u64 = 100;
const IDLE_REPAINT_MS: u64 = 1000;

pub struct OxradApp {
    stations: Result<Vec<Station>, String>,
    config: Config,
    config_writable: bool,
    notice: Option<String>,
    query: String,
    picker_index: usize,
    picker_focused: bool,
    site: Option<SiteId>,
    pipeline: Option<Pipeline>,
    display: Display,
}

struct PickerChoice {
    index: usize,
    chosen: Option<SiteId>,
}

impl OxradApp {
    pub fn new(gpu: Option<RadarGpu>, initial: Option<SiteId>) -> Self {
        let stations = stations::catalog().map_err(|error| error.to_string());
        let mut config = Config::default();
        let mut config_writable = true;
        let mut notice = None;
        match config::load_default() {
            Ok(Loaded::Missing) => {}
            Ok(Loaded::Present(loaded)) => config = loaded,
            Err(error) => {
                config_writable = false;
                notice = Some(format!("Configuration ignored: {error}"));
            }
        }
        let restored = initial.or_else(|| config.site.clone());
        let mut app = Self {
            stations,
            config,
            config_writable,
            notice,
            query: String::new(),
            picker_index: 0,
            picker_focused: false,
            site: None,
            pipeline: None,
            display: Display::new(gpu),
        };
        if let Some(id) = restored {
            match &app.stations {
                Ok(list) if stations::find(list, &id).is_some() => app.open_site(id, false),
                Ok(_) => {
                    app.notice = Some(format!(
                        "Site {id} is not in the radar site catalog; choose a site."
                    ));
                }
                Err(_) => {}
            }
        }
        app
    }

    fn open_site(&mut self, id: SiteId, persist: bool) {
        self.display.reset();
        self.pipeline = Some(Pipeline::start(id.clone()));
        self.site = Some(id.clone());
        self.picker_focused = false;
        if !persist {
            return;
        }
        self.config.site = Some(id);
        if !self.config_writable {
            return;
        }
        match config::save_default(&self.config) {
            Ok(()) => self.notice = None,
            Err(error) => self.notice = Some(format!("Could not save the selected site: {error}")),
        }
    }

    fn update_workers(&mut self) {
        if let Some(pipeline) = self.pipeline.as_mut()
            && let Some(volume) = pipeline.poll()
        {
            self.display.submit(volume);
        }
        self.display.poll();
    }

    fn site_name(&self) -> &str {
        self.site.as_ref().map_or("no site", |site| site.as_str())
    }

    fn placeholder_text(&self) -> String {
        match (&self.site, &self.pipeline) {
            (None, _) => String::from("No site selected. Choose a radar on the left to begin."),
            (Some(site), Some(pipeline)) => match &pipeline.state {
                PipelineState::Failed(failure) => format!(
                    "Radar unavailable. {} failed for {}: {}",
                    failure.operation.description(),
                    site,
                    failure.error
                ),
                PipelineState::Working { operation, .. } => {
                    format!("{} the latest volume for {site}…", operation.description())
                }
                PipelineState::Ready => match &self.display.state {
                    DisplayState::Unavailable(_) | DisplayState::Failed { .. } => String::from(
                        "The radar image could not be prepared; the status bar has the reason.",
                    ),
                    DisplayState::Waiting | DisplayState::Building | DisplayState::Ready => {
                        String::from("Preparing the radar image…")
                    }
                },
            },
            (Some(site), None) => format!("Starting the radar pipeline for {site}…"),
        }
    }

    const fn phase(&self) -> &'static str {
        match &self.pipeline {
            None => "No site selected",
            Some(pipeline) => match &pipeline.state {
                PipelineState::Failed(_) => "Error",
                PipelineState::Working { .. } => {
                    if self.display.frame.is_some() {
                        "Refreshing"
                    } else {
                        pipeline.state.label()
                    }
                }
                PipelineState::Ready => match &self.display.state {
                    DisplayState::Ready => "Ready",
                    DisplayState::Unavailable(_) | DisplayState::Failed { .. } => "Render error",
                    DisplayState::Waiting | DisplayState::Building => "Preparing frame",
                },
            },
        }
    }

    fn failed(&self) -> bool {
        self.pipeline
            .as_ref()
            .is_some_and(|pipeline| pipeline.state.failure().is_some())
            || self.display.state.error().is_some()
    }

    fn busy(&self) -> bool {
        if self.failed() {
            return false;
        }
        self.site.is_some()
            && (self.display.state.is_busy()
                || self
                    .pipeline
                    .as_ref()
                    .is_some_and(|pipeline| pipeline.state.is_working())
                || self.display.frame.is_none())
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        let mut retry_pipeline = false;
        let mut retry_display = false;
        let failed = self.failed();
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    match &self.site {
                        Some(site) => {
                            ui.strong(format!("{site} · Level II · Base reflectivity"));
                        }
                        None => {
                            ui.strong("No site selected");
                        }
                    }
                    ui.separator();
                    ui.label(self.phase());
                    if let Some(frame) = &self.display.frame {
                        scan_status(ui, frame, failed);
                    }
                });
                if let Some(notice) = &self.notice {
                    ui.colored_label(STALE, notice);
                }
                if let Err(error) = &self.stations {
                    ui.colored_label(ERROR, format!("Radar site catalog unavailable: {error}"));
                }
                if let Some(failure) = self
                    .pipeline
                    .as_ref()
                    .and_then(|pipeline| pipeline.state.failure())
                {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            ERROR,
                            format!(
                                "{} failed for {}: {}",
                                failure.operation.description(),
                                self.site_name(),
                                failure.error
                            ),
                        );
                        if let Some(wait) = failure.retry_in {
                            ui.label(format!("Retry in {} s", wait.as_secs()));
                        }
                        if failure.waiting_for_next_volume {
                            ui.label("Waiting for a new volume or a manual retry.");
                        }
                        retry_pipeline = ui.button("Retry now").clicked();
                    });
                }
                if let Some(error) = self.display.state.error() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(ERROR, format!("{}: {error}", self.site_name()));
                        retry_display = ui.button("Retry render").clicked();
                    });
                }
                if let Some(frame) = &self.display.frame {
                    diagnostics(ui, frame);
                }
            });
        });
        if retry_pipeline
            && let Some(pipeline) = self.pipeline.as_mut()
            && let Err(error) = pipeline.retry()
        {
            self.notice = Some(format!("Retry unavailable: {error}"));
        }
        if retry_display {
            self.display.retry();
        }
    }

    fn scene(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
        if self.site.is_none() {
            self.picker(ui);
            return;
        }
        if self.display.frame.is_none() {
            self.placeholder(ui);
            return;
        }
        let Some(frame) = self.display.frame.as_ref() else {
            return;
        };
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let viewport = render::radar_viewport(rect);
        let transform = match ViewTransform::new(viewport, &frame.geometry) {
            Ok(transform) => transform,
            Err(error) => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("Radar view unavailable: {error}"),
                    FontId::proportional(15.0),
                    ERROR,
                );
                return;
            }
        };
        if let Some(shape) = frame.shape(viewport) {
            ui.painter().add(shape);
        }
        if frame.mesh.is_none() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "No measured echo in this sweep; this does not establish clear weather.",
                FontId::proportional(15.0),
                TEXT,
            );
        }
        overlay::draw_grid(ui.painter(), &frame.geometry, &transform);
        overlay::draw_rings(ui.painter(), &frame.geometry, &transform);
        overlay::draw_legend(ui.painter(), rect);
        readout(ui, frame, &transform, viewport, rect);
    }

    fn placeholder(&self, ui: &egui::Ui) {
        ui.painter().text(
            ui.max_rect().center(),
            Align2::CENTER_CENTER,
            self.placeholder_text(),
            FontId::proportional(16.0),
            TEXT,
        );
    }

    fn picker(&mut self, ui: &mut egui::Ui) {
        let mut query = std::mem::take(&mut self.query);
        let mut index = self.picker_index;
        let mut chosen = None;
        let width = (ui.available_width() - 32.0).clamp(240.0, PICKER_WIDTH);
        egui::ScrollArea::vertical().show(ui, |ui| {
            let indent = ((ui.available_width() - width) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(indent);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(20.0);
                    ui.heading("Choose a radar");
                    ui.colored_label(
                        MUTED,
                        "Live Level II base reflectivity from a National Weather Service radar.",
                    );
                    ui.colored_label(
                        MUTED,
                        "Type a site identifier or place name, then press Enter.",
                    );
                    ui.add_space(12.0);
                    let response = ui.add_sized(
                        [width, 30.0],
                        egui::TextEdit::singleline(&mut query)
                            .hint_text("Search sites, for example Paducah or KPAH"),
                    );
                    if !self.picker_focused && !response.has_focus() {
                        response.request_focus();
                        self.picker_focused = true;
                    }
                    ui.add_space(8.0);
                    match &self.stations {
                        Err(error) => {
                            ui.colored_label(
                                ERROR,
                                format!("Radar site catalog unavailable: {error}"),
                            );
                        }
                        Ok(stations) => {
                            let choice = picker_list(ui, stations, &query, index, width);
                            index = choice.index;
                            chosen = choice.chosen;
                        }
                    }
                    if let Some(notice) = &self.notice {
                        ui.add_space(8.0);
                        ui.colored_label(STALE, notice);
                    }
                });
            });
        });
        self.query = query;
        self.picker_index = index;
        if let Some(id) = chosen {
            self.open_site(id, true);
        }
    }
}

fn picker_list(
    ui: &mut egui::Ui,
    stations: &[Station],
    query: &str,
    index: usize,
    width: f32,
) -> PickerChoice {
    let filtered: Vec<&Station> = stations
        .iter()
        .filter(|station| stations::matches(station, query))
        .collect();
    if filtered.is_empty() {
        ui.colored_label(STALE, "No radar site matches that search.");
        return PickerChoice {
            index: 0,
            chosen: None,
        };
    }
    let count = filtered.len();
    let mut index = index.min(count.saturating_sub(1));
    let down = ui.input(|input| input.key_pressed(egui::Key::ArrowDown));
    let up = ui.input(|input| input.key_pressed(egui::Key::ArrowUp));
    if down {
        index = (index + 1) % count;
    }
    if up {
        index = index
            .checked_sub(1)
            .unwrap_or_else(|| count.saturating_sub(1));
    }
    let mut chosen = None;
    egui::ScrollArea::vertical()
        .max_height(PICKER_LIST_HEIGHT)
        .show(ui, |ui| {
            for (position, station) in filtered.iter().enumerate() {
                let selected = position == index;
                let button = egui::Button::new(station.label())
                    .min_size(Vec2::new(width - 20.0, PICKER_ROW_HEIGHT))
                    .fill(if selected { HIGHLIGHT } else { BACKGROUND });
                if ui.add(button).clicked() {
                    chosen = Some(station.id.clone());
                }
            }
        });
    if chosen.is_none() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
        chosen = filtered.get(index).map(|station| station.id.clone());
    }
    PickerChoice { index, chosen }
}

fn scan_status(ui: &mut egui::Ui, frame: &Frame, failed: bool) {
    let scan = frame.volume.scan_unix_ms;
    match time::format_utc(scan) {
        Ok(text) => {
            ui.label(format!("Scan {text}"));
        }
        Err(error) => {
            ui.colored_label(ERROR, format!("Scan time unavailable: {error}"));
        }
    }
    match time::now_unix_ms() {
        Ok(now) if now < scan => {
            ui.colored_label(ERROR, time::age_label(now, scan));
        }
        Ok(now) if failed || now - scan > STALE_AFTER_MS => {
            ui.colored_label(
                STALE,
                format!("Stale radar: {}", time::age_label(now, scan)),
            );
        }
        Ok(now) => {
            ui.label(time::age_label(now, scan));
        }
        Err(error) => {
            ui.colored_label(ERROR, format!("Scan age unavailable: {error}"));
        }
    }
}

fn diagnostics(ui: &mut egui::Ui, frame: &Frame) {
    let volume = &frame.volume;
    let text = format!(
        "{} · {} radials · {} gates · {} below threshold · {} range folded · {:.0} km · tilt {} at {:.2}° · Skipped radials: {} · Other messages: {}",
        volume.site,
        volume.sweep.radials.len(),
        volume.sweep.gate_count(),
        volume.sweep.below_threshold,
        volume.sweep.range_folded,
        volume.sweep.max_ground_range_km,
        volume.sweep.elevation_number,
        volume.sweep.elevation_deg,
        volume.skipped_radials,
        volume.skipped_non_data
    );
    if volume.skipped_radials > 0 || volume.sweep.below_threshold > 0 {
        ui.colored_label(STALE, text);
    } else {
        ui.label(text);
    }
    if let Some(error) = &volume.moment_error {
        ui.colored_label(STALE, format!("Rejected reflectivity radial: {error}"));
    }
}

fn gate_label(value: Option<GateValue>) -> String {
    match value {
        Some(GateValue::Measured(dbz)) => format!("{dbz:.1} dBZ"),
        Some(GateValue::BelowThreshold) => String::from("below threshold"),
        Some(GateValue::RangeFolded) => String::from("range folded"),
        None => String::from("no data"),
    }
}

fn readout(ui: &egui::Ui, frame: &Frame, transform: &ViewTransform, viewport: Rect, bounds: Rect) {
    let Some(position) = ui.input(|input| input.pointer.hover_pos()) else {
        return;
    };
    if !bounds.contains(position) {
        return;
    }
    let painter = ui.painter();
    let info = match transform.pointer_info(position) {
        Ok(info) => info,
        Err(error) => {
            painter.text(
                position,
                Align2::LEFT_TOP,
                format!("Pointer unavailable: {error}"),
                FontId::monospace(12.0),
                ERROR,
            );
            return;
        }
    };
    let (value, beam) = if viewport.contains(position) {
        let hit = frame
            .geometry
            .lookup
            .gate(&frame.volume, info.range_km, info.azimuth_deg);
        hit.map_or_else(
            || (gate_label(None), String::new()),
            |hit| {
                (
                    gate_label(Some(hit.value)),
                    format!("\n{:.2}° beam", hit.elevation_deg),
                )
            },
        )
    } else {
        (String::from("no data"), String::new())
    };
    let label = format!(
        "{:.4}, {:.4}\n{:.1} km  {:.0}°\n{value}{beam}",
        info.lat, info.lon, info.range_km, info.azimuth_deg
    );
    let stroke = Stroke::new(1.0_f32, TEXT);
    painter.line_segment(
        [
            position - Vec2::new(6.0, 0.0),
            position + Vec2::new(6.0, 0.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            position - Vec2::new(0.0, 6.0),
            position + Vec2::new(0.0, 6.0),
        ],
        stroke,
    );
    let galley = painter.layout_no_wrap(label, FontId::monospace(12.0), TEXT);
    let padding = Vec2::splat(6.0);
    let size = galley.size() + 2.0 * padding;
    let x = (position.x + 12.0).min((bounds.right() - size.x).max(bounds.left()));
    let y = (position.y + 12.0).min((bounds.bottom() - size.y).max(bounds.top()));
    let origin = Pos2::new(x, y);
    painter.rect_filled(
        Rect::from_min_size(origin, size),
        CornerRadius::same(4),
        Color32::from_black_alpha(220),
    );
    painter.galley(origin + padding, galley, TEXT);
}

impl eframe::App for OxradApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_workers();
        self.status(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BACKGROUND))
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
