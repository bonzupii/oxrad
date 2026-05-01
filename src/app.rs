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
use std::sync::mpsc;

use eframe::egui;

use crate::colormap;
use crate::error::RadarError;
use crate::error::RenderError;
use crate::projection;
use crate::radar::RadarBin;
use crate::radar::RadarMessage;
use crate::radar::RadarVolume;

const LAT_GRID_SPACING: f64 = 0.5;
const LON_GRID_SPACING: f64 = 0.5;
const RANGE_RING_SPACING_KM: f64 = 50.0;
const RANGE_RING_SEGMENTS: usize = 360;
const BLACK_DBZ_MAX: f32 = -20.0;
const MESH_BATCH_VERTICES: usize = 65536;

struct BinRenderFrame {
    site_lat: f64,
    site_lon: f64,
    center_lat: f64,
    center_lon: f64,
    lat_span: f64,
    lon_span: f64,
    screen: egui::Rect,
}

pub struct OxradApp {
    rx: mpsc::Receiver<RadarMessage>,
    fetch_handle: Option<std::thread::JoinHandle<Result<(), RadarError>>>,
    volume: Option<RadarVolume>,
    status: String,
    skipped_bins: usize,
    first_render_error: Option<RenderError>,
    center_lat: f64,
    center_lon: f64,
    lat_span: f64,
    lon_span: f64,
    cursor_info: String,
}

impl OxradApp {
    pub fn new(
        rx: mpsc::Receiver<RadarMessage>,
        fetch_handle: std::thread::JoinHandle<Result<(), RadarError>>,
    ) -> Self {
        OxradApp {
            rx,
            fetch_handle: Some(fetch_handle),
            volume: None,
            status: String::from("Fetching radar data..."),
            skipped_bins: 0,
            first_render_error: None,
            center_lat: 0.0,
            center_lon: 0.0,
            lat_span: 3.0,
            lon_span: 4.0,
            cursor_info: String::new(),
        }
    }

    fn status_is_error(&self) -> bool {
        self.status.starts_with("Error") || self.status.starts_with("Render error")
    }

    fn site_coords(&self) -> Option<(f64, f64)> {
        self.volume.as_ref().map(|vol| (vol.site_lat, vol.site_lon))
    }

    fn poll_receiver(&mut self) {
        match self.rx.try_recv() {
            Ok(RadarMessage::Volume(vol)) => {
                let sweep_count = vol.sweeps.len();
                self.center_lat = vol.site_lat;
                self.center_lon = vol.site_lon;
                self.volume = Some(vol);
                self.status = format!("Loaded {sweep_count} sweep(s)");
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                if !self.status_is_error() {
                    self.status = String::from("Data channel disconnected");
                }
            }
        }

        let finished = match self.fetch_handle {
            Some(ref handle) => handle.is_finished(),
            None => false,
        };
        if !finished {
            return;
        }

        let handle = match self.fetch_handle.take() {
            Some(handle) => handle,
            None => return,
        };
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                self.status = format!("Error: {e}");
            }
            Err(join_err) => {
                let message = match join_err.downcast::<String>() {
                    Ok(text) => format!("Error: Radar fetch thread panicked: {text}"),
                    Err(join_err) => match join_err.downcast::<&'static str>() {
                        Ok(text) => format!("Error: Radar fetch thread panicked: {text}"),
                        Err(unhandled) => {
                            let payload_type = std::any::type_name_of_val(&unhandled);
                            format!("Error: Radar fetch thread panicked with {payload_type}")
                        }
                    },
                };
                self.status = message;
            }
        }
    }

    fn geo_to_screen(
        lat: f64,
        lon: f64,
        center_lat: f64,
        center_lon: f64,
        lat_span: f64,
        lon_span: f64,
        screen: egui::Rect,
    ) -> (f32, f32) {
        let x = screen.width() as f64 * (lon - center_lon) / lon_span + screen.width() as f64 / 2.0;
        let y =
            screen.height() as f64 / 2.0 - screen.height() as f64 * (lat - center_lat) / lat_span;
        (
            (screen.left() as f64 + x) as f32,
            (screen.top() as f64 + y) as f32,
        )
    }

    fn screen_to_geo(
        sx: f32,
        sy: f32,
        center_lat: f64,
        center_lon: f64,
        lat_span: f64,
        lon_span: f64,
        screen: egui::Rect,
    ) -> (f64, f64) {
        let x = sx as f64 - screen.left() as f64;
        let y = sy as f64 - screen.top() as f64;
        let lon = center_lon + (x - screen.width() as f64 / 2.0) * lon_span / screen.width() as f64;
        let lat =
            center_lat + (screen.height() as f64 / 2.0 - y) * lat_span / screen.height() as f64;
        (lat, lon)
    }

    fn draw_radar_bins(&mut self, painter: &egui::Painter, screen: egui::Rect) {
        let (site_lat, site_lon, sweep) = match self.volume {
            Some(ref vol) => match vol.sweeps.first() {
                Some(sweep) => (vol.site_lat, vol.site_lon, sweep),
                None => return,
            },
            None => return,
        };

        let frame = BinRenderFrame {
            site_lat,
            site_lon,
            center_lat: self.center_lat,
            center_lon: self.center_lon,
            lat_span: self.lat_span,
            lon_span: self.lon_span,
            screen,
        };

        let mut mesh = egui::Mesh::default();
        for bin in &sweep.bins {
            if bin.range_start_km + bin.range_width_km <= 0.0 {
                continue;
            }
            if !bin.dbz.is_nan() && bin.dbz < BLACK_DBZ_MAX {
                continue;
            }
            match append_bin_to_mesh(bin, &frame, &mut mesh) {
                Ok(()) => {
                    if mesh.vertices.len() >= MESH_BATCH_VERTICES {
                        painter.add(egui::Shape::Mesh(mesh.into()));
                        mesh = egui::Mesh::default();
                    }
                }
                Err(e) => {
                    self.skipped_bins += 1;
                    if self.first_render_error.is_none() {
                        self.first_render_error = Some(e);
                    }
                }
            }
        }
        if !mesh.vertices.is_empty() {
            painter.add(egui::Shape::Mesh(mesh.into()));
        }
    }

    fn draw_range_rings(&mut self, painter: &egui::Painter, screen: egui::Rect) {
        let (site_lat, site_lon, max_range_km) = match self.volume {
            Some(ref vol) => match vol.sweeps.first() {
                Some(sweep) => (vol.site_lat, vol.site_lon, sweep.max_range_km),
                None => return,
            },
            None => return,
        };
        let mut range_km = RANGE_RING_SPACING_KM;
        while range_km <= max_range_km {
            let mut prev: Option<egui::Pos2> = None;
            for i in 0..=RANGE_RING_SEGMENTS {
                let az_deg = 360.0 * i as f64 / RANGE_RING_SEGMENTS as f64;
                let (lat, lon) = match projection::forward(range_km, az_deg, site_lat, site_lon) {
                    Ok(coords) => coords,
                    Err(e) => {
                        self.skipped_bins += 1;
                        if self.first_render_error.is_none() {
                            self.first_render_error = Some(RenderError::RangeRing {
                                range_km,
                                az_deg,
                                source: e,
                            });
                        }
                        prev = None;
                        continue;
                    }
                };
                let (sx, sy) = Self::geo_to_screen(
                    lat,
                    lon,
                    self.center_lat,
                    self.center_lon,
                    self.lat_span,
                    self.lon_span,
                    screen,
                );
                let curr = egui::Pos2::new(sx, sy);
                if let Some(p) = prev {
                    painter.line_segment(
                        [p, curr],
                        egui::Stroke::new(0.5, egui::Color32::from_rgb(60, 60, 60)),
                    );
                }
                prev = Some(curr);
            }
            let label_pos = match projection::forward(range_km, 0.0, site_lat, site_lon) {
                Ok((lat, lon)) => {
                    let (sx, sy) = Self::geo_to_screen(
                        lat,
                        lon,
                        self.center_lat,
                        self.center_lon,
                        self.lat_span,
                        self.lon_span,
                        screen,
                    );
                    egui::Pos2::new(sx, sy)
                }
                Err(e) => {
                    self.skipped_bins += 1;
                    if self.first_render_error.is_none() {
                        self.first_render_error = Some(RenderError::RangeRing {
                            range_km,
                            az_deg: 0.0,
                            source: e,
                        });
                    }
                    range_km += RANGE_RING_SPACING_KM;
                    continue;
                }
            };
            painter.text(
                label_pos,
                egui::Align2::LEFT_BOTTOM,
                format!("{range_km:.0} km"),
                egui::FontId::proportional(9.0),
                egui::Color32::from_rgb(100, 100, 100),
            );
            range_km += RANGE_RING_SPACING_KM;
        }
    }

    fn draw_grid(&self, painter: &egui::Painter, screen: egui::Rect) {
        let lat_min = self.center_lat - self.lat_span / 2.0;
        let lat_max = self.center_lat + self.lat_span / 2.0;
        let lon_min = self.center_lon - self.lon_span / 2.0;
        let lon_max = self.center_lon + self.lon_span / 2.0;

        let mut lat = (lat_min / LAT_GRID_SPACING).floor() * LAT_GRID_SPACING;
        while lat <= lat_max {
            let (x1, y1) = Self::geo_to_screen(
                lat,
                lon_min,
                self.center_lat,
                self.center_lon,
                self.lat_span,
                self.lon_span,
                screen,
            );
            let (x2, y2) = Self::geo_to_screen(
                lat,
                lon_max,
                self.center_lat,
                self.center_lon,
                self.lat_span,
                self.lon_span,
                screen,
            );
            let p1 = egui::Pos2::new(x1, y1);
            let p2 = egui::Pos2::new(x2, y2);
            painter.line_segment(
                [p1, p2],
                egui::Stroke::new(0.5, egui::Color32::from_rgb(80, 80, 80)),
            );
            painter.text(
                egui::Pos2::new(x1, y1),
                egui::Align2::RIGHT_BOTTOM,
                format!("{lat:.1}"),
                egui::FontId::proportional(10.0),
                egui::Color32::GRAY,
            );
            lat += LAT_GRID_SPACING;
        }

        let mut lon = (lon_min / LON_GRID_SPACING).floor() * LON_GRID_SPACING;
        while lon <= lon_max {
            let (x1, y1) = Self::geo_to_screen(
                lat_min,
                lon,
                self.center_lat,
                self.center_lon,
                self.lat_span,
                self.lon_span,
                screen,
            );
            let (x2, y2) = Self::geo_to_screen(
                lat_max,
                lon,
                self.center_lat,
                self.center_lon,
                self.lat_span,
                self.lon_span,
                screen,
            );
            let p1 = egui::Pos2::new(x1, y1);
            let p2 = egui::Pos2::new(x2, y2);
            painter.line_segment(
                [p1, p2],
                egui::Stroke::new(0.5, egui::Color32::from_rgb(80, 80, 80)),
            );
            painter.text(
                egui::Pos2::new(x2, y2),
                egui::Align2::RIGHT_BOTTOM,
                format!("{lon:.1}"),
                egui::FontId::proportional(10.0),
                egui::Color32::GRAY,
            );
            lon += LON_GRID_SPACING;
        }
    }
}

fn append_bin_to_mesh(
    bin: &RadarBin,
    frame: &BinRenderFrame,
    mesh: &mut egui::Mesh,
) -> Result<(), RenderError> {
    let [r, g, b, _] =
        colormap::map_value(colormap::Product::BaseReflectivity, bin.dbz).map_err(|e| {
            RenderError::ColorMap {
                dbz: bin.dbz,
                source: e,
            }
        })?;
    let color = egui::Color32::from_rgb(r, g, b);

    let az_center = bin.azimuth_deg as f64;
    let az_width = bin.azimuth_width_deg as f64;
    let range_near_km = bin.range_start_km as f64;
    let range_far_km = (bin.range_start_km + bin.range_width_km) as f64;

    let az_a = ((az_center - az_width / 2.0) % 360.0 + 360.0) % 360.0;
    let az_b = ((az_center + az_width / 2.0) % 360.0 + 360.0) % 360.0;

    let (lat_a, lon_a) = projection::forward(range_near_km, az_a, frame.site_lat, frame.site_lon)
        .map_err(|e| RenderError::BinCorner {
        corner: "A",
        source: e,
    })?;
    let (lat_b, lon_b) = projection::forward(range_near_km, az_b, frame.site_lat, frame.site_lon)
        .map_err(|e| RenderError::BinCorner {
        corner: "B",
        source: e,
    })?;
    let (lat_c, lon_c) = projection::forward(range_far_km, az_b, frame.site_lat, frame.site_lon)
        .map_err(|e| RenderError::BinCorner {
            corner: "C",
            source: e,
        })?;
    let (lat_d, lon_d) = projection::forward(range_far_km, az_a, frame.site_lat, frame.site_lon)
        .map_err(|e| RenderError::BinCorner {
            corner: "D",
            source: e,
        })?;

    let pa = OxradApp::geo_to_screen(
        lat_a,
        lon_a,
        frame.center_lat,
        frame.center_lon,
        frame.lat_span,
        frame.lon_span,
        frame.screen,
    );
    let pb = OxradApp::geo_to_screen(
        lat_b,
        lon_b,
        frame.center_lat,
        frame.center_lon,
        frame.lat_span,
        frame.lon_span,
        frame.screen,
    );
    let pc = OxradApp::geo_to_screen(
        lat_c,
        lon_c,
        frame.center_lat,
        frame.center_lon,
        frame.lat_span,
        frame.lon_span,
        frame.screen,
    );
    let pd = OxradApp::geo_to_screen(
        lat_d,
        lon_d,
        frame.center_lat,
        frame.center_lon,
        frame.lat_span,
        frame.lon_span,
        frame.screen,
    );

    let base = mesh.vertices.len() as u32;
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::Pos2::new(pa.0, pa.1),
        uv: egui::Pos2::ZERO,
        color,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::Pos2::new(pb.0, pb.1),
        uv: egui::Pos2::ZERO,
        color,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::Pos2::new(pc.0, pc.1),
        uv: egui::Pos2::ZERO,
        color,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: egui::Pos2::new(pd.0, pd.1),
        uv: egui::Pos2::ZERO,
        color,
    });

    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    Ok(())
}

impl eframe::App for OxradApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_receiver();
        self.skipped_bins = 0;
        self.first_render_error = None;

        egui::Panel::bottom("status_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    if self.status_is_error() {
                        egui::Color32::RED
                    } else {
                        egui::Color32::LIGHT_GRAY
                    },
                    &self.status,
                );
                if self.skipped_bins > 0 {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!("Skipped {} render items", self.skipped_bins),
                    );
                }
                if let Some(ref volume) = self.volume {
                    if volume.skipped_radials > 0 {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            format!("Skipped {} radials", volume.skipped_radials),
                        );
                    } else {
                        ui.colored_label(egui::Color32::LIGHT_GRAY, "Skipped 0 radials");
                    }
                    if volume.skipped_non_data_messages > 0 {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            format!(
                                "Skipped {} non-data messages",
                                volume.skipped_non_data_messages
                            ),
                        );
                    } else {
                        ui.colored_label(egui::Color32::LIGHT_GRAY, "Skipped 0 non-data messages");
                    }
                }
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let painter = ui.painter();
            let rect = ui.max_rect();

            painter.rect_filled(rect, 0.0, egui::Color32::BLACK);

            self.draw_range_rings(painter, rect);
            self.draw_radar_bins(painter, rect);
            self.draw_grid(painter, rect);

            if self.skipped_bins > 0
                && !self.status_is_error()
                && let Some(ref e) = self.first_render_error
            {
                self.status = format!("Render error: {e}");
            }

            if let Some(ref volume) = self.volume {
                let site_lat = volume.site_lat;
                let site_lon = volume.site_lon;
                let (sx, sy) = Self::geo_to_screen(
                    site_lat,
                    site_lon,
                    self.center_lat,
                    self.center_lon,
                    self.lat_span,
                    self.lon_span,
                    rect,
                );
                painter.circle_filled(egui::Pos2::new(sx, sy), 3.0, egui::Color32::YELLOW);
                painter.text(
                    egui::Pos2::new(sx, sy - 8.0),
                    egui::Align2::CENTER_BOTTOM,
                    &volume.site,
                    egui::FontId::proportional(12.0),
                    egui::Color32::YELLOW,
                );
            }

            let hover_pos = ui.input(|i| i.pointer.hover_pos());
            if let Some(pos) = hover_pos
                && rect.contains(pos)
            {
                let (lat, lon) = Self::screen_to_geo(
                    pos.x,
                    pos.y,
                    self.center_lat,
                    self.center_lon,
                    self.lat_span,
                    self.lon_span,
                    rect,
                );
                self.cursor_info = match self.site_coords() {
                    Some((site_lat, site_lon)) => {
                        match projection::inverse(lat, lon, site_lat, site_lon) {
                            Ok((range_km, az_deg)) => {
                                format!("{lat:.4} {lon:.4} {range_km:.1} km @ {az_deg:.1} deg")
                            }
                            Err(e) => format!("{lat:.4} {lon:.4} inverse error: {e}"),
                        }
                    }
                    None => {
                        format!("{lat:.4} {lon:.4}")
                    }
                };
            }
        });

        egui::Panel::bottom("cursor_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::LIGHT_GRAY, &self.cursor_info);
            });
        });

        ui.ctx().request_repaint();
    }
}
