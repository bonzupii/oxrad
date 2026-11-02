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
//! The information radio: a running account of what the open layers are
//! reporting, built only from data the application already holds.
//!
//! Nothing here fetches, guesses, or fills a gap. A bulletin exists because a
//! record exists, and it carries the provider it came from and the time the
//! record is for. When an open layer has nothing worth saying, it says nothing
//! and the radio is shorter rather than padded.

use crate::cyclone::{self, StormList};
use crate::forecast::Forecast;
use crate::gauge::GaugeSet;
use crate::observations::ObservationSet;
use crate::quake::Quakes;
use crate::quality::AirQuality;
use crate::sounding::Sounding;
use crate::spaceweather::{KpStatus, SpaceWeather};
use crate::time;
use crate::volcano::{Colour, Report};
use crate::warnings::{Severity, Warning};

/// How much attention a bulletin deserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    /// Something to plan around.
    Routine,
    /// Something that changes what you do next.
    Notable,
    /// Something that can hurt you now.
    Critical,
}

impl Urgency {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::Notable => "notable",
            Self::Critical => "take action",
        }
    }
}

/// One thing the radio can say.
#[derive(Debug, Clone, PartialEq)]
pub struct Bulletin {
    /// Who reported it, in the words the rail uses for that provider.
    pub source: &'static str,
    /// The time the record is for, when the provider stated one.
    pub at_ms: Option<u64>,
    pub urgency: Urgency,
    pub text: String,
}

/// The records the radio may speak about. A layer that is off contributes
/// nothing, so the radio never reports a provider nobody switched on.
#[derive(Default)]
pub struct Input<'a> {
    pub now_ms: u64,
    /// Whether the system clock could be read, so a bulletin never claims an
    /// age the application cannot actually work out.
    pub clock_read: bool,
    pub place: Option<&'a str>,
    pub warnings: &'a [Warning],
    pub quakes: Option<&'a Quakes>,
    pub stations: Option<&'a ObservationSet>,
    pub forecast: Option<&'a Forecast>,
    pub air: Option<&'a AirQuality>,
    pub space: Option<&'a SpaceWeather>,
    pub sounding: Option<&'a Sounding>,
    pub volcanoes: Option<&'a Report>,
    pub gauges: Option<&'a GaugeSet>,
    pub cyclones: Option<&'a StormList>,
}

/// Builds the radio's running order: everything worth saying, most urgent
/// first and newest first within an urgency.
pub fn bulletins(input: &Input<'_>) -> Vec<Bulletin> {
    let mut sayings = Vec::new();
    warnings(input, &mut sayings);
    quakes(input, &mut sayings);
    stations(input, &mut sayings);
    trends(input, &mut sayings);
    air(input, &mut sayings);
    space(input, &mut sayings);
    volcanoes(input, &mut sayings);
    gauges(input, &mut sayings);
    cyclones(input, &mut sayings);
    column(input, &mut sayings);
    sayings.sort_by(|left, right| {
        right
            .urgency
            .cmp(&left.urgency)
            .then(right.at_ms.cmp(&left.at_ms))
    });
    sayings
}

/// The most serious warnings, one line each.
fn warnings(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let mut ordered: Vec<&Warning> = input.warnings.iter().collect();
    ordered.sort_by_key(|warning| std::cmp::Reverse(rank(warning.severity)));
    for warning in ordered.iter().take(3) {
        let urgency = match warning.severity {
            Severity::Extreme => Urgency::Critical,
            Severity::Severe => Urgency::Notable,
            Severity::Moderate | Severity::Minor | Severity::Unknown => Urgency::Routine,
        };
        let area = area_summary(&warning.area);
        let expiry = match warning.expires.trim() {
            "" => String::from("with no stated expiry"),
            expires => expiry_label(expires),
        };
        sayings.push(Bulletin {
            source: "National Weather Service",
            at_ms: time::parse_iso_utc(&warning.sent),
            urgency,
            text: format!(
                "{} for {area}, {expiry}.",
                warning.event.trim_end_matches('.')
            ),
        });
    }
}

const fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Extreme => 3,
        Severity::Severe => 2,
        Severity::Moderate => 1,
        Severity::Minor | Severity::Unknown => 0,
    }
}

/// The strongest earthquake the feed is holding, when it is worth mentioning.
fn quakes(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(set) = input.quakes else {
        return;
    };
    for quake in set.events.iter().take(2) {
        if quake.magnitude < 4.5 {
            continue;
        }
        let depth = if quake.depth_km < 1.0 {
            String::from("at the surface")
        } else {
            format!("{:.0} km deep", quake.depth_km)
        };
        let tsunami = if quake.tsunami {
            " A tsunami message was issued."
        } else {
            ""
        };
        sayings.push(Bulletin {
            source: "USGS",
            at_ms: Some(quake.time_ms),
            urgency: if quake.magnitude >= 6.5 || quake.tsunami {
                Urgency::Critical
            } else {
                Urgency::Notable
            },
            text: format!(
                "Magnitude {:.1} {} {depth} near {}.{tsunami}",
                quake.magnitude, quake.magnitude_type, quake.place
            ),
        });
    }
}

/// The worst surface observation the feed is holding.
fn stations(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(set) = input.stations else {
        return;
    };
    let newest_gust = set
        .stations
        .iter()
        .filter_map(|station| {
            let gust = station.gust_kt?;
            if gust < 35.0 {
                return None;
            }
            Some((gust, station))
        })
        .max_by(|left, right| left.0.total_cmp(&right.0));
    if let Some((gust, station)) = newest_gust {
        let direction = station.wind_from_deg.map_or_else(
            || String::from("a shifting"),
            |degrees| format!("the {}", compass(degrees)),
        );
        sayings.push(Bulletin {
            source: "Aviation Weather Center",
            at_ms: Some(station.observed_ms),
            urgency: if gust >= 50.0 {
                Urgency::Notable
            } else {
                Urgency::Routine
            },
            text: format!(
                "{} reports gusts to {gust:.0} kt out of {direction}.",
                station.name
            ),
        });
    }
    let fog = set
        .stations
        .iter()
        .filter_map(|station| {
            let visibility = station.visibility_mi?;
            if visibility > 1.0 {
                return None;
            }
            Some((visibility, station))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0));
    if let Some((visibility, station)) = fog {
        sayings.push(Bulletin {
            source: "Aviation Weather Center",
            at_ms: Some(station.observed_ms),
            urgency: Urgency::Notable,
            text: format!("{} is down to {visibility:.1} mi visibility.", station.name),
        });
    }
}

/// What the forecast does to the temperature over the next six hours, and
/// what the surface column says about storms.
fn trends(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    if let Some(forecast) = input.forecast {
        let name = input.place.unwrap_or(&forecast.place.name);
        let start = forecast.current.temperature;
        let later = forecast.hourly.get(5).map(|hour| hour.temperature);
        if let Some(later) = later {
            let change = later - start;
            if change.abs() >= 5.0 {
                sayings.push(Bulletin {
                    source: forecast.provider,
                    at_ms: None,
                    urgency: if change.abs() >= 10.0 {
                        Urgency::Notable
                    } else {
                        Urgency::Routine
                    },
                    text: format!(
                        "{name} is {start:.0} {} now and {later:.0} {} in six hours.",
                        forecast.current.unit, forecast.current.unit
                    ),
                });
            }
        }
        if forecast.current.precipitation > 0.0 {
            sayings.push(Bulletin {
                source: forecast.provider,
                at_ms: None,
                urgency: Urgency::Routine,
                text: format!(
                    "{name} reports {:.1} mm falling now, {}.",
                    forecast.current.precipitation,
                    forecast.current.short.to_lowercase()
                ),
            });
        }
    }
}

/// The air quality index when it leaves the good band.
fn air(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(record) = input.air else {
        return;
    };
    let Some(index) = record.current.us_aqi else {
        return;
    };
    let Some(category) = record.current.category() else {
        return;
    };
    if index <= 50.0 {
        return;
    }
    sayings.push(Bulletin {
        source: "Open-Meteo CAMS",
        at_ms: Some(record.current.time_ms),
        urgency: if index > 150.0 {
            Urgency::Notable
        } else {
            Urgency::Routine
        },
        text: format!(
            "Air quality index {index:.0}, {} at the map centre.",
            category.label()
        ),
    });
}

/// A geomagnetic storm, from the planetary index.
fn space(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(record) = input.space else {
        return;
    };
    let Some(point) = record.latest_observed() else {
        return;
    };
    if point.kp < 5.0 {
        return;
    }
    let measured = match point.status {
        KpStatus::Observed => "measured",
        KpStatus::Estimated => "estimated",
        KpStatus::Predicted => "predicted",
    };
    sayings.push(Bulletin {
        source: "NOAA SWPC",
        at_ms: Some(point.time_ms),
        urgency: if point.kp >= 7.0 {
            Urgency::Notable
        } else {
            Urgency::Routine
        },
        text: format!(
            "Planetary K index {:.0} ({measured}), a {} geomagnetic storm.",
            point.kp,
            storm_grade(point.kp)
        ),
    });
}

/// A volcano an observatory has raised, from the volcanic-activity report.
fn volcanoes(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(report) = input.volcanoes else {
        return;
    };
    let Some(highest) = report.highest() else {
        return;
    };
    if highest == Colour::Yellow {
        return;
    }
    let Some(volcano) = report
        .volcanoes
        .iter()
        .filter(|volcano| volcano.notice.colour == Some(highest))
        .max_by_key(|volcano| volcano.notice.sent_ms)
    else {
        return;
    };
    let code = volcano.notice.colour.map_or_else(
        || volcano.notice.colour_text.to_lowercase(),
        |code| String::from(code.label()),
    );
    sayings.push(Bulletin {
        source: "USGS Volcano Hazards Program",
        at_ms: volcano.notice.sent_ms,
        urgency: if highest >= Colour::Orange {
            Urgency::Notable
        } else {
            Urgency::Routine
        },
        text: format!(
            "{} at {} alert, aviation colour code {code}: {}.",
            volcano.notice.volcano,
            volcano.notice.alert_level.to_lowercase(),
            highest.meaning()
        ),
    });
}

/// The highest river stage in view. This is what the gauge publishes, not a
/// flood threshold: the radio never says a river is flooding on its own.
fn gauges(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(set) = input.gauges else {
        return;
    };
    let highest = set
        .gauges
        .iter()
        .filter_map(|gauge| {
            let reading = gauge.stage.as_ref()?;
            Some((reading, gauge))
        })
        .max_by(|left, right| left.0.value.total_cmp(&right.0.value));
    let Some((reading, gauge)) = highest else {
        return;
    };
    let provisional = if reading.provisional {
        ", provisional"
    } else {
        ""
    };
    sayings.push(Bulletin {
        source: "USGS National Water Information System",
        at_ms: Some(reading.at_ms),
        urgency: Urgency::Routine,
        text: format!(
            "Highest river stage in view {:.2} {}{provisional} at {}.",
            reading.value, reading.unit, gauge.name
        ),
    });
}

/// A tropical cyclone the alert service is carrying, when it is either still in
/// progress or at the highest alert level.
fn cyclones(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(list) = input.cyclones else {
        return;
    };
    let Some(storm) = list
        .cyclones
        .iter()
        .filter(|storm| storm.current || storm.alert == Some(cyclone::Alert::Red))
        .max_by_key(|storm| storm.wind.map_or(0, |wind| wind as i64))
    else {
        return;
    };
    let wind = storm.wind.map_or_else(
        || String::from("no wind published"),
        |wind| format!("maximum wind {wind:.0} km/h"),
    );
    sayings.push(Bulletin {
        source: "GDACS global disaster alerts",
        at_ms: storm.to_ms,
        urgency: if storm.alert == Some(cyclone::Alert::Red) {
            Urgency::Notable
        } else {
            Urgency::Routine
        },
        text: format!(
            "Tropical cyclone {} in {}, {wind}.",
            storm.name,
            storm.country.as_deref().unwrap_or("no country stated")
        ),
    });
}

fn storm_grade(kp: f64) -> &'static str {
    if kp >= 9.0 {
        "G5"
    } else if kp >= 8.0 {
        "G4"
    } else if kp >= 7.0 {
        "G3"
    } else if kp >= 6.0 {
        "G2"
    } else {
        "G1"
    }
}

/// The column's own headline: how much helicity and shear the model has.
fn column(input: &Input<'_>, sayings: &mut Vec<Bulletin>) {
    let Some(record) = input.sounding else {
        return;
    };
    let indices = record.indices();
    let (Some(shear), Some(srh)) = (indices.deep_shear, indices.srh_0_3) else {
        return;
    };
    if shear.1 < 15.0 || srh < 100.0 {
        return;
    }
    sayings.push(Bulletin {
        source: "Open-Meteo forecast",
        at_ms: Some(record.time_ms),
        urgency: if srh >= 250.0 && shear.1 >= 20.0 {
            Urgency::Notable
        } else {
            Urgency::Routine
        },
        text: format!(
            "The column holds {srh:.0} m²/s² of 0–3 km helicity with {:.0} m/s of shear through {:.1} km.",
            shear.1,
            shear.0 / 1000.0
        ),
    });
}

/// The first few places a warning covers, so a bulletin of thirty counties
/// stays a sentence.
fn area_summary(area: &str) -> String {
    let places: Vec<&str> = area
        .split(';')
        .map(str::trim)
        .filter(|place| !place.is_empty())
        .collect();
    match places.split_first() {
        None => String::from("an unnamed area"),
        Some((first, rest)) => {
            if rest.is_empty() {
                return String::from(*first);
            }
            let shown: Vec<&str> = rest.iter().take(2).copied().collect();
            let held = rest.len().saturating_sub(shown.len());
            if held == 0 {
                format!("{first} and {}", shown.join(" and "))
            } else {
                format!("{first}, {}, and {held} more", shown.join(", "))
            }
        }
    }
}

/// The end of a warning in UTC, or the provider's own wording when it cannot
/// be read as a time.
fn expiry_label(text: &str) -> String {
    let Some(ms) = time::parse_iso_utc(text) else {
        return format!("until {text}");
    };
    let Ok(days) = i64::try_from(ms / time::MS_PER_DAY) else {
        return format!("until {text}");
    };
    let (_, month, day) = time::civil_from_days(days);
    let hour = (ms / 3_600_000) % 24;
    let minute = (ms / 60_000) % 60;
    format!("until {month:02}-{day:02} {hour:02}:{minute:02}Z")
}

/// The compass point a direction falls in.
fn compass(degrees: f64) -> String {
    let points = [
        "north",
        "north-northeast",
        "northeast",
        "east-northeast",
        "east",
        "east-southeast",
        "southeast",
        "south-southeast",
        "south",
        "south-southwest",
        "southwest",
        "west-southwest",
        "west",
        "west-northwest",
        "northwest",
        "north-northwest",
    ];
    let turned = degrees.rem_euclid(360.0);
    let index = ((turned / 22.5).round() as usize) % points.len();
    points.get(index).map_or_else(
        || format!("{turned:.0}°"),
        |point| format!("{point} ({turned:.0}°)"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observations::Observation;
    use crate::quake::Quake;

    const NOW: u64 = 1_793_000_000_000;

    fn warning(event: &str, severity: Severity, area: &str) -> Warning {
        Warning {
            id: String::from("urn:test"),
            event: String::from(event),
            severity,
            urgency: crate::warnings::Urgency::Immediate,
            headline: String::new(),
            description: String::new(),
            instruction: None,
            area: String::from(area),
            sent: String::from("2026-11-02T02:00:00-06:00"),
            expires: String::from("2026-11-02T03:00:00-06:00"),
            polygons: Vec::new(),
        }
    }

    fn observation(gust: Option<f64>, visibility: Option<f64>) -> Observation {
        Observation {
            station: String::from("KTWX"),
            name: String::from("Topeka"),
            latitude: 39.07,
            longitude: -95.62,
            elevation_m: Some(320.0),
            observed_ms: NOW - 600_000,
            temperature_c: Some(18.0),
            dewpoint_c: Some(15.0),
            wind_from_deg: Some(170.0),
            wind_speed_kt: Some(20.0),
            gust_kt: gust,
            visibility_mi: visibility,
            altimeter_hpa: Some(1004.0),
            category: crate::observations::FlightCategory::Marginal,
            cover: Some(String::from("BKN025")),
            raw: String::from("KTWX 020250Z 17020G40KT 1/2SM"),
        }
    }

    #[test]
    fn an_urgent_warning_outranks_a_routine_one() {
        let warnings = [
            warning("Heat Advisory", Severity::Minor, "Shawnee"),
            warning("Tornado Warning", Severity::Extreme, "Jefferson"),
        ];
        let input = Input {
            now_ms: NOW,
            warnings: &warnings,
            ..Input::default()
        };
        let said = bulletins(&input);
        let first = said.first().map(|bulletin| bulletin.text.as_str());
        assert!(
            first.is_some_and(|text| text.contains("Tornado")),
            "{first:?}"
        );
        assert_eq!(
            said.first().map(|bulletin| bulletin.urgency),
            Some(Urgency::Critical)
        );
        assert!(said.iter().any(|bulletin| bulletin.text.contains("until")));
    }

    #[test]
    fn a_quiet_region_says_nothing_rather_than_filler() {
        let said = bulletins(&Input {
            now_ms: NOW,
            ..Input::default()
        });
        assert!(said.is_empty(), "{said:?}");
    }

    #[test]
    fn a_strong_gust_is_reported_with_its_station() {
        let set = ObservationSet {
            stations: vec![observation(Some(46.0), Some(10.0))],
            rejected: 0,
        };
        let said = bulletins(&Input {
            now_ms: NOW,
            stations: Some(&set),
            ..Input::default()
        });
        let text = said
            .first()
            .map(|bulletin| bulletin.text.clone())
            .unwrap_or_default();
        assert!(text.contains("Topeka"), "{text}");
        assert!(text.contains("46 kt"), "{text}");
        assert!(text.contains("south"), "{text}");
        assert_eq!(
            said.first().and_then(|bulletin| bulletin.at_ms),
            Some(NOW - 600_000)
        );
    }

    #[test]
    fn a_gust_below_the_threshold_is_not_announced() {
        let set = ObservationSet {
            stations: vec![observation(Some(20.0), Some(10.0))],
            rejected: 0,
        };
        let said = bulletins(&Input {
            now_ms: NOW,
            stations: Some(&set),
            ..Input::default()
        });
        assert!(said.is_empty(), "{said:?}");
    }

    #[test]
    fn a_tsunami_flag_makes_a_quake_critical() {
        let quakes = Quakes {
            window: crate::quake::Window::Day,
            generated_ms: NOW,
            title: String::from("test"),
            events: vec![Quake {
                id: String::from("us1"),
                magnitude: 6.8,
                magnitude_type: String::from("mww"),
                place: String::from("120 km SW of Somewhere"),
                time_ms: NOW - 1_800_000,
                depth_km: 12.0,
                lat: 0.0,
                lon: 0.0,
                tsunami: true,
                alert: None,
                status: String::from("reviewed"),
                felt: None,
                url: String::new(),
            }],
            unreadable: 0,
        };
        let said = bulletins(&Input {
            now_ms: NOW,
            quakes: Some(&quakes),
            ..Input::default()
        });
        let bulletin = said.first().cloned();
        assert_eq!(
            bulletin.as_ref().map(|found| found.urgency),
            Some(Urgency::Critical)
        );
        let text = bulletin.map(|found| found.text).unwrap_or_default();
        assert!(text.contains("Magnitude 6.8"), "{text}");
        assert!(text.contains("tsunami"), "{text}");
    }

    #[test]
    fn a_long_area_list_becomes_a_short_sentence() {
        assert_eq!(area_summary(""), "an unnamed area");
        assert_eq!(area_summary("Shawnee"), "Shawnee");
        assert_eq!(
            area_summary("Shawnee; Jefferson; Douglas; Leavenworth"),
            "Shawnee, Jefferson, Douglas, and 1 more"
        );
        assert_eq!(area_summary("Shawnee; Jefferson"), "Shawnee and Jefferson");
    }

    #[test]
    fn a_warning_time_with_an_offset_is_read_back_in_utc() {
        assert_eq!(
            expiry_label("2026-11-02T02:00:00-06:00"),
            "until 11-02 08:00Z"
        );
        assert_eq!(expiry_label("soon"), "until soon");
    }

    #[test]
    fn a_storm_motion_direction_is_named() {
        assert_eq!(compass(170.0), "south (170°)");
        assert_eq!(compass(360.0), "north (0°)");
    }
}
