// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenFreeMap vector tiles and map styles acquired at runtime.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Align2, Color32, FontId, Rect};
use walkers::sources::{Attribution, TileSource};
use walkers::{Map, MapMemory, Style, TileId, TilePiece, Tiles};

use crate::basemap_tiles::BasemapTiles;
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

fn cached_style(store: &Store, url: &str) -> Result<Option<serde_json::Value>, String> {
    let Some(entry) = store.load(url).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let document: serde_json::Value =
        serde_json::from_slice(&entry.body).map_err(|error| error.to_string())?;
    serde_json::from_value::<Style>(document.clone()).map_err(|error| error.to_string())?;
    Ok(Some(document))
}

fn load_style(style: MapStyle, sender: &mpsc::Sender<StyleResult>) -> bool {
    let cache = paths::cache_directory()
        .map(|path| Store::at(path.join("basemap").join("styles")))
        .map_err(|error| error.to_string());
    load_style_with(style, sender, &cache, |url| {
        provider::get_json(url, "application/json").map_err(|error| error.to_string())
    })
}

fn load_style_with(
    style: MapStyle,
    sender: &mpsc::Sender<StyleResult>,
    cache: &Result<Store, String>,
    fetch: impl FnOnce(&str) -> Result<serde_json::Value, String>,
) -> bool {
    let url = format!("https://tiles.openfreemap.org/styles/{}", style.token());
    let cache_read_error = match cache {
        Ok(store) => match cached_style(store, &url) {
            Ok(Some(document)) => {
                if sender
                    .send(StyleResult::Cached { style, document })
                    .is_err()
                {
                    return false;
                }
                None
            }
            Ok(None) => None,
            Err(error) => Some(error),
        },
        Err(error) => Some(error.clone()),
    };
    let result = match fetch(&url) {
        Ok(document) if serde_json::from_value::<Style>(document.clone()).is_err() => {
            StyleResult::Failed {
                style,
                error: String::from("Provider returned an unsupported or invalid style document."),
            }
        }
        Ok(document) => {
            let cache_error = match cache {
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
            error: match cache_read_error {
                Some(cache_error) => {
                    format!("{error}; cached style could not be read: {cache_error}")
                }
                None => error,
            },
        },
    };
    sender.send(result).is_ok()
}

/// Owns style acquisition, tile requests, and the persistent HTTP cache.
pub struct Basemap {
    selected: MapStyle,
    active: Option<MapStyle>,
    active_document: Option<serde_json::Value>,
    receiver: Option<Receiver<StyleResult>>,
    cached_style: bool,
    tiles: Option<BasemapTiles<OpenFreeMap>>,
    missing_tiles: usize,
    tile_error: Option<String>,
    memory: MapMemory,
    error: Option<String>,
}

impl Basemap {
    pub fn new(selected: MapStyle) -> Self {
        Self {
            selected,
            active: None,
            active_document: None,
            receiver: None,
            cached_style: false,
            tiles: None,
            missing_tiles: 0,
            tile_error: None,
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
            self.cached_style = false;
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
        } else if self.missing_tiles > 0 {
            ui.label(format!(
                "{} visible map tiles are loading.",
                self.missing_tiles
            ));
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
        if self.tiles.is_some()
            && self.active == Some(selected)
            && self.active_document.as_ref() == Some(&document)
        {
            return;
        }
        let style = match serde_json::from_value::<Style>(document.clone()) {
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
            ));
        }
        self.active = Some(selected);
        self.active_document = Some(document);
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
                self.cached_style = self.active == Some(style) && self.error.is_none();
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
    pub fn draw_attribution(&self, ui: &egui::Ui, rect: Rect) {
        if self.tiles.is_none() {
            return;
        }
        let status = if self.error.is_some() && self.active != Some(self.selected) {
            " · selected style unavailable"
        } else if self.error.is_some() && self.cached_style {
            " · cached style; refresh failed"
        } else if self.cached_style {
            " · cached style; refreshing"
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
        let attribution = format!("{CREDIT}{status}{tile_status}");
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

    fn test_store(name: &str) -> Result<(std::path::PathBuf, Store), TestError> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::path::PathBuf::from("target").join(format!(
            "basemap-test-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok((root.clone(), Store::at(root)))
    }

    #[test]
    fn cached_style_survives_a_failed_refresh() -> Result<(), TestError> {
        let (root, store) = test_store("offline")?;
        let url = "https://tiles.openfreemap.org/styles/liberty";
        let original = serde_json::json!({"layers": []});
        store.store(
            url,
            &serde_json::to_vec(&original).map_err(|error| error.to_string())?,
        )?;
        let (sender, receiver) = mpsc::channel();
        assert!(load_style_with(
            MapStyle::Liberty,
            &sender,
            &Ok(store),
            |_| { Err(String::from("network unavailable")) }
        ));
        match receiver.try_recv().map_err(|error| error.to_string())? {
            StyleResult::Cached { style, document } => {
                assert_eq!(style, MapStyle::Liberty);
                assert_eq!(document, original);
            }
            _ => return Err(TestError::from("expected the cached style first")),
        }
        match receiver.try_recv().map_err(|error| error.to_string())? {
            StyleResult::Failed { style, error } => {
                assert_eq!(style, MapStyle::Liberty);
                assert_eq!(error, "network unavailable");
            }
            _ => return Err(TestError::from("expected a refresh failure")),
        }
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn invalid_refreshed_style_does_not_replace_cached_style() -> Result<(), TestError> {
        let (root, store) = test_store("invalid")?;
        let url = "https://tiles.openfreemap.org/styles/liberty";
        let original = serde_json::json!({"layers": []});
        store.store(
            url,
            &serde_json::to_vec(&original).map_err(|error| error.to_string())?,
        )?;
        let (sender, receiver) = mpsc::channel();
        assert!(load_style_with(
            MapStyle::Liberty,
            &sender,
            &Ok(store),
            |_| { Ok(serde_json::json!({"not_layers": []})) }
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(StyleResult::Cached { .. })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(StyleResult::Failed { .. })
        ));
        assert_eq!(
            cached_style(&Store::at(root.clone()), url).map_err(TestError::from)?,
            Some(original)
        );
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn corrupt_cache_and_failed_refresh_report_both_errors() -> Result<(), TestError> {
        let (root, store) = test_store("corrupt")?;
        let url = "https://tiles.openfreemap.org/styles/liberty";
        store.store(url, b"not json")?;
        let (sender, receiver) = mpsc::channel();
        assert!(load_style_with(
            MapStyle::Liberty,
            &sender,
            &Ok(store),
            |_| { Err(String::from("network unavailable")) }
        ));
        match receiver.try_recv().map_err(|error| error.to_string())? {
            StyleResult::Failed { error, .. } => {
                assert!(error.contains("network unavailable"));
                assert!(error.contains("cached style could not be read"));
            }
            _ => return Err(TestError::from("expected both failures")),
        }
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

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

    struct LocalSource {
        url: String,
    }

    impl TileSource for LocalSource {
        fn tile_url(&self, _: TileId) -> String {
            self.url.clone()
        }

        fn attribution(&self) -> Attribution {
            OpenFreeMap.attribution()
        }
    }

    fn wait_for_tile(
        tiles: &mut BasemapTiles<LocalSource>,
        id: TileId,
    ) -> Result<TilePiece, TestError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            if let Some(tile) = tiles.at(id) {
                return Ok(tile);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(TestError::from("timed out waiting for the tile"))
    }

    fn local_tiles(
        url: &str,
        path: &std::path::Path,
    ) -> Result<BasemapTiles<LocalSource>, TestError> {
        Ok(BasemapTiles::new(
            LocalSource {
                url: String::from(url),
            },
            synthetic_style()?,
            path,
            &egui::Context::default(),
        ))
    }

    #[test]
    fn fresh_http_tile_is_reused_after_tile_source_restart() -> Result<(), TestError> {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let (root, _) = test_store("http-restart")?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let body = synthetic_tile()?;
        let server = std::thread::spawn(move || -> Result<String, String> {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error.to_string()),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
            let mut request = [0_u8; 4096];
            let length = stream
                .read(&mut request)
                .map_err(|error| error.to_string())?;
            let request =
                String::from_utf8_lossy(request.get(..length).ok_or("invalid request length")?)
                    .into_owned();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.mapbox-vector-tile\r\nCache-Control: public, max-age=3600\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(header.as_bytes())
                .map_err(|error| error.to_string())?;
            stream.write_all(&body).map_err(|error| error.to_string())?;
            Ok(request)
        });
        let url = format!("http://{address}/4/3/7.pbf");
        let id = TileId {
            zoom: 4,
            x: 3,
            y: 7,
        };
        let mut first = local_tiles(&url, &root)?;
        let first_tile = wait_for_tile(&mut first, id)?;
        assert!(matches!(first_tile.tile, walkers::Tile::Vector(ref shapes) if !shapes.is_empty()));
        drop(first);
        let request = server
            .join()
            .map_err(|_| "tile server stopped")?
            .map_err(TestError::from)?;
        assert!(request.starts_with("GET /4/3/7.pbf HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains(concat!("user-agent: oxrad/", env!("CARGO_PKG_VERSION")))
        );
        let mut restarted = local_tiles(&url, &root)?;
        let cached = wait_for_tile(&mut restarted, id)?;
        assert!(matches!(cached.tile, walkers::Tile::Vector(ref shapes) if !shapes.is_empty()));
        assert!(restarted.attribution().text.contains("OpenStreetMap"));
        drop(restarted);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn failed_tile_reports_error_and_can_be_retried() -> Result<(), TestError> {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let (root, _) = test_store("http-retry")?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let body = synthetic_tile()?;
        let server = std::thread::spawn(move || -> Result<(), String> {
            for response in 0..2 {
                let deadline = Instant::now() + Duration::from_secs(8);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .map_err(|error| error.to_string())?;
                let mut request = [0_u8; 4096];
                stream
                    .read(&mut request)
                    .map_err(|error| error.to_string())?;
                if response == 0 {
                    stream.write_all(b"HTTP/1.1 404 Not Found\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").map_err(|error| error.to_string())?;
                } else {
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream
                        .write_all(header.as_bytes())
                        .map_err(|error| error.to_string())?;
                    stream.write_all(&body).map_err(|error| error.to_string())?;
                }
            }
            Ok(())
        });
        let url = format!("http://{address}/4/3/7.pbf");
        let id = TileId {
            zoom: 4,
            x: 3,
            y: 7,
        };
        let mut tiles = local_tiles(&url, &root)?;
        let deadline = Instant::now() + Duration::from_secs(8);
        let error = loop {
            assert!(tiles.at(id).is_none());
            if let Some(error) = tiles.error(id) {
                break error.to_string();
            }
            if Instant::now() >= deadline {
                return Err(TestError::from("tile failure was not reported"));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(error.contains("returned 404"), "unexpected error: {error}");
        tiles.retry_failed();
        let tile = wait_for_tile(&mut tiles, id)?;
        assert!(matches!(tile.tile, walkers::Tile::Vector(ref shapes) if !shapes.is_empty()));
        assert!(tiles.error(id).is_none());
        server
            .join()
            .map_err(|_| "tile server stopped")?
            .map_err(TestError::from)?;
        drop(tiles);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
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
