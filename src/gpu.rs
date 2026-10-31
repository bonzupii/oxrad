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
//! The GPU pipeline that draws the radar mesh.
//!
//! Vertices hold site-relative kilometre offsets. One uniform carries the
//! clip-space scale for the volume, so every window size draws the same mesh.

use eframe::egui::{Rect, Shape};
use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

use crate::error::{CoordinateError, RenderError};
use crate::render::{self, RadarGeometry};

const SHADER: &str = r#"
struct Frame {
    scale: vec2<f32>,
    origin: vec2<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;

struct VertexInput {
    @location(0) offset_km: vec2<f32>,
    @location(1) colour: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex fn vertex_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>((input.offset_km + frame.origin) * frame.scale, 0.0, 1.0);
    output.colour = input.colour;
    return output;
}

@fragment fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.colour;
}
"#;

const VERTEX_STRIDE: u64 = 12;

pub struct RadarGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
}

/// Uploaded buffers and transform for one volume's mesh.
pub struct RadarMesh {
    pipeline: wgpu::RenderPipeline,
    frame_bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    /// Kept alive with its bind group so another mesh cannot overwrite its scale.
    _uniform: wgpu::Buffer,
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
                format: wgpu::VertexFormat::Unorm8x4,
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
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                targets: &[Some(state.target_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            device,
            queue: state.queue.clone(),
            pipeline,
        }
    }

    /// Uploads one volume's mesh.
    pub fn upload(&self, geometry: &RadarGeometry) -> Result<RadarMesh, RenderError> {
        if geometry.index_count == 0 || geometry.vertices.is_empty() || geometry.indices.is_empty()
        {
            return Err(RenderError::MeshSize { vertices: 0 });
        }
        let scale = render::projection_scale(geometry.extent_km)
            .map_err(|source: CoordinateError| RenderError::Coordinate { source })?;
        let uniform = uniform_bytes(scale as f32, scale as f32, 0.0, 0.0);
        let vertices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar vertices"),
                contents: &geometry.vertices,
                usage: wgpu::BufferUsages::VERTEX,
            });
        let indices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar indices"),
                contents: &geometry.indices,
                usage: wgpu::BufferUsages::INDEX,
            });
        let frame_uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar mesh uniform"),
                contents: &uniform,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let frame_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("radar mesh bind group"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_uniform.as_entire_binding(),
            }],
        });
        self.queue.write_buffer(&frame_uniform, 0, &uniform);
        Ok(RadarMesh {
            pipeline: self.pipeline.clone(),
            frame_bind_group,
            vertices,
            indices,
            index_count: geometry.index_count,
            _uniform: frame_uniform,
        })
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
