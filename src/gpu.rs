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
//! The GPU pipeline that draws a frame's gate raster.
//!
//! The vertex buffer is the frame's lattice: a site-relative kilometre offset
//! and a raster coordinate per corner. One uniform carries the clip-space scale
//! for the volume, so every window size and every zoom level draws the same
//! uploaded vertices. The sampler decides whether the raster is drawn as
//! measured gates or filtered between them.

use eframe::egui::{Rect, Shape};
use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

use crate::error::{CoordinateError, RenderError};
use crate::render::{self, LATTICE_STRIDE, RadarGeometry, Sampling};

const SHADER: &str = r#"
struct Frame {
    scale: vec2<f32>,
    origin: vec2<f32>,
    texels: vec2<f32>,
    smoothing: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var raster: texture_2d<f32>;
@group(0) @binding(2) var raster_sampler: sampler;

const TAPS: f32 = 4.0;

fn texel_at(coord: vec2<f32>) -> vec4<f32> {
    let sample = textureSampleLevel(raster, raster_sampler, coord, 0.0);
    return vec4<f32>(sample.rgb * sample.a, sample.a);
}

struct VertexInput {
    @location(0) offset_km: vec2<f32>,
    @location(1) texcoord: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) texcoord: vec2<f32>,
};

@vertex fn vertex_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>((input.offset_km + frame.origin) * frame.scale, 0.0, 1.0);
    output.texcoord = input.texcoord;
    return output;
}

@fragment fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // A whole sweep is far denser than the screen: 720 radials and 1,832 gates
    // pressed into a few hundred pixels. One tap per fragment aliases those
    // gates into moire, so the smoothed mode averages the footprint instead.
    let footprint = max(
        length(dpdx(input.texcoord) * frame.texels),
        length(dpdy(input.texcoord) * frame.texels),
    );
    if frame.smoothing < 0.5 || footprint <= 1.0 {
        return texel_at(input.texcoord);
    }
    // Average only the taps that carry data. Averaging empty gates in as
    // transparent would fade an isolated echo to nothing at every zoom level
    // where one gate covers a fraction of a pixel.
    let spacing = min(footprint, 8.0) / TAPS / frame.texels;
    var total = vec3<f32>(0.0);
    var covered = 0.0;
    for (var row = 0; row < 4; row = row + 1) {
        for (var column = 0; column < 4; column = column + 1) {
            let shift = vec2<f32>(f32(column), f32(row)) - (TAPS - 1.0) * 0.5;
            let tap = texel_at(input.texcoord + shift * spacing);
            if (tap.a > 0.5) {
                total += tap.rgb;
                covered += 1.0;
            }
        }
    }
    if covered < 0.5 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(total / covered, 1.0);
}
"#;

#[derive(Clone)]
pub struct RadarGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
}

/// Uploaded raster, lattice, and transform for one frame.
pub struct RadarMesh {
    pipeline: wgpu::RenderPipeline,
    frame_bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    /// Kept alive with its bind group so another mesh cannot overwrite its scale.
    _uniform: wgpu::Buffer,
    /// Kept alive by the bind group that samples it.
    _texture: wgpu::Texture,
}

struct RadarDraw {
    pipeline: wgpu::RenderPipeline,
    frame_bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

/// Clip-space scale and map-space origin for a mesh whose vertices carry
/// north-positive kilometre offsets. WebGPU clip space has y up, so both axes
/// keep their sign; the screen transform in `render` applies the pixel flip.
fn uniform_bytes(scale_x: f32, scale_y: f32, origin_x: f32, origin_y: f32) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    for (chunk, value) in bytes
        .chunks_mut(4)
        .zip([scale_x, scale_y, origin_x, origin_y])
    {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// The full frame uniform: the view transform, the raster size, and whether
/// the raster is smoothed.
const UNIFORM_BYTES: usize = 32;

fn frame_uniform(scale: f32, texels: [f32; 2], smoothing: bool) -> [u8; UNIFORM_BYTES] {
    let mut bytes = [0_u8; UNIFORM_BYTES];
    let values = [
        scale,
        scale,
        0.0,
        0.0,
        texels[0],
        texels[1],
        if smoothing { 1.0 } else { 0.0 },
        0.0,
    ];
    for (chunk, value) in bytes.chunks_mut(4).zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn sampler(device: &wgpu::Device, label: &str, filtering: wgpu::FilterMode) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: filtering,
        min_filter: filtering,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..wgpu::SamplerDescriptor::default()
    })
}

impl RadarGpu {
    pub fn new(state: &egui_wgpu::RenderState) -> Self {
        let device = state.device.clone();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("radar shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 8,
                shader_location: 1,
            },
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("radar pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: LATTICE_STRIDE as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: state.target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let nearest = sampler(&device, "radar nearest sampler", wgpu::FilterMode::Nearest);
        let linear = sampler(&device, "radar linear sampler", wgpu::FilterMode::Linear);
        Self {
            device,
            queue: state.queue.clone(),
            pipeline,
            nearest,
            linear,
        }
    }

    /// Uploads one frame's raster and lattice.
    ///
    /// # Errors
    ///
    /// [`RenderError::MeshSize`] when the frame has nothing to draw.
    pub fn upload(
        &self,
        geometry: &RadarGeometry,
        sampling: Sampling,
    ) -> Result<RadarMesh, RenderError> {
        if geometry.lattice.is_empty() || geometry.lattice.vertices.is_empty() {
            return Err(RenderError::MeshSize { vertices: 0 });
        }
        let scale = render::projection_scale(geometry.extent_km)
            .map_err(|source: CoordinateError| RenderError::Coordinate { source })?;
        let uniform = frame_uniform(
            scale as f32,
            [geometry.raster.width as f32, geometry.raster.height as f32],
            matches!(sampling, Sampling::Bilinear),
        );
        let vertices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar lattice"),
                contents: &geometry.lattice.vertices,
                usage: wgpu::BufferUsages::VERTEX,
            });
        let indices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar lattice indices"),
                contents: &geometry.lattice.indices,
                usage: wgpu::BufferUsages::INDEX,
            });
        let frame_uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar frame uniform"),
                contents: &uniform,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let texture = self.upload_texture(geometry)?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let frame_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("radar frame bind group"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(match sampling {
                        Sampling::Nearest => &self.nearest,
                        Sampling::Bilinear => &self.linear,
                    }),
                },
            ],
        });
        self.queue.write_buffer(&frame_uniform, 0, &uniform);
        Ok(RadarMesh {
            pipeline: self.pipeline.clone(),
            frame_bind_group,
            vertices,
            indices,
            index_count: geometry.lattice.index_count,
            _uniform: frame_uniform,
            _texture: texture,
        })
    }

    fn upload_texture(&self, geometry: &RadarGeometry) -> Result<wgpu::Texture, RenderError> {
        let raster = &geometry.raster;
        if raster.width == 0 || raster.height == 0 {
            return Err(RenderError::MeshSize { vertices: 0 });
        }
        let size = wgpu::Extent3d {
            width: raster.width,
            height: raster.height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("radar raster"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let pixels = padded_rows(raster)?;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels.bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pixels.row_bytes),
                rows_per_image: Some(raster.height),
            },
            size,
        );
        Ok(texture)
    }

    /// Places an uploaded mesh in a geographic viewport. The origin is the
    /// station offset from the viewport centre in kilometres, north-positive.
    pub fn write_view(&self, mesh: &RadarMesh, scale_x: f32, scale_y: f32, origin: [f32; 2]) {
        self.queue.write_buffer(
            &mesh._uniform,
            0,
            &uniform_bytes(scale_x, scale_y, origin[0], origin[1]),
        );
    }
}

/// A texture write whose rows each start on the transfer alignment wgpu
/// requires, with the unused tail of every row zeroed.
struct PaddedRows {
    bytes: Vec<u8>,
    row_bytes: u32,
}

fn padded_rows(raster: &crate::raster::RadarRaster) -> Result<PaddedRows, RenderError> {
    let oversized = || RenderError::MeshSize {
        vertices: raster.texels.len() / 4,
    };
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let unpadded = raster.width.checked_mul(4).ok_or_else(oversized)?;
    let row_bytes = unpadded
        .div_ceil(alignment)
        .checked_mul(alignment)
        .ok_or_else(oversized)?;
    let total = row_bytes.checked_mul(raster.height).ok_or_else(oversized)?;
    let (Ok(unpadded), Ok(row_bytes), Ok(total)) = (
        usize::try_from(unpadded),
        usize::try_from(row_bytes),
        usize::try_from(total),
    ) else {
        return Err(oversized());
    };
    let mut bytes = vec![0_u8; total];
    for (row, chunk) in bytes.chunks_exact_mut(row_bytes).enumerate() {
        let start = row.saturating_mul(unpadded);
        if let (Some(source), Some(target)) = (
            raster.texels.get(start..start + unpadded),
            chunk.get_mut(..unpadded),
        ) {
            target.copy_from_slice(source);
        }
    }
    Ok(PaddedRows {
        bytes,
        row_bytes: u32::try_from(row_bytes).map_err(|_| oversized())?,
    })
}

/// A paint callback that draws `mesh` inside `rect`.
pub fn shape(mesh: &RadarMesh, rect: Rect) -> Shape {
    Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        RadarDraw {
            pipeline: mesh.pipeline.clone(),
            frame_bind_group: mesh.frame_bind_group.clone(),
            vertices: mesh.vertices.clone(),
            indices: mesh.indices.clone(),
            index_count: mesh.index_count,
        },
    ))
}

impl egui_wgpu::CallbackTrait for RadarDraw {
    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::uniform_bytes;

    use crate::test_support::TestError;

    fn component(bytes: [u8; 16], offset: usize) -> Result<f32, TestError> {
        let slice = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| format!("uniform byte offset {offset}"))?;
        let value: [u8; 4] = slice
            .try_into()
            .map_err(|_| format!("uniform byte offset {offset}"))?;
        Ok(f32::from_le_bytes(value))
    }

    /// Clip space has y up. A north-positive vertex must therefore keep a
    /// positive y scale; negating it here drew the bins upside down while the
    /// screen-space rings and readout stayed north-up.
    #[test]
    fn clip_space_keeps_north_positive() -> Result<(), TestError> {
        let bytes = uniform_bytes(2.0, 3.0, -40.0, 25.0);
        assert_eq!(component(bytes, 0)?, 2.0, "x scale");
        assert_eq!(component(bytes, 4)?, 3.0, "y scale");
        assert_eq!(component(bytes, 8)?, -40.0, "x origin");
        assert_eq!(component(bytes, 12)?, 25.0, "y origin");
        Ok(())
    }
}
