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

    #[error("Azimuth {azimuth_deg} is invalid; it must be in the range 0 to 360.")]
    Azimuth { azimuth_deg: f64 },
}

#[derive(Debug, Error)]
pub enum ColorMapError {
    #[error("Value {value} is out of range for product {product}.")]
    OutOfRange { product: &'static str, value: f32 },

    #[error("Value {value} did not match any palette interval for product {product}.")]
    Unmapped { product: &'static str, value: f32 },
}

#[derive(Debug, Error)]
pub enum RadarError {
    #[error("HTTP request to {url} failed: {source}")]
    Http {
        url: String,
        #[source]
        source: ureq::Error,
    },

    #[error("S3 listing response for prefix {prefix} could not be parsed.")]
    Listing { prefix: String },

    #[error("No volume was listed for site {site} in the last three days.")]
    NoVolume { site: String },

    #[error("Archive record at byte {offset} of {key} has no four-byte length prefix.")]
    RecordHeader { key: String, offset: usize },

    #[error("Archive record at byte {offset} of {key} extends past the end of the file.")]
    RecordBounds { key: String, offset: usize },

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

    #[error("No volume data block was found in any radial; site coordinates are unavailable.")]
    MissingSiteCoordinates,

    #[error("No reflectivity data was found in the selected sweep for site {site}.")]
    NoReflectivityData { site: String },

    #[error("The clock is before the Unix epoch or outside the supported date range: {detail}")]
    Clock { detail: String },
}
