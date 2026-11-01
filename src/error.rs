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
//! Every failure the application can report, with its carried values.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io;
use std::path::PathBuf;

use crate::place::PlaceError;

/// A value that cannot be placed on the ellipsoid or in the view.
#[derive(Debug)]
pub enum CoordinateError {
    Latitude { lat: f64 },
    Longitude { lon: f64 },
    MercatorLatitude { lat: f64 },
    Range { range_km: f64 },
    GroundRange { slant_km: f64, elevation_deg: f64 },
    ViewExtent { extent_km: f64 },
    Viewport { width: f32, height: f32 },
    Azimuth { azimuth_deg: f64 },
    Elevation { elevation_deg: f64 },
}

impl CoordinateError {
    fn message(&self) -> String {
        match self {
            Self::Latitude { lat } => format!("Latitude {lat} is outside -90 to 90."),
            Self::Longitude { lon } => format!("Longitude {lon} is outside -180 to 180."),
            Self::MercatorLatitude { lat } => {
                format!("Latitude {lat} is outside the range Web Mercator can project.")
            }
            Self::Range { range_km } => format!("Range {range_km} km is not a valid distance."),
            Self::GroundRange {
                slant_km,
                elevation_deg,
            } => format!(
                "Slant range {slant_km} km at {elevation_deg} degrees of elevation has no ground range."
            ),
            Self::ViewExtent { extent_km } => {
                format!("Radar extent {extent_km} km is not positive and finite.")
            }
            Self::Viewport { width, height } => {
                format!("The viewport is {width} by {height} pixels, too small to draw radar.")
            }
            Self::Azimuth { azimuth_deg } => {
                format!("Azimuth {azimuth_deg} is outside 0 to 360.")
            }
            Self::Elevation { elevation_deg } => {
                format!("Elevation {elevation_deg} is outside -90 to 90 degrees.")
            }
        }
    }
}

impl Display for CoordinateError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for CoordinateError {}

/// A value that is not a four-character radar site identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteIdError {
    Length { value: String, length: usize },
    Character { value: String, byte: u8 },
}

impl SiteIdError {
    fn message(&self) -> String {
        match self {
            Self::Length { value, length } => {
                format!("Site identifier {value:?} has {length} characters; four are required.")
            }
            Self::Character { value, byte } => format!(
                "Site identifier {value:?} contains {byte}; only A to Z and 0 to 9 are allowed."
            ),
        }
    }
}

impl Display for SiteIdError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for SiteIdError {}

/// A failure reading or writing the application directory.
#[derive(Debug)]
pub enum PathError {
    HomeUnavailable,
}

impl Display for PathError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeUnavailable => write!(
                formatter,
                "No home directory is set, so the oxrad configuration cannot be stored."
            ),
        }
    }
}

impl Error for PathError {}

/// A failure reading or writing the user configuration file.
#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Write {
        path: PathBuf,
        source: io::Error,
    },
    Syntax {
        path: PathBuf,
        line: usize,
    },
    InvalidUnit {
        path: PathBuf,
        line: usize,
        value: String,
    },
    InvalidTheme {
        path: PathBuf,
        line: usize,
        value: String,
    },
    UnknownKey {
        path: PathBuf,
        line: usize,
        key: String,
    },
    Site {
        path: PathBuf,
        line: usize,
        source: SiteIdError,
    },
    IncompletePlaceEntry {
        path: PathBuf,
        line: usize,
    },
    PlaceNumber {
        path: PathBuf,
        line: usize,
        field: &'static str,
        value: String,
    },
    PlaceEntry {
        path: PathBuf,
        line: usize,
        source: PlaceError,
    },
    Path {
        source: PathError,
    },
}

impl ConfigError {
    fn message(&self) -> String {
        match self {
            Self::Read { path, source } => {
                format!("Failed to read {}: {source}", path.display())
            }
            Self::Write { path, source } => {
                format!("Failed to write {}: {source}", path.display())
            }
            Self::Syntax { path, line } => {
                format!("{} line {line} is not a key and a value.", path.display())
            }
            Self::InvalidUnit { path, line, value } => format!(
                "{} line {line} sets units to {value:?}, which is not metric or imperial.",
                path.display()
            ),
            Self::InvalidTheme { path, line, value } => format!(
                "{} line {line} sets the theme to {value:?}, which is not dark or light.",
                path.display()
            ),
            Self::UnknownKey { path, line, key } => format!(
                "{} line {line} sets the unrecognized key {key}.",
                path.display()
            ),
            Self::Site { path, line, source } => {
                format!(
                    "{} line {line} has an invalid site: {source}",
                    path.display()
                )
            }
            Self::IncompletePlaceEntry { path, line } => format!(
                "{} line {line} starts a saved place, but one or more place fields are missing.",
                path.display()
            ),
            Self::PlaceNumber {
                path,
                line,
                field,
                value,
            } => format!(
                "{} line {line} has an invalid saved-place {field}: {value:?}.",
                path.display()
            ),
            Self::PlaceEntry { path, line, source } => format!(
                "{} line {line} has an invalid saved place: {source}",
                path.display()
            ),
            Self::Path { source } => source.to_string(),
        }
    }
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Site { source, .. } => Some(source),
            Self::PlaceEntry { source, .. } => Some(source),
            Self::Path { source } => Some(source),
            Self::Syntax { .. }
            | Self::InvalidUnit { .. }
            | Self::InvalidTheme { .. }
            | Self::UnknownKey { .. }
            | Self::IncompletePlaceEntry { .. }
            | Self::PlaceNumber { .. } => None,
        }
    }
}

/// A reflectivity moment that cannot be decoded into gate values.
#[derive(Debug)]
pub enum MomentError {
    Scale { scale: f32 },
    Offset { offset: f32 },
    WordSize { word_size: u8 },
    Length { expected: usize, actual: usize },
    Value { value: f32 },
}

impl MomentError {
    fn message(&self) -> String {
        match self {
            Self::Scale { scale } => format!("Moment scale {scale} is not positive and finite."),
            Self::Offset { offset } => format!("Moment offset {offset} is not finite."),
            Self::WordSize { word_size } => format!("Moment word size {word_size} is unsupported."),
            Self::Length { expected, actual } => {
                format!("Moment has {actual} bytes; expected {expected}.")
            }
            Self::Value { value } => {
                format!("Moment decodes to {value}, outside the representable reflectivity range.")
            }
        }
    }
}

impl Display for MomentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for MomentError {}

/// The specific structural violation found in a type 31 message.
#[derive(Debug)]
pub enum MessageDefect {
    TruncatedHeader {
        available: usize,
    },
    DeclaredSize {
        declared: usize,
        minimum: usize,
    },
    Bounds {
        declared: usize,
        available: usize,
    },
    Segmented {
        segment_count: u16,
        segment_number: u16,
    },
    TruncatedRadialHeader {
        available: usize,
    },
    CompressedRadial,
    RadialLength {
        declared: u16,
        available: usize,
    },
    PointerTable {
        blocks: u16,
        radial_length: usize,
    },
    PointerInsideTable {
        index: u16,
        pointer: usize,
        table_end: usize,
    },
    PointerUnrepresentable {
        index: u16,
        pointer: u32,
    },
    BlockIdentifier {
        index: u16,
        pointer: usize,
    },
    BlockEnd {
        id: [u8; 4],
        pointer: usize,
        length: usize,
    },
    UnknownBlock {
        pointer: usize,
        id: [u8; 4],
    },
    BlockLength {
        id: [u8; 4],
        declared: u16,
        minimum: u16,
    },
    BlockBounds {
        id: [u8; 4],
        pointer: usize,
        end: usize,
        radial_length: usize,
    },
    BlockOverlap {
        id: [u8; 4],
        pointer: usize,
    },
    WordSize {
        id: [u8; 4],
        word_size: u8,
    },
    GateLength {
        id: [u8; 4],
        gates: u16,
    },
}

impl MessageDefect {
    fn message(&self) -> String {
        match self {
            Self::TruncatedHeader { available } => {
                format!("The message header is truncated at {available} bytes of 28.")
            }
            Self::DeclaredSize { declared, minimum } => {
                format!("The message declares {declared} bytes, below {minimum}.")
            }
            Self::Bounds {
                declared,
                available,
            } => {
                format!("The message declares {declared} bytes with {available} left.")
            }
            Self::Segmented {
                segment_count,
                segment_number,
            } => {
                format!("The type 31 message is segment {segment_number} of {segment_count}.")
            }
            Self::TruncatedRadialHeader { available } => {
                format!("The radial header is truncated at {available} bytes of 32.")
            }
            Self::CompressedRadial => {
                String::from("The radial is compressed, which type 31 does not define.")
            }
            Self::RadialLength {
                declared,
                available,
            } => {
                format!("The radial declares {declared} bytes with {available} left.")
            }
            Self::PointerTable {
                blocks,
                radial_length,
            } => format!(
                "The radial declares {blocks} data blocks that do not fit its {radial_length} bytes."
            ),
            Self::PointerInsideTable {
                index,
                pointer,
                table_end,
            } => format!(
                "Data block {index} points to {pointer}, inside the table ending at {table_end}."
            ),
            Self::PointerUnrepresentable { index, pointer } => format!(
                "Data block {index} has offset {pointer}, which this machine cannot address."
            ),
            Self::BlockIdentifier { index, pointer } => {
                format!("Data block {index} at {pointer} has no four-byte identifier.")
            }
            Self::BlockEnd {
                id,
                pointer,
                length,
            } => {
                format!("Data block {id:?} at {pointer} cannot hold {length} bytes.")
            }
            Self::UnknownBlock { pointer, id } => {
                format!("Data block {id:?} at {pointer} is not defined by the ICD.")
            }
            Self::BlockLength {
                id,
                declared,
                minimum,
            } => format!(
                "Block {id:?} declares {declared} bytes, below the {minimum} the decoder reads."
            ),
            Self::BlockBounds {
                id,
                pointer,
                end,
                radial_length,
            } => format!(
                "Block {id:?} at {pointer} ends at {end}, past its {radial_length} byte radial."
            ),
            Self::BlockOverlap { id, pointer } => {
                format!("Data block {id:?} at {pointer} overlaps another block.")
            }
            Self::WordSize { id, word_size } => {
                format!("Moment block {id:?} declares a {word_size} bit word.")
            }
            Self::GateLength { id, gates } => {
                format!("Moment block {id:?} declares {gates} gates with an overflow.")
            }
        }
    }
}

impl Display for MessageDefect {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for MessageDefect {}

/// Why a decoded archive does not form a complete volume.
#[derive(Debug)]
pub enum VolumeDefect {
    MissingStart,
    MissingEnd,
    SweepBoundaries,
    NoCompleteSweep,
    NoReflectivity,
    MissingSiteCoordinates,
    AzimuthGeometry { radials: usize },
}

impl VolumeDefect {
    fn message(&self) -> String {
        match self {
            Self::MissingStart => String::from("no radial declares the volume start"),
            Self::MissingEnd => String::from("no radial declares the volume end"),
            Self::SweepBoundaries => String::from("a radial appears outside any elevation sweep"),
            Self::NoCompleteSweep => String::from("no sweep has both a first and a last radial"),
            Self::NoReflectivity => String::from("the lowest sweep carries no reflectivity gates"),
            Self::MissingSiteCoordinates => {
                String::from("no radial carries the site coordinate block")
            }
            Self::AzimuthGeometry { radials } => {
                format!("{radials} radials do not define a usable azimuth sequence")
            }
        }
    }
}

impl Display for VolumeDefect {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for VolumeDefect {}

/// A failure acquiring, decoding, or assembling a radar volume.
#[derive(Debug)]
pub enum RadarError {
    Http {
        url: String,
        source: ureq::Error,
    },
    HttpStatus {
        url: String,
        status: u16,
        retry_after: Option<String>,
    },
    VolumeGone {
        key: String,
    },
    InvalidRetryAfter {
        value: String,
    },
    Listing {
        prefix: String,
    },
    ListingBody {
        prefix: String,
    },
    ListingXml {
        prefix: String,
        source: quick_xml::Error,
    },
    NoVolume {
        site: String,
    },
    ArchiveVersion {
        key: String,
        version: [u8; 12],
    },
    ArchiveSite {
        key: String,
        expected: String,
        actual: [u8; 4],
    },
    ArchiveTime {
        key: String,
        date: u32,
        time: u32,
    },
    RecordHeader {
        key: String,
        offset: usize,
    },
    RecordBounds {
        key: String,
        offset: usize,
    },
    RecordTooLarge {
        key: String,
        offset: usize,
        limit: u64,
    },
    Decompress {
        key: String,
        offset: usize,
        source: io::Error,
    },
    Decode {
        key: String,
        offset: usize,
        source: nexrad_decode::result::Error,
    },
    MessageStructure {
        key: String,
        record_offset: usize,
        message_offset: usize,
        defect: MessageDefect,
    },
    SiteMismatch {
        requested: String,
        actual: [u8; 4],
    },
    SiteCoordinates {
        source: CoordinateError,
    },
    GateRange {
        site: String,
        azimuth_number: u16,
        source: CoordinateError,
    },
    IncompleteVolume {
        site: String,
        defect: VolumeDefect,
    },
    DecoderStopped {
        site: String,
    },
    DecoderPanic {
        site: String,
    },
    WorkerStopped {
        site: String,
    },
    WorkerPanic {
        site: String,
    },
    Clock {
        source: std::time::SystemTimeError,
    },
    DateRange {
        source: std::num::TryFromIntError,
    },
}

impl RadarError {
    fn message(&self) -> String {
        match self {
            Self::Http { url, source } => format!("The request to {url} failed: {source}"),
            Self::HttpStatus {
                url,
                status,
                retry_after,
            } => {
                format!("{url} returned {status}; Retry-After {retry_after:?}.")
            }
            Self::VolumeGone { key } => {
                format!("Volume {key} left the feed; the listing is refreshed.")
            }
            Self::InvalidRetryAfter { value } => format!("Retry-After {value:?} is not a time."),
            Self::Listing { prefix } => {
                format!("The listing for {prefix} could not be read.")
            }
            Self::ListingBody { prefix } => {
                format!("The listing for {prefix} is not UTF-8 text.")
            }
            Self::ListingXml { prefix, source } => format!("{prefix} listing is not XML: {source}"),
            Self::NoVolume { site } => format!("No {site} volume in the last three days."),
            Self::ArchiveVersion { key, version } => {
                format!("{key} declares {version:?}, which is not AR2V.")
            }
            Self::ArchiveSite {
                key,
                expected,
                actual,
            } => {
                format!("{key} is site {actual:?}; {expected} was requested.")
            }
            Self::ArchiveTime { key, date, time } => {
                format!("{key} has date {date} and time {time} ms, not a valid time.")
            }
            Self::RecordHeader { key, offset } => {
                format!("{key} at byte {offset} has no record length.")
            }
            Self::RecordBounds { key, offset } => {
                format!("{key} at byte {offset} ends past the file.")
            }
            Self::RecordTooLarge { key, offset, limit } => {
                format!("{key} at byte {offset} exceeds {limit} decompressed bytes.")
            }
            Self::Decompress {
                key,
                offset,
                source,
            } => {
                format!("Decompressing {key} at {offset} failed: {source}")
            }
            Self::Decode {
                key,
                offset,
                source,
            } => {
                format!("Decoding {key} at {offset} failed: {source}")
            }
            Self::MessageStructure {
                key,
                record_offset,
                message_offset,
                defect,
            } => {
                format!("{key} record {record_offset} message {message_offset}: {defect}")
            }
            Self::SiteMismatch { requested, actual } => {
                format!("Site {requested} was requested; radials say {actual:?}.")
            }
            Self::SiteCoordinates { source } => {
                format!("Invalid radar site coordinates: {source}")
            }
            Self::GateRange {
                site,
                azimuth_number,
                source,
            } => {
                format!("{site} radial {azimuth_number} gate range: {source}")
            }
            Self::IncompleteVolume { site, defect } => {
                format!("The {site} archive is incomplete: {defect}.")
            }
            Self::DecoderStopped { site } => {
                format!("The {site} decoder stopped; no volume arrived.")
            }
            Self::DecoderPanic { site } => {
                format!("The {site} decoder panicked; no volume arrived.")
            }
            Self::WorkerStopped { site } => {
                format!("The {site} fetch worker stopped; updates stopped.")
            }
            Self::WorkerPanic { site } => {
                format!("The {site} fetch worker panicked; no volume arrived.")
            }
            Self::Clock { source } => format!("The clock is before the Unix epoch: {source}"),
            Self::DateRange { source } => {
                format!("The date cannot be represented: {source}")
            }
        }
    }
}

impl Display for RadarError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for RadarError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Http { source, .. } => Some(source),
            Self::Decompress { source, .. } => Some(source),
            Self::Decode { source, .. } => Some(source),
            Self::MessageStructure { defect, .. } => Some(defect),
            Self::SiteCoordinates { source } => Some(source),
            Self::GateRange { source, .. } => Some(source),
            Self::ListingXml { source, .. } => Some(source),
            Self::IncompleteVolume { defect, .. } => Some(defect),
            Self::Clock { source } => Some(source),
            Self::DateRange { source } => Some(source),
            Self::HttpStatus { .. }
            | Self::VolumeGone { .. }
            | Self::InvalidRetryAfter { .. }
            | Self::Listing { .. }
            | Self::ListingBody { .. }
            | Self::NoVolume { .. }
            | Self::ArchiveVersion { .. }
            | Self::ArchiveSite { .. }
            | Self::ArchiveTime { .. }
            | Self::RecordHeader { .. }
            | Self::RecordBounds { .. }
            | Self::RecordTooLarge { .. }
            | Self::SiteMismatch { .. }
            | Self::DecoderStopped { .. }
            | Self::DecoderPanic { .. }
            | Self::WorkerStopped { .. }
            | Self::WorkerPanic { .. } => None,
        }
    }
}

/// A failure mapping a reflectivity value to a palette entry.
#[derive(Debug)]
pub enum ColorMapError {
    OutOfRange { product: &'static str, value: f32 },
}

impl Display for ColorMapError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { product, value } => {
                write!(
                    formatter,
                    "Value {value} is out of range for product {product}."
                )
            }
        }
    }
}

impl Error for ColorMapError {}

/// A failure preparing or drawing the radar frame.
#[derive(Debug)]
pub enum RenderError {
    WorkerStopped,
    GpuUnavailable,
    Panic,
    Coordinate { source: CoordinateError },
    Color { source: ColorMapError },
    MeshSize { vertices: usize },
}

impl RenderError {
    fn message(&self) -> String {
        match self {
            Self::WorkerStopped => {
                String::from("The render worker stopped before producing a frame.")
            }
            Self::GpuUnavailable => {
                String::from("The WGPU renderer is unavailable; radar cannot be displayed.")
            }
            Self::Panic => {
                String::from("The render worker panicked while preparing the radar frame.")
            }
            Self::Coordinate { source } => {
                format!("Radar geographic placement failed: {source}")
            }
            Self::Color { source } => {
                format!("Failed to map radar value to a color: {source}")
            }
            Self::MeshSize { vertices } => {
                format!("The radar mesh has {vertices} vertices, too many to draw.")
            }
        }
    }
}

impl Display for RenderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for RenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Coordinate { source } => Some(source),
            Self::Color { source } => Some(source),
            Self::WorkerStopped | Self::GpuUnavailable | Self::Panic | Self::MeshSize { .. } => {
                None
            }
        }
    }
}
