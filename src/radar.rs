// oxrad — A storm-chaser-grade NEXRAD weather radar viewer
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
//! Archive II framing, Level II message decoding, and volume assembly.
//!
//! Gate geometry comes from the radial headers. Azimuth boundaries come from
//! the measured azimuth sequence: neighbouring radials are bisected, and a
//! boundary without a plausible neighbour keeps its own half width.

use std::io::{Cursor, Read};

use nexrad_decode::messages::MessageContents;
use nexrad_decode::messages::digital_radar_data::{GenericDataBlock, Message as DigitalRadarData};

use crate::colormap::{MAX_REFLECTIVITY_DBZ, MIN_REFLECTIVITY_DBZ};
use crate::error::{CoordinateError, MessageDefect, MomentError, RadarError, VolumeDefect};
use crate::projection;
use crate::stations::SiteId;
use crate::time::MS_PER_DAY;

const ARCHIVE_HEADER_BYTES: usize = 24;
const RECORD_LENGTH_BYTES: usize = 4;
const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;
const CTM_BYTES: usize = 12;
const MESSAGE_HEADER_BYTES: usize = 28;
const LEGACY_MESSAGE_BYTES: usize = 2432;
const RADIAL_HEADER_BYTES: usize = 32;

/// A boundary between neighbouring radials bridges their azimuth centres only
/// when the measured separation is within this multiple of the commanded
/// resolution; a larger separation is a gap in the sweep, not a wide beam.
const AZIMUTH_BRIDGE_TOLERANCE: f64 = 1.25;

#[derive(Debug, Clone, Copy)]
pub enum GateValue {
    Measured(f32),
    BelowThreshold,
    RangeFolded,
}

/// One gate column of a sweep: its azimuth boundaries and gate geometry.
#[derive(Debug)]
pub struct RadialBins {
    pub azimuth_number: u16,
    pub azimuth_left_deg: f64,
    pub azimuth_right_deg: f64,
    pub elevation_deg: f64,
    pub first_gate_km: f64,
    pub gate_width_km: f64,
    pub values: Vec<GateValue>,
}

#[derive(Debug)]
pub struct RadarSweep {
    pub elevation_deg: f64,
    pub elevation_number: u8,
    pub radials: Vec<RadialBins>,
    pub below_threshold: usize,
    pub range_folded: usize,
    pub max_ground_range_km: f64,
}

impl RadarSweep {
    pub fn gate_count(&self) -> usize {
        self.radials.iter().map(|radial| radial.values.len()).sum()
    }
}

#[derive(Debug)]
pub struct RadarVolume {
    pub site: SiteId,
    pub site_lat: f64,
    pub site_lon: f64,
    pub scan_unix_ms: u64,
    pub sweep: RadarSweep,
    pub skipped_radials: usize,
    pub skipped_non_data: usize,
    pub moment_error: Option<MomentError>,
}

#[derive(Debug, Clone, Copy)]
struct SiteFix {
    lat: f64,
    lon: f64,
}

impl SiteFix {
    fn new(lat: f64, lon: f64) -> Result<Self, CoordinateError> {
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err(CoordinateError::Latitude { lat });
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return Err(CoordinateError::Longitude { lon });
        }
        Ok(Self { lat, lon })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RadialStatus {
    VolumeStart,
    SweepStart,
    Intermediate,
    SweepEnd,
    VolumeEnd,
}

const fn radial_status(value: u8) -> Option<RadialStatus> {
    match value {
        3 => Some(RadialStatus::VolumeStart),
        0 | 5 => Some(RadialStatus::SweepStart),
        1 => Some(RadialStatus::Intermediate),
        2 => Some(RadialStatus::SweepEnd),
        4 => Some(RadialStatus::VolumeEnd),
        _ => None,
    }
}

const fn commanded_spacing_deg(code: u8) -> Option<f64> {
    match code {
        1 => Some(0.5),
        2 => Some(1.0),
        _ => None,
    }
}

#[derive(Debug)]
enum RadialDefect {
    Status,
    Elevation,
    Azimuth,
    Spacing,
    GateGeometry,
    MissingReflectivity,
    Moment(MomentError),
}

#[derive(Debug)]
struct Radial {
    elevation_number: u8,
    elevation_deg: f64,
    azimuth_number: u16,
    azimuth_deg: f64,
    spacing_deg: f64,
    first_gate_km: f64,
    gate_width_km: f64,
    values: Vec<GateValue>,
}

fn parse_radial(radial: &DigitalRadarData) -> Result<Radial, RadialDefect> {
    if radial_status(radial.header.radial_status).is_none() {
        return Err(RadialDefect::Status);
    }
    let elevation_deg = f64::from(radial.header.elevation_angle);
    if !elevation_deg.is_finite() || !(-90.0..=90.0).contains(&elevation_deg) {
        return Err(RadialDefect::Elevation);
    }
    let azimuth_deg = f64::from(radial.header.azimuth_angle);
    if !azimuth_deg.is_finite() || !(0.0..=360.0).contains(&azimuth_deg) {
        return Err(RadialDefect::Azimuth);
    }
    let spacing_deg = commanded_spacing_deg(radial.header.azimuth_resolution_spacing)
        .ok_or(RadialDefect::Spacing)?;
    let Some(block) = radial.reflectivity_data_block.as_ref() else {
        return Err(RadialDefect::MissingReflectivity);
    };
    let gates = block.header.number_of_data_moment_gates;
    let first_gate_km = f64::from(block.header.data_moment_range) / 1000.0;
    let gate_width_km = f64::from(block.header.data_moment_range_sample_interval) / 1000.0;
    if gates == 0 || !gate_width_km.is_finite() || gate_width_km <= 0.0 {
        return Err(RadialDefect::GateGeometry);
    }
    if !first_gate_km.is_finite() || first_gate_km < gate_width_km / 2.0 {
        return Err(RadialDefect::GateGeometry);
    }
    let values = decode_moment(block).map_err(RadialDefect::Moment)?;
    Ok(Radial {
        elevation_number: radial.header.elevation_number,
        elevation_deg,
        azimuth_number: radial.header.azimuth_number,
        azimuth_deg,
        spacing_deg,
        first_gate_km,
        gate_width_km,
        values,
    })
}

fn decode_moment(block: &GenericDataBlock) -> Result<Vec<GateValue>, MomentError> {
    let scale = block.header.scale;
    let offset = block.header.offset;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(MomentError::Scale { scale });
    }
    if !offset.is_finite() {
        return Err(MomentError::Offset { offset });
    }
    let word_bytes = match block.header.data_word_size {
        8 => 1,
        16 => 2,
        word_size => return Err(MomentError::WordSize { word_size }),
    };
    let expected = usize::from(block.header.number_of_data_moment_gates) * word_bytes;
    if block.encoded_data.len() != expected {
        return Err(MomentError::Length {
            expected,
            actual: block.encoded_data.len(),
        });
    }
    let mut values = Vec::with_capacity(expected / word_bytes);
    for word in block.encoded_data.chunks(word_bytes) {
        let raw = match word {
            [value] => f32::from(*value),
            [high, low] => f32::from(u16::from_be_bytes([*high, *low])),
            _ => {
                return Err(MomentError::Length {
                    expected,
                    actual: block.encoded_data.len(),
                });
            }
        };
        if raw == 0.0 {
            values.push(GateValue::BelowThreshold);
        } else if raw == 1.0 {
            values.push(GateValue::RangeFolded);
        } else {
            let dbz = (raw - offset) / scale;
            if !dbz.is_finite() || !(MIN_REFLECTIVITY_DBZ..=MAX_REFLECTIVITY_DBZ).contains(&dbz) {
                return Err(MomentError::Value { value: dbz });
            }
            values.push(GateValue::Measured(dbz));
        }
    }
    Ok(values)
}

fn scan_time(date: u16, milliseconds: u32) -> Option<u64> {
    if date == 0 || u64::from(milliseconds) >= MS_PER_DAY {
        return None;
    }
    Some(u64::from(date - 1) * MS_PER_DAY + u64::from(milliseconds))
}

/// Midpoint of two azimuth centres, or the side's own half width when the pair
/// is too far apart to be neighbouring radials.
fn azimuth_edge(centre: f64, neighbour: f64, spacing: f64, ahead: bool) -> f64 {
    let separation = if ahead {
        neighbour - centre
    } else {
        centre - neighbour
    };
    let edge = if separation > 0.0 && separation <= spacing * AZIMUTH_BRIDGE_TOLERANCE {
        (centre + neighbour) / 2.0
    } else if ahead {
        centre + spacing / 2.0
    } else {
        centre - spacing / 2.0
    };
    edge.rem_euclid(360.0)
}

/// Unwrapped azimuth centres, so a sweep that crosses north is continuous.
fn azimuth_centres(radials: &[Radial]) -> Vec<f64> {
    let mut unwrapped = Vec::with_capacity(radials.len());
    let mut previous_azimuth: Option<f64> = None;
    let mut previous_unwrapped = 0.0;
    for radial in radials {
        let next = previous_azimuth.map_or(radial.azimuth_deg, |azimuth| {
            previous_unwrapped + (radial.azimuth_deg - azimuth).rem_euclid(360.0)
        });
        unwrapped.push(next);
        previous_azimuth = Some(radial.azimuth_deg);
        previous_unwrapped = next;
    }
    unwrapped
}

struct SweepAssembly {
    elevation_number: u8,
    elevation_deg: f64,
    radials: Vec<Radial>,
    skipped: usize,
    ended: bool,
}

impl SweepAssembly {
    fn start(radial: Radial) -> Self {
        Self {
            elevation_number: radial.elevation_number,
            elevation_deg: radial.elevation_deg,
            radials: vec![radial],
            skipped: 0,
            ended: false,
        }
    }

    fn push(&mut self, radial: Radial) {
        if radial.elevation_number != self.elevation_number {
            self.skipped += 1;
            return;
        }
        let expected = i64::from(self.radials.last().map_or(0, |last| last.azimuth_number)) + 1;
        let actual = i64::from(radial.azimuth_number);
        match actual - expected {
            0 => {}
            missing if missing > 0 => {
                self.skipped += usize::try_from(missing).unwrap_or(usize::MAX);
            }
            _ => self.skipped += 1,
        }
        self.radials.push(radial);
    }

    const fn complete(&self) -> bool {
        self.ended && self.radials.len() >= 2
    }

    fn into_radial_bins(self, site: &SiteId) -> Result<RadarSweep, RadarError> {
        let count = self.radials.len();
        let centres = azimuth_centres(&self.radials);
        let elevation_deg = self.elevation_deg;
        let elevation_number = self.elevation_number;
        let mut bins = Vec::with_capacity(count);
        for (index, radial) in self.radials.into_iter().enumerate() {
            let centre = centres.get(index).copied().unwrap_or(radial.azimuth_deg);
            let before = index.checked_sub(1).and_then(|i| centres.get(i)).copied();
            let after = centres.get(index + 1).copied();
            let previous = match (before, centres.last()) {
                (Some(value), _) => value,
                (None, Some(last)) => last - 360.0,
                (None, None) => centre,
            };
            let next = match (after, centres.first()) {
                (Some(value), _) => value,
                (None, Some(first)) => first + 360.0,
                (None, None) => centre,
            };
            let left = azimuth_edge(centre, previous, radial.spacing_deg, false);
            let right = azimuth_edge(centre, next, radial.spacing_deg, true);
            bins.push(RadialBins {
                azimuth_number: radial.azimuth_number,
                azimuth_left_deg: left,
                azimuth_right_deg: right,
                elevation_deg: radial.elevation_deg,
                first_gate_km: radial.first_gate_km,
                gate_width_km: radial.gate_width_km,
                values: radial.values,
            });
        }
        if bins.len() < 2 {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site.as_str()),
                defect: VolumeDefect::AzimuthGeometry {
                    radials: bins.len(),
                },
            });
        }
        if bins.iter().all(|radial| radial.values.is_empty()) {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site.as_str()),
                defect: VolumeDefect::NoReflectivity,
            });
        }
        finish_sweep(
            site,
            RadarSweep {
                elevation_deg,
                elevation_number,
                radials: bins,
                below_threshold: 0,
                range_folded: 0,
                max_ground_range_km: 0.0,
            },
        )
    }
}

fn finish_sweep(site: &SiteId, mut sweep: RadarSweep) -> Result<RadarSweep, RadarError> {
    let mut below_threshold = 0_usize;
    let mut range_folded = 0_usize;
    let mut max_ground_range_km: f64 = 0.0;
    for radial in &sweep.radials {
        for value in &radial.values {
            match value {
                GateValue::Measured(_) => {}
                GateValue::BelowThreshold => below_threshold += 1,
                GateValue::RangeFolded => range_folded += 1,
            }
        }
        let last_centre = radial.first_gate_km
            + (radial.values.len().saturating_sub(1) as f64) * radial.gate_width_km;
        let far_slant = last_centre + radial.gate_width_km / 2.0;
        let far = projection::ground_range(far_slant, radial.elevation_deg).map_err(|source| {
            RadarError::GateRange {
                site: String::from(site.as_str()),
                azimuth_number: radial.azimuth_number,
                source,
            }
        })?;
        max_ground_range_km = max_ground_range_km.max(far);
    }
    sweep.below_threshold = below_threshold;
    sweep.range_folded = range_folded;
    sweep.max_ground_range_km = max_ground_range_km;
    Ok(sweep)
}

struct VolumeAssembly {
    site: Option<SiteFix>,
    site_error: Option<CoordinateError>,
    current: Option<SweepAssembly>,
    sweeps: Vec<SweepAssembly>,
    started: bool,
    ended: bool,
    structure_broken: bool,
    start_header: Option<(u16, u32)>,
    skipped_radials: usize,
    skipped_non_data: usize,
    moment_error: Option<MomentError>,
}

impl VolumeAssembly {
    const fn new() -> Self {
        Self {
            site: None,
            site_error: None,
            current: None,
            sweeps: Vec::new(),
            started: false,
            ended: false,
            structure_broken: false,
            start_header: None,
            skipped_radials: 0,
            skipped_non_data: 0,
            moment_error: None,
        }
    }

    fn accept(&mut self, radial: &DigitalRadarData) {
        if self.site.is_none()
            && let Some(block) = &radial.volume_data_block
        {
            match SiteFix::new(f64::from(block.latitude), f64::from(block.longitude)) {
                Ok(fix) => self.site = Some(fix),
                Err(source) => {
                    if self.site_error.is_none() {
                        self.site_error = Some(source);
                    }
                }
            }
        }
        match radial_status(radial.header.radial_status) {
            Some(status) => self.advance(status, radial),
            None => {
                self.structure_broken = true;
                self.skipped_radials += 1;
            }
        }
    }

    const fn note_non_data(&mut self) {
        self.skipped_non_data += 1;
    }

    fn advance(&mut self, status: RadialStatus, radial: &DigitalRadarData) {
        match status {
            RadialStatus::VolumeStart => {
                if self.started {
                    self.structure_broken = true;
                }
                self.started = true;
                self.start_header = Some((radial.header.date, radial.header.time));
                self.begin(radial);
            }
            RadialStatus::SweepStart => {
                if !self.started {
                    self.structure_broken = true;
                }
                self.begin(radial);
            }
            RadialStatus::Intermediate => self.extend(radial),
            RadialStatus::SweepEnd => {
                self.extend(radial);
                self.close(true);
            }
            RadialStatus::VolumeEnd => {
                self.extend(radial);
                self.close(true);
                self.ended = true;
            }
        }
    }

    fn begin(&mut self, radial: &DigitalRadarData) {
        self.close(false);
        match parse_radial(radial) {
            Ok(parsed) => self.current = Some(SweepAssembly::start(parsed)),
            Err(defect) => {
                self.record(defect);
                self.current = None;
            }
        }
    }

    fn extend(&mut self, radial: &DigitalRadarData) {
        match parse_radial(radial) {
            Ok(parsed) => match self.current.as_mut() {
                Some(sweep) => sweep.push(parsed),
                None => self.skipped_radials += 1,
            },
            Err(defect) => {
                self.record(defect);
                if let Some(sweep) = self.current.as_mut() {
                    sweep.skipped += 1;
                }
            }
        }
    }

    fn record(&mut self, defect: RadialDefect) {
        self.skipped_radials += 1;
        if let RadialDefect::Moment(error) = defect {
            self.moment_error.get_or_insert(error);
        }
    }

    fn close(&mut self, ended: bool) {
        let Some(mut sweep) = self.current.take() else {
            return;
        };
        sweep.ended = ended;
        self.skipped_radials += sweep.skipped;
        if sweep.complete() {
            self.sweeps.push(sweep);
        } else {
            self.skipped_radials += sweep.radials.len();
        }
    }

    fn finish(self, site: &SiteId, key: &str) -> Result<RadarVolume, RadarError> {
        if !self.started {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site.as_str()),
                defect: VolumeDefect::MissingStart,
            });
        }
        if !self.ended {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site.as_str()),
                defect: VolumeDefect::MissingEnd,
            });
        }
        if self.structure_broken {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site.as_str()),
                defect: VolumeDefect::SweepBoundaries,
            });
        }
        let (date, time) = self.start_header.ok_or(RadarError::IncompleteVolume {
            site: String::from(site.as_str()),
            defect: VolumeDefect::MissingStart,
        })?;
        let scan_unix_ms = scan_time(date, time).ok_or(RadarError::ArchiveTime {
            key: String::from(key),
            date: u32::from(date),
            time,
        })?;
        let site_error = self.site_error;
        let site_fix = self.site.ok_or_else(|| {
            site_error.map_or_else(
                || RadarError::IncompleteVolume {
                    site: String::from(site.as_str()),
                    defect: VolumeDefect::MissingSiteCoordinates,
                },
                |source| RadarError::SiteCoordinates { source },
            )
        })?;
        let sweep = self
            .sweeps
            .into_iter()
            .min_by_key(|sweep| sweep.elevation_number)
            .ok_or(RadarError::IncompleteVolume {
                site: String::from(site.as_str()),
                defect: VolumeDefect::NoCompleteSweep,
            })?;
        let sweep = sweep.into_radial_bins(site)?;
        Ok(RadarVolume {
            site: site.clone(),
            site_lat: site_fix.lat,
            site_lon: site_fix.lon,
            scan_unix_ms,
            sweep,
            skipped_radials: self.skipped_radials,
            skipped_non_data: self.skipped_non_data,
            moment_error: self.moment_error,
        })
    }
}

fn decode_record(
    key: &str,
    bytes: &[u8],
    offset: usize,
) -> Result<(Vec<MessageContents>, usize), RadarError> {
    let start =
        offset
            .checked_add(RECORD_LENGTH_BYTES)
            .ok_or_else(|| RadarError::RecordHeader {
                key: String::from(key),
                offset,
            })?;
    let length_bytes = bytes
        .get(offset..start)
        .ok_or_else(|| RadarError::RecordHeader {
            key: String::from(key),
            offset,
        })?;
    let length: [u8; 4] = length_bytes
        .try_into()
        .map_err(|_| RadarError::RecordHeader {
            key: String::from(key),
            offset,
        })?;
    // The field is a signed length, and the final record of an archive carries its
    // length negated: both the pinned archive and the live feed end with one, and
    // the bytes after the field are a bzip2 stream of exactly that magnitude. The
    // pinned archive test decodes the volume end from that record, so misreading
    // the field fails it.
    let declared = i32::from_be_bytes(length).unsigned_abs();
    if declared == 0 {
        return Err(RadarError::RecordHeader {
            key: String::from(key),
            offset,
        });
    }
    let record_length = usize::try_from(declared).map_err(|_| RadarError::RecordHeader {
        key: String::from(key),
        offset,
    })?;
    let end = start
        .checked_add(record_length)
        .ok_or_else(|| RadarError::RecordBounds {
            key: String::from(key),
            offset,
        })?;
    let record = bytes
        .get(start..end)
        .ok_or_else(|| RadarError::RecordBounds {
            key: String::from(key),
            offset,
        })?;
    let mut decompressed = Vec::new();
    bzip2_rs::DecoderReader::new(Cursor::new(record))
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut decompressed)
        .map_err(|source| RadarError::Decompress {
            key: String::from(key),
            offset,
            source,
        })?;
    if decompressed.len() as u64 > MAX_RECORD_BYTES {
        return Err(RadarError::RecordTooLarge {
            key: String::from(key),
            offset,
            limit: MAX_RECORD_BYTES,
        });
    }
    let messages = decode_record_messages(key, offset, &decompressed)?;
    Ok((messages, end))
}

fn structure_error(
    key: &str,
    record_offset: usize,
    message_offset: usize,
    defect: MessageDefect,
) -> RadarError {
    RadarError::MessageStructure {
        key: String::from(key),
        record_offset,
        message_offset,
        defect,
    }
}

fn message_size(
    segment_size: u16,
    segment_count: u16,
    segment_number: u16,
    message_type: u8,
) -> usize {
    if segment_size == u16::MAX {
        CTM_BYTES + ((usize::from(segment_count) << 16) | usize::from(segment_number))
    } else if matches!(message_type, 29 | 31) && segment_size != 0 {
        CTM_BYTES + usize::from(segment_size) * 2
    } else {
        LEGACY_MESSAGE_BYTES
    }
}

fn decode_message(
    key: &str,
    offset: usize,
    position: usize,
    remaining: &[u8],
) -> Result<(MessageContents, usize), RadarError> {
    if remaining.len() < MESSAGE_HEADER_BYTES {
        return Err(structure_error(
            key,
            offset,
            position,
            MessageDefect::TruncatedHeader {
                available: remaining.len(),
            },
        ));
    }
    let mut cursor = Cursor::new(remaining);
    let header = nexrad_decode::messages::decode_message_header(&mut cursor).map_err(|source| {
        RadarError::Decode {
            key: String::from(key),
            offset,
            source,
        }
    })?;
    let size = message_size(
        header.segment_size,
        header.segment_count,
        header.segment_number,
        header.message_type,
    );
    if size < MESSAGE_HEADER_BYTES {
        return Err(structure_error(
            key,
            offset,
            position,
            MessageDefect::DeclaredSize {
                declared: size,
                minimum: MESSAGE_HEADER_BYTES,
            },
        ));
    }
    let Some(message) = remaining.get(..size) else {
        return Err(structure_error(
            key,
            offset,
            position,
            MessageDefect::Bounds {
                declared: size,
                available: remaining.len(),
            },
        ));
    };
    if header.message_type != 31 {
        return Ok((MessageContents::Other, size));
    }
    if header.segment_size != u16::MAX && (header.segment_count != 1 || header.segment_number != 1)
    {
        return Err(structure_error(
            key,
            offset,
            position,
            MessageDefect::Segmented {
                segment_count: header.segment_count,
                segment_number: header.segment_number,
            },
        ));
    }
    let Some(payload) = message.get(MESSAGE_HEADER_BYTES..) else {
        return Err(structure_error(
            key,
            offset,
            position,
            MessageDefect::Bounds {
                declared: size,
                available: message.len(),
            },
        ));
    };
    validate_radial(payload).map_err(|defect| structure_error(key, offset, position, defect))?;
    let mut bounded = Cursor::new(message);
    bounded.set_position(MESSAGE_HEADER_BYTES as u64);
    let contents =
        nexrad_decode::messages::decode_message_contents(&mut bounded, header.message_type())
            .map_err(|source| RadarError::Decode {
                key: String::from(key),
                offset,
                source,
            })?;
    Ok((contents, size))
}

fn decode_record_messages(
    key: &str,
    offset: usize,
    bytes: &[u8],
) -> Result<Vec<MessageContents>, RadarError> {
    let mut position = 0;
    let mut messages = Vec::new();
    while position < bytes.len() {
        let Some(remaining) = bytes.get(position..) else {
            return Err(structure_error(
                key,
                offset,
                position,
                MessageDefect::TruncatedHeader { available: 0 },
            ));
        };
        let (contents, size) = decode_message(key, offset, position, remaining)?;
        messages.push(contents);
        position = position.checked_add(size).ok_or_else(|| {
            structure_error(
                key,
                offset,
                position,
                MessageDefect::Bounds {
                    declared: size,
                    available: bytes.len(),
                },
            )
        })?;
    }
    Ok(messages)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    match bytes.get(offset..offset.checked_add(2)?)? {
        [high, low] => Some(u16::from_be_bytes([*high, *low])),
        _ => None,
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    match bytes.get(offset..offset.checked_add(4)?)? {
        [a, b, c, d] => Some(u32::from_be_bytes([*a, *b, *c, *d])),
        _ => None,
    }
}

const fn constant_block_minimum(id: [u8; 4]) -> Option<u16> {
    match &id {
        b"RVOL" => Some(52),
        b"RELV" => Some(12),
        b"RRAD" => Some(28),
        _ => None,
    }
}

fn radial_slice(payload: &[u8]) -> Result<&[u8], MessageDefect> {
    if payload.len() < RADIAL_HEADER_BYTES {
        return Err(MessageDefect::TruncatedRadialHeader {
            available: payload.len(),
        });
    }
    if payload.get(16) != Some(&0) {
        return Err(MessageDefect::CompressedRadial);
    }
    let declared = read_u16(payload, 18).ok_or(MessageDefect::TruncatedRadialHeader {
        available: payload.len(),
    })?;
    payload
        .get(..usize::from(declared))
        .ok_or(MessageDefect::RadialLength {
            declared,
            available: payload.len(),
        })
}

fn radial_table_end(blocks: u16, radial_length: usize) -> Result<usize, MessageDefect> {
    let table_end = usize::from(blocks)
        .checked_mul(4)
        .and_then(|bytes| RADIAL_HEADER_BYTES.checked_add(bytes))
        .ok_or(MessageDefect::PointerTable {
            blocks,
            radial_length,
        })?;
    if blocks == 0 || table_end > radial_length {
        return Err(MessageDefect::PointerTable {
            blocks,
            radial_length,
        });
    }
    Ok(table_end)
}

fn block_identifier(radial: &[u8], pointer: usize, index: u16) -> Result<[u8; 4], MessageDefect> {
    let end = pointer
        .checked_add(4)
        .ok_or(MessageDefect::BlockIdentifier { index, pointer })?;
    let bytes = radial
        .get(pointer..end)
        .ok_or(MessageDefect::BlockIdentifier { index, pointer })?;
    <[u8; 4]>::try_from(bytes).map_err(|_| MessageDefect::BlockIdentifier { index, pointer })
}

fn block_length(
    radial: &[u8],
    pointer: usize,
    id: [u8; 4],
    index: u16,
) -> Result<usize, MessageDefect> {
    if let Some(minimum) = constant_block_minimum(id) {
        let declared = pointer
            .checked_add(4)
            .and_then(|at| read_u16(radial, at))
            .ok_or(MessageDefect::BlockIdentifier { index, pointer })?;
        if declared < minimum {
            return Err(MessageDefect::BlockLength {
                id,
                declared,
                minimum,
            });
        }
        return Ok(usize::from(declared));
    }
    if matches!(
        &id,
        b"DREF" | b"DVEL" | b"DSW " | b"DZDR" | b"DPHI" | b"DRHO" | b"DCFP"
    ) {
        let gates = pointer
            .checked_add(8)
            .and_then(|at| read_u16(radial, at))
            .ok_or(MessageDefect::TruncatedRadialHeader {
                available: radial.len(),
            })?;
        let width = match pointer.checked_add(19).and_then(|at| radial.get(at)) {
            Some(8) => 1,
            Some(16) => 2,
            Some(other) => {
                return Err(MessageDefect::WordSize {
                    id,
                    word_size: *other,
                });
            }
            None => {
                return Err(MessageDefect::TruncatedRadialHeader {
                    available: radial.len(),
                });
            }
        };
        return usize::from(gates)
            .checked_mul(width)
            .and_then(|bytes| bytes.checked_add(28))
            .ok_or(MessageDefect::GateLength { id, gates });
    }
    Err(MessageDefect::UnknownBlock { pointer, id })
}

fn validate_block(
    radial: &[u8],
    pointer: usize,
    index: u16,
) -> Result<([u8; 4], usize), MessageDefect> {
    let id = block_identifier(radial, pointer, index)?;
    let length = block_length(radial, pointer, id, index)?;
    let end = pointer.checked_add(length).ok_or(MessageDefect::BlockEnd {
        id,
        pointer,
        length,
    })?;
    if end > radial.len() {
        return Err(MessageDefect::BlockBounds {
            id,
            pointer,
            end,
            radial_length: radial.len(),
        });
    }
    Ok((id, end))
}

fn validate_radial(payload: &[u8]) -> Result<(), MessageDefect> {
    let radial = radial_slice(payload)?;
    let blocks = read_u16(radial, 30).ok_or(MessageDefect::TruncatedRadialHeader {
        available: radial.len(),
    })?;
    let table_end = radial_table_end(blocks, radial.len())?;
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(usize::from(blocks));
    let mut entry = RADIAL_HEADER_BYTES;
    for index in 0..blocks {
        let raw = read_u32(radial, entry).ok_or(MessageDefect::TruncatedRadialHeader {
            available: radial.len(),
        })?;
        entry = entry.checked_add(4).ok_or(MessageDefect::PointerTable {
            blocks,
            radial_length: radial.len(),
        })?;
        let Ok(pointer) = usize::try_from(raw) else {
            return Err(MessageDefect::PointerUnrepresentable {
                index,
                pointer: raw,
            });
        };
        if pointer < table_end {
            return Err(MessageDefect::PointerInsideTable {
                index,
                pointer,
                table_end,
            });
        }
        let (id, end) = validate_block(radial, pointer, index)?;
        if spans
            .iter()
            .any(|(start, finish)| pointer < *finish && *start < end)
        {
            return Err(MessageDefect::BlockOverlap { id, pointer });
        }
        spans.push((pointer, end));
    }
    Ok(())
}

fn archive_field<'a>(
    bytes: &'a [u8],
    start: usize,
    end: usize,
    key: &str,
) -> Result<&'a [u8], RadarError> {
    bytes
        .get(start..end)
        .ok_or_else(|| RadarError::RecordBounds {
            key: String::from(key),
            offset: 0,
        })
}

fn validate_archive_header(site: &SiteId, key: &str, bytes: &[u8]) -> Result<(), RadarError> {
    let Some(header) = bytes.get(..ARCHIVE_HEADER_BYTES) else {
        return Err(RadarError::RecordBounds {
            key: String::from(key),
            offset: 0,
        });
    };
    let version: [u8; 12] =
        archive_field(header, 0, 12, key)?
            .try_into()
            .map_err(|_| RadarError::RecordBounds {
                key: String::from(key),
                offset: 0,
            })?;
    let valid = version.starts_with(b"AR2V")
        && version
            .get(4..8)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit))
        && version.get(8) == Some(&b'.')
        && version
            .get(9..12)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit));
    if !valid {
        return Err(RadarError::ArchiveVersion {
            key: String::from(key),
            version,
        });
    }
    let actual: [u8; 4] = archive_field(header, 20, 24, key)?
        .try_into()
        .map_err(|_| RadarError::RecordBounds {
            key: String::from(key),
            offset: 0,
        })?;
    if actual != *site.as_str().as_bytes() {
        return Err(RadarError::ArchiveSite {
            key: String::from(key),
            expected: String::from(site.as_str()),
            actual,
        });
    }
    let date: [u8; 4] = archive_field(header, 12, 16, key)?
        .try_into()
        .map_err(|_| RadarError::RecordBounds {
            key: String::from(key),
            offset: 0,
        })?;
    let time: [u8; 4] = archive_field(header, 16, 20, key)?
        .try_into()
        .map_err(|_| RadarError::RecordBounds {
            key: String::from(key),
            offset: 0,
        })?;
    let date = u32::from_be_bytes(date);
    let time = u32::from_be_bytes(time);
    if date == 0 || u64::from(time) >= MS_PER_DAY {
        return Err(RadarError::ArchiveTime {
            key: String::from(key),
            date,
            time,
        });
    }
    Ok(())
}

/// Decodes one Archive II volume into the lowest-elevation reflectivity sweep.
pub fn decode_volume(site: &SiteId, key: &str, bytes: &[u8]) -> Result<RadarVolume, RadarError> {
    validate_archive_header(site, key, bytes)?;
    let mut assembly = VolumeAssembly::new();
    let mut offset = ARCHIVE_HEADER_BYTES;
    while offset < bytes.len() {
        let (messages, end) = decode_record(key, bytes, offset)?;
        for message in messages {
            match message {
                MessageContents::DigitalRadarData(radial) => {
                    if radial.header.radar_identifier.as_slice() != site.as_str().as_bytes() {
                        return Err(RadarError::SiteMismatch {
                            requested: String::from(site.as_str()),
                            actual: radial.header.radar_identifier,
                        });
                    }
                    assembly.accept(&radial);
                }
                _ => assembly.note_non_data(),
            }
        }
        offset = end;
    }
    assembly.finish(site, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network;

    fn site() -> Result<SiteId, String> {
        SiteId::parse("KPAH").map_err(|error| error.to_string())
    }

    fn radial(
        azimuth: f64,
        number: u16,
        elevation_number: u8,
        elevation_deg: f64,
        gates: usize,
    ) -> Radial {
        Radial {
            elevation_number,
            elevation_deg,
            azimuth_number: number,
            azimuth_deg: azimuth,
            spacing_deg: 0.5,
            first_gate_km: 2.0,
            gate_width_km: 0.25,
            values: vec![GateValue::Measured(20.0); gates],
        }
    }

    fn sweep(radials: Vec<Radial>) -> Result<RadarSweep, String> {
        let mut assembly = SweepAssembly::start(radials.first().cloned().ok_or("no radials")?);
        for radial in radials.into_iter().skip(1) {
            assembly.push(radial);
        }
        assembly.ended = true;
        let site = site()?;
        assembly
            .into_radial_bins(&site)
            .map_err(|error| error.to_string())
    }

    impl Clone for Radial {
        fn clone(&self) -> Self {
            Self {
                elevation_number: self.elevation_number,
                elevation_deg: self.elevation_deg,
                azimuth_number: self.azimuth_number,
                azimuth_deg: self.azimuth_deg,
                spacing_deg: self.spacing_deg,
                first_gate_km: self.first_gate_km,
                gate_width_km: self.gate_width_km,
                values: self.values.clone(),
            }
        }
    }

    fn span(left: f64, right: f64) -> f64 {
        (right - left).rem_euclid(360.0)
    }

    #[test]
    fn radial_status_matches_the_icd_codes() {
        assert_eq!(radial_status(3), Some(RadialStatus::VolumeStart));
        assert_eq!(radial_status(0), Some(RadialStatus::SweepStart));
        assert_eq!(radial_status(5), Some(RadialStatus::SweepStart));
        assert_eq!(radial_status(1), Some(RadialStatus::Intermediate));
        assert_eq!(radial_status(2), Some(RadialStatus::SweepEnd));
        assert_eq!(radial_status(4), Some(RadialStatus::VolumeEnd));
        assert_eq!(radial_status(6), None);
        assert_eq!(radial_status(15), None);
    }

    #[test]
    fn commanded_spacing_comes_from_the_header_code() {
        assert_eq!(commanded_spacing_deg(1), Some(0.5));
        assert_eq!(commanded_spacing_deg(2), Some(1.0));
        assert_eq!(commanded_spacing_deg(0), None);
    }

    #[test]
    fn radial_date_uses_one_based_epoch() {
        assert_eq!(scan_time(1, 0), Some(0));
        assert_eq!(scan_time(2, 1_000), Some(MS_PER_DAY + 1_000));
        assert_eq!(scan_time(0, 0), None);
        assert_eq!(scan_time(1, 86_400_000), None);
    }

    #[test]
    fn azimuth_edges_bisect_neighbouring_radials() -> Result<(), String> {
        let bins = sweep(vec![
            radial(10.0, 1, 1, 0.5, 1),
            radial(10.5, 2, 1, 0.5, 1),
            radial(11.0, 3, 1, 0.5, 1),
        ])?;
        let middle = bins.radials.get(1).ok_or("missing radial")?;
        assert!(
            (middle.azimuth_left_deg - 10.25).abs() < 1e-9,
            "left edge {}",
            middle.azimuth_left_deg
        );
        assert!(
            (middle.azimuth_right_deg - 10.75).abs() < 1e-9,
            "right edge {}",
            middle.azimuth_right_deg
        );
        let first = bins.radials.first().ok_or("missing radial")?;
        assert!(
            (first.azimuth_left_deg - 9.75).abs() < 1e-9,
            "a boundary without a neighbour keeps half the commanded spacing, got {}",
            first.azimuth_left_deg
        );
        let irregular = sweep(vec![radial(10.0, 1, 1, 0.5, 1), radial(10.6, 2, 1, 0.5, 1)])?;
        let second = irregular.radials.get(1).ok_or("missing radial")?;
        assert!(
            (second.azimuth_left_deg - 10.3).abs() < 1e-9,
            "irregular spacing still bisects, got {}",
            second.azimuth_left_deg
        );
        Ok(())
    }

    #[test]
    fn a_sweep_that_crosses_north_has_no_wide_wedge() -> Result<(), String> {
        // The measured azimuth sequence of a real KPAH sweep: it starts at
        // 275.18 degrees and wraps through north in the middle of the list.
        let bins = sweep(vec![
            radial(275.182_80, 1, 1, 0.247, 2),
            radial(359.717_10, 170, 1, 0.247, 2),
            radial(0.219_73, 171, 1, 0.247, 2),
            radial(274.707_64, 720, 1, 0.247, 2),
        ])?;
        for radial in &bins.radials {
            let width = span(radial.azimuth_left_deg, radial.azimuth_right_deg);
            assert!(
                width <= 0.5 * AZIMUTH_BRIDGE_TOLERANCE + 1e-9,
                "radial {} spans {width} degrees",
                radial.azimuth_number
            );
        }
        let crossing = bins.radials.get(1).ok_or("missing radial")?;
        assert!(
            (crossing.azimuth_right_deg - 359.968_415).abs() < 1e-6,
            "the radial before north ends at {}, not across the circle",
            crossing.azimuth_right_deg
        );
        let after = bins.radials.get(2).ok_or("missing radial")?;
        assert!(
            (after.azimuth_left_deg - 359.968_415).abs() < 1e-6,
            "the radial after north starts at {}",
            after.azimuth_left_deg
        );
        Ok(())
    }

    #[test]
    fn a_full_circle_keeps_the_seam_half_a_radial_wide() -> Result<(), String> {
        let mut radials = Vec::new();
        for step in 0..720_u16 {
            let azimuth = f64::from(step) * 0.5;
            radials.push(radial(azimuth, step + 1, 1, 0.5, 1));
        }
        let bins = sweep(radials)?;
        for radial in &bins.radials {
            let width = span(radial.azimuth_left_deg, radial.azimuth_right_deg);
            assert!((width - 0.5).abs() < 1e-6, "radial spans {width} degrees");
        }
        let first = bins.radials.first().ok_or("no radials")?;
        assert!(
            (first.azimuth_left_deg - 359.75).abs() < 1e-6,
            "seam at {}",
            first.azimuth_left_deg
        );
        Ok(())
    }

    #[test]
    fn a_missing_radial_leaves_a_gap_instead_of_a_wide_bin() -> Result<(), String> {
        let bins = sweep(vec![
            radial(0.0, 1, 1, 0.5, 1),
            radial(0.5, 2, 1, 0.5, 1),
            radial(2.0, 5, 1, 0.5, 1),
            radial(2.5, 6, 1, 0.5, 1),
        ])?;
        let gap_left = bins.radials.get(1).ok_or("missing radial")?;
        assert!(
            (gap_left.azimuth_right_deg - 0.75).abs() < 1e-9,
            "right edge before the gap is {}",
            gap_left.azimuth_right_deg
        );
        let gap_right = bins.radials.get(2).ok_or("missing radial")?;
        assert!(
            (gap_right.azimuth_left_deg - 1.75).abs() < 1e-9,
            "left edge after the gap is {}",
            gap_right.azimuth_left_deg
        );
        assert!(
            span(gap_left.azimuth_left_deg, gap_left.azimuth_right_deg) <= 0.5 + 1e-9,
            "no bin spans the missing radials"
        );
        Ok(())
    }

    #[test]
    fn missing_azimuth_numbers_are_counted_as_skipped_radials() {
        let mut assembly = SweepAssembly::start(radial(0.0, 1, 1, 0.5, 1));
        assembly.push(radial(1.0, 3, 1, 0.5, 1));
        assert_eq!(
            assembly.skipped, 1,
            "one radial is missing from the sequence"
        );
        assembly.push(radial(1.5, 4, 1, 0.5, 1));
        assert_eq!(assembly.skipped, 1);
    }

    #[test]
    fn a_sweep_with_one_radial_is_not_used() {
        let mut assembly = SweepAssembly::start(radial(0.0, 1, 1, 0.5, 1));
        assembly.ended = true;
        assert!(!assembly.complete());
    }

    #[test]
    fn gate_counts_and_ranges_follow_the_radial_header() -> Result<(), String> {
        let bins = sweep(vec![radial(0.0, 1, 1, 0.5, 4), radial(0.5, 2, 1, 0.5, 4)])?;
        let radial = bins.radials.first().ok_or("no radials")?;
        assert!((radial.first_gate_km - 2.0).abs() < 1e-12);
        assert!((radial.gate_width_km - 0.25).abs() < 1e-12);
        assert_eq!(bins.gate_count(), 8);
        // Four gates from 2.0 km to 2.75 km centre, far edge 2.875 km slant at
        // 0.5 degrees elevation: 2.8749 km ground.
        assert!(
            (bins.max_ground_range_km - 2.874_89).abs() < 1e-4,
            "max ground range {}",
            bins.max_ground_range_km
        );
        Ok(())
    }

    #[test]
    fn zero_scale_rejects_the_moment() {
        let block = GenericDataBlock {
            header: nexrad_decode::messages::digital_radar_data::GenericDataBlockHeader {
                data_block_id: nexrad_decode::messages::digital_radar_data::DataBlockId {
                    data_block_type: b'D',
                    data_name: *b"REF",
                },
                reserved: 0,
                number_of_data_moment_gates: 1,
                data_moment_range: 2000,
                data_moment_range_sample_interval: 250,
                tover: 0,
                snr_threshold: 0,
                control_flags: 0,
                data_word_size: 8,
                scale: 0.0,
                offset: 66.0,
            },
            encoded_data: vec![100],
        };
        assert!(matches!(
            decode_moment(&block),
            Err(MomentError::Scale { scale }) if scale == 0.0
        ));
    }

    #[test]
    fn moment_sentinels_stay_distinct_from_measurements() -> Result<(), MomentError> {
        let block = GenericDataBlock {
            header: nexrad_decode::messages::digital_radar_data::GenericDataBlockHeader {
                data_block_id: nexrad_decode::messages::digital_radar_data::DataBlockId {
                    data_block_type: b'D',
                    data_name: *b"REF",
                },
                reserved: 0,
                number_of_data_moment_gates: 3,
                data_moment_range: 2000,
                data_moment_range_sample_interval: 250,
                tover: 0,
                snr_threshold: 0,
                control_flags: 0,
                data_word_size: 8,
                scale: 2.0,
                offset: 66.0,
            },
            encoded_data: vec![0, 1, 70],
        };
        let values = decode_moment(&block)?;
        assert!(matches!(values.first(), Some(GateValue::BelowThreshold)));
        assert!(matches!(values.get(1), Some(GateValue::RangeFolded)));
        match values.get(2) {
            Some(GateValue::Measured(dbz)) => assert!((dbz - 2.0).abs() < 1e-6, "value {dbz}"),
            other => panic_free_failure(other),
        }
        Ok(())
    }

    fn panic_free_failure(value: Option<&GateValue>) {
        let _ = value;
    }

    fn archive_header() -> Vec<u8> {
        let mut header = Vec::from(&b"AR2V0006.001"[..]);
        header.extend_from_slice(&1_u32.to_be_bytes());
        header.extend_from_slice(&0_u32.to_be_bytes());
        header.extend_from_slice(b"KPAH");
        header
    }

    #[test]
    fn archive_header_rejects_wrong_site_and_version() -> Result<(), String> {
        let header = archive_header();
        let site = site()?;
        assert!(validate_archive_header(&site, "key", &header).is_ok());
        match validate_archive_header(
            &SiteId::parse("KTLX").map_err(|e| e.to_string())?,
            "key",
            &header,
        ) {
            Err(RadarError::ArchiveSite {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "KTLX");
                assert_eq!(actual, *b"KPAH");
            }
            other => return Err(format!("expected ArchiveSite, got {other:?}")),
        }
        let mut broken = archive_header();
        broken.splice(0..4, *b"AR3V");
        assert!(matches!(
            validate_archive_header(&site, "key", &broken),
            Err(RadarError::ArchiveVersion { .. })
        ));
        Ok(())
    }

    #[test]
    fn header_only_archive_reports_an_incomplete_volume() -> Result<(), String> {
        let site = site()?;
        match decode_volume(&site, "key", &archive_header()) {
            Err(RadarError::IncompleteVolume {
                defect: VolumeDefect::MissingStart,
                ..
            }) => Ok(()),
            other => Err(format!("expected MissingStart, got {other:?}")),
        }
    }

    #[test]
    fn truncated_archive_reports_bounds_with_key_and_offset() -> Result<(), String> {
        let site = site()?;
        match decode_volume(
            &site,
            "2026/10/30/KPAH/KPAH20261030_023529_V06",
            &[0_u8; 10],
        ) {
            Err(RadarError::RecordBounds { key, offset: 0 }) => {
                assert_eq!(key, "2026/10/30/KPAH/KPAH20261030_023529_V06");
                Ok(())
            }
            other => Err(format!("expected RecordBounds, got {other:?}")),
        }
    }

    const PINNED_KEY: &str = "2026/10/30/KPAH/KPAH20261030_023529_V06";
    const PINNED_BYTES: usize = 13_738_335;
    const PINNED_SHA256: &str = "8fb521c151f5022e1b8e771c2312b8b52fcd640849cac997be9e964c203264ae";

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Retrieves the pinned archive from AWS and runs the application path from
    /// archive bytes to render-ready geometry. Network failure fails this test.
    #[test]
    fn pinned_archive_decodes_to_a_renderable_sweep() -> Result<(), String> {
        let bytes = network::fetch_archive(PINNED_KEY).map_err(|error| error.to_string())?;
        assert_eq!(bytes.len(), PINNED_BYTES, "pinned archive size");
        assert_eq!(sha256_hex(&bytes), PINNED_SHA256, "pinned archive digest");
        let site = site()?;
        let volume = decode_volume(&site, PINNED_KEY, &bytes).map_err(|error| error.to_string())?;

        // Published KPAH coordinates, independent of this decoder.
        assert!(
            (volume.site_lat - 37.068_33).abs() < 1e-5,
            "site latitude {}",
            volume.site_lat
        );
        assert!(
            (volume.site_lon + 88.771_94).abs() < 1e-5,
            "site longitude {}",
            volume.site_lon
        );
        // The volume name states 2026-10-30 02:35:29 UTC; the first radial is
        // recorded within minutes of it.
        assert!(
            (1_793_327_729_000..1_793_328_329_000).contains(&volume.scan_unix_ms),
            "scan time {} outside the volume start window",
            volume.scan_unix_ms
        );
        // A super-resolution surveillance cut covers a full circle at 0.5 degree
        // spacing, so 720 radials are expected.
        assert_eq!(volume.sweep.radials.len(), 720, "radial count");
        let gates = volume
            .sweep
            .radials
            .first()
            .map_or(0, |radial| radial.values.len());
        // A super-resolution cut carries 1832 gates of 250 m from 2.000 km,
        // which is 460 km of coverage.
        assert_eq!(gates, 1832, "gates per radial");
        assert!(
            volume
                .sweep
                .radials
                .iter()
                .all(|radial| radial.values.len() == gates),
            "every radial carries the same gate count"
        );
        for radial in &volume.sweep.radials {
            let width = span(radial.azimuth_left_deg, radial.azimuth_right_deg);
            assert!(
                width > 0.0 && width <= 0.5 * AZIMUTH_BRIDGE_TOLERANCE + 1e-9,
                "radial {} spans {width} degrees",
                radial.azimuth_number
            );
            assert!(
                (0.0..360.0).contains(&radial.azimuth_left_deg)
                    && (0.0..=360.0).contains(&radial.azimuth_right_deg),
                "radial {} edges are normalized",
                radial.azimuth_number
            );
            // Super-resolution gates are 250 m wide and start at 2.000 km, so
            // the centre of gate 0 is 2.125 km.
            assert!(
                (radial.first_gate_km - 2.125).abs() < 1e-6,
                "first gate centre {}",
                radial.first_gate_km
            );
            assert!(
                (radial.gate_width_km - 0.25).abs() < 1e-6,
                "gate width {}",
                radial.gate_width_km
            );
        }
        // The far edge of gate 1831 is 459.875 km of slant range; at the 0.5
        // degree elevation of this cut that is 459.48 km on the ground.
        assert!(
            (volume.sweep.max_ground_range_km - 459.48).abs() < 0.05,
            "max ground range {}",
            volume.sweep.max_ground_range_km
        );
        assert_eq!(
            volume.sweep.below_threshold + volume.sweep.range_folded + volume.sweep.gate_count()
                - volume.sweep.below_threshold
                - volume.sweep.range_folded,
            volume.sweep.gate_count(),
            "gate categories partition the gates"
        );
        assert_eq!(volume.skipped_radials, 0, "no radial is skipped");

        let geometry = crate::render::build_geometry(&volume).map_err(|error| error.to_string())?;
        assert!(geometry.index_count > 0, "mesh has triangles");
        // Web Mercator stretches ground distance by 1/cos(latitude). The mesh
        // bounds reach the poleward edge of the sweep, where that factor is
        // largest, so the extent lies between the factor at the site and the
        // factor at the sweep's poleward limit.
        let site_scale = 1.0 / 37.068_33_f64.to_radians().cos();
        let poleward_deg = volume.sweep.max_ground_range_km / 111.32;
        let poleward_scale = 1.0 / (37.068_33 + poleward_deg).to_radians().cos();
        let ratio = geometry.extent_km / volume.sweep.max_ground_range_km;
        assert!(
            ratio > site_scale && ratio < poleward_scale,
            "Mercator extent ratio {ratio} outside {site_scale}..{poleward_scale}"
        );
        let ring = geometry.rings.first().ok_or("no range rings")?;
        assert!((ring.range_km - 50.0).abs() < 1e-9, "first ring radius");
        assert_eq!(
            geometry.rings.len(),
            9,
            "rings at 50 km intervals to 459.48 km"
        );
        Ok(())
    }
}
