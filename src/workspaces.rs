// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace state: view, viewport, layer visibility, and measurement.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, Vec2};

use crate::error::CoordinateError;
use crate::map::{MapTransform, MapView};
use crate::place;
use crate::projection;
use crate::stations::SiteId;
use crate::units::{self, UnitSystem};

/// Corners a measurement accepts before the user must start another.
pub const MEASURE_CORNERS_MAX: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerVisibility {
    Shown,
    Hidden,
}

impl LayerVisibility {
    pub const fn is_shown(self) -> bool {
        matches!(self, Self::Shown)
    }

    pub const fn toggled(self) -> Self {
        match self {
            Self::Shown => Self::Hidden,
            Self::Hidden => Self::Shown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeasureMode {
    Off,
    On,
}

impl MeasureMode {
    const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }

    const fn toggled(self) -> Self {
        match self {
            Self::Off => Self::On,
            Self::On => Self::Off,
        }
    }
}

/// A measurement that already holds every corner it accepts.
#[derive(Debug)]
pub struct MeasureFull {
    pub limit: usize,
}

impl Display for MeasureFull {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "A measurement takes at most {} corners. Turn measurement off to start another.",
            self.limit
        )
    }
}

impl Error for MeasureFull {}

/// Colours the measurement outline, corners, and readout use.
#[derive(Debug, Clone, Copy)]
pub struct MeasureColors {
    pub accent: Color32,
    pub text: Color32,
    pub outline: Color32,
}

/// The corner-by-corner geodesic measurement on the map.
#[derive(Debug)]
pub struct Measurement {
    mode: MeasureMode,
    points: Vec<(f64, f64)>,
}

impl Measurement {
    pub const fn new() -> Self {
        Self {
            mode: MeasureMode::Off,
            points: Vec::new(),
        }
    }

    pub const fn is_on(&self) -> bool {
        self.mode.is_on()
    }

    /// Switches the tool and discards the previous corners.
    pub fn toggle(&mut self) {
        self.mode = self.mode.toggled();
        self.points.clear();
    }

    /// Adds one corner, refusing more than the tool draws legibly.
    ///
    /// # Errors
    ///
    /// Returns [`MeasureFull`] when the corner limit is reached.
    pub fn add(&mut self, point: (f64, f64)) -> Result<(), MeasureFull> {
        if self.points.len() >= MEASURE_CORNERS_MAX {
            return Err(MeasureFull {
                limit: MEASURE_CORNERS_MAX,
            });
        }
        self.points.push(point);
        Ok(())
    }

    pub fn readout(&self, units: UnitSystem) -> Result<String, CoordinateError> {
        match self.points.as_slice() {
            [] => Ok(String::from("Click a corner to start measuring")),
            [first] => Ok(format!(
                "Corner 1 {:.4}, {:.4}\nClick a second corner",
                first.0, first.1
            )),
            [first, second] => {
                let (distance_km, bearing_deg) =
                    projection::inverse(first.0, first.1, second.0, second.1)?;
                Ok(format!(
                    "{} · {bearing_deg:.0}° {}\n{:.4}, {:.4}\nClick a third corner for area",
                    units::distance(distance_km, units),
                    place::compass(bearing_deg),
                    first.0,
                    first.1
                ))
            }
            [first, ..] => {
                let (area_km2, perimeter_km) = projection::polygon_area_km2(&self.points)?;
                let mut path_km = 0.0;
                for (from, to) in self.points.iter().zip(self.points.iter().skip(1)) {
                    let (distance_km, _bearing) = projection::inverse(from.0, from.1, to.0, to.1)?;
                    path_km += distance_km;
                }
                Ok(format!(
                    "{} corners · area {}\nperimeter {} · path {}\n{:.4}, {:.4}",
                    self.points.len(),
                    units::area(area_km2, units),
                    units::distance(perimeter_km, units),
                    units::distance(path_km, units),
                    first.0,
                    first.1
                ))
            }
        }
    }

    /// Draws the outline, its corner markers, and the readout panel.
    pub fn draw(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        transform: &MapTransform,
        units: UnitSystem,
        colors: MeasureColors,
    ) {
        let MeasureColors {
            accent,
            text,
            outline,
        } = colors;
        let mut positions: Vec<Pos2> = Vec::with_capacity(self.points.len());
        for (lat, lon) in &self.points {
            match transform.screen(*lat, *lon) {
                Some(position) => positions.push(position),
                None => return,
            }
        }
        let Some(anchor) = positions.first().copied() else {
            return;
        };
        for (from, to) in positions.iter().zip(positions.iter().skip(1)) {
            painter.line_segment([*from, *to], Stroke::new(2.0_f32, accent));
        }
        if positions.len() > 2
            && let (Some(first), Some(end)) = (positions.first(), positions.last())
        {
            painter.line_segment([*end, *first], Stroke::new(1.5_f32, outline));
        }
        for (index, position) in positions.iter().enumerate() {
            painter.circle_filled(*position, 4.5, accent);
            painter.text(
                *position + Vec2::new(8.0, 8.0),
                Align2::LEFT_TOP,
                (index + 1).to_string(),
                FontId::monospace(11.0),
                text,
            );
        }
        let readout = match self.readout(units) {
            Ok(readout) => readout,
            Err(error) => format!("Measurement unavailable: {error}"),
        };
        let galley = painter.layout_no_wrap(readout, FontId::monospace(10.0), text);
        let padding = Vec2::splat(6.0);
        let size = galley.size() + 2.0 * padding;
        let origin = Pos2::new(
            (anchor.x + 12.0).clamp(
                rect.left() + 4.0,
                (rect.right() - size.x - 4.0).max(rect.left() + 4.0),
            ),
            (anchor.y - size.y - 12.0).clamp(
                rect.top() + 4.0,
                (rect.bottom() - size.y - 4.0).max(rect.top() + 4.0),
            ),
        );
        painter.rect_filled(
            Rect::from_min_size(origin, size),
            CornerRadius::same(4),
            Color32::from_black_alpha(220),
        );
        painter.galley(origin + padding, galley, text);
    }
}

/// Geographic workspace state: view, viewport, overlays, and reload timing.
pub struct MapWorkspace {
    pub view: MapView,
    pub viewport: Option<Rect>,
    pub radar: LayerVisibility,
    pub reload_due: Option<Instant>,
    pub measure: Measurement,
}

impl MapWorkspace {
    pub fn new() -> Self {
        Self {
            view: MapView::default(),
            viewport: None,
            radar: LayerVisibility::Shown,
            reload_due: None,
            measure: Measurement::new(),
        }
    }

    pub fn schedule_reload(&mut self, after: Duration) {
        self.reload_due = Instant::now().checked_add(after);
    }

    /// Takes the pending reload when its deadline has passed.
    pub fn take_due_reload(&mut self, now: Instant) -> bool {
        if self.reload_due.is_some_and(|due| now >= due) {
            self.reload_due = None;
            return true;
        }
        false
    }
}

/// Radar workspace state: its own view, fitted independently of the map.
pub struct RadarWorkspace {
    pub view: MapView,
    pub fitted: Option<SiteId>,
}

impl RadarWorkspace {
    pub fn new() -> Self {
        Self {
            view: MapView::default(),
            fitted: None,
        }
    }

    /// Centres the radar view on a station once per station selection.
    ///
    /// # Errors
    ///
    /// Returns [`crate::map::MapError`] when the station cannot be framed.
    pub fn fit(
        &mut self,
        site: &SiteId,
        latitude: f64,
        longitude: f64,
        span_km: f64,
    ) -> Result<(), crate::map::MapError> {
        if self.fitted.as_ref() == Some(site) {
            return Ok(());
        }
        self.view = MapView::focus(latitude, longitude, span_km)?;
        self.fitted = Some(site.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn a_measurement_accepts_corners_up_to_the_limit() -> Result<(), TestError> {
        let mut measure = Measurement::new();
        measure.toggle();
        assert!(measure.is_on(), "the tool must switch on");
        for _ in 0..MEASURE_CORNERS_MAX {
            measure
                .add((39.0, -96.0))
                .map_err(|error| error.to_string())?;
        }
        match measure.add((39.0, -96.0)) {
            Err(full) => assert_eq!(full.limit, MEASURE_CORNERS_MAX),
            Ok(()) => return Err(TestError::from("the corner limit was not enforced")),
        }
        Ok(())
    }

    #[test]
    fn two_corners_report_geodesic_distance() -> Result<(), TestError> {
        let mut measure = Measurement::new();
        measure.toggle();
        measure
            .add((39.0, -96.0))
            .map_err(|error| error.to_string())?;
        measure
            .add((39.0, -95.0))
            .map_err(|error| error.to_string())?;
        let readout = measure.readout(UnitSystem::Metric)?;
        assert!(readout.contains("km"), "unexpected readout: {readout}");
        assert!(
            readout.contains("Click a third"),
            "the next step is missing"
        );
        Ok(())
    }

    #[test]
    fn switching_the_tool_off_discards_the_corners() -> Result<(), TestError> {
        let mut measure = Measurement::new();
        measure.toggle();
        measure
            .add((39.0, -96.0))
            .map_err(|error| error.to_string())?;
        measure.toggle();
        assert!(!measure.is_on());
        assert_eq!(
            measure.readout(UnitSystem::Metric)?,
            "Click a corner to start measuring"
        );
        Ok(())
    }

    #[test]
    fn the_radar_view_fits_once_per_selection() -> Result<(), TestError> {
        let mut radar = RadarWorkspace::new();
        let site = SiteId::parse("KTWX")?;
        radar.fit(&site, 38.99, -96.23, 600.0)?;
        let fitted = radar.view.center_latlon()?;
        radar.fit(&site, 38.99, -96.23, 1_200.0)?;
        assert_eq!(radar.view.center_latlon()?, fitted);
        assert_eq!(radar.fitted.as_ref(), Some(&site));
        Ok(())
    }
}
