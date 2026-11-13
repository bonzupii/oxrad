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
//! The radar station catalog: cached discovery, refresh, search and the
//! station controls in the rail.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::canvas::{due_after, provenance};
use crate::engine::Served;
use crate::place::{self, Place};
use crate::stations::{self, SiteId, Station, StationError, StationFeed, StationOutcome};
use crate::theme;

const STATION_REFRESH_SECS: u64 = 6 * 60 * 60;
const STATION_RETRY_SECS: u64 = 15 * 60;
const STATION_LIST_HEIGHT: f32 = 220.0;

/// What a finished discovery round changed, for the shell to act on.
pub(crate) struct StationReport {
    pub(crate) notice: Option<String>,
    pub(crate) pending: Option<SiteId>,
}

pub(crate) struct StationPanel {
    list: Result<Served<Vec<Station>>, StationError>,
    feed: StationFeed,
    refresh_due: Instant,
    pending: Option<SiteId>,
    query: String,
    picker_open: bool,
}

impl StationPanel {
    pub(crate) fn new(acquire: &Acquire, restored: Option<&SiteId>) -> Self {
        Self {
            list: Err(StationError::NoStations),
            feed: StationFeed::new(acquire.clone()),
            refresh_due: due_after(Duration::from_secs(STATION_REFRESH_SECS)),
            pending: restored.cloned(),
            query: String::new(),
            picker_open: restored.is_none(),
        }
    }

    pub(crate) const fn result(&self) -> Result<&[Station], &StationError> {
        match &self.list {
            Ok(served) => Ok(served.value.as_slice()),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn served(&self) -> Option<&Served<Vec<Station>>> {
        self.list.as_ref().ok()
    }

    pub(crate) fn find(&self, id: &SiteId) -> Option<&Station> {
        let Ok(served) = &self.list else {
            return None;
        };
        stations::find(&served.value, id)
    }

    pub(crate) const fn close_picker(&mut self) {
        self.picker_open = false;
    }

    pub(crate) fn set_pending(&mut self, site: Option<SiteId>) {
        self.pending = site;
    }

    /// Polls discovery and rotates the feed when its next round is due.
    pub(crate) fn update(&mut self, now: Instant, acquire: &Acquire) -> Option<StationReport> {
        if let Some(outcome) = self.feed.poll() {
            return Some(self.apply(outcome));
        }
        if self.feed.is_finished() && now >= self.refresh_due {
            self.feed = StationFeed::new(acquire.clone());
            self.refresh_due = due_after(Duration::from_secs(STATION_REFRESH_SECS));
        }
        None
    }

    fn apply(&mut self, outcome: StationOutcome) -> StationReport {
        match outcome {
            StationOutcome::Ready(discovery) => {
                self.refresh_due = due_after(Duration::from_secs(STATION_REFRESH_SECS));
                self.list = Ok(discovery);
                StationReport {
                    notice: None,
                    pending: self.pending.take(),
                }
            }
            StationOutcome::Failed(error) => {
                self.refresh_due = due_after(Duration::from_secs(STATION_RETRY_SECS));
                let notice = if self.list.is_err() {
                    self.list = Err(error);
                    None
                } else {
                    Some(format!(
                        "Live station discovery failed; using cached station metadata: {error}"
                    ))
                };
                StationReport {
                    notice,
                    pending: None,
                }
            }
        }
    }

    /// The station controls in the rail. Returns the site the reader chose.
    pub(crate) fn section(
        &mut self,
        ui: &mut egui::Ui,
        site: Option<&SiteId>,
        place: Option<&Place>,
    ) -> Option<SiteId> {
        ui.label(
            RichText::new("Radar station")
                .size(theme::TYPE_TITLE)
                .strong(),
        );
        ui.add_space(theme::SPACE_XS);
        match (site, site.and_then(|id| self.find(id))) {
            (Some(site), Some(station)) => {
                ui.label(
                    RichText::new(format!("{site} · {}", station.name))
                        .size(14.0)
                        .strong(),
                );
                if let Some(place) = place
                    && place.in_united_states()
                    && let Ok(served) = &self.list
                    && let Ok(Some(nearest)) =
                        place::nearest_station(&served.value, place.lat, place.lon)
                {
                    ui.label(
                        RichText::new(format!(
                            "Nearest radar to {}: {} km {}.",
                            place.name,
                            nearest.distance_km.round(),
                            place::compass(nearest.bearing_deg)
                        ))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                    );
                }
                egui::CollapsingHeader::new("Station details")
                    .id_salt("station-details")
                    .show(ui, |ui| station_details(ui, station));
            }
            _ => {
                ui.label(RichText::new("No radar station selected").size(theme::TYPE_BODY));
            }
        }
        if let Some(place) = place
            && !place.in_united_states()
        {
            ui.label(
                RichText::new(format!(
                    "No supported radar source covers {}.",
                    place.country
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        ui.add_space(theme::SPACE_S);
        if ui
            .button(if self.picker_open {
                "Close station list"
            } else {
                "Change radar station"
            })
            .clicked()
        {
            self.picker_open = !self.picker_open;
        }
        if self.picker_open {
            self.choices(ui, site)
        } else {
            None
        }
    }

    fn choices(&mut self, ui: &mut egui::Ui, site: Option<&SiteId>) -> Option<SiteId> {
        ui.add_space(6.0);
        let mut query = std::mem::take(&mut self.query);
        ui.add_sized(
            [ui.available_width(), 28.0],
            egui::TextEdit::singleline(&mut query).hint_text("Station name or identifier"),
        );
        let mut chosen: Option<SiteId> = None;
        if let Some(served) = self.served() {
            provenance(ui, served);
        }
        match &self.list {
            Err(error) => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Radar site catalog unavailable: {error}"),
                );
            }
            Ok(served) => {
                egui::ScrollArea::vertical()
                    .max_height(STATION_LIST_HEIGHT)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for station in served
                            .value
                            .iter()
                            .filter(|station| stations::matches(station, &query))
                        {
                            let selected = site == Some(&station.id);
                            let fill = if selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                ui.visuals().faint_bg_color
                            };
                            if ui
                                .add_sized(
                                    [ui.available_width(), 26.0],
                                    egui::Button::new(format!("{} · {}", station.id, station.name))
                                        .fill(fill),
                                )
                                .clicked()
                            {
                                chosen = Some(station.id.clone());
                            }
                        }
                    });
            }
        }
        self.query = query;
        chosen
    }
}

fn station_details(ui: &mut egui::Ui, station: &Station) {
    let capabilities: Vec<&str> = station
        .capabilities()
        .iter()
        .map(|product| product.label())
        .collect();
    let dual_polarization = station
        .supports(crate::product::RadarProduct::DifferentialReflectivity)
        && station
            .capabilities()
            .iter()
            .any(|product| product.is_dual_polarization());
    ui.label(
        RichText::new(format!(
            "{} · {} · {}{}",
            station.source.label(),
            station.kind.label(),
            if capabilities.is_empty() {
                String::from("no supported products")
            } else {
                capabilities.join(", ")
            },
            if dual_polarization {
                " · dual-polarization"
            } else {
                ""
            }
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    )
    .on_hover_text(
        station
            .capabilities()
            .iter()
            .map(|product| {
                let (minimum, maximum) = product.value_range();
                format!(
                    "{} ({} to {} {}): {}",
                    product.id(),
                    minimum,
                    maximum,
                    product.unit(),
                    product.description()
                )
            })
            .collect::<Vec<String>>()
            .join("\n"),
    );
    if let Some(elevation) = station.elevation_m {
        ui.label(
            RichText::new(format!("Site elevation {elevation:.0} m"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    if let Some(status) = &station.status {
        ui.label(
            RichText::new(format!("RDA status: {status}"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    if let Some(last_data) = &station.last_data {
        ui.label(
            RichText::new(format!("Level II last received: {last_data}"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    ui.label(
        RichText::new(format!(
            "Source {} via {}. Radar observations come from this station, not from your town.",
            station.source.id(),
            station.source.provider()
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
}
