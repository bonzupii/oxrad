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
//! The river-discharge layer: state, the hydrograph, and its rail section.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::hydro::{self, Discharge, HydroState, HydroView};
use crate::map::MapView;
use crate::projection;
use crate::theme::{self, Palette};
use crate::time;

/// How far the map may move before the discharge record is asked for again.
const MOVE_KM: f64 = 12.0;
/// How long a record stands before it is refreshed even without movement.
const REFRESH_AFTER: Duration = Duration::from_secs(900);
const GRAPH_HEIGHT: f32 = 124.0;

/// The river-discharge layer.
pub struct HydroLayer {
    view: HydroView,
    shown: bool,
    ensemble: bool,
    asked: Option<(f64, f64, bool)>,
    asked_at: Option<Instant>,
}

impl HydroLayer {
    pub fn new() -> Self {
        Self {
            view: HydroView::new(),
            shown: false,
            ensemble: false,
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
            Some((asked_lat, asked_lon, asked_ensemble)) => {
                asked_ensemble != self.ensemble
                    || distance_km(asked_lat, asked_lon, lat, lon) > MOVE_KM
            }
            None => true,
        };
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        let retry = retry_due && self.view.may_retry();
        if moved || stale || retry {
            self.request(lat, lon);
        }
    }

    fn request(&mut self, latitude: f64, longitude: f64) {
        self.asked = Some((latitude, longitude, self.ensemble));
        self.asked_at = Some(Instant::now());
        self.view
            .start(latitude, longitude, self.ensemble, self.view.attempts());
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("River discharge")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{} · global, no key", hydro::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Rivers: on"
        } else {
            "Rivers: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Modelled river discharge at the map centre: three weeks behind, one week ahead.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if !self.shown {
                self.asked = None;
            }
        }
        let mut ensemble = self.ensemble;
        if ui
            .checkbox(&mut ensemble, "Ensemble spread")
            .on_hover_text(
                "Ask for the GloFAS members and draw their range across the forecast, which shows how far the runs disagree.",
            )
            .changed()
        {
            self.ensemble = ensemble;
            self.asked = None;
        }
        if !self.shown {
            ui.label(
                RichText::new("Turn the layer on to read the river at the map centre.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        let now_ms = match time::now_unix_ms() {
            Ok(now) => now,
            Err(error) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("The clock cannot be read, so the hydrograph has no dates: {error}"),
                );
                return;
            }
        };
        self.status(ui);
        if let Some(record) = self.view.record() {
            ui.label(
                RichText::new(hydro::describe(record))
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            readout(ui, record, now_ms, palette);
            hydrograph(ui, record, now_ms, palette);
        }
    }

    /// The request's state, with the attempt count when it failed.
    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            HydroState::Idle => {
                ui.label(
                    RichText::new("No discharge record requested yet.")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            HydroState::Loading { last } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the river network…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
                if let Some(last) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", hydro::describe(last)))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
            HydroState::Ready { record } => {
                ui.label(
                    RichText::new(format!(
                        "At {:.2}, {:.2} · the map centre",
                        record.latitude, record.longitude
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
            HydroState::Failed {
                error,
                last,
                attempts,
                ..
            } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("River discharge failed: {error}"),
                );
                if let Some(last) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", hydro::describe(last)))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
                if *attempts > 0 {
                    ui.label(
                        RichText::new(format!("Attempt {attempts} of {}", hydro::MAX_ATTEMPTS))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
        }
    }
}

fn distance_km(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    match projection::inverse(from_lat, from_lon, to_lat, to_lon) {
        Ok((distance, _)) => distance,
        Err(_) => f64::MAX,
    }
}

/// Today's flow, the forecast peak, and the ensemble range when it was asked for.
fn readout(ui: &mut egui::Ui, record: &Discharge, now_ms: u64, palette: &Palette) {
    let observed = record.observed(now_ms);
    let forecast = record.forecast_peak(now_ms);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Now").size(theme::TYPE_SMALL).weak());
        ui.label(
            RichText::new(observed.and_then(|day| day.discharge).map_or_else(
                || String::from("no value today"),
                |value| format!("{value:.2} {}", record.units),
            ))
            .size(theme::TYPE_SMALL)
            .strong()
            .color(palette.text),
        );
    });
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Forecast peak")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        ui.label(
            RichText::new(forecast_text(forecast, &record.units))
                .size(theme::TYPE_SMALL)
                .color(palette.warning),
        );
    });
    if let Some((high, day)) = record.band_peak()
        && high > 0.0
        && Some(high) > record.forecast_peak(now_ms).and_then(|day| day.discharge)
    {
        ui.label(
            RichText::new(format!(
                "Widest member {high:.2} {} · {}",
                record.units,
                day_label(day.time_ms)
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    if !record.members.is_empty()
        && let Some(index) = record.first_after(now_ms)
        && let Some((low, high)) = record.spread(index)
    {
        ui.label(
            RichText::new(format!(
                "{} members span {low:.2} to {high:.2} {} on the first forecast day",
                record.members.len(),
                record.units
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
}

/// The forecast peak as a sentence, with its date.
fn forecast_text(forecast: Option<hydro::HydroDay>, units: &str) -> String {
    let Some(day) = forecast else {
        return String::from("none in the forecast");
    };
    let Some(value) = day.discharge else {
        return String::from("no forecast value");
    };
    match time::format_utc(day.time_ms) {
        Ok(when) => {
            let when = when.get(..10).unwrap_or(&when);
            format!("{value:.2} {units} on {when}")
        }
        Err(error) => format!("{value:.2} {units} on an unreadable date: {error}"),
    }
}

/// The hydrograph: the observed record, the forecast, and the ensemble range.
fn hydrograph(ui: &mut egui::Ui, record: &Discharge, now_ms: u64, palette: &Palette) {
    let width = ui.available_width();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, GRAPH_HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = rect.shrink2(egui::vec2(theme::SPACE_S, theme::SPACE_M));
    let Some(maximum) = plot_maximum(record) else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No day carries a discharge value.",
            FontId::proportional(theme::TYPE_SMALL),
            palette.text,
        );
        return;
    };
    let count = record.days.len();
    if count < 2 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "One day is not a hydrograph.",
            FontId::proportional(theme::TYPE_SMALL),
            palette.text,
        );
        return;
    }
    let now_index = record.first_after(now_ms).unwrap_or(count);
    draw_series(&painter, plot, record, maximum, now_index, palette);
    draw_now(&painter, plot, count, now_index, maximum, palette);
    draw_band(&painter, plot, record, maximum, now_index);
    draw_axes(&painter, plot, record, maximum, palette);
    response.on_hover_text(
        "The line is the modelled discharge in cubic metres per second. Solid is the observed end of the record, amber is the forecast, and the pale band is the range of the ensemble members.",
    );
}

/// The observed line and the forecast line, joined day by day.
fn draw_series(
    painter: &egui::Painter,
    plot: Rect,
    record: &Discharge,
    maximum: f64,
    now_index: usize,
    palette: &Palette,
) {
    let count = record.days.len();
    for (index, day) in record.days.iter().enumerate() {
        let Some(value) = day.discharge else {
            continue;
        };
        if index == 0 {
            continue;
        }
        let Some(previous) = record.days.get(index - 1).and_then(|day| day.discharge) else {
            continue;
        };
        let from = point(plot, index - 1, count, previous, maximum);
        let to = point(plot, index, count, value, maximum);
        let forecast = index >= now_index;
        let colour = if forecast {
            palette.warning
        } else {
            palette.accent
        };
        painter.line_segment([from, to], Stroke::new(1.6_f32, colour));
    }
}

/// The boundary between the observed record and the forecast.
fn draw_now(
    painter: &egui::Painter,
    plot: Rect,
    count: usize,
    now_index: usize,
    maximum: f64,
    palette: &Palette,
) {
    if now_index < count {
        let boundary = point(plot, now_index, count, 0.0, maximum).x;
        painter.line_segment(
            [
                Pos2::new(boundary, plot.top()),
                Pos2::new(boundary, plot.bottom()),
            ],
            Stroke::new(1.0_f32, palette.border),
        );
        painter.text(
            Pos2::new(boundary - 4.0, plot.top()),
            Align2::RIGHT_TOP,
            "now",
            FontId::proportional(10.0),
            palette.text,
        );
    }
}

/// The range of the ensemble members across the forecast.
fn draw_band(
    painter: &egui::Painter,
    plot: Rect,
    record: &Discharge,
    maximum: f64,
    now_index: usize,
) {
    if record.members.is_empty() {
        return;
    }
    // The band is drawn as quads between neighbouring days rather than one
    // polygon: a spread that widens and narrows is concave, and a convex fill
    // would bridge across the narrow parts and invent a range nobody modelled.
    let points = band_points(plot, record, maximum, now_index);
    if points.len() < 2 {
        return;
    }
    let colour = Color32::from_rgba_unmultiplied(255, 255, 255, 26);
    let mut mesh = egui::Mesh::default();
    for pair in points.windows(2) {
        let Some((lower, upper)) = pair.first().zip(pair.get(1)) else {
            continue;
        };
        let base = u32::try_from(mesh.vertices.len()).unwrap_or(u32::MAX);
        for position in [lower.0, lower.1, upper.1, upper.0] {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: position,
                uv: egui::epaint::WHITE_UV,
                color: colour,
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    if !mesh.indices.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// The bottom and top of the ensemble range for each forecast day that has one.
fn band_points(
    plot: Rect,
    record: &Discharge,
    maximum: f64,
    now_index: usize,
) -> Vec<(Pos2, Pos2)> {
    let count = record.days.len();
    let start = now_index.max(1);
    let mut points = Vec::new();
    for index in start..count {
        let Some((low, high)) = record.spread(index) else {
            continue;
        };
        points.push((
            point(plot, index, count, low, maximum),
            point(plot, index, count, high, maximum),
        ));
    }
    points
}

/// The scale and the dates at the edges of the plot.
fn draw_axes(
    painter: &egui::Painter,
    plot: Rect,
    record: &Discharge,
    maximum: f64,
    palette: &Palette,
) {
    painter.text(
        Pos2::new(plot.left(), plot.top()),
        Align2::LEFT_TOP,
        format!("{maximum:.2} {}", record.units),
        FontId::proportional(10.0),
        palette.text,
    );
    painter.text(
        Pos2::new(plot.left(), plot.bottom() - 1.0),
        Align2::LEFT_BOTTOM,
        String::from("0"),
        FontId::proportional(10.0),
        palette.text,
    );
    let span = match (record.days.first(), record.days.last()) {
        (Some(first), Some(last)) => format!(
            "{} to {}",
            day_label(first.time_ms),
            day_label(last.time_ms)
        ),
        _ => String::from("no dates"),
    };
    painter.text(
        Pos2::new(plot.right(), plot.bottom() + 2.0),
        Align2::RIGHT_TOP,
        span,
        FontId::proportional(10.0),
        palette.text,
    );
}

fn day_label(time_ms: u64) -> String {
    match time::format_utc(time_ms) {
        Ok(text) => String::from(text.get(..10).unwrap_or(&text)),
        Err(error) => format!("<{error}>"),
    }
}

/// The top of the plot: the largest value in the record, rounded up so the line
/// never touches the frame.
fn plot_maximum(record: &Discharge) -> Option<f64> {
    let mut maximum: Option<f64> = None;
    for day in &record.days {
        if let Some(value) = day.discharge
            && value.is_finite()
        {
            maximum = Some(maximum.map_or(value, |held: f64| held.max(value)));
        }
    }
    for index in 0..record.days.len() {
        if let Some((_, high)) = record.spread(index) {
            maximum = Some(maximum.map_or(high, |held: f64| held.max(high)));
        }
    }
    let maximum = maximum?;
    if maximum <= 0.0 {
        return Some(1.0);
    }
    let headroom = maximum * 1.12;
    Some(headroom)
}

fn point(plot: Rect, index: usize, count: usize, value: f64, maximum: f64) -> Pos2 {
    let last = count.saturating_sub(1).max(1);
    let x = plot.left() + plot.width() * (index as f32 / last as f32);
    let share = (value / maximum).clamp(0.0, 1.0) as f32;
    let y = plot.bottom() - plot.height() * share;
    Pos2::new(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(values: &[Option<f64>]) -> Discharge {
        let days = values
            .iter()
            .enumerate()
            .map(|(index, value)| hydro::HydroDay {
                time_ms: 20_750 * time::MS_PER_DAY + index as u64 * time::MS_PER_DAY,
                discharge: *value,
            })
            .collect();
        Discharge {
            latitude: 39.0,
            longitude: -96.0,
            units: String::from("m³/s"),
            days,
            members: Vec::new(),
        }
    }

    #[test]
    fn the_plot_headroom_clears_the_highest_value() {
        let top = match plot_maximum(&series(&[Some(1.0), Some(5.0), None])) {
            Some(top) => top,
            None => 0.0,
        };
        assert!((top - 5.6).abs() < 1e-9, "{top} clears the highest day");
        assert_eq!(plot_maximum(&series(&[None, None])), None);
        assert_eq!(plot_maximum(&series(&[Some(0.0)])), Some(1.0));
    }

    #[test]
    fn a_value_sits_between_the_floor_and_the_top() {
        let plot = Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 50.0));
        let bottom = point(plot, 0, 3, 0.0, 10.0);
        let top = point(plot, 0, 3, 10.0, 10.0);
        assert!((bottom.y - 50.0).abs() < f32::EPSILON);
        assert!(top.y.abs() < f32::EPSILON);
        let last = point(plot, 2, 3, 5.0, 10.0);
        assert!((last.x - 100.0).abs() < f32::EPSILON);
        assert!((last.y - 25.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_band_follows_each_forecast_day_inside_the_plot() {
        let mut record = series(&[Some(1.0), Some(1.0), Some(1.0), Some(1.0)]);
        record.members = vec![
            vec![Some(1.0), Some(1.0), Some(0.5), Some(2.0)],
            vec![Some(1.0), Some(1.0), Some(1.5), Some(3.0)],
        ];
        let plot = Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 50.0));
        let points = band_points(plot, &record, 10.0, 2);
        assert_eq!(points.len(), 2, "only the two forecast days carry a spread");
        let (low, high) = points[0];
        assert!(low.y > high.y, "a lower discharge sits nearer the floor");
        for (low, high) in &points {
            for position in [low, high] {
                assert!(plot.x_range().contains(position.x));
                assert!(plot.y_range().contains(position.y));
            }
        }
    }

    #[test]
    fn a_single_day_is_not_a_hydrograph() {
        assert_eq!(series(&[Some(2.0)]).days.len(), 1);
    }

    #[test]
    fn the_day_needs_movement_beyond_the_threshold() {
        assert!(distance_km(39.0, -96.0, 39.0, -96.0) < 0.01);
        assert!(distance_km(39.0, -96.0, 39.2, -96.0) > MOVE_KM);
    }

    #[test]
    fn a_new_layer_is_off_and_has_asked_nothing() {
        let layer = HydroLayer::new();
        assert!(!layer.shown);
        assert!(layer.asked.is_none());
        assert!(matches!(layer.view.state(), HydroState::Idle));
    }
}
