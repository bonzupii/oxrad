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
//! The status bar: which radar and moment are on screen, how old the scan is,
//! failures with their retries, and the full diagnostics line.

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::canvas::{ERROR, STALE, STALE_AFTER_MS};
use crate::config::Config;
use crate::display::Frame;
use crate::history;
use crate::history_panel::HistoryPanel;
use crate::radar::ArchiveOrigin;
use crate::radar_panel::RadarPanel;
use crate::station_panel::StationPanel;
use crate::stations::Station;
use crate::theme;
use crate::time;

/// Draws the status bar and applies its retry controls.
pub(crate) fn status(
    ui: &mut egui::Ui,
    config: &Config,
    radar: &mut RadarPanel,
    stations: &StationPanel,
    history: &HistoryPanel,
    engine: (&Acquire, &crate::engine::EngineStatus),
    notice: &mut Option<String>,
) {
    let mut retry_pipeline = false;
    let mut refresh_now = false;
    let failed = radar.failed();
    egui::Panel::bottom("status").show_inside(ui, |ui| {
        ui.style_mut().visuals = config.theme.visuals();
        ui.add_space(6.0);
        refresh_now = identity_row(ui, radar, stations, history, failed);
        retry_pipeline = failure_rows(ui, radar);
        requests_row(ui, engine.0, engine.1, notice);
        if let Some(frame) = &radar.display().frame {
            diagnostics(ui, frame, radar.selected_station(stations));
        }
        ui.add_space(6.0);
    });
    if retry_pipeline || refresh_now {
        radar.retry_pipeline(notice);
    }
}

/// The request engine's cache and provenance line, with an export control.
fn requests_row(
    ui: &mut egui::Ui,
    acquire: &Acquire,
    status: &crate::engine::EngineStatus,
    notice: &mut Option<String>,
) {
    ui.horizontal_wrapped(|ui| {
        let latest = status.latest();
        let summary = latest
            .as_ref()
            .and_then(|status| status.summary.clone())
            .unwrap_or_else(|| String::from("No requests yet · the engine is idle"));
        let requests = latest.as_ref().map_or(0, |status| status.requests);
        ui.label(
            RichText::new(format!("Requests {requests} · {summary}"))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        if ui
            .button("Export provenance")
            .on_hover_text(
                "Write every request's source, station, product, scan time, URL, cache state and attempt count to a JSON file.",
            )
            .clicked()
        {
            match acquire.export_provenance() {
                Ok(path) => {
                    *notice = Some(format!("Request provenance written to {}.", path.display()));
                }
                Err(error) => {
                    *notice = Some(format!("Request provenance could not be written: {error}"));
                }
            }
        }
        if let Some(error) = latest.and_then(|status| status.cache_error) {
            ui.label(
                RichText::new(format!("Cache unavailable: {error}"))
                    .size(theme::TYPE_SMALL)
                    .color(ui.visuals().warn_fg_color),
            );
        }
    });
}

/// The identity and freshness row: which radar, which moment, how old, and
/// the manual refresh. Returns whether a refresh was requested.
fn identity_row(
    ui: &mut egui::Ui,
    radar: &RadarPanel,
    stations: &StationPanel,
    history: &HistoryPanel,
    failed: bool,
) -> bool {
    let mut refresh_now = false;
    let product = radar.product();
    ui.horizontal_wrapped(|ui| {
        match (radar.site(), radar.selected_station(stations)) {
            (Some(site), Some(station)) => {
                ui.strong(format!(
                    "{site} · {} · {} ({})",
                    station.name,
                    product.label(),
                    product.id()
                ));
            }
            (Some(site), None) => {
                ui.strong(format!("{site} · {} ({})", product.label(), product.id()));
            }
            (None, _) => {
                ui.strong("No radar station");
            }
        }
        ui.separator();
        ui.label(radar.phase(history));
        if radar
            .display()
            .frame
            .as_ref()
            .is_some_and(|frame| frame.origin == ArchiveOrigin::Cached)
        {
            ui.label(
                RichText::new("cached volume")
                    .size(theme::TYPE_SMALL)
                    .color(ui.visuals().warn_fg_color),
            );
        }
        if let Some(frame) = &radar.display().frame {
            if let Some(key) = &frame.history_key {
                ui.separator();
                ui.label(
                    RichText::new(format!("History {}", history::key_label(key)))
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
            }
            scan_status(ui, frame, failed);
        }
        if radar.site().is_some() {
            ui.separator();
            let working = radar
                .pipeline()
                .is_some_and(|pipeline| pipeline.state.is_working());
            if ui
                .add_enabled(!working, egui::Button::new("Refresh now"))
                .on_hover_text(
                    "Fetch the newest volume for this site instead of waiting for the next poll.",
                )
                .clicked()
            {
                refresh_now = true;
            }
        }
    });
    refresh_now
}

/// The failure rows of the status bar, with their retry controls. Returns
/// whether a pipeline retry was requested.
fn failure_rows(ui: &mut egui::Ui, radar: &mut RadarPanel) -> bool {
    let mut retry_pipeline = false;
    let mut retry_display = false;
    let site = radar
        .site()
        .map_or("no site", crate::stations::SiteId::as_str);
    if let Some(failure) = radar
        .pipeline()
        .and_then(|pipeline| pipeline.state.failure())
    {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                ERROR,
                format!(
                    "{} failed for {}: {}",
                    failure.operation.description(),
                    site,
                    failure.error
                ),
            );
            if let Some(wait) = failure.retry_in {
                ui.label(format!("Retry in {} s", wait.as_secs()));
            }
            if failure.waiting_for_next_volume {
                ui.label("Waiting for a new volume or a manual retry.");
            }
            retry_pipeline = ui.button("Retry now").clicked();
        });
    }
    if let Some(error) = radar.display().state.error() {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(ERROR, format!("Radar image for {site} failed: {error}"));
            if radar.display().state.retryable() {
                retry_display = ui.button("Retry render").clicked();
            }
        });
    }
    if retry_display {
        radar.retry_display();
    }
    retry_pipeline
}

fn scan_status(ui: &mut egui::Ui, frame: &Frame, failed: bool) {
    let scan = frame.volume.scan_unix_ms;
    match time::format_utc(scan) {
        Ok(text) => {
            ui.label(format!("Scan {text}"));
        }
        Err(error) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Scan time unavailable: {error}"),
            );
        }
    }
    match time::now_unix_ms() {
        Ok(now) if now < scan => {
            ui.colored_label(ui.visuals().error_fg_color, time::age_label(now, scan));
        }
        Ok(now) if failed || now - scan > STALE_AFTER_MS => {
            ui.colored_label(
                STALE,
                format!("Stale radar: {}", time::age_label(now, scan)),
            );
        }
        Ok(now) => {
            ui.label(time::age_label(now, scan));
        }
        Err(error) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Scan age unavailable: {error}"),
            );
        }
    }
}

fn diagnostics(ui: &mut egui::Ui, frame: &Frame, catalog: Option<&Station>) {
    let volume = &frame.volume;
    let product = frame.geometry.product;
    let counts = frame.geometry.raster.counts;
    let radials = volume.sweep.radials.len();
    let max_range = volume.sweep.max_ground_range_km;
    let skipped = volume.skipped_radials;
    let non_data = volume.skipped_non_data;
    let (minimum, maximum) = product.value_range();
    let id = product.id();
    let text = format!(
        "{radials} radials · {id} {minimum} to {maximum} · drawn {} · below threshold {} · range folded {} · below the floor {} · above the scale {} · unreadable {} · {max_range:.0} km · built in {} ms, uploaded in {} ms",
        counts.drawn,
        counts.below_threshold,
        counts.range_folded,
        counts.below_scale,
        counts.above_scale,
        counts.not_finite,
        frame.built_in.as_millis(),
        frame.uploaded_in.as_millis(),
    );
    let text = if counts.mismatched_radials > 0 {
        format!(
            "{text} · {} radials on a different gate spacing",
            counts.mismatched_radials
        )
    } else {
        text
    };
    let text = if skipped > 0 {
        format!("{text} · Skipped radials: {skipped} · Non-data messages: {non_data}")
    } else {
        text
    };
    let catalog_note =
        catalog.map_or_else(String::new, |station| {
            match crate::projection::inverse(
                volume.site_lat,
                volume.site_lon,
                station.lat,
                station.lon,
            ) {
                Ok((distance_km, _)) => format!(" · Catalog vs header: {distance_km:.1} km"),
                Err(error) => format!(" · Catalog/header distance unavailable: {error}"),
            }
        });
    let text = format!("{text}{catalog_note}");
    if volume.skipped_radials > 0 {
        ui.add(
            egui::Label::new(
                RichText::new(text)
                    .size(theme::TYPE_SMALL)
                    .color(ui.visuals().warn_fg_color),
            )
            .wrap(),
        );
    } else {
        ui.add(egui::Label::new(RichText::new(text).size(theme::TYPE_SMALL).weak()).wrap());
    }
    if let Some((product, error)) = &volume.moment_error {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("Rejected {} radial: {error}", product.id()),
        );
    }
}
