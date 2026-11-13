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
//! The global radar composite: tiles for the current map view, playback over
//! the two-hour history, and the composite's own timeline bar.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Layout, Pos2, Rect, RichText};

use crate::acquire::Acquire;
use crate::canvas::due_after;
use crate::config::{CachePolicy, Config};
use crate::global_radar::{
    self, DecodedTile, GlobalEvent, GlobalRadarError, GlobalRadarFeed, TileBounds, TileImage,
};
use crate::map::{self, MapView};
use crate::theme;
use crate::time;

const GLOBAL_REFRESH_SECS: u64 = 5 * 60;
const GLOBAL_RETRY_SECS: u64 = 60;
const GLOBAL_PLAYBACK_MS: u64 = 650;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalPlayback {
    Stopped,
    Playing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalState {
    Idle,
    Loading,
    Ready,
    Failed,
}

struct GlobalTileTexture {
    bounds: TileBounds,
    texture: egui::TextureHandle,
}

struct GlobalFrameTexture {
    time_unix_ms: u64,
    tiles: Vec<GlobalTileTexture>,
}

pub(crate) struct GlobalPanel {
    feed: GlobalRadarFeed,
    generation: u64,
    frames: Vec<GlobalFrameTexture>,
    index: usize,
    playback: GlobalPlayback,
    next_frame: Option<Instant>,
    request_due: Option<Instant>,
    refresh_due: Option<Instant>,
    state: GlobalState,
}

impl GlobalPanel {
    pub(crate) fn new(cache_policy: CachePolicy, acquire: &Acquire) -> Self {
        Self {
            feed: GlobalRadarFeed::new(cache_policy, acquire.clone()),
            generation: 0,
            frames: Vec::new(),
            index: 0,
            playback: GlobalPlayback::Stopped,
            next_frame: None,
            request_due: None,
            refresh_due: None,
            state: GlobalState::Idle,
        }
    }

    pub(crate) const fn is_playing(&self) -> bool {
        matches!(self.playback, GlobalPlayback::Playing)
    }

    pub(crate) const fn is_loading(&self) -> bool {
        matches!(self.state, GlobalState::Loading)
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub(crate) fn request_now(&mut self) {
        self.request_due = Some(Instant::now());
    }

    pub(crate) const fn layer_summary(&self) -> &'static str {
        match self.state {
            GlobalState::Idle => "",
            GlobalState::Loading => " · global loading",
            GlobalState::Ready => " · global RainViewer",
            GlobalState::Failed => " · global error",
        }
    }

    pub(crate) const fn stop(&mut self) {
        self.playback = GlobalPlayback::Stopped;
        self.next_frame = None;
    }

    /// Polls the composite when its layer is visible, then advances playback.
    pub(crate) fn update(
        &mut self,
        context: &egui::Context,
        view: &MapView,
        visible: bool,
        notice: &mut Option<String>,
    ) {
        if visible {
            let now = Instant::now();
            let due = self.request_due.is_some_and(|due| now >= due)
                || self.refresh_due.is_some_and(|due| now >= due)
                || matches!(self.state, GlobalState::Idle);
            if due {
                self.refresh_due = None;
                self.request(view, notice);
            }
        }
        while let Some(event) = self.feed.poll() {
            self.apply_event(context, event, notice);
        }
        self.advance_playback();
    }

    fn request(&mut self, view: &MapView, notice: &mut Option<String>) {
        self.request_due = None;
        let (center_lat, center_lon) = match view.center_latlon() {
            Ok(center) => center,
            Err(error) => {
                *notice = Some(format!("Global radar view unavailable: {error}"));
                return;
            }
        };
        match self.feed.request(center_lat, center_lon, view.span_km()) {
            Ok(generation) => {
                self.generation = generation;
                self.state = GlobalState::Loading;
            }
            Err(error) => {
                *notice = Some(format!("Global radar request failed: {error}"));
                self.state = GlobalState::Failed;
                self.refresh_due = Some(due_after(Duration::from_secs(GLOBAL_RETRY_SECS)));
            }
        }
    }

    fn apply_event(
        &mut self,
        context: &egui::Context,
        event: GlobalEvent,
        notice: &mut Option<String>,
    ) {
        match event {
            GlobalEvent::Manifest { generation } => {
                if generation != self.generation {
                    return;
                }
                self.frames.clear();
                self.index = 0;
                self.playback = GlobalPlayback::Stopped;
                self.next_frame = None;
                self.state = GlobalState::Loading;
                self.refresh_due = Some(due_after(Duration::from_secs(GLOBAL_REFRESH_SECS)));
            }
            GlobalEvent::Frame {
                generation,
                frame,
                tiles,
            } => {
                if generation != self.generation {
                    return;
                }
                self.add_frame(context, frame.time_unix_ms, tiles, notice);
            }
            GlobalEvent::Failed { generation, error } => {
                if generation != self.generation {
                    return;
                }
                *notice = Some(format!("Global radar: {error}"));
                self.state = GlobalState::Failed;
                self.refresh_due = Some(due_after(Duration::from_secs(GLOBAL_RETRY_SECS)));
            }
            GlobalEvent::Notice {
                generation,
                message,
            } => {
                if generation == self.generation {
                    *notice = Some(format!("Global radar cache: {message}"));
                }
            }
        }
    }

    fn add_frame(
        &mut self,
        context: &egui::Context,
        time_unix_ms: u64,
        tiles: Vec<TileImage>,
        notice: &mut Option<String>,
    ) {
        let mut textures = Vec::with_capacity(tiles.len());
        for tile in tiles {
            match global_texture(context, time_unix_ms, tile.bounds, tile.image) {
                Ok(texture) => textures.push(GlobalTileTexture {
                    bounds: tile.bounds,
                    texture,
                }),
                Err(error) => {
                    *notice = Some(format!("Global radar tile could not be shown: {error}"));
                    self.state = GlobalState::Failed;
                    self.refresh_due = Some(due_after(Duration::from_secs(GLOBAL_RETRY_SECS)));
                    return;
                }
            }
        }
        let was_live = self.index.saturating_add(1) >= self.frames.len();
        let position = self
            .frames
            .partition_point(|stored| stored.time_unix_ms < time_unix_ms);
        self.frames.insert(
            position,
            GlobalFrameTexture {
                time_unix_ms,
                tiles: textures,
            },
        );
        if was_live {
            self.index = self.frames.len().saturating_sub(1);
        } else if position <= self.index {
            self.index = self.index.saturating_add(1);
        }
        self.state = GlobalState::Ready;
    }

    fn advance_playback(&mut self) {
        if self.playback != GlobalPlayback::Playing || self.frames.len() < 2 {
            return;
        }
        let now = Instant::now();
        if self.next_frame.is_some_and(|due| now < due) {
            return;
        }
        let count = self.frames.len();
        self.index = self.index.saturating_add(1) % count;
        self.next_frame = now.checked_add(Duration::from_millis(GLOBAL_PLAYBACK_MS));
    }

    /// Draws the composite's tiles over a geographic canvas.
    pub(crate) fn draw(
        &self,
        painter: &egui::Painter,
        transform: &map::MapTransform,
        alpha: u8,
    ) -> bool {
        let Some(frame) = self.frames.get(self.index) else {
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
            painter.image(
                tile.texture.id(),
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::from_white_alpha(alpha),
            );
            drew = true;
        }
        drew
    }

    /// The composite's timeline and playback controls.
    pub(crate) fn history_bar(&mut self, ui: &mut egui::Ui, config: &Config) {
        let last = self.frames.len().saturating_sub(1);
        let index = self.index.min(last);
        let Some(frame) = self.frames.get(index) else {
            return;
        };
        let stamp = time::format_utc(frame.time_unix_ms)
            .unwrap_or_else(|_| String::from("time unavailable"));
        let count = self.frames.len();
        let mut requested = index;
        let mut playback = self.playback;
        let mut scrub = false;
        let palette = config.theme.palette();
        egui::Panel::bottom("global_history")
            .exact_size(64.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(palette.raised)
                    .inner_margin(egui::Margin::symmetric(10, 5)),
            )
            .show_inside(ui, |ui| {
                ui.style_mut().visuals = config.theme.visuals();
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
            self.index = requested;
        }
        if playback != self.playback {
            if playback == GlobalPlayback::Playing {
                self.next_frame = Some(Instant::now());
            } else {
                self.next_frame = None;
            }
        }
        self.playback = playback;
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
