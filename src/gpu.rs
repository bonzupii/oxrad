// oxrad — A storm-chaser-grade NEXRAD weather radar viewer
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
    padding: vec2<f32>,
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
    output.position = vec4<f32>(input.offset_km * frame.scale, 0.0, 1.0);
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
    frame_uniform: wgpu::Buffer,
    frame_bind_group: wgpu::BindGroup,
}

/// Uploaded buffers for one volume's mesh.
pub struct RadarMesh {
    pipeline: wgpu::RenderPipeline,
    frame_bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

struct RadarDraw {
    pipeline: wgpu::RenderPipeline,
    frame_bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

fn uniform_bytes(scale: f32) -> [u8; 16] {
    let values = [scale, -scale, 0.0_f32, 0.0_f32];
    let mut bytes = [0_u8; 16];
    for (chunk, value) in bytes.chunks_mut(4).zip(values) {
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
        let frame_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("radar frame uniform"),
            contents: &uniform_bytes(0.0),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("radar frame bind group"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_uniform.as_entire_binding(),
            }],
        });
        Self {
            device,
            queue: state.queue.clone(),
            pipeline,
            frame_uniform,
            frame_bind_group,
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
        self.queue
            .write_buffer(&self.frame_uniform, 0, &uniform_bytes(scale as f32));
        Ok(RadarMesh {
            pipeline: self.pipeline.clone(),
            frame_bind_group: self.frame_bind_group.clone(),
            vertices,
            indices,
            index_count: geometry.index_count,
        })
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
