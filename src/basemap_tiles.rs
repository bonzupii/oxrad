// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! Asynchronous basemap tile acquisition, decoding, and display state.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use eframe::egui::{self, Pos2, Rect};
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

use crate::acquire::Acquire;
use crate::engine::RequestSpec;
use crate::provider::FetchError;
use crate::sources;

/// Concurrent requests allowed by the OpenFreeMap terms for offline downloads.
const WORKERS: usize = 4;
const MAX_MEMORY_TILES: usize = 256;

/// A tile request or decode that did not produce renderable geometry.
#[derive(Debug)]
pub enum TileError {
    Status {
        url: String,
        code: u16,
        retry_after: Option<String>,
    },
    Provider {
        source: FetchError,
    },
    Decode {
        url: String,
        zoom: u8,
        detail: String,
    },
    WorkerStopped,
}

impl TileError {
    fn message(&self) -> String {
        match self {
            Self::Status {
                url,
                code,
                retry_after,
            } => retry_after.as_ref().map_or_else(
                || format!("{url} returned {code}."),
                |value| format!("{url} returned {code} and asked to retry after {value}."),
            ),
            Self::Provider { source } => format!("The map tile request failed: {source}"),
            Self::Decode { url, zoom, detail } => {
                format!("The tile from {url} at zoom {zoom} is not renderable: {detail}")
            }
            Self::WorkerStopped => String::from("The tile download worker stopped."),
        }
    }
}

impl Display for TileError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for TileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Status { .. } | Self::Decode { .. } | Self::WorkerStopped => None,
        }
    }
}

struct Job {
    id: TileId,
    url: String,
}

struct Completion {
    id: TileId,
    result: Result<Vec<u8>, TileError>,
    warning: Option<TileError>,
}

struct Worker {
    requests: Sender<Job>,
    busy: Option<TileId>,
}

fn start_worker(
    _path: PathBuf,
    events: Sender<Completion>,
    context: egui::Context,
    acquire: Acquire,
) -> Worker {
    let (requests, receiver) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        while let Ok(job) = receiver.recv() {
            let (result, warning) = load_tile(&acquire, &job.url);
            if events
                .send(Completion {
                    id: job.id,
                    result,
                    warning,
                })
                .is_err()
            {
                return;
            }
            context.request_repaint();
        }
    });
    Worker {
        requests,
        busy: None,
    }
}

/// Reads a tile through the engine, which owns its cache and retries.
fn load_tile(acquire: &Acquire, url: &str) -> (Result<Vec<u8>, TileError>, Option<TileError>) {
    let spec = RequestSpec::for_source(
        &sources::OPENFREEMAP,
        url,
        "application/vnd.mapbox-vector-tile",
    )
    .ttl(Duration::from_secs(86_400));
    match acquire.fetch(&spec) {
        Ok(fetched) => (Ok(fetched.body), None),
        Err(source) => (Err(provider_error(source)), None),
    }
}

fn provider_error(source: FetchError) -> TileError {
    match source {
        FetchError::Status {
            url,
            status,
            retry_after,
        } => TileError::Status {
            url,
            code: status,
            retry_after,
        },
        other => TileError::Provider { source: other },
    }
}

struct ReadyTile {
    bytes: Vec<u8>,
    tile: Tile,
    touched: u64,
}

struct FailedTile {
    error: TileError,
}

/// Owns tile requests, decoded tiles, and their failure state.
pub struct BasemapTiles<S: TileSource> {
    source: S,
    style: Style,
    ready: HashMap<TileId, ReadyTile>,
    failed: HashMap<TileId, FailedTile>,
    workers: Vec<Worker>,
    events: Receiver<Completion>,
    clock: u64,
    warning: Option<TileError>,
}

impl<S: TileSource> BasemapTiles<S> {
    pub fn new(
        source: S,
        style: Style,
        path: &Path,
        context: &egui::Context,
        acquire: &Acquire,
    ) -> Self {
        let (events, receiver) = mpsc::channel();
        let workers = (0..WORKERS)
            .map(|_| {
                start_worker(
                    path.to_path_buf(),
                    events.clone(),
                    context.clone(),
                    acquire.clone(),
                )
            })
            .collect();
        Self {
            source,
            style,
            ready: HashMap::new(),
            failed: HashMap::new(),
            workers,
            events: receiver,
            clock: 0,
            warning: None,
        }
    }

    /// Takes the most recent non-fatal cache problem, if any.
    pub const fn take_warning(&mut self) -> Option<TileError> {
        self.warning.take()
    }

    /// Decodes retained tile bytes with the new style, without network access.
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
        let ids: Vec<TileId> = self.ready.keys().copied().collect();
        for id in ids {
            let decoded = self.ready.get(&id).map(|ready| {
                Tile::from_mvt(&ready.bytes, &self.style, id.zoom)
                    .map_err(|source| source.to_string())
            });
            match decoded {
                Some(Ok(tile)) => {
                    if let Some(ready) = self.ready.get_mut(&id) {
                        ready.tile = tile;
                    }
                }
                Some(Err(detail)) => {
                    self.ready.remove(&id);
                    let url = self.source.tile_url(id);
                    self.failed.insert(
                        id,
                        FailedTile {
                            error: TileError::Decode {
                                url,
                                zoom: id.zoom,
                                detail,
                            },
                        },
                    );
                }
                None => {}
            }
        }
    }

    /// Clears failures so the next draw asks again through the engine.
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }

    pub fn error(&self, id: TileId) -> Option<&TileError> {
        let (id, _) = self.limit_zoom(id)?;
        self.failed.get(&id).map(|failure| &failure.error)
    }

    /// The source tile that carries this view tile, with its sub-tile bounds.
    fn limit_zoom(&self, id: TileId) -> Option<(TileId, Rect)> {
        let zoom = id.zoom.min(self.source.max_zoom());
        let depth = u32::from(id.zoom.checked_sub(zoom)?);
        let scale = 1_u32.checked_shl(depth)?;
        let side = 1.0 / scale as f32;
        let x = (id.x % scale) as f32 * side;
        let y = (id.y % scale) as f32 * side;
        Some((
            TileId {
                x: id.x / scale,
                y: id.y / scale,
                zoom,
            },
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + side, y + side)),
        ))
    }

    fn drain(&mut self) {
        while let Ok(completion) = self.events.try_recv() {
            self.complete(completion);
        }
    }

    fn decode(&self, id: TileId, bytes: Vec<u8>) -> Result<(Vec<u8>, Tile), TileError> {
        let tile =
            Tile::from_mvt(&bytes, &self.style, id.zoom).map_err(|source| TileError::Decode {
                url: self.source.tile_url(id),
                zoom: id.zoom,
                detail: source.to_string(),
            })?;
        Ok((bytes, tile))
    }

    fn complete(&mut self, completion: Completion) {
        if let Some(warning) = completion.warning {
            self.warning = Some(warning);
        }
        if let Some(worker) = self
            .workers
            .iter_mut()
            .find(|worker| worker.busy == Some(completion.id))
        {
            worker.busy = None;
        }
        let decoded = completion
            .result
            .and_then(|bytes| self.decode(completion.id, bytes));
        match decoded {
            Ok((bytes, tile)) => {
                self.failed.remove(&completion.id);
                self.ready.insert(
                    completion.id,
                    ReadyTile {
                        bytes,
                        tile,
                        touched: self.clock,
                    },
                );
                self.evict();
            }
            Err(error) => {
                self.failed.insert(completion.id, FailedTile { error });
            }
        }
    }

    fn evict(&mut self) {
        while self.ready.len() > MAX_MEMORY_TILES {
            let Some(id) = self
                .ready
                .iter()
                .min_by_key(|(_, tile)| tile.touched)
                .map(|(id, _)| *id)
            else {
                return;
            };
            self.ready.remove(&id);
        }
    }

    fn waiting(&self, id: TileId) -> bool {
        if self.ready.contains_key(&id) {
            return true;
        }
        if self.workers.iter().any(|worker| worker.busy == Some(id)) {
            return true;
        }
        let Some(_failure) = self.failed.get(&id) else {
            return false;
        };
        true
    }

    fn schedule(&mut self, id: TileId) {
        if self.waiting(id) {
            return;
        }
        self.failed.remove(&id);
        let Some(worker) = self.workers.iter_mut().find(|worker| worker.busy.is_none()) else {
            return;
        };
        let job = Job {
            id,
            url: self.source.tile_url(id),
        };
        match worker.requests.send(job) {
            Ok(()) => worker.busy = Some(id),
            Err(_) => {
                self.failed.insert(
                    id,
                    FailedTile {
                        error: TileError::WorkerStopped,
                    },
                );
            }
        }
    }
}

impl<S: TileSource> Tiles for BasemapTiles<S> {
    fn at(&mut self, id: TileId) -> Option<TilePiece> {
        self.drain();
        let (id, uv) = self.limit_zoom(id)?;
        self.schedule(id);
        self.clock = self.clock.saturating_add(1);
        let tile = self.ready.get_mut(&id)?;
        tile.touched = self.clock;
        Some(TilePiece::new(tile.tile.clone(), uv))
    }

    fn attribution(&self) -> Attribution {
        self.source.attribution()
    }

    fn tile_size(&self) -> u32 {
        self.source.tile_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    struct FixedSource {
        url: String,
        max_zoom: u8,
    }

    impl TileSource for FixedSource {
        fn tile_url(&self, _: TileId) -> String {
            self.url.clone()
        }

        fn attribution(&self) -> Attribution {
            Attribution {
                text: "test",
                url: "https://example.test",
                logo_light: None,
                logo_dark: None,
            }
        }

        fn max_zoom(&self) -> u8 {
            self.max_zoom
        }
    }

    fn directory(name: &str) -> Result<PathBuf, TestError> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = PathBuf::from("target").join(format!(
            "basemap-tiles-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(root)
    }

    fn owner(root: &Path, max_zoom: u8) -> BasemapTiles<FixedSource> {
        BasemapTiles::new(
            FixedSource {
                url: String::from("http://127.0.0.1:9/0/0/0.pbf"),
                max_zoom,
            },
            Style::default(),
            root,
            &egui::Context::default(),
            &Acquire::offline(),
        )
    }

    #[test]
    fn a_tile_above_the_source_zoom_maps_onto_its_parent() -> Result<(), TestError> {
        let root = directory("limit")?;
        let tiles = owner(&root, 2);
        let (parent, uv) = tiles
            .limit_zoom(TileId {
                zoom: 4,
                x: 9,
                y: 6,
            })
            .ok_or("a zoom-limited tile must resolve")?;
        assert_eq!(
            parent,
            TileId {
                zoom: 2,
                x: 2,
                y: 1
            }
        );
        assert!((uv.min.x - 0.25).abs() < 1e-6);
        assert!((uv.min.y - 0.5).abs() < 1e-6);
        assert!((uv.max.x - 0.5).abs() < 1e-6);
        assert!((uv.max.y - 0.75).abs() < 1e-6);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn a_zoom_beyond_the_shift_limit_is_rejected() -> Result<(), TestError> {
        let root = directory("shift")?;
        let tiles = owner(&root, 0);
        assert!(
            tiles
                .limit_zoom(TileId {
                    zoom: 40,
                    x: 0,
                    y: 0
                })
                .is_none(),
            "a zoom above 31 must not shift"
        );
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn a_missing_tile_is_visible_until_the_user_retries_it() -> Result<(), TestError> {
        let root = directory("missing")?;
        let mut tiles = owner(&root, 14);
        let id = TileId {
            zoom: 4,
            x: 3,
            y: 7,
        };
        tiles.complete(Completion {
            id,
            result: Err(TileError::Status {
                url: String::from("https://example.test/4/3/7.pbf"),
                code: 404,
                retry_after: None,
            }),
            warning: None,
        });
        assert!(matches!(
            tiles.error(id),
            Some(TileError::Status { code: 404, .. })
        ));
        tiles.retry_failed();
        assert!(
            tiles.error(id).is_none(),
            "the retry must clear a 404 state"
        );
        drop(tiles);
        std::fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }
}
