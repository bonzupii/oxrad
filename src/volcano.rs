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
//! Volcanic activity: which volcanoes the observatories have raised, at what
//! alert level and aviation colour code, and what they last said about them.
//!
//! The USGS Volcano Hazards Program publishes one summary per observatory
//! notice. The summary carries the alert level and colour code but not the
//! volcano's position, so each raised volcano is followed up with its own
//! record for latitude, longitude and elevation. A volcano whose record cannot
//! be read is still listed, with its position stated as unpublished, rather
//! than dropped or drawn at a guessed place.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Instant;

use serde_json::{Map, Value};

use crate::acquire::Acquire;
use crate::dispatch;
use crate::engine::Served;
use crate::provider::FetchError;
use crate::sources;
/// The provider, in the words the rail uses.
pub const SOURCE: &str = "USGS Volcano Hazards Program";

/// How many raised volcanoes are followed up for a position, so one busy day
/// cannot turn into an unbounded burst of requests.
pub const MAX_VOLCANOES: usize = 12;

/// The aviation colour code, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Colour {
    Green,
    Yellow,
    Orange,
    Red,
}

impl Colour {
    /// Reads the code the observatory published.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_uppercase().as_str() {
            "GREEN" => Some(Self::Green),
            "YELLOW" => Some(Self::Yellow),
            "ORANGE" => Some(Self::Orange),
            "RED" => Some(Self::Red),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Orange => "orange",
            Self::Red => "red",
        }
    }

    /// What the code means for an aircraft over the volcano.
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Green => "no eruption expected",
            Self::Yellow => "eruption under way but no ash above the summit",
            Self::Orange => "ash possible above the summit",
            Self::Red => "ash above the summit is expected or observed",
        }
    }
}

/// One observatory notice about one volcano.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub volcano: String,
    pub vnum: String,
    pub observatory: String,
    pub colour: Option<Colour>,
    pub colour_text: String,
    pub alert_level: String,
    /// When the notice was sent, milliseconds since the Unix epoch.
    pub sent_ms: Option<u64>,
    /// The USGS identifier of the notice this summary came from.
    pub identifier: String,
}

/// The position and description the observatory publishes per volcano.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub elevation_m: Option<f64>,
    pub region: Option<String>,
    pub url: Option<String>,
    pub summary: Option<String>,
}

/// A raised volcano with everything that could be read about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Volcano {
    pub notice: Notice,
    pub place: Place,
}

impl Volcano {
    /// The position, when the observatory published one.
    pub fn position(&self) -> Option<(f64, f64)> {
        match (self.place.latitude, self.place.longitude) {
            (Some(latitude), Some(longitude))
                if (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude) =>
            {
                Some((latitude, longitude))
            }
            (Some(_) | None, Some(_) | None) => None,
        }
    }
}

/// Every raised volcano the observatory is reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub volcanoes: Vec<Volcano>,
    /// How many raised volcanoes were not followed up because the list was
    /// longer than the cap.
    pub not_followed: usize,
}

impl Report {
    /// The raised volcanoes whose position is known.
    pub fn located(&self) -> impl Iterator<Item = &Volcano> {
        self.volcanoes
            .iter()
            .filter(|volcano| volcano.position().is_some())
    }

    /// How many volcanoes are at each colour code, highest first.
    pub fn by_colour(&self) -> Vec<(Colour, usize)> {
        let mut counts: BTreeMap<Colour, usize> = BTreeMap::new();
        for volcano in &self.volcanoes {
            if let Some(colour) = volcano.notice.colour {
                let entry = counts.entry(colour).or_insert(0);
                *entry += 1;
            }
        }
        let mut rows: Vec<(Colour, usize)> = counts.into_iter().collect();
        rows.reverse();
        rows
    }

    /// The highest colour code in the report.
    pub fn highest(&self) -> Option<Colour> {
        self.volcanoes
            .iter()
            .filter_map(|volcano| volcano.notice.colour)
            .max()
    }

    /// The newest notice in the report.
    pub fn newest(&self) -> Option<&Volcano> {
        self.volcanoes
            .iter()
            .filter(|volcano| volcano.notice.sent_ms.is_some())
            .max_by(|left, right| left.notice.sent_ms.cmp(&right.notice.sent_ms))
    }
}

/// Why a volcano report could not be read.
#[derive(Debug)]
pub enum VolcanoError {
    Provider { source: FetchError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for VolcanoError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The volcanic-activity request failed: {source}")
            }
            Self::Shape { detail } => write!(
                formatter,
                "The volcanic-activity record is not usable: it wants {detail}."
            ),
            Self::WorkerStopped => write!(
                formatter,
                "The volcano worker stopped, so the raised volcanoes cannot be read."
            ),
        }
    }
}

impl Error for VolcanoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl VolcanoError {}

const fn shape(detail: &'static str) -> VolcanoError {
    VolcanoError::Shape { detail }
}

/// The text of a field, when the document carries one.
fn text(row: &Map<String, Value>, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}

/// Reads the raised-volcano summary.
pub fn parse_list(document: &Value) -> Result<Vec<Notice>, VolcanoError> {
    let Some(rows) = document.as_array() else {
        return Err(shape("an array of observatory notices"));
    };
    let mut notices = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(row) = row.as_object() else {
            return Err(shape("an object for every notice"));
        };
        let Some(volcano) = text(row, "volcano_name") else {
            return Err(shape("a name for every notice"));
        };
        let Some(vnum) = text(row, "vnum") else {
            return Err(shape("a volcano number for every notice"));
        };
        let colour_text = text(row, "color_code").unwrap_or_else(|| String::from("not stated"));
        notices.push(Notice {
            volcano,
            vnum,
            observatory: text(row, "obs_fullname")
                .unwrap_or_else(|| String::from("observatory not stated")),
            colour: Colour::parse(&colour_text),
            colour_text,
            alert_level: text(row, "alert_level").unwrap_or_else(|| String::from("not stated")),
            sent_ms: row
                .get("sent_unixtime")
                .and_then(Value::as_u64)
                .and_then(|seconds| seconds.checked_mul(1000)),
            identifier: text(row, "notice_identifier").unwrap_or_default(),
        });
    }
    Ok(notices)
}

/// Reads one volcano's own record.
pub fn parse_volcano(document: &Value) -> Result<Place, VolcanoError> {
    let Some(row) = document.as_object() else {
        return Err(shape("an object for the volcano"));
    };
    let coordinate = |field: &str| {
        row.get(field)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
    };
    Ok(Place {
        latitude: coordinate("latitude"),
        longitude: coordinate("longitude"),
        elevation_m: row
            .get("elevation_meters")
            .and_then(Value::as_f64)
            .filter(|metres| *metres > -500.0),
        region: text(row, "region"),
        url: text(row, "volcano_url"),
        summary: None,
    })
}

/// The one-sentence synopsis the notice carries, as plain text.
pub fn parse_synopsis(document: &Value) -> Option<String> {
    let row = document.as_object()?;
    let html = row.get("notice_html").and_then(Value::as_str)?;
    let marker = html.find("name=\"synopsis\"")?;
    let after_tag = marker + html.get(marker..)?.find('>')?;
    let rest = html.get(after_tag..)?;
    let end = rest.find("</span>")?;
    let span = rest.get(..end)?;
    let text = plain_text(span);
    if text.is_empty() {
        return None;
    }
    Some(String::from(match text.strip_prefix("Summary:") {
        Some(trimmed) => trimmed.trim(),
        None => &text,
    }))
}

/// Reads one observatory notice and returns its synopsis, or says why the
/// document is not a notice this program can display.
///
/// # Errors
///
/// Returns [`VolcanoError::Shape`] when the document is not an object, carries
/// no notice text, or carries notice text without a synopsis.
pub fn parse_notice(document: &Value) -> Result<String, VolcanoError> {
    let Some(row) = document.as_object() else {
        return Err(shape("an object for the observatory notice"));
    };
    let Some(html) = row.get("notice_html").and_then(Value::as_str) else {
        return Err(shape("notice_html on the observatory notice"));
    };
    if html.trim().is_empty() {
        return Err(shape("a non-empty notice_html"));
    }
    parse_synopsis(document).ok_or(shape("a synopsis in the notice text"))
}

/// Strips markup from the notice text and collapses its whitespace.
pub fn plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut inside_tag = false;
    for character in html.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => {
                inside_tag = false;
                text.push(' ');
            }
            _ if inside_tag => {}
            _ => text.push(character),
        }
    }
    let decoded = text
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<&str>>().join(" ")
}
/// The notice that carries a volcano's latest statement./// Fetches the raised volcanoes and follows each one up for its position.
pub fn fetch(acquire: &Acquire) -> Result<Served<Report>, VolcanoError> {
    let list_fetched = acquire
        .fetch_operation(&sources::USGS_VOLCANO, "list", &[])
        .map_err(|source| VolcanoError::Provider { source })?;
    let document = list_fetched
        .json()
        .map_err(|source| VolcanoError::Provider { source })?;
    let notices = parse_list(&document)?;
    let not_followed = notices.len().saturating_sub(MAX_VOLCANOES);
    let mut volcanoes = Vec::with_capacity(notices.len().min(MAX_VOLCANOES));
    let mut requests = Vec::new();
    for notice in notices.into_iter().take(MAX_VOLCANOES) {
        let mut place = match acquire.fetch_operation(
            &sources::USGS_VOLCANO,
            "volcano",
            &[("vnum", notice.vnum.as_str())],
        ) {
            Ok(fetched) => {
                let document = fetched
                    .json()
                    .map_err(|source| VolcanoError::Provider { source })?;
                let place = parse_volcano(&document)?;
                requests.push((fetched.provenance, fetched.cache));
                place
            }
            Err(_) => Place {
                latitude: None,
                longitude: None,
                elevation_m: None,
                region: None,
                url: None,
                summary: None,
            },
        };
        if !notice.identifier.is_empty()
            && let Ok(fetched) = acquire.fetch_operation(
                &sources::USGS_VOLCANO,
                "notice",
                &[("notice", notice.identifier.as_str())],
            )
        {
            let document = fetched
                .json()
                .map_err(|source| VolcanoError::Provider { source })?;
            place.summary = parse_synopsis(&document);
            requests.push((fetched.provenance, fetched.cache));
        }
        volcanoes.push(Volcano { notice, place });
    }
    let mut served = Served::new(
        Report {
            volcanoes,
            not_followed,
        },
        list_fetched.provenance,
        list_fetched.cache,
    );
    for (provenance, cache) in requests {
        served = served.with_request(provenance, cache);
    }
    Ok(served)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct VolcanoRequest {
    pub generation: u64,
}

/// Answers from the worker.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "the served value travels whole so its provenance cannot be separated from it"
)]
pub enum VolcanoEvent {
    Ready {
        generation: u64,
        report: Served<Report>,
    },
    Failed {
        generation: u64,
        error: VolcanoError,
    },
}

/// Fetches the volcano report on the shared job queue.
pub struct VolcanoFeed {
    feed: dispatch::Feed<VolcanoEvent>,
    generation: u64,
    stopped: bool,
}

impl VolcanoFeed {
    pub fn new(acquire: Acquire) -> Self {
        Self {
            feed: dispatch::Feed::new(acquire),
            generation: 0,
            stopped: false,
        }
    }

    pub fn request(&mut self, request: &VolcanoRequest) -> Result<(), VolcanoError> {
        self.generation = request.generation;
        let generation = request.generation;
        self.feed
            .request(move |acquire| match fetch(acquire) {
                Ok(report) => VolcanoEvent::Ready { generation, report },
                Err(error) => VolcanoEvent::Failed { generation, error },
            })
            .map_err(|_| {
                self.stopped = true;
                VolcanoError::WorkerStopped
            })
    }

    pub fn poll(&mut self) -> Option<VolcanoEvent> {
        match self.feed.poll() {
            Ok(Some(event)) => Some(event),
            Ok(None) => None,
            Err(dispatch::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(VolcanoEvent::Failed {
                    generation: self.generation,
                    error: VolcanoError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum VolcanoState {
    Idle,
    Loading {
        last: Option<Served<Report>>,
    },
    Ready {
        served: Served<Report>,
    },
    Failed {
        error: VolcanoError,
        last: Option<Served<Report>>,
    },
}

impl VolcanoState {
    /// The most recent report still worth drawing.
    pub const fn last(&self) -> Option<&Served<Report>> {
        match self {
            Self::Ready { served } => Some(served),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and the value it last delivered.
pub struct VolcanoView {
    feed: VolcanoFeed,
    state: VolcanoState,
    generation: u64,
}

impl VolcanoView {
    pub fn new(acquire: Acquire) -> Self {
        Self {
            feed: VolcanoFeed::new(acquire),
            state: VolcanoState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &VolcanoState {
        &self.state
    }

    pub fn report(&self) -> Option<&Report> {
        self.state.last().map(|served| &served.value)
    }

    pub const fn served(&self) -> Option<&Served<Report>> {
        self.state.last()
    }

    pub fn start(&mut self) {
        let last = take_last(&mut self.state);
        self.state = VolcanoState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = VolcanoRequest {
            generation: self.generation,
        };
        if let Err(error) = self.feed.request(&request) {
            self.state = VolcanoState::Failed { error, last: None };
        }
    }

    /// Takes in the worker's answers. The engine owns retry and cache.
    pub fn update(&mut self, _now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                VolcanoEvent::Ready { generation, report } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = VolcanoState::Ready { served: report };
                }
                VolcanoEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    self.state = VolcanoState::Failed { error, last };
                }
            }
        }
        false
    }
}

/// Empties the state and returns the report it was holding.
fn take_last(state: &mut VolcanoState) -> Option<Served<Report>> {
    match std::mem::replace(state, VolcanoState::Idle) {
        VolcanoState::Loading { last } | VolcanoState::Failed { last, .. } => last,
        VolcanoState::Ready { served } => Some(served),
        VolcanoState::Idle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Value {
        let body = r#"[
            {
                "obs_fullname": "Alaska Volcano Observatory", "obs_abbr": "avo",
                "volcano_name": "Great Sitkin", "vnum": "311120",
                "notice_identifier": "DOI-USGS-AVO-2026-11-01T20:02:20+00:00",
                "sent_utc": "2026-11-01 20:05:10", "sent_unixtime": 1793563510,
                "color_code": "ORANGE", "alert_level": "WATCH"
            },
            {
                "obs_fullname": "Alaska Volcano Observatory", "obs_abbr": "avo",
                "volcano_name": "Shishaldin", "vnum": "311360",
                "notice_identifier": "DOI-USGS-AVO-2026-11-01T20:02:20+00:00",
                "sent_utc": "2026-11-01 20:05:10", "sent_unixtime": 1793563510,
                "color_code": "YELLOW", "alert_level": "ADVISORY"
            }
        ]"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn volcano() -> Value {
        let body = r#"{
            "volcano_name": "Great Sitkin", "vnum": "311120", "obs_abbr": "avo",
            "volcano_cd": "ak111", "region": "Aleutians",
            "latitude": 52.0765, "longitude": -176.1109, "elevation_meters": 1740,
            "volcano_url": "https://avo.alaska.edu/volcano/great-sitkin"
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    fn notice() -> Value {
        let body = r#"{
            "notice_html": "<br/>\n<center><b>ALASKA VOLCANO OBSERVATORY DAILY UPDATE</b></center><br/>\n<span name=\"synopsis\"><b>Summary:</b> Slow eruption of lava within the summit crater continues.</span><br/>\n<span name=\"summary\"><p>Lava continues to erupt slowly.</p></span>"
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    #[test]
    fn a_summary_lists_each_raised_volcano() -> Result<(), VolcanoError> {
        let notices = parse_list(&list())?;
        assert_eq!(notices.len(), 2);
        let first = notices.first().ok_or(VolcanoError::Shape {
            detail: "a first notice",
        })?;
        assert_eq!(first.volcano, "Great Sitkin");
        assert_eq!(first.colour, Some(Colour::Orange));
        assert_eq!(first.alert_level, "WATCH");
        assert_eq!(first.sent_ms, Some(1_793_563_510_000));
        Ok(())
    }

    #[test]
    fn an_unfamiliar_colour_code_is_kept_as_text_and_not_guessed() -> Result<(), VolcanoError> {
        let body = r#"[{"volcano_name": "Etna", "vnum": "211060", "color_code": "PURPLE"}]"#;
        let Ok(document) = serde_json::from_str::<Value>(body) else {
            return Ok(());
        };
        let notices = parse_list(&document)?;
        let first = notices.first().ok_or(VolcanoError::Shape {
            detail: "a first notice",
        })?;
        assert_eq!(first.colour, None);
        assert_eq!(first.colour_text, "PURPLE");
        Ok(())
    }

    #[test]
    fn a_volcano_record_carries_its_position() -> Result<(), VolcanoError> {
        let place = parse_volcano(&volcano())?;
        assert_eq!(place.latitude, Some(52.0765));
        assert_eq!(place.longitude, Some(-176.1109));
        assert_eq!(place.elevation_m, Some(1740.0));
        assert_eq!(place.region.as_deref(), Some("Aleutians"));
        Ok(())
    }

    #[test]
    fn a_position_outside_the_globe_is_not_used() {
        let body = r#"{"latitude": 95.0, "longitude": 10.0}"#;
        let Ok(document) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let Ok(place) = parse_volcano(&document) else {
            return;
        };
        let volcano = Volcano {
            notice: Notice {
                volcano: String::from("Nowhere"),
                vnum: String::from("0"),
                observatory: String::from("none"),
                colour: None,
                colour_text: String::from("not stated"),
                alert_level: String::from("not stated"),
                sent_ms: None,
                identifier: String::new(),
            },
            place,
        };
        assert_eq!(volcano.position(), None);
    }

    #[test]
    fn the_synopsis_is_read_as_plain_text() -> Result<(), VolcanoError> {
        let summary = parse_notice(&notice())?;
        assert_eq!(
            summary,
            "Slow eruption of lava within the summit crater continues."
        );
        Ok(())
    }

    #[test]
    fn a_notice_without_notice_text_is_refused() {
        let Ok(document) = serde_json::from_str::<Value>("{\"notice_html\": \"\"}") else {
            return;
        };
        assert!(parse_notice(&document).is_err());
    }

    #[test]
    fn a_notice_without_a_synopsis_is_refused() {
        let Ok(document) =
            serde_json::from_str::<Value>("{\"notice_html\": \"<p>No summary</p>\"}")
        else {
            return;
        };
        assert!(parse_notice(&document).is_err());
    }

    #[test]
    fn a_notice_that_is_not_an_object_is_refused() {
        let Ok(document) = serde_json::from_str::<Value>("[]") else {
            return;
        };
        assert!(parse_notice(&document).is_err());
    }

    #[test]
    fn markup_and_entities_are_stripped_from_notice_text() {
        assert_eq!(
            plain_text("<p>Ash &amp; steam&#39;s plume</p>\n<b>now</b>"),
            "Ash & steam's plume now"
        );
    }

    #[test]
    fn the_colours_rank_from_green_to_red() {
        assert!(Colour::Red > Colour::Orange);
        assert!(Colour::Orange > Colour::Yellow);
        assert!(Colour::Yellow > Colour::Green);
        assert_eq!(Colour::parse("orange"), Some(Colour::Orange));
    }

    #[test]
    fn a_report_counts_and_ranks_its_colours() {
        let notices = parse_list(&list()).unwrap_or_default();
        let report = Report {
            volcanoes: notices
                .into_iter()
                .map(|notice| Volcano {
                    notice,
                    place: Place {
                        latitude: None,
                        longitude: None,
                        elevation_m: None,
                        region: None,
                        url: None,
                        summary: None,
                    },
                })
                .collect(),
            not_followed: 0,
        };
        assert_eq!(report.highest(), Some(Colour::Orange));
        assert_eq!(
            report.by_colour(),
            vec![(Colour::Orange, 1), (Colour::Yellow, 1)]
        );
        assert_eq!(report.located().count(), 0);
    }
}
