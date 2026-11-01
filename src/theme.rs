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
//! Design tokens: colour, spacing, and type scale for the interface.
//!
//! Every colour the interface draws with comes from one `Palette`, selected by
//! `Theme`. The radar canvas stays dark in both themes because the product
//! colours are calibrated against a dark background; the basemap and all chrome
//! follow the selected theme.

use eframe::egui::{self, Color32, Stroke};

/// Interface spacing scale, in points.
pub const SPACE_XS: f32 = 4.0;
/// Interface spacing scale, in points.
pub const SPACE_S: f32 = 8.0;
/// Interface spacing scale, in points.
pub const SPACE_M: f32 = 12.0;
/// Interface spacing scale, in points.
pub const SPACE_L: f32 = 18.0;
/// Interface spacing scale, in points.
pub const SPACE_XL: f32 = 26.0;

/// Type scale, in points. `TYPE_SMALL` is the minimum interface size.
pub const TYPE_DISPLAY: f32 = 21.0;
/// Type scale, in points.
pub const TYPE_TITLE: f32 = 16.0;
/// Type scale, in points.
pub const TYPE_BODY: f32 = 13.0;
/// Type scale, in points.
pub const TYPE_LABEL: f32 = 12.0;
/// Type scale, in points.
pub const TYPE_SMALL: f32 = 11.0;

/// The selected colour theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Dark chrome around a dark data canvas.
    #[default]
    Dark,
    /// Light chrome around a light basemap.
    Light,
}

impl Theme {
    /// The label shown on the theme control.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    /// The configuration token written to disk.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// Parses a configuration token.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    /// The colours this theme draws with.
    pub const fn palette(self) -> Palette {
        match self {
            Self::Dark => DARK,
            Self::Light => LIGHT,
        }
    }

    /// The egui widget styling for this theme.
    pub fn visuals(self) -> egui::Visuals {
        let palette = self.palette();
        let mut visuals = match self {
            Self::Dark => egui::Visuals::dark(),
            Self::Light => egui::Visuals::light(),
        };
        visuals.panel_fill = palette.background;
        visuals.window_fill = palette.surface;
        visuals.faint_bg_color = palette.raised;
        visuals.extreme_bg_color = palette.raised;
        visuals.selection.bg_fill = palette.accent;
        visuals.selection.stroke = Stroke::new(1.0_f32, palette.text);
        visuals.hyperlink_color = palette.accent;
        visuals.warn_fg_color = palette.warning;
        visuals.error_fg_color = palette.error;
        visuals.window_stroke = Stroke::new(1.0_f32, palette.border);
        let text = Stroke::new(1.0_f32, palette.text);
        visuals.widgets.noninteractive.fg_stroke = text;
        visuals.widgets.noninteractive.bg_fill = palette.surface;
        visuals.widgets.noninteractive.weak_bg_fill = palette.background;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette.border);
        visuals.widgets.inactive.fg_stroke = text;
        visuals.widgets.inactive.bg_fill = palette.raised;
        visuals.widgets.inactive.weak_bg_fill = palette.surface;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette.border);
        visuals.widgets.hovered.fg_stroke = text;
        visuals.widgets.hovered.bg_fill = palette.raised;
        visuals.widgets.hovered.weak_bg_fill = palette.raised;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette.accent);
        visuals.widgets.active.fg_stroke = text;
        visuals.widgets.active.bg_fill = palette.accent;
        visuals.widgets.active.weak_bg_fill = palette.accent;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette.accent);
        visuals
    }
}

/// Every colour the interface, the basemap, and the radar canvas use.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window behind all panels.
    pub background: Color32,
    /// Panel and card surface.
    pub surface: Color32,
    /// Raised surface for inputs and selected controls.
    pub raised: Color32,
    /// Hairline borders and separators.
    pub border: Color32,
    /// Primary text.
    pub text: Color32,
    /// Selection and primary action colour.
    pub accent: Color32,
    /// Fault text.
    pub error: Color32,
    /// Stale or cautionary text.
    pub warning: Color32,
    /// Radar canvas background, dark in both themes.
    pub canvas: Color32,
    /// Ocean fill in map mode.
    pub ocean: Color32,
    /// Land fill in map mode.
    pub land: Color32,
    /// Coastline stroke in map mode.
    pub coast: Color32,
    /// Graticule stroke in map mode.
    pub graticule: Color32,
    /// Range rings and azimuth lines over the dark radar canvas.
    pub ring: Color32,
    /// Text drawn over the dark radar canvas.
    pub canvas_text: Color32,
    /// Backdrop behind the product legend.
    pub legend: Color32,
    /// Map labels and map readouts.
    pub map_text: Color32,
}

const DARK: Palette = Palette {
    background: Color32::from_rgb(8, 10, 14),
    surface: Color32::from_rgb(18, 22, 30),
    raised: Color32::from_rgb(26, 32, 42),
    border: Color32::from_rgb(42, 52, 66),
    text: Color32::from_rgb(214, 221, 232),
    accent: Color32::from_rgb(44, 92, 143),
    error: Color32::from_rgb(255, 122, 122),
    warning: Color32::from_rgb(255, 196, 64),
    canvas: Color32::from_rgb(8, 10, 14),
    ocean: Color32::from_rgb(8, 18, 27),
    land: Color32::from_rgb(26, 39, 35),
    coast: Color32::from_rgb(72, 102, 91),
    graticule: Color32::from_rgb(43, 60, 69),
    ring: Color32::from_rgb(174, 184, 197),
    canvas_text: Color32::from_rgb(206, 214, 226),
    legend: Color32::from_rgb(12, 16, 22),
    map_text: Color32::from_rgb(206, 214, 226),
};

const LIGHT: Palette = Palette {
    background: Color32::from_rgb(242, 245, 248),
    surface: Color32::from_rgb(255, 255, 255),
    raised: Color32::from_rgb(232, 237, 243),
    border: Color32::from_rgb(198, 208, 218),
    text: Color32::from_rgb(20, 28, 37),
    accent: Color32::from_rgb(23, 81, 140),
    error: Color32::from_rgb(163, 35, 27),
    warning: Color32::from_rgb(122, 78, 0),
    canvas: Color32::from_rgb(8, 10, 14),
    ocean: Color32::from_rgb(207, 224, 238),
    land: Color32::from_rgb(231, 239, 228),
    coast: Color32::from_rgb(110, 134, 152),
    graticule: Color32::from_rgb(180, 198, 212),
    ring: Color32::from_rgb(174, 184, 197),
    canvas_text: Color32::from_rgb(206, 214, 226),
    legend: Color32::from_rgb(12, 16, 22),
    map_text: Color32::from_rgb(20, 28, 37),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(colour: Color32) -> f64 {
        fn channel(value: u8) -> f64 {
            let value = f64::from(value) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let first = luminance(a);
        let second = luminance(b);
        let (bright, dark) = if first > second {
            (first, second)
        } else {
            (second, first)
        };
        (bright + 0.05) / (dark + 0.05)
    }

    #[test]
    fn tokens_round_trip_and_reject_unknown_values() {
        for theme in [Theme::Dark, Theme::Light] {
            assert_eq!(Theme::parse(theme.token()), Some(theme));
        }
        assert_eq!(Theme::parse("sepia"), None);
    }

    #[test]
    fn text_meets_aaa_contrast_on_panels() {
        for theme in [Theme::Dark, Theme::Light] {
            let palette = theme.palette();
            for surface in [palette.background, palette.surface, palette.raised] {
                let ratio = contrast(palette.text, surface);
                assert!(
                    ratio >= 7.0,
                    "{} text contrast on a panel is {ratio:.2}",
                    theme.label()
                );
            }
        }
    }
}
