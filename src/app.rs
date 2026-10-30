// oxrad: A storm-chaser-grade NEXRAD weather radar viewer
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
use std::time::Duration;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Shape, Stroke, Vec2};

use crate::display::{Display, DisplayState, Frame};
use crate::gpu::RadarGpu;
use crate::overlay;
use crate::pipeline::{Operation, Pipeline, PipelineState};
use crate::radar::GateValue;
use crate::render::{self, ViewTransform};
use crate::time;

const BACKGROUND: Color32 = Color32::from_rgb(8, 10, 14);
const TEXT: Color32 = Color32::from_rgb(206, 214, 226);
const ERROR: Color32 = Color32::from_rgb(255, 122, 122);
const STALE: Color32 = Color32::from_rgb(255, 196, 64);
const STALE_AFTER_MS: u64 = 15 * 60 * 1000;

pub struct OxradApp {
    site: String,
    pipeline: Pipeline,
    display: Display,
}

impl OxradApp {
    pub fn new(site: String, gpu: Option<RadarGpu>) -> Self {
        Self {
            pipeline: Pipeline::start(site.clone()),
            display: Display::new(gpu),
            site,
        }
    }

    fn update_workers(&mut self) {
        if let Some(volume) = self.pipeline.poll() {
            self.display.submit(volume);
        }
        self.display.poll();
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("{} · Level II · Base reflectivity", self.site));
                    ui.separator();
                    ui.label(phase_label(
                        &self.pipeline.state,
                        &self.display.state,
                        self.display.frame.is_some(),
                        self.display.gpu_error.is_some(),
                    ));
                    if let Some(frame) = &self.display.frame {
                        draw_frame_status(
                            ui,
                            frame,
                            self.pipeline.state.failure().is_some()
                                || matches!(self.pipeline.state, PipelineState::Stopped(_))
                                || self.display.gpu_error.is_some()
                                || matches!(
                                    self.display.state,
                                    DisplayState::Failed { .. } | DisplayState::Unavailable(_)
                                ),
                        );
                    }
                });
                let mut retry_fetch = false;
                if let Some(failure) = self.pipeline.state.failure() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            ERROR,
                            format!(
                                "{} failed for {}: {}",
                                failure.operation.description(),
                                self.site,
                                failure.error
                            ),
                        );
                        if let Some(wait) = failure.retry_in {
                            ui.label(format!("Retry scheduled after {} s", wait.as_secs()));
                        }
                        if failure.waiting_for_next_volume {
                            ui.label("Waiting for a new volume or manual retry.");
                        }
                        retry_fetch = ui.button("Retry").clicked();
                    });
                }
                if retry_fetch && let Err(error) = self.pipeline.retry() {
                    self.pipeline.state = PipelineState::Stopped(error);
                }
                if let PipelineState::Stopped(error) = &self.pipeline.state {
                    ui.colored_label(ERROR, format!("{}: {error}", self.site));
                }
                if let Some(error) = &self.display.gpu_error {
                    ui.colored_label(ERROR, format!("{}: {error}", self.site));
                }
                let mut retry_render = false;
                match &self.display.state {
                    DisplayState::Failed { error, .. } => {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(ERROR, format!("{}: {error}", self.site));
                            retry_render = ui.button("Retry render").clicked();
                        });
                    }
                    DisplayState::Unavailable(error) => {
                        ui.colored_label(ERROR, format!("{}: {error}", self.site));
                    }
                    DisplayState::Waiting | DisplayState::Preparing(_) | DisplayState::Ready => {}
                }
                if retry_render {
                    self.display.retry();
                }
                if let Some(frame) = &self.display.frame {
                    draw_diagnostics(ui, frame);
                }
            });
        });
    }

    fn scene(&self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
        let Some(frame) = &self.display.frame else {
            let message = if self.pipeline.state.failure().is_some()
                || matches!(self.pipeline.state, PipelineState::Stopped(_))
                || self.display.gpu_error.is_some()
                || matches!(
                    self.display.state,
                    DisplayState::Failed { .. } | DisplayState::Unavailable(_)
                ) {
                "Radar image unavailable"
            } else {
                "Waiting for radar data"
            };
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                message,
                FontId::proportional(16.0),
                TEXT,
            );
            return;
        };
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let viewport = render::radar_viewport(rect);
        let transform = frame.cache.transform.for_viewport(viewport);
        let mut shape = frame.shape.clone();
        if let Shape::Callback(callback) = &mut shape {
            callback.rect = viewport;
        }
        painter.add(shape);
        if frame.volume.bins.len()
            == frame.volume.below_threshold_bins + frame.volume.range_folded_bins
        {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "No measured echo in this sweep; this does not establish clear weather.",
                FontId::proportional(16.0),
                TEXT,
            );
        }
        overlay::draw_grid(painter, &frame.cache, viewport);
        overlay::draw_rings(painter, &frame.cache, viewport);
        overlay::draw_legend(painter, rect);
        draw_cursor(ui, frame, &transform, viewport, rect);
    }
}

impl eframe::App for OxradApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_workers();
        self.status(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BACKGROUND))
            .show_inside(ui, |ui| self.scene(ui));
        let interval = if matches!(self.display.state, DisplayState::Preparing(_))
            || self.display.frame.is_none()
        {
            100
        } else {
            1000
        };
        ui.ctx()
            .request_repaint_after(Duration::from_millis(interval));
    }
}

const fn phase_label(
    pipeline: &PipelineState,
    display: &DisplayState,
    has_frame: bool,
    gpu_failed: bool,
) -> &'static str {
    if matches!(
        pipeline,
        PipelineState::Failed(_) | PipelineState::Stopped(_)
    ) || pipeline.failure().is_some()
    {
        return "Error";
    }
    if gpu_failed
        || matches!(
            display,
            DisplayState::Failed { .. } | DisplayState::Unavailable(_)
        )
    {
        return "Render error";
    }
    match (pipeline, display, has_frame) {
        (
            PipelineState::Working {
                operation: Operation::Fetching,
                ..
            },
            _,
            true,
        ) => "Refreshing · fetching",
        (
            PipelineState::Working {
                operation: Operation::Decoding,
                ..
            },
            _,
            true,
        ) => "Refreshing · decoding",
        (
            PipelineState::Working {
                operation: Operation::Fetching,
                ..
            },
            _,
            false,
        ) => "Fetching",
        (
            PipelineState::Working {
                operation: Operation::Decoding,
                ..
            },
            _,
            false,
        ) => "Decoding",
        (PipelineState::Ready, DisplayState::Preparing(_), true) => "Refreshing · preparing frame",
        (PipelineState::Ready, DisplayState::Preparing(_) | DisplayState::Waiting, false) => {
            "Preparing frame"
        }
        (PipelineState::Ready, DisplayState::Ready, true) => "Ready",
        _ => "Radar unavailable",
    }
}

fn draw_frame_status(ui: &mut egui::Ui, frame: &Frame, failed: bool) {
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

fn draw_diagnostics(ui: &mut egui::Ui, frame: &Frame) {
    let volume = &frame.volume;
    let text = format!(
        "{} gates · {} below threshold · {} range folded · {:.0} km · Skipped radials: {} · Other messages: {}",
        volume.bins.len(),
        volume.below_threshold_bins,
        volume.range_folded_bins,
        volume.max_range_km,
        volume.skipped_radials,
        volume.skipped_non_data
    );
    if volume.skipped_radials > 0 {
        ui.colored_label(STALE, text);
    } else {
        ui.label(text);
    }
    if let Some(error) = &volume.moment_error {
        ui.colored_label(STALE, format!("Rejected reflectivity radial: {error}"));
    }
}

fn draw_cursor(
    ui: &egui::Ui,
    frame: &Frame,
    transform: &ViewTransform,
    viewport: Rect,
    bounds: Rect,
) {
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
    let canonical = frame.cache.rect.center()
        + (position - viewport.center()) * (frame.cache.rect.width() / viewport.width());
    let value = match frame.cache.hover.find(canonical) {
        Some(GateValue::Measured(dbz)) => format!("{dbz:.1} dBZ"),
        Some(GateValue::BelowThreshold) => String::from("below threshold"),
        Some(GateValue::RangeFolded) => String::from("range folded"),
        None => String::from("no data"),
    };
    let label = format!(
        "{:.4}, {:.4}\n{:.1} km  {:.0}°\n{}",
        info.lat, info.lon, info.range_km, info.azimuth_deg, value
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
