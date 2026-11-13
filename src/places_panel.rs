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
//! Place search and the selected place card. The panel owns the query, the
//! search worker and the place the rest of the window follows.

use std::time::Instant;

use eframe::egui::{self, CornerRadius, RichText, Vec2};

use crate::acquire::Acquire;
use crate::place::{self, Place, SearchInteraction, SearchState};
use crate::station_panel::StationPanel;
use crate::stations::{Station, StationError};
use crate::theme;

const SEARCH_ROW_HEIGHT: f32 = 32.0;

pub(crate) struct PlacesPanel {
    search: SearchInteraction,
    place: Option<Place>,
    focus_search: bool,
}

impl PlacesPanel {
    pub(crate) fn new(acquire: &Acquire, place: Option<Place>) -> Self {
        Self {
            search: SearchInteraction::new(acquire.clone()),
            place,
            focus_search: true,
        }
    }

    pub(crate) const fn place(&self) -> Option<&Place> {
        self.place.as_ref()
    }

    pub(crate) fn set_place(&mut self, place: Option<Place>) {
        self.place = place;
    }

    pub(crate) fn update(&mut self, now: Instant) {
        self.search.update(now);
    }

    pub(crate) fn clear_search(&mut self) {
        self.search.clear();
    }

    pub(crate) const fn is_busy(&self) -> bool {
        matches!(self.search.state(), SearchState::Working { .. })
    }

    /// The search controls. Returns a place the reader selected.
    pub(crate) fn search_section(
        &mut self,
        ui: &mut egui::Ui,
        stations: &StationPanel,
        notice: &mut Option<String>,
    ) -> Option<Place> {
        let mut submit = false;
        let mut edited = false;
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 82.0).max(80.0);
            let response = ui.add_sized(
                [width, SEARCH_ROW_HEIGHT],
                egui::TextEdit::singleline(self.search.query_mut())
                    .hint_text("Town or place")
                    .margin(Vec2::new(10.0, 7.0)),
            );
            edited = response.changed();
            let shortcut = ui.input(|input| input.key_pressed(egui::Key::Slash));
            if self.focus_search || shortcut {
                response.request_focus();
                self.focus_search = false;
            }
            if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            if ui
                .add_sized([72.0, SEARCH_ROW_HEIGHT], egui::Button::new("Search"))
                .clicked()
            {
                submit = true;
            }
        });
        if edited {
            self.search.edited(Instant::now());
        }
        if submit && !self.search.submit() {
            *notice = Some(String::from("Type a town or place name first."));
        }
        ui.add_space(theme::SPACE_S);
        let mut chosen: Option<Place> = None;
        match self.search.state() {
            SearchState::Idle => {
                let hint = if self.place.is_some() {
                    "Type to search for another town."
                } else {
                    "Type a town or place; matches appear as you type."
                };
                ui.label(RichText::new(hint).size(theme::TYPE_SMALL).weak());
            }
            SearchState::Working { query } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Looking up {query}…"))
                            .size(theme::TYPE_LABEL)
                            .weak(),
                    );
                });
            }
            SearchState::Failed { query, error } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Could not search for {query}: {error}"),
                );
            }
            SearchState::Ready { places } => {
                crate::canvas::provenance(ui, places);
                chosen = search_results(ui, &places.value, stations.result());
            }
        }
        ui.add_space(theme::SPACE_XS);
        ui.label(
            RichText::new("Place search © OpenStreetMap contributors, via Photon.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        chosen
    }

    pub(crate) fn place_card(&self, ui: &mut egui::Ui) {
        let Some(place) = &self.place else {
            return;
        };
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::same(10))
            .corner_radius(CornerRadius::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(place.label()).size(14.0).strong());
                ui.label(
                    RichText::new(format!(
                        "{} · forecast point {}, {}",
                        place.country, place.lat, place.lon
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            });
    }
}

fn search_results(
    ui: &mut egui::Ui,
    places: &[Place],
    stations: Result<&[Station], &StationError>,
) -> Option<Place> {
    if places.is_empty() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "No place matches that search. Check the spelling.",
        );
        return None;
    }
    let mut chosen: Option<Place> = None;
    for place in places {
        let detail = if !place.in_united_states() {
            format!("{} · global radar composite", place.country)
        } else {
            match stations {
                Ok(list) => match place::nearest_station(list, place.lat, place.lon) {
                    Ok(Some(nearest)) => format!(
                        "{} · {} · {:.0} km {}",
                        place.country,
                        nearest.station.id,
                        nearest.distance_km,
                        place::compass(nearest.bearing_deg)
                    ),
                    Ok(None) => format!("{} · no radar source", place.country),
                    Err(error) => format!("{} · {error}", place.country),
                },
                Err(error) => format!("{} · station feed unavailable: {error}", place.country),
            }
        };
        let label = format!("{}\n{}", place.label(), detail);
        if ui
            .add_sized(
                [ui.available_width(), 52.0],
                egui::Button::new(RichText::new(label).size(theme::TYPE_LABEL))
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(CornerRadius::same(4)),
            )
            .clicked()
        {
            chosen = Some(place.clone());
        }
    }
    chosen
}
