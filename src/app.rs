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
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use eframe::egui;
use egui::Align2;
use egui::Color32;
use egui::CornerRadius;
use egui::FontId;
use egui::Mesh;
use egui::Pos2;
use egui::Rect;
use egui::Shape;
use egui::Stroke;
use egui::Vec2;

use crate::colormap::color_for_reflectivity;
use crate::error::RadarError;
use crate::projection;
use crate::radar::RadarVolume;

const BACKGROUND: Color32 = Color32::from_rgb(8, 10, 14);
const GRID_COLOR: Color32 = Color32::from_rgb(38, 44, 54);
const RING_COLOR: Color32 = Color32::from_rgb(62, 72, 86);
const TEXT_COLOR: Color32 = Color32::from_rgb(206, 214, 226);
const ERROR_COLOR: Color32 = Color32::from_rgb(255, 122, 122);
const CURSOR_DISTANCE_KM: f64 = 100.0;
const RINGS: [f64; 4] = [50.0, 100.0, 150.0, 200.0];
const AZIMUTH_STEPS: u32 = 72;

pub struct OxradApp {
    receiver: Receiver<Result<RadarVolume, RadarError>>,
    volume: Option<RadarVolume>,
    status: String,
    status_is_error: bool,
    receiver_closed: bool,
}

impl OxradApp {
    pub fn new(receiver: Receiver<Result<RadarVolume, RadarError>>) -> Self {
        Self {
            receiver,
            volume: None,
            status: String::from("Fetching the latest volume for KPAH."),
            status_is_error: false,
            receiver_closed: false,
        }
    }

    fn poll_worker(&mut self) {
        if self.receiver_closed {
            return;
        }

        match self.receiver.try_recv() {
            Ok(Ok(volume)) => {
                self.status = format!(
                    "{}: {} bins, {:.0} km range.",
                    volume.site,
                    volume.bins.len(),
                    volume.max_range_km
                );
                self.status_is_error = false;
                self.volume = Some(volume);
            }
            Ok(Err(error)) => {
                self.status = error.to_string();
                self.status_is_error = true;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.receiver_closed = true;
                if self.volume.is_none() {
                    self.status = String::from("The fetch worker stopped without producing a volume.");
                    self.status_is_error = true;
                }
            }
        }
    }

    fn draw_scene(&self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::ZERO, BACKGROUND);

        let Some(volume) = &self.volume else {
            return;
        };
        let Some(transform) = ViewTransform::new(rect, volume) else {
            return;
        };

        draw_grid(painter, &transform);
        draw_rings(painter, volume, &transform);
        draw_bins(painter, volume, &transform);
        draw_cursor(ui, painter, &transform);
    }

    fn draw_status(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let color = if self.status_is_error {
                    ERROR_COLOR
                } else {
                    TEXT_COLOR
                };
                ui.colored_label(color, &self.status);
                if let Some(volume) = &self.volume {
                    ui.separator();
                    ui.label(format!(
                        "skipped radials {}, bins {}, non-data messages {}",
                        volume.skipped_radials, volume.skipped_bins, volume.skipped_non_data
                    ));
                }
            });
        });
    }
}

impl eframe::App for OxradApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_worker();
        self.draw_status(ui);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BACKGROUND))
            .show_inside(ui, |ui| self.draw_scene(ui));

        if self.volume.is_none() && !self.receiver_closed {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }
}

struct ViewTransform {
    center: Pos2,
    rect: Rect,
    pixels_per_km: f64,
    km_per_deg_lat: f64,
    km_per_deg_lon: f64,
    site_lat: f64,
    site_lon: f64,
}

impl ViewTransform {
    fn new(rect: Rect, volume: &RadarVolume) -> Option<Self> {
        let (north_lat, _) = projection::forward(
            CURSOR_DISTANCE_KM,
            0.0,
            volume.site_lat,
            volume.site_lon,
        )
        .ok()?;
        let (_, east_lon) = projection::forward(
            CURSOR_DISTANCE_KM,
            90.0,
            volume.site_lat,
            volume.site_lon,
        )
        .ok()?;

        let km_per_deg_lat = CURSOR_DISTANCE_KM / (north_lat - volume.site_lat);
        let km_per_deg_lon = CURSOR_DISTANCE_KM / (east_lon - volume.site_lon);
        let range_km = volume.max_range_km.max(1.0);
        let pixels_per_km =
            f64::from(rect.width().min(rect.height())) * 0.48 / range_km;

        Some(Self {
            center: rect.center(),
            rect,
            pixels_per_km,
            km_per_deg_lat,
            km_per_deg_lon,
            site_lat: volume.site_lat,
            site_lon: volume.site_lon,
        })
    }

    fn screen(&self, lat: f64, lon: f64) -> Pos2 {
        let east_km = (lon - self.site_lon) * self.km_per_deg_lon;
        let north_km = (lat - self.site_lat) * self.km_per_deg_lat;
        self.center
            + Vec2::new(
                (east_km * self.pixels_per_km) as f32,
                (-north_km * self.pixels_per_km) as f32,
            )
    }

    fn position(&self, position: Pos2) -> (f64, f64) {
        let east_km =
            f64::from(position.x - self.center.x) / self.pixels_per_km;
        let north_km =
            -f64::from(position.y - self.center.y) / self.pixels_per_km;
        (
            self.site_lat + north_km / self.km_per_deg_lat,
            self.site_lon + east_km / self.km_per_deg_lon,
        )
    }
}

fn draw_grid(painter: &egui::Painter, transform: &ViewTransform) {
    let stroke = Stroke::new(1.0_f32, GRID_COLOR);
    for step in -2..=2 {
        let lon = transform.site_lon + f64::from(step);
        let top = transform.screen(transform.site_lat + 2.5, lon);
        let bottom = transform.screen(transform.site_lat - 2.5, lon);
        painter.line_segment([top, bottom], stroke);

        let lat = transform.site_lat + f64::from(step);
        let left = transform.screen(lat, transform.site_lon - 2.5);
        let right = transform.screen(lat, transform.site_lon + 2.5);
        painter.line_segment([left, right], stroke);
    }
}

fn draw_rings(painter: &egui::Painter, volume: &RadarVolume, transform: &ViewTransform) {
    let stroke = Stroke::new(1.0_f32, RING_COLOR);
    for range_km in RINGS {
        if range_km > volume.max_range_km {
            continue;
        }
        let mut previous: Option<Pos2> = None;
        for step in 0..=AZIMUTH_STEPS {
            let azimuth_deg = f64::from(step) * 360.0 / f64::from(AZIMUTH_STEPS);
            let Ok((lat, lon)) = projection::forward(
                range_km,
                azimuth_deg,
                volume.site_lat,
                volume.site_lon,
            ) else {
                previous = None;
                continue;
            };
            let point = transform.screen(lat, lon);
            if let Some(last) = previous {
                painter.line_segment([last, point], stroke);
            }
            previous = Some(point);
        }
    }
}

fn draw_bins(painter: &egui::Painter, volume: &RadarVolume, transform: &ViewTransform) {
    let mut mesh = Mesh::default();
    let mut next_index: u32 = 0;

    for bin in &volume.bins {
        let Ok(rgba) = color_for_reflectivity(bin.dbz) else {
            continue;
        };
        let color = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], 210);
        let [near_left, near_right, far_right, far_left] = bin.corners;

        let p0 = transform.screen(near_left.0, near_left.1);
        let p1 = transform.screen(near_right.0, near_right.1);
        let p2 = transform.screen(far_right.0, far_right.1);
        let p3 = transform.screen(far_left.0, far_left.1);

        mesh.colored_vertex(p0, color);
        mesh.colored_vertex(p1, color);
        mesh.colored_vertex(p2, color);
        mesh.colored_vertex(p3, color);
        mesh.add_triangle(next_index, next_index + 1, next_index + 2);
        mesh.add_triangle(next_index, next_index + 2, next_index + 3);
        next_index += 4;
    }

    painter.add(Shape::mesh(mesh));
}

fn draw_cursor(ui: &egui::Ui, painter: &egui::Painter, transform: &ViewTransform) {
    let Some(position) = ui.input(|input| input.pointer.hover_pos()) else {
        return;
    };
    if !transform.rect.contains(position) {
        return;
    }
    let (lat, lon) = transform.position(position);
    let Ok((range_km, azimuth_deg)) =
        projection::inverse(lat, lon, transform.site_lat, transform.site_lon)
    else {
        return;
    };
    let label = format!("{lat:.4}, {lon:.4}\n{range_km:.1} km  {azimuth_deg:.0} deg");
    painter.text(
        position + Vec2::new(12.0, 12.0),
        Align2::LEFT_TOP,
        label,
        FontId::monospace(12.0),
        TEXT_COLOR,
    );
}

