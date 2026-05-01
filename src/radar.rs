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
use std::sync::mpsc;

use crate::error::RadarError;

const BELOW_THRESHOLD_DBZ: f32 = -32.0;

pub struct RadarBin {
    pub azimuth_deg: f32,
    pub azimuth_width_deg: f32,
    pub range_start_km: f32,
    pub range_width_km: f32,
    pub dbz: f32,
}

pub struct RadarSweep {
    pub bins: Vec<RadarBin>,
    pub max_range_km: f64,
}

struct ExtractedSweep {
    bins: Vec<RadarBin>,
    site_lat: f64,
    site_lon: f64,
    max_range_km: f64,
    skipped_non_data_messages: usize,
    skipped_radials: usize,
}

pub struct RadarVolume {
    pub sweeps: Vec<RadarSweep>,
    pub site: String,
    pub site_lat: f64,
    pub site_lon: f64,
    pub skipped_non_data_messages: usize,
    pub skipped_radials: usize,
}

pub enum RadarMessage {
    Volume(RadarVolume),
}

fn extract_bins_from_messages(
    records: &[nexrad_data::volume::Record<'_>],
) -> Result<ExtractedSweep, RadarError> {
    let mut bins: Vec<RadarBin> = Vec::new();
    let mut site_lat: Option<f64> = None;
    let mut site_lon: Option<f64> = None;
    let mut skipped_radials: usize = 0;
    let mut skipped_non_data_messages: usize = 0;
    let mut selected_elevation: Option<u8> = None;
    let mut max_range_km: f64 = 0.0;

    'records: for record in records {
        let decompressed_record;
        let decode_record = if record.compressed() {
            decompressed_record = match record.decompress() {
                Ok(decompressed) => decompressed,
                Err(e) => {
                    return Err(RadarError::Decode { source: e });
                }
            };
            &decompressed_record
        } else {
            record
        };

        let messages = match decode_record.messages() {
            Ok(msgs) => msgs,
            Err(e) => {
                return Err(RadarError::Decode { source: e });
            }
        };

        for message in messages {
            let inner = match message.contents() {
                nexrad_decode::messages::MessageContents::DigitalRadarData(drd) => drd,
                _ => {
                    skipped_non_data_messages += 1;
                    continue;
                }
            };

            if (inner.header.radial_status == 0 || inner.header.radial_status == 3)
                && let Some(ref vdb) = inner.volume_data_block
                && site_lat.is_none()
            {
                site_lat = Some(vdb.latitude as f64);
                site_lon = Some(vdb.longitude as f64);
            }

            let refl_block = match inner.reflectivity_data_block {
                Some(ref b) => b,
                None => {
                    skipped_radials += 1;
                    continue;
                }
            };

            let elevation_number = inner.header.elevation_number;
            match selected_elevation {
                Some(selected) => {
                    if elevation_number != selected {
                        break 'records;
                    }
                }
                None => {
                    selected_elevation = Some(elevation_number);
                }
            }

            let first_gate_center_km = refl_block.header.data_moment_range as f32 / 1000.0;
            let gate_width_km = refl_block.header.data_moment_range_sample_interval as f32 / 1000.0;
            let num_gates = refl_block.header.number_of_data_moment_gates as usize;

            if first_gate_center_km <= 0.0 || gate_width_km <= 0.0 || num_gates == 0 {
                skipped_radials += 1;
                continue;
            }

            let az_deg = inner.header.azimuth_angle;
            let az_width = inner.header.azimuth_resolution_spacing as f32 * 0.5;

            let decoded = refl_block.decoded_values();

            for (i, val) in decoded.iter().enumerate() {
                if i >= num_gates {
                    break;
                }

                let gate_center_km = first_gate_center_km + i as f32 * gate_width_km;
                let range_start_km = gate_center_km - gate_width_km / 2.0;

                let dbz = match val {
                    nexrad_decode::messages::digital_radar_data::ScaledMomentValue::Value(v) => *v,
                    nexrad_decode::messages::digital_radar_data::ScaledMomentValue::BelowThreshold => BELOW_THRESHOLD_DBZ,
                    nexrad_decode::messages::digital_radar_data::ScaledMomentValue::RangeFolded => f32::NAN,
                };

                bins.push(RadarBin {
                    azimuth_deg: az_deg,
                    azimuth_width_deg: az_width,
                    range_start_km,
                    range_width_km: gate_width_km,
                    dbz,
                });
                let far_edge = (range_start_km + gate_width_km) as f64;
                if far_edge > max_range_km {
                    max_range_km = far_edge;
                }
            }

            if (inner.header.radial_status == 2 || inner.header.radial_status == 4)
                && site_lat.is_some()
            {
                break 'records;
            }
        }
    }

    let site_lat = match site_lat {
        Some(lat) => lat,
        None => return Err(RadarError::MissingVolumeDataBlock),
    };
    let site_lon = match site_lon {
        Some(lon) => lon,
        None => return Err(RadarError::MissingVolumeDataBlock),
    };

    if bins.is_empty() {
        return Err(RadarError::NoReflectivityData);
    }

    Ok(ExtractedSweep {
        bins,
        site_lat,
        site_lon,
        max_range_km,
        skipped_non_data_messages,
        skipped_radials,
    })
}

pub fn fetch_volume(site: String) -> Result<RadarVolume, RadarError> {
    let rt =
        tokio::runtime::Runtime::new().map_err(|e| RadarError::RuntimeCreation { source: e })?;

    let latest = match rt.block_on(nexrad_data::aws::realtime::get_latest_volume(&site)) {
        Ok(vol) => vol,
        Err(e) => {
            return Err(RadarError::NetworkFetch { site, source: e });
        }
    };

    let volume_index = match latest.volume {
        Some(idx) => idx,
        None => {
            return Err(RadarError::LatestVolumeNotFound { site });
        }
    };

    let chunk_ids: Vec<nexrad_data::aws::realtime::ChunkIdentifier>;
    let mut max_keys: usize = 100;
    let mut pages: usize = 0;
    loop {
        pages += 1;
        if pages > 20 {
            return Err(RadarError::ChunkListTruncated { site, pages });
        }
        match rt.block_on(nexrad_data::aws::realtime::list_chunks_in_volume(
            &site,
            volume_index,
            max_keys,
        )) {
            Ok(ids) => {
                chunk_ids = ids;
                break;
            }
            Err(nexrad_data::result::Error::AWS(
                nexrad_data::result::aws::AWSError::TruncatedListObjectsResponse,
            )) => {
                max_keys *= 2;
            }
            Err(e) => {
                return Err(RadarError::NetworkFetch { site, source: e });
            }
        }
    }

    let mut all_data: Vec<u8> = Vec::new();
    for chunk_id in &chunk_ids {
        let (_id, chunk) =
            match rt.block_on(nexrad_data::aws::realtime::download_chunk(&site, chunk_id)) {
                Ok(pair) => pair,
                Err(e) => {
                    return Err(RadarError::ChunkDownload {
                        site,
                        chunk_id: format!("{chunk_id:?}"),
                        source: e,
                    });
                }
            };
        all_data.extend_from_slice(chunk.data());
    }

    let file = nexrad_data::volume::File::new(all_data);
    let records = file.records();

    let extracted = extract_bins_from_messages(&records)?;

    Ok(RadarVolume {
        sweeps: vec![RadarSweep {
            bins: extracted.bins,
            max_range_km: extracted.max_range_km,
        }],
        site,
        site_lat: extracted.site_lat,
        site_lon: extracted.site_lon,
        skipped_non_data_messages: extracted.skipped_non_data_messages,
        skipped_radials: extracted.skipped_radials,
    })
}

pub fn start_fetch_thread(
    site: String,
) -> (
    mpsc::Receiver<RadarMessage>,
    std::thread::JoinHandle<Result<(), RadarError>>,
) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let volume = fetch_volume(site)?;
        match tx.send(RadarMessage::Volume(volume)) {
            Ok(()) => Ok(()),
            Err(send_err) => Err(RadarError::SendFailure(format!("{send_err}"))),
        }
    });
    (rx, handle)
}
