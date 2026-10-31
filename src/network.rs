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
//! The AWS NEXRAD Level II source: listing, ordering, and retrieval.

use std::collections::HashSet;
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::error::RadarError;
use crate::stations::SiteId;
use crate::time::{MS_PER_DAY, civil_from_days, now_unix_ms};

const BUCKET_URL: &str = "https://unidata-nexrad-level2.s3.amazonaws.com";
const LOOKBACK_DAYS: i64 = 3;
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LIST_PAGES: usize = 10_000;
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

#[derive(Default)]
struct ListingPage {
    keys: Vec<String>,
    truncated: Option<bool>,
    continuation: Option<String>,
}

fn listing_xml(prefix: &str, xml: &str) -> Result<ListingPage, RadarError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut page = ListingPage::default();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => names.push(event.name().as_ref().to_vec()),
            Ok(Event::End(_)) => {
                names.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map_err(|source| RadarError::ListingXml {
                    prefix: String::from(prefix),
                    source: source.into(),
                })?;
                let Some(name) = names.last() else {
                    continue;
                };
                match name.as_slice() {
                    b"Key" if names.iter().any(|name| name.as_slice() == b"Contents") => {
                        page.keys.push(value.into_owned());
                    }
                    b"IsTruncated" => {
                        let parsed = match value.trim() {
                            "true" => true,
                            "false" => false,
                            _ => {
                                return Err(RadarError::Listing {
                                    prefix: String::from(prefix),
                                });
                            }
                        };
                        if page.truncated.replace(parsed).is_some() {
                            return Err(RadarError::Listing {
                                prefix: String::from(prefix),
                            });
                        }
                    }
                    b"NextContinuationToken" if page.continuation.is_some() => {
                        return Err(RadarError::Listing {
                            prefix: String::from(prefix),
                        });
                    }
                    b"NextContinuationToken" => page.continuation = Some(value.into_owned()),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(source) => {
                return Err(RadarError::ListingXml {
                    prefix: String::from(prefix),
                    source,
                });
            }
        }
    }
    if page.truncated.is_none() {
        return Err(RadarError::Listing {
            prefix: String::from(prefix),
        });
    }
    Ok(page)
}

/// Paginates a bucket listing prefix to exhaustion.
fn list_keys(prefix: &str) -> Result<Vec<String>, RadarError> {
    let mut keys = Vec::new();
    let mut token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
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
        let xml = String::from_utf8(body).map_err(|_| RadarError::ListingBody {
            prefix: String::from(prefix),
        })?;
        let page = listing_xml(prefix, &xml)?;
        keys.extend(page.keys);
        match page.truncated {
            Some(false) => break,
            Some(true) => {}
            None => {
                return Err(RadarError::Listing {
                    prefix: String::from(prefix),
                });
            }
        }
        token = match page.continuation {
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

/// The newest volume the feed lists for a site, ordered by its UTC timestamp.
pub fn latest_volume_key(site: &SiteId) -> Result<String, RadarError> {
    for days_ago in 0..LOOKBACK_DAYS {
        let (year, month, day) = utc_day(days_ago)?;
        let prefix = format!("{year:04}/{month:02}/{day:02}/{site}/");
        let keys = list_keys(&prefix)?;
        if let Some(key) = keys.iter().filter(|key| is_volume_key(key)).max() {
            return Ok(key.clone());
        }
    }
    Err(RadarError::NoVolume {
        site: String::from(site.as_str()),
    })
}

/// Retrieves one Archive II volume. A 404 means the listing went stale.
pub fn fetch_archive(key: &str) -> Result<Vec<u8>, RadarError> {
    match http_get(&format!("{BUCKET_URL}/{key}")) {
        Ok(bytes) => Ok(bytes),
        Err(RadarError::HttpStatus { status: 404, .. }) => Err(RadarError::VolumeGone {
            key: String::from(key),
        }),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TestError;

    fn site() -> Result<SiteId, TestError> {
        Ok(SiteId::parse("KPAH")?)
    }

    #[test]
    fn volume_listing_excludes_metadata() {
        assert!(is_volume_key("2026/10/29/KPAH/KPAH20261029_000115_V06"));
        assert!(!is_volume_key("2026/10/29/KPAH/KPAH20261029_000115_MDM"));
        assert!(!is_volume_key(
            "2026/10/29/KPAH/KPAH20261029_000115_V06_MDM"
        ));
    }

    #[test]
    fn listing_xml_tolerates_whitespace_and_nesting() -> Result<(), TestError> {
        let xml = "<?xml version=\"1.0\"?>\n<ListBucketResult>\n\
                   <IsTruncated>\n true \n</IsTruncated>\n\
                   <Contents><Key> 2026/10/30/KPAH/KPAH20261030_023529_V06 </Key></Contents>\n\
                   <Contents><Key>2026/10/30/KPAH/KPAH20261030_024005_V06</Key></Contents>\n\
                   <NextContinuationToken>\n abc \n</NextContinuationToken>\n\
                   </ListBucketResult>";
        let page = listing_xml("prefix", xml)?;
        assert_eq!(
            page.keys,
            vec![
                "2026/10/30/KPAH/KPAH20261030_023529_V06",
                "2026/10/30/KPAH/KPAH20261030_024005_V06"
            ]
        );
        assert_eq!(page.truncated, Some(true));
        assert_eq!(page.continuation.as_deref(), Some("abc"));
        Ok(())
    }

    #[test]
    fn malformed_listing_xml_is_rejected() {
        let xml = "<ListBucketResult><IsTruncated>false</ListBucketResult>";
        assert!(matches!(
            listing_xml("prefix", xml),
            Err(RadarError::ListingXml { .. })
        ));
    }

    #[test]
    fn query_values_keep_path_separators() {
        assert_eq!(encode_query_value("2026/10/30/KPAH/"), "2026/10/30/KPAH/");
        assert_eq!(encode_query_value("a b+c"), "a%20b%2Bc");
    }

    /// Exercises the live source: listing, ordering, retrieval, and the
    /// completeness sentinels inside the decoded volume. A network failure
    /// fails this test; it is not replaced with synthetic data.
    #[test]
    fn live_listing_retrieves_a_complete_current_volume() -> Result<(), TestError> {
        let site = site()?;
        let key = latest_volume_key(&site)?;
        assert!(is_volume_key(&key), "listed key {key}");
        let name = key
            .rsplit('/')
            .next()
            .ok_or("listed key has no file name")?;
        assert!(name.starts_with("KPAH"), "listed key {key}");
        assert_eq!(
            name.len(),
            "KPAH20261030_023529_V06".len(),
            "volume file name {name}"
        );
        assert!(name.ends_with("_V06"), "volume file name {name}");
        let stamp = name
            .get(4..19)
            .ok_or_else(|| format!("volume file name {name} has no timestamp"))?;
        let digits = stamp
            .chars()
            .filter(|character| *character != '_')
            .all(|character| character.is_ascii_digit());
        assert!(digits, "volume timestamp {stamp}");
        let bytes = fetch_archive(&key)?;
        assert!(bytes.len() > 1_000_000, "archive size {}", bytes.len());
        let volume = crate::radar::decode_volume(&site, &key, &bytes)?;
        assert!(
            volume.sweep.radials.len() >= 360,
            "radial count {}",
            volume.sweep.radials.len()
        );
        assert_eq!(volume.skipped_radials, 0, "skipped radials");
        let now = now_unix_ms()?;
        let age_ms = now.saturating_sub(volume.scan_unix_ms);
        assert!(age_ms < 3 * 3_600_000, "live volume age {age_ms} ms");
        Ok(())
    }

    #[test]
    fn a_missing_volume_is_reported_as_a_stale_listing() -> Result<(), TestError> {
        match fetch_archive("2026/10/30/KPAH/KPAH20261030_999999_V06") {
            Err(RadarError::VolumeGone { key }) => {
                assert_eq!(key, "2026/10/30/KPAH/KPAH20261030_999999_V06");
                Ok(())
            }
            other => Err(TestError::from(format!(
                "expected VolumeGone, got {other:?}"
            ))),
        }
    }
}
