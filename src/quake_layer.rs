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
//! The earthquake layer: its feed, its selection, and its presentation.
//!
//! Symbol area follows magnitude and colour follows the depth band, so a large
//! deep event cannot read as a shallow damaging one. The layer owns the
//! request cadence and the selection; the application only tells it when the
//! layer is on screen and where the map is pointing.

use std::time::{Duration, Instant, SystemTimeError};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, RichText, Stroke, Vec2,
};

use crate::app::{action_button, toggle_button};
use crate::map;
use crate::quake::{self, DepthBand, Quake, QuakeState, QuakeView, Window};
use crate::theme::{self, Palette};
use crate::time;

const TEXT: Color32 = Color32::from_rgb(214, 221, 232);
const TSUNAMI: Color32 = Color32::from_rgb(255, 196, 64);
const HIGHLIGHT: Color32 = Color32::from_rgb(255, 255, 255);
const LIST_LIMIT: usize = 10;
const BANDS: [DepthBand; 4] = [
    DepthBand::Shallow,
    DepthBand::Intermediate,
    DepthBand::Deep,
    DepthBand::VeryDeep,
];

/// A symbol's radius in points, for a magnitude.
pub fn radius(magnitude: f64) -> f32 {
    let scaled = 3.0 + 2.6 * (magnitude - 2.5).max(0.0);
    scaled.min(20.0) as f32
}

/// The colour of a depth band, shallowest first.
pub const fn colour(band: DepthBand) -> Color32 {
    match band {
        DepthBand::Shallow => Color32::from_rgb(255, 82, 62),
        DepthBand::Intermediate => Color32::from_rgb(255, 156, 48),
        DepthBand::Deep => Color32::from_rgb(216, 212, 96),
        DepthBand::VeryDeep => Color32::from_rgb(126, 178, 255),
    }
}

/// Everything the application needs to keep the layer working.
pub struct QuakeLayer {
    view: QuakeView,
    shown: bool,
    window: Window,
    selected: Option<String>,
    refresh_due: Option<Instant>,
}

impl QuakeLayer {
    pub fn new() -> Self {
        Self {
            view: QuakeView::new(),
            shown: false,
            window: Window::Day,
            selected: None,
            refresh_due: None,
        }
    }

    /// Polls the worker and keeps the request cadence. Nothing is requested
    /// while the layer is hidden.
    pub fn update(&mut self, now: Instant) {
        if !self.shown {
            return;
        }
        self.view.update(now);
        let due = self.refresh_due.is_some_and(|at| now >= at);
        if due || matches!(self.view.state(), QuakeState::Idle) {
            self.refresh_due = None;
            self.refresh();
        }
    }

    fn refresh(&mut self) {
        self.view.start(self.window, 0);
        self.refresh_due = Instant::now().checked_add(Duration::from_secs(quake::REFRESH_SECS));
    }

    /// Draws every epicentre inside the rectangle and reports the event a click
    /// landed on, if any.
    pub fn draw(
        &mut self,
        painter: &Painter,
        rect: Rect,
        transform: &map::MapTransform,
        pointer: Option<Pos2>,
        clicked: bool,
    ) {
        if !self.shown {
            return;
        }
        let Some(quakes) = self.view.events() else {
            return;
        };
        let now = time::now_unix_ms();
        let mut hits: Vec<(&Quake, Pos2, f32)> = Vec::new();
        for quake in &quakes.events {
            let Some(position) = transform.screen(quake.lat, quake.lon) else {
                continue;
            };
            if !rect.contains(position) {
                continue;
            }
            let radius = radius(quake.magnitude);
            let filled = colour(quake.depth_band());
            painter.circle_filled(position, radius, filled.gamma_multiply(0.82));
            painter.circle_stroke(position, radius, Stroke::new(1.0_f32, Color32::BLACK));
            if quake.tsunami || quake.alert.is_some() {
                painter.circle_stroke(position, radius + 2.5, Stroke::new(1.5_f32, TSUNAMI));
            }
            if self.selected.as_deref() == Some(quake.id.as_str()) {
                painter.circle_stroke(position, radius + 4.5, Stroke::new(2.0_f32, HIGHLIGHT));
                painter.text(
                    position + Vec2::new(radius + 6.0, -radius - 6.0),
                    Align2::LEFT_BOTTOM,
                    format!("M{:.1} · {:.0} km deep", quake.magnitude, quake.depth_km),
                    FontId::monospace(11.0),
                    TEXT,
                );
            }
            hits.push((quake, position, radius));
        }
        let pointer = pointer.filter(|at| rect.contains(*at));
        if clicked && let Some(at) = pointer {
            if let Some((quake, ..)) = nearest(&hits, at) {
                self.selected = Some(quake.id.clone());
            }
            return;
        }
        if let Some(at) = pointer
            && let Some((quake, ..)) = nearest(&hits, at)
        {
            paint_tooltip(painter, rect, at, &tooltip(quake, &now));
        }
    }

    /// The rail section. Returns a focus request when the user asks to centre
    /// the map on an event.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Option<(f64, f64)> {
        self.rail_header(ui, palette);
        if self.status(ui, palette) {
            self.refresh();
        }
        let now = time::now_unix_ms();
        let focus = self.events_report(ui, palette, &now);
        self.rail_details(ui);
        focus
    }

    /// Title, on/off control, the feed window, and the depth key.
    fn rail_header(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.label(RichText::new("Earthquakes").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new("USGS · worldwide, no key required")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Earthquakes: on"
        } else {
            "Earthquakes: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Live epicentres from the USGS summary feeds. Symbol size follows magnitude, colour follows depth.",
            )
            .clicked()
        {
            self.shown = !self.shown;
        }
        let mut window = None;
        ui.horizontal(|ui| {
            for candidate in Window::ALL {
                if ui
                    .selectable_label(candidate == self.window, candidate.label())
                    .on_hover_text(format!("{} · {}", candidate.label(), candidate.floor()))
                    .clicked()
                {
                    window = Some(candidate);
                }
            }
        });
        ui.label(
            RichText::new(self.window.floor())
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        if let Some(window) = window {
            self.window = window;
            self.refresh();
        }
        ui.horizontal(|ui| {
            for band in BANDS {
                ui.label(
                    RichText::new("●")
                        .color(colour(band))
                        .size(theme::TYPE_BODY),
                );
                ui.label(RichText::new(band.label()).size(theme::TYPE_SMALL).weak());
            }
        });
    }

    /// The chosen event, or the strongest one the feed carries.
    fn rail_details(&mut self, ui: &mut egui::Ui) {
        let Some(quakes) = self.view.events() else {
            return;
        };
        let chosen = self
            .selected
            .as_deref()
            .and_then(|id| quakes.events.iter().find(|quake| quake.id == id));
        match chosen {
            Some(quake) => details(ui, "Selected event", quake),
            None => match quakes.strongest() {
                Some(quake) => details(ui, "Strongest event in this feed", quake),
                None => {
                    ui.label(
                        RichText::new("This feed holds no event with a usable magnitude.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            },
        }
    }

    /// The feed's own state, with its age and provenance. Returns whether the
    /// user asked for another request.
    fn status(&self, ui: &mut egui::Ui, palette: &Palette) -> bool {
        let mut refresh = false;
        match self.view.state() {
            QuakeState::Idle => {
                ui.label(
                    RichText::new("Earthquake feed not requested yet.")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            QuakeState::Loading { last, .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Requesting the USGS feed…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
                if let Some(last) = last {
                    ui.label(
                        RichText::new(format!("Still showing {}", quake::describe(last)))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
            QuakeState::Ready { quakes } => {
                ui.label(
                    RichText::new(quake::describe(quakes))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                )
                .on_hover_text(&quakes.title);
                let built = match time::format_utc(quakes.generated_ms) {
                    Ok(when) => when,
                    Err(error) => format!("feed time unavailable: {error}"),
                };
                ui.label(
                    RichText::new(format!("Feed built {built}"))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            QuakeState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("USGS feed failed: {error}"),
                );
                if let Some(last) = last {
                    ui.label(
                        RichText::new(format!(
                            "Showing the last feed received: {}",
                            quake::describe(last)
                        ))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                    );
                }
            }
        }
        if ui
            .add_sized([110.0, 28.0], action_button(palette, "Refresh quakes"))
            .clicked()
        {
            refresh = true;
        }
        refresh
    }

    fn events_report(
        &mut self,
        ui: &mut egui::Ui,
        palette: &Palette,
        now: &Result<u64, SystemTimeError>,
    ) -> Option<(f64, f64)> {
        let quakes = self.view.events()?;
        let mut focus = None;
        let mut order: Vec<&Quake> = quakes.events.iter().collect();
        order.sort_by(|left, right| right.magnitude.total_cmp(&left.magnitude));
        egui::ScrollArea::vertical()
            .max_height(190.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for quake in order.iter().take(LIST_LIMIT) {
                    let age = match now {
                        Ok(now) => format!(" · {}", time::age_label(*now, quake.time_ms)),
                        Err(error) => format!(" · age unavailable: {error}"),
                    };
                    let label = format!(
                        "M{:.1} ({}) · {}\n{:.0} km deep{age}",
                        quake.magnitude, quake.magnitude_type, quake.place, quake.depth_km
                    );
                    ui.horizontal(|ui| {
                        let picked = ui
                            .add_sized(
                                [(ui.available_width() - 68.0).max(60.0), 52.0],
                                toggle_button(
                                    palette,
                                    &label,
                                    self.selected.as_deref() == Some(quake.id.as_str()),
                                ),
                            )
                            .on_hover_text(tooltip(quake, now))
                            .clicked();
                        if picked {
                            self.selected = Some(quake.id.clone());
                        }
                        if ui
                            .add_sized([48.0, 52.0], action_button(palette, "Focus"))
                            .on_hover_text("Centre the map on this epicentre.")
                            .clicked()
                        {
                            self.selected = Some(quake.id.clone());
                            focus = Some((quake.lat, quake.lon));
                        }
                    });
                }
            });
        if quakes.events.len() > LIST_LIMIT {
            ui.label(
                RichText::new(format!(
                    "Showing the {LIST_LIMIT} strongest of {} events.",
                    quakes.events.len()
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        focus
    }
}

/// The selected event's own record: the scale, depth, origin time, review
/// status and provider page, because a magnitude alone is not an event.
fn details(ui: &mut egui::Ui, title: &str, quake: &Quake) {
    ui.add_space(theme::SPACE_S);
    ui.label(RichText::new(title).size(theme::TYPE_LABEL).weak());
    ui.label(
        RichText::new(format!("M{:.1} · {}", quake.magnitude, quake.place))
            .size(theme::TYPE_BODY)
            .strong(),
    );
    let mut lines = vec![
        format!("Scale {}", quake.magnitude_type),
        format!(
            "{:.1} km deep · {}",
            quake.depth_km,
            quake.depth_band().label()
        ),
        match time::format_utc(quake.time_ms) {
            Ok(when) => when,
            Err(error) => format!("Origin time unavailable: {error}"),
        },
        format!("Status {}", quake.status),
    ];
    if let Some(felt) = quake.felt {
        lines.push(format!("Felt reports · {felt}"));
    }
    if let Some(alert) = &quake.alert {
        lines.push(format!("PAGER alert level {alert}"));
    }
    if quake.tsunami {
        lines.push(String::from("Tsunami flag set by the provider"));
    }
    for line in lines {
        ui.label(RichText::new(line).size(theme::TYPE_SMALL).weak());
    }
    ui.hyperlink_to("USGS event page", &quake.url);
}

fn nearest<'a>(hits: &'a [(&'a Quake, Pos2, f32)], at: Pos2) -> Option<&'a (&'a Quake, Pos2, f32)> {
    hits.iter()
        .filter(|(_, position, radius)| position.distance(at) <= (radius + 6.0).max(12.0))
        .min_by(|left, right| left.1.distance(at).total_cmp(&right.1.distance(at)))
}

fn paint_tooltip(painter: &Painter, rect: Rect, at: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(String::from(text), FontId::monospace(10.0), TEXT);
    let padding = Vec2::splat(6.0);
    let size = galley.size() + 2.0 * padding;
    let origin = Pos2::new(
        (at.x + 14.0).clamp(
            rect.left() + 4.0,
            (rect.right() - size.x - 4.0).max(rect.left() + 4.0),
        ),
        (at.y - size.y - 12.0).clamp(
            rect.top() + 4.0,
            (rect.bottom() - size.y - 4.0).max(rect.top() + 4.0),
        ),
    );
    painter.rect_filled(
        Rect::from_min_size(origin, size),
        CornerRadius::same(4),
        Color32::from_black_alpha(220),
    );
    painter.galley(origin + padding, galley, TEXT);
}

fn tooltip(quake: &Quake, now: &Result<u64, SystemTimeError>) -> String {
    let mut lines = vec![
        format!(
            "M{:.1} ({}) · {}",
            quake.magnitude, quake.magnitude_type, quake.place
        ),
        format!(
            "{:.0} km deep · {}",
            quake.depth_km,
            quake.depth_band().label()
        ),
    ];
    match now {
        Ok(now) => lines.push(format!(
            "{} · {}",
            time::age_label(*now, quake.time_ms),
            quake.status
        )),
        Err(error) => lines.push(format!("{} · age unavailable: {error}", quake.status)),
    }
    if quake.tsunami {
        lines.push(String::from("Tsunami flag set by the provider"));
    }
    if let Some(alert) = &quake.alert {
        lines.push(format!("PAGER alert level {alert}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quake(magnitude: f64, depth_km: f64) -> Quake {
        Quake {
            id: String::from("id"),
            magnitude,
            magnitude_type: String::from("mww"),
            place: String::from("Somewhere"),
            time_ms: 1_793_553_116_053,
            depth_km,
            lat: 35.0,
            lon: -97.0,
            tsunami: false,
            alert: None,
            status: String::from("reviewed"),
            felt: None,
            url: String::from("https://earthquake.usgs.gov/e"),
        }
    }

    #[test]
    fn symbol_area_grows_with_magnitude_and_stops_growing() {
        assert!(radius(3.0) < radius(5.0));
        assert!(radius(5.0) < radius(7.0));
        assert!(radius(9.0) < radius(9.5));
        assert_eq!(radius(9.5), 20.0, "the symbol has a ceiling");
        assert!(radius(2.0) >= 3.0);
    }

    #[test]
    fn depth_bands_get_distinct_colours() {
        let colours: Vec<Color32> = BANDS.iter().map(|band| colour(*band)).collect();
        for (index, left) in colours.iter().enumerate() {
            for right in colours.iter().skip(index + 1) {
                assert_ne!(left, right, "two depth bands share a colour");
            }
        }
    }

    #[test]
    fn a_tooltip_states_the_scale_depth_and_status() {
        let quake = quake(5.4, 12.0);
        let text = tooltip(&quake, &Ok(1_793_560_000_000));
        assert!(text.contains("M5.4 (mww)"));
        assert!(text.contains("12 km deep"));
        assert!(text.contains("0–20 km"));
        assert!(text.contains("reviewed"));
        assert!(text.contains("1 h old"));
    }

    #[test]
    fn a_tooltip_without_a_usable_clock_says_so() {
        let quake = quake(5.4, 12.0);
        // The epoch is always behind the clock now, so this really is the
        // error a clock that cannot be read produces.
        let clock: Result<u64, SystemTimeError> = std::time::SystemTime::UNIX_EPOCH
            .duration_since(std::time::SystemTime::now())
            .map(|elapsed| elapsed.as_millis() as u64);
        assert!(clock.is_err(), "the epoch should trail the clock");
        let text = tooltip(&quake, &clock);
        assert!(text.contains("reviewed"));
        assert!(text.contains("age unavailable"));
        assert!(!text.contains("old"));
    }

    #[test]
    fn a_new_layer_is_hidden_and_requests_nothing() {
        let layer = QuakeLayer::new();
        assert!(!layer.shown);
        assert!(layer.view.events().is_none());
    }

    #[test]
    fn a_hidden_layer_stays_idle_however_long_it_runs() {
        let mut layer = QuakeLayer::new();
        layer.update(Instant::now());
        assert!(matches!(layer.view.state(), QuakeState::Idle));
    }
}
