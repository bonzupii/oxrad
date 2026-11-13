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

// Test modules assert their expectations; the production paths keep the
// panic, unwrap and indexing bans. This exemption does not reach the binary.
#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::panic_in_result_fn,
        clippy::unwrap_used,
        clippy::indexing_slicing
    )
)]
mod acquire;
mod app;
mod basemap;
mod basemap_tiles;
mod cache;
mod canvas;
mod climate;
mod climate_layer;
mod colormap;
mod config;
mod cyclone;
mod cyclone_layer;
mod derived;
mod display;
mod domains;
mod engine;
mod environment;
mod environment_layer;
mod error;
mod fire;
mod fire_layer;
mod forecast;
mod forecast_panel;
mod gauge;
mod gauge_layer;
mod global_panel;
mod global_radar;
mod gpu;
mod history;
mod history_panel;
mod hydro;
mod hydro_layer;
mod land;
mod land_layer;
mod layer_catalog;
mod map;
mod map_panel;
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
mod places_panel;
mod power;
mod power_layer;
mod probe;
mod product;
mod projection;
mod provider;
mod quake;
mod quake_layer;
mod quality;
mod quality_layer;
mod radar;
mod radar_controls;
mod radar_panel;
mod radio;
mod radio_layer;
mod raster;
mod render;
mod request_report;
mod retry;
mod satellite;
mod seaice;
mod seaice_layer;
mod shell;
mod snow;
mod snow_layer;
mod sounding;
mod sounding_layer;
mod sources;
mod spaceweather;
mod spaceweather_layer;
mod station_panel;
mod stations;
mod status_bar;
mod sun;
mod terrain;
#[cfg(test)]
mod test_support;
mod text;
mod theme;
mod time;
mod uk_rivers;
mod uk_rivers_layer;
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
             [--print-config | --prune-cache | --sources | --fetch-source ID]
             [--value NAME=VALUE]...

Runs the radar viewer. The last selected site is restored from the
configuration file; with no saved site the picker opens.

  --site IDENTIFIER   Start with this site instead of the saved one
  --profile PATH      Read this profile instead of ~/.oxrad/config.toml
  --set key=value     Override one setting for this run; repeatable
  --print-config      Print the effective configuration and exit
  --prune-cache       Enforce the configured cache budget and exit
  --sources           Print the open-source inventory and exit
  --fetch-source ID   Fetch the source's first endpoint twice, or a composite
                      rail's full request set, parse it with the rail's
                      parser, and print the value, units, station, product,
                      observation time and every request's provenance
  --value NAME=VALUE  Supply one endpoint placeholder to --fetch-source;
                      repeatable
  -h, --help          Show this message";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Action {
    #[default]
    Run,
    PrintConfig,
    PruneCache,
    PrintSources,
    FetchSource,
    Help,
}

#[derive(Default)]
struct Invocation {
    initial: Option<SiteId>,
    profile: Option<PathBuf>,
    selection: Vec<String>,
    action: Action,
    fetch_source: Option<String>,
    fetch_values: Vec<(String, String)>,
}

fn choose(action: &mut Action, chosen: Action) -> Result<(), String> {
    if *action != Action::Run {
        return Err(String::from(
            "only one of --print-config, --prune-cache, --sources, --fetch-source and --help may be given.",
        ));
    }
    *action = chosen;
    Ok(())
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
            "--print-config" => choose(&mut invocation.action, Action::PrintConfig)?,
            "--prune-cache" => choose(&mut invocation.action, Action::PruneCache)?,
            "--sources" => choose(&mut invocation.action, Action::PrintSources)?,
            "--fetch-source" => {
                let value = arguments.next().ok_or_else(|| {
                    String::from(
                        "--fetch-source needs a registry identifier such as usgs-earthquakes.",
                    )
                })?;
                choose(&mut invocation.action, Action::FetchSource)?;
                invocation.fetch_source = Some(value);
            }
            "--value" => {
                let value = arguments.next().ok_or_else(|| {
                    String::from("--value needs a placeholder and value, such as limit=12.")
                })?;
                let (name, value) = value.split_once('=').ok_or_else(|| {
                    String::from("--value needs a name and value separated by =.")
                })?;
                if name.is_empty() || value.is_empty() {
                    return Err(String::from(
                        "--value needs a name and value separated by =.",
                    ));
                }
                if invocation
                    .fetch_values
                    .iter()
                    .any(|(existing, _)| existing == name)
                {
                    return Err(format!("--value names {name} more than once."));
                }
                invocation
                    .fetch_values
                    .push((String::from(name), String::from(value)));
            }
            "--help" | "-h" => choose(&mut invocation.action, Action::Help)?,
            unexpected => return Err(format!("Unexpected argument {unexpected}.")),
        }
    }
    if !invocation.fetch_values.is_empty() && invocation.action != Action::FetchSource {
        return Err(String::from("--value is only read with --fetch-source."));
    }
    Ok(invocation)
}

fn prune_cache(policy: config::CachePolicy) -> ExitCode {
    let store = match cache::Store::default_location() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    match store.prune(policy) {
        Ok(pruned) => {
            println!("cache {}", store.root().display());
            println!(
                "budget {} MiB, {} entries",
                pruned.max_bytes / (1024 * 1024),
                pruned.max_entries
            );
            println!("removed {}", pruned.removed);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Fetches one registry endpoint twice through the engine and reports what it
/// served, so cache state and provenance can be observed without a display.
fn fetch_source(config: &config::Config, id: &str, values: &[(String, String)]) -> ExitCode {
    let Some(source) = sources::find(id) else {
        eprintln!("No registry source is named {id}. Run --sources for the identifiers.");
        return ExitCode::FAILURE;
    };
    let acquire = acquire::Acquire::new(config.providers.clone());
    let Some(operation) = source.endpoints.first() else {
        eprintln!("{} records no endpoint.", source.id.as_str());
        return ExitCode::FAILURE;
    };
    let borrowed: Vec<(&str, &str)> = values
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    if probe::is_composite(source.id.as_str()) {
        return fetch_composite(source, &acquire, &borrowed);
    }
    let spec = match engine::RequestSpec::for_operation(source, operation.operation, &borrowed) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("The request could not be built: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut served = None;
    for pass in 1..=2 {
        match acquire.fetch(&spec) {
            Ok(fetched) => {
                println!(
                    "request {pass}: {} bytes from {} ({})",
                    fetched.body.len(),
                    fetched.cache.label(),
                    source.credit
                );
                served = Some(fetched);
            }
            Err(error) => {
                eprintln!("request {pass} failed: {error}");
                report_provenance(&acquire);
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(served) = served else {
        eprintln!("The request produced no body to parse.");
        report_provenance(&acquire);
        return ExitCode::FAILURE;
    };
    match probe::parse(
        source,
        operation.operation,
        &served.body,
        &borrowed,
        &served.provenance.url,
    ) {
        Ok(report) => println!("{}", report.render()),
        Err(error) => {
            eprintln!("Parse failed: {error}");
            report_provenance(&acquire);
            return ExitCode::FAILURE;
        }
    }
    report_provenance(&acquire);
    ExitCode::SUCCESS
}

/// Drives a composite rail through its full request set, prints parsed values
/// and every request, and writes the raw list to the cache directory.
fn fetch_composite(
    source: &'static sources::Source,
    acquire: &acquire::Acquire,
    values: &[(&str, &str)],
) -> ExitCode {
    let run = match probe::live(source, acquire, values) {
        Ok(run) => run,
        Err(error) => {
            eprintln!("Composite observation failed: {error}");
            report_provenance(acquire);
            return ExitCode::FAILURE;
        }
    };
    println!("{}", run.parsed.render());
    println!();
    println!("{}", run.requests.text());
    let exported = match run.requests.write() {
        Ok(path) => {
            println!("request list: {}", path.display());
            true
        }
        Err(error) => {
            eprintln!("The request list could not be written: {error}");
            false
        }
    };
    report_provenance(acquire);
    if exported {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn report_provenance(acquire: &acquire::Acquire) {
    match serde_json::to_string_pretty(&acquire.provenance()) {
        Ok(report) => println!("{report}"),
        Err(error) => eprintln!("The request provenance could not be rendered: {error}"),
    }
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
    match invocation.action {
        Action::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Action::PrintSources => return print_inventory(),

        Action::Run | Action::PrintConfig | Action::PruneCache | Action::FetchSource => {}
    }
    let selection = match config::selection_text(&invocation.selection) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let layered = invocation.action != Action::Run
        || invocation.profile.is_some()
        || !invocation.selection.is_empty();
    let mut resolved = None;
    if layered {
        let effective = match config::load_effective(invocation.profile.as_deref(), &selection) {
            Ok(effective) => effective,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };
        match invocation.action {
            Action::PrintConfig => match effective.render() {
                Ok(report) => {
                    print!("{report}");
                    return ExitCode::SUCCESS;
                }
                Err(error) => {
                    eprintln!("The configuration report could not be rendered: {error}");
                    return ExitCode::FAILURE;
                }
            },
            Action::PruneCache => return prune_cache(effective.config.cache),
            Action::FetchSource => {
                let Some(id) = invocation.fetch_source.as_deref() else {
                    eprintln!("--fetch-source needs a registry identifier.");
                    return ExitCode::FAILURE;
                };
                return fetch_source(&effective.config, id, &invocation.fetch_values);
            }
            Action::Run | Action::PrintSources | Action::Help => {}
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
