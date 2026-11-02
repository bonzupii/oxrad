// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenFreeMap vector tiles and map styles acquired at runtime.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Align2, Color32, FontId, Rect};
use walkers::sources::{Attribution, TileSource};
use walkers::{HttpOptions, HttpTiles, Map, MapMemory, MaxParallelDownloads, Style, TileId};

use crate::cache::Store;
use crate::map::MapView;
use crate::paths;
use crate::provider;

const WORLD_KM: f64 = 40_075.016_685_578_49;
const CREDIT: &str = "OpenFreeMap · © OpenMapTiles · © OpenStreetMap contributors";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MapStyle {
    Bright,
    Positron,
    #[default]
    Liberty,
    Dark,
    Fiord,
}

impl MapStyle {
    const ALL: [Self; 5] = [
        Self::Bright,
        Self::Positron,
        Self::Liberty,
        Self::Dark,
        Self::Fiord,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Bright => "Bright",
            Self::Positron => "Positron",
            Self::Liberty => "Liberty",
            Self::Dark => "Dark",
            Self::Fiord => "Fiord",
        }
    }

    pub const fn token(self) -> &'static str {
        match self {
            Self::Bright => "bright",
            Self::Positron => "positron",
            Self::Liberty => "liberty",
            Self::Dark => "dark",
            Self::Fiord => "fiord",
        }
    }
    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.token() == token)
    }
}

struct OpenFreeMap;

impl TileSource for OpenFreeMap {
    fn tile_url(&self, tile: TileId) -> String {
        format!(
            "https://tiles.openfreemap.org/planet/latest/{}/{}/{}.pbf",
            tile.zoom, tile.x, tile.y
        )
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            text: CREDIT,
            url: "https://www.openstreetmap.org/copyright",
            logo_light: None,
            logo_dark: None,
        }
    }

    fn tile_size(&self) -> u32 {
        512
    }
    fn max_zoom(&self) -> u8 {
        14
    }
}

enum StyleResult {
    Cached {
        style: MapStyle,
        document: serde_json::Value,
    },
    Refreshed {
        style: MapStyle,
        document: serde_json::Value,
        cache_error: Option<String>,
    },
    Failed {
        style: MapStyle,
        error: String,
    },
}

fn load_style(style: MapStyle, sender: &mpsc::Sender<StyleResult>) -> bool {
    let url = format!("https://tiles.openfreemap.org/styles/{}", style.token());
    let cache = paths::cache_directory().map(|path| Store::at(path.join("basemap").join("styles")));
    if let Ok(store) = &cache
        && let Ok(Some(entry)) = store.load(&url)
        && let Ok(document) = serde_json::from_slice::<serde_json::Value>(&entry.body)
        && serde_json::from_value::<Style>(document.clone()).is_ok()
        && sender
            .send(StyleResult::Cached { style, document })
            .is_err()
    {
        return false;
    }
    let result = match provider::get_json(&url, "application/json") {
        Ok(document) if serde_json::from_value::<Style>(document.clone()).is_err() => {
            StyleResult::Failed {
                style,
                error: String::from("Provider returned an unsupported or invalid style document."),
            }
        }
        Ok(document) => {
            let cache_error = match &cache {
                Ok(store) => match serde_json::to_vec(&document) {
                    Ok(bytes) => store
                        .store(&url, &bytes)
                        .err()
                        .map(|error| error.to_string()),
                    Err(error) => Some(error.to_string()),
                },
                Err(error) => Some(error.to_string()),
            };
            StyleResult::Refreshed {
                style,
                document,
                cache_error,
            }
        }
        Err(error) => StyleResult::Failed {
            style,
            error: error.to_string(),
        },
    };
    sender.send(result).is_ok()
}

/// Owns style acquisition, tile requests, and the persistent HTTP cache.
pub struct Basemap {
    selected: MapStyle,
    active: Option<MapStyle>,
    receiver: Option<Receiver<StyleResult>>,
    cached_style: bool,
    tiles: Option<HttpTiles>,
    memory: MapMemory,
    error: Option<String>,
}

impl Basemap {
    pub fn new(selected: MapStyle) -> Self {
        Self {
            selected,
            active: None,
            receiver: None,
            cached_style: false,
            tiles: None,
            memory: MapMemory::default(),
            error: None,
        }
    }

    pub fn controls(&mut self, ui: &mut egui::Ui) -> Option<MapStyle> {
        let previous = self.selected;
        egui::ComboBox::from_id_salt("basemap-style")
            .selected_text(self.selected.name())
            .show_ui(ui, |ui| {
                for style in MapStyle::ALL {
                    ui.selectable_value(&mut self.selected, style, style.name());
                }
            });
        if self.selected != previous {
            self.receiver = None;
            self.error = None;
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            if ui.button("Retry basemap").clicked() {
                self.error = None;
            }
        }
        ui.hyperlink_to(CREDIT, "https://www.openstreetmap.org/copyright");
        (self.selected != previous).then_some(self.selected)
    }

    fn request_style(&mut self) {
        let style = self.selected;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || load_style(style, &sender));
        self.receiver = Some(receiver);
    }

    fn install_style(
        &mut self,
        context: &egui::Context,
        selected: MapStyle,
        document: serde_json::Value,
    ) {
        let style = match serde_json::from_value::<Style>(document) {
            Ok(style) => style,
            Err(error) => {
                self.error = Some(format!("Basemap style could not be parsed: {error}"));
                return;
            }
        };
        let path = match paths::cache_directory() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("Basemap cache unavailable: {error}"));
                return;
            }
        };
        let options = HttpOptions {
            cache: Some(path.join("basemap").join("openfreemap")),
            user_agent: Some(walkers::HeaderValue::from_static(concat!(
                "oxrad/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/bonzupii/oxrad)"
            ))),
            max_parallel_downloads:
                MaxParallelDownloads::value_manually_confirmed_with_provider_limits(4),
        };
        self.tiles = Some(HttpTiles::with_options_and_style(
            OpenFreeMap,
            options,
            style,
            context.clone(),
        ));
        self.active = Some(selected);
    }

    fn poll(&mut self, context: &egui::Context) {
        if (self.active != Some(self.selected) || self.cached_style)
            && self.receiver.is_none()
            && self.error.is_none()
        {
            self.request_style();
        }
        let Some(receiver) = &self.receiver else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                context.request_repaint_after(std::time::Duration::from_millis(250));
                return;
            }
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                self.error = Some(String::from("Basemap style worker stopped."));
                return;
            }
        };
        match result {
            StyleResult::Cached { style, document } => {
                self.install_style(context, style, document);
                self.cached_style = true;
                context.request_repaint_after(std::time::Duration::from_millis(250));
            }
            StyleResult::Refreshed {
                style,
                document,
                cache_error,
            } => {
                self.receiver = None;
                self.install_style(context, style, document);
                self.cached_style = false;
                if let Some(error) = cache_error {
                    self.error = Some(format!("Basemap style cache unavailable: {error}"));
                }
            }
            StyleResult::Failed { style, error } => {
                self.receiver = None;
                self.error = Some(format!(
                    "{} basemap style refresh failed: {error}",
                    style.name()
                ));
            }
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, rect: Rect, view: &MapView) {
        self.poll(ui.ctx());
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_rgb(34, 43, 51));
        let Some(tiles) = self.tiles.as_mut() else {
            let message = self
                .error
                .as_deref()
                .unwrap_or("Loading OpenFreeMap style and tiles…");
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                message,
                FontId::proportional(14.0),
                Color32::WHITE,
            );
            return;
        };
        let (latitude, longitude) = match view.center_latlon() {
            Ok(centre) => centre,
            Err(error) => {
                self.error = Some(format!("Basemap position unavailable: {error}"));
                return;
            }
        };
        let zoom = (WORLD_KM * f64::from(rect.width()) / (view.span_km() * 256.0))
            .log2()
            .clamp(1.0, 26.0);
        if let Err(error) = self.memory.set_zoom(zoom) {
            self.error = Some(format!("Basemap zoom unavailable: {error}"));
            return;
        }
        let center = walkers::lat_lon(latitude, longitude);
        self.memory.center_at(center);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            Map::new(Some(tiles), &mut self.memory, center)
                .zoom_gesture(false)
                .panning(false)
                .show(ui, |_, _, _, _| {});
        });
        let status = if self.error.is_some() && self.cached_style {
            " · cached style; refresh failed"
        } else if self.cached_style {
            " · cached style; refreshing"
        } else if self.active != Some(self.selected) {
            " · updating style"
        } else {
            ""
        };
        let attribution = format!("{CREDIT}{status}");
        let galley =
            ui.painter()
                .layout_no_wrap(attribution, FontId::proportional(10.0), Color32::WHITE);
        let attribution_rect = Rect::from_min_size(
            rect.right_bottom() - egui::vec2(galley.size().x + 16.0, galley.size().y + 8.0),
            galley.size() + egui::vec2(8.0, 4.0),
        );
        ui.painter()
            .rect_filled(attribution_rect, 2.0, Color32::from_black_alpha(220));
        ui.painter().text(
            rect.right_bottom() - egui::vec2(12.0, 6.0),
            Align2::RIGHT_BOTTOM,
            galley.text(),
            FontId::proportional(10.0),
            Color32::WHITE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openfreemap_tile_urls_preserve_xyz_coordinates() {
        let tile = TileId {
            zoom: 4,
            x: 3,
            y: 7,
        };
        assert_eq!(
            OpenFreeMap.tile_url(tile),
            "https://tiles.openfreemap.org/planet/latest/4/3/7.pbf"
        );
        assert_eq!(OpenFreeMap.max_zoom(), 14);
        assert!(OpenFreeMap.attribution().text.contains("OpenStreetMap"));
    }

    #[test]
    fn all_requested_styles_have_distinct_provider_paths() {
        let names: std::collections::HashSet<_> =
            MapStyle::ALL.into_iter().map(MapStyle::token).collect();
        assert_eq!(names.len(), 5);
    }
}
