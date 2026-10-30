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
mod app;
mod colormap;
mod display;
mod error;
mod gpu;
mod network;
mod overlay;
mod pipeline;
mod projection;
mod radar;
mod render;
mod retry;
mod time;

use std::process::ExitCode;

use app::OxradApp;

fn is_site_identifier(site: &str) -> bool {
    site.len() == 4
        && site
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let Some(site) = arguments.next() else {
        eprintln!("Usage: oxrad <site>");
        return ExitCode::FAILURE;
    };
    if !is_site_identifier(&site) {
        eprintln!("Site {site} is not a four-character radar identifier.");
        return ExitCode::FAILURE;
    }
    if let Some(unexpected) = arguments.next() {
        eprintln!("Unexpected argument {unexpected}.");
        return ExitCode::FAILURE;
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([900.0, 900.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    match eframe::run_native(
        "oxrad",
        options,
        Box::new(move |creation| {
            let gpu = creation.wgpu_render_state.as_ref().map(gpu::RadarGpu::new);
            Ok(Box::new(OxradApp::new(site, gpu)))
        }),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("oxrad could not start: {error}");
            ExitCode::FAILURE
        }
    }
}
