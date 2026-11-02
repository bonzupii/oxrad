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
//! The sea-ice layer: how much ice each hemisphere is carrying this month, how
//! that compares with the 1981-2010 baseline, and where it stands in the record.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, Pos2, Rect, RichText, Stroke};

use crate::app::toggle_button;
use crate::seaice::{self, BASELINE_FROM, BASELINE_TO, SeaIceMonth, SeaIceState, SeaIceView};
use crate::theme::{self, Palette};

/// How long the index stands before it is asked for again. The monthly file
/// changes once a month, so this is generous.
const REFRESH_AFTER: Duration = Duration::from_secs(6 * 3600);
const CHART_HEIGHT: f32 = 96.0;
const YEAR_GUTTER: f32 = 12.0;

/// The sea-ice layer.
pub struct SeaIceLayer {
    view: SeaIceView,
    shown: bool,
    asked_at: Option<Instant>,
    asked_month: Option<u32>,
}

impl SeaIceLayer {
    pub fn new() -> Self {
        Self {
            view: SeaIceView::new(),
            shown: false,
            asked_at: None,
            asked_month: None,
        }
    }

    /// Takes in the worker's answers and keeps the request current.
    pub fn update(&mut self, now: Instant) {
        let retry_due = self.view.update(now);
        if !self.shown {
            return;
        }
        let month = match seaice::current_month() {
            Ok(month) => month,
            Err(_) => return,
        };
        let stale = self
            .asked_at
            .is_some_and(|at| now.duration_since(at) >= REFRESH_AFTER);
        let changed = self.asked_month != Some(month);
        let retry = retry_due && self.view.may_retry();
        if changed || stale || retry || matches!(self.view.state(), SeaIceState::Idle) {
            self.asked_at = Some(Instant::now());
            self.asked_month = Some(month);
            self.view.start(month, self.view.attempts());
        }
    }

    /// The rail section for the layer.
    pub fn rail(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(theme::SPACE_M);
        ui.separator();
        ui.label(RichText::new("Sea ice").size(theme::TYPE_LABEL).weak());
        ui.label(
            RichText::new(format!("{} · both hemispheres", seaice::SOURCE))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
        let label = if self.shown {
            "Sea ice: on"
        } else {
            "Sea ice: off"
        };
        if ui
            .add_sized([150.0, 30.0], toggle_button(palette, label, self.shown))
            .on_hover_text(
                "How much of each polar ocean is covered by ice this month, compared with the 1981-2010 baseline and every year the index covers.",
            )
            .clicked()
        {
            self.shown = !self.shown;
            if !self.shown {
                self.asked_at = None;
                self.asked_month = None;
            }
        }
        if !self.shown {
            ui.label(
                RichText::new("Turn the layer on to read the ice for this month.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
            return;
        }
        self.status(ui);
        let Some(record) = self.view.record() else {
            return;
        };
        for month in [&record.north, &record.south] {
            hemisphere(ui, month, palette);
        }
        chart(ui, &record.north, palette);
    }

    fn status(&self, ui: &mut egui::Ui) {
        match self.view.state() {
            SeaIceState::Idle | SeaIceState::Loading { last: None } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Reading the sea-ice index…")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                });
            }
            SeaIceState::Loading { last: Some(record) } => {
                ui.label(
                    RichText::new(format!(
                        "Refreshing · still showing {} {}",
                        month_name(record.month),
                        record.north.latest().map_or(0, |year| year.year)
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
            SeaIceState::Ready { record } => {
                ui.label(
                    RichText::new(format!(
                        "{} · {} years on record",
                        month_name(record.month),
                        record.north.years.len()
                    ))
                    .size(theme::TYPE_SMALL)
                    .weak(),
                );
            }
            SeaIceState::Failed { error, last, .. } => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Sea ice failed: {error}"),
                );
                if last.is_some() {
                    ui.label(
                        RichText::new("Still showing the last index that arrived.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
                if self.view.retry_waiting() {
                    ui.label(
                        RichText::new("Asking again shortly.")
                            .size(theme::TYPE_SMALL)
                            .weak(),
                    );
                }
            }
        }
    }
}

const fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "an unstated month",
    }
}

/// One hemisphere's numbers for the month.
fn hemisphere(ui: &mut egui::Ui, month: &SeaIceMonth, palette: &Palette) {
    let Some(latest) = month.latest() else {
        return;
    };
    ui.label(
        RichText::new(format!(
            "{} · {}",
            month.hemisphere.label(),
            month_name(month.month)
        ))
        .size(theme::TYPE_SMALL)
        .strong(),
    );
    let extent = format!("{:.2} M km² extent", latest.extent_mkm2);
    ui.label(
        RichText::new(extent)
            .size(theme::TYPE_SMALL)
            .color(palette.text),
    );
    if let Some(area) = latest.area_mkm2 {
        ui.label(
            RichText::new(format!("{:.2} M km² area", area))
                .size(theme::TYPE_SMALL)
                .weak(),
        );
    }
    let anomaly = month.anomaly_mkm2().map(|value| {
        let colour = if value < 0.0 {
            palette.warning
        } else {
            palette.text
        };
        let direction = if value < 0.0 { "below" } else { "above" };
        (
            format!("{value:+.2} M km² {direction} the {BASELINE_FROM}–{BASELINE_TO} mean"),
            colour,
        )
    });
    match anomaly {
        Some((text, colour)) => {
            ui.label(RichText::new(text).size(theme::TYPE_SMALL).color(colour));
        }
        None => {
            ui.label(
                RichText::new("No baseline year in the record to compare against.")
                    .size(theme::TYPE_SMALL)
                    .weak(),
            );
        }
    }
    if let (Some(rank), count) = (month.rank_from_lowest(), month.years.len()) {
        let text = if rank == count {
            format!(
                "{} least, the most ice in {count} years of records",
                ordinal(rank)
            )
        } else {
            format!(
                "{} least ice in {count} years of {} records",
                ordinal(rank),
                month_name(month.month)
            )
        };
        ui.label(RichText::new(text).size(theme::TYPE_SMALL).weak());
    }
    if let Some(smallest) = month.smallest() {
        ui.label(
            RichText::new(format!(
                "Least in the record: {:.2} M km² in {}",
                smallest.extent_mkm2, smallest.year
            ))
            .size(theme::TYPE_SMALL)
            .weak(),
        );
    }
    ui.label(
        RichText::new(format!("Newest year in the file: {}", latest.year))
            .size(theme::TYPE_SMALL)
            .weak(),
    );
}

/// A count written as an ordinal, so a rank reads as "2nd" rather than "2th".
fn ordinal(rank: usize) -> String {
    let suffix = match (rank % 10, rank % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{rank}{suffix}")
}

/// Extent for every year the month covers, with the baseline drawn across it.
fn chart(ui: &mut egui::Ui, month: &SeaIceMonth, palette: &Palette) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, CHART_HEIGHT + YEAR_GUTTER),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::ZERO, palette.raised);
    let plot = Rect::from_min_max(
        Pos2::new(rect.left() + 3.0, rect.top() + 4.0),
        Pos2::new(rect.right() - 3.0, rect.bottom() - YEAR_GUTTER),
    );
    let count = month.years.len();
    if count == 0 {
        return;
    }
    let top = month.axis_top();
    let slot = plot.width() / count as f32;
    let bar_width = (slot * 0.7).max(1.0);
    let mut baseline_y = None;
    for (index, year) in month.years.iter().enumerate() {
        let share = (year.extent_mkm2 / top).clamp(0.0, 1.0) as f32;
        let x = plot.left() + slot * index as f32 + (slot - bar_width) / 2.0;
        let bar = Rect::from_min_max(
            Pos2::new(x, plot.bottom() - plot.height() * share),
            Pos2::new(x + bar_width, plot.bottom()),
        );
        let colour = if (BASELINE_FROM..=BASELINE_TO).contains(&year.year) {
            palette.text.gamma_multiply(0.35)
        } else {
            palette.accent
        };
        painter.rect_filled(bar, egui::CornerRadius::ZERO, colour);
        if month
            .latest()
            .is_some_and(|latest| latest.year == year.year)
        {
            painter.rect_stroke(
                bar,
                egui::CornerRadius::ZERO,
                Stroke::new(1.0_f32, palette.warning),
                egui::StrokeKind::Inside,
            );
        }
    }
    if let Some(baseline) = month.baseline_extent() {
        let share = (baseline / top).clamp(0.0, 1.0) as f32;
        let y = plot.bottom() - plot.height() * share;
        baseline_y = Some(y);
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0_f32, palette.warning.gamma_multiply(0.8)),
        );
    }
    painter.text(
        Pos2::new(plot.left() + 2.0, plot.top()),
        Align2::LEFT_TOP,
        format!("{} · {top:.0} M km²", month.hemisphere.label()),
        FontId::proportional(9.0),
        palette.text.gamma_multiply(0.6),
    );
    if baseline_y.is_some() {
        painter.text(
            Pos2::new(plot.left() + 2.0, plot.top() + 11.0),
            Align2::LEFT_TOP,
            format!("{BASELINE_FROM}–{BASELINE_TO} mean"),
            FontId::proportional(8.0),
            palette.warning.gamma_multiply(0.9),
        );
    }
    for (year, alignment, x) in [
        (month.years.first(), Align2::LEFT_TOP, plot.left()),
        (month.years.last(), Align2::RIGHT_TOP, plot.right()),
    ] {
        let Some(year) = year else {
            continue;
        };
        painter.text(
            Pos2::new(x, plot.bottom() + 1.0),
            alignment,
            year.year.to_string(),
            FontId::proportional(9.0),
            palette.text.gamma_multiply(0.6),
        );
    }
    response.on_hover_text(
        "Ice extent for this calendar month in every year the index covers. The pale bars are the 1981-2010 baseline years, the line is their mean, and the outlined bar is the newest year.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seaice::{Hemisphere, IceYear};

    fn month() -> SeaIceMonth {
        SeaIceMonth {
            hemisphere: Hemisphere::North,
            month: 11,
            years: vec![
                IceYear {
                    year: 1981,
                    extent_mkm2: 9.9,
                    area_mkm2: Some(8.2),
                },
                IceYear {
                    year: 2010,
                    extent_mkm2: 9.1,
                    area_mkm2: Some(7.5),
                },
                IceYear {
                    year: 2025,
                    extent_mkm2: 8.8,
                    area_mkm2: Some(7.4),
                },
            ],
            unreadable: 0,
        }
    }

    #[test]
    fn a_rank_is_written_as_an_ordinal() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(21), "21st");
        assert_eq!(ordinal(23), "23rd");
    }

    #[test]
    fn a_month_names_itself() {
        assert_eq!(month_name(11), "November");
        assert_eq!(month_name(0), "an unstated month");
        assert_eq!((BASELINE_FROM, BASELINE_TO), (1981, 2010));
    }

    #[test]
    fn the_chart_axis_holds_every_year() {
        let top = month().axis_top();
        assert!(top > 9.9, "{top}");
    }
}
