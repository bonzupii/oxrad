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
//! The displayed frame and the render worker that prepares it.

use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use eframe::egui::{Rect, Shape};

use crate::colormap::ColorTable;
use crate::error::RenderError;
use crate::gpu::{self, RadarGpu, RadarMesh};
use crate::product::RadarProduct;
use crate::radar::{ArchiveOrigin, RadarVolume, RetrievedVolume};
use crate::render::{self, RadarGeometry};

pub struct Frame {
    pub volume: RadarVolume,
    pub origin: ArchiveOrigin,
    pub history_key: Option<String>,
    pub geometry: RadarGeometry,
    pub mesh: Option<RadarMesh>,
}

impl Frame {
    /// The draw callback for this frame at the current viewport.
    pub fn shape(&self, viewport: Rect) -> Option<Shape> {
        self.mesh.as_ref().map(|mesh| gpu::shape(mesh, viewport))
    }
}

pub enum DisplayState {
    Waiting,
    Building,
    Ready,
    Unavailable(RenderError),
    Failed {
        error: RenderError,
        volume: Box<RetrievedVolume>,
    },
}

impl DisplayState {
    pub const fn error(&self) -> Option<&RenderError> {
        match self {
            Self::Unavailable(error) => Some(error),
            Self::Failed { error, .. } => Some(error),
            Self::Waiting | Self::Building | Self::Ready => None,
        }
    }

    pub const fn is_busy(&self) -> bool {
        matches!(self, Self::Waiting | Self::Building)
    }

    pub const fn retryable(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

pub struct Display {
    gpu: Option<RadarGpu>,
    sender: Sender<render::BuildRequest>,
    receiver: Receiver<render::BuildResult>,
    pub state: DisplayState,
    pub frame: Option<Frame>,
    queued: Option<(u64, RetrievedVolume, RadarProduct)>,
    generation: u64,
    product: RadarProduct,
    color_table: ColorTable,
}

impl Display {
    pub fn new(gpu: Option<RadarGpu>, color_table: ColorTable) -> Self {
        let (sender, receiver) = render::start_build_thread();
        let state = if gpu.is_some() {
            DisplayState::Waiting
        } else {
            DisplayState::Unavailable(RenderError::GpuUnavailable)
        };
        Self {
            gpu,
            sender,
            receiver,
            state,
            frame: None,
            queued: None,
            generation: 0,
            product: RadarProduct::Reflectivity,
            color_table,
        }
    }

    /// Recolors the displayed volume without changing its product or tilt.
    pub fn set_color_table(&mut self, color_table: ColorTable) -> bool {
        self.color_table = color_table;
        self.rebuild(self.product, None)
    }

    /// Drops the current frame and discards any result still in flight.
    /// Moves the current mesh into a geographic view. A missing mesh is a
    /// valid empty product, not an error.
    pub fn set_map_view(
        &self,
        scale_x: f32,
        scale_y: f32,
        origin: [f32; 2],
    ) -> Result<(), RenderError> {
        let Some(gpu) = self.gpu.as_ref() else {
            return Ok(());
        };
        let Some(mesh) = self.frame.as_ref().and_then(|frame| frame.mesh.as_ref()) else {
            return Ok(());
        };
        gpu.write_view(mesh, scale_x, scale_y, origin);
        Ok(())
    }

    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.frame = None;
        self.queued = None;
        self.state = if self.gpu.is_some() {
            DisplayState::Waiting
        } else {
            DisplayState::Unavailable(RenderError::GpuUnavailable)
        };
    }

    pub fn submit(&mut self, volume: RetrievedVolume, product: RadarProduct) {
        if self.gpu.is_none() {
            return;
        }
        self.product = product;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        if matches!(self.state, DisplayState::Building) {
            self.queued = Some((generation, volume, product));
        } else {
            self.send(generation, volume, product);
        }
    }

    /// Rebuilds the displayed volume for a product or tilt change.
    pub fn rebuild(&mut self, product: RadarProduct, elevation: Option<u8>) -> bool {
        if self.gpu.is_none() {
            return false;
        }
        let Some(mut frame) = self.frame.take() else {
            self.product = product;
            return false;
        };
        let selected_elevation = elevation.or_else(|| {
            (!frame.volume.sweep.products.contains(&product))
                .then(|| frame.volume.elevation_for_product(product))
                .flatten()
        });
        if let Some(number) = selected_elevation
            && (!frame.volume.sweep_has_product(number, product)
                || !frame.volume.select_sweep(number))
        {
            self.frame = Some(frame);
            return false;
        }
        self.product = product;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.send(
            generation,
            RetrievedVolume {
                volume: frame.volume,
                origin: frame.origin,
                history_key: frame.history_key,
            },
            product,
        );
        true
    }

    fn send(&mut self, generation: u64, volume: RetrievedVolume, product: RadarProduct) {
        match self.sender.send(render::BuildRequest {
            volume,
            product,
            color_table: self.color_table,
            generation,
        }) {
            Ok(()) => self.state = DisplayState::Building,
            Err(_) => self.state = DisplayState::Unavailable(RenderError::WorkerStopped),
        }
    }

    pub fn retry(&mut self) {
        let state = std::mem::replace(&mut self.state, DisplayState::Waiting);
        if let DisplayState::Failed { volume, .. } = state {
            self.send(self.generation, *volume, self.product);
        } else {
            self.state = state;
        }
    }

    pub fn poll(&mut self) {
        match self.receiver.try_recv() {
            Ok(result) => self.complete(result),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.state = DisplayState::Unavailable(RenderError::WorkerStopped);
            }
        }
    }

    fn complete(&mut self, result: render::BuildResult) {
        // A queued volume waits for the worker to finish whatever it was doing,
        // including work that this frame has already superseded. Sending it here
        // is what keeps the display from waiting on a build that never starts.
        if let Some((generation, volume, product)) = self.queued.take() {
            self.send(generation, volume, product);
            return;
        }
        if !matches!(self.state, DisplayState::Building) || result.generation != self.generation {
            return;
        }
        if result
            .geometry
            .as_ref()
            .is_ok_and(|geometry| geometry.color_table != self.color_table)
        {
            self.send(self.generation, result.volume, self.product);
            return;
        }
        self.state = match result.geometry {
            Ok(geometry) => match self.upload(&geometry) {
                Ok(mesh) => {
                    self.frame = Some(Frame {
                        volume: result.volume.volume,
                        origin: result.volume.origin,
                        history_key: result.volume.history_key,
                        geometry,
                        mesh,
                    });
                    DisplayState::Ready
                }
                Err(error) => DisplayState::Failed {
                    error,
                    volume: Box::new(result.volume),
                },
            },
            Err(error) => DisplayState::Failed {
                error,
                volume: Box::new(result.volume),
            },
        };
    }

    fn upload(&self, geometry: &RadarGeometry) -> Result<Option<RadarMesh>, RenderError> {
        let Some(gpu) = self.gpu.as_ref() else {
            return Err(RenderError::GpuUnavailable);
        };
        if geometry.index_count == 0 {
            return Ok(None);
        }
        gpu.upload(geometry).map(Some)
    }
}
