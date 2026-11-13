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
//! Canvas geometry, colours and small controls shared by the map and radar
//! panels. Nothing here holds state.

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, Vec2,
};

use crate::colormap::ColorTable;
use crate::display::Display;
use crate::gpu::RadarGpu;
use crate::map;
use crate::product::RadarProduct;
use crate::radar::GateValue;
use crate::render;
use crate::theme;
use crate::warnings::Severity;

pub(crate) const TEXT: Color32 = Color32::from_rgb(214, 221, 232);
pub(crate) const MUTED: Color32 = Color32::from_rgb(156, 168, 184);
pub(crate) const ERROR: Color32 = Color32::from_rgb(255, 122, 122);
pub(crate) const STALE: Color32 = Color32::from_rgb(255, 196, 64);
pub(crate) const STALE_AFTER_MS: u64 = 15 * 60 * 1000;
const LEGEND_STRIP: f32 = 92.0;

/// Which geographic canvas received a pointer gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Canvas {
    Map,
    Radar,
}

pub(crate) fn due_after(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}

pub(crate) const fn scans_align(reference_ms: u64, scan_ms: u64) -> bool {
    reference_ms.abs_diff(scan_ms) <= 6 * 60 * 1000
}

pub(crate) const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Extreme => 4,
        Severity::Severe => 3,
        Severity::Moderate => 2,
        Severity::Minor => 1,
        Severity::Unknown => 0,
    }
}

pub(crate) const fn warning_colour(severity: Severity) -> Color32 {
    match severity {
        Severity::Extreme => Color32::from_rgb(255, 58, 58),
        Severity::Severe => Color32::from_rgb(255, 138, 32),
        Severity::Moderate => Color32::from_rgb(255, 214, 64),
        Severity::Minor => Color32::from_rgb(100, 190, 255),
        Severity::Unknown => Color32::from_rgb(190, 198, 210),
    }
}

pub(crate) fn toggle_button(
    palette: &crate::theme::Palette,
    label: &str,
    selected: bool,
) -> egui::Button<'static> {
    let fill = if selected {
        palette.accent
    } else {
        palette.surface
    };
    let text = if selected {
        Color32::WHITE
    } else {
        palette.text
    };
    egui::Button::new(RichText::new(label).size(theme::TYPE_BODY).color(text))
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, palette.border))
}

/// Draws the source, operation, cache state, request URL, credit and licence
/// for one served value, so a readout can account for itself.
pub(crate) fn provenance(
    ui: &mut egui::Ui,
    provenance: &crate::engine::Provenance,
    cache: crate::engine::CacheState,
) {
    ui.label(
        RichText::new(format!(
            "{} · operation {} · cache {}",
            provenance.source_name,
            provenance
                .operation
                .map_or("not named", |operation| operation),
            cache.label()
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.label(
        RichText::new(&provenance.url)
            .size(theme::TYPE_SMALL)
            .weak(),
    );
    ui.label(
        RichText::new(format!("{} · {}", provenance.credit, provenance.licence))
            .size(theme::TYPE_SMALL)
            .weak(),
    );
}

pub(crate) fn action_button(palette: &crate::theme::Palette, label: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label)
            .size(theme::TYPE_BODY)
            .color(palette.text),
    )
    .fill(palette.surface)
    .stroke(Stroke::new(1.0_f32, palette.border))
}

pub(crate) fn color_table_selector(ui: &mut egui::Ui, selected: ColorTable) -> ColorTable {
    let mut chosen = selected;
    let response = egui::ComboBox::from_id_salt("color-table")
        .selected_text(format!("Colors · {}", selected.label()))
        .width(126.0)
        .show_ui(ui, |ui| {
            for table in ColorTable::ALL {
                ui.selectable_value(&mut chosen, table, table.label())
                    .on_hover_text(table.description());
            }
        });
    response
        .response
        .on_hover_text("Radar color table, including options for color-vision deficiencies.");
    chosen
}

/// The map's title panel, pinned to the canvas's top-left corner.
pub(crate) fn map_header_rect(rect: Rect) -> Rect {
    Rect::from_min_size(
        rect.left_top() + Vec2::new(8.0, 8.0),
        Vec2::new((rect.width() - 24.0).clamp(0.0, 470.0), 58.0),
    )
}

/// The layer and warning detail block, stacked directly under the title so the
/// centre of the map keeps showing terrain instead of text.
pub(crate) fn map_status_area(rect: Rect) -> Rect {
    let header = map_header_rect(rect);
    let top = (header.bottom() + 6.0).min((rect.bottom() - 8.0).max(rect.top()));
    let height = (rect.bottom() - top - 8.0).clamp(0.0, 110.0);
    Rect::from_min_size(
        Pos2::new(header.left(), top),
        Vec2::new(header.width(), height),
    )
}

/// The band the scale bar occupies, bottom-left of the canvas.
fn scale_band(rect: Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(rect.left() + 8.0, rect.bottom() - 54.0),
        Vec2::new(190.0, 52.0),
    )
}

/// The bands the canvas's own chrome occupies, top-left and bottom-left, so
/// place labels do not print through the scan caption, the map's status panel,
/// or the scale bar.
pub(crate) fn chrome_obstacles(rect: Rect, canvas: Canvas) -> [Rect; 2] {
    match canvas {
        Canvas::Map => [map_header_rect(rect), map_status_area(rect)],
        Canvas::Radar => {
            let caption_width = (rect.width().min(470.0) - 8.0).max(0.0);
            let caption = Rect::from_min_size(
                Pos2::new(rect.left() + 8.0, rect.top() + 2.0),
                Vec2::new(caption_width, 20.0),
            );
            [caption, scale_band(rect)]
        }
    }
}

/// Splits a canvas into the map and a strip for the colour legend.
///
/// The legend used to be painted over the map's right edge, where it covered
/// the echo and the place names beside it. The strip is wide enough for the
/// legend's backing panel and is filled separately, so neither the map nor its
/// labels can end up underneath it.
pub(crate) fn legend_split(rect: Rect, reserved: bool) -> (Rect, Option<Rect>) {
    let width = LEGEND_STRIP.min(rect.width() / 3.0);
    if !reserved || rect.width() <= width {
        return (rect, None);
    }
    let divide = rect.right() - width;
    let map = Rect::from_min_max(rect.min, Pos2::new(divide, rect.bottom()));
    let legend = Rect::from_min_max(Pos2::new(divide, rect.top()), rect.max);
    (map, Some(legend))
}

pub(crate) fn map_caption(painter: &egui::Painter, rect: Rect, frame: &crate::display::Frame) {
    let product = frame.geometry.product;
    let label = format!(
        "{} · {} ({}) · tilt {} at {:.2}°",
        frame.volume.site,
        product.label(),
        product.id(),
        frame.volume.sweep.elevation_number,
        frame.volume.sweep.elevation_deg
    );
    let position = Pos2::new(rect.left() + 16.0, rect.top() + 14.0);
    let galley = painter.layout_no_wrap(label, FontId::proportional(12.0), TEXT);
    let background = Rect::from_min_size(position, galley.size()).expand2(Vec2::new(5.0, 3.0));
    let painter = painter.with_clip_rect(rect);
    painter.rect_filled(background, 3.0, Color32::from_black_alpha(220));
    painter.galley(position, galley, TEXT);
}

pub(crate) fn north_marker(painter: &egui::Painter, rect: Rect) {
    let x = rect.center().x;
    let top = rect.top() + 12.0;
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(x - 9.0, top - 3.0), Vec2::new(18.0, 36.0)),
        3.0,
        Color32::from_black_alpha(220),
    );
    painter.text(
        Pos2::new(x, top),
        Align2::CENTER_TOP,
        "N",
        FontId::monospace(12.0),
        TEXT,
    );
    painter.line_segment(
        [Pos2::new(x, top + 16.0), Pos2::new(x, top + 30.0)],
        Stroke::new(1.0_f32, TEXT),
    );
}

/// Draws one display's raster into a geographic canvas, from the same
/// transform that places the rings and the readout.
pub(crate) fn draw_map_display(
    display: &Display,
    gpu: &RadarGpu,
    painter: &egui::Painter,
    rect: Rect,
    transform: &map::MapTransform,
    span_km: f64,
) -> Result<bool, crate::error::RenderError> {
    let Some(frame) = &display.frame else {
        return Ok(false);
    };
    let Some(site) = transform.screen(frame.geometry.site_lat, frame.geometry.site_lon) else {
        return Ok(false);
    };
    let placement = render::Placement::new(site, rect, span_km)
        .map_err(|source| crate::error::RenderError::Coordinate { source })?;
    if let Some(shape) = frame.shape(gpu, rect, placement) {
        painter.add(shape);
    }
    Ok(true)
}

/// What an empty frame means for the product that was drawn.
///
/// A measured sweep with no gate above the scale says nothing about the weather
/// in the gaps, and a computed column below the scale says the same about the
/// sky. Either way an empty frame is not a clear-weather report.
/// Drag to pan and scroll to zoom, on whichever canvas the pointer is in.
pub(crate) fn canvas_input(
    ui: &egui::Ui,
    rect: Rect,
    interaction: &egui::Response,
    view: &mut crate::map::MapView,
) -> Result<bool, crate::map::MapError> {
    let mut outcome: Result<bool, crate::map::MapError> = Ok(false);
    if interaction.dragged() {
        let delta = ui.input(|input| input.pointer.delta());
        outcome = view.pan(rect, delta).map(|()| true);
    } else if interaction.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() > 0.01
            && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
        {
            let factor = (f64::from(scroll) / 240.0).exp();
            outcome = view.zoom_at(rect, pointer, factor).map(|()| true);
        }
    }
    outcome
}

/// The pointer readout shared by both canvases: coordinates, and the radar
/// gate under the pointer when a matching product is drawn.
pub(crate) fn radar_readout(
    ui: &egui::Ui,
    rect: Rect,
    transform: &map::MapTransform,
    display: &Display,
    product: RadarProduct,
    radar_visible: bool,
    obstacle: Option<Rect>,
) {
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
        return;
    };
    if !rect.contains(pointer) || obstacle.is_some_and(|band| band.contains(pointer)) {
        return;
    }
    let (lat, lon) = match transform.latlon_at(pointer) {
        Ok(point) => point,
        Err(error) => {
            ui.painter().text(
                pointer,
                Align2::LEFT_TOP,
                format!("Map coordinates unavailable: {error}"),
                FontId::proportional(theme::TYPE_SMALL),
                ERROR,
            );
            return;
        }
    };
    let mut label = format!("{lat:.4}, {lon:.4}");
    if radar_visible
        && let Some(frame) = &display.frame
        && frame.geometry.product == product
    {
        match crate::projection::inverse(frame.geometry.site_lat, frame.geometry.site_lon, lat, lon)
        {
            Ok((range_km, azimuth_deg)) => {
                match frame
                    .geometry
                    .lookup
                    .gate(&frame.volume, range_km, azimuth_deg)
                {
                    Ok(Some(hit)) => {
                        if std::fmt::Write::write_fmt(
                            &mut label,
                            format_args!(
                                "\n{} · {:.2}° beam",
                                gate_label(Some(hit.reading), frame.geometry.product),
                                hit.elevation_deg
                            ),
                        )
                        .is_err()
                        {
                            label.push_str("\nradar readout unavailable");
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if std::fmt::Write::write_fmt(
                            &mut label,
                            format_args!("\nreadout unavailable: {error}"),
                        )
                        .is_err()
                        {
                            label.push_str("\nreadout unavailable");
                        }
                    }
                }
            }
            Err(error) => {
                if std::fmt::Write::write_fmt(
                    &mut label,
                    format_args!("\nreadout unavailable: {error}"),
                )
                .is_err()
                {
                    label.push_str("\nreadout unavailable");
                }
            }
        }
    }
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(label, FontId::monospace(11.0), TEXT);
    let padding = Vec2::splat(6.0);
    let size = galley.size() + 2.0 * padding;
    let x = (pointer.x + 12.0).min((rect.right() - size.x).max(rect.left()));
    let y = if pointer.y >= rect.center().y {
        (pointer.y - size.y - 12.0).max(rect.top())
    } else {
        (pointer.y + 12.0).min((rect.bottom() - size.y).max(rect.top()))
    };
    let origin = Pos2::new(x, y);
    painter.rect_filled(
        Rect::from_min_size(origin, size),
        CornerRadius::same(4),
        Color32::from_black_alpha(220),
    );
    painter.galley(origin + padding, galley, TEXT);
}

pub(crate) const fn empty_frame_notice(product: RadarProduct) -> &'static str {
    if product.is_derived() {
        return "No column reaches this product's scale. This does not establish clear weather.";
    }
    "No measured echo in this sweep. This does not establish clear weather."
}

pub(crate) fn gate_label(reading: Option<render::Reading>, product: RadarProduct) -> String {
    match reading {
        Some(render::Reading::Measured(GateValue::Measured(value))) => {
            format!("{value:.2} {}", product.unit())
        }
        Some(render::Reading::Measured(GateValue::BelowThreshold)) => {
            String::from("below threshold")
        }
        Some(render::Reading::Measured(GateValue::RangeFolded)) => String::from("range folded"),
        Some(render::Reading::Derived(value)) => format!("{value:.2} {}", product.unit()),
        None => String::from("no radar data"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_status_area_stays_within_the_map() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(410.0, 544.0));
        assert!(rect.contains_rect(map_status_area(rect)));
        let short = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 40.0));
        let area = map_status_area(short);
        assert!(
            area.height() < 12.0,
            "a 40 px canvas got {} px of detail",
            area.height()
        );
        assert!(short.contains_rect(area));
    }

    #[test]
    fn viewport_scans_share_a_bounded_time_window() {
        assert!(scans_align(0, 6 * 60 * 1000));
        assert!(scans_align(6 * 60 * 1000, 0));
        assert!(!scans_align(0, 6 * 60 * 1000 + 1));
    }

    #[test]
    fn a_computed_gate_reads_in_its_own_unit() {
        let label = gate_label(
            Some(render::Reading::Derived(12.345)),
            RadarProduct::EchoTops,
        );
        assert_eq!(label, "12.35 km");
        let liquid = gate_label(
            Some(render::Reading::Derived(3.5)),
            RadarProduct::VerticallyIntegratedLiquid,
        );
        assert_eq!(liquid, "3.50 kg/m²");
    }

    #[test]
    fn an_empty_frame_is_never_reported_as_clear_weather() {
        for product in RadarProduct::ALL.iter().chain(RadarProduct::DERIVED.iter()) {
            let notice = empty_frame_notice(*product);
            assert!(notice.contains("does not establish"), "{notice}");
        }
        assert!(
            empty_frame_notice(RadarProduct::VerticallyIntegratedLiquid).contains("scale"),
            "a computed product talks about its scale, not about measured echo"
        );
    }
}
