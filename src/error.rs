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
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoordinateError {
    #[error("Latitude {lat} is invalid; it must be in the range -90 to 90.")]
    Latitude { lat: f64 },

    #[error("Longitude {lon} is invalid; it must be in the range -180 to 180.")]
    Longitude { lon: f64 },

    #[error("Range {range_km} is invalid; it must be non-negative.")]
    Range { range_km: f64 },

    #[error("Radar view range {range_km} is invalid; it must be finite and greater than zero.")]
    ViewRange { range_km: f64 },

    #[error("Azimuth {azimuth_deg} is invalid; it must be in the range 0 to 360.")]
    Azimuth { azimuth_deg: f64 },

    #[error("Elevation {elevation_deg} is invalid; it must be in the range -90 to 90 degrees.")]
    Elevation { elevation_deg: f64 },
}

#[derive(Debug, Error)]
pub enum ColorMapError {
    #[error("Value {value} is out of range for product {product}.")]
    OutOfRange { product: &'static str, value: f32 },

    #[error("Value {value} did not match any palette interval for product {product}.")]
    Unmapped { product: &'static str, value: f32 },
}

#[derive(Debug, Error)]
pub enum MomentError {
    #[error("Moment scale {scale} must be finite and positive.")]
    Scale { scale: f32 },
    #[error("Moment offset {offset} must be finite.")]
    Offset { offset: f32 },
    #[error("Moment word size {word_size} is unsupported.")]
    WordSize { word_size: u8 },
    #[error("Moment has {actual} bytes; expected {expected}.")]
    Length { expected: usize, actual: usize },
    #[error("Moment decodes to {value}, outside the representable reflectivity range.")]
    Value { value: f32 },
}

/// The specific structural violation found in a type 31 message.
#[derive(Debug, Error)]
pub enum MessageDefect {
    #[error("Message header is truncated: {available} bytes remain, 28 are required.")]
    TruncatedHeader { available: usize },
    #[error("Message declares {declared} bytes, less than its {minimum} byte header.")]
    DeclaredSize { declared: usize, minimum: usize },
    #[error("Message declares {declared} bytes but only {available} remain in its record.")]
    Bounds { declared: usize, available: usize },
    #[error("Type 31 message is segmented: segment {segment_number} of {segment_count}.")]
    Segmented {
        segment_count: u16,
        segment_number: u16,
    },
    #[error("Radial header is truncated: {available} bytes remain, 32 are required.")]
    TruncatedRadialHeader { available: usize },
    #[error("Radial is compressed, which type 31 decoding does not define.")]
    CompressedRadial,
    #[error("Radial declares {declared} bytes but {available} remain in its message.")]
    RadialLength { declared: u16, available: usize },
    #[error(
        "Radial declares {blocks} data blocks that do not fit within its {radial_length} bytes."
    )]
    PointerTable { blocks: u16, radial_length: usize },
    #[error(
        "Data block {index} points to {pointer}, inside the pointer table ending at {table_end}."
    )]
    PointerInsideTable {
        index: u16,
        pointer: usize,
        table_end: usize,
    },
    #[error("Data block {index} has offset {pointer}, which this machine cannot address.")]
    PointerUnrepresentable { index: u16, pointer: u32 },
    #[error("Data block {index} at {pointer} has no four-byte identifier inside its radial.")]
    BlockIdentifier { index: u16, pointer: usize },
    #[error("Data block {id:?} at {pointer} cannot hold its {length} bytes.")]
    BlockEnd {
        id: [u8; 4],
        pointer: usize,
        length: usize,
    },
    #[error("Data block {id:?} at {pointer} is not defined by the ICD.")]
    UnknownBlock { pointer: usize, id: [u8; 4] },
    #[error(
        "Constant block {id:?} declares {declared} bytes, below the {minimum} the decoder reads."
    )]
    BlockLength {
        id: [u8; 4],
        declared: u16,
        minimum: u16,
    },
    #[error(
        "Data block {id:?} at {pointer} ends at {end}, beyond its {radial_length} byte radial."
    )]
    BlockBounds {
        id: [u8; 4],
        pointer: usize,
        end: usize,
        radial_length: usize,
    },
    #[error("Data block {id:?} at {pointer} overlaps another data block.")]
    BlockOverlap { id: [u8; 4], pointer: usize },
    #[error("Moment block {id:?} declares a {word_size} bit word.")]
    WordSize { id: [u8; 4], word_size: u8 },
    #[error("Moment block {id:?} declares {gates} gates whose byte length overflows.")]
    GateLength { id: [u8; 4], gates: u16 },
}

/// Why a decoded archive does not form a complete volume.
#[derive(Debug, Error)]
pub enum VolumeDefect {
    #[error("no radial declares the start of the volume")]
    MissingStart,
    #[error("no radial declares the end of the volume")]
    MissingEnd,
    #[error("radials appear outside any elevation sweep")]
    SweepBoundaries,
}

#[derive(Debug, Error)]
pub enum RadarError {
    #[error("HTTP request to {url} failed: {source}")]
    Http {
        url: String,
        #[source]
        source: ureq::Error,
    },

    #[error("HTTP request to {url} returned status {status}; Retry-After: {retry_after:?}.")]
    HttpStatus {
        url: String,
        status: u16,
        retry_after: Option<String>,
    },

    #[error("Invalid Retry-After value {value:?}.")]
    InvalidRetryAfter { value: String },

    #[error("{request} Retry-After could not be applied: {source}")]
    RetryAfter {
        request: Box<RadarError>,
        #[source]
        source: Box<RadarError>,
    },

    #[error("S3 listing response for prefix {prefix} could not be parsed.")]
    Listing { prefix: String },

    #[error("No volume was listed for site {site} in the last three days.")]
    NoVolume { site: String },

    #[error(
        "Archive {key} declares version {version:?}, which is not AR2V with a numeric volume and version."
    )]
    ArchiveVersion { key: String, version: [u8; 12] },

    #[error("Archive {key} is for site {actual:?}; site {expected:?} was requested.")]
    ArchiveSite {
        key: String,
        expected: String,
        actual: [u8; 4],
    },

    #[error(
        "Archive {key} declares date {date} and time {time} ms, which are not a valid scan time."
    )]
    ArchiveTime { key: String, date: u32, time: u32 },

    #[error("Archive record at byte {offset} of {key} has no four-byte length prefix.")]
    RecordHeader { key: String, offset: usize },

    #[error("Archive record at byte {offset} of {key} extends past the end of the file.")]
    RecordBounds { key: String, offset: usize },

    #[error(
        "Archive record at byte {offset} of {key} exceeds the decompressed limit of {limit} bytes."
    )]
    RecordTooLarge {
        key: String,
        offset: usize,
        limit: u64,
    },

    #[error("Archive record at byte {offset} of {key} failed to decompress: {source}")]
    Decompress {
        key: String,
        offset: usize,
        #[source]
        source: std::io::Error,
    },

    #[error("Archive record at byte {offset} of {key} failed to decode: {source}")]
    Decode {
        key: String,
        offset: usize,
        #[source]
        source: nexrad_decode::result::Error,
    },

    #[error(
        "Message at byte {message_offset} of record {record_offset} in {key} is invalid: {defect}"
    )]
    MessageStructure {
        key: String,
        record_offset: usize,
        message_offset: usize,
        #[source]
        defect: MessageDefect,
    },

    #[error("No volume data block was found in any radial; site coordinates are unavailable.")]
    MissingSiteCoordinates,

    #[error("Requested site {requested}, but a radar radial identifies site {actual:?}.")]
    SiteMismatch { requested: String, actual: [u8; 4] },

    #[error("Invalid radar site coordinates: {source}")]
    SiteCoordinates {
        #[source]
        source: CoordinateError,
    },

    #[error("Radial {azimuth_number} of site {site} has an invalid gate range: {source}")]
    GateRange {
        site: String,
        azimuth_number: u16,
        #[source]
        source: CoordinateError,
    },

    #[error("No reflectivity data was found in the selected sweep for site {site}.")]
    NoReflectivityData { site: String },

    #[error(
        "The selected sweep for site {site} ended before the elevation-end radial; the volume is incomplete."
    )]
    IncompleteSweep { site: String },

    #[error("The archive for site {site} is incomplete: {defect}.")]
    IncompleteVolume { site: String, defect: VolumeDefect },

    #[error("The decoder worker for site {site} stopped before producing a volume.")]
    DecoderStopped { site: String },

    #[error("The decoder worker for site {site} panicked; no volume was produced.")]
    DecoderPanic { site: String },

    #[error("The fetch worker for site {site} stopped; radar updates are unavailable.")]
    WorkerStopped { site: String },

    #[error("The fetch worker for site {site} panicked; no volume was produced.")]
    WorkerPanic { site: String },

    #[error("The clock is before the Unix epoch: {source}")]
    Clock {
        #[source]
        source: std::time::SystemTimeError,
    },

    #[error("The current date cannot be represented: {source}")]
    DateRange {
        #[source]
        source: std::num::TryFromIntError,
    },
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("The render worker stopped before producing a frame.")]
    WorkerStopped,

    #[error("The GPU radar draw failed.")]
    GpuDraw,

    #[error("The WGPU renderer is unavailable; radar cannot be displayed.")]
    GpuUnavailable,

    #[error("The render worker panicked while preparing the radar frame.")]
    Panic,

    #[error("The radar geometry has no geodesic paths for a gate.")]
    MissingGeodesicPaths,

    #[error("Radar geographic placement failed: {source}")]
    Coordinate {
        #[source]
        source: CoordinateError,
    },

    #[error("Failed to map radar value to a color: {source}")]
    Color {
        #[source]
        source: ColorMapError,
    },

    #[error("Radar mesh has too many vertices: {vertices}.")]
    MeshSize { vertices: usize },
}
