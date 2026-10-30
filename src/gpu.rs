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
use std::sync::mpsc::Sender;

use eframe::egui::{self, Shape};
use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

use crate::error::RenderError;
use crate::render::RenderCache;

const SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};
@vertex fn vertex_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    return output;
}
@fragment fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

pub struct RadarGpu {
    device: wgpu::Device,
    pipeline: wgpu::RenderPipeline,
}

struct RadarDraw {
    pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    errors: Sender<u64>,
    generation: u64,
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
                    array_stride: 12,
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
        Self { device, pipeline }
    }

    pub fn upload(
        &self,
        cache: &RenderCache,
        errors: Sender<u64>,
        generation: u64,
    ) -> Result<Shape, RenderError> {
        let vertex_bytes = if cache.vertices.is_empty() {
            &[0_u8; 12][..]
        } else {
            &cache.vertices
        };
        let index_bytes = if cache.indices.is_empty() {
            &[0_u8; 4][..]
        } else {
            &cache.indices
        };
        let vertices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar vertices"),
                contents: vertex_bytes,
                usage: wgpu::BufferUsages::VERTEX,
            });
        let indices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("radar indices"),
                contents: index_bytes,
                usage: wgpu::BufferUsages::INDEX,
            });
        let callback = RadarDraw {
            pipeline: self.pipeline.clone(),
            vertices,
            indices,
            index_count: cache.index_count,
            errors,
            generation,
        };
        Ok(Shape::Callback(egui_wgpu::Callback::new_paint_callback(
            cache.rect, callback,
        )))
    }
}

impl egui_wgpu::CallbackTrait for RadarDraw {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.index_count, 0, 0..1);
        }));
        if outcome.is_err() && self.errors.send(self.generation).is_err() {
            eprintln!("The GPU radar draw failed and the UI is unavailable.");
        }
    }
}
