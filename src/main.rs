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
mod basemap;
mod basemap_tiles;
mod cache;
mod climate;
mod climate_layer;
mod colormap;
mod config;
mod cyclone;
mod cyclone_layer;
mod derived;
mod display;
mod domains;
mod environment;
mod environment_layer;
mod error;
mod fire;
mod fire_layer;
mod forecast;
mod gauge;
mod gauge_layer;
mod global_radar;
mod gpu;
mod history;
mod hydro;
mod hydro_layer;
mod land;
mod land_layer;
mod layer_catalog;
mod map;
mod network;
mod observations;
mod observations_layer;
mod ocean;
mod ocean_layer;
mod overlay;
mod overlay_select;
mod paths;
mod pipeline;
mod place;
mod product;
mod projection;
mod provider;
mod quake;
mod quake_layer;
mod quality;
mod quality_layer;
mod radar;
mod radar_controls;
mod radio;
mod radio_layer;
mod raster;
mod render;
mod retry;
mod satellite;
mod seaice;
mod seaice_layer;
mod snow;
mod snow_layer;
mod sounding;
mod sounding_layer;
mod sources;
mod spaceweather;
mod spaceweather_layer;
mod stations;
mod sun;
#[cfg(test)]
mod test_support;
mod theme;
mod time;
mod units;
mod volcano;
mod volcano_layer;
mod warning_view;
mod warnings;
mod workspaces;

use std::path::PathBuf;
use std::process::ExitCode;

use app::OxradApp;
use stations::SiteId;

const USAGE: &str = "\
Usage: oxrad [--site IDENTIFIER] [--profile PATH] [--set key=value]...
             [--print-config] [--sources]

Runs the radar viewer. The last selected site is restored from the
configuration file; with no saved site the picker opens.

  --site IDENTIFIER   Start with this site instead of the saved one
  --profile PATH      Read this profile instead of ~/.oxrad/config.toml
  --set key=value     Override one setting for this run; repeatable
  --print-config      Print the effective configuration and exit
  --sources           Print the open-source inventory and exit
  -h, --help          Show this message";

#[derive(Default)]
struct Invocation {
    initial: Option<SiteId>,
    profile: Option<PathBuf>,
    selection: Vec<String>,
    print_config: bool,
    print_sources: bool,
    help: bool,
}

fn parse_arguments() -> Result<Invocation, String> {
    let mut invocation = Invocation::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--site" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--site needs an identifier such as KTWX."))?;
                let site = SiteId::parse(&value).map_err(|error| error.to_string())?;
                invocation.selection.push(format!("site={}", site.as_str()));
                invocation.initial = Some(site);
            }
            "--profile" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--profile needs a path."))?;
                invocation.profile = Some(PathBuf::from(value));
            }
            "--set" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--set needs a setting such as units=imperial."))?;
                invocation.selection.push(value);
            }
            "--print-config" => invocation.print_config = true,
            "--sources" => invocation.print_sources = true,
            "--help" | "-h" => invocation.help = true,
            unexpected => return Err(format!("Unexpected argument {unexpected}.")),
        }
    }
    Ok(invocation)
}

fn print_inventory() -> ExitCode {
    match sources::inventory_markdown() {
        Ok(inventory) => {
            print!("{inventory}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("The source inventory could not be rendered: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let invocation = match parse_arguments() {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    if invocation.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if invocation.print_sources {
        return print_inventory();
    }
    let selection = match config::selection_text(&invocation.selection) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let layered =
        invocation.print_config || invocation.profile.is_some() || !invocation.selection.is_empty();
    let mut resolved = None;
    if layered {
        let effective = match config::load_effective(invocation.profile.as_deref(), &selection) {
            Ok(effective) => effective,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };
        if invocation.print_config {
            match effective.render() {
                Ok(report) => {
                    print!("{report}");
                    return ExitCode::SUCCESS;
                }
                Err(error) => {
                    eprintln!("The configuration report could not be rendered: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        resolved = Some(effective.config);
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
            Ok(Box::new(OxradApp::new(gpu, invocation.initial, resolved)))
        }),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("oxrad could not start: {error}");
            ExitCode::FAILURE
        }
    }
}
