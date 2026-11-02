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
//! River gauges: what a gauge actually measures, as against the modelled
//! discharge the hydrology layer reads.
//!
//! The USGS National Water Information System publishes the latest value of
//! every series a gauge reports, one series per site and parameter. A site
//! usually reports gage height and discharge, so the two are joined into one
//! gauge. A value the agency has not yet approved is marked provisional, and
//! that mark is carried through to the display: a provisional stage is the
//! agency's own unapproved number, not a settled measurement.
//!
//! The units are the ones the provider states for each series. Gage height is
//! published in feet and discharge in cubic feet per second, but the layer does
//! not assume that: it reads the unit with the value and shows what it is told.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::provider::{self, JsonError};
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "USGS National Water Information System";
const BASE: &str = "https://waterservices.usgs.gov/nwis/iv/";
/// Gage height, in the provider's feet.
pub const STAGE: &str = "00065";
/// Discharge, in the provider's cubic feet per second.
pub const DISCHARGE: &str = "00060";
/// How much history the hydrograph asks for.
const HISTORY_PERIOD: &str = "P7D";
/// How long a gauge query may take.
///
/// The water-services host answers a small box in half a minute on a busy day:
/// the query is server-side and slow independently of how much it returns, so
/// the shared ten-second budget fails every attempt while the service is up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);
/// How many times a failing request is retried before the layer stops asking.
pub const MAX_ATTEMPTS: u32 = 4;
const RETRY_BASE: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// How many gauges the rail lists.
pub const LIST_LIMIT: usize = 10;
/// The value the service writes where it has no measurement.
///
/// A gauge that cannot report still appears in the response, with this number
/// where a stage should be. It is not a measurement — a river is never a
/// million feet below its datum — so a reading at or beyond it is treated as no
/// reading at all rather than listed as one.
pub const NO_DATA: f64 = -999_999.0;

/// Whether a value is the provider's no-data sentinel rather than a reading.
pub fn is_no_data(value: f64) -> bool {
    value.abs() >= NO_DATA.abs()
}

/// One value a gauge published.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub value: f64,
    pub unit: String,
    pub at_ms: u64,
    /// The agency has not approved this value yet, so it may still be revised.
    pub provisional: bool,
    /// The agency's own qualifiers, in its own letters, kept as published.
    pub qualifiers: Vec<String>,
}

impl Reading {
    /// The qualifiers as a short label, naming the ones that change how the
    /// number should be read.
    pub fn qualifier_label(&self) -> Option<String> {
        let named: Vec<&str> = self
            .qualifiers
            .iter()
            .map(String::as_str)
            .filter(|qualifier| *qualifier != "P" && *qualifier != "A")
            .collect();
        if named.is_empty() {
            return None;
        }
        Some(qualifier_meaning(&named))
    }
}

/// What a qualifier code means, in the provider's own scheme.
fn qualifier_meaning(codes: &[&str]) -> String {
    let meanings: Vec<&str> = codes
        .iter()
        .map(|code| match *code {
            "e" => "estimated",
            "Ice" => "affected by ice",
            "Eqp" => "equipment malfunction",
            "Bkw" => "backwater",
            "Dis" => "discontinued",
            "Mnt" => "maintenance",
            "***" => "temporarily unavailable",
            "Ssn" => "seasonal",
            "Rat" => "rating being developed",
            other => other,
        })
        .collect();
    meanings.join(", ")
}

/// One gauge's latest values.
#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    pub site: String,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub stage: Option<Reading>,
    pub discharge: Option<Reading>,
}

impl Gauge {
    /// The newest value the gauge published, whichever parameter it is.
    pub const fn newest(&self) -> Option<&Reading> {
        match (&self.stage, &self.discharge) {
            (Some(stage), Some(discharge)) => {
                if stage.at_ms >= discharge.at_ms {
                    Some(stage)
                } else {
                    Some(discharge)
                }
            }
            (Some(stage), None) => Some(stage),
            (None, Some(discharge)) => Some(discharge),
            (None, None) => None,
        }
    }

    /// How far the gauge is from a point, in kilometres.
    pub fn distance_from(&self, latitude: f64, longitude: f64) -> f64 {
        match crate::projection::inverse(latitude, longitude, self.latitude, self.longitude) {
            Ok((distance, _)) => distance,
            Err(_) => f64::MAX,
        }
    }
}

/// Every gauge in a box.
#[derive(Debug, Clone, PartialEq)]
pub struct GaugeSet {
    pub gauges: Vec<Gauge>,
    /// Series the provider sent that could not be read.
    pub rejected: usize,
}

/// A week of one gauge's readings, for the hydrograph.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub site: String,
    pub name: String,
    pub stage: Vec<Reading>,
    pub discharge: Vec<Reading>,
}

impl History {
    /// The highest stage in the record.
    pub fn highest_stage(&self) -> Option<&Reading> {
        self.stage
            .iter()
            .filter(|reading| reading.value.is_finite())
            .max_by(|left, right| left.value.total_cmp(&right.value))
    }

    /// The lowest stage in the record.
    pub fn lowest_stage(&self) -> Option<&Reading> {
        self.stage
            .iter()
            .filter(|reading| reading.value.is_finite())
            .min_by(|left, right| left.value.total_cmp(&right.value))
    }

    /// The rise from the lowest to the highest stage, in the provider's unit.
    pub fn stage_range(&self) -> Option<f64> {
        let highest = self.highest_stage()?.value;
        let lowest = self.lowest_stage()?.value;
        Some(highest - lowest)
    }

    /// The bottom of the hydrograph axis, rounded down to a readable step.
    ///
    /// Gage height is measured from the gauge's own datum, and a river that sits
    /// between five and six feet would read as a flat line if the axis started
    /// at zero. The chart is about the river's movement, so the axis starts just
    /// below the lowest stage and says where that is.
    pub fn axis_floor(&self) -> f64 {
        let lowest = self.lowest_stage().map_or(0.0, |reading| reading.value);
        let highest = self.highest_stage().map_or(lowest, |reading| reading.value);
        let step = axis_step((highest - lowest).max(0.1) / 4.0);
        (lowest / step).floor() * step
    }

    /// The top of the hydrograph axis, rounded up to a readable step.
    pub fn axis_top(&self) -> f64 {
        let highest = self.highest_stage().map_or(0.0, |reading| reading.value);
        let lowest = self.lowest_stage().map_or(0.0, |reading| reading.value);
        let span = (highest - lowest).max(0.1);
        let step = axis_step(span / 4.0);
        let rounded = (highest / step).ceil() * step;
        if rounded > highest {
            rounded
        } else {
            highest + step / 2.0
        }
    }
}

/// A readable axis step at or above `rough`.
fn axis_step(rough: f64) -> f64 {
    for step in [
        0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0,
        2_000.0, 5_000.0, 10_000.0,
    ] {
        if step >= rough {
            return step;
        }
    }
    10_000.0
}

/// Why a gauge record could not be read.
#[derive(Debug)]
pub enum GaugeError {
    Provider { source: JsonError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for GaugeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => {
                write!(formatter, "The river-gauge request failed: {source}")
            }
            Self::Shape { detail } => write!(
                formatter,
                "The river-gauge record is not usable: it wants {detail}."
            ),
            Self::WorkerStopped => write!(
                formatter,
                "The river-gauge worker stopped, so no gauge can be read."
            ),
        }
    }
}

impl Error for GaugeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl GaugeError {
    /// Whether asking again could succeed.
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Provider { source } => source.is_transient(),
            Self::Shape { .. } | Self::WorkerStopped => false,
        }
    }
}

const fn shape(detail: &'static str) -> GaugeError {
    GaugeError::Shape { detail }
}

/// Reads the series list, joining a site's parameters into one gauge.
pub fn parse(document: &Value) -> Result<GaugeSet, GaugeError> {
    let Some(series) = document
        .get("value")
        .and_then(|value| value.get("timeSeries"))
        .and_then(Value::as_array)
    else {
        return Err(shape("a list of time series"));
    };
    let mut gauges: BTreeMap<String, Gauge> = BTreeMap::new();
    let mut rejected = 0_usize;
    for entry in series {
        let Some(entry) = entry.as_object() else {
            rejected += 1;
            continue;
        };
        let Some((site, name, latitude, longitude)) = site_of(entry) else {
            rejected += 1;
            continue;
        };
        let Some(code) = entry
            .get("variable")
            .and_then(|variable| variable.get("variableCode"))
            .and_then(Value::as_array)
            .and_then(|codes| codes.first())
            .and_then(|code| code.get("value"))
            .and_then(Value::as_str)
        else {
            rejected += 1;
            continue;
        };
        let Some(reading) = reading_of(entry) else {
            rejected += 1;
            continue;
        };
        let gauge = gauges.entry(site.clone()).or_insert_with(|| Gauge {
            site,
            name,
            latitude,
            longitude,
            stage: None,
            discharge: None,
        });
        match code {
            STAGE => gauge.stage = Some(reading),
            DISCHARGE => gauge.discharge = Some(reading),
            _ => rejected += 1,
        }
    }
    if gauges.is_empty() && rejected > 0 {
        return Err(shape("at least one gauge it could read"));
    }
    Ok(GaugeSet {
        gauges: gauges.into_values().collect(),
        rejected,
    })
}

/// A series' site, name, and position.
fn site_of(entry: &Map<String, Value>) -> Option<(String, String, f64, f64)> {
    let source = entry.get("sourceInfo")?.as_object()?;
    let site = source
        .get("siteCode")?
        .as_array()?
        .first()?
        .get("value")?
        .as_str()?
        .trim()
        .to_owned();
    let name = source
        .get("siteName")
        .and_then(Value::as_str)
        .unwrap_or("gauge name not stated")
        .trim()
        .to_owned();
    let location = source.get("geoLocation")?.get("geogLocation")?;
    let latitude = location.get("latitude")?.as_f64()?;
    let longitude = location.get("longitude")?.as_f64()?;
    if !latitude.is_finite() || !longitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return None;
    }
    Some((site, name, latitude, longitude))
}

/// A series' unit and its newest value.
fn reading_of(entry: &Map<String, Value>) -> Option<Reading> {
    let unit = entry
        .get("variable")
        .and_then(|variable| variable.get("unit"))
        .and_then(|unit| unit.get("unitCode"))
        .and_then(Value::as_str)
        .unwrap_or("no unit stated")
        .trim()
        .to_owned();
    let values = entry.get("values")?.as_array()?.first()?;
    let values = values.get("value")?.as_array()?;
    let last = values.last()?;
    let raw = last.get("value")?.as_str()?.trim();
    let value = raw
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && !is_no_data(*value))?;
    let at = last.get("dateTime")?.as_str()?;
    let at_ms = time::parse_iso_utc(at)?;
    let qualifiers: Vec<String> = last
        .get("qualifiers")
        .and_then(Value::as_array)
        .map(|codes| {
            codes
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let provisional = qualifiers.iter().any(|qualifier| qualifier == "P");
    Some(Reading {
        value,
        unit,
        at_ms,
        provisional,
        qualifiers,
    })
}

/// The box the request asks for, from the map view.
///
/// The service refuses any box larger than a few degrees on a side — a request
/// for an eleven-degree box comes back as a plain 400 — so the box is capped
/// well inside that limit instead of asking for the whole viewport.
pub fn bounds(centre_lat: f64, centre_lon: f64, span_km: f64) -> (f64, f64, f64, f64) {
    let half_lat = (span_km / 2.0 / 111.0).clamp(0.2, MAX_HALF_DEG);
    let cosine = centre_lat.to_radians().cos().abs().max(0.2);
    let half_lon = (half_lat / cosine).clamp(0.2, MAX_HALF_DEG);
    let south = (centre_lat - half_lat).clamp(-89.0, 89.0);
    let north = (centre_lat + half_lat).clamp(-89.0, 89.0);
    let west = (centre_lon - half_lon).clamp(-180.0, 180.0);
    let east = (centre_lon + half_lon).clamp(-180.0, 180.0);
    (south, west, north, east)
}

/// Half the widest box the service will answer, in degrees. A four-degree
/// square is sixteen square degrees, comfortably under the twenty-five the
/// service allows.
pub const MAX_HALF_DEG: f64 = 1.0;

/// The request for the gauges in a box.
pub fn request_url(bounds: (f64, f64, f64, f64)) -> String {
    let (south, west, north, east) = bounds;
    format!(
        "{BASE}?format=json&bBox={west:.4},{south:.4},{east:.4},{north:.4}&parameterCd={STAGE},{DISCHARGE}&siteStatus=all"
    )
}

/// The request for one gauge's week.
pub fn history_url(site: &str) -> String {
    format!(
        "{BASE}?format=json&sites={site}&parameterCd={STAGE},{DISCHARGE}&period={HISTORY_PERIOD}"
    )
}

/// Fetches the gauges in a box.
pub fn fetch(bounds: (f64, f64, f64, f64)) -> Result<GaugeSet, GaugeError> {
    let document =
        provider::get_json_within(&request_url(bounds), "application/json", REQUEST_TIMEOUT)
            .map_err(|source| GaugeError::Provider { source })?;
    parse(&document)
}

/// Reads a week of one gauge's readings into a hydrograph.
pub fn parse_history(document: &Value, site: &str) -> Result<History, GaugeError> {
    let Some(series) = document
        .get("value")
        .and_then(|value| value.get("timeSeries"))
        .and_then(Value::as_array)
    else {
        return Err(shape("a list of time series"));
    };
    let mut name = String::from("gauge name not stated");
    let mut stage = Vec::new();
    let mut discharge = Vec::new();
    for entry in series {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some((entry_site, entry_name, _, _)) = site_of(entry) else {
            continue;
        };
        if entry_site != site {
            continue;
        }
        name = entry_name;
        let Some(code) = entry
            .get("variable")
            .and_then(|variable| variable.get("variableCode"))
            .and_then(Value::as_array)
            .and_then(|codes| codes.first())
            .and_then(|code| code.get("value"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let readings = readings_of(entry);
        match code {
            STAGE => stage = readings,
            DISCHARGE => discharge = readings,
            _ => {}
        }
    }
    if stage.is_empty() && discharge.is_empty() {
        return Err(shape("at least one reading for the gauge"));
    }
    stage.sort_by_key(|reading| reading.at_ms);
    discharge.sort_by_key(|reading| reading.at_ms);
    Ok(History {
        site: String::from(site),
        name,
        stage,
        discharge,
    })
}

/// Every value in one series, skipping the ones the provider marks as
/// unavailable.
fn readings_of(entry: &Map<String, Value>) -> Vec<Reading> {
    let unit = entry
        .get("variable")
        .and_then(|variable| variable.get("unit"))
        .and_then(|unit| unit.get("unitCode"))
        .and_then(Value::as_str)
        .unwrap_or("no unit stated")
        .trim()
        .to_owned();
    let Some(values) = entry
        .get("values")
        .and_then(Value::as_array)
        .and_then(|series| series.first())
        .and_then(|first| first.get("value"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut readings = Vec::with_capacity(values.len());
    for value in values {
        let Some(raw) = value.get("value").and_then(Value::as_str) else {
            continue;
        };
        let Ok(number) = raw.trim().parse::<f64>() else {
            continue;
        };
        if !number.is_finite() || is_no_data(number) {
            continue;
        }
        let Some(at_ms) = value
            .get("dateTime")
            .and_then(Value::as_str)
            .and_then(time::parse_iso_utc)
        else {
            continue;
        };
        let qualifiers: Vec<String> = value
            .get("qualifiers")
            .and_then(Value::as_array)
            .map(|codes| {
                codes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let provisional = qualifiers.iter().any(|qualifier| qualifier == "P");
        readings.push(Reading {
            value: number,
            unit: unit.clone(),
            at_ms,
            provisional,
            qualifiers,
        });
    }
    readings
}

/// Fetches one gauge's week.
pub fn fetch_history(site: &str) -> Result<History, GaugeError> {
    let document =
        provider::get_json_within(&history_url(site), "application/json", REQUEST_TIMEOUT)
            .map_err(|source| GaugeError::Provider { source })?;
    parse_history(&document, site)
}

/// What the worker is asked for.
#[derive(Debug)]
pub enum GaugeRequest {
    Box {
        generation: u64,
        bounds: (f64, f64, f64, f64),
    },
    History {
        generation: u64,
        site: String,
    },
}

/// What the worker answers.
#[derive(Debug)]
pub enum GaugeEvent {
    Ready { generation: u64, set: GaugeSet },
    HistoryReady { generation: u64, history: History },
    Failed { generation: u64, error: GaugeError },
}

/// Fetches gauges off the UI thread.
pub struct GaugeFeed {
    requests: Sender<GaugeRequest>,
    events: Receiver<GaugeEvent>,
    generation: u64,
    stopped: bool,
}

impl GaugeFeed {
    pub fn new() -> Self {
        let (requests, request_receiver) = mpsc::channel::<GaugeRequest>();
        let (event_sender, events) = mpsc::channel::<GaugeEvent>();
        std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let event = match request {
                    GaugeRequest::Box { generation, bounds } => match fetch(bounds) {
                        Ok(set) => GaugeEvent::Ready { generation, set },
                        Err(error) => GaugeEvent::Failed { generation, error },
                    },
                    GaugeRequest::History { generation, site } => match fetch_history(&site) {
                        Ok(history) => GaugeEvent::HistoryReady {
                            generation,
                            history,
                        },
                        Err(error) => GaugeEvent::Failed { generation, error },
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

    pub fn request(&mut self, request: GaugeRequest) -> Result<(), GaugeError> {
        self.generation = match request {
            GaugeRequest::Box { generation, .. } | GaugeRequest::History { generation, .. } => {
                generation
            }
        };
        if self.requests.send(request).is_err() {
            self.stopped = true;
            return Err(GaugeError::WorkerStopped);
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Option<GaugeEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(GaugeEvent::Failed {
                    generation: self.generation,
                    error: GaugeError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the gauge request stands.
#[derive(Debug)]
pub enum GaugeState {
    Idle,
    Loading {
        last: Option<GaugeSet>,
    },
    Ready {
        set: GaugeSet,
    },
    Failed {
        error: GaugeError,
        last: Option<GaugeSet>,
        attempts: u32,
        retry_due: Option<Instant>,
    },
}

impl GaugeState {
    /// The most recent gauge set still worth drawing.
    pub const fn last(&self) -> Option<&GaugeSet> {
        match self {
            Self::Ready { set } => Some(set),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// Where the hydrograph request stands.
#[derive(Debug)]
pub enum HistoryState {
    Off,
    Loading { site: String },
    Ready { history: History },
    Failed { site: String, error: GaugeError },
}

/// The worker and its state.
///
/// The two kinds of request carry their own generation counters, because a box
/// refresh that follows a gauge choice must not retire that gauge's week: they
/// are separate questions with separate answers, and one arriving later than
/// the other is normal rather than superseded.
pub struct GaugeView {
    feed: GaugeFeed,
    state: GaugeState,
    history: HistoryState,
    box_generation: u64,
    history_generation: u64,
}

impl GaugeView {
    pub fn new() -> Self {
        Self {
            feed: GaugeFeed::new(),
            state: GaugeState::Idle,
            history: HistoryState::Off,
            box_generation: 0,
            history_generation: 0,
        }
    }

    pub const fn state(&self) -> &GaugeState {
        &self.state
    }

    pub const fn history(&self) -> &HistoryState {
        &self.history
    }

    pub const fn set(&self) -> Option<&GaugeSet> {
        self.state.last()
    }

    pub fn start(&mut self, bounds: (f64, f64, f64, f64), attempt: u32) {
        let last = take_last(&mut self.state);
        self.state = GaugeState::Loading { last };
        self.box_generation = self.box_generation.wrapping_add(1);
        let request = GaugeRequest::Box {
            generation: self.box_generation,
            bounds,
        };
        if let Err(error) = self.feed.request(request) {
            self.state = GaugeState::Failed {
                error,
                last: None,
                attempts: attempt,
                retry_due: None,
            };
        }
    }

    /// Asks for one gauge's week.
    pub fn start_history(&mut self, site: &str) {
        self.history_generation = self.history_generation.wrapping_add(1);
        self.history = HistoryState::Loading {
            site: String::from(site),
        };
        let request = GaugeRequest::History {
            generation: self.history_generation,
            site: String::from(site),
        };
        if let Err(error) = self.feed.request(request) {
            self.history = HistoryState::Failed {
                site: String::from(site),
                error,
            };
        }
    }

    /// Clears the hydrograph when the reader closes it.
    pub fn close_history(&mut self) {
        self.history = HistoryState::Off;
    }

    /// Takes in the worker's answers and returns whether a retry came due.
    pub fn update(&mut self, now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                GaugeEvent::Ready { generation, set } => {
                    if generation != self.box_generation {
                        continue;
                    }
                    self.state = GaugeState::Ready { set };
                }
                GaugeEvent::HistoryReady {
                    generation,
                    history,
                } => {
                    if generation != self.history_generation {
                        continue;
                    }
                    self.history = HistoryState::Ready { history };
                }
                GaugeEvent::Failed { generation, error } => {
                    let asked_history = generation == self.history_generation;
                    let asked_box = generation == self.box_generation;
                    if asked_history && let HistoryState::Loading { site } = &self.history {
                        self.history = HistoryState::Failed {
                            site: site.clone(),
                            error,
                        };
                        continue;
                    }
                    if !asked_box {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    let attempts = self.attempts().saturating_add(1);
                    let transient = error.is_transient();
                    self.state = GaugeState::Failed {
                        error,
                        last,
                        attempts,
                        retry_due: retry_due(transient, attempts),
                    };
                }
            }
        }
        match &self.state {
            GaugeState::Failed { retry_due, .. } => retry_due.is_some_and(|due| now >= due),
            GaugeState::Idle | GaugeState::Loading { .. } | GaugeState::Ready { .. } => false,
        }
    }

    /// The attempts already made for the current request.
    pub const fn attempts(&self) -> u32 {
        match &self.state {
            GaugeState::Failed { attempts, .. } => *attempts,
            GaugeState::Idle | GaugeState::Loading { .. } | GaugeState::Ready { .. } => 0,
        }
    }

    /// Whether another attempt is still allowed.
    pub const fn may_retry(&self) -> bool {
        self.attempts() < MAX_ATTEMPTS
    }

    /// Whether a retry has been scheduled but not yet made.
    pub const fn retry_waiting(&self) -> bool {
        match &self.state {
            GaugeState::Failed { retry_due, .. } => retry_due.is_some(),
            GaugeState::Idle | GaugeState::Loading { .. } | GaugeState::Ready { .. } => false,
        }
    }
}

/// Empties the state and returns the set it was holding.
fn take_last(state: &mut GaugeState) -> Option<GaugeSet> {
    match std::mem::replace(state, GaugeState::Idle) {
        GaugeState::Loading { last } | GaugeState::Failed { last, .. } => last,
        GaugeState::Ready { set } => Some(set),
        GaugeState::Idle => None,
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

    fn document() -> Value {
        let body = r#"{
            "value": {
                "timeSeries": [
                    {
                        "sourceInfo": {
                            "siteName": "ARKANSAS R AT MULVANE, KS",
                            "siteCode": [{"value": "07144570", "network": "NWIS", "agencyCode": "USGS"}],
                            "geoLocation": {"geogLocation": {"latitude": 37.4755222, "longitude": -97.2615472}}
                        },
                        "variable": {"variableCode": [{"value": "00065"}], "unit": {"unitCode": "ft"}},
                        "values": [{"value": [
                            {"value": "2.11", "qualifiers": ["P"], "dateTime": "2026-11-01T21:15:00.000-06:00"},
                            {"value": "2.14", "qualifiers": ["P"], "dateTime": "2026-11-01T22:15:00.000-06:00"}
                        ]}]
                    },
                    {
                        "sourceInfo": {
                            "siteName": "ARKANSAS R AT MULVANE, KS",
                            "siteCode": [{"value": "07144570", "network": "NWIS", "agencyCode": "USGS"}],
                            "geoLocation": {"geogLocation": {"latitude": 37.4755222, "longitude": -97.2615472}}
                        },
                        "variable": {"variableCode": [{"value": "00060"}], "unit": {"unitCode": "ft3/s"}},
                        "values": [{"value": [
                            {"value": "199", "qualifiers": ["P"], "dateTime": "2026-11-01T22:15:00.000-06:00"}
                        ]}]
                    }
                ]
            }
        }"#;
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(error) => Value::String(format!("{error}")),
        }
    }

    #[test]
    fn a_sites_parameters_are_joined_into_one_gauge() -> Result<(), GaugeError> {
        let set = parse(&document())?;
        assert_eq!(set.gauges.len(), 1);
        assert_eq!(set.rejected, 0);
        let gauge = set.gauges.first().ok_or(GaugeError::Shape {
            detail: "a first gauge",
        })?;
        assert_eq!(gauge.site, "07144570");
        assert_eq!(gauge.name, "ARKANSAS R AT MULVANE, KS");
        assert_eq!(
            gauge.stage.as_ref().map(|reading| reading.value),
            Some(2.14)
        );
        assert_eq!(
            gauge.discharge.as_ref().map(|reading| reading.value),
            Some(199.0)
        );
        assert_eq!(
            gauge.stage.as_ref().map(|reading| reading.unit.as_str()),
            Some("ft")
        );
        Ok(())
    }

    #[test]
    fn the_newest_value_is_the_one_the_gauge_last_published() -> Result<(), GaugeError> {
        let set = parse(&document())?;
        let gauge = set.gauges.first().ok_or(GaugeError::Shape {
            detail: "a first gauge",
        })?;
        // Both series end at the same hour, so the stage is the one returned
        // and its timestamp is the gauge's newest.
        let newest = gauge.newest().ok_or(GaugeError::Shape {
            detail: "a newest reading",
        })?;
        assert_eq!(
            Some(newest.at_ms),
            gauge.stage.as_ref().map(|reading| reading.at_ms)
        );
        assert_eq!(newest.value, 2.14);
        Ok(())
    }

    #[test]
    fn a_provisional_value_is_marked_as_one() -> Result<(), GaugeError> {
        let set = parse(&document())?;
        let gauge = set.gauges.first().ok_or(GaugeError::Shape {
            detail: "a first gauge",
        })?;
        let stage = gauge
            .stage
            .as_ref()
            .ok_or(GaugeError::Shape { detail: "a stage" })?;
        assert!(stage.provisional);
        assert_eq!(stage.qualifier_label(), None);
        Ok(())
    }

    #[test]
    fn the_no_data_sentinel_is_not_a_reading() {
        assert!(is_no_data(-999_999.0));
        assert!(is_no_data(NO_DATA));
        assert!(!is_no_data(0.0));
        assert!(!is_no_data(11.99));
        assert!(!is_no_data(-2.5));
    }

    #[test]
    fn a_series_of_sentinels_reads_as_no_gauge() {
        let body = r#"{"value": {"timeSeries": [
            {"sourceInfo": {"siteName": "x", "siteCode": [{"value": "1"}],
             "geoLocation": {"geogLocation": {"latitude": 1.0, "longitude": 2.0}}},
             "variable": {"variableCode": [{"value": "00065"}], "unit": {"unitCode": "ft"}},
             "values": [{"value": [
                {"value": "-999999", "qualifiers": ["P"], "dateTime": "2026-11-01T22:15:00.000-06:00"}
             ]}]}
        ]}}"#;
        let Ok(document) = serde_json::from_str::<Value>(body) else {
            return;
        };
        assert!(
            parse(&document).is_err(),
            "a sentinel must not become a gauge"
        );
    }

    #[test]
    fn a_qualifier_that_changes_the_reading_is_named() {
        let reading = Reading {
            value: 1.0,
            unit: String::from("ft"),
            at_ms: 0,
            provisional: false,
            qualifiers: vec![String::from("A"), String::from("Ice")],
        };
        assert_eq!(
            reading.qualifier_label().as_deref(),
            Some("affected by ice")
        );
    }

    #[test]
    fn a_series_with_no_values_is_counted_as_rejected() {
        let body = r#"{"value": {"timeSeries": [
            {"sourceInfo": {"siteName": "x", "siteCode": [{"value": "1"}],
             "geoLocation": {"geogLocation": {"latitude": 1.0, "longitude": 2.0}}},
             "variable": {"variableCode": [{"value": "00065"}], "unit": {"unitCode": "ft"}},
             "values": []}
        ]}}"#;
        let Ok(document) = serde_json::from_str::<Value>(body) else {
            return;
        };
        match parse(&document) {
            Err(GaugeError::Shape { .. }) => {}
            other => assert!(other.is_err(), "an empty series must not read as a gauge"),
        }
    }

    #[test]
    fn the_axis_starts_just_below_the_river_rather_than_at_its_datum() {
        let mut history = History {
            site: String::from("1"),
            name: String::from("x"),
            stage: Vec::new(),
            discharge: Vec::new(),
        };
        for (index, value) in [5.06, 5.6, 6.21].iter().enumerate() {
            history.stage.push(Reading {
                value: *value,
                unit: String::from("ft"),
                at_ms: index as u64,
                provisional: false,
                qualifiers: Vec::new(),
            });
        }
        let floor = history.axis_floor();
        let top = history.axis_top();
        assert!(floor <= 5.06, "{floor}");
        assert!(floor >= 4.5, "{floor} loses the river's whole range");
        assert!(top >= 6.21 && top <= 7.0, "{top}");
        assert!(
            top - floor <= 2.5,
            "{floor}..{top} is more than the river moves"
        );
    }

    #[test]
    fn an_axis_rounds_up_to_a_readable_step() {
        let history = History {
            site: String::from("1"),
            name: String::from("x"),
            stage: vec![
                Reading {
                    value: 2.0,
                    unit: String::from("ft"),
                    at_ms: 1,
                    provisional: false,
                    qualifiers: Vec::new(),
                },
                Reading {
                    value: 6.4,
                    unit: String::from("ft"),
                    at_ms: 2,
                    provisional: false,
                    qualifiers: Vec::new(),
                },
            ],
            discharge: Vec::new(),
        };
        let top = history.axis_top();
        assert!(top >= 6.4, "{top}");
        assert!(top <= 9.0, "{top}");
        let range = history.stage_range();
        assert!(
            range.is_some_and(|value| (value - 4.4).abs() < 1e-9),
            "{range:?}"
        );
    }

    #[test]
    fn a_wide_view_still_asks_for_a_box_the_service_will_answer() {
        let (south, west, north, east) = bounds(37.0, -97.0, 4_000.0);
        let area = (north - south) * (east - west);
        assert!(
            area <= 25.0,
            "{area} square degrees is more than the service allows"
        );
        assert!(
            area <= 4.0,
            "{area} square degrees is more than a gauge query should carry"
        );
        let (south, west, north, east) = bounds(60.0, 20.0, 900.0);
        let area = (north - south) * (east - west);
        assert!(area <= 25.0, "{area} square degrees at high latitude");
    }

    #[test]
    fn the_requests_name_the_box_and_the_parameters() {
        let url = request_url((37.0, -98.0, 38.0, -97.0));
        assert!(
            url.contains("bBox=-98.0000,37.0000,-97.0000,38.0000"),
            "{url}"
        );
        assert!(url.contains("parameterCd=00065,00060"), "{url}");
        assert!(history_url("07144570").contains("sites=07144570"));
    }

    #[test]
    fn a_distance_is_measured_from_the_view_centre() -> Result<(), GaugeError> {
        let set = parse(&document())?;
        let gauge = set.gauges.first().ok_or(GaugeError::Shape {
            detail: "a first gauge",
        })?;
        let near = gauge.distance_from(37.48, -97.26);
        let far = gauge.distance_from(40.0, -100.0);
        assert!(near < far);
        assert!(near < 2.0, "{near}");
        Ok(())
    }
}
