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
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::RadarError;

pub const MS_PER_DAY: u64 = 86_400_000;

pub fn now_unix_ms() -> Result<u64, std::time::SystemTimeError> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    Ok(elapsed.as_millis() as u64)
}

pub const fn civil_from_days(days: i64) -> (i32, u32, u32) {
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

pub fn format_utc(unix_ms: u64) -> Result<String, RadarError> {
    let days =
        i64::try_from(unix_ms / MS_PER_DAY).map_err(|source| RadarError::DateRange { source })?;
    let ms_of_day = unix_ms % MS_PER_DAY;
    let (year, month, day) = civil_from_days(days);
    let hours = ms_of_day / 3_600_000;
    let minutes = (ms_of_day / 60_000) % 60;
    let seconds = (ms_of_day / 1_000) % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02} UTC"
    ))
}

pub fn age_label(now_ms: u64, scan_ms: u64) -> String {
    if scan_ms > now_ms {
        return format!(
            "Scan time is {} s ahead of the system clock",
            (scan_ms - now_ms) / 1_000
        );
    }
    let age_s = (now_ms - scan_ms) / 1_000;
    if age_s < 60 {
        format!("{age_s} s old")
    } else if age_s < 3_600 {
        format!("{} min old", age_s / 60)
    } else {
        format!("{} h old", age_s / 3_600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(unix_ms: u64) -> String {
        match format_utc(unix_ms) {
            Ok(text) => text,
            Err(error) => format!("<{error}>"),
        }
    }

    #[test]
    fn formats_epoch() {
        assert_eq!(formatted(0), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn formats_known_timestamp() {
        assert_eq!(formatted(946_684_800_000), "2000-01-01 00:00:00 UTC");
    }

    #[test]
    fn formats_scan_timestamp() {
        assert_eq!(formatted(1_793_232_075_000), "2026-10-29 00:01:15 UTC");
    }

    #[test]
    fn age_units() {
        assert_eq!(age_label(0, 0), "0 s old");
        assert_eq!(age_label(120_000, 0), "2 min old");
        assert_eq!(age_label(7_200_000, 0), "2 h old");
        assert_eq!(
            age_label(0, 120_000),
            "Scan time is 120 s ahead of the system clock"
        );
    }
}
