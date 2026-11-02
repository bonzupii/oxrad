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
//! Which overlay features a view is actually showing: the warnings whose
//! polygons cross the canvas, and the radar sites near its centre.

use eframe::egui::{Pos2, Rect};

use crate::map;
use crate::stations::{SiteId, Station};
use crate::warnings::Warning;

/// Drops a selected warning that the current feed no longer lists, so the
/// detail panel never describes an alert that has expired out of existence.
pub fn retain_warning_selection(selected: &mut Option<String>, warnings: &[Warning]) {
    if selected
        .as_ref()
        .is_some_and(|id| !warnings.iter().any(|warning| &warning.id == id))
    {
        *selected = None;
    }
}

/// Whether any ring of a warning's polygon falls inside the canvas.
pub fn warning_intersects(warning: &Warning, transform: &map::MapTransform, rect: Rect) -> bool {
    warning.polygons.iter().any(|ring| {
        let bounds = ring
            .iter()
            .filter_map(|(lat, lon)| transform.screen(*lat, *lon))
            .fold(None, |bounds: Option<Rect>, point| {
                Some(bounds.map_or_else(
                    || Rect::from_min_max(point, point),
                    |bounds| {
                        Rect::from_min_max(
                            Pos2::new(bounds.min.x.min(point.x), bounds.min.y.min(point.y)),
                            Pos2::new(bounds.max.x.max(point.x), bounds.max.y.max(point.y)),
                        )
                    },
                ))
            });
        bounds.is_some_and(|bounds| bounds.intersects(rect))
    })
}

/// The radar sites within a radius of a point, nearest first.
pub fn nearby_station_ids(
    stations: &[Station],
    center_lat: f64,
    center_lon: f64,
    radius_km: f64,
    excluded: Option<&SiteId>,
    limit: usize,
) -> Result<Vec<SiteId>, crate::error::CoordinateError> {
    let mut candidates: Vec<(f64, SiteId)> = Vec::new();
    for station in stations {
        if excluded == Some(&station.id) {
            continue;
        }
        let (distance_km, _) =
            crate::projection::inverse(center_lat, center_lon, station.lat, station.lon)?;
        if distance_km <= radius_km {
            candidates.push((distance_km, station.id.clone()));
        }
    }
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
    candidates.truncate(limit);
    Ok(candidates.into_iter().map(|(_, id)| id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_the_feed_dropped_is_cleared() {
        let mut selected = Some(String::from("urn:old"));
        retain_warning_selection(&mut selected, &[]);
        assert!(selected.is_none());
    }

    #[test]
    fn a_selection_the_feed_still_lists_is_kept() {
        let mut selected = Some(String::from("urn:kept"));
        let warning = Warning {
            id: String::from("urn:kept"),
            event: String::from("Tornado Warning"),
            severity: crate::warnings::Severity::Extreme,
            urgency: crate::warnings::Urgency::Immediate,
            headline: String::new(),
            description: String::new(),
            instruction: None,
            area: String::new(),
            sent: String::new(),
            expires: String::new(),
            polygons: Vec::new(),
        };
        retain_warning_selection(&mut selected, &[warning]);
        assert_eq!(selected, Some(String::from("urn:kept")));
    }
}
