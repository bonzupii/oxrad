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
//! The application window: place search, radar map, forecast, and status.

use std::fmt::Write;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Stroke, Vec2,
};

use crate::config::{self, Config};
use crate::display::{Display, DisplayState, Frame};
use crate::forecast::{Forecast, ForecastError, ForecastFeed, ForecastOutcome, ForecastPeriod};
use crate::gpu::RadarGpu;
use crate::map::{self, MapError, MapView, WorldMap};
use crate::overlay;
use crate::pipeline::{Pipeline, PipelineState};
use crate::place::{self, Place, PlaceSearch, SearchOutcome};
use crate::product::RadarProduct;
use crate::provider::{JsonError, timestamp_label};
use crate::radar::GateValue;
use crate::render::{self, ViewTransform};
use crate::stations::{self, SiteId, Station, StationError, StationFeed, StationOutcome};
use crate::time;

const BACKGROUND: Color32 = Color32::from_rgb(8, 10, 14);
const CARD: Color32 = Color32::from_rgb(18, 22, 30);
const TEXT: Color32 = Color32::from_rgb(214, 221, 232);
const MUTED: Color32 = Color32::from_rgb(156, 168, 184);
const ERROR: Color32 = Color32::from_rgb(255, 122, 122);
const STALE: Color32 = Color32::from_rgb(255, 196, 64);
const HIGHLIGHT: Color32 = Color32::from_rgb(40, 64, 96);
const STALE_AFTER_MS: u64 = 15 * 60 * 1000;
const BUSY_REPAINT_MS: u64 = 100;
const IDLE_REPAINT_MS: u64 = 1000;
const RAIL_WIDTH: f32 = 300.0;
const FORECAST_WIDTH: f32 = 330.0;
const SEARCH_ROW_HEIGHT: f32 = 34.0;
const STATION_LIST_HEIGHT: f32 = 220.0;
const FORECAST_RETRY_BASE_SECS: u64 = 2;
const FORECAST_RETRY_MAX_SECS: u64 = 60;
const VIEW_RADAR_LIMIT: usize = 3;
const STATION_REFRESH_SECS: u64 = 6 * 60 * 60;
const STATION_RETRY_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SceneMode {
    Radar,
    Map,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RadarLayer {
    Shown,
    Hidden,
}

impl RadarLayer {
    const fn is_shown(self) -> bool {
        matches!(self, Self::Shown)
    }

    const fn toggled(self) -> Self {
        match self {
            Self::Shown => Self::Hidden,
            Self::Hidden => Self::Shown,
        }
    }
}

fn due_after(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}

fn nearby_station_ids(
    stations: &[Station],
    center_lat: f64,
    center_lon: f64,
    radius_km: f64,
    excluded: Option<&SiteId>,
    limit: usize,
) -> Result<Vec<SiteId>, crate::error::CoordinateError> {
    let mut candidates: Vec<(f64, SiteId)> = Vec::new();
    for station in stations {
        if excluded == Some(&station.id) {
            continue;
        }
        let (distance_km, _) =
            crate::projection::inverse(center_lat, center_lon, station.lat, station.lon)?;
        if distance_km <= radius_km {
            candidates.push((distance_km, station.id.clone()));
        }
    }
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
    candidates.truncate(limit);
    Ok(candidates.into_iter().map(|(_, id)| id).collect())
}

fn forecast_retry_delay(error: &ForecastError, attempt: u32) -> Duration {
    if let Some(value) = error.retry_after()
        && let Ok(delay) = crate::retry::retry_after_delay(value)
    {
        return delay;
    }
    let factor = 2_u64.saturating_pow(attempt.min(5));
    let seconds = FORECAST_RETRY_BASE_SECS
        .saturating_mul(factor)
        .min(FORECAST_RETRY_MAX_SECS);
    Duration::from_secs(seconds) + Duration::from_millis(u64::from(attempt).wrapping_mul(137) % 500)
}

enum SearchState {
    Idle,
    Working { query: String },
    Ready { places: Vec<Place> },
    Failed { query: String, error: JsonError },
}

enum ForecastState {
    Empty,
    Loading {
        place: Place,
        last: Option<Forecast>,
        attempt: u32,
    },
    Ready {
        forecast: Forecast,
    },
    Failed {
        place: Place,
        error: ForecastError,
        last: Option<Forecast>,
        retry_at: Option<Instant>,
        attempt: u32,
    },
}

struct ViewRadar {
    site: SiteId,
    pipeline: Pipeline,
    display: Display,
    failure_reported: bool,
}

pub struct OxradApp {
    gpu: Option<RadarGpu>,
    view_radars: Vec<ViewRadar>,
    stations: Result<Vec<Station>, StationError>,
    station_feed: StationFeed,
    station_refresh_due: Instant,
    pending_site: Option<SiteId>,
    config: Config,
    config_writable: bool,
    notice: Option<String>,
    query: String,
    search: PlaceSearch,
    search_state: SearchState,
    place: Option<Place>,
    site: Option<SiteId>,
    pipeline: Option<Pipeline>,
    display: Display,
    forecast: ForecastFeed,
    forecast_state: ForecastState,
    product: RadarProduct,
    station_query: String,
    station_picker_open: bool,
    first_run: bool,
    scene_mode: SceneMode,
    map: Result<WorldMap, MapError>,
    map_view: MapView,
    map_radar: RadarLayer,
    map_reload_due: Option<Instant>,
}

impl OxradApp {
    pub fn new(gpu: Option<RadarGpu>, initial: Option<SiteId>) -> Self {
        let mut config = Config::default();
        let mut config_writable = true;
        let mut notice = None;
        match config::load_default() {
            Ok(None) => {}
            Ok(Some(loaded)) => config = loaded,
            Err(error) => {
                config_writable = false;
                notice = Some(format!("Configuration ignored: {error}"));
            }
        }
        let restored_site = initial.or_else(|| config.site.clone());
        let restored_place = config.place.clone();
        let stations = match stations::load_cache() {
            Ok(Some(cached)) => Ok(cached),
            Ok(None) => Err(StationError::NoStations),
            Err(error) => Err(error),
        };
        let mut app = Self {
            gpu: gpu.clone(),
            view_radars: Vec::new(),
            stations,
            station_feed: StationFeed::new(),
            station_refresh_due: due_after(Duration::from_secs(STATION_REFRESH_SECS)),
            pending_site: restored_site.clone(),
            config,
            config_writable,
            notice,
            query: String::new(),
            search: PlaceSearch::new(),
            search_state: SearchState::Idle,
            place: restored_place.clone(),
            site: None,
            pipeline: None,
            display: Display::new(gpu),
            forecast: ForecastFeed::new(),
            forecast_state: ForecastState::Empty,
            product: RadarProduct::Reflectivity,
            station_query: String::new(),
            station_picker_open: restored_site.is_none(),
            first_run: true,
            scene_mode: SceneMode::Radar,
            map: WorldMap::load(),
            map_view: MapView::default(),
            map_radar: RadarLayer::Shown,
            map_reload_due: None,
        };
        if let Some(place) = &restored_place {
            match MapView::focus(place.lat, place.lon, 2_200.0) {
                Ok(view) => app.map_view = view,
                Err(error) => app.notice = Some(format!("Map centre unavailable: {error}")),
            }
            app.start_forecast(place, 0);
        }
        if let Some(id) = restored_site
            && let Ok(list) = &app.stations
        {
            if stations::find(list, &id).is_some() {
                app.open_site(id, false);
                app.pending_site = None;
            } else {
                app.notice = Some(format!(
                    "Site {id} is not in the discovered radar source; choose a station."
                ));
            }
        }
        app
    }

    fn apply_stations(&mut self, outcome: StationOutcome) {
        match outcome {
            StationOutcome::Ready(discovery) => {
                self.station_refresh_due = due_after(Duration::from_secs(STATION_REFRESH_SECS));
                self.stations = Ok(discovery.stations);
                if let Some(warning) = discovery.cache_warning {
                    self.notice = Some(format!(
                        "Station metadata is live, but the cache was not updated: {warning}"
                    ));
                }
                if self.site.is_none()
                    && let Some(pending) = self.pending_site.clone()
                    && let Ok(list) = &self.stations
                    && stations::find(list, &pending).is_some()
                {
                    self.open_site(pending, true);
                    self.pending_site = None;
                }
                if self.scene_mode == SceneMode::Map {
                    self.ensure_viewport_radars();
                }
            }
            StationOutcome::Failed(error) => {
                self.station_refresh_due = due_after(Duration::from_secs(STATION_RETRY_SECS));
                if self.stations.is_err() {
                    self.stations = Err(error);
                } else {
                    self.notice = Some(format!(
                        "Live station discovery failed; using cached station metadata: {error}"
                    ));
                }
            }
        }
    }

    fn open_site(&mut self, id: SiteId, persist: bool) {
        self.display.reset();
        self.pipeline = Some(Pipeline::start(id.clone()));
        self.site = Some(id.clone());
        self.station_picker_open = false;
        self.view_radars.clear();
        self.config.site = Some(id);
        if persist {
            self.save_config();
        }
        if self.scene_mode == SceneMode::Map {
            self.ensure_viewport_radars();
        }
    }

    fn choose_site(&mut self, id: SiteId) {
        self.search_state = SearchState::Idle;
        self.search.cancel();
        self.open_site(id, true);
    }

    fn clear_site(&mut self) {
        self.site = None;
        self.pipeline = None;
        self.display.reset();
        self.view_radars.clear();
        self.config.site = None;
    }

    fn save_config(&mut self) {
        if !self.config_writable {
            return;
        }
        if let Err(error) = config::save_default(&self.config) {
            self.notice = Some(format!("Could not save your location: {error}"));
        }
    }

    fn submit_search(&mut self) {
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            self.notice = Some(String::from("Type a town or place name first."));
            return;
        }
        self.notice = None;
        self.station_picker_open = false;
        match self.search.start(query.clone()) {
            Ok(()) => self.search_state = SearchState::Working { query },
            Err(error) => self.search_state = SearchState::Failed { query, error },
        }
    }

    fn focus_map_context(&mut self) {
        let place = self
            .place
            .as_ref()
            .map(|place| (place.lat, place.lon, 2_200.0));
        let station = self
            .site_station()
            .map(|station| (station.lat, station.lon, 950.0));
        let target = place.or(station);
        let Some((lat, lon, span)) = target else {
            return;
        };
        match MapView::focus(lat, lon, span) {
            Ok(view) => self.map_view = view,
            Err(error) => self.notice = Some(format!("Map centre unavailable: {error}")),
        }
    }

    fn ensure_viewport_radars(&mut self) {
        if !self.view_radars.is_empty() {
            return;
        }
        let Some(gpu) = self.gpu.clone() else {
            return;
        };
        let (center_lat, center_lon) = match self.map_view.center_latlon() {
            Ok(center) => center,
            Err(error) => {
                self.notice = Some(format!("Viewport radar centre unavailable: {error}"));
                return;
            }
        };
        let Ok(stations) = &self.stations else {
            return;
        };
        let radius_km = self.map_view.span_km() / 2.0 + 250.0;
        let sites = match nearby_station_ids(
            stations,
            center_lat,
            center_lon,
            radius_km,
            self.site.as_ref(),
            VIEW_RADAR_LIMIT,
        ) {
            Ok(sites) => sites,
            Err(error) => {
                self.notice = Some(format!("Viewport radar distance failed: {error}"));
                return;
            }
        };
        let mut layers = Vec::with_capacity(sites.len());
        for site in sites {
            layers.push(ViewRadar {
                pipeline: Pipeline::start(site.clone()),
                display: Display::new(Some(gpu.clone())),
                site,
                failure_reported: false,
            });
        }
        self.view_radars = layers;
    }

    fn reload_viewport_radars(&mut self) {
        self.view_radars.clear();
        self.ensure_viewport_radars();
    }

    fn refresh_view_radars(&mut self) {
        for layer in &mut self.view_radars {
            let elevation = layer
                .display
                .frame
                .as_ref()
                .and_then(|frame| frame.volume.elevation_for_product(self.product));
            if layer.display.frame.is_some() && !layer.display.rebuild(self.product, elevation) {
                layer.failure_reported = false;
            }
        }
    }

    fn choose_place(&mut self, place: &Place) {
        self.query.clear();
        self.search_state = SearchState::Idle;
        match MapView::focus(place.lat, place.lon, 2_200.0) {
            Ok(view) => self.map_view = view,
            Err(error) => self.notice = Some(format!("Map centre unavailable: {error}")),
        }
        self.place = Some(place.clone());
        self.config.place = Some(place.clone());
        self.start_forecast(place, 0);
        if !place.in_united_states() {
            self.clear_site();
            self.notice = Some(format!(
                "No supported radar source covers {}. The Open-Meteo forecast on the right is still available for {}.",
                place.country,
                place.label()
            ));
            self.save_config();
            return;
        }
        let station = match &self.stations {
            Ok(list) => match place::nearest_station(list, place.lat, place.lon) {
                Ok(station) => station,
                Err(error) => {
                    self.clear_site();
                    self.notice = Some(error.to_string());
                    self.save_config();
                    return;
                }
            },
            Err(error) => {
                let message = format!("Radar station catalog unavailable: {error}");
                self.clear_site();
                self.notice = Some(message);
                self.save_config();
                return;
            }
        };
        match station {
            Some(nearest) => {
                self.open_site(nearest.station.id, true);
                self.notice = None;
            }
            None => {
                self.clear_site();
                self.notice = Some(format!(
                    "No radar station is available for {}.",
                    place.label()
                ));
                self.save_config();
            }
        }
    }

    fn start_forecast(&mut self, place: &Place, attempt: u32) {
        let last = self.remembered_forecast(place);
        match self.forecast.start(place) {
            Ok(()) => {
                self.forecast_state = ForecastState::Loading {
                    place: place.clone(),
                    last,
                    attempt,
                };
            }
            Err(error) => {
                self.forecast_state = ForecastState::Failed {
                    place: place.clone(),
                    error,
                    last,
                    retry_at: None,
                    attempt,
                };
            }
        }
    }

    fn remembered_forecast(&self, place: &Place) -> Option<Forecast> {
        match &self.forecast_state {
            ForecastState::Ready { forecast } if forecast.place.same_place(place) => {
                Some(forecast.clone())
            }
            ForecastState::Loading { last, .. } | ForecastState::Failed { last, .. } => last
                .as_ref()
                .filter(|forecast| forecast.place.same_place(place))
                .cloned(),
            ForecastState::Ready { .. } | ForecastState::Empty => None,
        }
    }

    fn apply_search(&mut self, outcome: SearchOutcome) {
        match outcome {
            SearchOutcome::Results { places } => {
                self.search_state = SearchState::Ready { places };
            }
            SearchOutcome::Failed { query, error } => {
                self.search_state = SearchState::Failed { query, error };
            }
        }
    }

    fn apply_forecast(&mut self, outcome: ForecastOutcome) {
        match outcome {
            ForecastOutcome::Ready(forecast) => {
                if self
                    .place
                    .as_ref()
                    .is_some_and(|place| forecast.place.same_place(place))
                {
                    self.forecast_state = ForecastState::Ready { forecast };
                }
            }
            ForecastOutcome::Failed { place, error } => {
                if self
                    .place
                    .as_ref()
                    .is_some_and(|current| !place.same_place(current))
                {
                    return;
                }
                let (last, attempt) =
                    match std::mem::replace(&mut self.forecast_state, ForecastState::Empty) {
                        ForecastState::Loading { last, attempt, .. }
                        | ForecastState::Failed { last, attempt, .. } => (last, attempt),
                        ForecastState::Ready { forecast } => (Some(forecast), 0),
                        ForecastState::Empty => (None, 0),
                    };
                let retry_at = if error.is_transient() {
                    Instant::now().checked_add(forecast_retry_delay(&error, attempt))
                } else {
                    None
                };
                self.forecast_state = ForecastState::Failed {
                    place,
                    error,
                    last,
                    retry_at,
                    attempt,
                };
            }
        }
    }

    fn update_workers(&mut self) {
        if let Some(pipeline) = self.pipeline.as_mut()
            && let Some(volume) = pipeline.poll()
        {
            self.display.submit(volume, self.product);
        }
        self.display.poll();
        let mut layer_failure = None;
        for layer in &mut self.view_radars {
            if let Some(volume) = layer.pipeline.poll() {
                layer.display.submit(volume, self.product);
                layer.failure_reported = false;
            }
            layer.display.poll();
            let failure = layer
                .pipeline
                .state
                .failure()
                .map(|failure| failure.error.to_string())
                .or_else(|| {
                    layer
                        .display
                        .state
                        .error()
                        .map(std::string::ToString::to_string)
                });
            if let Some(message) = failure {
                if !layer.failure_reported {
                    layer_failure = Some(format!("Viewport radar {}: {message}", layer.site));
                    layer.failure_reported = true;
                }
            } else {
                layer.failure_reported = false;
            }
        }
        if let Some(message) = layer_failure {
            self.notice = Some(message);
        }
        if let Some(outcome) = self.station_feed.poll() {
            self.apply_stations(outcome);
        }
        if self.station_feed.is_finished() && Instant::now() >= self.station_refresh_due {
            self.station_feed = StationFeed::new();
            self.station_refresh_due = due_after(Duration::from_secs(STATION_REFRESH_SECS));
        }
        while let Some(outcome) = self.search.poll() {
            self.apply_search(outcome);
        }
        if let Some(outcome) = self.forecast.poll() {
            self.apply_forecast(outcome);
        }
        let retry = match &self.forecast_state {
            ForecastState::Failed {
                place,
                retry_at: Some(due),
                attempt,
                ..
            } if Instant::now() >= *due => Some((place.clone(), attempt.saturating_add(1))),
            ForecastState::Failed { .. }
            | ForecastState::Empty
            | ForecastState::Loading { .. }
            | ForecastState::Ready { .. } => None,
        };
        if let Some((place, attempt)) = retry {
            self.start_forecast(&place, attempt);
        }
    }

    fn site_station(&self) -> Option<&Station> {
        let site = self.site.as_ref()?;
        let Ok(list) = &self.stations else {
            return None;
        };
        stations::find(list, site)
    }

    fn failed(&self) -> bool {
        self.pipeline
            .as_ref()
            .is_some_and(|pipeline| pipeline.state.failure().is_some())
            || self.display.state.error().is_some()
    }

    fn busy(&self) -> bool {
        if self.failed() {
            return false;
        }
        matches!(self.search_state, SearchState::Working { .. })
            || matches!(self.forecast_state, ForecastState::Loading { .. })
            || (self.site.is_some()
                && (self.display.state.is_busy()
                    || self
                        .pipeline
                        .as_ref()
                        .is_some_and(|pipeline| pipeline.state.is_working())
                    || self.display.frame.is_none()))
    }

    fn phase(&self) -> &'static str {
        self.pipeline.as_ref().map_or_else(
            || {
                if self.site.is_some() {
                    "Idle"
                } else {
                    "Idle · No site selected"
                }
            },
            |pipeline| match &pipeline.state {
                PipelineState::Failed(_) => "Error",
                PipelineState::Working { .. } => {
                    if self.display.frame.is_some() {
                        "Refreshing"
                    } else {
                        pipeline.state.label()
                    }
                }
                PipelineState::Ready => match &self.display.state {
                    DisplayState::Ready => "Ready",
                    DisplayState::Unavailable(_) | DisplayState::Failed { .. } => "Render error",
                    DisplayState::Waiting | DisplayState::Building => "Preparing frame",
                },
            },
        )
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("header")
            .exact_size(46.0)
            .show_inside(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("oxrad").size(21.0).strong().color(TEXT));
                    ui.label(RichText::new("weather radar").size(12.0).color(MUTED));
                    ui.add_space(18.0);
                    if ui
                        .selectable_label(self.scene_mode == SceneMode::Radar, "Radar")
                        .clicked()
                    {
                        self.scene_mode = SceneMode::Radar;
                    }
                    if ui
                        .selectable_label(self.scene_mode == SceneMode::Map, "Map")
                        .clicked()
                    {
                        self.scene_mode = SceneMode::Map;
                        self.focus_map_context();
                        self.ensure_viewport_radars();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(place) = &self.place {
                            ui.label(
                                RichText::new(format!("Weather for {}", place.label()))
                                    .size(13.0)
                                    .color(TEXT),
                            );
                        } else {
                            ui.label(RichText::new("No town selected").size(13.0).color(MUTED));
                        }
                        ui.separator();
                        match (&self.site, self.site_station()) {
                            (Some(site), Some(station)) => {
                                ui.label(
                                    RichText::new(format!("Radar {site} · {}", station.name))
                                        .size(13.0)
                                        .color(TEXT),
                                );
                            }
                            (Some(site), None) => {
                                ui.label(
                                    RichText::new(format!("Radar {site}"))
                                        .size(13.0)
                                        .color(TEXT),
                                );
                            }
                            (None, _) => {
                                ui.label(RichText::new("No site selected").size(13.0).color(MUTED));
                            }
                        }
                    });
                });
            });
    }

    fn rail(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("rail")
            .exact_size(RAIL_WIDTH)
            .resizable(false)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new("Weather for").size(12.0).color(MUTED));
                        self.search_section(ui);
                        if self.place.is_some() {
                            ui.add_space(14.0);
                            self.place_card(ui);
                            ui.add_space(18.0);
                        }
                        ui.separator();
                        ui.add_space(12.0);
                        self.station_section(ui);
                        self.product_section(ui);
                        if let Some(notice) = &self.notice {
                            ui.add_space(12.0);
                            ui.colored_label(STALE, notice);
                        }
                    });
            });
    }

    fn search_section(&mut self, ui: &mut egui::Ui) {
        let mut submit = false;
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 74.0).max(80.0);
            let response = ui.add_sized(
                [width, SEARCH_ROW_HEIGHT],
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Town or place")
                    .margin(Vec2::new(8.0, 6.0)),
            );
            let shortcut = ui.input(|input| input.key_pressed(egui::Key::Slash));
            if self.first_run || shortcut {
                response.request_focus();
                self.first_run = false;
            }
            if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            if ui
                .add_sized([66.0, SEARCH_ROW_HEIGHT], egui::Button::new("Search"))
                .clicked()
            {
                submit = true;
            }
        });
        if submit {
            self.submit_search();
        }
        ui.add_space(8.0);
        let mut chosen: Option<Place> = None;
        match &self.search_state {
            SearchState::Idle => {
                let hint = if self.place.is_some() {
                    "Search another town to change the forecast."
                } else {
                    "Type a town, then press Enter."
                };
                ui.label(RichText::new(hint).size(11.0).color(MUTED));
            }
            SearchState::Working { query } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Looking up {query}…"))
                            .size(12.0)
                            .color(MUTED),
                    );
                });
            }
            SearchState::Failed { query, error } => {
                ui.colored_label(ERROR, format!("Could not search for {query}: {error}"));
            }
            SearchState::Ready { places } => {
                chosen = search_results(ui, places, &self.stations);
            }
        }
        if let Some(place) = chosen {
            self.choose_place(&place);
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("Place search data © OpenStreetMap contributors.")
                .size(9.0)
                .color(MUTED),
        );
    }

    fn place_card(&self, ui: &mut egui::Ui) {
        let Some(place) = &self.place else {
            return;
        };
        egui::Frame::new()
            .fill(CARD)
            .inner_margin(egui::Margin::same(10))
            .corner_radius(CornerRadius::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(place.label()).size(14.0).strong().color(TEXT));
                ui.label(
                    RichText::new(format!(
                        "{} · forecast point {}, {}",
                        place.country, place.lat, place.lon
                    ))
                    .size(10.0)
                    .color(MUTED),
                );
            });
    }

    fn product_section(&mut self, ui: &mut egui::Ui) {
        let capabilities: Vec<RadarProduct> = self
            .site_station()
            .map(|station| station.capabilities().to_vec())
            .unwrap_or_default();
        if capabilities.is_empty() {
            return;
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Product and tilt").size(12.0).color(MUTED));
        ui.add_space(4.0);
        let available_products: Vec<RadarProduct> = self
            .display
            .frame
            .as_ref()
            .map(|frame| {
                RadarProduct::ALL
                    .into_iter()
                    .filter(|product| frame.volume.supports_product(*product))
                    .collect()
            })
            .unwrap_or_default();
        let mut requested_product = None;
        ui.horizontal_wrapped(|ui| {
            for product in &capabilities {
                let available =
                    available_products.is_empty() || available_products.contains(product);
                let selected = *product == self.product;
                let label = format!("{} {}", product.id(), product.unit());
                let button = ui.add_enabled(
                    available,
                    egui::Button::new(label).fill(if selected { HIGHLIGHT } else { CARD }),
                );
                let button = if available {
                    button.on_hover_text(product.description())
                } else {
                    button.on_disabled_hover_text("Not carried by this sweep.")
                };
                if button.clicked() {
                    requested_product = Some(*product);
                }
            }
        });
        let tilt_options: Vec<(u8, f64, bool)> = self
            .display
            .frame
            .as_ref()
            .map(|frame| frame.volume.tilt_options_for(self.product))
            .unwrap_or_default();
        let mut requested_tilt = None;
        if tilt_options.is_empty() {
            ui.label(
                RichText::new("Tilts appear when a volume is loaded.")
                    .size(10.0)
                    .color(MUTED),
            );
        } else {
            ui.label(RichText::new("Tilt").size(11.0).color(MUTED));
            ui.horizontal_wrapped(|ui| {
                for (number, elevation, selected) in tilt_options {
                    let label = format!("{number} · {elevation:.2}°");
                    if ui
                        .add(egui::Button::new(label).fill(if selected { HIGHLIGHT } else { CARD }))
                        .clicked()
                        && !selected
                    {
                        requested_tilt = Some(number);
                    }
                }
            });
        }
        if let Some(product) = requested_product {
            self.product = product;
            if self.display.frame.is_some() && !self.display.rebuild(product, requested_tilt) {
                self.notice = Some(String::from(
                    "That product and tilt could not be prepared from the displayed volume.",
                ));
            }
            self.refresh_view_radars();
        } else if let Some(number) = requested_tilt
            && !self.display.rebuild(self.product, Some(number))
        {
            self.notice = Some(String::from(
                "That tilt is not available in the displayed volume.",
            ));
        } else if requested_tilt.is_some() {
            self.refresh_view_radars();
        }
    }

    fn station_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Radar station").size(12.0).color(MUTED));
        ui.add_space(4.0);
        match (&self.site, self.site_station()) {
            (Some(site), Some(station)) => {
                ui.label(
                    RichText::new(format!("{site} · {}", station.name))
                        .size(14.0)
                        .strong(),
                );
                if let Some(place) = &self.place
                    && place.in_united_states()
                    && let Ok(list) = &self.stations
                    && let Ok(Some(nearest)) = place::nearest_station(list, place.lat, place.lon)
                {
                    ui.label(
                        RichText::new(format!(
                            "Nearest radar to {}: {} km {}.",
                            place.name,
                            nearest.distance_km.round(),
                            place::compass(nearest.bearing_deg)
                        ))
                        .size(11.0)
                        .color(MUTED),
                    );
                }
                station_details(ui, station);
            }
            _ => {
                ui.label(RichText::new("No radar station selected").size(13.0));
            }
        }
        if let Some(place) = &self.place
            && !place.in_united_states()
        {
            ui.label(
                RichText::new(format!(
                    "No supported radar source covers {}.",
                    place.country
                ))
                .size(11.0)
                .color(MUTED),
            );
        }
        ui.add_space(8.0);
        if ui
            .button(if self.station_picker_open {
                "Close station list"
            } else {
                "Change radar station"
            })
            .clicked()
        {
            self.station_picker_open = !self.station_picker_open;
        }
        if self.station_picker_open
            && let Some(id) = self.station_choices(ui)
        {
            self.choose_site(id);
        }
    }

    fn station_choices(&mut self, ui: &mut egui::Ui) -> Option<SiteId> {
        ui.add_space(6.0);
        let mut query = std::mem::take(&mut self.station_query);
        ui.add_sized(
            [ui.available_width(), 28.0],
            egui::TextEdit::singleline(&mut query).hint_text("Station name or identifier"),
        );
        let mut chosen: Option<SiteId> = None;
        match &self.stations {
            Err(error) => {
                ui.colored_label(ERROR, format!("Radar site catalog unavailable: {error}"));
            }
            Ok(list) => {
                egui::ScrollArea::vertical()
                    .max_height(STATION_LIST_HEIGHT)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for station in list
                            .iter()
                            .filter(|station| stations::matches(station, &query))
                        {
                            let selected = self.site.as_ref() == Some(&station.id);
                            let fill = if selected { HIGHLIGHT } else { CARD };
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
        self.station_query = query;
        chosen
    }

    fn forecast_panel(&mut self, ui: &mut egui::Ui) {
        let mut retry = false;
        egui::Panel::right("forecast")
            .exact_size(FORECAST_WIDTH)
            .resizable(false)
            .show_inside(ui, |ui| {
                retry = self.forecast_footer(ui);
                ui.add_space(12.0);
                ui.label(RichText::new("Forecast").size(12.0).color(MUTED));
                ui.add_space(4.0);
                match &self.forecast_state {
                    ForecastState::Empty => {
                        ui.label(RichText::new("No town selected").size(14.0).strong());
                        ui.label(
                            RichText::new(
                                "Search for a town on the left to see its forecast here.",
                            )
                            .size(11.0)
                            .color(MUTED),
                        );
                    }
                    ForecastState::Loading { place, .. } => {
                        ui.label(RichText::new(place.label()).size(14.0).strong());
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new("Loading the forecast…")
                                    .size(12.0)
                                    .color(MUTED),
                            );
                        });
                    }
                    ForecastState::Failed {
                        place,
                        error,
                        last,
                        retry_at,
                        ..
                    } => {
                        ui.label(RichText::new(place.label()).size(14.0).strong());
                        ui.colored_label(ERROR, error.to_string());
                        if let Some(due) = retry_at {
                            let seconds = due
                                .saturating_duration_since(Instant::now())
                                .as_secs()
                                .saturating_add(1);
                            ui.label(
                                RichText::new(format!("Automatic retry in {seconds} s."))
                                    .size(11.0)
                                    .color(STALE),
                            );
                            retry = ui.button("Retry now").clicked();
                        } else {
                            retry = ui.button("Retry forecast").clicked();
                        }
                        if let Some(forecast) = last {
                            ui.add_space(8.0);
                            ui.colored_label(STALE, "Showing the last forecast received.");
                            ui.separator();
                            forecast_body(ui, forecast, true);
                        }
                    }
                    ForecastState::Ready { forecast } => {
                        forecast_body(ui, forecast, false);
                    }
                }
                ui.add_space(8.0);
            });
        if retry && let Some(place) = self.place.clone() {
            self.start_forecast(&place, 0);
        }
    }

    fn forecast_footer(&self, ui: &mut egui::Ui) -> bool {
        let mut retry = false;
        if let Some(place) = &self.place {
            egui::Panel::bottom("forecast_footer").show_inside(ui, |ui| {
                ui.separator();
                if matches!(self.forecast_state, ForecastState::Ready { .. }) {
                    retry = ui.button("Refresh forecast").clicked();
                    ui.add_space(4.0);
                }
                let note = self.site.as_ref().map_or_else(
                    || {
                        format!(
                            "Forecast point {} is the town you searched. No radar station is selected.",
                            place.coordinates()
                        )
                    },
                    |site| {
                        format!(
                            "Forecast point {} is the town you searched. Radar comes from {site}. They are different places.",
                            place.coordinates()
                        )
                    },
                );
                ui.label(RichText::new(note).size(10.0).color(MUTED));
                ui.label(
                    RichText::new(
                        "Forecast times are local to the forecast point; radar times are UTC.",
                    )
                    .size(10.0)
                    .color(MUTED),
                );
                ui.add_space(4.0);
            });
        }
        retry
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        let mut retry_pipeline = false;
        let mut retry_display = false;
        let failed = self.failed();
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                match (&self.site, self.site_station()) {
                    (Some(site), Some(station)) => {
                        ui.strong(format!(
                            "{site} · {} · {} ({})",
                            station.name,
                            self.product.label(),
                            self.product.id()
                        ));
                    }
                    (Some(site), None) => {
                        ui.strong(format!(
                            "{site} · {} ({})",
                            self.product.label(),
                            self.product.id()
                        ));
                    }
                    (None, _) => {
                        ui.strong("No radar station");
                    }
                }
                ui.separator();
                ui.label(self.phase());
                if let Some(frame) = &self.display.frame {
                    scan_status(ui, frame, failed);
                }
            });
            if let Some(failure) = self
                .pipeline
                .as_ref()
                .and_then(|pipeline| pipeline.state.failure())
            {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        ERROR,
                        format!(
                            "{} failed for {}: {}",
                            failure.operation.description(),
                            self.site.as_ref().map_or("no site", |site| site.as_str()),
                            failure.error
                        ),
                    );
                    if let Some(wait) = failure.retry_in {
                        ui.label(format!("Retry in {} s", wait.as_secs()));
                    }
                    if failure.waiting_for_next_volume {
                        ui.label("Waiting for a new volume or a manual retry.");
                    }
                    retry_pipeline = ui.button("Retry now").clicked();
                });
            }
            if let Some(error) = self.display.state.error() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(
                        ERROR,
                        format!(
                            "Radar image for {} failed: {error}",
                            self.site.as_ref().map_or("no site", |site| site.as_str())
                        ),
                    );
                    if self.display.state.retryable() {
                        retry_display = ui.button("Retry render").clicked();
                    }
                });
            }
            if let Some(frame) = &self.display.frame {
                diagnostics(ui, frame, self.site_station());
            }
            ui.add_space(6.0);
        });
        if retry_pipeline
            && let Some(pipeline) = self.pipeline.as_mut()
            && let Err(error) = pipeline.retry()
        {
            self.notice = Some(format!("Retry unavailable: {error}"));
        }
        if retry_display {
            self.display.retry();
        }
    }

    fn scene(&mut self, ui: &mut egui::Ui) {
        match self.scene_mode {
            SceneMode::Radar => self.radar_scene(ui),
            SceneMode::Map => self.map_scene(ui),
        }
    }

    fn map_scene(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
        let interaction = ui.interact(
            rect,
            ui.id().with("geographic-map"),
            egui::Sense::click_and_drag(),
        );
        self.map_input(ui, rect, &interaction);
        if self.map_reload_due.is_some_and(|due| Instant::now() >= due) {
            self.map_reload_due = None;
            self.reload_viewport_radars();
        }
        let transform = match self.map_view.transform(rect) {
            Ok(transform) => transform,
            Err(error) => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("The map is unavailable: {error}"),
                    FontId::proportional(15.0),
                    ERROR,
                );
                return;
            }
        };
        self.map_basemap(ui, rect);
        let radar_visible = self.map_radar(ui, rect, &transform);
        let clicked_site = self.map_stations(ui, rect, &transform, &interaction);
        self.map_place(ui, &transform);
        map::draw_scale(ui.painter(), rect, &transform);
        self.map_controls(ui, rect);
        self.map_readout(ui, rect, &transform, radar_visible);
        if let Some(id) = clicked_site {
            self.choose_site(id);
        }
    }

    fn map_input(&mut self, ui: &egui::Ui, rect: Rect, interaction: &egui::Response) {
        if interaction.dragged() {
            let delta = ui.input(|input| input.pointer.delta());
            match self.map_view.pan(rect, delta) {
                Ok(()) => self.schedule_viewport_reload(),
                Err(error) => self.notice = Some(format!("Map pan failed: {error}")),
            }
        }
        if interaction.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > 0.01
                && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
            {
                let factor = (f64::from(scroll) / 240.0).exp();
                match self.map_view.zoom_at(rect, pointer, factor) {
                    Ok(()) => self.schedule_viewport_reload(),
                    Err(error) => self.notice = Some(format!("Map zoom failed: {error}")),
                }
            }
        }
    }

    fn schedule_viewport_reload(&mut self) {
        let now = Instant::now();
        self.map_reload_due = now.checked_add(Duration::from_millis(700));
    }

    fn map_radar(&mut self, ui: &egui::Ui, rect: Rect, transform: &map::MapTransform) -> bool {
        if !self.map_radar.is_shown() {
            return false;
        }
        let span_km = self.map_view.span_km();
        let mut drew = false;
        for layer in self.view_radars.iter().rev() {
            let matches_product = layer
                .display
                .frame
                .as_ref()
                .is_some_and(|frame| frame.geometry.product == self.product);
            if matches_product {
                match draw_map_display(&layer.display, ui.painter(), rect, transform, span_km) {
                    Ok(true) => drew = true,
                    Ok(false) => {}
                    Err(error) => {
                        self.notice = Some(format!(
                            "Viewport radar {} could not be placed: {error}",
                            layer.site
                        ));
                    }
                }
            }
        }
        match draw_map_display(&self.display, ui.painter(), rect, transform, span_km) {
            Ok(true) => drew = true,
            Ok(false) => {}
            Err(error) => {
                self.notice = Some(format!("Selected radar could not be placed: {error}"))
            }
        }
        if drew {
            overlay::draw_legend(ui.painter(), rect, self.product);
        }
        drew
    }

    fn map_basemap(&self, ui: &egui::Ui, rect: Rect) {
        match &self.map {
            Ok(world) => {
                if let Err(error) = world.draw(ui.painter(), &self.map_view, rect) {
                    ui.painter().text(
                        rect.left_top() + Vec2::new(18.0, 52.0),
                        Align2::LEFT_TOP,
                        format!("The basemap could not be drawn: {error}"),
                        FontId::proportional(13.0),
                        ERROR,
                    );
                }
            }
            Err(error) => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("The basemap could not be loaded: {error}"),
                    FontId::proportional(14.0),
                    ERROR,
                );
            }
        }
    }

    fn map_stations(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        interaction: &egui::Response,
    ) -> Option<SiteId> {
        let pointer = ui.input(|input| input.pointer.hover_pos());
        let mut nearest: Option<(f32, SiteId)> = None;
        if let Ok(stations) = &self.stations {
            for station in stations {
                let Some(position) = transform.screen(station.lat, station.lon) else {
                    continue;
                };
                if !rect.shrink(2.0).contains(position) {
                    continue;
                }
                let selected = self.site.as_ref() == Some(&station.id);
                if selected {
                    ui.painter()
                        .circle_stroke(position, 8.0, Stroke::new(2.0_f32, STALE));
                }
                ui.painter().circle_filled(
                    position,
                    if selected { 4.0 } else { 2.4 },
                    if selected {
                        STALE
                    } else {
                        Color32::from_rgb(105, 202, 214)
                    },
                );
                if selected || self.map_view.span_km() < 1_800.0 {
                    ui.painter().text(
                        position + Vec2::new(7.0, -7.0),
                        Align2::LEFT_BOTTOM,
                        station.id.as_str(),
                        FontId::monospace(10.0),
                        if selected { STALE } else { MUTED },
                    );
                }
                if let Some(pointer) = pointer {
                    let distance = position.distance(pointer);
                    if distance <= 12.0 && nearest.as_ref().is_none_or(|(best, _)| distance < *best)
                    {
                        nearest = Some((distance, station.id.clone()));
                    }
                }
            }
        }
        if interaction.clicked() {
            nearest.map(|(_, id)| id)
        } else {
            None
        }
    }

    fn map_place(&self, ui: &egui::Ui, transform: &map::MapTransform) {
        let Some(place) = &self.place else {
            return;
        };
        let Some(position) = transform.screen(place.lat, place.lon) else {
            return;
        };
        ui.painter()
            .circle_filled(position, 5.0, Color32::from_rgb(245, 245, 240));
        ui.painter().text(
            position + Vec2::new(8.0, 8.0),
            Align2::LEFT_TOP,
            place.label(),
            FontId::proportional(11.0),
            TEXT,
        );
    }

    fn map_controls(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let layer_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 16.0, rect.top() + 70.0),
            Vec2::new(116.0, 28.0),
        );
        let reload_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 16.0, rect.top() + 142.0),
            Vec2::new(116.0, 28.0),
        );
        if ui
            .put(reload_rect, egui::Button::new("Reload nearby"))
            .on_hover_text("Find up to three stations around the current map centre.")
            .clicked()
        {
            self.reload_viewport_radars();
        }
        if ui
            .put(
                layer_rect,
                egui::Button::new(if self.map_radar.is_shown() {
                    "Hide radar layer"
                } else {
                    "Show radar layer"
                }),
            )
            .clicked()
        {
            self.map_radar = self.map_radar.toggled();
        }
        let has_place = self.place.is_some();
        let zoom_rect = Rect::from_min_size(
            Pos2::new(rect.left() + 16.0, rect.top() + 104.0),
            Vec2::new(32.0, 30.0),
        );
        if ui.put(zoom_rect, egui::Button::new("+")).clicked() {
            match self.map_view.zoom_at(rect, rect.center(), 1.6) {
                Ok(()) => self.schedule_viewport_reload(),
                Err(error) => self.notice = Some(format!("Map zoom failed: {error}")),
            }
        }
        if ui
            .put(
                zoom_rect.translate(Vec2::new(36.0, 0.0)),
                egui::Button::new("−"),
            )
            .clicked()
        {
            match self.map_view.zoom_at(rect, rect.center(), 1.0 / 1.6) {
                Ok(()) => self.schedule_viewport_reload(),
                Err(error) => self.notice = Some(format!("Map zoom failed: {error}")),
            }
        }
        if has_place
            && ui
                .put(
                    Rect::from_min_size(
                        zoom_rect.min + Vec2::new(72.0, 0.0),
                        Vec2::new(58.0, 30.0),
                    ),
                    egui::Button::new("Focus"),
                )
                .clicked()
        {
            self.focus_map_context();
            self.schedule_viewport_reload();
        }
        self.map_status(ui, rect);
    }

    fn map_status(&self, ui: &egui::Ui, rect: Rect) {
        let countries = self.map.as_ref().map_or(0, WorldMap::country_count);
        let contributing = self
            .view_radars
            .iter()
            .map(|layer| layer.site.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !contributing.is_empty() {
            ui.painter().text(
                Pos2::new(rect.left() + 16.0, rect.top() + 178.0),
                Align2::LEFT_TOP,
                format!("Layers: {contributing}"),
                FontId::monospace(10.0),
                MUTED,
            );
        }
        let centre = self.map_view.center_latlon().map_or_else(
            |_| String::from("unavailable"),
            |(lat, lon)| format!("{lat:.2}, {lon:.2}"),
        );
        ui.painter().text(
            rect.left_top() + Vec2::new(16.0, 16.0),
            Align2::LEFT_TOP,
            "Geographic map",
            FontId::proportional(15.0),
            TEXT,
        );
        ui.painter().text(
            rect.left_top() + Vec2::new(16.0, 36.0),
            Align2::LEFT_TOP,
            format!(
                "Centre {centre} · {countries} countries · {:.0} km wide · {} viewport radars",
                self.map_view.span_km(),
                self.view_radars.len()
            ),
            FontId::proportional(10.0),
            MUTED,
        );
        ui.painter().text(
            Pos2::new(rect.right() - 12.0, rect.bottom() - 12.0),
            Align2::RIGHT_BOTTOM,
            "Natural Earth 1:110m · public domain",
            FontId::proportional(10.0),
            MUTED,
        );
        ui.painter().text(
            rect.left_top() + Vec2::new(16.0, 52.0),
            Align2::LEFT_TOP,
            "Drag to pan · scroll to zoom · click a radar site",
            FontId::proportional(10.0),
            MUTED,
        );
    }

    fn map_readout(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        radar_visible: bool,
    ) {
        let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        if !rect.contains(pointer) {
            return;
        }
        let (lat, lon) = transform.latlon_at(pointer);
        let mut label = format!("{lat:.4}, {lon:.4}");
        if radar_visible && let Some(frame) = &self.display.frame {
            match crate::projection::inverse(
                frame.geometry.site_lat,
                frame.geometry.site_lon,
                lat,
                lon,
            ) {
                Ok((range_km, azimuth_deg)) => {
                    match frame
                        .geometry
                        .lookup
                        .gate(&frame.volume, range_km, azimuth_deg)
                    {
                        Ok(Some(hit)) => {
                            if write!(
                                label,
                                "\n{} · {:.2}° beam",
                                gate_label(Some(hit.value), frame.geometry.product),
                                hit.elevation_deg
                            )
                            .is_err()
                            {
                                label.push_str("\nradar readout unavailable");
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            if write!(label, "\nreadout unavailable: {error}").is_err() {
                                label.push_str("\nreadout unavailable");
                            }
                        }
                    }
                }
                Err(error) => {
                    if write!(label, "\nreadout unavailable: {error}").is_err() {
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

    fn radar_scene(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, BACKGROUND);
        if self.display.frame.is_none() {
            self.empty_state(ui, rect);
            return;
        }
        let Some(frame) = self.display.frame.as_ref() else {
            return;
        };
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let viewport = render::radar_viewport(rect);
        let transform = match ViewTransform::new(viewport, &frame.geometry) {
            Ok(transform) => transform,
            Err(error) => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("Radar view unavailable: {error}"),
                    FontId::proportional(15.0),
                    ERROR,
                );
                return;
            }
        };
        if let Some(shape) = frame.shape(viewport) {
            ui.painter().add(shape);
        }
        if frame.mesh.is_none() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "No measured echo in this sweep. This does not establish clear weather.",
                FontId::proportional(15.0),
                TEXT,
            );
        }
        overlay::draw_grid(ui.painter(), &frame.geometry, &transform);
        overlay::draw_rings(ui.painter(), &frame.geometry, &transform);
        overlay::draw_legend(ui.painter(), rect, frame.geometry.product);
        if let Some(place) = &self.place {
            draw_place_marker(ui.painter(), frame, &transform, viewport, rect, place);
        }
        north_marker(ui.painter(), &viewport);
        map_caption(ui.painter(), rect, frame);
        readout(ui, frame, &transform, viewport, rect);
    }

    fn empty_state(&self, ui: &egui::Ui, rect: Rect) {
        let painter = ui.painter();
        let mut y = rect.center().y - 60.0;
        let (title, body) = self.empty_text();
        painter.text(
            Pos2::new(rect.center().x, y),
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(21.0),
            TEXT,
        );
        y += 30.0;
        for line in body {
            painter.text(
                Pos2::new(rect.center().x, y),
                Align2::CENTER_CENTER,
                line,
                FontId::proportional(13.0),
                MUTED,
            );
            y += 21.0;
        }
    }

    fn empty_text(&self) -> (&'static str, Vec<String>) {
        match (&self.place, &self.site, &self.pipeline) {
            (None, None, _) => (
                "Where do you want weather for?",
                vec![
                    String::from("Choose a radar station in the left panel, or search for a town."),
                    String::from(
                        "The forecast follows your town; radar comes from the nearest station.",
                    ),
                ],
            ),
            (Some(place), None, _) if !place.in_united_states() => (
                "No supported radar source covers this place",
                vec![
                    format!("The forecast on the right is for {}.", place.label()),
                    String::from("Radar coverage is limited to the sources currently configured."),
                ],
            ),
            (Some(place), None, _) => (
                "No radar station selected",
                vec![format!("Choose a radar station for {}.", place.label())],
            ),
            (Some(_), Some(site), Some(pipeline)) => match &pipeline.state {
                PipelineState::Failed(failure) => (
                    "Radar unavailable",
                    vec![
                        format!("{} failed for {site}.", failure.operation.description()),
                        failure.error.to_string(),
                    ],
                ),
                PipelineState::Working { operation, .. } => (
                    "Loading radar",
                    vec![format!(
                        "{} the latest volume for {site}…",
                        operation.description()
                    )],
                ),
                PipelineState::Ready => match &self.display.state {
                    DisplayState::Unavailable(_) | DisplayState::Failed { .. } => (
                        "The radar image could not be prepared",
                        vec![String::from("The status bar has the reason and a retry.")],
                    ),
                    DisplayState::Waiting | DisplayState::Building | DisplayState::Ready => (
                        "Preparing the radar image…",
                        vec![String::from(
                            "The first frame appears as soon as it is built.",
                        )],
                    ),
                },
            },
            (None, Some(site), _) => (
                "Loading radar",
                vec![format!("Starting the radar pipeline for {site}…")],
            ),
            (Some(_), Some(_), None) => (
                "Loading radar",
                vec![String::from("The pipeline is starting.")],
            ),
        }
    }
}

fn draw_map_display(
    display: &Display,
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
    if !(span_km.is_finite() && span_km > 0.0 && rect.width() > 0.0 && rect.height() > 0.0) {
        return Ok(false);
    }
    let kilometers_per_pixel = transform.kilometers_per_pixel();
    let origin = [
        ((site.x - rect.center().x) as f64 * kilometers_per_pixel) as f32,
        (-(site.y - rect.center().y) as f64 * kilometers_per_pixel) as f32,
    ];
    let scale_x = (2.0 / span_km) as f32;
    let scale_y = (2.0 * f64::from(rect.width()) / (span_km * f64::from(rect.height()))) as f32;
    display.set_map_view(scale_x, scale_y, origin)?;
    if let Some(shape) = frame.shape(rect) {
        painter.add(shape);
    }
    Ok(true)
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
        .size(10.0)
        .color(MUTED),
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
                .size(10.0)
                .color(MUTED),
        );
    }
    if let Some(status) = &station.status {
        ui.label(
            RichText::new(format!("RDA status: {status}"))
                .size(10.0)
                .color(MUTED),
        );
    }
    if let Some(last_data) = &station.last_data {
        ui.label(
            RichText::new(format!("Level II last received: {last_data}"))
                .size(10.0)
                .color(MUTED),
        );
    }
    ui.label(
        RichText::new(format!(
            "Source {} via {}. Radar observations come from this station, not from your town.",
            station.source.id(),
            station.source.provider()
        ))
        .size(10.0)
        .color(MUTED),
    );
}

fn search_results(
    ui: &mut egui::Ui,
    places: &[Place],
    stations: &Result<Vec<Station>, StationError>,
) -> Option<Place> {
    if places.is_empty() {
        ui.colored_label(STALE, "No place matches that search. Check the spelling.");
        return None;
    }
    let mut chosen: Option<Place> = None;
    for place in places {
        let detail = if !place.in_united_states() {
            format!("{} · no radar source", place.country)
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
                egui::Button::new(RichText::new(label).size(12.0))
                    .fill(CARD)
                    .corner_radius(CornerRadius::same(4)),
            )
            .clicked()
        {
            chosen = Some(place.clone());
        }
    }
    chosen
}

fn forecast_body(ui: &mut egui::Ui, forecast: &Forecast, stale: bool) {
    ui.label(
        RichText::new(forecast.place.label())
            .size(15.0)
            .strong()
            .color(TEXT),
    );
    ui.label(
        RichText::new(format!("Forecast for {}", forecast.place.coordinates()))
            .size(10.0)
            .color(MUTED),
    );
    if stale {
        ui.colored_label(STALE, "Stale forecast");
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "{} · {} ({}) · {}",
            forecast.provider,
            forecast.timezone,
            forecast.timezone_abbreviation,
            timestamp_label(&forecast.current.observed)
        ))
        .size(10.0)
        .color(MUTED),
    );
    ui.separator();
    ui.label(RichText::new("Now").size(12.0).strong().color(TEXT));
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!(
                "{:.0} {}",
                forecast.current.temperature, forecast.current.unit
            ))
            .size(20.0)
            .strong(),
        );
        ui.label(
            RichText::new(&forecast.current.short)
                .size(12.0)
                .color(TEXT),
        );
    });
    ui.label(
        RichText::new(format!(
            "Feels {:.0} {} · humidity {:.0}% · {} · precipitation {:.1} mm",
            forecast.current.apparent,
            forecast.current.unit,
            forecast.current.humidity,
            forecast.current.wind,
            forecast.current.precipitation
        ))
        .size(10.0)
        .color(MUTED),
    );
    ui.add_space(6.0);
    ui.separator();
    ui.label(RichText::new("Hourly").size(12.0).strong().color(TEXT));
    let list_height = (ui.available_height() - 8.0).max(80.0);
    egui::ScrollArea::vertical()
        .max_height(list_height)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            for period in &forecast.hourly {
                forecast_period(ui, period);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Daily").size(12.0).strong().color(TEXT));
            for period in &forecast.daily {
                forecast_period(ui, period);
            }
        });
}

fn forecast_period(ui: &mut egui::Ui, period: &ForecastPeriod) {
    ui.label(RichText::new(&period.name).size(12.0).strong().color(TEXT));
    ui.horizontal_wrapped(|ui| {
        let temperature = period.minimum.map_or_else(
            || format!("{:.0} {}", period.temperature, period.unit),
            |minimum| format!("{:.0}/{:.0} {}", period.temperature, minimum, period.unit),
        );
        ui.label(RichText::new(temperature).size(17.0).strong());
        ui.label(RichText::new(&period.short).size(12.0).color(TEXT));
    });
    let precipitation = period
        .precipitation
        .map_or_else(String::new, |chance| format!(" · {chance}% precipitation"));
    let wind = if period.wind.is_empty() {
        String::new()
    } else {
        format!(" · wind {}", period.wind)
    };
    ui.label(
        RichText::new(format!(
            "{}{wind}{precipitation}",
            timestamp_label(&period.start)
        ))
        .size(10.0)
        .color(MUTED),
    );
    ui.add_space(6.0);
    ui.separator();
}

fn draw_place_marker(
    painter: &egui::Painter,
    frame: &Frame,
    transform: &ViewTransform,
    viewport: Rect,
    bounds: Rect,
    place: &Place,
) {
    let offset = match render::place_offset(
        place.lat,
        place.lon,
        frame.geometry.site_x,
        frame.geometry.site_y,
    ) {
        Ok(offset) => offset,
        Err(error) => {
            painter.text(
                Pos2::new(bounds.left() + 16.0, bounds.top() + 34.0),
                Align2::LEFT_TOP,
                format!("Town marker unavailable: {error}"),
                FontId::proportional(12.0),
                ERROR,
            );
            return;
        }
    };
    let (distance_km, azimuth_deg) = match crate::projection::inverse(
        frame.volume.site_lat,
        frame.volume.site_lon,
        place.lat,
        place.lon,
    ) {
        Ok(value) => value,
        Err(error) => {
            painter.text(
                Pos2::new(bounds.left() + 16.0, bounds.top() + 34.0),
                Align2::LEFT_TOP,
                format!("Town distance unavailable: {error}"),
                FontId::proportional(12.0),
                ERROR,
            );
            return;
        }
    };
    let position = transform.screen(offset);
    let detail = format!(
        "{} · {:.0} km {}",
        place.name,
        distance_km,
        place::compass(azimuth_deg)
    );
    if viewport.contains(position) {
        painter.circle_filled(position, 4.0, STALE);
        painter.circle_stroke(position, 7.0, Stroke::new(1.0_f32, TEXT));
        let galley = painter.layout_no_wrap(detail, FontId::proportional(12.0), TEXT);
        let size = galley.size();
        let gap = 10.0;
        let origin = if position.x < viewport.center().x {
            Pos2::new(position.x - size.x - gap, position.y - size.y - gap)
        } else {
            Pos2::new(position.x + gap, position.y - size.y - gap)
        };
        let origin = Pos2::new(
            origin.x.clamp(
                bounds.left() + 4.0,
                (bounds.right() - size.x - 10.0).max(bounds.left() + 4.0),
            ),
            origin.y.clamp(
                bounds.top() + 4.0,
                (bounds.bottom() - size.y - 10.0).max(bounds.top() + 4.0),
            ),
        );
        painter.rect_filled(
            Rect::from_min_size(origin - Vec2::splat(3.0), size + Vec2::splat(6.0)),
            CornerRadius::same(3),
            Color32::from_black_alpha(210),
        );
        painter.galley(origin, galley, TEXT);
    } else {
        painter.text(
            Pos2::new(bounds.left() + 16.0, bounds.top() + 34.0),
            Align2::LEFT_TOP,
            format!("{detail} · outside the radar view"),
            FontId::proportional(12.0),
            STALE,
        );
    }
}

fn map_caption(painter: &egui::Painter, rect: Rect, frame: &Frame) {
    let product = frame.geometry.product;
    let label = format!(
        "{} · {} ({}) · tilt {} at {:.2}°",
        frame.volume.site,
        product.label(),
        product.id(),
        frame.volume.sweep.elevation_number,
        frame.volume.sweep.elevation_deg
    );
    painter.text(
        Pos2::new(rect.left() + 16.0, rect.top() + 14.0),
        Align2::LEFT_TOP,
        label,
        FontId::proportional(12.0),
        TEXT,
    );
}

fn north_marker(painter: &egui::Painter, viewport: &Rect) {
    let x = viewport.center().x;
    let top = viewport.top() + 12.0;
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

fn scan_status(ui: &mut egui::Ui, frame: &Frame, failed: bool) {
    let scan = frame.volume.scan_unix_ms;
    match time::format_utc(scan) {
        Ok(text) => {
            ui.label(format!("Scan {text}"));
        }
        Err(error) => {
            ui.colored_label(ERROR, format!("Scan time unavailable: {error}"));
        }
    }
    match time::now_unix_ms() {
        Ok(now) if now < scan => {
            ui.colored_label(ERROR, time::age_label(now, scan));
        }
        Ok(now) if failed || now - scan > STALE_AFTER_MS => {
            ui.colored_label(
                STALE,
                format!("Stale radar: {}", time::age_label(now, scan)),
            );
        }
        Ok(now) => {
            ui.label(time::age_label(now, scan));
        }
        Err(error) => {
            ui.colored_label(ERROR, format!("Scan age unavailable: {error}"));
        }
    }
}

fn diagnostics(ui: &mut egui::Ui, frame: &Frame, catalog: Option<&Station>) {
    let volume = &frame.volume;
    let product = frame.geometry.product;
    let (gates, below, folded) = volume
        .sweep
        .radials
        .iter()
        .filter_map(|radial| radial.moment(product))
        .fold((0_usize, 0_usize, 0_usize), |counts, moment| {
            (
                counts.0.saturating_add(moment.values.len()),
                counts.1.saturating_add(moment.below_threshold),
                counts.2.saturating_add(moment.range_folded),
            )
        });
    let radials = volume.sweep.radials.len();
    let total_gates = volume.sweep.gate_count();
    let max_range = volume.sweep.max_ground_range_km;
    let skipped = volume.skipped_radials;
    let non_data = volume.skipped_non_data;
    let id = product.id();
    let text = format!(
        "{radials} radials · {total_gates} gates total · {gates} {id} gates · {below} below threshold · {folded} range folded · {max_range:.0} km · Skipped radials: {skipped} · Non-data messages: {non_data}"
    );
    let catalog_note =
        catalog.map_or_else(String::new, |station| {
            match crate::projection::inverse(
                volume.site_lat,
                volume.site_lon,
                station.lat,
                station.lon,
            ) {
                Ok((distance_km, _)) => format!(" · Catalog vs header: {distance_km:.1} km"),
                Err(error) => format!(" · Catalog/header distance unavailable: {error}"),
            }
        });
    let text = format!("{text}{catalog_note}");
    if volume.skipped_radials > 0 {
        ui.colored_label(STALE, text);
    } else {
        ui.label(RichText::new(text).size(10.0).color(MUTED));
    }
    if let Some((product, error)) = &volume.moment_error {
        ui.colored_label(STALE, format!("Rejected {} radial: {error}", product.id()));
    }
}

fn gate_label(value: Option<GateValue>, product: RadarProduct) -> String {
    match value {
        Some(GateValue::Measured(value)) => format!("{value:.2} {}", product.unit()),
        Some(GateValue::BelowThreshold) => String::from("below threshold"),
        Some(GateValue::RangeFolded) => String::from("range folded"),
        None => String::from("no radar data"),
    }
}

fn readout(ui: &egui::Ui, frame: &Frame, transform: &ViewTransform, viewport: Rect, bounds: Rect) {
    let Some(position) = ui.input(|input| input.pointer.hover_pos()) else {
        return;
    };
    if !bounds.contains(position) {
        return;
    }
    let painter = ui.painter();
    let info = match transform.pointer_info(position) {
        Ok(info) => info,
        Err(error) => {
            painter.text(
                position,
                Align2::LEFT_TOP,
                format!("Pointer unavailable: {error}"),
                FontId::monospace(12.0),
                ERROR,
            );
            return;
        }
    };
    let (value, site) = if viewport.contains(position) {
        let product = frame.geometry.product;
        let value = match frame
            .geometry
            .lookup
            .gate(&frame.volume, info.range_km, info.azimuth_deg)
        {
            Ok(Some(hit)) => format!(
                "{} · {:.2}° beam",
                gate_label(Some(hit.value), product),
                hit.elevation_deg
            ),
            Ok(None) => gate_label(None, product),
            Err(error) => format!("readout unavailable: {error}"),
        };
        (value, format!("{}: ", frame.volume.site))
    } else {
        (String::from("no radar data"), String::new())
    };
    let label = format!(
        "{:.4}, {:.4}\n{:.1} km  {:.0}°\n{site}{value}",
        info.lat, info.lon, info.range_km, info.azimuth_deg
    );
    let stroke = Stroke::new(1.0_f32, TEXT);
    painter.line_segment(
        [
            position - Vec2::new(6.0, 0.0),
            position + Vec2::new(6.0, 0.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            position - Vec2::new(0.0, 6.0),
            position + Vec2::new(0.0, 6.0),
        ],
        stroke,
    );
    let galley = painter.layout_no_wrap(label, FontId::monospace(12.0), TEXT);
    let padding = Vec2::splat(6.0);
    let size = galley.size() + 2.0 * padding;
    let x = (position.x + 12.0).min((bounds.right() - size.x).max(bounds.left()));
    let y = if position.y >= viewport.center().y {
        (position.y - size.y - 12.0).max(bounds.top())
    } else {
        (position.y + 12.0).min((bounds.bottom() - size.y).max(bounds.top()))
    };
    let origin = Pos2::new(x, y);
    painter.rect_filled(
        Rect::from_min_size(origin, size),
        CornerRadius::same(4),
        Color32::from_black_alpha(220),
    );
    painter.galley(origin + padding, galley, TEXT);
}

impl eframe::App for OxradApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_workers();
        self.header(ui);
        self.status(ui);
        self.rail(ui);
        self.forecast_panel(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BACKGROUND))
            .show_inside(ui, |ui| self.scene(ui));
        let interval = if self.busy() {
            BUSY_REPAINT_MS
        } else {
            IDLE_REPAINT_MS
        };
        ui.ctx()
            .request_repaint_after(Duration::from_millis(interval));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    fn throttled(retry_after: Option<&str>) -> ForecastError {
        ForecastError::Provider {
            source: JsonError::Status {
                url: String::from("https://api.weather.gov/points/example"),
                status: 503,
                retry_after: retry_after.map(String::from),
            },
        }
    }

    #[test]
    fn forecast_retry_honors_retry_after_and_grows_exponentially() {
        assert_eq!(
            forecast_retry_delay(&throttled(Some("17")), 0),
            Duration::from_secs(17)
        );
        let first = forecast_retry_delay(&throttled(None), 0);
        let second = forecast_retry_delay(&throttled(None), 1);
        assert_eq!(first, Duration::from_secs(2));
        assert!(second > first, "{second:?} must exceed {first:?}");
        assert!(second <= Duration::from_secs(5));
    }
    fn test_station(id: &str, lat: f64, lon: f64) -> Result<Station, TestError> {
        Ok(Station {
            id: SiteId::parse(id)?,
            name: String::from(id),
            lat,
            lon,
            elevation_m: None,
            kind: crate::stations::StationKind::Wsr88d,
            source: crate::stations::RadarSource::Nexrad,
            status: None,
            last_data: None,
        })
    }

    #[test]
    fn viewport_station_selection_sorts_excludes_and_limits() -> Result<(), TestError> {
        let stations = vec![
            test_station("KAAA", 37.05, -88.05)?,
            test_station("KAAB", 38.0, -88.05)?,
            test_station("KAAC", 39.0, -88.05)?,
            test_station("KAAD", 40.0, -88.05)?,
        ];
        let excluded = SiteId::parse("KAAA")?;
        let selected = nearby_station_ids(&stations, 37.05, -88.05, 400.0, Some(&excluded), 2)?;
        assert_eq!(
            selected.iter().map(SiteId::as_str).collect::<Vec<_>>(),
            vec!["KAAB", "KAAC"]
        );
        Ok(())
    }
}
