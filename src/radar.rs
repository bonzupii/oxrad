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
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use nexrad_decode::messages::Message;
use nexrad_decode::messages::MessageContents;
use nexrad_decode::messages::digital_radar_data::{Message as DigitalRadarData, ScaledMomentValue};

use crate::error::RadarError;
use crate::projection;

const BUCKET_URL: &str = "https://unidata-nexrad-level2.s3.amazonaws.com";
const ARCHIVE_HEADER_BYTES: usize = 24;
const RECORD_LENGTH_BYTES: usize = 4;
const BELOW_THRESHOLD_DBZ: f32 = -32.0;
const LOOKBACK_DAYS: i64 = 3;
const SECONDS_PER_DAY: u64 = 86_400;
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct RadarBin {
    pub corners: [(f64, f64); 4],
    pub dbz: f32,
}

#[derive(Debug)]
pub struct RadarVolume {
    pub site: String,
    pub site_lat: f64,
    pub site_lon: f64,
    pub bins: Vec<RadarBin>,
    pub max_range_km: f64,
    pub skipped_radials: usize,
    pub skipped_bins: usize,
    pub skipped_non_data: usize,
}

#[derive(Clone, Copy)]
struct GateGeometry {
    site_lat: f64,
    site_lon: f64,
    azimuth_deg: f64,
    half_width_deg: f64,
    first_gate_km: f64,
    gate_width_km: f64,
    gate_count: usize,
}

struct SweepBuilder {
    site_lat: Option<f64>,
    site_lon: Option<f64>,
    selected_elevation: Option<u8>,
    bins: Vec<RadarBin>,
    max_range_km: f64,
    skipped_radials: usize,
    skipped_bins: usize,
    skipped_non_data: usize,
    sweep_complete: bool,
}

impl SweepBuilder {
    const fn new() -> Self {
        Self {
            site_lat: None,
            site_lon: None,
            selected_elevation: None,
            bins: Vec::new(),
            max_range_km: 0.0,
            skipped_radials: 0,
            skipped_bins: 0,
            skipped_non_data: 0,
            sweep_complete: false,
        }
    }

    fn push_message(&mut self, message: Message) -> bool {
        match message.into_contents() {
            MessageContents::DigitalRadarData(radar_data) => self.push_radial(&radar_data),
            _ => {
                self.skipped_non_data += 1;
                false
            }
        }
    }

    fn push_radial(&mut self, radar_data: &DigitalRadarData) -> bool {
        let status = radar_data.header.radial_status;
        if (status == 0 || status == 3)
            && self.site_lat.is_none()
            && let Some(block) = &radar_data.volume_data_block
        {
            self.site_lat = Some(f64::from(block.latitude));
            self.site_lon = Some(f64::from(block.longitude));
        }

        let elevation = radar_data.header.elevation_number;
        match self.selected_elevation {
            None => self.selected_elevation = Some(elevation),
            Some(selected) if selected != elevation => return true,
            Some(_) => {}
        }

        let Some(block) = &radar_data.reflectivity_data_block else {
            self.skipped_radials += 1;
            return false;
        };

        let first_gate_m = f64::from(block.header.data_moment_range);
        let gate_width_m = f64::from(block.header.data_moment_range_sample_interval);
        let gate_count = usize::from(block.header.number_of_data_moment_gates);
        if first_gate_m <= 0.0 || gate_width_m <= 0.0 || gate_count == 0 {
            self.skipped_radials += 1;
            return false;
        }

        let (Some(site_lat), Some(site_lon)) = (self.site_lat, self.site_lon) else {
            self.skipped_radials += 1;
            return false;
        };

        let azimuth_deg = f64::from(radar_data.header.azimuth_angle);
        let half_width_deg = f64::from(radar_data.header.azimuth_resolution_spacing) * 0.5;
        let values = block.decoded_values();
        self.push_gates(
            GateGeometry {
                site_lat,
                site_lon,
                azimuth_deg,
                half_width_deg,
                first_gate_km: first_gate_m / 1000.0,
                gate_width_km: gate_width_m / 1000.0,
                gate_count,
            },
            &values,
        );

        status == 2 || status == 4
    }

    fn push_gates(&mut self, gates: GateGeometry, values: &[ScaledMomentValue]) {
        let GateGeometry {
            site_lat,
            site_lon,
            azimuth_deg,
            half_width_deg,
            first_gate_km,
            gate_width_km,
            gate_count,
        } = gates;

        for (index, value) in values.iter().take(gate_count).enumerate() {
            let dbz = match value {
                ScaledMomentValue::Value(dbz) => *dbz,
                ScaledMomentValue::BelowThreshold => BELOW_THRESHOLD_DBZ,
                ScaledMomentValue::RangeFolded => f32::NAN,
            };
            let center_km = first_gate_km + index as f64 * gate_width_km;
            let near_km = center_km - gate_width_km / 2.0;
            let far_km = center_km + gate_width_km / 2.0;

            let Ok(corners) = bin_corners(
                site_lat,
                site_lon,
                near_km,
                far_km,
                azimuth_deg,
                half_width_deg,
            ) else {
                self.skipped_bins += 1;
                continue;
            };

            self.bins.push(RadarBin { corners, dbz });
            if far_km > self.max_range_km {
                self.max_range_km = far_km;
            }
        }
    }

    fn finish(self, site: &str) -> Result<RadarVolume, RadarError> {
        let site_lat = self
            .site_lat
            .ok_or(RadarError::MissingSiteCoordinates)?;
        let site_lon = self
            .site_lon
            .ok_or(RadarError::MissingSiteCoordinates)?;
        if self.bins.is_empty() {
            return Err(RadarError::NoReflectivityData {
                site: String::from(site),
            });
        }

        Ok(RadarVolume {
            site: String::from(site),
            site_lat,
            site_lon,
            bins: self.bins,
            max_range_km: self.max_range_km,
            skipped_radials: self.skipped_radials,
            skipped_bins: self.skipped_bins,
            skipped_non_data: self.skipped_non_data,
        })
    }
}

fn bin_corners(
    site_lat: f64,
    site_lon: f64,
    near_km: f64,
    far_km: f64,
    azimuth_deg: f64,
    half_width_deg: f64,
) -> Result<[(f64, f64); 4], crate::error::CoordinateError> {
    let left = (azimuth_deg - half_width_deg).rem_euclid(360.0);
    let right = (azimuth_deg + half_width_deg).rem_euclid(360.0);

    let near_left = projection::forward(near_km, left, site_lat, site_lon)?;
    let near_right = projection::forward(near_km, right, site_lat, site_lon)?;
    let far_right = projection::forward(far_km, right, site_lat, site_lon)?;
    let far_left = projection::forward(far_km, left, site_lat, site_lon)?;

    Ok([near_left, near_right, far_right, far_left])
}

fn http_get(url: &str) -> Result<Vec<u8>, RadarError> {
    let mut response = ureq::get(url).call().map_err(|source| RadarError::Http {
        url: String::from(url),
        source,
    })?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()
        .map_err(|source| RadarError::Http {
            url: String::from(url),
            source,
        })
}

fn encode_query_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        let unreserved = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/');
        if unreserved {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(hex_digit((byte >> 4) & 0x0f));
            encoded.push(hex_digit(byte & 0x0f));
        }
    }
    encoded
}

fn hex_digit(value: u8) -> char {
    if value < 10 {
        char::from(b'0' + value)
    } else {
        char::from(b'A' + value - 10)
    }
}

fn tag_values<'a>(text: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut remaining = text;

    while let Some(start) = remaining.find(&open) {
        let Some(after_open) = remaining.get(start + open.len()..) else {
            break;
        };
        let Some(end) = after_open.find(&close) else {
            break;
        };
        if let Some(value) = after_open.get(..end) {
            values.push(value);
        }
        let Some(next) = after_open.get(end + close.len()..) else {
            break;
        };
        remaining = next;
    }

    values
}

fn list_keys(prefix: &str) -> Result<Vec<String>, RadarError> {
    let mut keys = Vec::new();
    let mut token: Option<String> = None;

    loop {
        let mut url = format!(
            "{BUCKET_URL}?list-type=2&prefix={}",
            encode_query_value(prefix)
        );
        if let Some(current) = &token {
            url.push_str("&continuation-token=");
            url.push_str(&encode_query_value(current));
        }

        let body = http_get(&url)?;
        let xml = String::from_utf8(body).map_err(|_| RadarError::Listing {
            prefix: String::from(prefix),
        })?;
        keys.extend(tag_values(&xml, "Key").into_iter().map(String::from));

        if !xml.contains("<IsTruncated>true</IsTruncated>") {
            break;
        }

        let next = tag_values(&xml, "NextContinuationToken")
            .into_iter()
            .next()
            .map(String::from);
        token = match next {
            Some(value) => Some(value),
            None => {
                return Err(RadarError::Listing {
                    prefix: String::from(prefix),
                });
            }
        };
    }

    Ok(keys)
}

const fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let civil_year = if month <= 2 { year + 1 } else { year };
    (civil_year as i32, month as u32, day as u32)
}

fn utc_day(offset_days: i64) -> Result<(i32, u32, u32), RadarError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RadarError::Clock {
            detail: error.to_string(),
        })?;
    let days = i64::try_from(elapsed.as_secs() / SECONDS_PER_DAY).map_err(|error| {
        RadarError::Clock {
            detail: error.to_string(),
        }
    })?;
    Ok(civil_from_days(days + offset_days))
}

fn latest_volume_key(site: &str) -> Result<String, RadarError> {
    for offset in 0..LOOKBACK_DAYS {
        let (year, month, day) = utc_day(offset)?;
        let prefix = format!("{year:04}/{month:02}/{day:02}/{site}/");
        let keys = list_keys(&prefix)?;
        if let Some(key) = keys.last() {
            return Ok(key.clone());
        }
    }

    Err(RadarError::NoVolume {
        site: String::from(site),
    })
}

fn decode_volume(site: &str, key: &str, bytes: &[u8]) -> Result<RadarVolume, RadarError> {
    if bytes.len() < ARCHIVE_HEADER_BYTES {
        return Err(RadarError::RecordBounds {
            key: String::from(key),
            offset: 0,
        });
    }

    let mut builder = SweepBuilder::new();
    let mut offset = ARCHIVE_HEADER_BYTES;

    while offset + RECORD_LENGTH_BYTES <= bytes.len() {
        let length_bytes = bytes
            .get(offset..offset + RECORD_LENGTH_BYTES)
            .ok_or_else(|| RadarError::RecordHeader {
                key: String::from(key),
                offset,
            })?;
        let length: [u8; 4] = length_bytes.try_into().map_err(|_| RadarError::RecordHeader {
            key: String::from(key),
            offset,
        })?;
        let size = i32::from_be_bytes(length).unsigned_abs() as usize;
        let start = offset + RECORD_LENGTH_BYTES;
        let end = start.checked_add(size).ok_or_else(|| RadarError::RecordBounds {
            key: String::from(key),
            offset,
        })?;
        let record = bytes.get(start..end).ok_or_else(|| RadarError::RecordBounds {
            key: String::from(key),
            offset,
        })?;

        let mut decompressed = Vec::new();
        bzip2_rs::DecoderReader::new(Cursor::new(record))
            .read_to_end(&mut decompressed)
            .map_err(|source| RadarError::Decompress {
                key: String::from(key),
                offset,
                source,
            })?;

        let messages = nexrad_decode::messages::decode_messages(&mut Cursor::new(
            decompressed.as_slice(),
        ))
        .map_err(|source| RadarError::Decode {
            key: String::from(key),
            offset,
            source,
        })?;

        for message in messages {
            if builder.push_message(message) {
                builder.sweep_complete = true;
                break;
            }
        }

        if builder.sweep_complete {
            break;
        }
        offset = end;
    }

    builder.finish(site)
}

pub fn fetch_volume(site: &str) -> Result<RadarVolume, RadarError> {
    let key = latest_volume_key(site)?;
    let url = format!("{BUCKET_URL}/{key}");
    let bytes = http_get(&url)?;
    decode_volume(site, &key, &bytes)
}

pub fn start_fetch_thread(
    site: String,
) -> mpsc::Receiver<Result<RadarVolume, RadarError>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = fetch_volume(&site);
        if let Err(mpsc::SendError(_)) = sender.send(result) {
            // The receiver drops with the window; no UI remains to report to.
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "2026/10/29/KPAH/KPAH20261029_000115_V06";

    #[test]
    fn truncated_archive_reports_bounds_with_key_and_offset() {
        let result = decode_volume("KPAH", KEY, &[0_u8; 10]);
        assert!(
            matches!(result, Err(RadarError::RecordBounds { ref key, offset: 0 }) if key == KEY),
            "expected RecordBounds at offset 0 for {KEY}, got {result:?}"
        );
    }

    #[test]
    fn header_only_archive_reports_missing_site_coordinates() {
        let result = decode_volume("KPAH", KEY, &[0_u8; ARCHIVE_HEADER_BYTES]);
        assert!(
            matches!(result, Err(RadarError::MissingSiteCoordinates)),
            "expected MissingSiteCoordinates, got {result:?}"
        );
    }

    #[test]
    fn malformed_record_reports_decompression_with_key_and_offset() {
        let mut bytes = vec![0_u8; ARCHIVE_HEADER_BYTES];
        bytes.extend_from_slice(&[0, 0, 0, 8]);
        bytes.extend_from_slice(&[0_u8; 8]);
        let result = decode_volume("KPAH", KEY, &bytes);
        assert!(
            matches!(
                result,
                Err(RadarError::Decompress { ref key, offset: 24, .. }) if key == KEY
            ),
            "expected Decompress at offset 24 for {KEY}, got {result:?}"
        );
    }
}
