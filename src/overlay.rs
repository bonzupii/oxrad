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
//! The active product legend.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Vec2};

use crate::colormap::{ColorTable, color_for_product_with};
use crate::product::RadarProduct;
use crate::theme::Palette;
const LEGEND_WIDTH: f32 = 14.0;
const LEGEND_HEIGHT: f32 = 260.0;
const LEGEND_BANDS: usize = 20;

fn legend_colour(
    table: ColorTable,
    product: RadarProduct,
    fraction: f32,
) -> Result<[u8; 4], crate::error::ColorMapError> {
    let (minimum, maximum) = product.value_range();
    let value = maximum - fraction * (maximum - minimum);
    color_for_product_with(table, product, value)
}

pub fn draw_legend(
    painter: &egui::Painter,
    rect: Rect,
    product: RadarProduct,
    table: ColorTable,
    palette: &Palette,
) {
    let (minimum, maximum) = product.value_range();
    let range = maximum - minimum;
    let x = rect.right() - 42.0;
    let top = rect.top() + 32.0;
    let height = LEGEND_HEIGHT.min((rect.height() - 72.0).max(20.0));
    let band_height = height / LEGEND_BANDS as f32;
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(x - 42.0, top - 28.0),
            Pos2::new(x + LEGEND_WIDTH + 10.0, top + height + 24.0),
        ),
        CornerRadius::same(4),
        Color32::from_rgba_unmultiplied(
            palette.legend.r(),
            palette.legend.g(),
            palette.legend.b(),
            220,
        ),
    );
    painter.text(
        Pos2::new(x + LEGEND_WIDTH / 2.0, top - 10.0),
        Align2::CENTER_BOTTOM,
        format!("{} {}", product.id(), product.unit()),
        FontId::monospace(10.0),
        palette.canvas_text,
    );
    painter.text(
        Pos2::new(x + LEGEND_WIDTH / 2.0, top + height + 12.0),
        Align2::CENTER_TOP,
        product.scale_label(),
        FontId::monospace(9.0),
        palette.canvas_text,
    );
    for index in 0..LEGEND_BANDS {
        let fraction = (index as f32 + 0.5) / LEGEND_BANDS as f32;
        let rgba = match legend_colour(table, product, fraction) {
            Ok(value) => value,
            Err(error) => {
                painter.text(
                    Pos2::new(x, top),
                    Align2::LEFT_TOP,
                    format!("Legend error: {error}"),
                    FontId::monospace(12.0),
                    palette.error,
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
    for fraction in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
        let value = maximum - fraction * range;
        let y = top + height * fraction;
        painter.text(
            Pos2::new(x - 4.0, y),
            Align2::RIGHT_CENTER,
            format!("{value:.1}"),
            FontId::monospace(10.0),
            palette.canvas_text,
        );
    }
}
