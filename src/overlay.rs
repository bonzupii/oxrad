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
//! Grid, range rings, and the reflectivity legend.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, Vec2};

use crate::colormap::{MAX_REFLECTIVITY_DBZ, MIN_REFLECTIVITY_DBZ, color_for_reflectivity};
use crate::render::{RadarGeometry, ViewTransform};

const GRID_COLOR: Color32 = Color32::from_rgb(90, 100, 114);
const RING_COLOR: Color32 = Color32::from_rgb(174, 184, 197);
const TEXT_COLOR: Color32 = Color32::from_rgb(206, 214, 226);
const ERROR_COLOR: Color32 = Color32::from_rgb(255, 122, 122);
const LEGEND_WIDTH: f32 = 14.0;
const LEGEND_HEIGHT: f32 = 260.0;
const LEGEND_BANDS: usize = 20;
const RING_LABEL_INDEX: usize = 32;

fn polyline(painter: &egui::Painter, points: &[Pos2], transform: &ViewTransform, stroke: Stroke) {
    for pair in points.windows(2) {
        if let [a, b] = pair {
            painter.line_segment([transform.screen(*a), transform.screen(*b)], stroke);
        }
    }
}

pub fn draw_grid(painter: &egui::Painter, geometry: &RadarGeometry, transform: &ViewTransform) {
    let stroke = Stroke::new(1.0_f32, GRID_COLOR);
    for line in &geometry.grid {
        polyline(painter, line, transform, stroke);
    }
}

pub fn draw_rings(painter: &egui::Painter, geometry: &RadarGeometry, transform: &ViewTransform) {
    let stroke = Stroke::new(1.0_f32, RING_COLOR);
    for ring in &geometry.rings {
        polyline(painter, &ring.points, transform, stroke);
        if let Some(point) = ring.points.get(RING_LABEL_INDEX) {
            painter.text(
                transform.screen(*point) + Vec2::new(4.0, -4.0),
                Align2::LEFT_BOTTOM,
                format!("{:.0} km", ring.range_km),
                FontId::monospace(10.0),
                RING_COLOR,
            );
        }
    }
}

fn legend_colour(fraction: f32) -> Result<[u8; 4], crate::error::ColorMapError> {
    let value = MAX_REFLECTIVITY_DBZ - fraction * (MAX_REFLECTIVITY_DBZ - MIN_REFLECTIVITY_DBZ);
    color_for_reflectivity(value)
}

pub fn draw_legend(painter: &egui::Painter, rect: Rect) {
    let x = rect.right() - 42.0;
    let top = rect.top() + 32.0;
    let height = LEGEND_HEIGHT.min((rect.height() - 72.0).max(20.0));
    let band_height = height / LEGEND_BANDS as f32;
    let range = MAX_REFLECTIVITY_DBZ - MIN_REFLECTIVITY_DBZ;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(x - 42.0, top - 28.0),
            Pos2::new(x + LEGEND_WIDTH + 10.0, top + height + 24.0),
        ),
        CornerRadius::same(4),
        Color32::from_black_alpha(220),
    );
    painter.text(
        Pos2::new(x + LEGEND_WIDTH / 2.0, top - 10.0),
        Align2::CENTER_BOTTOM,
        "dBZ",
        FontId::monospace(11.0),
        TEXT_COLOR,
    );
    for index in 0..LEGEND_BANDS {
        let fraction = (index as f32 + 0.5) / LEGEND_BANDS as f32;
        let rgba = match legend_colour(fraction) {
            Ok(value) => value,
            Err(error) => {
                painter.text(
                    Pos2::new(x, top),
                    Align2::LEFT_TOP,
                    format!("Legend error: {error}"),
                    FontId::monospace(12.0),
                    ERROR_COLOR,
                );
                return;
            }
        };
        let colour = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], 255);
        let y = top + band_height * index as f32;
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(x, y), Vec2::new(LEGEND_WIDTH, band_height)),
            CornerRadius::ZERO,
            colour,
        );
    }
    for (value, label) in [
        (MAX_REFLECTIVITY_DBZ, format!("{MAX_REFLECTIVITY_DBZ:.1}")),
        (75.0, String::from("75")),
        (60.0, String::from("60")),
        (50.0, String::from("50")),
        (35.0, String::from("35")),
        (20.0, String::from("20")),
        (0.0, String::from("0")),
        (MIN_REFLECTIVITY_DBZ, format!("{MIN_REFLECTIVITY_DBZ:.0}")),
    ] {
        let fraction = (MAX_REFLECTIVITY_DBZ - value) / range;
        let y = top + height * fraction;
        painter.text(
            Pos2::new(x - 4.0, y),
            Align2::RIGHT_CENTER,
            label,
            FontId::monospace(10.0),
            TEXT_COLOR,
        );
    }
}
