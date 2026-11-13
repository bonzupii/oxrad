// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenFreeMap vector tiles and map styles acquired at runtime.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Align2, Color32, FontId, Rect};
use walkers::sources::{Attribution, TileSource};
use walkers::{Map, MapMemory, Style, TileId, TilePiece, Tiles};

use crate::acquire::Acquire;
use crate::basemap_tiles::BasemapTiles;
use crate::canvas::provenance;
use crate::engine::Served;
use crate::map::MapView;
use crate::paths;
use crate::sources;

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
        sources::basemap_tile_url(tile.zoom, tile.x, tile.y)
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            text: CREDIT,
            url: sources::openfreemap_attribution_url(),
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

struct VisibleTiles<'a> {
    source: &'a mut BasemapTiles<OpenFreeMap>,
    missing: usize,
    error: Option<String>,
}

impl Tiles for VisibleTiles<'_> {
    fn at(&mut self, tile: TileId) -> Option<TilePiece> {
        let piece = self.source.at(tile);
        if piece.is_none() {
            self.missing = self.missing.saturating_add(1);
            if self.error.is_none() {
                self.error = self.source.error(tile).map(|error| error.to_string());
            }
        }
        piece
    }

    fn attribution(&self) -> Attribution {
        self.source.attribution()
    }

    fn tile_size(&self) -> u32 {
        self.source.tile_size()
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "the served value travels whole so its provenance cannot be separated from it"
)]
enum StyleResult {
    Refreshed {
        style: MapStyle,
        document: Served<serde_json::Value>,
    },
    Failed {
        style: MapStyle,
        error: String,
    },
}

fn load_style(style: MapStyle, sender: &mpsc::Sender<StyleResult>, acquire: &Acquire) -> bool {
    let result = match acquire.fetch_operation(
        &sources::OPENFREEMAP,
        "style",
        &[("style", style.token())],
    ) {
        Ok(fetched) => match fetched.json() {
            Ok(document) if serde_json::from_value::<Style>(document.clone()).is_err() => {
                StyleResult::Failed {
                    style,
                    error: String::from(
                        "Provider returned an unsupported or invalid style document.",
                    ),
                }
            }
            Ok(document) => StyleResult::Refreshed {
                style,
                document: Served::new(document, fetched.provenance, fetched.cache),
            },
            Err(error) => StyleResult::Failed {
                style,
                error: error.to_string(),
            },
        },
        Err(error) => StyleResult::Failed {
            style,
            error: error.to_string(),
        },
    };
    sender.send(result).is_ok()
}

pub struct Basemap {
    selected: MapStyle,
    acquire: Acquire,
    active: Option<MapStyle>,
    active_document: Option<Served<serde_json::Value>>,
    receiver: Option<Receiver<StyleResult>>,
    tiles: Option<BasemapTiles<OpenFreeMap>>,
    missing_tiles: usize,
    tile_error: Option<String>,
    tile_warning: Option<String>,
    memory: MapMemory,
    error: Option<String>,
}

impl Basemap {
    pub fn new(selected: MapStyle, acquire: Acquire) -> Self {
        Self {
            selected,
            acquire,
            active: None,
            active_document: None,
            receiver: None,
            tiles: None,
            missing_tiles: 0,
            tile_error: None,
            tile_warning: None,
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
        if let Some(error) = &self.tile_error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Map tile unavailable: {error}"),
            );
            if ui.button("Retry map tiles").clicked() {
                if let Some(tiles) = &mut self.tiles {
                    tiles.retry_failed();
                }
                self.tile_error = None;
            }
        }
        if let Some(warning) = &self.tile_warning {
            ui.label(format!("Map tile cache: {warning}"));
        } else if self.missing_tiles > 0 {
            ui.label(format!(
                "{} visible map tiles are loading.",
                self.missing_tiles
            ));
        }
        ui.hyperlink_to(CREDIT, sources::openfreemap_attribution_url());
        if let Some(document) = &self.active_document {
            provenance(ui, document);
        }
        (self.selected != previous).then_some(self.selected)
    }

    fn request_style(&mut self) {
        let style = self.selected;
        let (sender, receiver) = mpsc::channel();
        let acquire = self.acquire.clone();
        std::thread::spawn(move || load_style(style, &sender, &acquire));
        self.receiver = Some(receiver);
    }

    fn install_style(
        &mut self,
        context: &egui::Context,
        selected: MapStyle,
        document: Served<serde_json::Value>,
    ) {
        if self.tiles.is_some()
            && self.active == Some(selected)
            && self
                .active_document
                .as_ref()
                .is_some_and(|active| active.value == document.value)
        {
            return;
        }
        let style = match serde_json::from_value::<Style>(document.value.clone()) {
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
        if let Some(tiles) = &mut self.tiles {
            tiles.set_style(style);
        } else {
            self.tiles = Some(BasemapTiles::new(
                OpenFreeMap,
                style,
                &path.join("basemap").join("openfreemap"),
                context,
                &self.acquire,
            ));
        }
        self.active = Some(selected);
        self.active_document = Some(document);
    }

    fn poll(&mut self, context: &egui::Context) {
        if self.active != Some(self.selected) && self.receiver.is_none() && self.error.is_none() {
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
            StyleResult::Refreshed { style, document } => {
                self.receiver = None;
                self.install_style(context, style, document);
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
        if let Some(tiles) = self.tiles.as_mut()
            && let Some(warning) = tiles.take_warning()
        {
            self.tile_warning = Some(warning.to_string());
        }
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
        let mut visible = VisibleTiles {
            source: tiles,
            missing: 0,
            error: None,
        };
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            Map::new(Some(&mut visible), &mut self.memory, center)
                .zoom_gesture(false)
                .panning(false)
                .show(ui, |_, _, _, _| {});
        });
        self.missing_tiles = visible.missing;
        self.tile_error = visible.error;
        if self.missing_tiles > 0 {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Draw attribution after overlays so it cannot be covered by map data.
    pub fn draw_attribution(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        providers: &crate::config::EnabledProviders,
    ) {
        if self.tiles.is_none() || !providers.enables("openfreemap") {
            return;
        }
        let status = if self.error.is_some() && self.active != Some(self.selected) {
            " · selected style unavailable"
        } else if self.active != Some(self.selected) {
            " · updating style"
        } else {
            ""
        };
        let tile_status = if self.tile_error.is_some() {
            " · map tile unavailable"
        } else if self.missing_tiles > 0 {
            " · map tiles loading"
        } else {
            ""
        };
        let attribution = format!(
            "{}{status}{tile_status}",
            crate::sources::OPENFREEMAP.credit
        );
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
    use crate::test_support::TestError;

    fn synthetic_tile() -> Result<Vec<u8>, TestError> {
        // One synthetic line in an MVT layer named "road": (0, 0) to (10, 10).
        let feature = [0x18, 0x02, 0x22, 0x06, 0x09, 0x00, 0x00, 0x0a, 0x14, 0x14];
        let mut layer = vec![0x0a, 0x04, b'r', b'o', b'a', b'd', 0x12, 0x0a];
        layer.extend_from_slice(&feature);
        layer.extend_from_slice(&[0x28, 0x80, 0x20, 0x78, 0x02]);
        let mut tile = vec![
            0x1a,
            u8::try_from(layer.len()).map_err(|error| error.to_string())?,
        ];
        tile.extend_from_slice(&layer);
        Ok(tile)
    }

    fn synthetic_style() -> Result<Style, TestError> {
        serde_json::from_value(serde_json::json!({
            "layers": [{"type": "line", "source-layer": "road", "paint": {"line-color": "#000000", "line-width": 1}}]
        })).map_err(|error| TestError::from(error.to_string()))
    }

    #[test]
    fn generated_vector_tile_decodes_into_renderable_shapes() -> Result<(), TestError> {
        let tile = synthetic_tile()?;
        let style = synthetic_style()?;
        match walkers::Tile::from_mvt(&tile, &style, 4).map_err(|error| error.to_string())? {
            walkers::Tile::Vector(shapes) => assert!(!shapes.is_empty(), "line was not rendered"),
            walkers::Tile::Raster(_) => {
                return Err(TestError::from("vector tile decoded as raster"));
            }
        }
        Ok(())
    }

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
