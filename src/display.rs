// oxrad: A storm-chaser-grade NEXRAD weather radar viewer
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
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use eframe::egui::Shape;

use crate::error::RenderError;
use crate::gpu::RadarGpu;
use crate::radar::RadarVolume;
use crate::render::{self, RenderCache, RenderRequest, RenderResult};

pub struct Frame {
    pub volume: RadarVolume,
    pub cache: RenderCache,
    pub shape: Shape,
    generation: u64,
}

pub enum DisplayState {
    Waiting,
    Preparing(u64),
    Ready,
    Failed {
        error: RenderError,
        volume: RadarVolume,
    },
    Unavailable(RenderError),
}

pub struct Display {
    sender: Sender<RenderRequest>,
    receiver: Receiver<RenderResult>,
    gpu_errors: Receiver<u64>,
    pub state: DisplayState,
    pub frame: Option<Frame>,
    pub gpu_error: Option<RenderError>,
    queued: Option<(u64, RadarVolume)>,
    generation: u64,
}

impl Display {
    pub fn new(gpu: Option<RadarGpu>) -> Self {
        let available = gpu.is_some();
        let (gpu_sender, gpu_errors) = mpsc::channel();
        let (sender, receiver) = render::start_render_thread(gpu, gpu_sender);
        let state = if available {
            DisplayState::Waiting
        } else {
            DisplayState::Unavailable(RenderError::GpuUnavailable)
        };
        Self {
            sender,
            receiver,
            gpu_errors,
            state,
            frame: None,
            gpu_error: None,
            queued: None,
            generation: 0,
        }
    }

    pub fn submit(&mut self, volume: RadarVolume) {
        self.generation = self.generation.wrapping_add(1);
        if matches!(self.state, DisplayState::Preparing(_)) {
            self.queued = Some((self.generation, volume));
        } else if !matches!(self.state, DisplayState::Unavailable(_)) {
            self.send(self.generation, volume);
        }
    }

    fn send(&mut self, generation: u64, volume: RadarVolume) {
        let request = RenderRequest {
            volume,
            rect: render::canonical_rect(),
            generation,
        };
        self.state = match self.sender.send(request) {
            Ok(()) => DisplayState::Preparing(generation),
            Err(_) => DisplayState::Unavailable(RenderError::WorkerStopped),
        };
    }

    pub fn retry(&mut self) {
        let state = std::mem::replace(&mut self.state, DisplayState::Waiting);
        match state {
            DisplayState::Failed { volume, .. } => self.send(self.generation, volume),
            other => self.state = other,
        }
    }

    pub fn poll(&mut self) {
        self.poll_gpu();
        match self.receiver.try_recv() {
            Ok(result) => self.complete(result),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.state = DisplayState::Unavailable(RenderError::WorkerStopped)
            }
        }
    }

    fn poll_gpu(&mut self) {
        while let Ok(generation) = self.gpu_errors.try_recv() {
            if self
                .frame
                .as_ref()
                .is_some_and(|frame| frame.generation == generation)
                && let Some(frame) = self.frame.take()
            {
                if matches!(self.state, DisplayState::Ready) {
                    self.state = DisplayState::Failed {
                        error: RenderError::GpuDraw,
                        volume: frame.volume,
                    };
                } else {
                    self.gpu_error = Some(RenderError::GpuDraw);
                }
            }
        }
    }

    fn complete(&mut self, result: RenderResult) {
        if !matches!(self.state, DisplayState::Preparing(generation) if generation == result.generation)
        {
            return;
        }
        if let Some((generation, volume)) = self.queued.take() {
            self.send(generation, volume);
            return;
        }
        self.state = match result.cache {
            Ok((cache, shape)) => {
                self.gpu_error = None;
                self.frame = Some(Frame {
                    volume: result.volume,
                    cache,
                    shape,
                    generation: result.generation,
                });
                DisplayState::Ready
            }
            Err(error) => DisplayState::Failed {
                error,
                volume: result.volume,
            },
        };
    }
}
