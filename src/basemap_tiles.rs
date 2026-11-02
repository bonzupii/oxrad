// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded asynchronous tile acquisition and display state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Pos2, Rect};
use http_cache_reqwest::{CACacheManager, Cache, CacheMode, HttpCache, HttpCacheOptions};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use walkers::sources::{Attribution, TileSource};
use walkers::{Style, Tile, TileId, TilePiece, Tiles};

const WORKERS: usize = 4;
const MAX_TILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MEMORY_TILES: usize = 256;
const RETRY_DELAY: Duration = Duration::from_secs(10);
const TIMEOUT: Duration = Duration::from_secs(20);
const USER_AGENT: &str = concat!(
    "oxrad/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/bonzupii/oxrad)"
);

#[derive(Debug)]
enum Failure {
    Setup(String),
    Request(reqwest_middleware::Error),
    Status {
        code: u16,
        retry_after: Option<String>,
    },
    Body(reqwest::Error),
    TooLarge,
    Decode(String),
    WorkerStopped,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "Tile worker setup failed: {error}"),
            Self::Request(error) => write!(formatter, "Tile request failed: {error}"),
            Self::Status { code, retry_after } => {
                write!(formatter, "Tile provider returned HTTP {code}")?;
                if let Some(value) = retry_after {
                    write!(formatter, " (Retry-After: {value})")?;
                }
                Ok(())
            }
            Self::Body(error) => write!(formatter, "Tile response failed: {error}"),
            Self::TooLarge => formatter.write_str("Tile response exceeds the size limit"),
            Self::Decode(error) => write!(formatter, "Tile could not be decoded: {error}"),
            Self::WorkerStopped => formatter.write_str("Tile worker stopped"),
        }
    }
}

impl Failure {
    fn retry_in(&self) -> Option<Duration> {
        match self {
            Self::Request(_) | Self::Body(_) => Some(RETRY_DELAY),
            Self::Status {
                code: 429 | 503,
                retry_after: Some(value),
            } => crate::retry::retry_after_delay(value)
                .map_or(None, |delay| Some(delay.max(RETRY_DELAY))),
            Self::Status {
                code: 500..=599, ..
            }
            | Self::Status { code: 429, .. } => Some(RETRY_DELAY),
            Self::Setup(_)
            | Self::Status { .. }
            | Self::TooLarge
            | Self::Decode(_)
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
    result: Result<Vec<u8>, Failure>,
}

struct Worker {
    requests: Sender<Job>,
    busy: Option<TileId>,
}

fn client(path: PathBuf) -> Result<ClientWithMiddleware, Failure> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(3))
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| Failure::Setup(error.to_string()))?;
    Ok(ClientBuilder::new(client)
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

async fn fetch(client: &ClientWithMiddleware, url: &str) -> Result<Vec<u8>, Failure> {
    let mut response = client.get(url).send().await.map_err(Failure::Request)?;
    let code = response.status().as_u16();
    if !(200..300).contains(&code) {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        return Err(Failure::Status { code, retry_after });
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_TILE_BYTES as u64)
    {
        return Err(Failure::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Failure::Body)? {
        if body.len().saturating_add(chunk.len()) > MAX_TILE_BYTES {
            return Err(Failure::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn start_worker(path: PathBuf, events: Sender<Completion>, context: egui::Context) -> Worker {
    let (requests, receiver) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let client = client(path);
        while let Ok(job) = receiver.recv() {
            let result = match (&runtime, &client) {
                (Ok(runtime), Ok(client)) => runtime.block_on(fetch(client, &job.url)),
                (Err(error), _) => Err(Failure::Setup(error.to_string())),
                (_, Err(error)) => Err(Failure::Setup(error.to_string())),
            };
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
    error: Failure,
    retry_at: Option<Instant>,
}

/// Owns tile requests and their lifecycle; workers send results to this owner.
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

    pub fn set_style(&mut self, style: Style) {
        self.style = style;
        let mut invalid = Vec::new();
        for (id, ready) in &mut self.ready {
            match Tile::from_mvt(&ready.bytes, &self.style, id.zoom) {
                Ok(tile) => ready.tile = tile,
                Err(error) => invalid.push((*id, Failure::Decode(error.to_string()))),
            }
        }
        for (id, error) in invalid {
            self.ready.remove(&id);
            self.failed.insert(
                id,
                FailedTile {
                    error,
                    retry_at: None,
                },
            );
        }
    }

    pub fn retry_failed(&mut self) {
        let now = Instant::now();
        self.failed
            .retain(|_, failure| failure.retry_at.is_some_and(|due| due > now));
    }

    pub fn error(&self, id: TileId) -> Option<String> {
        let id = self.limit_zoom(id).0;
        self.failed
            .get(&id)
            .map(|failure| failure.error.to_string())
    }

    fn limit_zoom(&self, id: TileId) -> (TileId, Rect) {
        let zoom = id.zoom.min(self.source.max_zoom());
        let scale = 1_u32 << (id.zoom - zoom);
        let side = 1.0 / scale as f32;
        let x = (id.x % scale) as f32 * side;
        let y = (id.y % scale) as f32 * side;
        (
            TileId {
                x: id.x / scale,
                y: id.y / scale,
                zoom,
            },
            Rect::from_min_max(Pos2::new(x, y), Pos2::new(x + side, y + side)),
        )
    }

    fn poll(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(completion) => self.complete(completion),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn complete(&mut self, completion: Completion) {
        if let Some(worker) = self
            .workers
            .iter_mut()
            .find(|worker| worker.busy == Some(completion.id))
        {
            worker.busy = None;
        }
        let result = completion.result.and_then(|bytes| {
            Tile::from_mvt(&bytes, &self.style, completion.id.zoom)
                .map(|tile| (bytes, tile))
                .map_err(|error| Failure::Decode(error.to_string()))
        });
        match result {
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
                if self.ready.len() > MAX_MEMORY_TILES
                    && let Some(id) = self
                        .ready
                        .iter()
                        .min_by_key(|(_, tile)| tile.touched)
                        .map(|(id, _)| *id)
                {
                    self.ready.remove(&id);
                }
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

    fn schedule(&mut self, id: TileId) {
        if self.ready.contains_key(&id) || self.workers.iter().any(|worker| worker.busy == Some(id))
        {
            return;
        }
        if let Some(failure) = self.failed.get(&id) {
            if failure.retry_at.is_none_or(|at| Instant::now() < at) {
                return;
            }
            self.failed.remove(&id);
        }
        let Some(worker) = self.workers.iter_mut().find(|worker| worker.busy.is_none()) else {
            return;
        };
        let job = Job {
            id,
            url: self.source.tile_url(id),
        };
        if worker.requests.send(job).is_ok() {
            worker.busy = Some(id);
        } else {
            self.failed.insert(
                id,
                FailedTile {
                    error: Failure::WorkerStopped,
                    retry_at: None,
                },
            );
        }
    }
}

impl<S: TileSource> Tiles for BasemapTiles<S> {
    fn at(&mut self, id: TileId) -> Option<TilePiece> {
        self.poll();
        let count = 1_u32.checked_shl(u32::from(id.zoom))?;
        if id.x >= count || id.y >= count {
            return None;
        }
        let (id, uv) = self.limit_zoom(id);
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
