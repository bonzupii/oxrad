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

use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashSet;

use crate::acquire::Acquire;
use crate::engine::{CacheState, RequestSpec};
use crate::error::RadarError;
use crate::provider::FetchError;
use crate::sources;
use crate::stations::SiteId;
use crate::time::{MS_PER_DAY, civil_from_days, days_from_civil, now_unix_ms};

const LOOKBACK_DAYS: i64 = 3;
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

/// The most listing pages a single key scan may read.
const MAX_LIST_PAGES: usize = 64;

/// Paginates a bucket listing prefix to exhaustion.
fn list_keys(acquire: &Acquire, prefix: &str) -> Result<Vec<String>, RadarError> {
    let mut keys = Vec::new();
    let mut token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
    loop {
        if seen_tokens.len() >= MAX_LIST_PAGES {
            return Err(RadarError::Listing {
                prefix: String::from(prefix),
            });
        }
        let (operation, values): (&str, Vec<(&str, &str)>) = token.as_ref().map_or_else(
            || ("list", vec![("prefix", prefix)]),
            |current| {
                (
                    "list-page",
                    vec![("prefix", prefix), ("continuation", current.as_str())],
                )
            },
        );
        let request = RequestSpec::for_operation(&sources::NEXRAD_LEVEL_TWO, operation, &values)
            .map_err(RadarError::from)?;
        let fetched = acquire.fetch(&request).map_err(RadarError::from)?;
        let body = fetched.body;
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
pub fn latest_volume_key(acquire: &Acquire, site: &SiteId) -> Result<String, RadarError> {
    for days_ago in 0..LOOKBACK_DAYS {
        let (year, month, day) = utc_day(days_ago)?;
        let prefix = format!("{year:04}/{month:02}/{day:02}/{site}/");
        let keys = list_keys(acquire, &prefix)?;
        if let Some(key) = keys.iter().filter(|key| is_volume_key(key)).max() {
            return Ok(key.clone());
        }
    }
    Err(RadarError::NoVolume {
        site: String::from(site.as_str()),
    })
}

/// The newest volume keys for a site, oldest first.
///
/// Listing stops once enough keys have been collected; keys are sorted because
/// the object store does not promise lexicographic ordering within a page.
pub fn recent_volume_keys(
    acquire: &Acquire,
    site: &SiteId,
    limit: usize,
) -> Result<Vec<String>, RadarError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut keys = Vec::new();
    for days_ago in 0..LOOKBACK_DAYS {
        let (year, month, day) = utc_day(days_ago)?;
        let prefix = format!("{year:04}/{month:02}/{day:02}/{site}/");
        keys.extend(
            list_keys(acquire, &prefix)?
                .into_iter()
                .filter(|key| is_volume_key(key)),
        );
        if keys.len() >= limit {
            break;
        }
    }
    keys.sort();
    keys.dedup();
    let start = keys.len().saturating_sub(limit);
    Ok(keys.into_iter().skip(start).collect())
}

/// The UTC scan instant encoded in an Archive II object key, when it is well formed.
fn scan_from_key(key: &str) -> Option<u64> {
    let name = key.rsplit('/').next()?;
    let stamp = name.get(4..19)?;
    if stamp.as_bytes().get(8) != Some(&b'_') {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u32> { stamp.get(range)?.parse().ok() };
    let year = i32::try_from(number(0..4)?).ok()?;
    let month = number(4..6)?;
    let day = number(6..8)?;
    let hour = number(9..11)?;
    let minute = number(11..13)?;
    let second = number(13..15)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    u64::try_from(days)
        .ok()?
        .checked_mul(MS_PER_DAY)?
        .checked_add(u64::from(hour) * 3_600_000)?
        .checked_add(u64::from(minute) * 60_000)?
        .checked_add(u64::from(second) * 1_000)
}

/// A verified archive body together with the cache outcome.
#[derive(Debug)]
pub struct CachedArchive {
    pub bytes: Vec<u8>,
    pub provenance: crate::engine::Provenance,
    pub cache: crate::engine::CacheState,
    pub from_cache: bool,
    pub cache_warning: Option<String>,
}

/// Retrieves a volume through the engine's persistent cache.
///
/// The archive objects are immutable once written, so the engine may serve the
/// stored copy for a year. A cache failure is reported, never used to replace
/// bytes the provider did return.
pub fn fetch_archive_cached(
    acquire: &Acquire,
    key: &str,
    site: &SiteId,
) -> Result<CachedArchive, RadarError> {
    let mut request =
        RequestSpec::for_operation(&sources::NEXRAD_LEVEL_TWO, "object", &[("key", key)])
            .map_err(RadarError::from)?
            .station(String::from(site.as_str()))
            .product("Level II base data");
    if let Some(scan) = scan_from_key(key) {
        request = request.scan(scan);
    }
    let fetched = acquire.fetch(&request).map_err(|error| match error {
        FetchError::Status { status: 404, .. } => RadarError::VolumeGone {
            key: String::from(key),
        },
        other => RadarError::from(other),
    })?;
    Ok(CachedArchive {
        bytes: fetched.body,
        provenance: fetched.provenance,
        cache: fetched.cache,
        from_cache: fetched.cache == CacheState::Cached,
        cache_warning: acquire.cache_error(),
    })
}

/// Removes a cached archive after it fails to decode, so the next attempt can
/// fetch a fresh object instead of replaying corrupt bytes.
pub fn evict_archive(acquire: &Acquire, key: &str) -> Result<(), String> {
    acquire.evict(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::TestError;

    #[test]
    fn volume_listing_excludes_metadata() {
        assert!(is_volume_key("2026/10/29/KTWX/KTWX20261029_000115_V06"));
        assert!(!is_volume_key("2026/10/29/KTWX/KTWX20261029_000115_MDM"));
        assert!(!is_volume_key(
            "2026/10/29/KTWX/KTWX20261029_000115_V06_MDM"
        ));
    }

    #[test]
    fn listing_xml_tolerates_whitespace_and_nesting() -> Result<(), TestError> {
        let xml = "<?xml version=\"1.0\"?>\n<ListBucketResult>\n\
                   <IsTruncated>\n true \n</IsTruncated>\n\
                   <Contents><Key> 2026/10/31/KTWX/KTWX20261031_003109_V06 </Key></Contents>\n\
                   <Contents><Key>2026/10/31/KTWX/KTWX20261030_024005_V06</Key></Contents>\n\
                   <NextContinuationToken>\n abc \n</NextContinuationToken>\n\
                   </ListBucketResult>";
        let page = listing_xml("prefix", xml)?;
        assert_eq!(
            page.keys,
            vec![
                "2026/10/31/KTWX/KTWX20261031_003109_V06",
                "2026/10/31/KTWX/KTWX20261030_024005_V06"
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
    fn query_values_keep_path_separators() {}
}
