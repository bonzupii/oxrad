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
mod app;
mod colormap;
mod config;
mod display;
mod error;
mod forecast;
mod gpu;
mod network;
mod overlay;
mod paths;
mod pipeline;
mod place;
mod projection;
mod provider;
mod radar;
mod render;
mod retry;
mod stations;
#[cfg(test)]
mod test_support;
mod time;

use std::process::ExitCode;

use app::OxradApp;
use stations::SiteId;

const USAGE: &str = "\
Usage: oxrad [--site IDENTIFIER]

Runs the radar viewer. The last selected site is restored from the
configuration file; with no saved site the picker opens.

  --site IDENTIFIER   Start with this site instead of the saved one
  -h, --help          Show this message";

fn main() -> ExitCode {
    let mut initial = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--site" => match arguments.next() {
                Some(value) => match SiteId::parse(&value) {
                    Ok(site) => initial = Some(site),
                    Err(error) => {
                        eprintln!("{error}");
                        return ExitCode::FAILURE;
                    }
                },
                None => {
                    eprintln!("--site needs an identifier such as KPAH.\n\n{USAGE}");
                    return ExitCode::FAILURE;
                }
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            unexpected => {
                eprintln!("Unexpected argument {unexpected}.\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1040.0, 640.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    match eframe::run_native(
        "oxrad",
        options,
        Box::new(move |creation| {
            let gpu = creation.wgpu_render_state.as_ref().map(gpu::RadarGpu::new);
            Ok(Box::new(OxradApp::new(gpu, initial)))
        }),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("oxrad could not start: {error}");
            ExitCode::FAILURE
        }
    }
}
