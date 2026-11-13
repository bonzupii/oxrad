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
//! Sea ice for one calendar month, from the National Snow and Ice Data
//! Center's Sea Ice Index.
//!
//! The index is a monthly series of ice extent and ice area for each
//! hemisphere, one file per calendar month, so a month's file answers with
//! every year since 1978. That makes the comparison the rail shows a like-for-
//! like one: this November against every November on record, and against the
//! 1981-2010 mean of the same file.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Instant;

use crate::acquire::Acquire;
use crate::dispatch;
use crate::engine::Served;
use crate::provider::FetchError;
use crate::sources;
use crate::time;

/// The provider, in the words the rail uses.
pub const SOURCE: &str = "NSIDC Sea Ice Index v4";
/// The baseline the anomaly is taken against.
pub const BASELINE_FROM: i32 = 1981;
pub const BASELINE_TO: i32 = 2010;

/// Which half of the planet the ice is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hemisphere {
    North,
    South,
}

impl Hemisphere {
    pub const fn label(self) -> &'static str {
        match self {
            Self::North => "Arctic",
            Self::South => "Antarctic",
        }
    }

    /// The letter the provider's file names and rows use.
    const fn code(self) -> char {
        match self {
            Self::North => 'N',
            Self::South => 'S',
        }
    }
}

/// One year's ice in the month.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IceYear {
    pub year: i32,
    /// Ice extent, millions of square kilometres: every cell with at least
    /// fifteen per cent ice.
    pub extent_mkm2: f64,
    /// Ice area, millions of square kilometres: cells weighted by their
    /// concentration, so area is always the smaller of the two.
    pub area_mkm2: Option<f64>,
}

/// One calendar month of the index, every year it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct SeaIceMonth {
    pub hemisphere: Hemisphere,
    pub month: u32,
    /// Ordered by year, earliest first.
    pub years: Vec<IceYear>,
    /// Rows the file carried that could not be read as a year of ice.
    pub unreadable: usize,
}

impl SeaIceMonth {
    /// The newest year in the file, which is what the rail reports.
    pub fn latest(&self) -> Option<IceYear> {
        self.years.last().copied()
    }

    /// The mean extent over the baseline years the file covers.
    pub fn baseline_extent(&self) -> Option<f64> {
        let mut total = 0.0;
        let mut count = 0.0;
        for year in &self.years {
            if (BASELINE_FROM..=BASELINE_TO).contains(&year.year) {
                total += year.extent_mkm2;
                count += 1.0;
            }
        }
        if count == 0.0 {
            return None;
        }
        Some(total / count)
    }

    /// How far the newest year sits from the baseline, in millions of square
    /// kilometres. Negative means less ice than the baseline.
    pub fn anomaly_mkm2(&self) -> Option<f64> {
        let latest = self.latest()?;
        let baseline = self.baseline_extent()?;
        Some(latest.extent_mkm2 - baseline)
    }

    /// Where the newest year stands among the years on record, counting from
    /// the smallest extent: 1 is the least ice this month has ever had.
    pub fn rank_from_lowest(&self) -> Option<usize> {
        let latest = self.latest()?;
        let position = self
            .years
            .iter()
            .filter(|year| year.extent_mkm2 < latest.extent_mkm2)
            .count();
        Some(position + 1)
    }

    /// The least and the most ice this month has had, as years.
    pub fn smallest(&self) -> Option<IceYear> {
        self.years
            .iter()
            .copied()
            .min_by(|left, right| left.extent_mkm2.total_cmp(&right.extent_mkm2))
    }

    /// The extent the chart's axis has to hold, with a little headroom.
    pub fn axis_top(&self) -> f64 {
        let highest = self
            .years
            .iter()
            .fold(0.0_f64, |top, year| top.max(year.extent_mkm2));
        (highest * 1.08).max(1.0)
    }
}

/// Why a month of the index could not be read.
#[derive(Debug)]
pub enum SeaIceError {
    Provider { source: FetchError },
    Shape { detail: &'static str },
    WorkerStopped,
}

impl Display for SeaIceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { source } => write!(formatter, "The sea-ice request failed: {source}"),
            Self::Shape { detail } => {
                write!(
                    formatter,
                    "The sea-ice file is not usable: it wants {detail}."
                )
            }
            Self::WorkerStopped => write!(
                formatter,
                "The sea-ice worker stopped, so the index cannot be read."
            ),
        }
    }
}

impl Error for SeaIceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source } => Some(source),
            Self::Shape { .. } | Self::WorkerStopped => None,
        }
    }
}

impl SeaIceError {}

/// A shape complaint naming what the file lacked.
const fn shape(detail: &'static str) -> SeaIceError {
    SeaIceError::Shape { detail }
}

/// Reads one of the index's monthly files.
pub fn parse(text: &str, hemisphere: Hemisphere) -> Result<SeaIceMonth, SeaIceError> {
    let mut years = Vec::new();
    let mut unreadable = 0;
    let mut month = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.first().is_some_and(|field| *field == "year") {
            continue;
        }
        if fields.len() < 6 {
            unreadable += 1;
            continue;
        }
        let (Some(year), Some(month_field), Some(extent)) = (
            fields.first().and_then(|field| field.parse::<i32>().ok()),
            fields.get(1).and_then(|field| field.parse::<u32>().ok()),
            fields.get(4).and_then(|field| field.parse::<f64>().ok()),
        ) else {
            unreadable += 1;
            continue;
        };
        let area = fields
            .get(5)
            .and_then(|field| field.parse::<f64>().ok())
            .filter(|value| *value > 0.0);
        if !(1..=12).contains(&month_field) || !extent.is_finite() || extent < 0.0 {
            unreadable += 1;
            continue;
        }
        month = Some(month_field);
        years.push(IceYear {
            year,
            extent_mkm2: extent,
            area_mkm2: area,
        });
    }
    let Some(month) = month else {
        return Err(shape("a year of ice in the month"));
    };
    years.sort_by_key(|year| year.year);
    Ok(SeaIceMonth {
        hemisphere,
        month,
        years,
        unreadable,
    })
}

/// Both hemispheres for one calendar month.
#[derive(Debug, Clone, PartialEq)]
pub struct SeaIce {
    pub month: u32,
    pub north: SeaIceMonth,
    pub south: SeaIceMonth,
}
/// Fetches both hemispheres for one calendar month.
pub fn fetch(acquire: &Acquire, month: u32) -> Result<Served<SeaIce>, SeaIceError> {
    let north = fetch_one(acquire, Hemisphere::North, month)?;
    let south = fetch_one(acquire, Hemisphere::South, month)?;
    let value = SeaIce {
        month,
        north: north.value,
        south: south.value,
    };
    Ok(Served::new(value, north.provenance, north.cache)
        .with_request(south.provenance, south.cache))
}

fn fetch_one(
    acquire: &Acquire,
    hemisphere: Hemisphere,
    month: u32,
) -> Result<Served<SeaIceMonth>, SeaIceError> {
    let folder = match hemisphere {
        Hemisphere::North => "north",
        Hemisphere::South => "south",
    };
    let month_text = format!("{month:02}");
    let code = hemisphere.code().to_string();
    let fetched = acquire
        .fetch_operation(
            &sources::NSIDC_SEA_ICE,
            "month",
            &[
                ("hemisphere", folder),
                ("code", code.as_str()),
                ("month", month_text.as_str()),
            ],
        )
        .map_err(|source| SeaIceError::Provider { source })?;
    let text = fetched
        .text()
        .map_err(|source| SeaIceError::Provider { source })?;
    let value = parse(&text, hemisphere)?;
    Ok(Served::new(value, fetched.provenance, fetched.cache))
}

/// The month the rail asks for: the calendar month it is now.
pub fn current_month() -> Result<u32, SeaIceError> {
    let now_ms = match time::now_unix_ms() {
        Ok(now_ms) => now_ms,
        Err(_) => return Err(shape("a readable clock to pick the month")),
    };
    let days = i64::try_from(now_ms / time::MS_PER_DAY)
        .map_err(|_| shape("a date inside the calendar"))?;
    let (_, month, _) = time::civil_from_days(days);
    Ok(month)
}

/// Requests sent to the worker.
#[derive(Debug)]
pub struct SeaIceRequest {
    pub generation: u64,
    pub month: u32,
}

/// Answers from the worker.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "the served value travels whole so its provenance cannot be separated from it"
)]
pub enum SeaIceEvent {
    Ready {
        generation: u64,
        record: Served<SeaIce>,
    },
    Failed {
        generation: u64,
        error: SeaIceError,
    },
}

/// Reads the index on the shared job queue.
pub struct SeaIceFeed {
    feed: dispatch::Feed<SeaIceEvent>,
    generation: u64,
    stopped: bool,
}

impl SeaIceFeed {
    pub fn new(acquire: Acquire) -> Self {
        Self {
            feed: dispatch::Feed::new(acquire),
            generation: 0,
            stopped: false,
        }
    }

    pub fn request(&mut self, request: &SeaIceRequest) -> Result<(), SeaIceError> {
        self.generation = request.generation;
        let generation = request.generation;
        let month = request.month;
        self.feed
            .request(move |acquire| match fetch(acquire, month) {
                Ok(record) => SeaIceEvent::Ready { generation, record },
                Err(error) => SeaIceEvent::Failed { generation, error },
            })
            .map_err(|_| {
                self.stopped = true;
                SeaIceError::WorkerStopped
            })
    }

    pub fn poll(&mut self) -> Option<SeaIceEvent> {
        match self.feed.poll() {
            Ok(Some(event)) => Some(event),
            Ok(None) => None,
            Err(dispatch::Disconnected) => {
                if self.stopped {
                    return None;
                }
                self.stopped = true;
                Some(SeaIceEvent::Failed {
                    generation: self.generation,
                    error: SeaIceError::WorkerStopped,
                })
            }
        }
    }
}

/// Where the layer's request stands.
#[derive(Debug)]
pub enum SeaIceState {
    Idle,
    Loading {
        last: Option<Served<SeaIce>>,
    },
    Ready {
        served: Served<SeaIce>,
    },
    Failed {
        error: SeaIceError,
        last: Option<Served<SeaIce>>,
    },
}

impl SeaIceState {
    /// The most recent index still worth drawing.
    pub const fn last(&self) -> Option<&Served<SeaIce>> {
        match self {
            Self::Ready { served } => Some(served),
            Self::Loading { last } | Self::Failed { last, .. } => last.as_ref(),
            Self::Idle => None,
        }
    }
}

/// The worker and the value it last delivered.
pub struct SeaIceView {
    feed: SeaIceFeed,
    state: SeaIceState,
    generation: u64,
}

impl SeaIceView {
    pub fn new(acquire: Acquire) -> Self {
        Self {
            feed: SeaIceFeed::new(acquire),
            state: SeaIceState::Idle,
            generation: 0,
        }
    }

    pub const fn state(&self) -> &SeaIceState {
        &self.state
    }

    pub fn record(&self) -> Option<&SeaIce> {
        self.state.last().map(|served| &served.value)
    }

    pub const fn served(&self) -> Option<&Served<SeaIce>> {
        self.state.last()
    }

    /// The month the index was last asked for, so a new month re-asks.
    pub fn start(&mut self, month: u32) {
        let last = take_last(&mut self.state);
        self.state = SeaIceState::Loading { last };
        self.generation = self.generation.wrapping_add(1);
        let request = SeaIceRequest {
            generation: self.generation,
            month,
        };
        if let Err(error) = self.feed.request(&request) {
            self.state = SeaIceState::Failed { error, last: None };
        }
    }

    /// Takes in the worker's answers. The engine owns retry and cache.
    pub fn update(&mut self, _now: Instant) -> bool {
        while let Some(event) = self.feed.poll() {
            match event {
                SeaIceEvent::Ready { generation, record } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.state = SeaIceState::Ready { served: record };
                }
                SeaIceEvent::Failed { generation, error } => {
                    if generation != self.generation {
                        continue;
                    }
                    let last = take_last(&mut self.state);
                    self.state = SeaIceState::Failed { error, last };
                }
            }
        }
        false
    }
}

/// Empties the state and returns the record it was holding.
fn take_last(state: &mut SeaIceState) -> Option<Served<SeaIce>> {
    match std::mem::replace(state, SeaIceState::Idle) {
        SeaIceState::Loading { last } | SeaIceState::Failed { last, .. } => last,
        SeaIceState::Ready { served } => Some(served),
        SeaIceState::Idle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "year, mo,source_dataset, region, extent,   area
1979, 11,    NSIDC-0051,      N,   9.29,   7.60
1981, 11,    NSIDC-0051,      N,   9.90,   8.20
1995, 11,    NSIDC-0051,      N,   9.30,   7.70
2010, 11,    NSIDC-0051,      N,   9.11,   7.51
2024, 11,    NSIDC-0051,      N,   9.15,   7.74
2025, 11,    NSIDC-0803,      N,   8.77,   7.36
2026, 11,    broken row
";

    fn record() -> SeaIceMonth {
        parse(FILE, Hemisphere::North).unwrap_or_else(|_| SeaIceMonth {
            hemisphere: Hemisphere::North,
            month: 11,
            years: Vec::new(),
            unreadable: 0,
        })
    }

    #[test]
    fn a_month_file_is_read_with_its_years_in_order() {
        let record = record();
        assert_eq!(record.month, 11);
        assert_eq!(record.years.len(), 6);
        assert_eq!(
            record.unreadable, 1,
            "the broken row is counted, not guessed"
        );
        assert_eq!(record.latest().map(|year| year.year), Some(2025));
    }

    #[test]
    fn the_baseline_is_the_mean_of_the_years_it_names() {
        let record = record();
        let baseline = record.baseline_extent();
        // 1981, 1995, and 2010 are the years in 1981-2010.
        assert_eq!(baseline, Some((9.90 + 9.30 + 9.11) / 3.0));
    }

    #[test]
    fn the_anomaly_is_against_that_baseline() {
        let record = record();
        let anomaly = record.anomaly_mkm2();
        let expected = 8.77 - (9.90 + 9.30 + 9.11) / 3.0;
        assert!(anomaly.is_some_and(|value| (value - expected).abs() < 1e-9));
        assert!(
            anomaly.is_some_and(|value| value < 0.0),
            "less ice than the baseline"
        );
    }

    #[test]
    fn the_newest_year_is_ranked_from_the_least_ice() {
        let record = record();
        // 8.77 is the smallest of the six years, so it is the first.
        assert_eq!(record.rank_from_lowest(), Some(1));
        assert_eq!(record.smallest().map(|year| year.year), Some(2025));
    }

    #[test]
    fn a_file_without_a_year_is_refused() {
        let text = "year, mo,source_dataset, region, extent,   area\n";
        assert!(matches!(
            parse(text, Hemisphere::North),
            Err(SeaIceError::Shape { .. })
        ));
    }
}
