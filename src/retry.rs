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
//! Retry scheduling: exponential backoff, jitter, and `Retry-After`.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::time::Duration;

use crate::error::RadarError;
use crate::pipeline::{FetchFailure, FetchUpdate};
use crate::time::{civil_from_days, now_unix_ms};

pub const BASE_DELAY: Duration = Duration::from_secs(2);
pub const MAX_DELAY: Duration = Duration::from_secs(60);

pub fn error_update(
    failure: FetchFailure,
    failures: u8,
    delay: Duration,
    attempt: u32,
    jitter: &RandomState,
) -> FetchUpdate {
    let FetchFailure { operation, error } = failure;
    let wait = if is_decode_failure(&error) && failures >= 2 {
        None
    } else if let Some(value) = retry_after(&error) {
        match retry_after_delay(value) {
            Ok(requested) => Some(requested),
            Err(invalid) => {
                return FetchUpdate {
                    result: Err(FetchFailure {
                        operation,
                        error: invalid,
                    }),
                    retry_in: Some(jittered(delay, attempt, jitter)),
                    waiting_for_next_volume: false,
                };
            }
        }
    } else {
        Some(jittered(delay, attempt, jitter))
    };
    FetchUpdate {
        result: Err(FetchFailure { operation, error }),
        retry_in: wait,
        waiting_for_next_volume: wait.is_none(),
    }
}

pub const fn is_decode_failure(error: &RadarError) -> bool {
    matches!(
        error,
        RadarError::ArchiveVersion { .. }
            | RadarError::ArchiveSite { .. }
            | RadarError::ArchiveTime { .. }
            | RadarError::RecordHeader { .. }
            | RadarError::RecordBounds { .. }
            | RadarError::RecordTooLarge { .. }
            | RadarError::Decompress { .. }
            | RadarError::Decode { .. }
            | RadarError::MessageStructure { .. }
            | RadarError::SiteMismatch { .. }
            | RadarError::SiteCoordinates { .. }
            | RadarError::GateRange { .. }
            | RadarError::IncompleteVolume { .. }
            | RadarError::DecoderPanic { .. }
    )
}

/// The `Retry-After` value of a throttled response, if it carries one.
fn retry_after(error: &RadarError) -> Option<&str> {
    match error {
        RadarError::HttpStatus {
            status: 429 | 503,
            retry_after: Some(value),
            ..
        } => Some(value),
        _ => None,
    }
}

/// The wait a `Retry-After` value asks for, in seconds or as an HTTP date.
fn retry_after_delay(value: &str) -> Result<Duration, RadarError> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Ok(Duration::from_secs(seconds));
    }
    http_date_delay(value)
}

fn http_date_delay(value: &str) -> Result<Duration, RadarError> {
    let timestamp = parse_http_date(value).ok_or_else(|| RadarError::InvalidRetryAfter {
        value: String::from(value),
    })?;
    let now = now_unix_ms().map_err(|source| RadarError::Clock { source })? / 1000;
    Ok(Duration::from_secs(timestamp.saturating_sub(now)))
}

fn parse_http_date(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let weekday = match parts.next()? {
        "Sun," => 0,
        "Mon," => 1,
        "Tue," => 2,
        "Wed," => 3,
        "Thu," => 4,
        "Fri," => 5,
        "Sat," => 6,
        _ => return None,
    };
    let day = parse_decimal(parts.next()?)?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = i64::from(parse_decimal(parts.next()?)?);
    let mut time = parts.next()?.split(':');
    let hour = u64::from(parse_decimal(time.next()?)?);
    let minute = u64::from(parse_decimal(time.next()?)?);
    let second = u64::from(parse_decimal(time.next()?)?);
    if time.next().is_some()
        || parts.next()? != "GMT"
        || parts.next().is_some()
        || !(1970..=9999).contains(&year)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    if civil_from_days(days) != (year as i32, month as u32, day)
        || (days + 4).rem_euclid(7) != weekday
        || days < 0
    {
        return None;
    }
    Some(days as u64 * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn parse_decimal(value: &str) -> Option<u32> {
    if value.is_empty() {
        return None;
    }
    value.bytes().try_fold(0_u32, |number, byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        number.checked_mul(10)?.checked_add(u32::from(byte - b'0'))
    })
}

fn jittered(delay: Duration, attempt: u32, state: &RandomState) -> Duration {
    let millis = state.hash_one(attempt) % 500;
    (delay + Duration::from_millis(millis)).min(MAX_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::VolumeDefect;
    use crate::pipeline::Operation;

    fn incomplete() -> RadarError {
        RadarError::IncompleteVolume {
            site: String::from("KPAH"),
            defect: VolumeDefect::MissingEnd,
        }
    }

    #[test]
    fn backoff_starts_at_two_seconds_and_caps_at_one_minute() {
        let random = RandomState::new();
        assert_eq!(BASE_DELAY, Duration::from_secs(2));
        for attempt in 0..10 {
            assert_eq!(jittered(MAX_DELAY, attempt, &random), MAX_DELAY);
            let short = jittered(BASE_DELAY, attempt, &random);
            assert!(
                (BASE_DELAY..Duration::from_millis(2500)).contains(&short),
                "unexpected jitter {short:?}"
            );
        }
    }

    #[test]
    fn retry_after_seconds_is_honored_for_throttling() {
        let error = RadarError::HttpStatus {
            url: String::from("https://example.invalid"),
            status: 429,
            retry_after: Some(String::from("120")),
        };
        assert_eq!(retry_after(&error), Some("120"));
        assert!(matches!(
            retry_after_delay("120"),
            Ok(value) if value == Duration::from_secs(120)
        ));
        let error = RadarError::HttpStatus {
            url: String::from("https://example.invalid"),
            status: 404,
            retry_after: Some(String::from("120")),
        };
        assert_eq!(retry_after(&error), None, "only 429 and 503 ask for a wait");
    }

    #[test]
    fn retry_after_http_date_is_parsed_and_invalid_dates_are_rejected() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date("Mon, 06 Nov 1994 08:49:37 GMT"), None);
        assert_eq!(parse_http_date("Thu, 31 Feb 2000 00:00:00 GMT"), None);
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37"),
            None,
            "a date without a zone is rejected"
        );
    }

    #[test]
    fn an_invalid_retry_after_falls_back_to_the_backoff_schedule() {
        let state = RandomState::new();
        let error = RadarError::HttpStatus {
            url: String::from("https://example.invalid"),
            status: 503,
            retry_after: Some(String::from("invalid")),
        };
        let update = error_update(
            FetchFailure {
                operation: Operation::Fetching,
                error,
            },
            0,
            BASE_DELAY,
            0,
            &state,
        );
        assert!(matches!(
            update.result,
            Err(FetchFailure {
                error: RadarError::InvalidRetryAfter { ref value },
                ..
            }) if value == "invalid"
        ));
        assert!(update.retry_in.is_some(), "the retry still happens");
        assert!(!update.waiting_for_next_volume);
    }

    #[test]
    fn a_decode_failure_waits_for_the_next_volume_after_one_retry() {
        let state = RandomState::new();
        let first = error_update(
            FetchFailure {
                operation: Operation::Decoding,
                error: incomplete(),
            },
            1,
            BASE_DELAY,
            0,
            &state,
        );
        assert!(first.retry_in.is_some());
        assert!(!first.waiting_for_next_volume);
        let second = error_update(
            FetchFailure {
                operation: Operation::Decoding,
                error: incomplete(),
            },
            2,
            Duration::from_secs(4),
            1,
            &state,
        );
        assert!(second.retry_in.is_none());
        assert!(second.waiting_for_next_volume);
        assert!(is_decode_failure(&incomplete()));
        assert!(!is_decode_failure(&RadarError::VolumeGone {
            key: String::from("2026/10/30/KPAH/KPAH20261030_023529_V06"),
        }));
    }
}
