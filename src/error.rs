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

#[derive(Error, Debug)]
pub enum RadarError {
    #[error("Network error occurred while fetching radar data for site {site}: {source}")]
    NetworkFetch {
        site: String,
        source: nexrad_data::result::Error,
    },

    #[error("The latest volume for site {site} was not found; no volume index was returned.")]
    LatestVolumeNotFound { site: String },

    #[error("Chunk {chunk_id} for site {site} failed to download: {source}")]
    ChunkDownload {
        site: String,
        chunk_id: String,
        source: nexrad_data::result::Error,
    },

    #[error("Radar data failed to decode: {source}")]
    Decode { source: nexrad_data::result::Error },

    #[error("No reflectivity data was found in the sweep.")]
    NoReflectivityData,

    #[error("No volume data block was found in any radial; site coordinates unavailable.")]
    MissingVolumeDataBlock,

    #[error("The tokio runtime failed to initialize: {source}")]
    RuntimeCreation { source: std::io::Error },

    #[error("Failed to send radar data to the UI thread: {0}")]
    SendFailure(String),

    #[error("S3 chunk list was truncated after {pages} pagination attempts for site {site}")]
    ChunkListTruncated { site: String, pages: usize },
}

#[derive(Error, Debug)]
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

#[derive(Error, Debug)]
pub enum ColorMapError {
    #[error("Value {value} is out of range for product {product}.")]
    OutOfRange { product: &'static str, value: f32 },
}

#[derive(Error, Debug)]
pub enum RenderError {
    #[error("Colormap rejected dbz={dbz}: {source}")]
    ColorMap { dbz: f32, source: ColorMapError },

    #[error("Forward projection failed for bin corner {corner}: {source}")]
    BinCorner {
        corner: &'static str,
        source: CoordinateError,
    },

    #[error("Forward projection failed for range ring at {range_km} km, az {az_deg}: {source}")]
    RangeRing {
        range_km: f64,
        az_deg: f64,
        source: CoordinateError,
    },
}
