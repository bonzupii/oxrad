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
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Shape,
    Stroke, Vec2,
};

use crate::basemap::{BasemapError, BasemapLayers};
use crate::colormap::ColorTable;
use crate::config::{self, Config};
use crate::display::{Display, DisplayState, Frame};
use crate::forecast::{Forecast, ForecastPeriod, ForecastState, ForecastView};
use crate::global_radar::{
    self, DecodedTile, GlobalEvent, GlobalRadarError, GlobalRadarFeed, TileBounds, TileImage,
};
use crate::gpu::RadarGpu;
use crate::history::{self, HistoryError, HistoryEvent, HistoryFeed};
use crate::map::{self, MapError, MapView, WorldMap};
use crate::overlay;
use crate::pipeline::{Pipeline, PipelineState};
use crate::place::{self, Place, SearchInteraction, SearchState};
use crate::product::RadarProduct;
use crate::provider::timestamp_label;
use crate::radar::{ArchiveOrigin, GateValue, RetrievedVolume};
use crate::radar_controls::{self, Selection};
use crate::render::{self, ViewTransform};
use crate::stations::{self, SiteId, Station, StationError, StationFeed, StationOutcome};
use crate::theme::{self, Palette, Theme};
use crate::time;
use crate::warnings::{Severity, Warning, WarningError, WarningEvent, WarningFeed};

const TEXT: Color32 = Color32::from_rgb(214, 221, 232);
const MUTED: Color32 = Color32::from_rgb(156, 168, 184);
const ERROR: Color32 = Color32::from_rgb(255, 122, 122);
const STALE: Color32 = Color32::from_rgb(255, 196, 64);
const STALE_AFTER_MS: u64 = 15 * 60 * 1000;
const BUSY_REPAINT_MS: u64 = 100;
const IDLE_REPAINT_MS: u64 = 1000;
const RAIL_WIDTH: f32 = 320.0;
const FORECAST_WIDTH: f32 = 340.0;
const SEARCH_ROW_HEIGHT: f32 = 32.0;
const STATION_LIST_HEIGHT: f32 = 220.0;
const VIEW_RADAR_LIMIT: usize = 3;
const VIEW_RADAR_TIME_WINDOW_MS: u64 = 6 * 60 * 1000;
const STATION_REFRESH_SECS: u64 = 6 * 60 * 60;
const STATION_RETRY_SECS: u64 = 15 * 60;
const GLOBAL_REFRESH_SECS: u64 = 5 * 60;
const GLOBAL_RETRY_SECS: u64 = 60;
const GLOBAL_PLAYBACK_MS: u64 = 650;
const HISTORY_FRAME_MS: u64 = 900;
const WARNING_REFRESH_SECS: u64 = 5 * 60;
const WARNING_RETRY_SECS: u64 = 2 * 60;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalPlayback {
    Stopped,
    Playing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeasureMode {
    Off,
    On,
}

impl MeasureMode {
    const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }

    const fn toggled(self) -> Self {
        match self {
            Self::Off => Self::On,
            Self::On => Self::Off,
        }
    }
}

enum GlobalState {
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WarningState {
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WarningScope {
    All,
    Viewport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryState {
    Idle,
    LoadingKeys,
    Ready,
    LoadingVolume,
    Reviewing,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryStart {
    Oldest,
    Current,
}

struct HistoryView {
    feed: HistoryFeed,
    keys: Vec<String>,
    key: Option<String>,
    state: HistoryState,
    error: Option<HistoryError>,
    playback: GlobalPlayback,
    next_frame: Option<Instant>,
    scrub_index: Option<usize>,
}

impl HistoryView {
    fn new() -> Self {
        Self {
            feed: HistoryFeed::new(),
            keys: Vec::new(),
            key: None,
            state: HistoryState::Idle,
            error: None,
            playback: GlobalPlayback::Stopped,
            next_frame: None,
            scrub_index: None,
        }
    }

    fn clear(&mut self) {
        self.feed.cancel();
        self.keys.clear();
        self.key = None;
        self.state = HistoryState::Idle;
        self.error = None;
        self.playback = GlobalPlayback::Stopped;
        self.next_frame = None;
        self.scrub_index = None;
    }
}

struct GlobalTileTexture {
    bounds: TileBounds,
    texture: egui::TextureHandle,
}

struct GlobalFrameTexture {
    time_unix_ms: u64,
    tiles: Vec<GlobalTileTexture>,
}

fn due_after(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}

fn load_basemap(notice: Option<String>) -> (Result<BasemapLayers, BasemapError>, Option<String>) {
    let layers = BasemapLayers::load();
    let notice = match &layers {
        Ok(_) => notice,
        Err(error) => Some(notice.map_or_else(
            || format!("Supplemental map layers unavailable: {error}"),
            |existing| format!("{existing} Supplemental map layers unavailable: {error}"),
        )),
    };
    (layers, notice)
}

const fn scans_align(reference_ms: u64, scan_ms: u64) -> bool {
    reference_ms.abs_diff(scan_ms) <= VIEW_RADAR_TIME_WINDOW_MS
}

const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Extreme => 4,
        Severity::Severe => 3,
        Severity::Moderate => 2,
        Severity::Minor => 1,
        Severity::Unknown => 0,
    }
}

const fn warning_colour(severity: Severity) -> Color32 {
    match severity {
        Severity::Extreme => Color32::from_rgb(255, 58, 58),
        Severity::Severe => Color32::from_rgb(255, 138, 32),
        Severity::Moderate => Color32::from_rgb(255, 214, 64),
        Severity::Minor => Color32::from_rgb(100, 190, 255),
        Severity::Unknown => Color32::from_rgb(190, 198, 210),
    }
}

fn toggle_button(palette: &Palette, label: &str, selected: bool) -> egui::Button<'static> {
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

fn color_table_selector(ui: &mut egui::Ui, selected: ColorTable) -> ColorTable {
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

fn action_button(palette: &Palette, label: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label)
            .size(theme::TYPE_BODY)
            .color(palette.text),
    )
    .fill(palette.surface)
    .stroke(Stroke::new(1.0_f32, palette.border))
}

fn map_status_area(rect: Rect) -> Option<Rect> {
    let width = (rect.width() - 110.0).clamp(0.0, 520.0);
    let height = (rect.height() - 400.0).clamp(0.0, 130.0);
    (width > 12.0 && height > 12.0).then(|| {
        Rect::from_min_size(
            rect.left_top() + Vec2::new(16.0, 312.0),
            Vec2::new(width, height),
        )
    })
}

fn retain_warning_selection(selected: &mut Option<String>, warnings: &[Warning]) {
    if selected
        .as_ref()
        .is_some_and(|id| !warnings.iter().any(|warning| &warning.id == id))
    {
        *selected = None;
    }
}

fn warning_intersects(warning: &Warning, transform: &map::MapTransform, rect: Rect) -> bool {
    warning.polygons.iter().any(|ring| {
        let bounds = ring
            .iter()
            .filter_map(|(lat, lon)| transform.screen(*lat, *lon))
            .fold(None, |bounds: Option<Rect>, point| {
                Some(bounds.map_or_else(
                    || Rect::from_min_max(point, point),
                    |bounds| {
                        Rect::from_min_max(
                            Pos2::new(bounds.min.x.min(point.x), bounds.min.y.min(point.y)),
                            Pos2::new(bounds.max.x.max(point.x), bounds.max.y.max(point.y)),
                        )
                    },
                ))
            });
        bounds.is_some_and(|bounds| bounds.intersects(rect))
    })
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
    applied_theme: Option<Theme>,
    config_writable: bool,
    notice: Option<String>,
    search: SearchInteraction,
    place: Option<Place>,
    site: Option<SiteId>,
    pipeline: Option<Pipeline>,
    display: Display,
    forecast: ForecastView,
    product: RadarProduct,
    station_query: String,
    station_picker_open: bool,
    first_run: bool,
    scene_mode: SceneMode,
    map: Result<WorldMap, MapError>,
    map_view: MapView,
    map_viewport: Option<Rect>,
    basemap_layers: Result<BasemapLayers, BasemapError>,
    map_radar: RadarLayer,
    map_reload_due: Option<Instant>,
    global_feed: GlobalRadarFeed,
    global_generation: u64,
    global_frames: Vec<GlobalFrameTexture>,
    global_index: usize,
    global_layer: RadarLayer,
    global_playback: GlobalPlayback,
    global_next_frame: Option<Instant>,
    global_request_due: Option<Instant>,
    global_refresh_due: Option<Instant>,
    global_state: GlobalState,
    warning_feed: WarningFeed,
    warning_generation: u64,
    warnings: Vec<Warning>,
    warnings_without_geometry: usize,
    warnings_rejected: usize,
    selected_warning: Option<String>,
    measure: MeasureMode,
    measure_start: Option<(f64, f64)>,
    measure_end: Option<(f64, f64)>,
    warning_layer: RadarLayer,
    warning_scope: WarningScope,
    warning_state: WarningState,
    warning_request_due: Option<Instant>,
    warning_refresh_due: Option<Instant>,
    history: HistoryView,
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
        let color_table = config.color_table;
        let stations = match stations::load_cache() {
            Ok(Some(cached)) => Ok(cached),
            Ok(None) => Err(StationError::NoStations),
            Err(error) => Err(error),
        };
        let (basemap_layers, notice) = load_basemap(notice);
        let mut app = Self {
            gpu: gpu.clone(),
            view_radars: Vec::new(),
            stations,
            station_feed: StationFeed::new(),
            station_refresh_due: due_after(Duration::from_secs(STATION_REFRESH_SECS)),
            pending_site: restored_site.clone(),
            config,
            applied_theme: None,
            config_writable,
            notice,
            search: SearchInteraction::new(),
            place: restored_place.clone(),
            site: None,
            pipeline: None,
            display: Display::new(gpu, color_table),
            forecast: ForecastView::new(),
            product: RadarProduct::Reflectivity,
            station_query: String::new(),
            station_picker_open: restored_site.is_none(),
            first_run: true,
            scene_mode: SceneMode::Radar,
            map: WorldMap::load(),
            map_view: MapView::default(),
            map_viewport: None,
            basemap_layers,
            map_radar: RadarLayer::Shown,
            map_reload_due: None,
            global_feed: GlobalRadarFeed::new(),
            global_generation: 0,
            global_frames: Vec::new(),
            global_index: 0,
            global_layer: RadarLayer::Shown,
            global_playback: GlobalPlayback::Stopped,
            global_next_frame: None,
            global_request_due: None,
            global_refresh_due: None,
            global_state: GlobalState::Idle,
            warning_feed: WarningFeed::new(),
            warning_generation: 0,
            warnings: Vec::new(),
            warnings_without_geometry: 0,
            warnings_rejected: 0,
            selected_warning: None,
            measure: MeasureMode::Off,
            measure_start: None,
            measure_end: None,
            warning_layer: RadarLayer::Shown,
            warning_scope: WarningScope::All,
            warning_state: WarningState::Idle,
            warning_request_due: Some(Instant::now()),
            warning_refresh_due: None,
            history: HistoryView::new(),
        };
        if let Some(place) = &restored_place {
            match MapView::focus(place.lat, place.lon, 2_200.0) {
                Ok(view) => app.map_view = view,
                Err(error) => app.notice = Some(format!("Map centre unavailable: {error}")),
            }
            app.forecast.start(place, 0);
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
        self.history.clear();
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
        self.search.clear();
        self.open_site(id, true);
    }

    fn clear_site(&mut self) {
        self.site = None;
        self.pipeline = None;
        self.history.clear();
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

    fn apply_color_table(&mut self) {
        let table = self.config.color_table;
        self.display.set_color_table(table);
        for layer in &mut self.view_radars {
            layer.display.set_color_table(table);
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
                display: Display::new(Some(gpu.clone()), self.config.color_table),
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

    fn request_warnings(&mut self) {
        self.warning_request_due = None;
        let generation = self.warning_generation.wrapping_add(1);
        match self.warning_feed.request(generation) {
            Ok(()) => {
                self.warning_generation = generation;
                self.warning_state = WarningState::Loading;
            }
            Err(error) => self.warning_failure(&error),
        }
    }

    fn warning_failure(&mut self, error: &WarningError) {
        self.warning_state = WarningState::Failed;
        self.warning_refresh_due = Some(Instant::now() + Duration::from_secs(WARNING_RETRY_SECS));
        self.notice = Some(format!("Active warnings are unavailable: {error}"));
    }

    fn poll_warnings(&mut self) {
        loop {
            match self.warning_feed.poll() {
                Ok(Some(event)) => self.apply_warning_event(event),
                Ok(None) => break,
                Err(error) => {
                    self.warning_failure(&error);
                    break;
                }
            }
        }
    }

    fn update_warnings(&mut self) {
        let warning_due = self
            .warning_request_due
            .is_some_and(|due| Instant::now() >= due)
            || self
                .warning_refresh_due
                .is_some_and(|due| Instant::now() >= due);
        if self.scene_mode == SceneMode::Map
            && (matches!(self.warning_state, WarningState::Idle) || warning_due)
        {
            self.request_warnings();
        }
        self.poll_warnings();
    }

    fn apply_warning_event(&mut self, event: WarningEvent) {
        match event {
            WarningEvent::Ready { generation, set } if generation == self.warning_generation => {
                let rejected = set.rejected;
                self.warnings = set.warnings;
                self.warnings_without_geometry = set.without_geometry;
                self.warnings_rejected = rejected.len();
                retain_warning_selection(&mut self.selected_warning, &self.warnings);
                if let Some(message) = rejected.first() {
                    self.notice = Some(format!(
                        "{} active alerts could not be read. First problem: {message}",
                        rejected.len()
                    ));
                }
                self.warning_state = WarningState::Ready;
                self.warning_refresh_due =
                    Some(Instant::now() + Duration::from_secs(WARNING_REFRESH_SECS));
            }
            WarningEvent::Failed { generation, error } if generation == self.warning_generation => {
                self.warning_state = WarningState::Failed;
                self.warning_refresh_due =
                    Some(Instant::now() + Duration::from_secs(WARNING_RETRY_SECS));
                self.notice = Some(format!("Active warnings are unavailable: {error}"));
            }
            WarningEvent::Ready { .. } | WarningEvent::Failed { .. } => {}
        }
    }

    fn choose_place(&mut self, place: &Place) {
        self.search.clear();
        match MapView::focus(place.lat, place.lon, 2_200.0) {
            Ok(view) => self.map_view = view,
            Err(error) => self.notice = Some(format!("Map centre unavailable: {error}")),
        }
        self.place = Some(place.clone());
        self.config.place = Some(place.clone());
        self.forecast.start(place, 0);
        if !place.in_united_states() {
            self.clear_site();
            self.scene_mode = SceneMode::Map;
            self.ensure_viewport_radars();
            self.request_global_radar();
            self.notice = Some(format!(
                "Showing the global RainViewer composite for {}. No Level II site is selected outside the United States.",
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

    fn poll_selected_pipeline(&mut self) {
        let Some(pipeline) = self.pipeline.as_mut() else {
            return;
        };
        let volume = pipeline.poll();
        if let Some(warning) = pipeline.take_cache_warning() {
            self.notice = Some(format!("NEXRAD cache: {warning}"));
        }
        if let Some(volume) = volume {
            self.display.submit(volume, self.product);
        }
    }

    fn update_workers(&mut self, context: &egui::Context) {
        self.poll_selected_pipeline();
        self.display.poll();
        self.poll_history();
        self.advance_history_playback();
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
        self.search.update(Instant::now());
        self.forecast.update(Instant::now());
        self.update_warnings();
        self.poll_global_radar(context);
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
        matches!(self.search.state(), SearchState::Working { .. })
            || matches!(self.forecast.state(), ForecastState::Loading { .. })
            || matches!(self.global_state, GlobalState::Loading)
            || matches!(self.global_playback, GlobalPlayback::Playing)
            || matches!(
                self.history.state,
                HistoryState::LoadingKeys | HistoryState::LoadingVolume
            )
            || self.history.playback == GlobalPlayback::Playing
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
            || match self.history.state {
                HistoryState::LoadingVolume if self.history.playback == GlobalPlayback::Playing => {
                    "History playing"
                }
                HistoryState::LoadingVolume => "History loading",
                HistoryState::Reviewing if self.display.state.error().is_some() => {
                    "History render error"
                }
                HistoryState::Reviewing if self.display.state.is_busy() => "History preparing",
                HistoryState::Reviewing if self.history.playback == GlobalPlayback::Playing => {
                    "History playing"
                }
                HistoryState::Reviewing => "History review",
                HistoryState::Failed => "History error",
                _ if self.site.is_some() => "Idle",
                _ => "Idle · No site selected",
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
        let mut units = self.config.units;
        let mut theme = self.config.theme;
        let mut color_table = self.config.color_table;
        egui::Panel::top("header")
            .exact_size(56.0)
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = self.config.theme.visuals();
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("oxrad").size(theme::TYPE_DISPLAY).strong());
                    ui.label(
                        RichText::new("weather radar")
                            .size(theme::TYPE_LABEL)
                            .weak(),
                    );
                    ui.add_space(theme::SPACE_L);
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
                        self.global_request_due = Some(Instant::now());
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.selectable_value(&mut theme, Theme::Light, Theme::Light.label());
                        ui.selectable_value(&mut theme, Theme::Dark, Theme::Dark.label());
                        ui.separator();
                        color_table = color_table_selector(ui, color_table);
                        ui.separator();
                        ui.selectable_value(
                            &mut units,
                            crate::units::UnitSystem::Imperial,
                            crate::units::UnitSystem::Imperial.label(),
                        );
                        ui.selectable_value(
                            &mut units,
                            crate::units::UnitSystem::Metric,
                            crate::units::UnitSystem::Metric.label(),
                        );
                        ui.separator();
                        if let Some(place) = &self.place {
                            ui.label(
                                RichText::new(format!("Weather for {}", place.label()))
                                    .size(theme::TYPE_BODY),
                            );
                        } else {
                            ui.label(
                                RichText::new("No town selected")
                                    .size(theme::TYPE_BODY)
                                    .weak(),
                            );
                        }
                        ui.separator();
                        match (&self.site, self.site_station()) {
                            (Some(site), Some(station)) => {
                                ui.label(
                                    RichText::new(format!("Radar {site} · {}", station.name))
                                        .size(theme::TYPE_BODY),
                                );
                            }
                            (Some(site), None) => {
                                ui.label(
                                    RichText::new(format!("Radar {site}")).size(theme::TYPE_BODY),
                                );
                            }
                            (None, _) => {
                                ui.label(
                                    RichText::new("No site selected")
                                        .size(theme::TYPE_BODY)
                                        .weak(),
                                );
                            }
                        }
                    });
                });
            });
        if units != self.config.units
            || theme != self.config.theme
            || color_table != self.config.color_table
        {
            let color_changed = color_table != self.config.color_table;
            self.config.units = units;
            self.config.theme = theme;
            self.config.color_table = color_table;
            self.save_config();
            if color_changed {
                self.apply_color_table();
            }
        }
    }

    fn rail(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("rail")
            .default_size(RAIL_WIDTH)
            .min_size(280.0)
            .max_size(440.0)
            .resizable(true)
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = self.config.theme.visuals();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(theme::SPACE_XL);
                        ui.label(
                            RichText::new("Weather for")
                                .size(theme::TYPE_TITLE)
                                .strong(),
                        );
                        self.search_section(ui);
                        if self.place.is_some() {
                            ui.add_space(14.0);
                            self.place_card(ui);
                            ui.add_space(theme::SPACE_L);
                        }
                        ui.separator();
                        ui.add_space(theme::SPACE_M);
                        self.station_section(ui);
                        self.product_section(ui);
                        self.history_section(ui);
                        self.warning_section(ui);
                        if let Some(notice) = &self.notice {
                            ui.add_space(theme::SPACE_M);
                            ui.colored_label(ui.visuals().warn_fg_color, notice);
                        }
                    });
            });
    }

    fn search_section(&mut self, ui: &mut egui::Ui) {
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
            if self.first_run || shortcut {
                response.request_focus();
                self.first_run = false;
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
        if submit {
            if self.search.submit() {
                self.notice = None;
                self.station_picker_open = false;
            } else {
                self.notice = Some(String::from("Type a town or place name first."));
            }
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
                chosen = search_results(ui, places, &self.stations);
            }
        }
        if let Some(place) = chosen {
            self.choose_place(&place);
        }
        ui.add_space(theme::SPACE_XS);
        ui.label(
            RichText::new("Place search © OpenStreetMap contributors, via Photon.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }

    fn place_card(&self, ui: &mut egui::Ui) {
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

    fn product_section(&mut self, ui: &mut egui::Ui) {
        let (Some(station), Some(volume)) = (self.site_station(), self.display.selection_volume())
        else {
            return;
        };
        let selection = radar_controls::show(
            ui,
            station.capabilities(),
            volume,
            self.product,
            !self.display.state.is_busy(),
        );
        let (product, elevation) = match selection {
            Some(Selection::Product(product)) => (product, None),
            Some(Selection::Elevation(elevation)) => (self.product, Some(elevation)),
            None => return,
        };
        if self.display.rebuild(product, elevation) {
            self.product = product;
            self.refresh_view_radars();
        } else {
            self.notice = Some(String::from(
                "That product and elevation could not be prepared from the displayed volume.",
            ));
        }
    }

    fn station_section(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Radar station")
                .size(theme::TYPE_TITLE)
                .strong(),
        );
        ui.add_space(theme::SPACE_XS);
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
        if let Some(place) = &self.place
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
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Radar site catalog unavailable: {error}"),
                );
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
        self.station_query = query;
        chosen
    }

    fn forecast_panel(&mut self, ui: &mut egui::Ui) {
        let mut retry = false;
        egui::Panel::right("forecast")
            .default_size(FORECAST_WIDTH)
            .min_size(300.0)
            .max_size(460.0)
            .resizable(true)
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = self.config.theme.visuals();
                retry = self.forecast_footer(ui);
                ui.add_space(theme::SPACE_M);
                ui.label(RichText::new("Forecast").size(theme::TYPE_TITLE).strong());
                ui.add_space(theme::SPACE_XS);
                match self.forecast.state() {
                    ForecastState::Empty => {
                        ui.label(RichText::new("No town selected").size(14.0).strong());
                        ui.label(
                            RichText::new(
                                "Search for a town on the left to see its forecast here.",
                            )
                            .size(theme::TYPE_SMALL)
                            .weak(),
                        );
                    }
                    ForecastState::Loading { place, .. } => {
                        ui.label(RichText::new(place.label()).size(14.0).strong());
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new("Loading the forecast…")
                                    .size(theme::TYPE_LABEL)
                                    .weak(),
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
                        ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                        if let Some(due) = retry_at {
                            let seconds = due
                                .saturating_duration_since(Instant::now())
                                .as_secs()
                                .saturating_add(1);
                            ui.label(
                                RichText::new(format!("Automatic retry in {seconds} s."))
                                    .size(theme::TYPE_SMALL)
                                    .color(ui.visuals().warn_fg_color),
                            );
                            retry = ui.button("Retry now").clicked();
                        } else {
                            retry = ui.button("Retry forecast").clicked();
                        }
                        if let Some(forecast) = last {
                            ui.add_space(theme::SPACE_S);
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Showing the last forecast received.",
                            );
                            ui.separator();
                            forecast_body(ui, forecast, true, self.config.units);
                        }
                    }
                    ForecastState::Ready { forecast } => {
                        forecast_body(ui, forecast, false, self.config.units);
                    }
                }
                ui.add_space(theme::SPACE_S);
            });
        if retry && let Some(place) = self.place.clone() {
            self.forecast.start(&place, 0);
        }
    }

    fn forecast_footer(&self, ui: &mut egui::Ui) -> bool {
        let mut retry = false;
        if let Some(place) = &self.place {
            egui::Panel::bottom("forecast_footer").show_inside(ui, |ui| {
                ui.separator();
                if matches!(self.forecast.state(), ForecastState::Ready { .. }) {
                    retry = ui.button("Refresh forecast").clicked();
                    ui.add_space(theme::SPACE_XS);
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
                ui.label(RichText::new(note).size(theme::TYPE_SMALL).weak());
                ui.label(
                    RichText::new(
                        "Forecast times are local to the forecast point; radar times are UTC.",
                    )
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
                ui.add_space(theme::SPACE_XS);
            });
        }
        retry
    }

    fn pipeline_cache_status(&self, ui: &mut egui::Ui) {
        if self
            .display
            .frame
            .as_ref()
            .is_some_and(|frame| frame.origin == ArchiveOrigin::Cached)
        {
            ui.label(
                RichText::new("cached volume")
                    .size(theme::TYPE_SMALL)
                    .color(ui.visuals().warn_fg_color),
            );
        }
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        let mut retry_pipeline = false;
        let mut retry_display = false;
        let failed = self.failed();
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            ui.style_mut().visuals = self.config.theme.visuals();
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
                self.pipeline_cache_status(ui);
                if let Some(frame) = &self.display.frame {
                    if let Some(key) = &frame.history_key {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("History {}", history::key_label(key)))
                                .size(theme::TYPE_SMALL)
                                .weak(),
                        );
                    }
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
        let full_rect = ui.max_rect();
        ui.painter().rect_filled(
            full_rect,
            CornerRadius::ZERO,
            self.config.theme.palette().background,
        );
        if self.global_layer.is_shown() && !self.global_frames.is_empty() {
            self.global_history(ui);
        }
        let rect = ui.available_rect_before_wrap();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        self.map_viewport = Some(rect);
        let interaction = ui.interact(
            rect,
            ui.id().with("geographic-map"),
            egui::Sense::click_and_drag(),
        );
        self.map_input(ui, rect, &interaction);
        let now = Instant::now();
        if self.map_reload_due.is_some_and(|due| now >= due) {
            self.map_reload_due = None;
            self.reload_viewport_radars();
        }
        if self.global_layer.is_shown()
            && (self.global_request_due.is_some_and(|due| now >= due)
                || self.global_refresh_due.is_some_and(|due| now >= due)
                || matches!(self.global_state, GlobalState::Idle))
        {
            self.global_refresh_due = None;
            self.request_global_radar();
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
        self.map_supplemental_lines(ui, rect, &transform);
        self.map_global(ui, &transform);
        let radar_visible = self.map_radar(ui, rect, &transform);
        let ring_origin = self
            .site_station()
            .map(|station| (station.lat, station.lon));
        if let Some((lat, lon)) = ring_origin
            && let Err(error) = map::draw_range_rings(
                ui.painter(),
                rect,
                &transform,
                lat,
                lon,
                self.config.units,
                &self.config.theme.palette(),
            )
        {
            self.notice = Some(format!("Map range rings could not be drawn: {error}"));
        }
        self.map_supplemental_roads(ui, rect, &transform);
        self.map_warnings(ui, rect, &transform);
        self.map_supplemental_places(ui, rect, &transform);
        self.map_measure(ui, rect, &transform, &interaction);
        let clicked_site = if self.measure.is_on() {
            None
        } else {
            self.map_stations(ui, rect, &transform, &interaction)
        };
        self.map_place(ui, &transform);
        map::draw_scale(ui.painter(), rect, &transform, &self.config.theme.palette());
        self.map_controls(ui, rect);
        self.map_readout(ui, rect, &transform, radar_visible);
        if let Some(id) = clicked_site {
            self.choose_site(id);
        }
    }

    fn map_supplemental_lines(&self, ui: &egui::Ui, rect: Rect, transform: &map::MapTransform) {
        if let Ok(layers) = &self.basemap_layers {
            layers.draw_lines(
                ui.painter(),
                transform,
                rect,
                self.map_view.span_km(),
                &self.config.theme.palette(),
            );
        }
    }

    fn map_supplemental_roads(&self, ui: &egui::Ui, rect: Rect, transform: &map::MapTransform) {
        if let Ok(layers) = &self.basemap_layers {
            layers.draw_roads(
                ui.painter(),
                transform,
                rect,
                self.map_view.span_km(),
                &self.config.theme.palette(),
            );
        }
    }

    fn map_supplemental_places(&self, ui: &egui::Ui, rect: Rect, transform: &map::MapTransform) {
        if let Ok(layers) = &self.basemap_layers {
            layers.draw_places(
                ui.painter(),
                transform,
                rect,
                self.map_view.span_km(),
                &self.config.theme.palette(),
            );
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
        if interaction.hovered()
            && !ui
                .input(|input| input.pointer.hover_pos())
                .is_some_and(|pointer| {
                    map_status_area(rect).is_some_and(|area| area.contains(pointer))
                })
        {
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
        self.global_request_due = now.checked_add(Duration::from_millis(900));
    }

    fn request_global_radar(&mut self) {
        self.global_request_due = None;
        let (center_lat, center_lon) = match self.map_view.center_latlon() {
            Ok(center) => center,
            Err(error) => {
                self.notice = Some(format!("Global radar view unavailable: {error}"));
                return;
            }
        };
        match self
            .global_feed
            .request(center_lat, center_lon, self.map_view.span_km())
        {
            Ok(generation) => {
                self.global_generation = generation;
                self.global_state = GlobalState::Loading;
            }
            Err(error) => {
                self.notice = Some(format!("Global radar request failed: {error}"));
                self.global_state = GlobalState::Failed;
                self.global_refresh_due = Some(due_after(Duration::from_secs(GLOBAL_RETRY_SECS)));
            }
        }
    }

    fn poll_global_radar(&mut self, context: &egui::Context) {
        while let Some(event) = self.global_feed.poll() {
            self.apply_global_event(context, event);
        }
        self.advance_global_playback();
    }

    fn apply_global_event(&mut self, context: &egui::Context, event: GlobalEvent) {
        match event {
            GlobalEvent::Manifest { generation } => {
                if generation != self.global_generation {
                    return;
                }
                self.global_frames.clear();
                self.global_index = 0;
                self.global_playback = GlobalPlayback::Stopped;
                self.global_next_frame = None;
                self.global_state = GlobalState::Loading;
                self.global_refresh_due = Some(due_after(Duration::from_secs(GLOBAL_REFRESH_SECS)));
            }
            GlobalEvent::Frame {
                generation,
                frame,
                tiles,
            } => {
                if generation != self.global_generation {
                    return;
                }
                self.add_global_frame(context, frame.time_unix_ms, tiles);
            }
            GlobalEvent::Failed { generation, error } => {
                if generation != self.global_generation {
                    return;
                }
                self.notice = Some(format!("Global radar: {error}"));
                self.global_state = GlobalState::Failed;
                self.global_refresh_due = Some(due_after(Duration::from_secs(GLOBAL_RETRY_SECS)));
            }
            GlobalEvent::Notice {
                generation,
                message,
            } => {
                if generation == self.global_generation {
                    self.notice = Some(format!("Global radar cache: {message}"));
                }
            }
        }
    }

    fn add_global_frame(
        &mut self,
        context: &egui::Context,
        time_unix_ms: u64,
        tiles: Vec<TileImage>,
    ) {
        let mut textures = Vec::with_capacity(tiles.len());
        for tile in tiles {
            match global_texture(context, time_unix_ms, tile.bounds, tile.image) {
                Ok(texture) => textures.push(GlobalTileTexture {
                    bounds: tile.bounds,
                    texture,
                }),
                Err(error) => {
                    self.notice = Some(format!("Global radar tile could not be shown: {error}"));
                    self.global_state = GlobalState::Failed;
                    self.global_refresh_due =
                        Some(due_after(Duration::from_secs(GLOBAL_RETRY_SECS)));
                    return;
                }
            }
        }
        let was_live = self.global_index.saturating_add(1) >= self.global_frames.len();
        let position = self
            .global_frames
            .partition_point(|stored| stored.time_unix_ms < time_unix_ms);
        self.global_frames.insert(
            position,
            GlobalFrameTexture {
                time_unix_ms,
                tiles: textures,
            },
        );
        if was_live {
            self.global_index = self.global_frames.len().saturating_sub(1);
        } else if position <= self.global_index {
            self.global_index = self.global_index.saturating_add(1);
        }
        self.global_state = GlobalState::Ready;
    }

    fn advance_global_playback(&mut self) {
        if self.global_playback != GlobalPlayback::Playing || self.global_frames.len() < 2 {
            return;
        }
        let now = Instant::now();
        if self.global_next_frame.is_some_and(|due| now < due) {
            return;
        }
        let count = self.global_frames.len();
        self.global_index = self.global_index.saturating_add(1) % count;
        self.global_next_frame = now.checked_add(Duration::from_millis(GLOBAL_PLAYBACK_MS));
    }

    fn map_global(&self, ui: &egui::Ui, transform: &map::MapTransform) -> bool {
        if !self.global_layer.is_shown() {
            return false;
        }
        let Some(frame) = self.global_frames.get(self.global_index) else {
            return false;
        };
        let mut drew = false;
        for tile in &frame.tiles {
            let (Some(north_west), Some(south_east)) = (
                transform.screen(tile.bounds.north, tile.bounds.west),
                transform.screen(tile.bounds.south, tile.bounds.east),
            ) else {
                continue;
            };
            let image_rect = Rect::from_two_pos(north_west, south_east);
            if image_rect.width() <= 0.0 || image_rect.height() <= 0.0 {
                continue;
            }
            ui.painter().image(
                tile.texture.id(),
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            drew = true;
        }
        drew
    }

    fn global_history(&mut self, ui: &mut egui::Ui) {
        let last = self.global_frames.len().saturating_sub(1);
        let index = self.global_index.min(last);
        let Some(frame) = self.global_frames.get(index) else {
            return;
        };
        let stamp = time::format_utc(frame.time_unix_ms)
            .unwrap_or_else(|_| String::from("time unavailable"));
        let count = self.global_frames.len();
        let mut requested = index;
        let mut playback = self.global_playback;
        let mut scrub = false;
        let palette = self.config.theme.palette();
        egui::Panel::bottom("global_history")
            .exact_size(64.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(palette.raised)
                    .inner_margin(egui::Margin::symmetric(10, 5)),
            )
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = self.config.theme.visuals();
                ui.horizontal(|ui| {
                    let label = if playback == GlobalPlayback::Playing {
                        "Pause"
                    } else {
                        "Play"
                    };
                    if ui.button(label).clicked() {
                        playback = match playback {
                            GlobalPlayback::Stopped => GlobalPlayback::Playing,
                            GlobalPlayback::Playing => GlobalPlayback::Stopped,
                        };
                    }
                    let slider_width = (ui.available_width() - 190.0).max(100.0);
                    if ui
                        .add_sized(
                            [slider_width, 18.0],
                            egui::Slider::new(&mut requested, 0..=last).show_value(false),
                        )
                        .changed()
                    {
                        scrub = true;
                    }
                    ui.label(
                        RichText::new(format!(
                            "{}/{} · {}",
                            index.saturating_add(1),
                            count,
                            global_radar::PALETTE
                        ))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Global composite · {stamp}"))
                            .size(theme::TYPE_SMALL)
                            .monospace(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.hyperlink_to(
                            RichText::new(global_radar::ATTRIBUTION)
                                .size(theme::TYPE_SMALL)
                                .weak(),
                            "https://www.rainviewer.com/",
                        );
                    });
                });
            });
        if scrub {
            playback = GlobalPlayback::Stopped;
            self.global_index = requested;
        }
        if playback != self.global_playback {
            if playback == GlobalPlayback::Playing {
                self.global_next_frame = Some(Instant::now());
            } else {
                self.global_next_frame = None;
            }
        }
        self.global_playback = playback;
    }

    fn displayed_scan_reference(&self) -> Option<u64> {
        self.display
            .frame
            .as_ref()
            .map(|frame| frame.volume.scan_unix_ms)
            .or_else(|| {
                self.view_radars
                    .iter()
                    .filter_map(|layer| layer.display.frame.as_ref())
                    .map(|frame| frame.volume.scan_unix_ms)
                    .max()
            })
    }

    fn map_radar(&mut self, ui: &egui::Ui, rect: Rect, transform: &map::MapTransform) -> bool {
        if !self.map_radar.is_shown() {
            return false;
        }
        let span_km = self.map_view.span_km();
        let reference = self.displayed_scan_reference();
        let mut drew = false;
        for layer in self.view_radars.iter().rev() {
            let Some(frame) = layer.display.frame.as_ref() else {
                continue;
            };
            if frame.geometry.product != self.product {
                continue;
            }
            if !reference.is_none_or(|reference| scans_align(reference, frame.volume.scan_unix_ms))
            {
                continue;
            }
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
        let selected_matches = self
            .display
            .frame
            .as_ref()
            .is_some_and(|frame| frame.geometry.product == self.product);
        if selected_matches {
            match draw_map_display(&self.display, ui.painter(), rect, transform, span_km) {
                Ok(true) => drew = true,
                Ok(false) => {}
                Err(error) => {
                    self.notice = Some(format!("Selected radar could not be placed: {error}"))
                }
            }
        }
        if drew {
            overlay::draw_legend(
                ui.painter(),
                rect,
                self.product,
                self.config.color_table,
                &self.config.theme.palette(),
            );
        }
        drew
    }

    fn map_warnings(&self, ui: &egui::Ui, rect: Rect, transform: &map::MapTransform) {
        if !self.warning_layer.is_shown() {
            return;
        }
        for warning in &self.warnings {
            if self.warning_scope == WarningScope::Viewport
                && !warning_intersects(warning, transform, rect)
            {
                continue;
            }
            let colour = warning_colour(warning.severity);
            for ring in &warning.polygons {
                let points: Vec<Pos2> = ring
                    .iter()
                    .filter_map(|(lat, lon)| transform.screen(*lat, *lon))
                    .collect();
                if points.len() < 3 {
                    continue;
                }
                let width = points
                    .iter()
                    .map(|point| point.x)
                    .fold(f32::NEG_INFINITY, f32::max)
                    - points
                        .iter()
                        .map(|point| point.x)
                        .fold(f32::INFINITY, f32::min);
                if width > rect.width() * 4.0 {
                    continue;
                }
                let (width, colour) =
                    if self.selected_warning.as_deref() == Some(warning.id.as_str()) {
                        (4.0_f32, TEXT)
                    } else {
                        (2.0_f32, colour)
                    };
                ui.painter()
                    .add(Shape::closed_line(points, Stroke::new(width, colour)));
            }
        }
    }

    fn radar_warnings(
        &self,
        painter: &egui::Painter,
        frame: &Frame,
        transform: &ViewTransform,
        viewport: Rect,
    ) -> Option<String> {
        if !self.warning_layer.is_shown() {
            return None;
        }
        for warning in &self.warnings {
            let mut rings = Vec::new();
            let mut bounds: Option<Rect> = None;
            for ring in &warning.polygons {
                let mut points = Vec::with_capacity(ring.len());
                for (lat, lon) in ring {
                    let offset = match render::place_offset(
                        *lat,
                        *lon,
                        frame.geometry.site_x,
                        frame.geometry.site_y,
                    ) {
                        Ok(offset) => offset,
                        Err(error) => {
                            return Some(format!(
                                "Warning {} could not be placed: {error}",
                                warning.event
                            ));
                        }
                    };
                    let point = transform.screen(offset);
                    points.push(point);
                    bounds = Some(bounds.map_or_else(
                        || Rect::from_min_max(point, point),
                        |bounds| {
                            Rect::from_min_max(
                                Pos2::new(bounds.min.x.min(point.x), bounds.min.y.min(point.y)),
                                Pos2::new(bounds.max.x.max(point.x), bounds.max.y.max(point.y)),
                            )
                        },
                    ));
                }
                if points.len() >= 3 {
                    rings.push(points);
                }
            }
            if rings.is_empty() || !bounds.is_some_and(|bounds| bounds.intersects(viewport)) {
                continue;
            }
            let selected = self.selected_warning.as_deref() == Some(warning.id.as_str());
            let width = if selected { 4.0_f32 } else { 2.0_f32 };
            let colour = if selected {
                TEXT
            } else {
                warning_colour(warning.severity)
            };
            for points in rings {
                painter.add(Shape::closed_line(points, Stroke::new(width, colour)));
            }
        }
        None
    }

    fn map_measure(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        interaction: &egui::Response,
    ) {
        if !self.measure.is_on() {
            return;
        }
        let palette = self.config.theme.palette();
        if interaction.clicked()
            && let Some(pointer) = ui.input(|input| input.pointer.hover_pos())
            && rect.contains(pointer)
        {
            let point = match transform.latlon_at(pointer) {
                Ok(point) => point,
                Err(error) => {
                    self.notice = Some(format!("Measurement point unavailable: {error}"));
                    return;
                }
            };
            if self.measure_start.is_none() || self.measure_end.is_some() {
                self.measure_start = Some(point);
                self.measure_end = None;
            } else {
                self.measure_end = Some(point);
            }
        }
        let painter = ui.painter();
        let Some(start) = self.measure_start else {
            return;
        };
        let Some(start_position) = transform.screen(start.0, start.1) else {
            return;
        };
        painter.circle_filled(start_position, 5.0, palette.warning);
        painter.text(
            start_position + Vec2::new(8.0, 8.0),
            Align2::LEFT_TOP,
            "A",
            FontId::monospace(12.0),
            STALE,
        );
        let Some(end) = self.measure_end else {
            painter.text(
                start_position + Vec2::new(8.0, 24.0),
                Align2::LEFT_TOP,
                "Click a second point",
                FontId::monospace(10.0),
                palette.map_text,
            );
            return;
        };
        let Some(end_position) = transform.screen(end.0, end.1) else {
            return;
        };
        painter.line_segment([start_position, end_position], Stroke::new(2.0_f32, STALE));
        painter.circle_filled(end_position, 5.0, STALE);
        painter.text(
            end_position + Vec2::new(8.0, 8.0),
            Align2::LEFT_TOP,
            "B",
            FontId::monospace(12.0),
            STALE,
        );
        let detail = match crate::projection::inverse(start.0, start.1, end.0, end.1) {
            Ok((distance_km, bearing_deg)) => format!(
                "{} · {bearing_deg:.0}° {}",
                crate::units::distance(distance_km, self.config.units),
                place::compass(bearing_deg)
            ),
            Err(error) => format!("Measurement unavailable: {error}"),
        };
        let label = format!(
            "{detail}\nA {:.4}, {:.4}\nB {:.4}, {:.4}",
            start.0, start.1, end.0, end.1
        );
        let galley = painter.layout_no_wrap(label, FontId::monospace(10.0), TEXT);
        let padding = Vec2::splat(6.0);
        let size = galley.size() + 2.0 * padding;
        let midpoint = start_position.lerp(end_position, 0.5);
        let origin = Pos2::new(
            (midpoint.x - size.x / 2.0).clamp(
                rect.left() + 4.0,
                (rect.right() - size.x - 4.0).max(rect.left() + 4.0),
            ),
            (midpoint.y - size.y - 10.0).clamp(
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

    fn map_basemap(&self, ui: &egui::Ui, rect: Rect) {
        match &self.map {
            Ok(world) => {
                if let Err(error) = world.draw(
                    ui.painter(),
                    &self.map_view,
                    rect,
                    &self.config.theme.palette(),
                ) {
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
        let palette = self.config.theme.palette();
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
                    ui.painter().circle_stroke(
                        position,
                        8.0,
                        Stroke::new(2.0_f32, palette.warning),
                    );
                }
                ui.painter().circle_filled(
                    position,
                    if selected { 4.0 } else { 2.4 },
                    if selected {
                        palette.warning
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
                        if selected {
                            palette.warning
                        } else {
                            palette.map_text
                        },
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
        let palette = self.config.theme.palette();
        let Some(position) = transform.screen(place.lat, place.lon) else {
            return;
        };
        ui.painter()
            .circle_filled(position, 5.0, Color32::from_rgb(245, 245, 240));
        ui.painter().text(
            position + Vec2::new(8.0, 8.0),
            Align2::LEFT_TOP,
            place.label(),
            FontId::proportional(theme::TYPE_SMALL),
            palette.map_text,
        );
    }

    fn map_controls(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let area = Rect::from_min_size(
            Pos2::new(rect.left() + 16.0, rect.top() + 70.0),
            Vec2::new(190.0, 230.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(area)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.style_mut().visuals = self.config.theme.visuals();
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                self.map_layer_controls(ui, rect);
                self.map_overlay_controls(ui);
            },
        );
        self.map_status(ui, rect);
    }

    fn map_layer_controls(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let palette = self.config.theme.palette();
        if ui
            .add_sized(
                [116.0, 30.0],
                action_button(
                    &palette,
                    if self.map_radar.is_shown() {
                        "Hide radar layer"
                    } else {
                        "Show radar layer"
                    },
                ),
            )
            .clicked()
        {
            self.map_radar = self.map_radar.toggled();
        }
        ui.horizontal(|ui| {
            if ui
                .add_sized([34.0, 30.0], action_button(&palette, "+"))
                .clicked()
            {
                match self.map_view.zoom_at(rect, rect.center(), 1.6) {
                    Ok(()) => self.schedule_viewport_reload(),
                    Err(error) => self.notice = Some(format!("Map zoom failed: {error}")),
                }
            }
            if ui
                .add_sized([34.0, 30.0], action_button(&palette, "−"))
                .clicked()
            {
                match self.map_view.zoom_at(rect, rect.center(), 1.0 / 1.6) {
                    Ok(()) => self.schedule_viewport_reload(),
                    Err(error) => self.notice = Some(format!("Map zoom failed: {error}")),
                }
            }
            if self.place.is_some()
                && ui
                    .add_sized([64.0, 30.0], action_button(&palette, "Focus"))
                    .clicked()
            {
                self.focus_map_context();
                self.schedule_viewport_reload();
            }
        });
        if ui
            .add_sized([116.0, 30.0], action_button(&palette, "Reload nearby"))
            .on_hover_text("Find up to three stations around the current map centre.")
            .clicked()
        {
            self.reload_viewport_radars();
        }
    }

    fn map_overlay_controls(&mut self, ui: &mut egui::Ui) {
        let palette = self.config.theme.palette();
        if ui
            .add_sized(
                [116.0, 30.0],
                toggle_button(
                    &palette,
                    if self.warning_layer.is_shown() {
                        "Hide warnings"
                    } else {
                        "Show warnings"
                    },
                    self.warning_layer.is_shown(),
                ),
            )
            .on_hover_text("Show active National Weather Service warning polygons for the United States and territories.")
            .clicked()
        {
            self.warning_layer = self.warning_layer.toggled();
            if self.warning_layer.is_shown() && matches!(self.warning_state, WarningState::Idle) {
                self.warning_request_due = Some(Instant::now());
            }
        }
        if ui
            .add_sized(
                [158.0, 30.0],
                toggle_button(
                    &palette,
                    if self.global_layer.is_shown() {
                        "Global radar: on"
                    } else {
                        "Global radar: off"
                    },
                    self.global_layer.is_shown(),
                ),
            )
            .on_hover_text("RainViewer global reflectivity composite with two hours of history.")
            .clicked()
        {
            self.global_layer = self.global_layer.toggled();
            if !self.global_layer.is_shown() {
                self.global_playback = GlobalPlayback::Stopped;
            } else if self.global_frames.is_empty() {
                self.request_global_radar();
            }
        }
        if ui
            .add_sized(
                [116.0, 30.0],
                toggle_button(
                    &palette,
                    if self.measure.is_on() {
                        "Measure: on"
                    } else {
                        "Measure: off"
                    },
                    self.measure.is_on(),
                ),
            )
            .on_hover_text(
                "Click two points on the map to measure geodesic distance and initial bearing.",
            )
            .clicked()
        {
            self.measure = self.measure.toggled();
            self.measure_start = None;
            self.measure_end = None;
        }
    }

    fn map_status_details(&self, ui: &mut egui::Ui, rect: Rect) {
        let Some(area) = map_status_area(rect) else {
            return;
        };
        let palette = self.config.theme.palette();
        let width = area.width();
        let height = area.height();
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(area)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(
                        palette.surface.r(),
                        palette.surface.g(),
                        palette.surface.b(),
                        232,
                    ))
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.set_width(width - 12.0);
                        egui::ScrollArea::vertical()
                            .max_height((height - 12.0).max(0.0))
                            .show(ui, |ui| {
                                if let Some(layers) = self.map_layer_line() {
                                    ui.label(
                                        RichText::new(layers)
                                            .monospace()
                                            .size(theme::TYPE_SMALL)
                                            .weak(),
                                    );
                                }
                                if self.warning_layer.is_shown()
                                    && let Some(warning) = self.most_severe_warning()
                                {
                                    ui.label(
                                        RichText::new(format!(
                                            "Most severe: {} · {} · {} · expires {}",
                                            warning.event,
                                            warning.area,
                                            warning.urgency.label(),
                                            warning.expires
                                        ))
                                        .monospace()
                                        .size(theme::TYPE_SMALL)
                                        .color(ui.visuals().error_fg_color),
                                    );
                                    if !warning.sent.is_empty() {
                                        ui.label(
                                            RichText::new(format!("Issued {}", warning.sent))
                                                .monospace()
                                                .size(theme::TYPE_SMALL)
                                                .weak(),
                                        );
                                    }
                                }
                            });
                    });
            },
        );
    }

    fn map_status(&self, ui: &mut egui::Ui, rect: Rect) {
        let countries = self.map.as_ref().map_or(0, WorldMap::country_count);
        self.map_status_details(ui, rect);
        let palette = self.config.theme.palette();
        let secondary = Color32::from_rgba_unmultiplied(
            palette.map_text.r(),
            palette.map_text.g(),
            palette.map_text.b(),
            200,
        );
        let centre = self.map_view.center_latlon().map_or_else(
            |_| String::from("unavailable"),
            |(lat, lon)| format!("{lat:.2}, {lon:.2}"),
        );
        let panel = Rect::from_min_size(
            rect.left_top() + Vec2::new(8.0, 8.0),
            Vec2::new(470.0, 62.0),
        );
        ui.painter().rect_filled(
            panel,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(
                palette.surface.r(),
                palette.surface.g(),
                palette.surface.b(),
                214,
            ),
        );
        ui.painter().text(
            panel.left_top() + Vec2::new(10.0, 6.0),
            Align2::LEFT_TOP,
            "Geographic map",
            FontId::proportional(theme::TYPE_TITLE),
            palette.map_text,
        );
        ui.painter().text(
            panel.left_top() + Vec2::new(10.0, 29.0),
            Align2::LEFT_TOP,
            format!(
                "Centre {centre} · {countries} countries · {:.0} km wide · {} viewport radars",
                self.map_view.span_km(),
                self.view_radars.len()
            ),
            FontId::proportional(theme::TYPE_SMALL),
            secondary,
        );
        ui.painter().text(
            panel.left_top() + Vec2::new(10.0, 44.0),
            Align2::LEFT_TOP,
            "Drag to pan · scroll to zoom · click a radar site",
            FontId::proportional(theme::TYPE_SMALL),
            secondary,
        );
        ui.painter().text(
            Pos2::new(rect.right() - 12.0, rect.bottom() - 12.0),
            Align2::RIGHT_BOTTOM,
            "Natural Earth 1:110m · public domain",
            FontId::proportional(theme::TYPE_SMALL),
            secondary,
        );
    }

    fn map_layer_line(&self) -> Option<String> {
        let reference = self.displayed_scan_reference();
        let mut contributing = Vec::new();
        let mut excluded = Vec::new();
        if self.map_radar.is_shown() {
            if self
                .display
                .frame
                .as_ref()
                .is_some_and(|frame| frame.geometry.product == self.product)
                && let Some(site) = &self.site
            {
                contributing.push(format!("{site} selected"));
            }
            for layer in &self.view_radars {
                let Some(frame) = layer.display.frame.as_ref() else {
                    continue;
                };
                if frame.geometry.product != self.product {
                    continue;
                }
                if reference
                    .is_none_or(|reference| scans_align(reference, frame.volume.scan_unix_ms))
                {
                    contributing.push(layer.site.to_string());
                } else {
                    excluded.push(layer.site.to_string());
                }
            }
        }
        let contributing = contributing.join(", ");
        let time_note = if excluded.is_empty() {
            String::new()
        } else {
            format!(" · time-excluded: {}", excluded.join(", "))
        };
        let global = if self.global_layer.is_shown() {
            match &self.global_state {
                GlobalState::Idle => String::new(),
                GlobalState::Loading => String::from(" · global loading"),
                GlobalState::Ready => String::from(" · global RainViewer"),
                GlobalState::Failed => String::from(" · global error"),
            }
        } else {
            String::from(" · global off")
        };
        if contributing.is_empty()
            && !self.global_layer.is_shown()
            && !self.warning_layer.is_shown()
        {
            return None;
        }
        let warnings = self.warning_layer_summary();
        let scope = if self.warning_scope == WarningScope::Viewport {
            " · warnings in view"
        } else {
            ""
        };
        Some(format!(
            "Layers: {contributing}{global}{warnings}{scope}{time_note}"
        ))
    }

    fn warning_layer_summary(&self) -> String {
        if !self.warning_layer.is_shown() {
            return String::from(" · warnings off");
        }
        match self.warning_state {
            WarningState::Idle => String::new(),
            WarningState::Loading => String::from(" · warnings loading"),
            WarningState::Ready => {
                let mut text = format!(" · {} warnings", self.warnings.len());
                if (self.warnings_without_geometry > 0 || self.warnings_rejected > 0)
                    && write!(
                        text,
                        " · {} without polygons · {} unreadable",
                        self.warnings_without_geometry, self.warnings_rejected
                    )
                    .is_err()
                {
                    text.push_str(" · warning counts unavailable");
                }
                text
            }
            WarningState::Failed => String::from(" · warnings error"),
        }
    }

    fn most_severe_warning(&self) -> Option<&Warning> {
        self.warnings
            .iter()
            .max_by_key(|warning| severity_rank(warning.severity))
    }

    fn poll_history(&mut self) {
        loop {
            let event = match self.history.feed.poll() {
                Ok(Some(event)) => event,
                Ok(None) => return,
                Err(error) => {
                    if self.history.error.is_none() {
                        self.history_failure(error);
                    }
                    return;
                }
            };
            self.apply_history_event(event);
        }
    }

    fn apply_history_event(&mut self, event: HistoryEvent) {
        match event {
            HistoryEvent::Keys { keys, .. } => {
                self.history.keys = keys;
                self.history.state = HistoryState::Ready;
            }
            HistoryEvent::Volume {
                key,
                volume,
                from_cache,
                cache_warning,
                ..
            } => {
                self.history.key = Some(key.clone());
                self.history.state = HistoryState::Reviewing;
                self.history.scrub_index = None;
                self.display.submit(
                    RetrievedVolume {
                        volume,
                        origin: ArchiveOrigin::from_cache(from_cache),
                        history_key: Some(key),
                    },
                    self.product,
                );
                self.notice = cache_warning.map(|warning| format!("NEXRAD cache: {warning}"));
                if self.history.playback == GlobalPlayback::Playing {
                    self.history.next_frame =
                        Some(due_after(Duration::from_millis(HISTORY_FRAME_MS)));
                } else {
                    self.history.next_frame = None;
                }
            }
            HistoryEvent::Failed {
                error,
                cache_warning,
                ..
            } => {
                self.history_failure(error);
                if let Some(warning) = cache_warning {
                    let failure = self
                        .notice
                        .take()
                        .unwrap_or_else(|| String::from("Radar history failed."));
                    self.notice = Some(format!("{failure} Cache warning: {warning}."));
                }
            }
        }
    }

    fn history_failure(&mut self, error: HistoryError) {
        self.notice = Some(format!("Radar history: {error}"));
        self.history.error = Some(error);
        self.history.state = HistoryState::Failed;
        self.history.playback = GlobalPlayback::Stopped;
        self.history.next_frame = None;
        self.history.scrub_index = None;
    }

    fn request_history_keys(&mut self) {
        let Some(site) = self.site.clone() else {
            self.notice = Some(String::from(
                "Choose a radar site before opening its history.",
            ));
            return;
        };
        self.history.keys.clear();
        self.history.key = None;
        self.history.error = None;
        match self.history.feed.request_keys(&site) {
            Ok(()) => self.history.state = HistoryState::LoadingKeys,
            Err(error) => self.history_failure(error),
        }
    }

    fn request_history_volume(&mut self, key: &str) {
        let Some(site) = self.site.clone() else {
            self.notice = Some(String::from(
                "Choose a radar site before opening its history.",
            ));
            return;
        };
        self.scene_mode = SceneMode::Radar;
        self.pipeline = None;
        self.history.key = Some(key.to_owned());
        self.history.error = None;
        self.history.next_frame = None;
        match self.history.feed.request_volume(&site, key) {
            Ok(()) => self.history.state = HistoryState::LoadingVolume,
            Err(error) => self.history_failure(error),
        }
    }

    fn start_history_playback(&mut self, start: HistoryStart) {
        let key = match start {
            HistoryStart::Oldest => history::next_key(&self.history.keys, None),
            HistoryStart::Current => {
                let current = self
                    .history
                    .key
                    .as_deref()
                    .filter(|key| self.history.keys.iter().any(|item| item == *key));
                current
                    .map(String::from)
                    .or_else(|| history::next_key(&self.history.keys, None))
            }
        };
        let Some(key) = key else {
            self.notice = Some(String::from(
                "Load the recent scan list before playing radar history.",
            ));
            return;
        };
        self.history.playback = GlobalPlayback::Playing;
        self.history.next_frame = None;
        self.request_history_volume(&key);
    }

    fn toggle_history_playback(&mut self) {
        if self.history.playback == GlobalPlayback::Playing {
            self.history.playback = GlobalPlayback::Stopped;
            self.history.next_frame = None;
        } else {
            self.start_history_playback(HistoryStart::Current);
        }
    }

    fn advance_history_playback(&mut self) {
        if self.history.playback != GlobalPlayback::Playing {
            return;
        }
        if !self
            .history
            .next_frame
            .is_some_and(|due| Instant::now() >= due)
        {
            return;
        }
        if self.display.state.error().is_some() {
            self.history.playback = GlobalPlayback::Stopped;
            self.history.next_frame = None;
            self.notice = Some(String::from(
                "History playback stopped because the current frame could not be prepared.",
            ));
            return;
        }
        if self.display.state.is_busy() {
            self.history.next_frame = Some(due_after(Duration::from_millis(BUSY_REPAINT_MS)));
            return;
        }
        let Some(key) = history::next_key(&self.history.keys, self.history.key.as_deref()) else {
            self.history.playback = GlobalPlayback::Stopped;
            self.history.next_frame = None;
            return;
        };
        self.history.next_frame = None;
        self.request_history_volume(&key);
    }

    fn return_to_live(&mut self) {
        let Some(site) = self.site.clone() else {
            return;
        };
        self.open_site(site, false);
        self.notice = Some(String::from("Returned to the live radar feed."));
    }

    fn history_section(&mut self, ui: &mut egui::Ui) {
        if self.site.is_none() {
            return;
        }
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Recent NEXRAD scans")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new("Archive II volumes · scan times in UTC")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let mut load = false;
        let mut selected = None;
        let mut retry = None;
        let mut resume = false;
        let mut play_all = false;
        let mut play_toggle = false;
        match self.history.state {
            HistoryState::Idle => {
                ui.label(
                    RichText::new("Load the latest scans without leaving the live feed.")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
                if ui.button("Load recent scans").clicked() {
                    load = true;
                }
            }
            HistoryState::LoadingKeys => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Listing recent volumes…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            HistoryState::Ready => {
                if self.history.keys.is_empty() {
                    ui.label(
                        RichText::new("No complete volumes were listed in the last three days.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                } else if let Some(key) = self.history_scan_list(ui) {
                    selected = Some(key);
                }
                if !self.history.keys.is_empty() && ui.button("Play all scans").clicked() {
                    play_all = true;
                }
                if ui.small_button("Refresh scan list").clicked() {
                    load = true;
                }
            }
            HistoryState::LoadingVolume => {
                let key = self
                    .history
                    .key
                    .as_deref()
                    .map_or_else(|| String::from("selected scan"), history::key_label);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Fetching {key}…"))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            HistoryState::Reviewing => {
                let (toggle, live, scrub) = self.history_reviewing_ui(ui);
                play_toggle = toggle;
                resume = live;
                if scrub.is_some() {
                    selected = scrub;
                }
            }
            HistoryState::Failed => {
                let (reload, retry_key) = self.history_failure_controls(ui);
                load |= reload;
                retry = retry_key;
            }
        }
        if play_all {
            self.start_history_playback(HistoryStart::Oldest);
        }
        if play_toggle {
            self.toggle_history_playback();
        }
        self.apply_history_actions(load, selected, retry, resume);
    }

    fn history_reviewing_ui(&mut self, ui: &mut egui::Ui) -> (bool, bool, Option<String>) {
        let mut play_toggle = false;
        let mut resume = false;
        let mut scrub = None;
        let key = self
            .history
            .key
            .as_deref()
            .map_or_else(|| String::from("selected scan"), history::key_label);
        let displayed = self
            .display
            .frame
            .as_ref()
            .and_then(|frame| frame.history_key.as_deref());
        let message = if displayed == self.history.key.as_deref() && displayed.is_some() {
            format!("Reviewing {key}")
        } else {
            format!("Preparing {key}…")
        };
        ui.label(RichText::new(message).size(theme::TYPE_SMALL));
        ui.label(
            RichText::new("Live radar is paused while you inspect this scan.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        if let Some(current) = self
            .history
            .key
            .as_deref()
            .and_then(|key| self.history.keys.iter().position(|item| item == key))
        {
            let last = self.history.keys.len().saturating_sub(1);
            let mut requested = self.history.scrub_index.unwrap_or(current).min(last);
            let response = ui.add(
                egui::Slider::new(&mut requested, 0..=last)
                    .show_value(false)
                    .text("scan"),
            );
            if response.changed() {
                self.history.scrub_index = Some(requested);
            }
            if (response.drag_stopped() || response.clicked())
                && let Some(key) = self.history.keys.get(requested)
            {
                scrub = Some(key.clone());
                self.history.scrub_index = None;
            }
        }
        ui.horizontal(|ui| {
            let label = if self.history.playback == GlobalPlayback::Playing {
                "Pause history"
            } else {
                "Play history"
            };
            if ui.button(label).clicked() {
                play_toggle = true;
            }
            if let Some(index) = self
                .history
                .keys
                .iter()
                .position(|item| Some(item.as_str()) == self.history.key.as_deref())
            {
                ui.label(
                    RichText::new(format!(
                        "Frame {}/{}",
                        index.saturating_add(1),
                        self.history.keys.len()
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
        });
        if ui.button("Return to live radar").clicked() {
            resume = true;
        }
        (play_toggle, resume, scrub)
    }

    fn history_scan_list(&self, ui: &mut egui::Ui) -> Option<String> {
        let mut selected = None;
        egui::ScrollArea::vertical()
            .max_height(150.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for key in &self.history.keys {
                    if ui
                        .add_sized(
                            [ui.available_width(), 32.0],
                            egui::Button::new(
                                RichText::new(history::key_label(key)).size(theme::TYPE_SMALL),
                            )
                            .fill(ui.visuals().faint_bg_color),
                        )
                        .clicked()
                    {
                        selected = Some(key.clone());
                    }
                }
            });
        selected
    }

    fn history_failure_controls(&self, ui: &mut egui::Ui) -> (bool, Option<String>) {
        let mut load = false;
        let mut retry = None;
        if let Some(error) = &self.history.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("History failed: {error}"),
            );
        }
        if let Some(key) = &self.history.key
            && ui.button("Retry selected scan").clicked()
        {
            retry = Some(key.clone());
        }
        if ui.button("Reload scan list").clicked() {
            load = true;
        }
        (load, retry)
    }

    fn apply_history_actions(
        &mut self,
        load: bool,
        selected: Option<String>,
        retry: Option<String>,
        resume: bool,
    ) {
        if load {
            self.request_history_keys();
        }
        if let Some(key) = retry {
            self.request_history_volume(&key);
        }
        if let Some(key) = selected {
            self.history.playback = GlobalPlayback::Stopped;
            self.history.next_frame = None;
            self.request_history_volume(&key);
        }
        if resume {
            self.return_to_live();
        }
    }

    fn warning_section(&mut self, ui: &mut egui::Ui) {
        if !self.warning_layer.is_shown() || self.warnings.is_empty() {
            return;
        }
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Active warnings")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new("National Weather Service · United States and territories")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.warning_scope, WarningScope::All, "All active");
            ui.selectable_value(
                &mut self.warning_scope,
                WarningScope::Viewport,
                "Visible map",
            );
        });
        let mut order: Vec<usize> = (0..self.warnings.len()).collect();
        order.sort_by_key(|index| {
            self.warnings
                .get(*index)
                .map_or(0, |warning| severity_rank(warning.severity))
        });
        order.reverse();
        if self.warning_scope == WarningScope::Viewport {
            order.retain(|index| {
                self.warnings
                    .get(*index)
                    .is_some_and(|warning| self.warning_visible(warning))
            });
        }
        let visible = order.len();
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for index in order.into_iter().take(40) {
                    let Some(warning) = self.warnings.get(index) else {
                        continue;
                    };
                    let selected = self.selected_warning.as_deref() == Some(warning.id.as_str());
                    let label = format!(
                        "{} · {}\n{} · expires {}",
                        warning.event,
                        warning.area,
                        warning.urgency.label(),
                        warning.expires
                    );
                    if ui
                        .add_sized(
                            [ui.available_width(), 52.0],
                            toggle_button(&self.config.theme.palette(), &label, selected),
                        )
                        .clicked()
                    {
                        chosen = Some(index);
                    }
                }
            });
        if visible == 0 {
            ui.label(
                RichText::new("No active warnings intersect the visible map.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
        if self.warnings.len() > 40 {
            ui.label(
                RichText::new(format!(
                    "Showing 40 most severe of {} active alerts.",
                    self.warnings.len()
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
        }
        if let Some(index) = chosen {
            self.focus_warning(index);
        }
        self.warning_detail(ui);
    }

    fn warning_visible(&self, warning: &Warning) -> bool {
        if self.warning_scope == WarningScope::All {
            return true;
        }
        let Some(rect) = self.map_viewport else {
            return true;
        };
        let Ok(transform) = self.map_view.transform(rect) else {
            return true;
        };
        warning_intersects(warning, &transform, rect)
    }

    fn warning_detail(&self, ui: &mut egui::Ui) {
        let Some(warning) = self
            .selected_warning
            .as_deref()
            .and_then(|id| self.warnings.iter().find(|warning| warning.id == id))
        else {
            return;
        };
        ui.add_space(10.0);
        ui.separator();
        ui.label(
            RichText::new("Selected alert")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        let title = if warning.headline.is_empty() {
            format!("{} for {}", warning.event, warning.area)
        } else {
            warning.headline.clone()
        };
        ui.label(RichText::new(title).size(theme::TYPE_LABEL).strong());
        ui.label(
            RichText::new(format!(
                "{} · {} · {}",
                warning.severity.label(),
                warning.urgency.label(),
                warning.area
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        ui.label(
            RichText::new(format!(
                "Issued {} · expires {}",
                warning.sent, warning.expires
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        if warning.description.is_empty() {
            ui.label(
                RichText::new("This alert carries no description text.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        } else {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.label(RichText::new(&warning.description).size(theme::TYPE_SMALL));
                    if let Some(instruction) = &warning.instruction {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Instructions")
                                .size(theme::TYPE_SMALL)
                                .strong()
                                .weak(),
                        );
                        ui.label(RichText::new(instruction).size(theme::TYPE_SMALL));
                    }
                });
        }
        ui.label(
            RichText::new(format!("Source: NWS active alerts · {}", warning.id))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }

    fn focus_warning(&mut self, index: usize) {
        let Some(warning) = self.warnings.get(index) else {
            self.notice = Some(String::from("That warning is no longer available."));
            return;
        };
        self.selected_warning = Some(warning.id.clone());
        let Some(ring) = warning.polygons.first() else {
            self.notice = Some(format!(
                "Warning {} has no mapped polygon; its area is {}.",
                warning.event, warning.area
            ));
            return;
        };
        let mut lat = 0.0_f64;
        let mut lon = 0.0_f64;
        for (point_lat, point_lon) in ring {
            lat += *point_lat;
            lon += *point_lon;
        }
        let count = ring.len() as f64;
        if count <= 0.0 {
            self.notice = Some(String::from("That warning polygon has no points."));
            return;
        }
        lat /= count;
        lon /= count;
        let label = format!("{} for {}", warning.event, warning.area);
        match MapView::focus(lat, lon, 700.0) {
            Ok(view) => {
                self.map_view = view;
                self.scene_mode = SceneMode::Map;
                self.schedule_viewport_reload();
                self.notice = Some(format!(
                    "Selected {label}; polygon centred near {lat:.2}, {lon:.2}."
                ));
            }
            Err(error) => {
                self.notice = Some(format!("Warning map centre unavailable: {error}"));
            }
        }
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
        if !rect.contains(pointer)
            || map_status_area(rect).is_some_and(|area| area.contains(pointer))
        {
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
            && let Some(frame) = &self.display.frame
            && frame.geometry.product == self.product
        {
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
        let palette = self.config.theme.palette();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, palette.canvas);
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
        overlay::draw_grid(ui.painter(), &frame.geometry, &transform, &palette);
        overlay::draw_rings(
            ui.painter(),
            &frame.geometry,
            &transform,
            self.config.units,
            &palette,
        );
        if let Some(error) = self.radar_warnings(ui.painter(), frame, &transform, viewport) {
            self.notice = Some(error);
        }
        overlay::draw_legend(
            ui.painter(),
            rect,
            frame.geometry.product,
            self.config.color_table,
            &palette,
        );
        if let Some(place) = &self.place {
            draw_place_marker(ui.painter(), frame, &transform, viewport, rect, place);
        }
        north_marker(ui.painter(), &viewport);
        map_caption(ui.painter(), rect, frame);
        readout(ui, frame, &transform, viewport, rect, self.config.units);
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
                "No local Level II station selected",
                vec![
                    format!("The forecast on the right is for {}.", place.label()),
                    String::from(
                        "Open Map for the global RainViewer composite and its two-hour history.",
                    ),
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
            (Some(_), Some(site), None)
                if matches!(
                    self.history.state,
                    HistoryState::LoadingVolume | HistoryState::Reviewing
                ) =>
            {
                (
                    "Reviewing radar history",
                    vec![
                        format!("The selected scan for {site} is being loaded."),
                        String::from(
                            "Use Recent NEXRAD scans to pick another scan or return to live.",
                        ),
                    ],
                )
            }
            (Some(_), Some(_), None) => (
                "Loading radar",
                vec![String::from("The pipeline is starting.")],
            ),
        }
    }
}

fn global_texture(
    context: &egui::Context,
    time_unix_ms: u64,
    bounds: TileBounds,
    tile: DecodedTile,
) -> Result<egui::TextureHandle, GlobalRadarError> {
    let DecodedTile {
        width,
        height,
        rgba,
    } = tile;
    let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
        return Err(GlobalRadarError::Pixels {
            width,
            height,
            bytes: rgba.len(),
        });
    };
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4));
    if width == 0 || height == 0 || expected != Some(rgba.len()) {
        return Err(GlobalRadarError::Pixels {
            width: u32::try_from(width).unwrap_or(u32::MAX),
            height: u32::try_from(height).unwrap_or(u32::MAX),
            bytes: rgba.len(),
        });
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
    Ok(context.load_texture(
        format!(
            "global-radar-{time_unix_ms}-{}-{}-{}",
            bounds.zoom, bounds.x, bounds.y
        ),
        image,
        egui::TextureOptions::LINEAR,
    ))
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

fn search_results(
    ui: &mut egui::Ui,
    places: &[Place],
    stations: &Result<Vec<Station>, StationError>,
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

fn forecast_body(
    ui: &mut egui::Ui,
    forecast: &Forecast,
    stale: bool,
    units: crate::units::UnitSystem,
) {
    ui.label(RichText::new(forecast.place.label()).size(15.0).strong());
    ui.label(
        RichText::new(format!("Forecast for {}", forecast.place.coordinates()))
            .size(theme::TYPE_SMALL)
            .weak(),
    );
    if stale {
        ui.colored_label(ui.visuals().warn_fg_color, "Stale forecast");
    }
    ui.add_space(theme::SPACE_XS);
    ui.label(
        RichText::new(format!(
            "{} · {} ({}) · {}",
            forecast.provider,
            forecast.timezone,
            forecast.timezone_abbreviation,
            timestamp_label(&forecast.current.observed)
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.separator();
    ui.label(RichText::new("Now").size(theme::TYPE_TITLE).strong());
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(
                crate::units::celsius_from(forecast.current.temperature, &forecast.current.unit)
                    .map_or_else(
                        || {
                            format!(
                                "{:.0} {}",
                                forecast.current.temperature, forecast.current.unit
                            )
                        },
                        |celsius| crate::units::temperature_whole(celsius, units),
                    ),
            )
            .size(20.0)
            .strong(),
        );
        ui.label(RichText::new(&forecast.current.short).size(theme::TYPE_LABEL));
    });
    let apparent = crate::units::celsius_from(forecast.current.apparent, &forecast.current.unit)
        .map_or_else(
            || format!("{:.0} {}", forecast.current.apparent, forecast.current.unit),
            |celsius| crate::units::temperature_whole(celsius, units),
        );
    ui.label(
        RichText::new(format!(
            "Feels {apparent} · humidity {:.0}% · {} · precipitation {}",
            forecast.current.humidity,
            crate::forecast::wind(
                forecast.current.wind_direction_deg,
                forecast.current.wind_speed_kmh,
                units,
            ),
            crate::units::precipitation(forecast.current.precipitation, units)
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
    );
    ui.add_space(6.0);
    ui.separator();
    ui.label(RichText::new("Hourly").size(theme::TYPE_TITLE).strong());
    let list_height = (ui.available_height() - 8.0).max(80.0);
    egui::ScrollArea::vertical()
        .max_height(list_height)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            for period in &forecast.hourly {
                forecast_period(ui, period, units);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Daily").size(theme::TYPE_TITLE).strong());
            for period in &forecast.daily {
                forecast_period(ui, period, units);
            }
        });
}

fn forecast_period(ui: &mut egui::Ui, period: &ForecastPeriod, units: crate::units::UnitSystem) {
    ui.label(RichText::new(&period.name).size(theme::TYPE_LABEL).strong());
    ui.horizontal_wrapped(|ui| {
        let high = crate::units::celsius_from(period.temperature, &period.unit);
        let low = period
            .minimum
            .and_then(|minimum| crate::units::celsius_from(minimum, &period.unit));
        let temperature = match (high, low) {
            (Some(high), Some(low)) => format!(
                "{:.0}/{:.0} {}",
                units.temperature(high),
                units.temperature(low),
                units.temperature_unit()
            ),
            (Some(high), None) => crate::units::temperature(high, units),
            (None, _) => period.minimum.map_or_else(
                || format!("{:.0} {}", period.temperature, period.unit),
                |minimum| format!("{:.0}/{:.0} {}", period.temperature, minimum, period.unit),
            ),
        };
        ui.label(RichText::new(temperature).size(17.0).strong());
        ui.label(RichText::new(&period.short).size(theme::TYPE_LABEL));
    });
    let precipitation = period
        .precipitation
        .map_or_else(String::new, |chance| format!(" · {chance}% precipitation"));
    let wind = period.wind_speed_kmh.map_or_else(String::new, |speed| {
        format!(" · wind {}", crate::units::wind_speed(speed, units))
    });
    ui.label(
        RichText::new(format!(
            "{}{wind}{precipitation}",
            timestamp_label(&period.start)
        ))
        .size(theme::TYPE_SMALL)
        .weak(),
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
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Scan time unavailable: {error}"),
            );
        }
    }
    match time::now_unix_ms() {
        Ok(now) if now < scan => {
            ui.colored_label(ui.visuals().error_fg_color, time::age_label(now, scan));
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
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Scan age unavailable: {error}"),
            );
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
        ui.add(
            egui::Label::new(
                RichText::new(text)
                    .size(theme::TYPE_SMALL)
                    .color(ui.visuals().warn_fg_color),
            )
            .wrap(),
        );
    } else {
        ui.add(egui::Label::new(RichText::new(text).size(theme::TYPE_SMALL).weak()).wrap());
    }
    if let Some((product, error)) = &volume.moment_error {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("Rejected {} radial: {error}", product.id()),
        );
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

fn readout(
    ui: &egui::Ui,
    frame: &Frame,
    transform: &ViewTransform,
    viewport: Rect,
    bounds: Rect,
    units: crate::units::UnitSystem,
) {
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
        "{:.4}, {:.4}\n{}  {:.0}°\n{site}{value}",
        info.lat,
        info.lon,
        crate::units::distance(info.range_km, units),
        info.azimuth_deg
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
        if self.applied_theme != Some(self.config.theme) {
            ui.ctx().set_visuals(self.config.theme.visuals());
            self.applied_theme = Some(self.config.theme);
        }
        self.update_workers(ui.ctx());
        self.header(ui);
        self.status(ui);
        self.rail(ui);
        self.forecast_panel(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.config.theme.palette().background))
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

    fn warning(id: &str, event: &str) -> Warning {
        Warning {
            id: String::from(id),
            event: String::from(event),
            severity: Severity::Severe,
            urgency: crate::warnings::Urgency::Immediate,
            headline: String::from(event),
            description: String::from("Test warning"),
            instruction: None,
            area: String::from("Test area"),
            sent: String::from("2026-10-31T12:00:00Z"),
            expires: String::from("2026-10-31T13:00:00Z"),
            polygons: Vec::new(),
        }
    }

    #[test]
    fn warning_selection_survives_reordering_and_clears_when_removed() {
        let mut selected = Some(String::from("alert-b"));
        let refreshed = vec![warning("alert-b", "Updated"), warning("alert-a", "Other")];
        retain_warning_selection(&mut selected, &refreshed);
        assert_eq!(selected.as_deref(), Some("alert-b"));
        assert_eq!(
            refreshed
                .iter()
                .find(|warning| Some(&warning.id) == selected.as_ref())
                .map(|warning| warning.event.as_str()),
            Some("Updated")
        );
        retain_warning_selection(&mut selected, &[warning("alert-a", "Other")]);
        assert_eq!(selected, None);
    }

    #[test]
    fn map_status_area_stays_within_the_map() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(410.0, 544.0));
        let area = map_status_area(rect);
        assert!(area.is_some_and(|area| rect.contains_rect(area)));
        assert_eq!(
            map_status_area(Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 100.0))),
            None
        );
    }

    #[test]
    fn viewport_scans_share_a_bounded_time_window() {
        assert!(scans_align(0, VIEW_RADAR_TIME_WINDOW_MS));
        assert!(scans_align(VIEW_RADAR_TIME_WINDOW_MS, 0));
        assert!(!scans_align(0, VIEW_RADAR_TIME_WINDOW_MS.saturating_add(1)));
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
