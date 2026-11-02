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
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "USGS Volcano Hazards Program";
const LIST_URL: &str = "https://volcanoes.usgs.gov/hans-public/api/volcano/getElevatedVolcanoes";
const VOLCANO_URL: &str = "https://volcanoes.usgs.gov/hans-public/api/volcano/getVolcano/";
/// How many raised volcanoes are followed up for a position, so one busy day
/// cannot turn into an unbounded burst of requests.
pub const MAX_VOLCANOES: usize = 12;
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);

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
    Provider { source: JsonError },
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

impl VolcanoError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

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

/// The summary request.
pub const fn list_url() -> &'static str {
    LIST_URL
}

/// The request for one volcano's own record.
pub fn volcano_url(vnum: &str) -> String {
    format!("{VOLCANO_URL}{vnum}")
}

/// The notice that carries a volcano's latest statement.
pub fn notice_url(notice_id: &str) -> String {
    format!("https://volcanoes.usgs.gov/hans-public/api/notice/getNotice/{notice_id}")
}

/// Fetches the raised volcanoes and follows each one up for its position.
pub fn fetch() -> Result<Report, VolcanoError> {
    let document = provider::get_json(list_url(), "application/json")
        .map_err(|source| VolcanoError::Provider { source })?;
    let notices = parse_list(&document)?;
    let not_followed = notices.len().saturating_sub(MAX_VOLCANOES);
    let mut volcanoes = Vec::with_capacity(notices.len().min(MAX_VOLCANOES));
    for notice in notices.into_iter().take(MAX_VOLCANOES) {
        let mut place = match provider::get_json(&volcano_url(&notice.vnum), "application/json") {
            Ok(document) => parse_volcano(&document)?,
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
            && let Ok(document) =
                provider::get_json(&notice_url(&notice.identifier), "application/json")
        {
            place.summary = parse_synopsis(&document);
        }
        volcanoes.push(Volcano { notice, place });
    }
    Ok(Report {
        volcanoes,
        not_followed,
    })
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct VolcanoRequest {
    pub generation: u64,
}

/// Answers from the worker.
#[derive(Debug)]
pub enum VolcanoEvent {
    Ready {
        generation: u64,
        report: Report,
    },
    Failed {
        generation: u64,
        error: VolcanoError,
    },
}

/// Fetches the volcano report off the UI thread.
pub struct VolcanoFeed {
    requests: Sender<VolcanoRequest>,
    events: Receiver<VolcanoEvent>,
    generation: u64,
    stopped: bool,
}

impl VolcanoFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<VolcanoRequest>();
        let (event_sender, events) = mpsc::channel::<VolcanoEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match fetch() {
                    Ok(report) => VolcanoEvent::Ready {
                        generation: request.generation,
                        report,
                    },
                    Err(error) => VolcanoEvent::Failed {
                        generation: request.generation,
                        error,
                    },
                };
                if event_sender.send(event).is_err() {
                    return;
                }
            }
        });
        Self {
            requests,
            events,
            generation: 0,
            stopped: false,
        }
    }

    pub fn request(&mut self, request: VolcanoRequest) -> Result<(), VolcanoError> {
        self.generation = request.generation;
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(VolcanoError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<VolcanoEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
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
        last: Option<Report>,
    },
    Ready {
        report: Report,
    },
    Failed {
        error: VolcanoError,
        last: Option<Report>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl VolcanoState {
    /// The most recent report still worth drawing.
    pub const fn last(&self) -> Option<&Report> {
        match self {
            Self::Ready { report } => Some(report),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and its current state, with retry scheduling.
pub struct VolcanoView {
    feed: VolcanoFeed,
    state: VolcanoState,
    generation: u64,
}

impl VolcanoView {
    pub fn new() -> Self {
        Self {
            feed: VolcanoFeed::new(),
            state: VolcanoState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &VolcanoState {
        &self.state
    }

    pub const fn report(&self) -> Option<&Report> {
        self.state.last()
    }

    pub fn start(&mut self, attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = VolcanoState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = VolcanoRequest {
            generation: self.generation,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = VolcanoState::Failed {
                error,
                last: None,
                attempts: attempt,
                retry_due: None,
            };
        }
    }

    /// Takes in the worker's answer and returns whether a retry came due.
    pub fn update(&mut self, now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                VolcanoEvent::Ready { generation, report } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = VolcanoState::Ready { report };
                }
                VolcanoEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = VolcanoState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            VolcanoState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            VolcanoState::Idle | VolcanoState::Loading { .. } | VolcanoState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            VolcanoState::Failed { attempts, .. } => *attempts,
            VolcanoState::Idle | VolcanoState::Loading { .. } | VolcanoState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            VolcanoState::Failed { retry_due, .. } => retry_due.is_some(),
            VolcanoState::Idle | VolcanoState::Loading { .. } | VolcanoState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the report it was holding.
fn take_last(state: &mut VolcanoState) -> Option<Report> {
    match std::mem::replace(state, VolcanoState::Idle) {
        VolcanoState::Loading { last } | VolcanoState::Failed { last, .. } => last,
        VolcanoState::Ready { report } => Some(report),
        VolcanoState::Idle => None,
    }
}

/// Exponential backoff for a request that may succeed later.
fn retry_due(transient: bool, attempts: u32) -> Option<Instant> {
    if !transient || attempts == 0 || attempts >= MAX_ATTEMPTS {
        return None;
    }
    let steps = attempts.min(4);
    let delay = RETRY_BASE.saturating_mul(1_u32 << steps).min(RETRY_MAX);
    Instant::now().checked_add(delay)
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
    fn the_synopsis_is_read_as_plain_text() {
        let summary = parse_synopsis(&notice());
        assert_eq!(
            summary.as_deref(),
            Some("Slow eruption of lava within the summit crater continues.")
        );
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

    #[test]
    fn the_requests_name_the_volcano_and_its_notice() {
        assert_eq!(
            volcano_url("311120"),
            "https://volcanoes.usgs.gov/hans-public/api/volcano/getVolcano/311120"
        );
        assert!(notice_url("DOI-USGS-AVO-1").ends_with("/DOI-USGS-AVO-1"));
    }
}
