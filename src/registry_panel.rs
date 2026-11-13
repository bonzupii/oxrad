// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! One workspace for every admitted registry operation.
//!
//! The catalog comes from the registry, the requests go through the shared job
//! queue and `probe::run_live`, and the same presentation model feeds both the
//! window and the headless renderer. No operation owns its own request loop.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::canvas::request_report;
use crate::dispatch;
use crate::probe::{self, OperationRun, ProbeError};
use crate::sources::{self, Source};
use crate::theme::{self, Palette};

/// One admitted endpoint in the workspace catalog.
#[derive(Debug, Clone, Copy)]
struct Entry {
    source: &'static Source,
    operation: &'static str,
    subject: &'static str,
}

impl Entry {
    fn title(self) -> String {
        format!(
            "{} · {} · {}",
            self.source.name, self.operation, self.subject
        )
    }
}

/// The request state of one operation. The last complete run stays available
/// through a failure, with the failure reported beside it.
#[derive(Debug)]
enum ReadState {
    Idle,
    Loading,
    Failed(String),
}

#[derive(Debug)]
struct Row {
    entry: Entry,
    state: ReadState,
    last: Option<OperationRun>,
}

/// One answer from the shared queue.
struct Update {
    generation: u64,
    index: usize,
    result: Result<OperationRun, ProbeError>,
}

/// What the window and the headless renderer print for one operation.
pub(crate) struct Presentation {
    pub(crate) heading: String,
    pub(crate) domain: String,
    pub(crate) coverage: String,
    pub(crate) status: String,
    pub(crate) station: String,
    pub(crate) product: String,
    pub(crate) observed: String,
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) provenance: Vec<String>,
    pub(crate) unknown: Option<String>,
}

impl Presentation {
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            self.heading.clone(),
            format!("domain: {} · coverage: {}", self.domain, self.coverage),
            format!("status: {}", self.status),
            format!("station: {}", self.station),
            format!("product: {}", self.product),
            format!("observed: {}", self.observed),
        ];
        for (label, value) in &self.fields {
            lines.push(format!("{label}: {value}"));
        }
        if let Some(unknown) = &self.unknown {
            lines.push(format!("unknown: {unknown}"));
        }
        if !self.provenance.is_empty() {
            lines.push(String::from("request provenance"));
            lines.extend(self.provenance.iter().cloned());
        }
        lines
    }
}

/// Every admitted registry operation, requested through one shared queue.
pub(crate) struct RegistryPanel {
    rows: Vec<Row>,
    feed: dispatch::Feed<Update>,
    generation: u64,
    query: String,
    started: bool,
}

impl RegistryPanel {
    pub(crate) fn new(acquire: Acquire) -> Self {
        let mut rows = Vec::new();
        for source in sources::admitted() {
            for endpoint in source.endpoints {
                rows.push(Row {
                    entry: Entry {
                        source,
                        operation: endpoint.operation,
                        subject: endpoint.subject,
                    },
                    state: ReadState::Idle,
                    last: None,
                });
            }
        }
        Self {
            rows,
            feed: dispatch::Feed::new(acquire),
            generation: 0,
            query: String::new(),
            started: false,
        }
    }

    pub(crate) const fn catalog_len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row.state, ReadState::Loading))
    }

    pub(crate) fn failed(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| matches!(row.state, ReadState::Failed(_)))
            .count()
    }

    pub(crate) fn ready(&self) -> usize {
        self.rows.iter().filter(|row| row.last.is_some()).count()
    }

    /// Queues every operation once. A newer sweep supersedes older answers.
    pub(crate) fn request_all(&mut self) {
        self.started = true;
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }
        let generation = self.generation;
        let entries: Vec<(usize, Entry)> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| (index, row.entry))
            .collect();
        for (index, entry) in entries {
            if let Some(row) = self.rows.get_mut(index) {
                row.state = ReadState::Loading;
            }
            let result = self.feed.request(move |acquire| Update {
                generation,
                index,
                result: probe::run_live(entry.source, entry.operation, acquire),
            });
            if result.is_err()
                && let Some(row) = self.rows.get_mut(index)
            {
                row.state = ReadState::Failed(String::from(
                    "The shared job queue stopped before this operation was requested.",
                ));
            }
        }
    }

    pub(crate) fn poll(&mut self) {
        loop {
            match self.feed.poll() {
                Ok(Some(update)) => self.apply(update),
                Ok(None) => return,
                Err(dispatch::Disconnected) => {
                    for row in &mut self.rows {
                        if matches!(row.state, ReadState::Loading) {
                            row.state = ReadState::Failed(String::from(
                                "The shared job queue stopped before this operation answered.",
                            ));
                        }
                    }
                    return;
                }
            }
        }
    }

    fn apply(&mut self, update: Update) {
        if update.generation != self.generation {
            return;
        }
        let Some(row) = self.rows.get_mut(update.index) else {
            return;
        };
        match update.result {
            Ok(run) => {
                row.last = Some(run);
                row.state = ReadState::Idle;
            }
            Err(error) => {
                row.state = ReadState::Failed(error.to_string());
            }
        }
    }

    fn presentation(&self, row: &Row) -> Presentation {
        let mut presentation = Presentation {
            heading: row.entry.title(),
            domain: String::from(row.entry.source.domain.label()),
            coverage: row.entry.source.coverage.label(),
            status: match &row.state {
                ReadState::Idle if row.last.is_some() => String::from("ready"),
                ReadState::Idle => String::from("not requested"),
                ReadState::Loading => String::from("requesting"),
                ReadState::Failed(error) => format!("failed: {error}"),
            },
            station: String::from("not stated by this request"),
            product: String::from("not stated by this request"),
            observed: String::from("not stated by this request"),
            fields: Vec::new(),
            provenance: Vec::new(),
            unknown: None,
        };
        if let Some(run) = &row.last {
            presentation.station = run
                .parsed
                .station
                .clone()
                .unwrap_or_else(|| String::from("not stated by this request"));
            presentation.product = run
                .parsed
                .product
                .clone()
                .unwrap_or_else(|| String::from("not stated by this request"));
            presentation.observed = run
                .parsed
                .observed
                .clone()
                .unwrap_or_else(|| String::from("not stated by this request"));
            if run.parsed.observed.is_none() {
                presentation.unknown = Some(String::from(
                    "the provider states no observation or reference time for this value",
                ));
            }
            presentation.fields = run
                .parsed
                .fields
                .iter()
                .map(|field| (field.label.clone(), field.value.clone()))
                .collect();
            presentation.provenance = run.requests.text().lines().map(String::from).collect();
        }
        presentation
    }

    /// The rail controls for the registry workspace.
    pub(crate) fn rail(&mut self, ui: &mut egui::Ui) {
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Earth systems")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new(format!(
                "{} admitted operations · {} ready · {} failed",
                self.catalog_len(),
                self.ready(),
                self.failed()
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        if ui
            .add_enabled(
                !self.is_loading(),
                egui::Button::new("Request every operation"),
            )
            .clicked()
        {
            self.request_all();
        }
        if self.is_loading() {
            ui.label(
                RichText::new("The shared queue is serving the sweep.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }

    /// The window's Earth-systems workspace.
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        if !self.started {
            self.request_all();
        }
        ui.add_space(theme::SPACE_L);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Earth systems")
                    .size(theme::TYPE_DISPLAY)
                    .strong(),
            );
            ui.label(
                RichText::new(format!(
                    "{} operations · {} ready · {} failed",
                    self.catalog_len(),
                    self.ready(),
                    self.failed()
                ))
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            if ui
                .add_enabled(!self.is_loading(), egui::Button::new("Refresh all"))
                .clicked()
            {
                self.request_all();
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Filter").size(theme::TYPE_SMALL).weak());
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("source, operation, domain or subject")
                    .desired_width(280.0),
            );
        });
        ui.add_space(theme::SPACE_M);
        let query = self.query.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in &self.rows {
                    let presentation = self.presentation(row);
                    if !query.is_empty() {
                        let haystack = format!(
                            "{} {} {}",
                            presentation.heading, presentation.domain, presentation.coverage
                        )
                        .to_lowercase();
                        if !haystack.contains(&query) {
                            continue;
                        }
                    }
                    egui::CollapsingHeader::new(presentation.heading.clone())
                        .id_salt(format!(
                            "{}-{}",
                            row.entry.source.id.as_str(),
                            row.entry.operation
                        ))
                        .show(ui, |ui| {
                            Self::operation_ui(ui, &presentation, row.last.as_ref(), palette);
                        });
                }
            });
    }

    /// Draws the expanded detail shared by every operation row.
    fn operation_ui(
        ui: &mut egui::Ui,
        presentation: &Presentation,
        run: Option<&OperationRun>,
        palette: &Palette,
    ) {
        ui.label(
            RichText::new(format!(
                "{} · {} · {}",
                presentation.domain, presentation.coverage, presentation.status
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
        ui.label(
            RichText::new(format!(
                "Station {} · product {} · observed {}",
                presentation.station, presentation.product, presentation.observed
            ))
            .size(theme::TYPE_SMALL)
            .color(palette.text),
        );
        if let Some(unknown) = &presentation.unknown {
            ui.label(
                RichText::new(format!("Unknown: {unknown}"))
                    .size(theme::TYPE_SMALL)
                    .color(palette.warning),
            );
        }
        for (label, value) in &presentation.fields {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(theme::TYPE_SMALL).weak());
                ui.label(
                    RichText::new(value)
                        .size(theme::TYPE_SMALL)
                        .strong()
                        .color(palette.text),
                );
            });
        }
        if let Some(run) = run {
            request_report(ui, &run.requests);
        }
    }

    /// The headless rendering of the same presentation model.
    pub(crate) fn render_text(&self) -> String {
        let mut lines = Vec::new();
        for row in &self.rows {
            lines.extend(self.presentation(row).lines());
            lines.push(String::new());
        }
        lines.push(format!(
            "registry: {} operations · {} ready · {} failed",
            self.catalog_len(),
            self.ready(),
            self.failed()
        ));
        lines.join("\n")
    }
}

/// Waits for a freshly queued sweep to finish, then returns the panel.
pub(crate) fn render_headless(acquire: Acquire, deadline: Duration) -> (RegistryPanel, bool) {
    let mut panel = RegistryPanel::new(acquire);
    panel.request_all();
    let until = Instant::now() + deadline;
    while panel.is_loading() && Instant::now() < until {
        panel.poll();
        std::thread::sleep(Duration::from_millis(50));
    }
    panel.poll();
    let complete = !panel.is_loading();
    (panel, complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::EnabledProviders;

    #[test]
    fn the_catalog_is_every_admitted_registry_operation() {
        let expected: usize = sources::admitted()
            .map(|source| source.endpoints.len())
            .sum();
        let panel = RegistryPanel::new(Acquire::new(EnabledProviders::all_admitted()));
        assert_eq!(panel.catalog_len(), expected);
        assert_eq!(panel.catalog_len(), 40);
    }

    #[test]
    fn a_new_panel_renders_every_operation_before_any_answer() {
        let panel = RegistryPanel::new(Acquire::new(EnabledProviders::all_admitted()));
        let text = panel.render_text();
        assert!(text.lines().filter(|line| line.contains(" · ")).count() >= 40);
        assert!(text.contains("status: not requested"));
    }
}
