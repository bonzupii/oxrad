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
//! The land-surface layer: soil water, evapotranspiration, and the sun that
//! drives both.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::land::{self, Land, LandState, LandView, SoilBand};
use crate::map::MapView;
use crate::projection;
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the record is asked for again.
const MOVE_KM: f64 = 20.0;
/// How long a record stands before it is refreshed.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
const CHART_HEIGHT: f32 = 92.0;
const HOUR_GUTTER: f32 = 14.0;
const DAY_MS: u64 = 86_400_000;

/// The land-surface layer.
pub struct LandLayer {
    view: LandView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl LandLayer {
    pub fn new() -> Self {
        Self {
            view: LandView::new(),
            shown: false,
            asked: None,
            asked_at: None,
        }
    }

    /// Follows the map centre while the layer is on, and takes in the worker's
    /// answers.
    pub fn update(&mut self, now: Instant, view: &MapView) {
        let retry_due = self.view.update(now);
        if !self.shown {
            return;
        }
        let Ok((lat, lon)) = view.center_latlon() else {
            return;
        };
        let moved = match self.asked {
            Some((asked_lat, asked_lon)) => distance_km(asked_lat, asked_lon, lat, lon) > MOVE_KM,
            None => true,
        };
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        let retry = retry_due && self.view.may_retry();
        if moved || stale || retry {
            self.asked = Some((lat, lon));
            self.asked_at = Some(Instant::now());
            self.view.start(lat, lon, self.view.attempts());
        }
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(RichText::new("Land surface").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!("{} · global, no key", land::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown { "Land: on" } else { "Land: off" };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Soil water in the top layers, the water the atmosphere will pull out of the ground, and the sunlight and ultraviolet reaching it.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if !self.shown {
                self.asked = None;
            }
        }
        if !self.shown {
            ui.label(
                RichText::new("Turn the layer on to read the ground under the map centre.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(record) = self.view.record() else {
            return;
        };
        let Ok(now_ms) = time::now_unix_ms() else {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "The system clock cannot be read, so the land record has no present hour.",
            );
            return;
        };
        readout(ui, record, now_ms, palette);
        chart(ui, record, now_ms, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            LandState::Idle | LandState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the land surface…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            LandState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            LandState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            LandState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Land surface failed: {error}"),
                );
                if let Some(record) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", describe(record)))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
                if self.view.retry_waiting() {
                    ui.label(
                        RichText::new("Asking again shortly.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
        }
    }
}

/// The hours the record holds and the point it was answered for.
fn describe(record: &Land) -> String {
    let (Some(first), Some(last)) = (record.hours.first(), record.hours.last()) else {
        return String::from("The provider sent no hours.");
    };
    let elevation = record.elevation_m.map_or_else(
        || String::from("elevation not stated"),
        |metres| format!("grid elevation {metres:.0} m"),
    );
    format!(
        "{} hours · {} to {} · {elevation}",
        record.hours.len(),
        short_hour(first.time_ms),
        short_hour(last.time_ms)
    )
}

fn short_hour(time_ms: u64) -> String {
    let Ok(days) = i64::try_from(time_ms / time::MS_PER_DAY) else {
        return format!("<{time_ms} ms is past the calendar>");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (time_ms / 3_600_000) % 24;
    format!("{month:02}-{day:02} {hour:02}Z")
}

/// The soil, the water leaving it, and the sun arriving.
fn readout(ui: &mut egui::Ui, record: &Land, now_ms: u64, palette: &Palette) {
    let Some(hour) = record.at(now_ms) else {
        ui.label(
            RichText::new("The record begins after now, so the present hour is not in it.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        return;
    };
    soil(ui, record, hour, now_ms, palette);
    sun(ui, record, hour);
    ui.label(
        RichText::new(
            "Model fields for the whole grid box: soil moisture is a volumetric fraction, not a depth, and the band is a rule of thumb for a loam rather than a drought index for this soil.",
        )
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}

/// The ground's own water and temperature.
fn soil(ui: &mut egui::Ui, record: &Land, hour: land::LandHour, now_ms: u64, palette: &Palette) {
    let unit = record.unit("soil_moisture_0_to_10cm");
    match hour.moisture_top {
        Some(water) => {
            let band = SoilBand::of(water);
            ui.label(
                RichText::new(format!("Top 10 cm {water:.3} {unit} · {}", band.label()))
                    .size(theme::TYPE_SMALL)
                    .strong()
                    .color(band_colour(band, palette)),
            );
        }
        None => {
            ui.label(
                RichText::new("The provider sent no soil moisture for the hour.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }
    if let Some(deep) = hour.moisture_deep {
        ui.label(
            RichText::new(format!("10–40 cm {deep:.3} {unit}"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    if let Some(temperature) = hour.soil_temperature_c {
        ui.label(
            RichText::new(format!("Soil at 6 cm {temperature:.1} °C"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    let day_et0 = record.et0_between(now_ms, now_ms.saturating_add(DAY_MS));
    ui.label(
        RichText::new(format!(
            "Water leaving the ground in 24 h {day_et0:.1} {}",
            record.unit("et0_fao_evapotranspiration")
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    if let Some(vpd) = hour.vpd_kpa {
        let dryness = if vpd < 0.5 {
            "the air is close to saturated"
        } else if vpd < 1.5 {
            "a moderate drying pull"
        } else if vpd < 3.0 {
            "a strong drying pull"
        } else {
            "a severe drying pull"
        };
        ui.label(
            RichText::new(format!(
                "Vapour-pressure deficit {vpd:.2} {} · {dryness}",
                record.unit("vapour_pressure_deficit")
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    if let Some(wettest) = record.wettest().and_then(|hour| hour.moisture_top) {
        ui.label(
            RichText::new(format!("Wettest hour in the record {wettest:.3} {unit}"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
}

/// The ultraviolet and the sunlight arriving at the ground.
fn sun(ui: &mut egui::Ui, record: &Land, hour: land::LandHour) {
    if let Some(uv) = hour.uv_index {
        ui.label(
            RichText::new(format!("Ultraviolet index {uv:.1} · {}", uv_band(uv)))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    if let Some(peak) = record.peak_uv().and_then(|hour| hour.uv_index) {
        ui.label(
            RichText::new(format!("Peak in the record {peak:.1} · {}", uv_band(peak)))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    let unit = record.unit("shortwave_radiation");
    if let Some(shortwave) = hour.shortwave_wm2 {
        let direct = hour.direct_normal_wm2.map_or_else(
            || String::from("direct beam not stated"),
            |direct| {
                format!(
                    "direct normal {direct:.0} {}",
                    record.unit("direct_normal_irradiance")
                )
            },
        );
        ui.label(
            RichText::new(format!("Shortwave {shortwave:.0} {unit} · {direct}"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    if let Some((strongest, sunniest)) = record
        .sunniest()
        .and_then(|hour| hour.shortwave_wm2.map(|sun| (hour, sun)))
        && unit != "no unit stated"
    {
        ui.label(
            RichText::new(format!(
                "Strongest sun in the record at {} · {sunniest:.0} {unit}",
                short_hour(strongest.time_ms)
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
}

/// The band a soil's water content is written in.
const fn band_colour(band: SoilBand, palette: &Palette) -> egui::Color32 {
    match band {
        SoilBand::VeryDry => palette.error,
        SoilBand::Dry => palette.warning,
        SoilBand::Moist => palette.text,
        SoilBand::Wet => palette.accent,
    }
}

/// The band a soil's water content is drawn in: red through orange to blue, so
/// the bars carry the band even when every hour of the record falls in one of
/// them.
fn band_bar_colour(band: SoilBand, palette: &Palette) -> egui::Color32 {
    match band {
        SoilBand::VeryDry => palette.error,
        SoilBand::Dry => palette.warning,
        SoilBand::Moist | SoilBand::Wet => water_colour(palette, band == SoilBand::Wet),
    }
}

/// A soil-water blue that reads on both themes. The palette's sea fill is
/// near-black in the dark theme and near-white in the light one, so neither
/// end of it works as a bar on its own background.
fn water_colour(palette: &Palette, wet: bool) -> egui::Color32 {
    let background = palette.background;
    let luminance = 0.299 * f32::from(background.r())
        + 0.587 * f32::from(background.g())
        + 0.114 * f32::from(background.b());
    match (luminance < 128.0, wet) {
        (true, false) => egui::Color32::from_rgb(74, 140, 196),
        (true, true) => egui::Color32::from_rgb(126, 190, 236),
        (false, false) => egui::Color32::from_rgb(44, 104, 164),
        (false, true) => egui::Color32::from_rgb(22, 70, 124),
    }
}

/// The wording the ultraviolet index is described with.
fn uv_band(uv: f64) -> &'static str {
    if uv < 3.0 {
        "low"
    } else if uv < 6.0 {
        "moderate"
    } else if uv < 8.0 {
        "high"
    } else if uv < 11.0 {
        "very high"
    } else {
        "extreme"
    }
}

/// Soil moisture hour by hour, with the present hour marked.
fn chart(ui: &mut egui::Ui, record: &Land, now_ms: u64, palette: &Palette) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, CHART_HEIGHT + HOUR_GUTTER),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 3.0, rect.top() + 4.0),
        Pos2::new(rect.right() - 3.0, rect.bottom() - HOUR_GUTTER),
    );
    let count = record.hours.len();
    if count == 0 {
        return;
    }
    let top = record.axis_top();
    let slot = plot.width() / count as f32;
    for (index, hour) in record.hours.iter().enumerate() {
        let Some(water) = hour.moisture_top else {
            continue;
        };
        let share = (water / top).clamp(0.0, 1.0) as f32;
        let x = plot.left() + slot * index as f32;
        let bar = Rect::from_min_max(
            Pos2::new(x, plot.bottom() - plot.height() * share),
            Pos2::new(x + (slot * 0.8).max(1.0), plot.bottom()),
        );
        painter.rect_filled(
            bar,
            egui::CornerRadius::ZERO,
            band_bar_colour(SoilBand::of(water), palette),
        );
    }
    if let Some(now) = record.hours.iter().position(|hour| hour.time_ms > now_ms)
        && now > 0
    {
        let x = plot.left() + slot * now as f32;
        painter.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0_f32, palette.text.gamma_multiply(0.5)),
        );
    }
    painter.text(
        Pos2::new(plot.left() + 2.0, plot.top()),
        Align2::LEFT_TOP,
        format!("{top:.2} {}", record.unit("soil_moisture_0_to_10cm")),
        FontId::proportional(9.0),
        palette.text,
    );
    for (hour, alignment, x) in [
        (record.hours.first(), Align2::LEFT_TOP, plot.left()),
        (record.hours.last(), Align2::RIGHT_TOP, plot.right()),
    ] {
        let Some(hour) = hour else {
            continue;
        };
        painter.text(
            Pos2::new(x, plot.bottom() + 1.0),
            alignment,
            short_hour(hour.time_ms),
            FontId::proportional(10.0),
            palette.text,
        );
    }
    response.on_hover_text(
        "Soil water in the top ten centimetres, hour by hour. The vertical line is now: to its left the model has already run, to its right it is forecasting.",
    );
}

/// Distance between two points, from the projection's own geodesic.
fn distance_km(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    match projection::inverse(from_lat, from_lon, to_lat, to_lon) {
        Ok((distance, _)) => distance,
        Err(_) => f64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uv_index_is_described_in_its_band() {
        assert_eq!(uv_band(1.0), "low");
        assert_eq!(uv_band(4.0), "moderate");
        assert_eq!(uv_band(7.0), "high");
        assert_eq!(uv_band(9.5), "very high");
        assert_eq!(uv_band(12.0), "extreme");
    }

    #[test]
    fn an_hour_label_names_its_day() {
        assert_eq!(
            short_hour(20_759 * time::MS_PER_DAY + 5 * 3_600_000),
            "11-02 05Z"
        );
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = LandLayer::new();
        assert!(!layer.shown);
        assert!(matches!(layer.view.state(), LandState::Idle));
    }
}
