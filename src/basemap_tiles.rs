// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! Asynchronous basemap tile acquisition, decoding, and display state.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Pos2, Rect};
use http_cache_reqwest::{CACacheManager, Cache, CacheMode, HttpCache, HttpCacheOptions};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

/// Concurrent requests allowed by the OpenFreeMap terms for offline downloads.
const WORKERS: usize = 4;
const MAX_TILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MEMORY_TILES: usize = 256;
const RETRY_DELAY: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = concat!(
    "oxrad/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/bonzupii/oxrad)"
);

/// A tile request or decode that did not produce renderable geometry.
#[derive(Debug)]
pub enum TileError {
    Runtime {
        source: std::io::Error,
    },
    Client {
        source: reqwest::Error,
    },
    Request {
        url: String,
        source: reqwest_middleware::Error,
    },
    Status {
        url: String,
        code: u16,
        retry_after: Option<String>,
    },
    Body {
        url: String,
        source: reqwest::Error,
    },
    TooLarge {
        url: String,
        bytes: usize,
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
            Self::Runtime { source } => {
                format!("The tile download runtime could not start: {source}")
            }
            Self::Client { source } => {
                format!("The tile HTTP client could not be created: {source}")
            }
            Self::Request { url, source } => format!("The request to {url} failed: {source}"),
            Self::Status {
                url,
                code,
                retry_after,
            } => retry_after.as_ref().map_or_else(
                || format!("{url} returned {code}."),
                |value| format!("{url} returned {code} and asked to retry after {value}."),
            ),
            Self::Body { url, source } => {
                format!("The tile body from {url} could not be read: {source}")
            }
            Self::TooLarge { url, bytes } => format!(
                "The tile from {url} is {bytes} bytes, above the {MAX_TILE_BYTES} byte limit."
            ),
            Self::Decode { url, zoom, detail } => {
                format!("The tile from {url} at zoom {zoom} is not renderable: {detail}")
            }
            Self::WorkerStopped => String::from("The tile download worker stopped."),
        }
    }

    /// How long to wait before repeating this request, when repeating may help.
    fn retry_in(&self) -> Option<Duration> {
        match self {
            Self::Request { .. } | Self::Body { .. } => Some(RETRY_DELAY),
            Self::Status {
                code: 429 | 503,
                retry_after: Some(value),
                ..
            } => crate::retry::retry_after_delay(value)
                .ok()
                .map(|delay| delay.max(RETRY_DELAY)),
            Self::Status {
                code: 429 | 500..=599,
                ..
            } => Some(RETRY_DELAY),
            Self::Runtime { .. }
            | Self::Client { .. }
            | Self::Status { .. }
            | Self::TooLarge { .. }
            | Self::Decode { .. }
            | Self::WorkerStopped => None,
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
            Self::Runtime { source } => Some(source),
            Self::Client { source } | Self::Body { source, .. } => Some(source),
            Self::Request { source, .. } => Some(source),
            Self::Status { .. }
            | Self::TooLarge { .. }
            | Self::Decode { .. }
            | Self::WorkerStopped => None,
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
}

struct Worker {
    requests: Sender<Job>,
    busy: Option<TileId>,
}

fn client(path: PathBuf) -> Result<ClientWithMiddleware, TileError> {
    let http = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|source| TileError::Client { source })?;
    Ok(ClientBuilder::new(http)
        .with(Cache(HttpCache {
            mode: CacheMode::Default,
            manager: CACacheManager {
                path,
                remove_opts: Default::default(),
            },
            options: HttpCacheOptions::default(),
        }))
        .build())
}

async fn read_body(response: reqwest::Response, url: &str) -> Result<Vec<u8>, TileError> {
    let mut response = response;
    let mut body = Vec::new();
    loop {
        let chunk = response.chunk().await.map_err(|source| TileError::Body {
            url: String::from(url),
            source,
        })?;
        let Some(chunk) = chunk else {
            return Ok(body);
        };
        let next = body.len().saturating_add(chunk.len());
        if next > MAX_TILE_BYTES {
            return Err(TileError::TooLarge {
                url: String::from(url),
                bytes: next,
            });
        }
        body.extend_from_slice(&chunk);
    }
}

async fn fetch(client: &ClientWithMiddleware, url: &str) -> Result<Vec<u8>, TileError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|source| TileError::Request {
            url: String::from(url),
            source,
        })?;
    let code = response.status().as_u16();
    if !(200..300).contains(&code) {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        return Err(TileError::Status {
            url: String::from(url),
            code,
            retry_after,
        });
    }
    read_body(response, url).await
}

/// Reports a setup failure for the first queued job, then lets the worker exit.
fn report_setup_failure(
    receiver: &Receiver<Job>,
    events: &Sender<Completion>,
    context: &egui::Context,
    error: TileError,
) {
    let Ok(job) = receiver.recv() else {
        return;
    };
    if events
        .send(Completion {
            id: job.id,
            result: Err(error),
        })
        .is_ok()
    {
        context.request_repaint();
    }
}

fn start_worker(path: PathBuf, events: Sender<Completion>, context: egui::Context) -> Worker {
    let (requests, receiver) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let client = client(path);
        let prepared: Result<(tokio::runtime::Runtime, ClientWithMiddleware), TileError> =
            match (runtime, client) {
                (Ok(runtime), Ok(client)) => Ok((runtime, client)),
                (Err(source), _) => {
                    report_setup_failure(
                        &receiver,
                        &events,
                        &context,
                        TileError::Runtime { source },
                    );
                    return;
                }
                (_, Err(error)) => {
                    report_setup_failure(&receiver, &events, &context, error);
                    return;
                }
            };
        let Ok((runtime, client)) = prepared else {
            return;
        };
        while let Ok(job) = receiver.recv() {
            let result = runtime.block_on(fetch(&client, &job.url));
            if events.send(Completion { id: job.id, result }).is_err() {
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

struct ReadyTile {
    bytes: Vec<u8>,
    tile: Tile,
    touched: u64,
}

struct FailedTile {
    error: TileError,
    retry_at: Option<Instant>,
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
}

impl<S: TileSource> BasemapTiles<S> {
    pub fn new(source: S, style: Style, path: &Path, context: &egui::Context) -> Self {
        let (events, receiver) = mpsc::channel();
        let workers = (0..WORKERS)
            .map(|_| start_worker(path.to_path_buf(), events.clone(), context.clone()))
            .collect();
        Self {
            source,
            style,
            ready: HashMap::new(),
            failed: HashMap::new(),
            workers,
            events: receiver,
            clock: 0,
        }
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
                            retry_at: None,
                        },
                    );
                }
                None => {}
            }
        }
    }

    /// Clears deterministic and backed-off failures so the next draw asks again.
    pub fn retry_failed(&mut self) {
        let now = Instant::now();
        self.failed
            .retain(|_, failure| failure.retry_at.is_some_and(|due| due > now));
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
                let retry_at = error
                    .retry_in()
                    .and_then(|delay| Instant::now().checked_add(delay));
                self.failed
                    .insert(completion.id, FailedTile { error, retry_at });
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
        let Some(failure) = self.failed.get(&id) else {
            return false;
        };
        failure.retry_at.is_none_or(|at| Instant::now() < at)
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
                        retry_at: None,
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
    fn a_status_error_names_the_provider_and_its_retry() {
        let error = TileError::Status {
            url: String::from("https://example.test/4/3/7.pbf"),
            code: 503,
            retry_after: Some(String::from("30")),
        };
        assert_eq!(
            error.to_string(),
            "https://example.test/4/3/7.pbf returned 503 and asked to retry after 30."
        );
        assert_eq!(error.retry_in(), Some(Duration::from_secs(30)));
        assert!(error.source().is_none());
    }

    #[test]
    fn a_missing_tile_is_not_retried_and_a_server_fault_is() {
        let missing = TileError::Status {
            url: String::from("https://example.test/4/3/7.pbf"),
            code: 404,
            retry_after: None,
        };
        assert!(missing.retry_in().is_none());
        let fault = TileError::Status {
            url: String::from("https://example.test/4/3/7.pbf"),
            code: 502,
            retry_after: None,
        };
        assert_eq!(fault.retry_in(), Some(RETRY_DELAY));
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
