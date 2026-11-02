// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! Warning layer state: acquisition, retry, selection, and display scope.

use std::time::{Duration, Instant};

use crate::overlay_select::retain_warning_selection;
use crate::warnings::{Warning, WarningError, WarningEvent, WarningFeed};
use crate::workspaces::LayerVisibility;

/// How long a successful warning set stays current.
const REFRESH_SECS: u64 = 5 * 60;
/// How long a failed warning request waits before it is tried again.
const RETRY_SECS: u64 = 2 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningState {
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningScope {
    All,
    Viewport,
}

/// Owns the active warning set and the request that produced it.
pub struct WarningView {
    feed: WarningFeed,
    generation: u64,
    pub list: Vec<Warning>,
    pub without_geometry: usize,
    pub rejected: usize,
    pub selected: Option<String>,
    pub layer: LayerVisibility,
    pub scope: WarningScope,
    pub state: WarningState,
    request_due: Option<Instant>,
    refresh_due: Option<Instant>,
}

impl WarningView {
    pub fn new() -> Self {
        Self {
            feed: WarningFeed::new(),
            generation: 0,
            list: Vec::new(),
            without_geometry: 0,
            rejected: 0,
            selected: None,
            layer: LayerVisibility::Shown,
            scope: WarningScope::All,
            state: WarningState::Idle,
            request_due: Some(Instant::now()),
            refresh_due: None,
        }
    }

    /// Asks for the warning set on the next update.
    pub fn retry_now(&mut self) {
        self.request_due = Some(Instant::now());
    }

    /// Advances the feed when the layer is on, returning a notice to display.
    pub fn update(&mut self, active: bool) -> Option<String> {
        let mut notice = None;
        let now = Instant::now();
        let due = self.request_due.is_some_and(|due| now >= due)
            || self.refresh_due.is_some_and(|due| now >= due);
        if active && (matches!(self.state, WarningState::Idle) || due) {
            self.request_due = None;
            notice = self.request();
        }
        if let Some(polled) = self.poll() {
            notice = Some(polled);
        }
        notice
    }

    fn request(&mut self) -> Option<String> {
        let generation = self.generation.wrapping_add(1);
        match self.feed.request(generation) {
            Ok(()) => {
                self.generation = generation;
                self.state = WarningState::Loading;
                None
            }
            Err(error) => Some(self.failure(&error)),
        }
    }

    fn failure(&mut self, error: &WarningError) -> String {
        self.state = WarningState::Failed;
        self.refresh_due = Some(Instant::now() + Duration::from_secs(RETRY_SECS));
        format!("Active warnings are unavailable: {error}")
    }

    fn poll(&mut self) -> Option<String> {
        let mut notice = None;
        loop {
            match self.feed.poll() {
                Ok(Some(event)) => {
                    if let Some(applied) = self.apply(event) {
                        notice = Some(applied);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    notice = Some(self.failure(&error));
                    break;
                }
            }
        }
        notice
    }

    fn apply(&mut self, event: WarningEvent) -> Option<String> {
        match event {
            WarningEvent::Ready { generation, set } if generation == self.generation => {
                let rejected = set.rejected;
                self.list = set.warnings;
                self.without_geometry = set.without_geometry;
                self.rejected = rejected.len();
                retain_warning_selection(&mut self.selected, &self.list);
                self.state = WarningState::Ready;
                self.refresh_due = Some(Instant::now() + Duration::from_secs(REFRESH_SECS));
                rejected.first().map(|message| {
                    format!(
                        "{} active alerts could not be read. First problem: {message}",
                        rejected.len()
                    )
                })
            }
            WarningEvent::Failed { generation, error } if generation == self.generation => {
                Some(self.failure(&error))
            }
            WarningEvent::Ready { .. } | WarningEvent::Failed { .. } => None,
        }
    }

    /// The selected warning, when it is still in the current set.
    pub fn selected_warning(&self) -> Option<&Warning> {
        let id = self.selected.as_deref()?;
        self.list.iter().find(|warning| warning.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;
    use crate::warnings::{Severity, Urgency};

    fn warning(id: &str, event: &str) -> Warning {
        Warning {
            id: String::from(id),
            event: String::from(event),
            severity: Severity::Severe,
            urgency: Urgency::Immediate,
            headline: String::from(event),
            description: String::from("Test warning"),
            instruction: None,
            area: String::from("Test area"),
            sent: String::from("2026-10-31T12:00:00Z"),
            expires: String::from("2026-10-31T13:00:00Z"),
            polygons: Vec::new(),
        }
    }

    #[test]
    fn a_selection_that_left_the_set_is_cleared() -> Result<(), TestError> {
        let mut view = WarningView::new();
        view.list = vec![warning("alert-b", "Updated"), warning("alert-a", "Other")];
        view.selected = Some(String::from("alert-b"));
        assert!(view.selected_warning().is_some());
        view.list = vec![warning("alert-a", "Other")];
        retain_warning_selection(&mut view.selected, &view.list);
        assert!(view.selected.is_none());
        Ok(())
    }
}
