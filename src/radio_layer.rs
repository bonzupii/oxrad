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
//! The information radio in the rail: one bulletin at a time, advanced on a
//! fixed cadence while it is playing.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::app::toggle_button;
use crate::radio::{self, Bulletin, Input, Urgency};
use crate::theme::{self, Palette};

/// How long one bulletin stays up while the radio is playing.
const DWELL: Duration = Duration::from_secs(8);

/// The radio's play state and position.
pub struct Radio {
    playing: bool,
    position: usize,
    shown_at: Option<Instant>,
    /// The running order the position belongs to, so a changed report starts
    /// from the top rather than from a stale index.
    signature: u64,
}

impl Radio {
    pub const fn new() -> Self {
        Self {
            playing: false,
            position: 0,
            shown_at: None,
            signature: 0,
        }
    }

    /// Advances the running order while it is playing.
    pub fn update(&mut self, now: Instant, input: &Input<'_>) {
        let said = radio::bulletins(input);
        let signature = fingerprint(&said);
        if signature != self.signature {
            self.signature = signature;
            self.position = 0;
            self.shown_at = Some(now);
            return;
        }
        if !self.playing || said.is_empty() {
            return;
        }
        let due = self
            .shown_at
            .is_some_and(|at| now.duration_since(at) >= DWELL);
        if due {
            self.position = (self.position + 1) % said.len();
            self.shown_at = Some(now);
        }
    }

    /// The rail section for the radio.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette, input: &Input<'_>) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(
            RichText::new("Information radio")
                .size(theme::TYPE_LABEL)
                .weak(),
        );
        ui.label(
            RichText::new("The open layers, read out in turn")
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.playing {
            "Radio: playing"
        } else {
            "Radio: paused"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.playing))
            .on_hover_text(
                "Reads the warnings, observations, earthquakes, forecast, air quality, space weather, and column the open layers are holding, without asking any provider for anything new.",
            )
            .clicked()
        {
            self.playing = !self.playing;
            self.shown_at = Some(Instant::now());
        }
        let said = radio::bulletins(input);
        if said.is_empty() {
            ui.label(
                RichText::new(
                    "Nothing current to report: no open layer is holding a record worth reading.",
                )
                .size(theme::TYPE_SMALL)
                .weak(),
            );
            return;
        }
        let position = self.position.min(said.len() - 1);
        let Some(bulletin) = said.get(position) else {
            return;
        };
        ui.horizontal(|ui| {
            let text = if self.playing { "Next" } else { "Read one" };
            if ui.button(text).clicked() {
                self.position = (position + 1) % said.len();
                self.shown_at = Some(Instant::now());
            }
            ui.label(
                RichText::new(format!("{} of {}", position + 1, said.len()))
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        });
        bulletin_card(ui, bulletin, palette, input);
    }
}

/// One bulletin, with its source, its age, and how urgent it is.
fn bulletin_card(ui: &mut egui::Ui, bulletin: &Bulletin, palette: &Palette, input: &Input<'_>) {
    let now_ms = input.now_ms;
    let frame = egui::Frame::NONE
        .fill(palette.raised)
        .inner_margin(egui::Margin::same(6))
        .corner_radius(egui::CornerRadius::same(3));
    frame.show(ui, |ui| {
        ui.label(
            RichText::new(bulletin.text.as_str())
                .size(theme::TYPE_SMALL)
                .color(urgency_colour(bulletin.urgency, palette)),
        );
        let age = bulletin.at_ms.map_or_else(
            || String::from("no time stated"),
            |at_ms| {
                if !input.clock_read {
                    return String::from("clock unreadable");
                }
                if at_ms > now_ms {
                    return String::from("ahead of the clock");
                }
                let minutes = (now_ms - at_ms) / 60_000;
                if minutes < 60 {
                    format!("{minutes} min old")
                } else {
                    format!("{} h old", minutes / 60)
                }
            },
        );
        ui.label(
            RichText::new(format!(
                "{} · {} · {age}",
                bulletin.source,
                bulletin.urgency.label()
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    });
}

/// The colour a bulletin's urgency is drawn in.
const fn urgency_colour(urgency: Urgency, palette: &Palette) -> egui::Color32 {
    match urgency {
        Urgency::Critical => palette.error,
        Urgency::Notable => palette.warning,
        Urgency::Routine => palette.text,
    }
}

/// A cheap signature of the running order, so the radio notices when the
/// records behind it change.
fn fingerprint(said: &[Bulletin]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for bulletin in said {
        for byte in bulletin.text.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= bulletin.at_ms.unwrap_or_default();
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paused_radio_does_not_advance() {
        let mut radio = Radio::new();
        let input = Input {
            now_ms: 1_793_000_000_000,
            ..Input::default()
        };
        let now = Instant::now();
        radio.update(now, &input);
        assert_eq!(radio.position, 0);
        assert!(!radio.playing);
    }

    #[test]
    fn the_shown_bulletin_changes_its_signature() {
        let first = Bulletin {
            source: "USGS",
            at_ms: Some(1),
            urgency: Urgency::Routine,
            text: String::from("Magnitude 5.0 near Somewhere."),
        };
        let second = Bulletin {
            text: String::from("Magnitude 6.0 near Somewhere."),
            ..first.clone()
        };
        assert_ne!(fingerprint(&[first.clone()]), fingerprint(&[second]));
        assert_eq!(fingerprint(&[first.clone()]), fingerprint(&[first]));
    }
}
