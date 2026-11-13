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
//! The radar workspace: the selected station's live or reviewed volume, the
//! viewport radars around it, product selection and the radar canvas.

use eframe::egui::{self, Align2, CornerRadius, FontId, Pos2, Rect};

use crate::acquire::Acquire;
use crate::canvas::{self, Canvas};
use crate::config::Config;
use crate::display::{Display, DisplayState};
use crate::gpu::RadarGpu;
use crate::map::{self, MapView};
use crate::overlay;
use crate::overlay_select::nearby_station_ids;
use crate::pipeline::{Pipeline, PipelineState};
use crate::place::Place;
use crate::product::RadarProduct;
use crate::radar_controls::{self, Selection};
use crate::station_panel::StationPanel;
use crate::stations::SiteId;
use crate::workspaces::RadarWorkspace;

const VIEW_RADAR_LIMIT: usize = 3;

pub(crate) struct ViewRadar {
    pub(crate) site: SiteId,
    pub(crate) pipeline: Pipeline,
    pub(crate) display: Display,
    failure_reported: bool,
}

pub(crate) struct RadarPanel {
    gpu: Option<RadarGpu>,
    site: Option<SiteId>,
    pipeline: Option<Pipeline>,
    display: Display,
    product: RadarProduct,
    view_radars: Vec<ViewRadar>,
    radar: RadarWorkspace,
}

impl RadarPanel {
    pub(crate) fn new(gpu: Option<RadarGpu>, config: &Config) -> Self {
        Self {
            gpu: gpu.clone(),
            site: None,
            pipeline: None,
            display: Display::new(
                gpu,
                config.color_table,
                config.sampling,
                config.storm_motion,
            ),
            product: RadarProduct::Reflectivity,
            view_radars: Vec::new(),
            radar: RadarWorkspace::new(),
        }
    }

    pub(crate) const fn site(&self) -> Option<&SiteId> {
        self.site.as_ref()
    }

    pub(crate) const fn display(&self) -> &Display {
        &self.display
    }

    pub(crate) const fn display_mut(&mut self) -> &mut Display {
        &mut self.display
    }

    pub(crate) const fn product(&self) -> RadarProduct {
        self.product
    }

    pub(crate) const fn pipeline(&self) -> Option<&Pipeline> {
        self.pipeline.as_ref()
    }

    pub(crate) fn view_radars(&self) -> &[ViewRadar] {
        &self.view_radars
    }

    pub(crate) fn open(&mut self, id: SiteId, acquire: &Acquire) {
        self.display.reset();
        self.pipeline = Some(Pipeline::start(id.clone(), acquire.clone()));
        self.site = Some(id);
        self.view_radars.clear();
    }

    pub(crate) fn clear(&mut self) {
        self.site = None;
        self.pipeline = None;
        self.display.reset();
        self.view_radars.clear();
    }

    pub(crate) fn apply_color_table(&mut self, table: crate::colormap::ColorTable) {
        self.display.set_color_table(table);
        for layer in &mut self.view_radars {
            layer.display.set_color_table(table);
        }
    }

    pub(crate) fn apply_motion(&mut self, motion: crate::derived::StormMotion) {
        self.display.set_motion(motion);
        for layer in &mut self.view_radars {
            layer.display.set_motion(motion);
        }
    }

    pub(crate) fn apply_sampling(&mut self, sampling: crate::render::Sampling) {
        self.display.set_sampling(sampling);
        for layer in &mut self.view_radars {
            layer.display.set_sampling(sampling);
        }
    }

    /// Polls the selected pipeline, the render, and every viewport radar.
    pub(crate) fn poll(&mut self, notice: &mut Option<String>) {
        if let Some(pipeline) = self.pipeline.as_mut() {
            let volume = pipeline.poll();
            if let Some(warning) = pipeline.take_cache_warning() {
                *notice = Some(format!("NEXRAD cache: {warning}"));
            }
            if let Some(volume) = volume {
                self.display.submit(volume, self.product);
            }
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
            *notice = Some(message);
        }
    }

    /// Reloads the products of every viewport radar after a product change.
    pub(crate) fn refresh_view_radars(&mut self) {
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

    /// The product and tilt controls, drawn when a volume is loaded.
    /// Returns whether the configuration changed and needs saving.
    pub(crate) fn product_section(
        &mut self,
        ui: &mut egui::Ui,
        station: &crate::stations::Station,
        config: &mut Config,
        notice: &mut Option<String>,
    ) -> bool {
        let Some(volume) = self.display.selection_volume() else {
            return false;
        };
        let selection = radar_controls::show(
            ui,
            station.capabilities(),
            volume,
            self.product,
            config.sampling,
            config.storm_motion,
            !self.display.state.is_busy(),
        );
        let (product, elevation) = match selection {
            Some(Selection::Product(product)) => (product, None),
            Some(Selection::Elevation(elevation)) => (self.product, Some(elevation)),
            Some(Selection::Sampling(sampling)) => {
                if config.sampling != sampling {
                    config.sampling = sampling;
                    self.apply_sampling(sampling);
                    return true;
                }
                return false;
            }
            Some(Selection::Motion(motion)) => {
                if config.storm_motion != motion {
                    config.storm_motion = motion;
                    self.apply_motion(motion);
                    return true;
                }
                return false;
            }
            None => return false,
        };
        if self.display.rebuild(product, elevation) {
            self.product = product;
            self.refresh_view_radars();
        } else {
            *notice = Some(String::from(
                "That product and elevation could not be prepared from the displayed volume.",
            ));
        }
        false
    }

    /// Fills the viewport radar set from the map centre when it is empty.
    pub(crate) fn ensure_viewport_radars(
        &mut self,
        view: &MapView,
        stations: &StationPanel,
        config: &Config,
        acquire: &Acquire,
        notice: &mut Option<String>,
    ) {
        if !self.view_radars.is_empty() {
            return;
        }
        let Some(gpu) = self.gpu.clone() else {
            return;
        };
        let (center_lat, center_lon) = match view.center_latlon() {
            Ok(center) => center,
            Err(error) => {
                *notice = Some(format!("Viewport radar centre unavailable: {error}"));
                return;
            }
        };
        let Ok(list) = stations.result() else {
            return;
        };
        let radius_km = view.span_km() / 2.0 + 250.0;
        let sites = match nearby_station_ids(
            list,
            center_lat,
            center_lon,
            radius_km,
            self.site.as_ref(),
            VIEW_RADAR_LIMIT,
        ) {
            Ok(sites) => sites,
            Err(error) => {
                *notice = Some(format!("Viewport radar distance failed: {error}"));
                return;
            }
        };
        let mut layers = Vec::with_capacity(sites.len());
        for site in sites {
            layers.push(ViewRadar {
                pipeline: Pipeline::start(site.clone(), acquire.clone()),
                display: Display::new(
                    Some(gpu.clone()),
                    config.color_table,
                    config.sampling,
                    config.storm_motion,
                ),
                site,
                failure_reported: false,
            });
        }
        self.view_radars = layers;
    }

    pub(crate) fn reload_viewport_radars(
        &mut self,
        view: &MapView,
        stations: &StationPanel,
        config: &Config,
        acquire: &Acquire,
        notice: &mut Option<String>,
    ) {
        self.view_radars.clear();
        self.ensure_viewport_radars(view, stations, config, acquire, notice);
    }

    /// The radar canvas, fitted to the selected station.
    #[expect(
        clippy::too_many_arguments,
        reason = "the canvas reads the map, stations, place and settings"
    )]
    pub(crate) fn draw(
        &mut self,
        ui: &mut egui::Ui,
        map: &mut crate::map_panel::MapPanel,
        stations: &StationPanel,
        place: Option<&Place>,
        config: &Config,
        history_reviewing: bool,
        notice: &mut Option<String>,
    ) -> Option<SiteId> {
        let full_rect = ui.max_rect();
        let palette = config.theme.palette();
        ui.painter()
            .rect_filled(full_rect, CornerRadius::ZERO, palette.canvas);
        if full_rect.width() <= 0.0 || full_rect.height() <= 0.0 {
            return None;
        }
        let (rect, legend_rect) = canvas::legend_split(full_rect, self.legend_expected());
        self.fit_radar_view(notice);
        if self.display.frame.is_none() {
            self.empty_state(ui, rect, place, history_reviewing);
            return None;
        }
        let transform = match self.radar.view.transform(rect) {
            Ok(transform) => transform,
            Err(error) => {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("Radar view unavailable: {error}"),
                    FontId::proportional(15.0),
                    canvas::ERROR,
                );
                return None;
            }
        };
        let view = self.radar.view;
        map.draw_basemap(ui, rect, &view);
        let interaction = ui.interact(
            rect,
            ui.id().with("radar-canvas"),
            egui::Sense::click_and_drag(),
        );
        if let Err(error) = canvas::canvas_input(ui, rect, &interaction, &mut self.radar.view) {
            *notice = Some(format!("The view could not be moved: {error}"));
        }
        if let Err(error) = map::draw_graticule(ui.painter(), &transform, rect, view.span_km()) {
            *notice = Some(format!("Radar grid unavailable: {error}"));
        }
        let radar_visible = self.radars(ui, rect, &transform, view.span_km(), false, notice);
        let ring_labels = self.draw_rings(ui, rect, &transform, stations, config, notice);
        map.draw_warnings(ui, rect, &transform);
        let mut occupied = canvas::chrome_obstacles(rect, Canvas::Radar).to_vec();
        self.draw_ring_labels(
            ui,
            rect,
            &transform,
            stations,
            config,
            &ring_labels,
            &mut occupied,
        );
        let clicked_site = map.draw_stations(
            ui,
            rect,
            &transform,
            &interaction,
            self.site.as_ref(),
            stations,
            config,
        );
        map.draw_place(ui, &transform, place, config);
        map::draw_scale(ui.painter(), rect, &transform, &palette);
        if radar_visible && let Some(legend) = legend_rect {
            ui.painter()
                .rect_filled(legend, CornerRadius::ZERO, palette.canvas);
            overlay::draw_legend(
                ui.painter(),
                legend,
                self.product,
                config.color_table,
                &palette,
            );
        }
        canvas::north_marker(ui.painter(), rect);
        if let Some(frame) = self.display.frame.as_ref() {
            canvas::map_caption(ui.painter(), rect, frame);
            if frame.geometry.raster.is_empty() {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    canvas::empty_frame_notice(frame.geometry.product),
                    FontId::proportional(15.0),
                    canvas::TEXT,
                );
            }
        }
        canvas::radar_readout(
            ui,
            rect,
            &transform,
            &self.display,
            self.product,
            radar_visible,
            None,
        );
        map.draw_attribution(ui, rect, &config.providers);
        clicked_site
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

    /// Draws every radar the view holds, nearest station last so the selected
    /// station covers the ones around it.
    fn radars(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        span_km: f64,
        include_viewport: bool,
        notice: &mut Option<String>,
    ) -> bool {
        let Some(gpu) = self.gpu.as_ref() else {
            return false;
        };
        let reference = self.displayed_scan_reference();
        let mut drew = false;
        let mut failure: Option<String> = None;
        for layer in self.view_radars.iter().rev().filter(|_| include_viewport) {
            let Some(frame) = layer.display.frame.as_ref() else {
                continue;
            };
            if frame.geometry.product != self.product {
                continue;
            }
            if !reference
                .is_none_or(|reference| canvas::scans_align(reference, frame.volume.scan_unix_ms))
            {
                continue;
            }
            match canvas::draw_map_display(
                &layer.display,
                gpu,
                ui.painter(),
                rect,
                transform,
                span_km,
            ) {
                Ok(true) => drew = true,
                Ok(false) => {}
                Err(error) => {
                    failure = Some(format!(
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
            match canvas::draw_map_display(
                &self.display,
                gpu,
                ui.painter(),
                rect,
                transform,
                span_km,
            ) {
                Ok(true) => drew = true,
                Ok(false) => {}
                Err(error) => {
                    failure = Some(format!("Selected radar could not be placed: {error}"));
                }
            }
        }
        if let Some(message) = failure {
            *notice = Some(message);
        }
        drew
    }

    /// Draws the viewport radars and the selected radar onto the map.
    pub(crate) fn draw_viewport_radars(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        span_km: f64,
        notice: &mut Option<String>,
    ) -> bool {
        self.radars(ui, rect, transform, span_km, true, notice)
    }

    pub(crate) fn draw_range_rings(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        stations: &StationPanel,
        config: &Config,
        notice: &mut Option<String>,
    ) -> Vec<map::RingLabel> {
        self.draw_rings(ui, rect, transform, stations, config, notice)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the ring stack needs geometry, station, units and occupied bands"
    )]
    pub(crate) fn draw_range_labels(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        stations: &StationPanel,
        config: &Config,
        labels: &[map::RingLabel],
        occupied: &mut Vec<Rect>,
    ) {
        self.draw_ring_labels(ui, rect, transform, stations, config, labels, occupied);
    }

    pub(crate) fn scan_reference(&self) -> Option<u64> {
        self.displayed_scan_reference()
    }

    /// Whether this workspace will draw a radar legend this frame.
    pub(crate) fn legend_expected(&self) -> bool {
        let selected = self
            .display
            .frame
            .as_ref()
            .is_some_and(|frame| frame.geometry.product == self.product);
        let viewport = self.view_radars.iter().any(|layer| {
            layer
                .display
                .frame
                .as_ref()
                .is_some_and(|frame| frame.geometry.product == self.product)
        });
        self.gpu.is_some() && (selected || viewport)
    }

    /// How far the loaded product reaches, or nothing when no volume says.
    ///
    /// Range rings have to stop somewhere, and the honest place is the range of
    /// the product actually decoded. When nothing is loaded there is no
    /// evidence for any particular reach, so rings are not drawn at all rather
    /// than at an assumed 400 km.
    pub(crate) fn coverage_km(&self) -> Option<f64> {
        let volume = match &self.display.frame {
            Some(frame) => &frame.volume,
            None => self.display.selection_volume()?,
        };
        let range_km = volume.sweep.max_ground_range_km;
        (range_km.is_finite() && range_km > 0.0).then_some(range_km)
    }

    fn site_station<'a>(&self, stations: &'a StationPanel) -> Option<&'a crate::stations::Station> {
        let site = self.site.as_ref()?;
        stations.find(site)
    }

    fn draw_rings(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        stations: &StationPanel,
        config: &Config,
        _notice: &mut Option<String>,
    ) -> Vec<map::RingLabel> {
        let Some(station) = self.site_station(stations) else {
            return Vec::new();
        };
        let Some(coverage_km) = self.coverage_km() else {
            return Vec::new();
        };
        let palette = config.theme.palette();
        let context = map::RingContext {
            painter: ui.painter(),
            rect,
            transform,
            site_lat: station.lat,
            site_lon: station.lon,
            coverage_km,
            units: config.units,
            palette: &palette,
        };
        match map::draw_range_rings(&context) {
            Ok(labels) => labels,
            Err(error) => {
                ui.painter().text(
                    Pos2::new(rect.left() + 16.0, rect.bottom() - 20.0),
                    Align2::LEFT_CENTER,
                    format!("Range rings could not be drawn: {error}"),
                    FontId::proportional(12.0),
                    canvas::ERROR,
                );

                Vec::new()
            }
        }
    }

    /// Places the ring distances after the place names, so a label never lands
    /// on top of a town.
    #[expect(
        clippy::too_many_arguments,
        reason = "ring labels need the geometry, station, units and occupied bands"
    )]
    fn draw_ring_labels(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        transform: &map::MapTransform,
        stations: &StationPanel,
        config: &Config,
        labels: &[map::RingLabel],
        occupied: &mut Vec<Rect>,
    ) {
        if labels.is_empty() {
            return;
        }
        let (Some(station), Some(coverage_km)) = (self.site_station(stations), self.coverage_km())
        else {
            return;
        };
        let palette = config.theme.palette();
        let context = map::RingContext {
            painter: ui.painter(),
            rect,
            transform,
            site_lat: station.lat,
            site_lon: station.lon,
            coverage_km,
            units: config.units,
            palette: &palette,
        };
        if let Err(error) = map::draw_ring_labels(&context, labels, occupied) {
            ui.painter().text(
                Pos2::new(rect.left() + 16.0, rect.bottom() - 36.0),
                Align2::LEFT_CENTER,
                format!("Range ring labels could not be drawn: {error}"),
                FontId::proportional(12.0),
                canvas::ERROR,
            );
        }
    }

    /// Fits the radar view to the selected station the first time its frame
    /// arrives, and after the station changes.
    fn fit_radar_view(&mut self, notice: &mut Option<String>) {
        let Some(site) = self.site.clone() else {
            return;
        };
        let Some((latitude, longitude, span_km)) = self.display.frame.as_ref().map(|frame| {
            (
                frame.geometry.site_lat,
                frame.geometry.site_lon,
                frame.geometry.extent_km * 2.1,
            )
        }) else {
            return;
        };
        if let Err(error) = self.radar.fit(&site, latitude, longitude, span_km) {
            *notice = Some(format!("Radar view could not be fitted: {error}"));
        }
    }

    fn empty_state(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        place: Option<&Place>,
        history_reviewing: bool,
    ) {
        let painter = ui.painter();
        let mut y = rect.center().y - 60.0;
        let (title, body) = self.empty_text(place, history_reviewing);
        painter.text(
            Pos2::new(rect.center().x, y),
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(21.0),
            canvas::TEXT,
        );
        y += 30.0;
        for line in body {
            painter.text(
                Pos2::new(rect.center().x, y),
                Align2::CENTER_CENTER,
                line,
                FontId::proportional(13.0),
                canvas::MUTED,
            );
            y += 21.0;
        }
    }

    fn empty_text(
        &self,
        place: Option<&Place>,
        history_reviewing: bool,
    ) -> (&'static str, Vec<String>) {
        match (place, self.site.as_ref(), self.pipeline.as_ref()) {
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
            (Some(_), Some(site), None) if history_reviewing => (
                "Reviewing radar history",
                vec![
                    format!("The selected scan for {site} is being loaded."),
                    String::from("Use Recent NEXRAD scans to pick another scan or return to live."),
                ],
            ),
            (Some(_), Some(_), None) => (
                "Loading radar",
                vec![String::from("The pipeline is starting.")],
            ),
        }
    }

    /// The state line for the status bar, aware of history review.
    pub(crate) fn phase(&self, history: &crate::history_panel::HistoryPanel) -> &'static str {
        self.pipeline.as_ref().map_or_else(
            || match history.state() {
                crate::history_panel::HistoryState::LoadingVolume if history.is_playing() => {
                    "History playing"
                }
                crate::history_panel::HistoryState::LoadingVolume => "History loading",
                crate::history_panel::HistoryState::Reviewing
                    if self.display.state.error().is_some() =>
                {
                    "History render error"
                }
                crate::history_panel::HistoryState::Reviewing if self.display.state.is_busy() => {
                    "History preparing"
                }
                crate::history_panel::HistoryState::Reviewing if history.is_playing() => {
                    "History playing"
                }
                crate::history_panel::HistoryState::Reviewing => "History review",
                crate::history_panel::HistoryState::Failed => "History error",
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

    pub(crate) fn failed(&self) -> bool {
        self.pipeline
            .as_ref()
            .is_some_and(|pipeline| pipeline.state.failure().is_some())
            || self.display.state.error().is_some()
    }

    pub(crate) fn retry_pipeline(&mut self, notice: &mut Option<String>) {
        if let Some(pipeline) = self.pipeline.as_mut() {
            match pipeline.retry() {
                Ok(()) => {
                    *notice = Some(String::from("Asking for the newest volume now."));
                }
                Err(error) => *notice = Some(format!("Refresh unavailable: {error}")),
            }
        }
    }

    pub(crate) fn retry_display(&mut self) {
        self.display.retry();
    }

    pub(crate) fn selected_station<'a>(
        &self,
        stations: &'a StationPanel,
    ) -> Option<&'a crate::stations::Station> {
        self.site_station(stations)
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.site.is_some()
            && (self.display.state.is_busy()
                || self
                    .pipeline
                    .as_ref()
                    .is_some_and(|pipeline| pipeline.state.is_working())
                || self.display.frame.is_none())
    }

    /// Drops the live pipeline when history review takes over the display.
    pub(crate) fn pause_live(&mut self) {
        self.pipeline = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_radar_with_no_place_asks_where_weather_is() {
        let panel = RadarPanel::new(None, &Config::default());
        let (title, body) = panel.empty_text(None, false);
        assert_eq!(title, "Where do you want weather for?");
        assert_eq!(body.len(), 2);
        assert!(body[0].contains("search for a town"));
    }
}
