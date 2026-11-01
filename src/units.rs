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
//! Measurement units and presentation formatting.
//!
//! Stored values stay canonical: kilometres, metres per second, and degrees
//! Celsius. Conversion happens only where a value becomes text.

const MILES_PER_KM: f64 = 0.621_371_192_237_334;

/// The unit system a user reads values in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnitSystem {
    #[default]
    Metric,
    Imperial,
}

impl UnitSystem {
    /// The control label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Metric => "Metric",
            Self::Imperial => "Imperial",
        }
    }

    /// The configuration token written to disk.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Metric => "metric",
            Self::Imperial => "imperial",
        }
    }

    /// Parses a configuration token.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "metric" => Some(Self::Metric),
            "imperial" => Some(Self::Imperial),
            _ => None,
        }
    }

    pub const fn distance_unit(self) -> &'static str {
        match self {
            Self::Metric => "km",
            Self::Imperial => "mi",
        }
    }

    pub const fn temperature_unit(self) -> &'static str {
        match self {
            Self::Metric => "°C",
            Self::Imperial => "°F",
        }
    }

    /// Converts canonical kilometres to the displayed distance.
    pub fn distance(self, kilometres: f64) -> f64 {
        match self {
            Self::Metric => kilometres,
            Self::Imperial => kilometres * MILES_PER_KM,
        }
    }

    /// Converts canonical degrees Celsius to the displayed temperature.
    pub fn temperature(self, celsius: f64) -> f64 {
        match self {
            Self::Metric => celsius,
            Self::Imperial => celsius * 9.0 / 5.0 + 32.0,
        }
    }

    pub const fn area_unit(self) -> &'static str {
        match self {
            Self::Metric => "km²",
            Self::Imperial => "mi²",
        }
    }

    /// Converts canonical square kilometres to the displayed area.
    pub fn area(self, square_kilometres: f64) -> f64 {
        match self {
            Self::Metric => square_kilometres,
            Self::Imperial => square_kilometres * MILES_PER_KM * MILES_PER_KM,
        }
    }
}

/// A distance with one decimal place.
pub fn distance(kilometres: f64, units: UnitSystem) -> String {
    format!(
        "{:.1} {}",
        units.distance(kilometres),
        units.distance_unit()
    )
}

/// A distance rounded to whole units, used for range-ring labels.
pub fn distance_whole(kilometres: f64, units: UnitSystem) -> String {
    format!(
        "{:.0} {}",
        units.distance(kilometres),
        units.distance_unit()
    )
}

/// An area with enough precision to stay readable from a neighborhood to a
/// county, computed from canonical square kilometres.
pub fn area(square_kilometres: f64, units: UnitSystem) -> String {
    let value = units.area(square_kilometres);
    let precision = if value.abs() < 10.0 {
        2
    } else if value.abs() < 1000.0 {
        1
    } else {
        0
    };
    format!("{value:.precision$} {}", units.area_unit())
}

/// A temperature with one decimal place.
pub fn temperature(celsius: f64, units: UnitSystem) -> String {
    format!(
        "{:.1} {}",
        units.temperature(celsius),
        units.temperature_unit()
    )
}

/// A temperature rounded to whole units, used for headline readings.
pub fn temperature_whole(celsius: f64, units: UnitSystem) -> String {
    format!(
        "{:.0} {}",
        units.temperature(celsius),
        units.temperature_unit()
    )
}

/// Formats a wind speed supplied in kilometres per hour.
pub fn wind_speed(kilometres_per_hour: f64, units: UnitSystem) -> String {
    match units {
        UnitSystem::Metric => format!("{kilometres_per_hour:.0} km/h"),
        UnitSystem::Imperial => format!("{:.0} mph", kilometres_per_hour * MILES_PER_KM),
    }
}

/// Formats a precipitation depth supplied in millimetres.
pub fn precipitation(millimetres: f64, units: UnitSystem) -> String {
    match units {
        UnitSystem::Metric => format!("{millimetres:.1} mm"),
        UnitSystem::Imperial => format!("{:.2} in", millimetres / 25.4),
    }
}

/// Converts a provider temperature to canonical degrees Celsius.
pub fn celsius_from(value: f64, unit: &str) -> Option<f64> {
    match unit.trim() {
        "°C" | "C" => Some(value),
        "°F" | "F" => Some((value - 32.0) * 5.0 / 9.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn metric_keeps_canonical_values() {
        close(UnitSystem::Metric.distance(12.5), 12.5);
        close(UnitSystem::Metric.temperature(-5.0), -5.0);
    }

    #[test]
    fn imperial_converts_distance_and_temperature() {
        close(UnitSystem::Imperial.distance(100.0), 62.137_119_223_733_4);
        close(UnitSystem::Imperial.temperature(0.0), 32.0);
        close(UnitSystem::Imperial.temperature(100.0), 212.0);
        close(UnitSystem::Imperial.temperature(-40.0), -40.0);
    }

    #[test]
    fn formatting_carries_the_selected_unit() {
        assert_eq!(distance(12.44, UnitSystem::Metric), "12.4 km");
        assert_eq!(distance_whole(50.0, UnitSystem::Imperial), "31 mi");
        assert_eq!(temperature(21.0, UnitSystem::Imperial), "69.8 °F");
        assert_eq!(celsius_from(68.0, "°F"), Some(20.0));
        assert_eq!(celsius_from(20.0, "°C"), Some(20.0));
        assert_eq!(celsius_from(20.0, "K"), None);
        assert_eq!(wind_speed(100.0, UnitSystem::Imperial), "62 mph");
        assert_eq!(wind_speed(100.0, UnitSystem::Metric), "100 km/h");
        assert_eq!(precipitation(25.4, UnitSystem::Imperial), "1.00 in");
        assert_eq!(precipitation(25.4, UnitSystem::Metric), "25.4 mm");
        assert_eq!(area(12308.778, UnitSystem::Metric), "12309 km²");
        assert_eq!(area(2.5, UnitSystem::Metric), "2.50 km²");
        assert_eq!(area(64.0, UnitSystem::Imperial), "24.7 mi²");
    }

    #[test]
    fn configuration_tokens_round_trip() {
        for system in [UnitSystem::Metric, UnitSystem::Imperial] {
            assert_eq!(UnitSystem::parse(system.token()), Some(system));
        }
        assert_eq!(UnitSystem::parse("kelvin"), None);
    }
}
