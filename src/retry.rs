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
//! Transport-neutral retry helpers shared by the engine and radar decoding.

use std::time::Duration;

use crate::error::RadarError;
use crate::time::{civil_from_days, now_unix_ms};

/// Whether a decode failure should wait for a newer volume rather than retry
/// the same bytes.
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
    )
}

/// The wait a `Retry-After` value asks for, in seconds or as an HTTP date.
pub fn retry_after_delay(value: &str) -> Result<Duration, RadarError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_seconds_is_honored_for_throttling() {
        assert!(matches!(
            retry_after_delay("120"),
            Ok(value) if value == Duration::from_secs(120)
        ));
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
}
