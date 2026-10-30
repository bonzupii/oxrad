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
use std::io::{Cursor, Read};

use nexrad_decode::messages::MessageContents;
use nexrad_decode::messages::digital_radar_data::{GenericDataBlock, Message as DigitalRadarData};

use crate::colormap::{MAX_REFLECTIVITY_DBZ, MIN_REFLECTIVITY_DBZ};
use crate::error::{CoordinateError, MessageDefect, MomentError, RadarError};
use crate::projection;
use crate::time::MS_PER_DAY;

const ARCHIVE_HEADER_BYTES: usize = 24;
const RECORD_LENGTH_BYTES: usize = 4;
const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;
const CTM_BYTES: usize = 12;
const MESSAGE_HEADER_BYTES: usize = 28;
const LEGACY_MESSAGE_BYTES: usize = 2432;
const RADIAL_HEADER_BYTES: usize = 32;

#[derive(Debug, Clone, Copy)]
pub enum GateValue {
    Measured(f32),
    BelowThreshold,
    RangeFolded,
}

#[derive(Debug, Clone, Copy)]
pub struct GateCorner {
    pub range_km: f64,
    pub azimuth_deg: f64,
}

#[derive(Debug)]
pub struct RadarBin {
    pub corners: [GateCorner; 4],
    pub value: GateValue,
}

#[derive(Debug)]
pub struct RadarVolume {
    pub site_lat: f64,
    pub site_lon: f64,
    pub bins: Vec<RadarBin>,
    pub max_range_km: f64,
    pub scan_unix_ms: u64,
    pub below_threshold_bins: usize,
    pub range_folded_bins: usize,
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
        0 => Some(RadialStatus::SweepStart),
        1 => Some(RadialStatus::Intermediate),
        2 => Some(RadialStatus::SweepEnd),
        3 => Some(RadialStatus::VolumeStart),
        5 => Some(RadialStatus::SweepStart),
        4 => Some(RadialStatus::VolumeEnd),
        _ => None,
    }
}

#[derive(Debug)]
enum RadialDefect {
    Status,
    Elevation,
    Azimuth,
    Spacing,
    ScanTime,
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
    first_gate_km: f64,
    gate_width_km: f64,
    scan_unix_ms: u64,
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
    if !matches!(radial.header.azimuth_resolution_spacing, 1 | 2) {
        return Err(RadialDefect::Spacing);
    }
    let Some(scan_unix_ms) = scan_time(radial.header.date, radial.header.time) else {
        return Err(RadialDefect::ScanTime);
    };
    let Some(block) = radial.reflectivity_data_block.as_ref() else {
        return Err(RadialDefect::MissingReflectivity);
    };
    let gates = block.header.number_of_data_moment_gates;
    let first_gate_km = f64::from(block.header.data_moment_range) / 1000.0;
    let gate_width_km = f64::from(block.header.data_moment_range_sample_interval) / 1000.0;
    if gates == 0 || gate_width_km <= 0.0 || first_gate_km < gate_width_km / 2.0 {
        return Err(RadialDefect::GateGeometry);
    }
    let values = decode_moment(block).map_err(RadialDefect::Moment)?;
    Ok(Radial {
        elevation_number: radial.header.elevation_number,
        elevation_deg,
        azimuth_number: radial.header.azimuth_number,
        azimuth_deg,
        first_gate_km,
        gate_width_km,
        scan_unix_ms,
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
            [value] => u16::from(*value),
            [high, low] => u16::from_be_bytes([*high, *low]),
            _ => {
                return Err(MomentError::Length {
                    expected,
                    actual: block.encoded_data.len(),
                });
            }
        };
        let value = match raw {
            0 => GateValue::BelowThreshold,
            1 => GateValue::RangeFolded,
            _ => {
                let dbz = (f32::from(raw) - offset) / scale;
                if !dbz.is_finite() || !(MIN_REFLECTIVITY_DBZ..=MAX_REFLECTIVITY_DBZ).contains(&dbz)
                {
                    return Err(MomentError::Value { value: dbz });
                }
                GateValue::Measured(dbz)
            }
        };
        values.push(value);
    }
    Ok(values)
}

fn scan_time(date: u16, milliseconds: u32) -> Option<u64> {
    if date == 0 || u64::from(milliseconds) >= MS_PER_DAY {
        return None;
    }
    Some(u64::from(date - 1) * MS_PER_DAY + u64::from(milliseconds))
}

struct SweepAssembly {
    elevation_number: u8,
    elevation_deg: f64,
    scan_unix_ms: u64,
    radials: Vec<Radial>,
    defects: usize,
    ended: bool,
}

impl SweepAssembly {
    fn start(radial: Radial) -> Self {
        Self {
            elevation_number: radial.elevation_number,
            elevation_deg: radial.elevation_deg,
            scan_unix_ms: radial.scan_unix_ms,
            radials: vec![radial],
            defects: 0,
            ended: false,
        }
    }

    fn push(&mut self, radial: Radial) {
        let ordered = match self.radials.last() {
            Some(last) => last.azimuth_number.checked_add(1) == Some(radial.azimuth_number),
            None => true,
        };
        if radial.elevation_number != self.elevation_number || !ordered {
            self.defects += 1;
        }
        self.radials.push(radial);
    }

    const fn complete(&self) -> bool {
        self.ended && self.defects == 0 && self.radials.len() >= 2
    }

    fn into_bins(self, site: &str) -> Result<SweepBins, RadarError> {
        let count = self.radials.len();
        let mut bins = Vec::new();
        let mut max_range_km: f64 = 0.0;
        let mut below_threshold_bins = 0_usize;
        let mut range_folded_bins = 0_usize;
        for (index, radial) in self.radials.iter().enumerate() {
            let previous_azimuth = match index.checked_sub(1).and_then(|i| self.radials.get(i)) {
                Some(other) => other.azimuth_deg,
                None => self
                    .radials
                    .get(count.saturating_sub(1))
                    .map_or(radial.azimuth_deg, |other| other.azimuth_deg - 360.0),
            };
            let next_azimuth = match self.radials.get(index.saturating_add(1)) {
                Some(other) => other.azimuth_deg,
                None => self
                    .radials
                    .first()
                    .map_or(radial.azimuth_deg, |other| other.azimuth_deg + 360.0),
            };
            let left = ((radial.azimuth_deg + previous_azimuth) / 2.0).rem_euclid(360.0);
            let right = ((radial.azimuth_deg + next_azimuth) / 2.0).rem_euclid(360.0);
            for (gate_index, value) in radial.values.iter().enumerate() {
                let center_km = radial.first_gate_km + (gate_index as f64) * radial.gate_width_km;
                let near_km = projection::ground_range(
                    center_km - radial.gate_width_km / 2.0,
                    radial.elevation_deg,
                )
                .map_err(|source| RadarError::GateRange {
                    site: String::from(site),
                    azimuth_number: radial.azimuth_number,
                    source,
                })?;
                let far_km = projection::ground_range(
                    center_km + radial.gate_width_km / 2.0,
                    radial.elevation_deg,
                )
                .map_err(|source| RadarError::GateRange {
                    site: String::from(site),
                    azimuth_number: radial.azimuth_number,
                    source,
                })?;
                let gate_value = match value {
                    GateValue::Measured(dbz) => GateValue::Measured(*dbz),
                    GateValue::BelowThreshold => {
                        below_threshold_bins += 1;
                        GateValue::BelowThreshold
                    }
                    GateValue::RangeFolded => {
                        range_folded_bins += 1;
                        GateValue::RangeFolded
                    }
                };
                max_range_km = max_range_km.max(far_km);
                bins.push(RadarBin {
                    corners: [
                        GateCorner {
                            range_km: near_km,
                            azimuth_deg: left,
                        },
                        GateCorner {
                            range_km: near_km,
                            azimuth_deg: right,
                        },
                        GateCorner {
                            range_km: far_km,
                            azimuth_deg: right,
                        },
                        GateCorner {
                            range_km: far_km,
                            azimuth_deg: left,
                        },
                    ],
                    value: gate_value,
                });
            }
        }
        Ok(SweepBins {
            bins,
            max_range_km,
            scan_unix_ms: self.scan_unix_ms,
            below_threshold_bins,
            range_folded_bins,
        })
    }
}

struct SweepBins {
    bins: Vec<RadarBin>,
    max_range_km: f64,
    scan_unix_ms: u64,
    below_threshold_bins: usize,
    range_folded_bins: usize,
}

/// The sweep currently being assembled. A start radial that fails validation
/// leaves the elevation open but unusable, so its radials are counted and dropped
/// without discarding the rest of the volume.
enum CurrentSweep {
    Sweep(SweepAssembly),
    Defective,
}

struct VolumeAssembly {
    site: Option<SiteFix>,
    site_error: Option<CoordinateError>,
    current: Option<CurrentSweep>,
    sweeps: Vec<SweepAssembly>,
    started: bool,
    ended: bool,
    structure_broken: bool,
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
        let status = radial.header.radial_status;
        match radial_status(status) {
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
            Ok(parsed) => self.current = Some(CurrentSweep::Sweep(SweepAssembly::start(parsed))),
            Err(defect) => {
                self.record(defect);
                self.current = Some(CurrentSweep::Defective);
            }
        }
    }

    fn extend(&mut self, radial: &DigitalRadarData) {
        match parse_radial(radial) {
            Ok(parsed) => match self.current.as_mut() {
                Some(CurrentSweep::Sweep(sweep)) => sweep.push(parsed),
                Some(CurrentSweep::Defective) => self.skipped_radials += 1,
                None => self.structure_broken = true,
            },
            Err(defect) => {
                self.record(defect);
                if let Some(CurrentSweep::Sweep(sweep)) = self.current.as_mut() {
                    sweep.defects += 1;
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
        let Some(CurrentSweep::Sweep(mut sweep)) = self.current.take() else {
            return;
        };
        sweep.ended = ended;
        if sweep.complete() {
            self.sweeps.push(sweep);
        }
    }

    fn finish(self, site: &str) -> Result<RadarVolume, RadarError> {
        if !self.started {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site),
                defect: crate::error::VolumeDefect::MissingStart,
            });
        }
        if !self.ended {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site),
                defect: crate::error::VolumeDefect::MissingEnd,
            });
        }
        if self.structure_broken {
            return Err(RadarError::IncompleteVolume {
                site: String::from(site),
                defect: crate::error::VolumeDefect::SweepBoundaries,
            });
        }
        let sweep = self
            .sweeps
            .into_iter()
            .min_by(|left, right| left.elevation_deg.total_cmp(&right.elevation_deg))
            .ok_or_else(|| RadarError::IncompleteSweep {
                site: String::from(site),
            })?;
        let site_fix = self.site.ok_or_else(|| {
            self.site_error.map_or_else(
                || RadarError::MissingSiteCoordinates,
                |source| RadarError::SiteCoordinates { source },
            )
        })?;
        let sweep_bins = sweep.into_bins(site)?;
        if sweep_bins.bins.is_empty() {
            return Err(RadarError::NoReflectivityData {
                site: String::from(site),
            });
        }
        Ok(RadarVolume {
            site_lat: site_fix.lat,
            site_lon: site_fix.lon,
            bins: sweep_bins.bins,
            max_range_km: sweep_bins.max_range_km,
            scan_unix_ms: sweep_bins.scan_unix_ms,
            below_threshold_bins: sweep_bins.below_threshold_bins,
            range_folded_bins: sweep_bins.range_folded_bins,
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
    let end = start
        .checked_add(i32::from_be_bytes(length).unsigned_abs() as usize)
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

fn validate_archive_header(site: &str, key: &str, bytes: &[u8]) -> Result<(), RadarError> {
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
    if actual != *site.as_bytes() {
        return Err(RadarError::ArchiveSite {
            key: String::from(key),
            expected: String::from(site),
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

pub fn decode_volume(site: &str, key: &str, bytes: &[u8]) -> Result<RadarVolume, RadarError> {
    validate_archive_header(site, key, bytes)?;
    let mut assembly = VolumeAssembly::new();
    let mut offset = ARCHIVE_HEADER_BYTES;
    while offset < bytes.len() {
        let (messages, end) = decode_record(key, bytes, offset)?;
        for message in messages {
            match message {
                MessageContents::DigitalRadarData(radial) => {
                    if radial.header.radar_identifier.as_slice() != site.as_bytes() {
                        return Err(RadarError::SiteMismatch {
                            requested: String::from(site),
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
    assembly.finish(site)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RenderError;

    const KEY: &str = "2026/10/30/KPAH/KPAH20261030_023529_V06";

    #[derive(Debug)]
    enum TestFailure {
        Radar(RadarError),
        Render(RenderError),
        Io(std::io::Error),
        Value {
            label: &'static str,
            expected: f64,
            actual: f64,
            tolerance: f64,
        },
        Count {
            label: &'static str,
            expected: usize,
            actual: usize,
        },
        Missing {
            label: &'static str,
            expected: &'static str,
        },
    }

    impl From<RadarError> for TestFailure {
        fn from(source: RadarError) -> Self {
            Self::Radar(source)
        }
    }

    impl From<RenderError> for TestFailure {
        fn from(source: RenderError) -> Self {
            Self::Render(source)
        }
    }

    impl std::fmt::Display for TestFailure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Radar(error) => write!(formatter, "{error}"),
                Self::Render(error) => write!(formatter, "{error}"),
                Self::Io(error) => write!(formatter, "{error}"),
                Self::Value {
                    label,
                    expected,
                    actual,
                    tolerance,
                } => write!(
                    formatter,
                    "{label}: expected {expected}, got {actual}, tolerance {tolerance}"
                ),
                Self::Count {
                    label,
                    expected,
                    actual,
                } => write!(formatter, "{label}: expected {expected}, got {actual}"),
                Self::Missing { label, expected } => {
                    write!(formatter, "{label}: expected {expected}")
                }
            }
        }
    }

    fn expect_within(
        label: &'static str,
        expected: f64,
        actual: f64,
        tolerance: f64,
    ) -> Result<(), TestFailure> {
        if (actual - expected).abs() <= tolerance {
            Ok(())
        } else {
            Err(TestFailure::Value {
                label,
                expected,
                actual,
                tolerance,
            })
        }
    }

    fn archive_header() -> Vec<u8> {
        let mut header = Vec::from(&b"AR2V0006.001"[..]);
        header.extend_from_slice(&1_u32.to_be_bytes());
        header.extend_from_slice(&0_u32.to_be_bytes());
        header.extend_from_slice(b"KPAH");
        header
    }

    fn radial(azimuth: f64, number: u16, elevation_number: u8, elevation_deg: f64) -> Radial {
        Radial {
            elevation_number,
            elevation_deg,
            azimuth_number: number,
            azimuth_deg: azimuth,
            first_gate_km: 1.0,
            gate_width_km: 0.25,
            scan_unix_ms: 0,
            values: vec![GateValue::Measured(20.0)],
        }
    }

    #[test]
    fn radial_status_matches_the_icd_codes() {
        assert_eq!(radial_status(0), Some(RadialStatus::SweepStart));
        assert_eq!(radial_status(1), Some(RadialStatus::Intermediate));
        assert_eq!(radial_status(2), Some(RadialStatus::SweepEnd));
        assert_eq!(radial_status(3), Some(RadialStatus::VolumeStart));
        assert_eq!(radial_status(4), Some(RadialStatus::VolumeEnd));
        assert_eq!(radial_status(5), Some(RadialStatus::SweepStart));
        assert_eq!(radial_status(6), None);
    }

    #[test]
    fn radial_date_uses_one_based_epoch() {
        assert_eq!(scan_time(1, 0), Some(0));
        assert_eq!(scan_time(2, 1_000), Some(MS_PER_DAY + 1_000));
        assert_eq!(scan_time(0, 0), None);
        assert_eq!(scan_time(1, 86_400_000), None);
    }

    #[test]
    fn azimuth_edges_bisect_neighbouring_radials() -> Result<(), TestFailure> {
        let mut sweep = SweepAssembly::start(radial(0.0, 1, 1, 0.5));
        for (azimuth, number) in [(90.0, 2), (180.0, 3), (270.0, 4)] {
            sweep.push(radial(azimuth, number, 1, 0.5));
        }
        sweep.ended = true;
        let bins = sweep.into_bins("KPAH")?;
        let first = bins
            .bins
            .first()
            .ok_or_else(|| RadarError::NoReflectivityData {
                site: String::from("KPAH"),
            })?;
        expect_within(
            "first radial left edge",
            315.0,
            first.corners[0].azimuth_deg,
            1e-9,
        )?;
        expect_within(
            "first radial right edge",
            45.0,
            first.corners[1].azimuth_deg,
            1e-9,
        )?;
        let last = bins
            .bins
            .last()
            .ok_or_else(|| RadarError::NoReflectivityData {
                site: String::from("KPAH"),
            })?;
        expect_within(
            "last radial left edge",
            225.0,
            last.corners[0].azimuth_deg,
            1e-9,
        )?;
        expect_within(
            "last radial right edge",
            315.0,
            last.corners[1].azimuth_deg,
            1e-9,
        )?;
        Ok(())
    }

    #[test]
    fn sweep_with_a_single_radial_has_no_derivable_edges() {
        let mut sweep = SweepAssembly::start(radial(0.0, 1, 1, 0.5));
        sweep.ended = true;
        assert!(!sweep.complete());
    }

    #[test]
    fn missing_azimuth_number_breaks_the_sweep() {
        let mut sweep = SweepAssembly::start(radial(0.0, 1, 1, 0.5));
        sweep.push(radial(1.0, 3, 1, 0.5));
        sweep.ended = true;
        assert!(!sweep.complete());
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
                data_moment_range: 1000,
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
    fn archive_header_rejects_wrong_site_and_version() {
        let header = archive_header();
        assert!(validate_archive_header("KPAH", KEY, &header).is_ok());
        assert!(matches!(
            validate_archive_header("KTLX", KEY, &header),
            Err(RadarError::ArchiveSite { ref expected, actual, .. })
                if expected == "KTLX" && actual == *b"KPAH"
        ));
        let mut broken = archive_header();
        broken.splice(0..4, *b"AR3V");
        assert!(matches!(
            validate_archive_header("KPAH", KEY, &broken),
            Err(RadarError::ArchiveVersion { .. })
        ));
    }

    #[test]
    fn fixture_volume_decodes_and_builds_a_frame() -> Result<(), TestFailure> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KPAH20261030_023529_V06"
        );
        let bytes = std::fs::read(path).map_err(TestFailure::Io)?;
        let volume = decode_volume("KPAH", "KPAH20261030_023529_V06", &bytes)?;
        if volume.bins.is_empty() {
            return Err(TestFailure::Count {
                label: "fixture gates",
                expected: 1,
                actual: 0,
            });
        }
        // The file name carries the volume start time of 2026-10-30 02:35:29 UTC.
        // The first recorded radial follows it, so the decoded scan time must fall
        // inside the first ten minutes after that timestamp on the same date.
        let scan = volume.scan_unix_ms;
        if !(1_793_327_729_000..1_793_328_329_000).contains(&scan) {
            return Err(TestFailure::Value {
                label: "fixture scan time against the 02:35:29 UTC file timestamp",
                expected: 1_793_327_729_000.0,
                actual: scan as f64,
                tolerance: 600_000.0,
            });
        }
        if !crate::time::format_utc(scan)?.starts_with("2026-10-30 ") {
            return Err(TestFailure::Missing {
                label: "fixture scan date",
                expected: "2026-10-30 in UTC",
            });
        }
        expect_within("fixture site latitude", 37.068_33, volume.site_lat, 1e-4)?;
        expect_within("fixture site longitude", -88.771_94, volume.site_lon, 1e-4)?;
        expect_within(
            "fixture skipped radials",
            0.0,
            volume.skipped_radials as f64,
            0.0,
        )?;
        let frame = crate::render::build_render(&volume, crate::render::canonical_rect())?;
        if frame.index_count == 0 {
            return Err(TestFailure::Count {
                label: "fixture mesh indices",
                expected: 1,
                actual: 0,
            });
        }
        match frame.rings.first() {
            Some((range_km, points)) if (range_km - 50.0).abs() < 1e-9 && !points.is_empty() => {}
            _ => {
                return Err(TestFailure::Missing {
                    label: "first range ring",
                    expected: "a 50 km ring with geodesic points",
                });
            }
        }
        Ok(())
    }

    #[test]
    fn header_only_archive_reports_incomplete_volume() {
        assert!(matches!(
            decode_volume("KPAH", KEY, &archive_header()),
            Err(RadarError::IncompleteVolume {
                ref site,
                defect: crate::error::VolumeDefect::MissingStart,
            }) if site == "KPAH"
        ));
    }

    #[test]
    fn truncated_archive_reports_bounds_with_key_and_offset() {
        assert!(matches!(
            decode_volume("KPAH", KEY, &[0_u8; 10]),
            Err(RadarError::RecordBounds { ref key, offset: 0 }) if key == KEY
        ));
    }
}
