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
//! The wildfire layer: its rail section, the nearest fires, and the perimeters
//! drawn over the map.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Shape, Stroke};

use crate::acquire::Acquire;
use crate::canvas::{provenance, toggle_button};
use crate::fire::{self, FireKind, FireProvider, FireSet, FireState, FireView};
use crate::map::{self, MapView};
use crate::projection;
use crate::theme::{self, Palette};
use crate::time;

/// How many of the nearest fires the rail lists.
const LISTED: usize = 6;
/// A perimeter whose ring reaches past this many map widths is refused by the
/// projection as unusable rather than drawn as a smear.
const MAX_RING_WIDTHS: f32 = 4.0;
/// A perimeter smaller than this on screen is drawn as a marker instead, or a
/// twenty-acre fire would be invisible at any zoom a chaser works at.
const MARKER_BELOW: f32 = 8.0;
const MARKER_RADIUS: f32 = 2.5;
/// How far the map may move before the perimeters are asked for again.
const MOVE_KM: f64 = 50.0;
/// How long a set stands before it is refreshed even without movement.
const REFRESH_AFTER: Duration = Duration::from_secs(1_800);

/// The wildfire layer.
pub struct FireLayer {
    view: FireView,
    shown: bool,
    provider: FireProvider,
    chosen: bool,
    asked: Option<(f64, f64, FireProvider)>,
    asked_at: Option<Instant>,
    bounds: Option<(f64, f64, f64, f64)>,
}

impl FireLayer {
    pub const fn new(acquire: Acquire) -> Self {
        Self {
            view: FireView::new(acquire),
            shown: false,
            provider: FireProvider::Nifc,
            chosen: false,
            asked: None,
            asked_at: None,
            bounds: None,
        }
    }

    /// Takes in the engine's answer and follows the map while the layer is on.
    pub fn update(&mut self, now: Instant, view: &MapView) {
        self.view.update();
        if !self.shown {
            return;
        }
        let Ok((lat, lon)) = view.center_latlon() else {
            return;
        };
        let bounds = fire::bounds(lat, lon, view.span_km());
        self.bounds = Some(bounds);
        let wanted = if self.chosen {
            Some(self.provider)
        } else {
            FireProvider::for_point(lat, lon)
        };
        let Some(wanted) = wanted else {
            self.view.reset();
            self.asked = None;
            self.asked_at = None;
            return;
        };
        if wanted != self.provider {
            self.provider = wanted;
            self.view.reset();
            self.asked = None;
            self.asked_at = None;
        }
        let moved = match self.asked {
            Some((asked_lat, asked_lon, asked_provider)) => {
                asked_provider != self.provider
                    || distance_km(asked_lat, asked_lon, lat, lon) > MOVE_KM
            }
            None => true,
        };
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        if (moved || stale) && !self.view.started() {
            self.asked = Some((lat, lon, self.provider));
            self.asked_at = Some(now);
            self.view.start(self.provider, bounds);
        }
    }

    /// Whether the perimeters should be drawn.
    pub const fn is_shown(&self) -> bool {
        self.shown
    }

    /// Draws every ring the projection can place on the map.
    pub fn draw(
        &self,
        painter: &egui::Painter,
        transform: &map::MapTransform,
        rect: Rect,
        palette: &Palette,
    ) {
        if !self.is_shown() {
            return;
        }
        let Some(set) = self.view.set() else {
            return;
        };
        for fire in &set.perimeters {
            let colour = perimeter_colour(fire.kind, palette);
            let rings: Vec<Vec<Pos2>> = fire
                .rings
                .iter()
                .map(|ring| -> Vec<Pos2> {
                    ring.iter()
                        .filter_map(|(lat, lon)| transform.screen(*lat, *lon))
                        .collect()
                })
                .filter(|points| points.len() >= 3)
                .filter(|points| extent_of(points) <= rect.width() * MAX_RING_WIDTHS)
                .collect();
            let widest = rings
                .iter()
                .map(|points| extent_of(points))
                .fold(0.0, f32::max);
            if widest < MARKER_BELOW {
                if let Some(centre) = transform.screen(fire.centre.0, fire.centre.1) {
                    painter.circle_filled(centre, MARKER_RADIUS, colour);
                }
                continue;
            }
            for points in rings {
                painter.add(Shape::closed_line(points, Stroke::new(1.6_f32, colour)));
            }
        }
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette, centre: Option<(f64, f64)>) {
        ui.add_space(theme::SPACE_M);
        ui.label(RichText::new("Wildfires").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!(
                "{} · {} · no key",
                self.provider.label(),
                self.provider.coverage()
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        let mut provider = self.provider;
        ui.horizontal(|ui| {
            for candidate in FireProvider::ALL {
                ui.selectable_value(&mut provider, candidate, candidate.label())
                    .on_hover_text(format!("Read {} perimeters.", candidate.coverage()));
            }
        });
        if provider != self.provider {
            self.provider = provider;
            self.chosen = true;
            self.view.reset();
            self.asked = None;
            self.asked_at = None;
        }
        let label = if self.shown {
            "Fires: on"
        } else {
            "Fires: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Current mapped perimeters from the selected national catalogue, drawn over the map and listed by distance from its centre. The default catalogue follows the map centre.",
            )
            .clicked()
        {
            self.shown = !self.shown;
        }
        if !self.shown {
            ui.label(
                RichText::new("Turn the layer on to draw the mapped fire perimeters.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        let waiting = !self.chosen
            && centre
                .and_then(|(lat, lon)| FireProvider::for_point(lat, lon))
                .is_none();
        if waiting {
            ui.label(
                RichText::new(
                    "No open perimeter catalogue covers this map centre. Pan into the United States or Canada, or choose a catalogue explicitly.",
                )
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(set) = self.view.set() else {
            return;
        };
        if centre.is_some_and(|(lat, lon)| !self.provider.covers(lat, lon)) {
            ui.label(
                RichText::new(format!(
                    "The map centre is outside {}; pan into it for an on-map reading.",
                    self.provider.coverage()
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        summary(ui, set, palette);
        if let Some((lat, lon)) = centre {
            nearest(ui, set, lat, lon, palette);
        }
        largest(ui, set);
        ui.label(
            RichText::new(format!(
                "A perimeter is what a crew last mapped, not the fire's edge now; {} coverage.",
                self.provider.coverage()
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            FireState::Idle | FireState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!(
                            "Reading the {} perimeter service…",
                            self.provider.label()
                        ))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                    );
                });
            }
            FireState::Loading { last: Some(_) } => {
                ui.label(
                    RichText::new("Refreshing · still showing the last perimeters")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            FireState::Ready { served } => {
                provenance(ui, served);
            }
            FireState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Fires failed: {error}"),
                );
                if last.is_some() {
                    ui.label(
                        RichText::new("Still showing the last perimeters.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
        }
    }
}

/// The count, the acreage, and how many features were refused.
fn summary(ui: &mut egui::Ui, set: &FireSet, palette: &Palette) {
    let prescribed = set
        .perimeters
        .iter()
        .filter(|fire| fire.kind == FireKind::Prescribed)
        .count();
    let text = if set.rejected == 0 {
        format!(
            "{} perimeters · {:.0} ha mapped · {prescribed} prescribed",
            set.perimeters.len(),
            set.total_hectares()
        )
    } else {
        format!(
            "{} perimeters · {:.0} ha mapped · {prescribed} prescribed · {} unreadable",
            set.perimeters.len(),
            set.total_hectares(),
            set.rejected
        )
    };
    ui.label(
        RichText::new(text)
            .size(theme::TYPE_SMALL)
            .strong()
            .color(palette.text),
    );
    let newest = set
        .perimeters
        .iter()
        .filter_map(|fire| fire.mapped_at_ms)
        .max();
    if let Ok(now) = time::now_unix_ms() {
        ui.label(
            RichText::new(format!(
                "Newest perimeter {}",
                fire::mapped_label(newest, now)
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
}

/// The fires nearest the map centre, with their distance.
fn nearest(ui: &mut egui::Ui, set: &FireSet, latitude: f64, longitude: f64, palette: &Palette) {
    let ordered = set.nearest(latitude, longitude);
    ui.add_space(theme::SPACE_S);
    ui.label(
        RichText::new("Nearest to the map centre")
            .size(theme::TYPE_SMALL)
            .weak(),
    );
    for (_, fire) in ordered.iter().take(LISTED) {
        let distance = projection::inverse(latitude, longitude, fire.centre.0, fire.centre.1)
            .map_or_else(
                |_| String::from("distance unavailable"),
                |(distance, _)| format!("{distance:.0} km"),
            );
        let hectares = fire.area_hectares.map_or_else(
            || String::from("area not reported"),
            |hectares| format!("{hectares:.0} ha"),
        );
        let contained = fire.contained.map_or_else(
            || String::from("containment not reported"),
            |share| format!("{share:.0}% contained"),
        );
        ui.label(
            RichText::new(format!(
                "{} · {distance} · {hectares} · {contained}",
                fire.name
            ))
            .size(theme::TYPE_SMALL)
            .color(perimeter_colour(fire.kind, palette)),
        );
        let place = fire.state.as_ref().map_or_else(
            || String::from(fire.kind.label()),
            |state| format!("{} · {state}", fire.kind.label()),
        );
        ui.label(RichText::new(place).size(theme::TYPE_SMALL).weak());
    }
    if ordered.len() > LISTED {
        ui.label(
            RichText::new(format!(
                "{} more beyond these",
                ordered.len().saturating_sub(LISTED)
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
}

/// The largest fire in the set, which is worth knowing even far from it.
fn largest(ui: &mut egui::Ui, set: &FireSet) {
    let Some(fire) = set.largest() else {
        return;
    };
    let hectares = match fire.area_hectares {
        Some(hectares) => format!("{hectares:.0} ha"),
        None => return,
    };
    ui.add_space(theme::SPACE_S);
    ui.label(
        RichText::new(format!(
            "Largest: {} · {hectares} · {:.0} km across",
            fire.name,
            fire.span_km()
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}

fn distance_km(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    match projection::inverse(from_lat, from_lon, to_lat, to_lon) {
        Ok((distance, _)) => distance,
        Err(_) => f64::MAX,
    }
}

/// The larger side of a ring's screen box, which is how big it looks.
fn extent_of(points: &[Pos2]) -> f32 {
    let left = points
        .iter()
        .map(|point| point.x)
        .fold(f32::INFINITY, f32::min);
    let right = points
        .iter()
        .map(|point| point.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let top = points
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    let bottom = points
        .iter()
        .map(|point| point.y)
        .fold(f32::NEG_INFINITY, f32::max);
    (right - left).max(bottom - top)
}

/// Wildfires in the warning colour, prescribed burns dimmer so the two are
/// never read as the same kind of event.
fn perimeter_colour(kind: FireKind, palette: &Palette) -> Color32 {
    match kind {
        FireKind::Wildfire => palette.warning,
        FireKind::Prescribed => palette.accent.gamma_multiply(0.8),
        FireKind::Unclassified => palette.text.gamma_multiply(0.7),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fire::FirePerimeter;

    fn fire(name: &str, kind: FireKind, lat: f64, lon: f64) -> FirePerimeter {
        FirePerimeter {
            name: String::from(name),
            kind,
            area_hectares: Some(100.0),
            contained: Some(10.0),
            state: Some(String::from("US-KS")),
            mapped_at_ms: None,
            rings: vec![vec![
                (lat, lon),
                (lat, lon + 0.1),
                (lat + 0.1, lon + 0.1),
                (lat, lon),
            ]],
            centre: (lat, lon),
        }
    }

    fn set() -> FireSet {
        FireSet {
            provider: FireProvider::Nifc,
            perimeters: vec![
                fire("Near", FireKind::Wildfire, 39.0, -96.0),
                fire("Far", FireKind::Prescribed, 45.0, -110.0),
            ],
            rejected: 0,
        }
    }

    #[test]
    fn a_new_layer_is_off() {
        let layer = FireLayer::new(Acquire::offline());
        assert!(!layer.is_shown());
        assert!(matches!(layer.view.state(), FireState::Idle));
    }

    #[test]
    fn the_nearest_fire_is_the_one_at_the_centre() {
        let set = set();
        let ordered = set.nearest(39.0, -96.0);
        assert_eq!(
            ordered.first().map(|(_, fire)| fire.name.as_str()),
            Some("Near")
        );
        assert!(ordered[0].0 < ordered[1].0);
    }

    #[test]
    fn a_prescribed_burn_is_not_drawn_as_a_wildfire() {
        let palette = theme::Theme::Dark.palette();
        let wildfire = perimeter_colour(FireKind::Wildfire, &palette);
        let prescribed = perimeter_colour(FireKind::Prescribed, &palette);
        assert_ne!(wildfire, prescribed);
    }
}
