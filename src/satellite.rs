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
//! Satellite imagery from NASA's Global Imagery Browse Services.
//!
//! GIBS republishes each day's swaths as Web Mercator tiles, so the map can
//! show what the satellites saw above the country outlines and under the
//! radar. A day here is the provider's own composite of that UTC day, not one
//! acquisition: the interface states the day and the layer, and never pretends
//! the picture is a single moment.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, RichText, TextureHandle};

use crate::app::toggle_button;
use crate::global_radar::{self, TileBounds};
use crate::map;
use crate::provider::{self, JsonError};
use crate::theme::{self, Palette};
use crate::time;

const GIBS: &str = "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best";
const PROVIDER: &str = "NASA EOSDIS GIBS";
const MAX_TILE_BYTES: u64 = 2 * 1024 * 1024;
const VIEW_TILE_LIMIT: usize = 9;
const DAYS_BACK: u8 = 6;
const CACHED_TILES: usize = 96;
const RETRY_BASE: Duration = Duration::from_secs(3);
const RETRY_MAX: Duration = Duration::from_secs(60);
const RETRY_STEPS: u32 = 5;

/// One published imagery layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Imagery {
    TrueColor,
    CloudTopTemp,
    Aerosol,
    Precipitation,
}

impl Imagery {
    pub const ALL: [Self; 4] = [
        Self::TrueColor,
        Self::CloudTopTemp,
        Self::Aerosol,
        Self::Precipitation,
    ];

    /// The provider's layer identifier.
    pub const fn layer(self) -> &'static str {
        match self {
            Self::TrueColor => "MODIS_Terra_CorrectedReflectance_TrueColor",
            Self::CloudTopTemp => "MODIS_Terra_Cloud_Top_Temp_Day",
            Self::Aerosol => "MODIS_Terra_Aerosol",
            Self::Precipitation => "IMERG_Precipitation_Rate",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::TrueColor => "True color",
            Self::CloudTopTemp => "Cloud-top temperature",
            Self::Aerosol => "Aerosol optical depth",
            Self::Precipitation => "Precipitation rate",
        }
    }

    /// What the picture is, in the provider's terms.
    pub const fn description(self) -> &'static str {
        match self {
            Self::TrueColor => {
                "MODIS Terra corrected reflectance: the day's swaths as a natural-color composite."
            }
            Self::CloudTopTemp => {
                "MODIS Terra daytime cloud-top temperature; cold tops mark deep convection."
            }
            Self::Aerosol => "MODIS Terra aerosol optical depth where the retrieval is valid.",
            Self::Precipitation => {
                "GPM IMERG precipitation rate, a merged satellite and gauge estimate."
            }
        }
    }

    /// The provider's tile set, which also caps the usable zoom.
    pub const fn level(self) -> u8 {
        match self {
            Self::TrueColor => 9,
            Self::CloudTopTemp | Self::Aerosol | Self::Precipitation => 6,
        }
    }

    /// The format as the provider's KVP interface expects it, with the slash
    /// escaped so the request stays a single query parameter.
    pub const fn format(self) -> &'static str {
        match self {
            Self::TrueColor => "image%2Fjpeg",
            Self::CloudTopTemp | Self::Aerosol | Self::Precipitation => "image%2Fpng",
        }
    }

    pub const fn content_type(self) -> &'static str {
        match self {
            Self::TrueColor => "image/jpeg",
            Self::CloudTopTemp | Self::Aerosol | Self::Precipitation => "image/png",
        }
    }

    /// What the colours mean, so the layer is never read as a calibrated value
    /// without its scale.
    pub const fn scale_hint(self) -> &'static str {
        match self {
            Self::TrueColor => "Natural color; no measurement scale applies.",
            Self::CloudTopTemp => "The provider's own cloud-top temperature colour map.",
            Self::Aerosol => "Transparent where the retrieval has no valid value.",
            Self::Precipitation => "Transparent where the estimate is zero.",
        }
    }

    pub const fn is_measurement(self) -> bool {
        !matches!(self, Self::TrueColor)
    }
}

#[derive(Debug)]
pub enum SatelliteError {
    Provider {
        source: JsonError,
    },
    Image {
        url: String,
        source: image::ImageError,
    },
    Shape {
        detail: &'static str,
    },
    WorkerStopped,
}

impl Display for SatelliteError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => write!(formatter, "{PROVIDER}: {source}"),
            Self::Image { url, source } => write!(
                formatter,
                "The satellite tile {url} is not a readable image: {source}"
            ),
            Self::Shape { detail } => write!(formatter, "{PROVIDER} request needs {detail}"),
            Self::WorkerStopped => formatter.write_str("The satellite worker stopped."),
        }
    }
}

impl Error for SatelliteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Image { source, .. } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl SatelliteError {
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Image { .. } | Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

/// The UTC day a composite is made of, as the provider wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CalendarDay {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl CalendarDay {
    pub fn path(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// The day `offset` days before a clock reading. A date is never guessed
    /// from a clock that cannot be read.
    pub fn today(now_ms: u64, offset: u8) -> Result<Self, SatelliteError> {
        let days = i64::try_from(now_ms / time::MS_PER_DAY).map_err(|_| SatelliteError::Shape {
            detail: "a day count that fits a signed integer",
        })?;
        let shifted = days
            .checked_sub(i64::from(offset))
            .ok_or(SatelliteError::Shape {
                detail: "a day count inside the calendar",
            })?;
        let (year, month, day) = time::civil_from_days(shifted);
        Ok(Self { year, month, day })
    }
}

/// The day the layer shows, or a sentence saying why it cannot.
fn day_for(offset: u8) -> Result<CalendarDay, String> {
    let now = match time::now_unix_ms() {
        Ok(now) => now,
        Err(error) => return Err(format!("the clock cannot be read: {error}")),
    };
    match CalendarDay::today(now, offset) {
        Ok(day) => Ok(day),
        Err(error) => Err(format!("{error}")),
    }
}

/// The tiles a view needs, and the zoom they came from.
#[derive(Debug, Clone, PartialEq)]
pub struct TilePlan {
    pub zoom: u8,
    pub tiles: Vec<TileBounds>,
}

/// # Errors
///
/// Returns [`SatelliteError::Shape`] for a view outside Web Mercator or one
/// needing more tiles than the layer will request at once.
pub fn plan_view(
    imagery: Imagery,
    center_lat: f64,
    center_lon: f64,
    span_km: f64,
) -> Result<TilePlan, SatelliteError> {
    let grid =
        global_radar::tile_grid_for_view_capped(center_lat, center_lon, span_km, imagery.level())
            .map_err(|_| SatelliteError::Shape {
            detail: "a map view inside Web Mercator limits",
        })?;
    if grid.tiles.is_empty() || grid.tiles.len() > VIEW_TILE_LIMIT {
        return Err(SatelliteError::Shape {
            detail: "a viewport of at most nine tiles",
        });
    }
    Ok(TilePlan {
        zoom: grid.zoom,
        tiles: grid.tiles,
    })
}

pub fn tile_url(imagery: Imagery, day: CalendarDay, tile: TileBounds) -> String {
    // The provider's RESTful path answers a zeroed placeholder for these
    // layers, so the request uses the same interface the capabilities document
    // advertises for real tiles: KVP.
    format!(
        concat!(
            "{}/wmts.cgi?SERVICE=WMTS&REQUEST=GetTile&VERSION=1.0.0",
            "&LAYER={}&STYLE=default&TILEMATRIXSET=GoogleMapsCompatible_Level{}",
            "&TILEMATRIX={}&TILEROW={}&TILECOL={}&FORMAT={}&TIME={}",
        ),
        GIBS,
        imagery.layer(),
        imagery.level(),
        tile.zoom,
        tile.y,
        tile.x,
        imagery.format(),
        day.path()
    )
}

/// A decoded tile with the geographic bounds it covers.
#[derive(Debug)]
pub struct TileImage {
    pub bounds: TileBounds,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub struct PlanRequest {
    generation: u64,
    imagery: Imagery,
    day: CalendarDay,
    tiles: Vec<TileBounds>,
}

#[derive(Debug)]
pub enum SatelliteEvent {
    Tile {
        generation: u64,
        tile: TileImage,
    },
    Done {
        generation: u64,
        loaded: usize,
        failed: usize,
    },
    Failed {
        generation: u64,
        error: SatelliteError,
    },
}

fn decode(url: &str, bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), SatelliteError> {
    let image = image::load_from_memory(bytes).map_err(|source| SatelliteError::Image {
        url: String::from(url),
        source,
    })?;
    let rgba = image.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Fetches and decodes tiles off the UI thread, keeping a bounded copy of what
/// it has already seen.
pub struct SatelliteFeed {
    requests: Sender<PlanRequest>,
    events: Receiver<SatelliteEvent>,
    generation: u64,
    stopped: bool,
}

impl SatelliteFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<PlanRequest>();
        let (event_sender, events) = mpsc::channel::<SatelliteEvent>();
        std::thread::spawn(move || {
            let mut cache: HashMap<String, (u32, u32, Vec<u8>)> = HashMap::new();
            while let Ok(request) = request_receiver.recv() {
                let mut loaded = 0;
                let mut failed = 0;
                let mut first_error = None;
                for tile in &request.tiles {
                    let url = tile_url(request.imagery, request.day, *tile);
                    let decoded = match cache.get(&url) {
                        Some(cached) => Ok(cached.clone()),
                        None => provider::get_bytes(
                            &url,
                            request.imagery.content_type(),
                            MAX_TILE_BYTES,
                        )
                        .map_err(|source| SatelliteError::Provider { source })
                        .and_then(|bytes| {
                            let decoded = decode(&url, &bytes)?;
                            if cache.len() >= CACHED_TILES {
                                cache.clear();
                            }
                            cache.insert(url.clone(), decoded.clone());
                            Ok(decoded)
                        }),
                    };
                    match decoded {
                        Ok((width, height, rgba)) => {
                            loaded += 1;
                            let image = TileImage {
                                bounds: *tile,
                                width,
                                height,
                                rgba,
                            };
                            if event_sender
                                .send(SatelliteEvent::Tile {
                                    generation: request.generation,
                                    tile: image,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(error) => {
                            failed += 1;
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                        }
                    }
                }
                if loaded == 0
                    && let Some(error) = first_error
                {
                    if event_sender
                        .send(SatelliteEvent::Failed {
                            generation: request.generation,
                            error,
                        })
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                if event_sender
                    .send(SatelliteEvent::Done {
                        generation: request.generation,
                        loaded,
                        failed,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            requests,
            events,
            generation: 0,
            stopped: false,
        }
    }

    pub fn request(&mut self, request: PlanRequest) -> Result<(), SatelliteError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(SatelliteError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<SatelliteEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(SatelliteEvent::Failed {
                    generation: self.generation,
                    error: SatelliteError::WorkerStopped,
                })
            }
        }
    }
}

struct PlacedTile {
    bounds: TileBounds,
    texture: TextureHandle,
}

/// What the layer is showing, for the status line.
#[derive(Debug, Clone, PartialEq)]
enum Status {
    Idle,
    Loading {
        loaded: usize,
        of: usize,
    },
    Ready {
        loaded: usize,
        failed: usize,
        zoom: u8,
    },
    Failed {
        message: String,
        retrying: bool,
    },
}

/// How long to wait before asking a provider that failed again.
///
/// Three seconds, then six, twelve, twenty-four, and a minute from there, so a
/// provider that is briefly down does not receive a request every frame.
fn retry_delay(attempt: u32) -> Duration {
    let steps = attempt.min(RETRY_STEPS);
    let factor = 1u32 << steps;
    RETRY_BASE.saturating_mul(factor).min(RETRY_MAX)
}

/// The satellite imagery layer.
pub struct SatelliteLayer {
    feed: SatelliteFeed,
    shown: bool,
    imagery: Imagery,
    day_offset: u8,
    opacity: u8,
    generation: u64,
    planned: Option<String>,
    tiles: Vec<PlacedTile>,
    status: Status,
    retry_due: Option<Instant>,
    failures: u32,
}

impl SatelliteLayer {
    pub fn new() -> Self {
        Self {
            feed: SatelliteFeed::new(),
            shown: false,
            imagery: Imagery::TrueColor,
            day_offset: 0,
            opacity: 235,
            generation: 0,
            planned: None,
            tiles: Vec::new(),
            status: Status::Idle,
            retry_due: None,
            failures: 0,
        }
    }

    /// Requests the tiles the view needs when the product, day, or tile grid
    /// changes, and uploads whatever the worker has finished.
    pub fn update(&mut self, context: &egui::Context, view: &map::MapView) {
        if !self.shown {
            return;
        }
        self.absorb(context);
        if let Some(due) = self.retry_due
            && Instant::now() >= due
        {
            self.retry_due = None;
            self.planned = None;
        }
        let Ok((lat, lon)) = view.center_latlon() else {
            return;
        };
        let plan = match plan_view(self.imagery, lat, lon, view.span_km()) {
            Ok(plan) => plan,
            Err(error) => {
                self.record_failure(format!("{error}"), false);
                return;
            }
        };
        let day = match day_for(self.day_offset) {
            Ok(day) => day,
            Err(message) => {
                self.record_failure(message, false);
                return;
            }
        };
        let key = format!(
            "{}:{}:{}:{}",
            self.imagery.layer(),
            day.path(),
            plan.zoom,
            plan.tiles
                .iter()
                .map(|tile| format!("{}-{}", tile.x, tile.y))
                .collect::<Vec<String>>()
                .join(",")
        );
        if self.planned.as_deref() == Some(key.as_str()) {
            return;
        }
        self.planned = Some(key);
        self.generation = self.generation.wrapping_add(1);
        self.tiles.clear();
        self.status = Status::Loading {
            loaded: 0,
            of: plan.tiles.len(),
        };
        let request = PlanRequest {
            generation: self.generation,
            imagery: self.imagery,
            day,
            tiles: plan.tiles,
        };
        if let Err(error) = self.feed.request(request) {
            let transient = error.is_transient();
            self.record_failure(format!("{error}"), transient);
        }
    }

    /// Records a failure. A provider that may recover is asked again after a
    /// backoff; a malformed answer or a view this layer cannot serve is not,
    /// because asking again would receive the same answer.
    fn record_failure(&mut self, message: String, transient: bool) {
        if transient {
            self.failures = self.failures.saturating_add(1);
            self.retry_due = Some(Instant::now() + retry_delay(self.failures));
            self.status = Status::Failed {
                message,
                retrying: true,
            };
            return;
        }
        self.failures = 0;
        self.retry_due = None;
        self.status = Status::Failed {
            message,
            retrying: false,
        };
    }

    /// Turns the worker's answers into textures and status.
    fn absorb(&mut self, context: &egui::Context) {
        while let Some(event) = self.feed.poll() {
            match event {
                SatelliteEvent::Tile { generation, tile } => {
                    if generation != self.generation {
                        continue;
                    }
                    if tile.rgba.len() != tile.width as usize * tile.height as usize * 4 {
                        self.record_failure(
                            format!(
                                "A tile arrived with {} bytes for {} by {} pixels",
                                tile.rgba.len(),
                                tile.width,
                                tile.height
                            ),
                            false,
                        );
                        continue;
                    }
                    let key = key_for(&tile.bounds);
                    self.tiles.retain(|placed| key_for(&placed.bounds) != key);
                    let texture = context.load_texture(
                        format!("satellite-{generation}-{key}"),
                        egui::ColorImage::from_rgba_unmultiplied(
                            [tile.width as usize, tile.height as usize],
                            &tile.rgba,
                        ),
                        egui::TextureOptions::LINEAR,
                    );
                    self.tiles.push(PlacedTile {
                        bounds: tile.bounds,
                        texture,
                    });
                    if let Status::Loading { of, .. } = self.status {
                        self.status = Status::Loading {
                            loaded: self.tiles.len(),
                            of,
                        };
                    }
                }
                SatelliteEvent::Done {
                    generation,
                    loaded,
                    failed,
                } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.status = Status::Ready {
                        loaded,
                        failed,
                        zoom: self.tiles.first().map_or(0, |tile| tile.bounds.zoom),
                    };
                    if failed == 0 {
                        self.failures = 0;
                        self.retry_due = None;
                    }
                }
                SatelliteEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let transient = error.is_transient();
                    self.record_failure(format!("{error}"), transient);
                }
            }
        }
    }

    /// Draws the imagery under the vector geography, inside the map canvas.
    ///
    /// The clip stays inside the canvas because a tile at a wide zoom reaches
    /// past it, and imagery painted over the history strip hides the frame the
    /// user is choosing.
    pub fn draw(&self, ui: &egui::Ui, transform: &map::MapTransform, clip: Rect) {
        if !self.shown {
            return;
        }
        let painter = ui.painter().with_clip_rect(clip.intersect(ui.clip_rect()));
        for tile in &self.tiles {
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
            painter.image(
                tile.texture.id(),
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::from_white_alpha(self.opacity),
            );
        }
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Satellite imagery")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!("{PROVIDER} · global, no key required"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Satellite: on"
        } else {
            "Satellite: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "Daily NASA imagery over the map. A day is the provider's composite of that UTC day, not one pass.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            self.planned = None;
        }
        let mut chosen = self.imagery;
        egui::ComboBox::from_id_salt("satellite-imagery")
            .selected_text(self.imagery.label())
            .width(210.0)
            .show_ui(ui, |ui| {
                for candidate in Imagery::ALL {
                    ui.selectable_value(&mut chosen, candidate, candidate.label())
                        .on_hover_text(candidate.description());
                }
            });
        if chosen != self.imagery {
            self.imagery = chosen;
            self.planned = None;
        }
        ui.label(
            RichText::new(self.imagery.description())
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        ui.label(
            RichText::new(self.imagery.scale_hint())
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let mut offset = self.day_offset;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Day").size(theme::TYPE_SMALL).weak());
            ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(40.0);
            if ui
                .add(egui::Slider::new(&mut offset, 0..=DAYS_BACK).show_value(false))
                .changed()
            {
                self.day_offset = offset;
                self.planned = None;
            }
            let text = match day_for(self.day_offset) {
                Ok(day) => day.path(),
                Err(message) => format!("unavailable: {message}"),
            };
            ui.label(RichText::new(text).size(theme::TYPE_SMALL).strong());
        });
        ui.label(
            RichText::new("Zero is today; the slider walks back through the archive.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        self.rail_report(ui);
    }

    /// Opacity, what the provider answered, and what the picture is.
    fn rail_report(&mut self, ui: &mut egui::Ui) {
        let mut opacity = self.opacity;
        ui.spacing_mut().slider_width = ui.available_width() - theme::SPACE_M;
        if ui
            .add(egui::Slider::new(&mut opacity, 40..=255).show_value(false))
            .changed()
        {
            self.opacity = opacity;
        }
        ui.label(
            RichText::new(format!("Opacity {:.0}%", f64::from(self.opacity) / 2.55))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        match &self.status {
            Status::Idle => {
                ui.label(
                    RichText::new("Not requested yet.")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            Status::Loading { loaded, of } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Loading {loaded} of {of} tiles…"))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            Status::Ready {
                loaded,
                failed,
                zoom,
            } => {
                let mut text = format!("{loaded} tiles at zoom {zoom}");
                if *failed > 0 {
                    text = format!("{text} · {failed} unavailable from the provider");
                }
                ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
            }
            Status::Failed { message, retrying } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Satellite imagery failed: {message}"),
                );
                if *retrying {
                    ui.label(
                        RichText::new(format!(
                            "Asking the provider again in about {} seconds.",
                            retry_delay(self.failures).as_secs()
                        ))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                    );
                }
            }
        }
        if self.shown && !self.tiles.is_empty() {
            let kind = if self.imagery.is_measurement() {
                "A retrieved measurement"
            } else {
                "A natural-colour composite, not a measurement"
            };
            ui.label(
                RichText::new(format!("{kind} · {PROVIDER}"))
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }
}

fn key_for(bounds: &TileBounds) -> String {
    format!("{}-{}-{}", bounds.zoom, bounds.x, bounds.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(x: u32, y: u32) -> TileBounds {
        TileBounds {
            zoom: 6,
            x,
            y,
            west: -180.0,
            south: -80.0,
            east: -170.0,
            north: -70.0,
        }
    }

    #[test]
    fn each_product_names_its_layer_tile_set_and_format() {
        assert_eq!(
            Imagery::TrueColor.layer(),
            "MODIS_Terra_CorrectedReflectance_TrueColor"
        );
        assert_eq!(Imagery::TrueColor.level(), 9);
        assert_eq!(Imagery::CloudTopTemp.level(), 6);
        assert_eq!(Imagery::TrueColor.format(), "image%2Fjpeg");
        assert_eq!(Imagery::Aerosol.format(), "image%2Fpng");
        assert!(Imagery::CloudTopTemp.is_measurement());
        assert!(!Imagery::TrueColor.is_measurement());
    }

    #[test]
    fn a_tile_url_carries_the_layer_day_level_and_tile() {
        let day = CalendarDay {
            year: 2026,
            month: 11,
            day: 1,
        };
        let url = tile_url(Imagery::TrueColor, day, tile(3, 6));
        assert!(url.starts_with(
            "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/wmts.cgi?SERVICE=WMTS&REQUEST=GetTile"
        ));
        assert!(
            url.contains("LAYER=MODIS_Terra_CorrectedReflectance_TrueColor"),
            "{url}"
        );
        assert!(url.contains("STYLE=default"), "{url}");
        assert!(
            url.contains("TILEMATRIXSET=GoogleMapsCompatible_Level9"),
            "{url}"
        );
        assert!(url.contains("TILEMATRIX=6&TILEROW=6&TILECOL=3"), "{url}");
        assert!(url.contains("FORMAT=image%2Fjpeg"), "{url}");
        assert!(url.contains("TIME=2026-11-01"), "{url}");
        let ir = tile_url(Imagery::CloudTopTemp, day, tile(3, 6));
        assert!(ir.contains("GoogleMapsCompatible_Level6"), "{ir}");
        assert!(ir.contains("FORMAT=image%2Fpng"), "{ir}");
    }

    #[test]
    fn days_are_counted_back_from_the_clock_in_utc() -> Result<(), SatelliteError> {
        let now_ms = 1_793_557_411_000;
        assert_eq!(CalendarDay::today(now_ms, 0)?.path(), "2026-11-01");
        assert_eq!(CalendarDay::today(now_ms, 1)?.path(), "2026-10-31");
        assert_eq!(CalendarDay::today(now_ms, 6)?.path(), "2026-10-26");
        Ok(())
    }

    #[test]
    fn a_plan_stays_inside_the_products_tile_level() -> Result<(), SatelliteError> {
        let plan = plan_view(Imagery::CloudTopTemp, 39.0, -96.2, 900.0)?;
        assert!(plan.zoom <= 6, "level six is the deepest this layer offers");
        assert!(!plan.tiles.is_empty());
        assert!(plan.tiles.len() <= VIEW_TILE_LIMIT);
        let wide = plan_view(Imagery::TrueColor, 0.0, 0.0, 18_000.0)?;
        assert!(wide.tiles.len() <= VIEW_TILE_LIMIT);
        assert!(wide.zoom <= 9);
        Ok(())
    }

    #[test]
    fn a_provider_failure_backs_off_to_a_minute() {
        assert_eq!(retry_delay(1), Duration::from_secs(6));
        assert_eq!(retry_delay(2), Duration::from_secs(12));
        assert_eq!(retry_delay(3), Duration::from_secs(24));
        assert_eq!(retry_delay(4), Duration::from_secs(48));
        assert_eq!(retry_delay(5), RETRY_MAX);
        assert_eq!(retry_delay(9), RETRY_MAX);
    }

    #[test]
    fn an_impossible_view_is_refused() {
        assert!(matches!(
            plan_view(Imagery::TrueColor, 95.0, 0.0, 900.0),
            Err(SatelliteError::Shape { .. })
        ));
    }

    #[test]
    fn a_shape_failure_is_not_retried_but_a_provider_failure_is() {
        let mut layer = SatelliteLayer::new();
        layer.shown = true;
        layer.record_failure(String::from("no tiles"), false);
        assert!(layer.retry_due.is_none());
        assert_eq!(layer.failures, 0);
        layer.record_failure(String::from("timed out"), true);
        assert!(layer.retry_due.is_some());
        assert_eq!(layer.failures, 1);
        assert!(matches!(
            layer.status,
            Status::Failed { retrying: true, .. }
        ));
    }

    #[test]
    fn a_new_layer_is_off_and_idle() {
        let layer = SatelliteLayer::new();
        assert!(!layer.shown);
        assert_eq!(layer.status, Status::Idle);
        assert!(layer.tiles.is_empty());
    }
}
