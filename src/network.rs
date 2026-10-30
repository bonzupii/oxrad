// oxrad: A storm-chaser-grade NEXRAD weather radar viewer
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
use std::collections::HashSet;
use std::time::Duration;

use crate::error::RadarError;
use crate::time::{MS_PER_DAY, civil_from_days, now_unix_ms};

const BUCKET_URL: &str = "https://unidata-nexrad-level2.s3.amazonaws.com";
const LOOKBACK_DAYS: i64 = 3;
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!(
    "oxrad/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/bonzupii/oxrad)"
);

fn http_get(url: &str) -> Result<Vec<u8>, RadarError> {
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|source| RadarError::Http {
            url: String::from(url),
            source,
        })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = response
            .headers()
            .get("retry-after")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
        return Err(RadarError::HttpStatus {
            url: String::from(url),
            status,
            retry_after,
        });
    }
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
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
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
    let mut seen_tokens = HashSet::new();
    const MAX_LIST_PAGES: usize = 10_000;
    loop {
        if seen_tokens.len() >= MAX_LIST_PAGES {
            return Err(RadarError::Listing {
                prefix: String::from(prefix),
            });
        }
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
        match tag_values(&xml, "IsTruncated").as_slice() {
            ["false"] => break,
            ["true"] => {}
            _ => {
                return Err(RadarError::Listing {
                    prefix: String::from(prefix),
                });
            }
        }
        let next = tag_values(&xml, "NextContinuationToken")
            .into_iter()
            .next()
            .map(String::from);
        token = match next {
            Some(value) if seen_tokens.insert(value.clone()) => Some(value),
            Some(_) | None => {
                return Err(RadarError::Listing {
                    prefix: String::from(prefix),
                });
            }
        };
    }
    Ok(keys)
}

fn utc_day(days_ago: i64) -> Result<(i32, u32, u32), RadarError> {
    let now_ms = now_unix_ms().map_err(|source| RadarError::Clock { source })?;
    let days =
        i64::try_from(now_ms / MS_PER_DAY).map_err(|source| RadarError::DateRange { source })?;
    Ok(civil_from_days(days - days_ago))
}

fn is_volume_key(key: &str) -> bool {
    let Some((_, version)) = key.rsplit_once("_V") else {
        return false;
    };
    version.len() == 2 && version.bytes().all(|byte| byte.is_ascii_digit())
}

pub fn latest_volume_key(site: &str) -> Result<String, RadarError> {
    for days_ago in 0..LOOKBACK_DAYS {
        let (year, month, day) = utc_day(days_ago)?;
        let prefix = format!("{year:04}/{month:02}/{day:02}/{site}/");
        let keys = list_keys(&prefix)?;
        if let Some(key) = keys.iter().filter(|key| is_volume_key(key)).max() {
            return Ok(key.clone());
        }
    }
    Err(RadarError::NoVolume {
        site: String::from(site),
    })
}

pub fn fetch_archive(key: &str) -> Result<Vec<u8>, RadarError> {
    http_get(&format!("{BUCKET_URL}/{key}"))
}

#[cfg(test)]
mod tests {
    use super::is_volume_key;

    #[test]
    fn volume_listing_excludes_metadata() {
        assert!(is_volume_key("2026/10/29/KPAH/KPAH20261029_000115_V06"));
        assert!(!is_volume_key("2026/10/29/KPAH/KPAH20261029_000115_MDM"));
        assert!(!is_volume_key(
            "2026/10/29/KPAH/KPAH20261029_000115_V06_MDM"
        ));
    }
}
