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
//! The Earth-system layers that follow the map, held in one place so the
//! application shell does not have to name every one of them.
//!
//! Each layer owns its own fetcher, its own retry schedule and its own rail
//! section. This module only keeps them in the order the rail shows them and
//! hands them the same map view and clock, so a new domain is one field and
//! two lines here rather than a handful of inserts in the shell.

use std::time::Instant;

use eframe::egui::{self, Painter, Rect};

use crate::climate_layer::ClimateLayer;
use crate::environment_layer::EnvironmentLayer;
use crate::fire_layer::FireLayer;
use crate::hydro_layer::HydroLayer;
use crate::land_layer::LandLayer;
use crate::map::{MapTransform, MapView};
use crate::observations::ObservationSet;
use crate::observations_layer::ObservationLayer;
use crate::ocean_layer::OceanLayer;
use crate::quality::AirQuality;
use crate::quality_layer::AirLayer;
use crate::satellite::SatelliteLayer;
use crate::seaice_layer::SeaIceLayer;
use crate::snow_layer::SnowLayer;
use crate::sounding::Sounding;
use crate::sounding_layer::SoundingLayer;
use crate::spaceweather::SpaceWeather;
use crate::spaceweather_layer::SpaceWeatherLayer;
use crate::theme::Palette;

/// Every layer that follows the map centre.
pub struct Domains {
    satellite: SatelliteLayer,
    hydro: HydroLayer,
    space_weather: SpaceWeatherLayer,
    air: AirLayer,
    climate: ClimateLayer,
    fires: FireLayer,
    environment: EnvironmentLayer,
    ocean: OceanLayer,
    stations_reported: ObservationLayer,
    snowpack: SnowLayer,
    profile: SoundingLayer,
    sea_ice: SeaIceLayer,
    land_surface: LandLayer,
}

impl Domains {
    pub fn new() -> Self {
        Self {
            satellite: SatelliteLayer::new(),
            hydro: HydroLayer::new(),
            space_weather: SpaceWeatherLayer::new(),
            air: AirLayer::new(),
            climate: ClimateLayer::new(),
            fires: FireLayer::new(),
            environment: EnvironmentLayer::new(),
            ocean: OceanLayer::new(),
            stations_reported: ObservationLayer::new(),
            snowpack: SnowLayer::new(),
            profile: SoundingLayer::new(),
            sea_ice: SeaIceLayer::new(),
            land_surface: LandLayer::new(),
        }
    }

    /// Follows the map centre for every layer that needs it.
    pub fn update(&mut self, now: Instant, context: &egui::Context, view: &MapView) {
        self.satellite.update(context, view);
        self.hydro.update(now, view);
        self.space_weather.update(now);
        self.air.update(now, view);
        self.climate.update(now, view);
        self.fires.update(now);
        self.environment.update(now, view);
        self.ocean.update(now, view);
        self.stations_reported.update(now, view);
        self.snowpack.update(now, view);
        self.profile.update(now, view);
        self.sea_ice.update(now);
        self.land_surface.update(now, view);
    }

    /// The rail sections that sit above the earthquake section.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette, centre: Option<(f64, f64)>) {
        self.satellite.rail(ui, palette);
        self.hydro.rail(ui, palette);
        self.space_weather.rail(ui, palette);
        self.air.rail(ui, palette);
        self.climate.rail(ui, palette);
        self.fires.rail(ui, palette, centre);
        self.environment.rail(ui, palette);
        self.ocean.rail(ui, palette);
        self.stations_reported.rail(ui, palette, centre);
        self.snowpack.rail(ui, palette);
        self.profile.rail(ui, palette);
        self.sea_ice.rail(ui, palette);
        self.land_surface.rail(ui, palette);
    }

    /// The satellite imagery, drawn under everything else on the map.
    pub fn draw_satellite(&self, ui: &egui::Ui, transform: &MapTransform, rect: Rect) {
        self.satellite.draw(ui, transform, rect);
    }

    /// The fire detections, drawn with the other point layers.
    pub fn draw_fires(
        &self,
        painter: &Painter,
        transform: &MapTransform,
        rect: Rect,
        palette: &Palette,
    ) {
        self.fires.draw(painter, transform, rect, palette);
    }

    /// The surface station plots, drawn with the other point layers.
    pub fn draw_stations(&self, painter: &Painter, transform: &MapTransform, palette: &Palette) {
        self.stations_reported.draw(painter, transform, palette);
    }

    /// What the surface stations are reporting, for the information radio.
    pub const fn stations(&self) -> Option<&ObservationSet> {
        self.stations_reported.stations()
    }

    /// The air-quality record, for the information radio.
    pub const fn air(&self) -> Option<&AirQuality> {
        self.air.record()
    }

    /// The space-weather record, for the information radio.
    pub const fn space(&self) -> Option<&SpaceWeather> {
        self.space_weather.record()
    }

    /// The vertical profile, for the information radio.
    pub const fn sounding(&self) -> Option<&Sounding> {
        self.profile.record()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_domain_set_holds_nothing_yet() {
        let domains = Domains::new();
        assert!(domains.stations().is_none());
        assert!(domains.air().is_none());
        assert!(domains.space().is_none());
        assert!(domains.sounding().is_none());
    }
}
