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
//! Recent NEXRAD archive volumes: listing, review and playback. The panel owns
//! the history feed; it drives the radar display through explicit calls.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::acquire::Acquire;
use crate::canvas::{due_after, request_report};
use crate::engine::SourceRecord;
use crate::global_panel::GlobalPlayback;
use crate::history::{self, HistoryError, HistoryEvent, HistoryFeed};
use crate::radar::{ArchiveOrigin, RetrievedVolume};
use crate::radar_panel::RadarPanel;
use crate::request_report::{ReportedRequest, RequestReport};
use crate::stations::SiteId;
use crate::theme;

const HISTORY_FRAME_MS: u64 = 900;
const BUSY_REPAINT_MS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryState {
    Idle,
    LoadingKeys,
    Ready,
    LoadingVolume,
    Reviewing,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryStart {
    Oldest,
    Current,
}

#[derive(Debug, Default)]
pub(crate) struct HistoryActions {
    pub(crate) return_to_live: bool,
    pub(crate) switch_to_radar: bool,
}

pub(crate) struct HistoryPanel {
    feed: HistoryFeed,
    keys: Vec<String>,
    listing_requests: Vec<SourceRecord>,
    key: Option<String>,
    state: HistoryState,
    error: Option<HistoryError>,
    playback: GlobalPlayback,
    next_frame: Option<Instant>,
    scrub_index: Option<usize>,
}

impl HistoryPanel {
    pub(crate) fn new(acquire: &Acquire) -> Self {
        Self {
            feed: HistoryFeed::new(acquire.clone()),
            keys: Vec::new(),
            listing_requests: Vec::new(),
            key: None,
            state: HistoryState::Idle,
            error: None,
            playback: GlobalPlayback::Stopped,
            next_frame: None,
            scrub_index: None,
        }
    }

    pub(crate) const fn state(&self) -> HistoryState {
        self.state
    }

    pub(crate) const fn is_playing(&self) -> bool {
        matches!(self.playback, GlobalPlayback::Playing)
    }

    pub(crate) const fn is_reviewing(&self) -> bool {
        matches!(
            self.state,
            HistoryState::LoadingVolume | HistoryState::Reviewing
        )
    }

    pub(crate) const fn is_loading(&self) -> bool {
        matches!(
            self.state,
            HistoryState::LoadingKeys | HistoryState::LoadingVolume
        )
    }

    pub(crate) fn clear(&mut self) {
        self.feed.cancel();
        self.keys.clear();
        self.listing_requests.clear();
        self.key = None;
        self.state = HistoryState::Idle;
        self.error = None;
        self.playback = GlobalPlayback::Stopped;
        self.next_frame = None;
        self.scrub_index = None;
    }

    pub(crate) fn update(&mut self, radar: &mut RadarPanel, notice: &mut Option<String>) {
        loop {
            let event = match self.feed.poll() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    if self.error.is_none() {
                        self.failure(error, notice);
                    }
                    return;
                }
            };
            self.apply_event(event, radar, notice);
        }
    }

    pub(crate) fn advance(
        &mut self,
        site: Option<&SiteId>,
        radar: &mut RadarPanel,
        notice: &mut Option<String>,
    ) {
        self.advance_playback(site, radar, notice);
    }

    /// Applies keys without a worker. The list is the same event the feed
    /// delivers, so tests can drive the state machine deterministically.
    #[cfg(test)]
    pub(crate) fn accept_keys(&mut self, keys: Vec<String>) {
        self.accept_listing(keys, Vec::new());
    }

    pub(crate) fn accept_listing(&mut self, keys: Vec<String>, requests: Vec<SourceRecord>) {
        self.keys = keys;
        self.listing_requests = requests;
        self.state = HistoryState::Ready;
    }

    fn apply_event(
        &mut self,
        event: HistoryEvent,
        radar: &mut RadarPanel,
        notice: &mut Option<String>,
    ) {
        match event {
            HistoryEvent::Keys { keys, requests, .. } => self.accept_listing(keys, requests),
            HistoryEvent::Volume {
                key,
                volume,
                from_cache,
                cache_warning,
                ..
            } => {
                let mut volume = volume;
                for record in &self.listing_requests {
                    let label = record
                        .label
                        .clone()
                        .unwrap_or_else(|| String::from("discovery listing"));
                    volume = volume.with_labelled_request(
                        label,
                        record.provenance.clone(),
                        record.cache,
                    );
                }
                self.key = Some(key.clone());
                self.state = HistoryState::Reviewing;
                self.scrub_index = None;
                let product = radar.product();
                radar.display_mut().submit(
                    RetrievedVolume {
                        volume,
                        origin: ArchiveOrigin::from_cache(from_cache),
                        history_key: Some(key),
                    },
                    product,
                );
                *notice = cache_warning.map(|warning| format!("NEXRAD cache: {warning}"));
                if self.playback == GlobalPlayback::Playing {
                    self.next_frame = Some(due_after(Duration::from_millis(HISTORY_FRAME_MS)));
                } else {
                    self.next_frame = None;
                }
            }
            HistoryEvent::Failed {
                error,
                cache_warning,
                ..
            } => {
                self.failure(error, notice);
                if let Some(warning) = cache_warning {
                    let failure = notice
                        .take()
                        .unwrap_or_else(|| String::from("Radar history failed."));
                    *notice = Some(format!("{failure} Cache warning: {warning}."));
                }
            }
        }
    }

    fn failure(&mut self, error: HistoryError, notice: &mut Option<String>) {
        *notice = Some(format!("Radar history: {error}"));
        self.error = Some(error);
        self.state = HistoryState::Failed;
        self.playback = GlobalPlayback::Stopped;
        self.next_frame = None;
        self.scrub_index = None;
    }

    fn request_keys(&mut self, site: &SiteId, notice: &mut Option<String>) {
        self.keys.clear();
        self.key = None;
        self.error = None;
        match self.feed.request_keys(site) {
            Ok(()) => self.state = HistoryState::LoadingKeys,
            Err(error) => self.failure(error, notice),
        }
    }

    fn request_volume(
        &mut self,
        site: &SiteId,
        key: &str,
        radar: &mut RadarPanel,
        notice: &mut Option<String>,
    ) -> bool {
        radar.pause_live();
        self.key = Some(key.to_owned());
        self.error = None;
        self.next_frame = None;
        match self.feed.request_volume(site, key) {
            Ok(()) => {
                self.state = HistoryState::LoadingVolume;
                true
            }
            Err(error) => {
                self.failure(error, notice);
                false
            }
        }
    }

    fn start_playback(
        &mut self,
        start: HistoryStart,
        site: &SiteId,
        radar: &mut RadarPanel,
        notice: &mut Option<String>,
    ) -> bool {
        let key = match start {
            HistoryStart::Oldest => history::next_key(&self.keys, None),
            HistoryStart::Current => {
                let current = self
                    .key
                    .as_deref()
                    .filter(|key| self.keys.iter().any(|item| item == *key));
                current
                    .map(String::from)
                    .or_else(|| history::next_key(&self.keys, None))
            }
        };
        let Some(key) = key else {
            *notice = Some(String::from(
                "Load the recent scan list before playing radar history.",
            ));
            return false;
        };
        self.playback = GlobalPlayback::Playing;
        self.next_frame = None;
        self.request_volume(site, &key, radar, notice)
    }

    fn toggle_playback(
        &mut self,
        site: &SiteId,
        radar: &mut RadarPanel,
        notice: &mut Option<String>,
    ) -> bool {
        if self.playback == GlobalPlayback::Playing {
            self.playback = GlobalPlayback::Stopped;
            self.next_frame = None;
            false
        } else {
            self.start_playback(HistoryStart::Current, site, radar, notice)
        }
    }

    fn advance_playback(
        &mut self,
        site: Option<&SiteId>,
        radar: &mut RadarPanel,
        notice: &mut Option<String>,
    ) {
        if self.playback != GlobalPlayback::Playing {
            return;
        }
        if !self.next_frame.is_some_and(|due| Instant::now() >= due) {
            return;
        }
        if radar.display().state.error().is_some() {
            self.playback = GlobalPlayback::Stopped;
            self.next_frame = None;
            *notice = Some(String::from(
                "History playback stopped because the current frame could not be prepared.",
            ));
            return;
        }
        if radar.display().state.is_busy() {
            self.next_frame = Some(due_after(Duration::from_millis(BUSY_REPAINT_MS)));
            return;
        }
        let Some(key) = history::next_key(&self.keys, self.key.as_deref()) else {
            self.playback = GlobalPlayback::Stopped;
            self.next_frame = None;
            return;
        };
        self.next_frame = None;
        if let Some(site) = site {
            self.request_volume(site, &key, radar, notice);
        }
    }

    /// The recent scans section of the rail.
    #[expect(
        clippy::too_many_lines,
        reason = "the section is one state machine over the history feed; splitting it would scatter the states"
    )]
    pub(crate) fn section(
        &mut self,
        ui: &mut egui::Ui,
        site: Option<&SiteId>,
        radar: &mut RadarPanel,
        notice: &mut Option<String>,
    ) -> HistoryActions {
        let mut actions = HistoryActions::default();
        let Some(site) = site else {
            return actions;
        };
        ui.add_space(theme::SPACE_M);
        ui.label(
            RichText::new("Recent NEXRAD scans")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new("Archive II volumes · scan times in UTC")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let mut load = false;
        let mut selected = None;
        let mut retry = None;
        let mut resume = false;
        let mut play_all = false;
        let mut play_toggle = false;
        match self.state {
            HistoryState::Idle => {
                ui.label(
                    RichText::new("Load the latest scans without leaving the live feed.")
                        .size(theme::TYPE_SMALL)
                        .weak(),
                );
                if ui.button("Load recent scans").clicked() {
                    load = true;
                }
            }
            HistoryState::LoadingKeys => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Listing recent volumes…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            HistoryState::Ready => {
                if self.keys.is_empty() {
                    ui.label(
                        RichText::new("No complete volumes were listed in the last three days.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                } else if let Some(key) = self.scan_list(ui) {
                    selected = Some(key);
                }
                if !self.keys.is_empty() && ui.button("Play all scans").clicked() {
                    play_all = true;
                }
                if !self.listing_requests.is_empty() {
                    let requests = self
                        .listing_requests
                        .iter()
                        .map(|record| {
                            ReportedRequest::new(
                                record.label.clone(),
                                record.provenance.clone(),
                                record.cache,
                            )
                        })
                        .collect();
                    let report = RequestReport::new("NEXRAD discovery requests", requests);
                    request_report(ui, &report);
                }
                if ui.small_button("Refresh scan list").clicked() {
                    load = true;
                }
            }
            HistoryState::LoadingVolume => {
                let key = self
                    .key
                    .as_deref()
                    .map_or_else(|| String::from("selected scan"), history::key_label);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Fetching {key}…"))
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            HistoryState::Reviewing => {
                let (toggle, live, scrub) = self.reviewing_ui(ui, radar);
                play_toggle = toggle;
                resume = live;
                if scrub.is_some() {
                    selected = scrub;
                }
            }
            HistoryState::Failed => {
                let (reload, retry_key) = self.failure_controls(ui);
                load |= reload;
                retry = retry_key;
            }
        }
        if play_all {
            actions.switch_to_radar =
                self.start_playback(HistoryStart::Oldest, site, radar, notice);
        }
        if play_toggle {
            actions.switch_to_radar |= self.toggle_playback(site, radar, notice);
        }
        if load {
            self.request_keys(site, notice);
        }
        if let Some(key) = retry {
            actions.switch_to_radar |= self.request_volume(site, &key, radar, notice);
        }
        if let Some(key) = selected {
            self.playback = GlobalPlayback::Stopped;
            self.next_frame = None;
            actions.switch_to_radar |= self.request_volume(site, &key, radar, notice);
        }
        if resume {
            actions.return_to_live = true;
        }
        actions
    }

    fn reviewing_ui(
        &mut self,
        ui: &mut egui::Ui,
        radar: &RadarPanel,
    ) -> (bool, bool, Option<String>) {
        let mut play_toggle = false;
        let mut resume = false;
        let mut scrub = None;
        let key = self
            .key
            .as_deref()
            .map_or_else(|| String::from("selected scan"), history::key_label);
        let displayed = radar
            .display()
            .frame
            .as_ref()
            .and_then(|frame| frame.history_key.as_deref());
        let message = if displayed == self.key.as_deref() && displayed.is_some() {
            format!("Reviewing {key}")
        } else {
            format!("Preparing {key}…")
        };
        ui.label(RichText::new(message).size(theme::TYPE_SMALL));
        ui.label(
            RichText::new("Live radar is paused while you inspect this scan.")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        if let Some(current) = self
            .key
            .as_deref()
            .and_then(|key| self.keys.iter().position(|item| item == key))
        {
            let last = self.keys.len().saturating_sub(1);
            let mut requested = self.scrub_index.unwrap_or(current).min(last);
            let response = ui.add(
                egui::Slider::new(&mut requested, 0..=last)
                    .show_value(false)
                    .text("scan"),
            );
            if response.changed() {
                self.scrub_index = Some(requested);
            }
            if (response.drag_stopped() || response.clicked())
                && let Some(key) = self.keys.get(requested)
            {
                scrub = Some(key.clone());
                self.scrub_index = None;
            }
        }
        ui.horizontal(|ui| {
            let label = if self.playback == GlobalPlayback::Playing {
                "Pause history"
            } else {
                "Play history"
            };
            if ui.button(label).clicked() {
                play_toggle = true;
            }
            if let Some(index) = self
                .keys
                .iter()
                .position(|item| Some(item.as_str()) == self.key.as_deref())
            {
                ui.label(
                    RichText::new(format!(
                        "Frame {}/{}",
                        index.saturating_add(1),
                        self.keys.len()
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
        });
        if ui.button("Return to live radar").clicked() {
            resume = true;
        }
        (play_toggle, resume, scrub)
    }

    fn scan_list(&self, ui: &mut egui::Ui) -> Option<String> {
        let mut selected = None;
        egui::ScrollArea::vertical()
            .max_height(150.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for key in &self.keys {
                    if ui
                        .add_sized(
                            [ui.available_width(), 32.0],
                            egui::Button::new(
                                RichText::new(history::key_label(key)).size(theme::TYPE_SMALL),
                            )
                            .fill(ui.visuals().faint_bg_color),
                        )
                        .clicked()
                    {
                        selected = Some(key.clone());
                    }
                }
            });
        selected
    }

    fn failure_controls(&self, ui: &mut egui::Ui) -> (bool, Option<String>) {
        let mut load = false;
        let mut retry = None;
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("History failed: {error}"),
            );
        }
        if let Some(key) = &self.key
            && ui.button("Retry selected scan").clicked()
        {
            retry = Some(key.clone());
        }
        if ui.button("Reload scan list").clicked() {
            load = true;
        }
        (load, retry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn listed_scans_are_ready_without_starting_review_or_playback() {
        let acquire = Acquire::new(Config::default().providers);
        let mut panel = HistoryPanel::new(&acquire);
        panel.accept_keys(vec![String::from("KTLX_20260101_000000_V06")]);
        assert_eq!(panel.state(), HistoryState::Ready);
        assert!(!panel.is_reviewing());
        assert!(!panel.is_playing());
        assert!(!panel.is_loading());
    }
}
