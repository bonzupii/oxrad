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
use std::time::{Duration, Instant};

use eframe::egui::{Rect, Shape};

use crate::colormap::ColorTable;
use crate::error::RenderError;
use crate::gpu::{RadarGpu, RadarMesh};
use crate::product::RadarProduct;
use crate::radar::{ArchiveOrigin, RadarVolume, RetrievedVolume};
use crate::render::{self, RadarGeometry, Sampling};

pub struct Frame {
    pub volume: RadarVolume,
    pub origin: ArchiveOrigin,
    pub history_key: Option<String>,
    pub geometry: RadarGeometry,
    pub mesh: Option<RadarMesh>,
    /// How long the frame took to prepare off the UI thread.
    pub built_in: Duration,
    /// How long this frame took to upload on the UI thread.
    pub uploaded_in: Duration,
}

impl Frame {
    /// The draw callback for this frame at one placement in one canvas.
    pub fn shape(&self, gpu: &RadarGpu, rect: Rect, placement: render::Placement) -> Option<Shape> {
        self.mesh
            .as_ref()
            .map(|mesh| gpu.shape(mesh, rect, placement))
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

fn select_product(
    volume: &mut RadarVolume,
    product: RadarProduct,
    elevation: Option<u8>,
) -> Result<(), RenderError> {
    let number = match elevation {
        Some(number) => number,
        None if volume.sweep.products.contains(&product) => volume.sweep.elevation_number,
        None => match volume.elevation_for_product(product) {
            Some(number) => number,
            None => {
                return Err(RenderError::ProductUnavailable {
                    product,
                    elevation: volume.sweep.elevation_number,
                });
            }
        },
    };
    // A computed product lives on the geometry of the lowest sweep, not in any
    // sweep's product list, so the sweep check applies only to measured
    // moments. The check runs before the sweep is moved: a rejected request
    // must leave the displayed sweep where it was.
    let available = product.is_derived() || volume.sweep_has_product(number, product);
    if !available || !volume.select_sweep(number) {
        return Err(RenderError::ProductUnavailable {
            product,
            elevation: number,
        });
    }
    Ok(())
}

pub struct Display {
    gpu: Option<RadarGpu>,
    sender: Sender<render::BuildRequest>,
    receiver: Receiver<render::BuildResult>,
    pub state: DisplayState,
    pub frame: Option<Frame>,
    queued: Option<(u64, RetrievedVolume, RadarProduct)>,
    in_flight: Option<u64>,
    generation: u64,
    product: RadarProduct,
    elevation: Option<u8>,
    color_table: ColorTable,
    sampling: Sampling,
}

impl Display {
    pub fn new(gpu: Option<RadarGpu>, color_table: ColorTable, sampling: Sampling) -> Self {
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
            in_flight: None,
            generation: 0,
            product: RadarProduct::Reflectivity,
            elevation: None,
            color_table,
            sampling,
        }
    }

    /// Recolors the displayed volume without changing its product or tilt.
    pub fn set_color_table(&mut self, color_table: ColorTable) -> bool {
        self.color_table = color_table;
        if self.in_flight.is_some() {
            return true;
        }
        self.rebuild(self.product, self.elevation)
    }

    /// Resamples the displayed frame without recolouring or re-downloading it.
    pub fn set_sampling(&mut self, sampling: Sampling) -> bool {
        if self.sampling == sampling {
            return false;
        }
        self.sampling = sampling;
        let Some(frame) = self.frame.as_mut() else {
            return false;
        };
        match self.gpu.as_ref() {
            Some(gpu) => match gpu.upload(&frame.geometry, sampling) {
                Ok(mesh) => {
                    frame.mesh = Some(mesh);
                    true
                }
                Err(error) => {
                    self.state = DisplayState::Unavailable(error);
                    false
                }
            },
            None => false,
        }
    }

    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.frame = None;
        self.queued = None;
        self.elevation = None;
        self.state = if self.gpu.is_some() {
            DisplayState::Waiting
        } else {
            DisplayState::Unavailable(RenderError::GpuUnavailable)
        };
    }

    pub fn submit(&mut self, mut volume: RetrievedVolume, product: RadarProduct) {
        if self.gpu.is_none() {
            return;
        }
        self.product = product;
        self.generation = self.generation.wrapping_add(1);
        self.queued = None;
        if let Err(error) = select_product(&mut volume.volume, product, self.elevation) {
            self.state = DisplayState::Failed {
                error,
                volume: Box::new(volume),
            };
            return;
        }
        let generation = self.generation;
        if self.in_flight.is_some() {
            self.queued = Some((generation, volume, product));
            self.state = DisplayState::Building;
        } else {
            self.send(generation, volume, product);
        }
    }

    pub fn selection_volume(&self) -> Option<&RadarVolume> {
        match &self.state {
            DisplayState::Failed { volume, .. } => Some(&volume.volume),
            DisplayState::Waiting
            | DisplayState::Building
            | DisplayState::Ready
            | DisplayState::Unavailable(_) => self.frame.as_ref().map(|frame| &frame.volume),
        }
    }

    /// Rebuilds the displayed volume for a product or tilt change.
    pub fn rebuild(&mut self, product: RadarProduct, elevation: Option<u8>) -> bool {
        if self.gpu.is_none() {
            return false;
        }
        if matches!(self.state, DisplayState::Failed { .. }) {
            return self.rebuild_failed(product, elevation);
        }
        if self.in_flight.is_some() {
            return false;
        }
        let Some(mut frame) = self.frame.take() else {
            return false;
        };
        if let Err(error) = select_product(&mut frame.volume, product, elevation) {
            self.state = DisplayState::Failed {
                error,
                volume: Box::new(RetrievedVolume {
                    volume: frame.volume,
                    origin: frame.origin,
                    history_key: frame.history_key,
                }),
            };
            return false;
        }
        self.product = product;
        self.elevation = elevation;
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
        !matches!(self.state, DisplayState::Unavailable(_))
    }

    fn rebuild_failed(&mut self, product: RadarProduct, elevation: Option<u8>) -> bool {
        let state = std::mem::replace(&mut self.state, DisplayState::Waiting);
        let DisplayState::Failed { mut volume, .. } = state else {
            self.state = state;
            return false;
        };
        if let Err(selection_error) = select_product(&mut volume.volume, product, elevation) {
            self.state = DisplayState::Failed {
                error: selection_error,
                volume,
            };
            return false;
        }
        self.product = product;
        self.elevation = elevation;
        self.submit(*volume, product);
        !matches!(self.state, DisplayState::Unavailable(_))
    }

    fn send(&mut self, generation: u64, volume: RetrievedVolume, product: RadarProduct) {
        match self.sender.send(render::BuildRequest {
            volume,
            product,
            color_table: self.color_table,
            generation,
        }) {
            Ok(()) => {
                self.in_flight = Some(generation);
                self.state = DisplayState::Building;
            }
            Err(_) => self.state = DisplayState::Unavailable(RenderError::WorkerStopped),
        }
    }

    pub fn retry(&mut self) {
        let state = std::mem::replace(&mut self.state, DisplayState::Waiting);
        if let DisplayState::Failed { volume, .. } = state {
            self.submit(*volume, self.product);
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
        if self.in_flight != Some(result.generation) {
            return;
        }
        self.in_flight = None;
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
        let upload_started = Instant::now();
        self.state = match result.geometry {
            Ok(geometry) => match self.upload(&geometry) {
                Ok(mesh) => {
                    let uploaded_in = upload_started.elapsed();
                    self.frame = Some(Frame {
                        volume: result.volume.volume,
                        origin: result.volume.origin,
                        history_key: result.volume.history_key,
                        geometry,
                        mesh,
                        built_in: result.elapsed,
                        uploaded_in,
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
        if geometry.lattice.is_empty() {
            return Ok(None);
        }
        gpu.upload(geometry, self.sampling).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::radar::RadarSweep;
    use crate::stations::SiteId;
    use crate::test_support::TestError;

    fn sweep(number: u8, products: Vec<RadarProduct>) -> RadarSweep {
        RadarSweep {
            elevation_deg: f64::from(number),
            elevation_number: number,
            radials: Vec::new(),
            products,
            below_threshold: 0,
            range_folded: 0,
            max_ground_range_km: 100.0,
        }
    }

    fn retrieved(scan: u64) -> Result<RetrievedVolume, TestError> {
        Ok(RetrievedVolume {
            volume: RadarVolume {
                site: SiteId::parse("KTWX")?,
                site_lat: 38.996_94,
                site_lon: -96.232_49,
                scan_unix_ms: scan,
                sweep: sweep(1, vec![RadarProduct::Reflectivity]),
                sweeps: vec![sweep(2, vec![RadarProduct::Velocity])],
                skipped_radials: 0,
                skipped_non_data: 0,
                moment_error: None,
            },
            origin: ArchiveOrigin::Downloaded,
            history_key: None,
        })
    }

    #[test]
    fn product_selection_uses_a_sweep_that_contains_the_product() -> Result<(), TestError> {
        let mut volume = retrieved(1)?.volume;
        select_product(&mut volume, RadarProduct::Velocity, None)?;
        assert_eq!(volume.sweep.elevation_number, 2);
        select_product(&mut volume, RadarProduct::Velocity, Some(2))?;
        assert_eq!(volume.sweep.elevation_number, 2);
        assert!(matches!(
            select_product(&mut volume, RadarProduct::Velocity, Some(1)),
            Err(RenderError::ProductUnavailable {
                product: RadarProduct::Velocity,
                elevation: 1,
            })
        ));
        assert_eq!(volume.sweep.elevation_number, 2);
        Ok(())
    }

    #[test]
    fn failed_volume_supplies_the_available_product_choices() -> Result<(), TestError> {
        let (sender, _requests) = mpsc::channel();
        let (_results, receiver) = mpsc::channel();
        let display = Display {
            gpu: None,
            sender,
            receiver,
            state: DisplayState::Failed {
                error: RenderError::ProductUnavailable {
                    product: RadarProduct::Velocity,
                    elevation: 1,
                },
                volume: Box::new(retrieved(200)?),
            },
            frame: None,
            queued: None,
            in_flight: None,
            generation: 1,
            product: RadarProduct::Velocity,
            elevation: Some(1),
            color_table: ColorTable::Standard,
            sampling: Sampling::Nearest,
        };
        let volume = display.selection_volume().ok_or("failed volume missing")?;
        assert_eq!(volume.scan_unix_ms, 200);
        assert!(volume.supports_product(RadarProduct::Reflectivity));
        Ok(())
    }

    #[test]
    fn a_palette_change_rebuilds_the_in_flight_volume() -> Result<(), TestError> {
        let (sender, requests) = mpsc::channel();
        let (_results, receiver) = mpsc::channel();
        let mut display = Display {
            gpu: None,
            sender,
            receiver,
            state: DisplayState::Building,
            frame: None,
            queued: None,
            in_flight: Some(1),
            generation: 1,
            product: RadarProduct::Reflectivity,
            elevation: None,
            color_table: ColorTable::Tritanopia,
            sampling: Sampling::Nearest,
        };
        let volume = retrieved(100)?;
        let geometry = render::build_geometry_for(
            &volume.volume,
            RadarProduct::Reflectivity,
            ColorTable::Standard,
        )?;
        display.complete(render::BuildResult {
            volume,
            geometry: Ok(geometry),
            generation: 1,
            elapsed: Duration::from_millis(7),
        });
        let request = requests
            .try_recv()
            .map_err(|error| TestError::from(format!("rebuild was not sent: {error}")))?;
        assert_eq!(request.volume.volume.scan_unix_ms, 100);
        assert_eq!(request.color_table, ColorTable::Tritanopia);
        assert!(matches!(display.state, DisplayState::Building));
        Ok(())
    }

    #[test]
    fn superseded_build_starts_the_latest_queued_volume() -> Result<(), TestError> {
        let (sender, requests) = mpsc::channel();
        let (_results, receiver) = mpsc::channel();
        let mut display = Display {
            gpu: None,
            sender,
            receiver,
            state: DisplayState::Building,
            frame: None,
            queued: Some((2, retrieved(200)?, RadarProduct::Reflectivity)),
            in_flight: Some(1),
            generation: 2,
            product: RadarProduct::Reflectivity,
            elevation: None,
            color_table: ColorTable::Tritanopia,
            sampling: Sampling::Nearest,
        };
        display.complete(render::BuildResult {
            volume: retrieved(100)?,
            geometry: Err(RenderError::WorkerStopped),
            generation: 1,
            elapsed: Duration::from_millis(7),
        });
        let request = requests
            .try_recv()
            .map_err(|error| TestError::from(format!("queued build was not sent: {error}")))?;
        assert_eq!(request.generation, 2);
        assert_eq!(request.volume.volume.scan_unix_ms, 200);
        assert_eq!(request.color_table, ColorTable::Tritanopia);
        assert!(display.frame.is_none());
        assert!(matches!(display.state, DisplayState::Building));
        Ok(())
    }
}
