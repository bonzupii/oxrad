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
//! The vertical-profile layer: the model column above the map centre drawn as
//! a skewed temperature chart with a hodograph beside it.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, Vec2};

use crate::app::toggle_button;
use crate::map::MapView;
use crate::projection;
use crate::sounding::{self, Sounding, SoundingState, SoundingView};
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the profile is asked for again.
const MOVE_KM: f64 = 30.0;
/// How long a profile stands before it is refreshed.
const REFRESH_AFTER: Duration = Duration::from_secs(1800);
/// The temperature at the left and right edges of the skewed chart.
const CHART_LEFT_C: f32 = -80.0;
const CHART_RIGHT_C: f32 = 40.0;
/// How far an isotherm leans over the full height of the chart.
const SKEW: f32 = 0.55;
const PLOT_HEIGHT: f32 = 200.0;
const WIND_GUTTER: f32 = 26.0;
const HODO_SIZE: f32 = 132.0;

const TEMPERATURE: Color32 = Color32::from_rgb(240, 116, 104);
const DEWPOINT: Color32 = Color32::from_rgb(110, 200, 140);

/// The vertical-profile layer.
pub struct SoundingLayer {
    view: SoundingView,
    shown: bool,
    asked: Option<(f64, f64)>,
    asked_at: Option<Instant>,
}

impl SoundingLayer {
    pub fn new() -> Self {
        Self {
            view: SoundingView::new(),
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
        ui.label(
            RichText::new("Vertical profile")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · global, no key", sounding::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Profile: on"
        } else {
            "Profile: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "The model's temperature, humidity, and wind on each pressure level above the map centre, with the cloud base, freezing level, shear, and helicity worked out from it.",
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
                RichText::new("Turn the layer on to read the column over the map centre.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(record) = self.view.record() else {
            return;
        };
        readout(ui, record, palette);
        skew_chart(ui, record, palette);
        hodograph(ui, record, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            SoundingState::Idle | SoundingState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the profile…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            SoundingState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!("Refreshing · still showing {}", describe(record)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            SoundingState::Ready { record } => {
                ui.label(
                    RichText::new(describe(record))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            SoundingState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Profile failed: {error}"),
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

/// The hour and the levels the profile holds.
fn describe(record: &Sounding) -> String {
    let elevation = record.elevation_m.map_or_else(
        || String::from("elevation not stated"),
        |metres| format!("point {metres:.0} m"),
    );
    format!(
        "{} · {} levels · {elevation}",
        short_hour(record.time_ms),
        record.levels.len()
    )
}

fn short_hour(time_ms: u64) -> String {
    let Ok(days) = i64::try_from(time_ms / time::MS_PER_DAY) else {
        return String::from("<that hour is past the calendar>");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (time_ms / 3_600_000) % 24;
    format!("{month:02}-{day:02} {hour:02}Z")
}

/// The derived quantities, each one named with the assumption behind it.
fn readout(ui: &mut egui::Ui, record: &Sounding, palette: &Palette) {
    let indices = record.indices();
    let surface = record.surface;
    let conditions = match (surface.temperature_c, surface.dewpoint_c) {
        (Some(temperature), Some(dewpoint)) => {
            format!("Surface {temperature:.0} °C, dew {dewpoint:.0} °C")
        }
        (Some(temperature), None) => format!("Surface {temperature:.0} °C, dew not stated"),
        _ => String::from("Surface temperature not stated"),
    };
    ui.label(RichText::new(conditions).size(theme::TYPE_SMALL).strong());

    for (label, value) in [
        (
            "Cloud base",
            indices.lcl_m.map(|metres| format!("{metres:.0} m AGL")),
        ),
        (
            "Freezing level",
            indices
                .freezing_level_m
                .map(|metres| format!("{metres:.0} m MSL")),
        ),
        (
            "Bulk shear",
            indices.deep_shear.map(|(depth, speed)| {
                format!(
                    "0–{:.1} km {speed:.0} m/s · {:.0} kt",
                    depth / 1000.0,
                    speed * 1.943_844
                )
            }),
        ),
        (
            "0–1 km shear",
            indices
                .shear_0_1_mps
                .map(|speed| format!("{speed:.0} m/s · {:.0} kt", speed * 1.943_844)),
        ),
        (
            "0–3 km helicity",
            indices
                .srh_0_3
                .map(|value| format!("{value:.0} m²/s² against the right-mover")),
        ),
        (
            "Storm motion",
            indices.right_mover.map(|(u, v)| {
                let speed = (u * u + v * v).sqrt() * 1.943_844;
                format!("{speed:.0} kt toward {}", bearing_label(u, v))
            }),
        ),
        (
            "700–500 lapse",
            indices
                .lapse_rate_700_500
                .map(|rate| format!("{rate:.1} °C/km")),
        ),
        (
            "Precipitable water",
            indices
                .precipitable_water_mm
                .map(|water| format!("{water:.1} mm")),
        ),
    ] {
        let colour = if value.is_some() {
            palette.text
        } else {
            palette.text.gamma_multiply(0.55)
        };
        let text = value.unwrap_or_else(|| String::from("not available from these levels"));
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(label)
                    .size(theme::TYPE_SMALL)
                    .color(palette.text.gamma_multiply(0.7)),
            );
            ui.label(RichText::new(text).size(theme::TYPE_SMALL).color(colour));
        });
    }
    ui.label(
        RichText::new(
            "A forecast-model column, not a balloon: the grid box is roughly 10 km wide, so a boundary a few kilometres away is invisible here. The cloud base is the 125 m per degree estimate, and the helicity is taken against the Bunkers right-mover computed from this same column.",
        )
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}

/// The compass direction a wind vector heads towards.
fn bearing_label(east: f64, north: f64) -> String {
    if east.abs() < f64::EPSILON && north.abs() < f64::EPSILON {
        return String::from("still air");
    }
    let degrees = east.atan2(north).to_degrees().rem_euclid(360.0);
    let points = [
        "north",
        "north-northeast",
        "northeast",
        "east-northeast",
        "east",
        "east-southeast",
        "southeast",
        "south-southeast",
        "south",
        "south-southwest",
        "southwest",
        "west-southwest",
        "west",
        "west-northwest",
        "northwest",
        "north-northwest",
    ];
    let index = ((degrees / 22.5).round() as usize) % points.len();
    points.get(index).map_or_else(
        || format!("{degrees:.0}°"),
        |point| format!("{degrees:.0}° ({point})"),
    )
}

/// The vertical position of a pressure on the chart, with a logarithmic axis.
fn pressure_y(plot: Rect, pressure_hpa: f32, bottom_hpa: f32, top_hpa: f32) -> Option<f32> {
    if pressure_hpa <= 0.0 || bottom_hpa <= top_hpa {
        return None;
    }
    let ratio = (bottom_hpa / pressure_hpa).ln() / (bottom_hpa / top_hpa).ln();
    Some(plot.bottom() - plot.height() * ratio)
}

/// The horizontal position of a temperature on the chart, leaning isotherms to
/// the right as pressure falls so a parcel's path is readable.
fn skew_x(plot: Rect, temperature_c: f32, pressure_hpa: f32, bottom_hpa: f32, top_hpa: f32) -> f32 {
    let share = (temperature_c - CHART_LEFT_C) / (CHART_RIGHT_C - CHART_LEFT_C);
    let lean = if bottom_hpa > top_hpa && pressure_hpa > 0.0 {
        (bottom_hpa / pressure_hpa).ln() / (bottom_hpa / top_hpa).ln()
    } else {
        0.0
    };
    plot.left() + plot.width() * (share + SKEW * lean)
}

/// The skewed temperature chart, with the wind staff on the right edge.
fn skew_chart(ui: &mut egui::Ui, record: &Sounding, palette: &Palette) {
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, PLOT_HEIGHT + 14.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 4.0, rect.top() + 4.0),
        Pos2::new(rect.right() - WIND_GUTTER, rect.bottom() - 12.0),
    );
    let bottom_hpa = record
        .levels
        .first()
        .map_or(1000.0, |level| level.pressure_hpa as f32);
    let top_hpa = record.top_hpa() as f32;
    grid(&painter, plot, bottom_hpa, top_hpa, palette);
    let mut temperature_points = Vec::new();
    let mut dewpoint_points = Vec::new();
    for level in &record.levels {
        let pressure = level.pressure_hpa as f32;
        let Some(y) = pressure_y(plot, pressure, bottom_hpa, top_hpa) else {
            continue;
        };
        if let Some(temperature) = level.temperature_c {
            temperature_points.push(Pos2::new(
                skew_x(plot, temperature as f32, pressure, bottom_hpa, top_hpa),
                y,
            ));
        }
        if let Some(dewpoint) = level.dewpoint_c {
            dewpoint_points.push(Pos2::new(
                skew_x(plot, dewpoint as f32, pressure, bottom_hpa, top_hpa),
                y,
            ));
        }
    }
    for (points, colour) in [
        (&temperature_points, TEMPERATURE),
        (&dewpoint_points, DEWPOINT),
    ] {
        for pair in points.windows(2) {
            let (Some(from), Some(to)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            painter.line_segment([*from, *to], Stroke::new(1.6_f32, colour));
        }
    }
    wind_staff(&painter, rect, plot, record, bottom_hpa, top_hpa, palette);
    painter.text(
        Pos2::new(plot.right() - 2.0, plot.top()),
        Align2::RIGHT_TOP,
        format!("{top_hpa:.0} hPa"),
        FontId::proportional(9.0),
        palette.text.gamma_multiply(0.5),
    );
    response.on_hover_text(
        "A skewed chart: isobars run across, isotherms lean, the red trace is the model's temperature and the green its dew point. The shafts on the right point the way the wind blows on each level.",
    );
}

/// The isobars and the skewed isotherms behind the traces.
fn grid(painter: &egui::Painter, plot: Rect, bottom_hpa: f32, top_hpa: f32, palette: &Palette) {
    let label_font = FontId::proportional(9.0);
    for pressure in [1000.0, 850.0, 700.0, 500.0, 300.0, 200.0_f32] {
        let Some(y) = pressure_y(plot, pressure, bottom_hpa, top_hpa) else {
            continue;
        };
        if y < plot.top() || y > plot.bottom() {
            continue;
        }
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0_f32, palette.border.gamma_multiply(0.8)),
        );
        painter.text(
            Pos2::new(plot.left() + 2.0, y - 1.0),
            Align2::LEFT_BOTTOM,
            format!("{pressure:.0}"),
            label_font.clone(),
            palette.text.gamma_multiply(0.55),
        );
    }
    for temperature in (-80..=40).step_by(20).map(|value| value as f32) {
        let low = skew_x(plot, temperature, bottom_hpa, bottom_hpa, top_hpa);
        let high = skew_x(plot, temperature, top_hpa, bottom_hpa, top_hpa);
        let colour = if temperature == 0.0 {
            palette.text.gamma_multiply(0.55)
        } else {
            palette.text.gamma_multiply(0.22)
        };
        painter.line_segment(
            [Pos2::new(low, plot.bottom()), Pos2::new(high, plot.top())],
            Stroke::new(1.0_f32, colour),
        );
        painter.text(
            Pos2::new(low, plot.bottom() + 1.0),
            Align2::CENTER_TOP,
            format!("{temperature:.0}"),
            label_font.clone(),
            palette.text.gamma_multiply(0.5),
        );
    }
}

/// A short shaft per level, pointing the way the wind blows.
fn wind_staff(
    painter: &egui::Painter,
    rect: Rect,
    plot: Rect,
    record: &Sounding,
    bottom_hpa: f32,
    top_hpa: f32,
    palette: &Palette,
) {
    for level in &record.levels {
        let (Some(from), Some(speed)) = (level.wind_from_deg, level.wind_speed_mps) else {
            continue;
        };
        let pressure = level.pressure_hpa as f32;
        let Some(y) = pressure_y(plot, pressure, bottom_hpa, top_hpa) else {
            continue;
        };
        if y < plot.top() || y > plot.bottom() {
            continue;
        }
        let heading = from.to_radians();
        let length = 8.0 + speed.min(40.0) as f32 * 0.22;
        // Meteorological direction is where the wind comes from, so the shaft
        // points the other way.
        let toward = Vec2::new(-heading.sin() as f32, heading.cos() as f32);
        let tail = Pos2::new(rect.right() - WIND_GUTTER / 2.0, y);
        painter.line_segment(
            [
                tail,
                Pos2::new(tail.x + toward.x * length, tail.y + toward.y * length * 0.5),
            ],
            Stroke::new(1.2_f32, palette.accent),
        );
    }
}

/// The hodograph: the wind vectors up the column, joined, with the storm motion.
fn hodograph(ui: &mut egui::Ui, record: &Sounding, palette: &Palette) {
    let width = ui.available_width().min(HODO_SIZE);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, HODO_SIZE), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let centre = rect.center();
    let scale = hodograph_frame(&painter, rect, record, palette);
    let Some(elevation) = record.elevation_m else {
        painter.text(
            centre,
            Align2::CENTER_CENTER,
            "no point height",
            FontId::proportional(9.0),
            palette.text.gamma_multiply(0.6),
        );
        return;
    };
    let points: Vec<(Pos2, f64)> = record
        .levels
        .iter()
        .filter_map(|level| {
            let vector = level.wind_vector()?;
            let height = level.height_m? - elevation;
            let point = Pos2::new(
                centre.x + (vector.0 * 1.943_844) as f32 * scale,
                centre.y - (vector.1 * 1.943_844) as f32 * scale,
            );
            Some((point, height))
        })
        .collect();
    let low: Vec<Pos2> = points
        .iter()
        .filter(|(_, height)| *height <= 3000.0)
        .map(|(point, _)| *point)
        .collect();
    let high: Vec<Pos2> = points
        .iter()
        .filter(|(_, height)| *height >= 3000.0)
        .map(|(point, _)| *point)
        .collect();
    for (path, colour) in [
        (&low, TEMPERATURE),
        (&high, Color32::from_rgb(240, 200, 96)),
    ] {
        for pair in path.windows(2) {
            let (Some(from), Some(to)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            painter.line_segment([*from, *to], Stroke::new(1.6_f32, colour));
        }
        for point in path.iter() {
            painter.circle_filled(*point, 2.0_f32, colour);
        }
    }
    if let Some((u, v)) = record.indices().right_mover {
        let point = Pos2::new(
            centre.x + (u * 1.943_844) as f32 * scale,
            centre.y - (v * 1.943_844) as f32 * scale,
        );
        painter.circle_stroke(point, 4.0_f32, Stroke::new(1.4_f32, palette.accent));
    }
    painter.text(
        Pos2::new(rect.left() + 3.0, rect.bottom() - 2.0),
        Align2::LEFT_BOTTOM,
        "red 0–3 km · yellow 3–6 km",
        FontId::proportional(8.0),
        palette.text.gamma_multiply(0.5),
    );
    response.on_hover_text(
        "The wind vectors up the column, joined in order of height: red is the lowest three kilometres and yellow the three above it. The ring marks the Bunkers right-moving storm motion, which is what the helicity is taken against.",
    );
}

/// The rings, axes, and captions of the hodograph, and the pixels per knot the
/// winds are drawn at.
fn hodograph_frame(
    painter: &egui::Painter,
    rect: Rect,
    record: &Sounding,
    palette: &Palette,
) -> f32 {
    let centre = rect.center();
    let radius = rect.width().min(rect.height()) / 2.0 - 2.0;
    // The outer ring holds the fastest wind the column reports, rounded up to
    // the next twenty knots, so a jet-level wind is never drawn off the chart.
    let fastest = record
        .levels
        .iter()
        .filter_map(|level| level.wind_speed_mps)
        .fold(0.0_f64, f64::max)
        * 1.943_844;
    let top_knots = ((fastest / 20.0).ceil() * 20.0).max(40.0) as f32;
    let scale = radius / top_knots;
    for knots in [top_knots / 2.0, top_knots] {
        painter.circle_stroke(
            centre,
            knots * scale,
            Stroke::new(1.0_f32, palette.border.gamma_multiply(0.8)),
        );
    }
    painter.text(
        Pos2::new(rect.right() - 3.0, centre.y + 2.0),
        Align2::RIGHT_TOP,
        format!("{top_knots:.0}"),
        FontId::proportional(8.0),
        palette.text.gamma_multiply(0.5),
    );
    for (from, to) in [
        (
            Pos2::new(centre.x - radius, centre.y),
            Pos2::new(centre.x + radius, centre.y),
        ),
        (
            Pos2::new(centre.x, centre.y - radius),
            Pos2::new(centre.x, centre.y + radius),
        ),
    ] {
        painter.line_segment(
            [from, to],
            Stroke::new(1.0_f32, palette.border.gamma_multiply(0.6)),
        );
    }
    painter.text(
        Pos2::new(rect.left() + 3.0, rect.top() + 2.0),
        Align2::LEFT_TOP,
        "hodograph, kt",
        FontId::proportional(9.0),
        palette.text.gamma_multiply(0.5),
    );
    scale
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

    fn plot() -> Rect {
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 200.0))
    }

    #[test]
    fn the_pressure_axis_puts_a_high_altitude_level_above_a_low_one() {
        let low = pressure_y(plot(), 1000.0, 1000.0, 200.0).unwrap_or(f32::NAN);
        let mid = pressure_y(plot(), 500.0, 1000.0, 200.0).unwrap_or(f32::NAN);
        let high = pressure_y(plot(), 200.0, 1000.0, 200.0).unwrap_or(f32::NAN);
        assert!(low > mid && mid > high, "{low} {mid} {high}");
        assert!((low - plot().bottom()).abs() < 0.5);
        assert!((high - plot().top()).abs() < 0.5);
    }

    #[test]
    fn an_isotherm_leans_right_as_the_pressure_falls() {
        let warm_low = skew_x(plot(), 20.0, 1000.0, 1000.0, 200.0);
        let warm_high = skew_x(plot(), 20.0, 200.0, 1000.0, 200.0);
        assert!(warm_high > warm_low, "{warm_high} {warm_low}");
        assert!(plot().x_range().contains(warm_low));
        // A temperature at the cold edge stays on the chart all the way up.
        let cold_low = skew_x(plot(), -60.0, 1000.0, 1000.0, 200.0);
        let cold_high = skew_x(plot(), -60.0, 200.0, 1000.0, 200.0);
        assert!(plot().x_range().contains(cold_low));
        assert!(cold_high <= plot().right() + 1.0, "{cold_high}");
    }

    #[test]
    fn a_storm_moving_east_is_named_east() {
        assert_eq!(bearing_label(10.0, 0.0), "90° (east)");
        assert_eq!(bearing_label(0.0, 10.0), "0° (north)");
        assert_eq!(bearing_label(0.0, 0.0), "still air");
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = SoundingLayer::new();
        assert!(!layer.shown);
        assert!(matches!(layer.view.state(), SoundingState::Idle));
    }

    #[test]
    fn an_hour_label_names_its_day() {
        assert_eq!(short_hour(20_760 * time::MS_PER_DAY), "11-03 00Z");
    }
}
