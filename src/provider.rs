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
//! Provider failures, query encoding and timestamp labels.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use crate::sources::SourceId;

/// A provider request that did not produce a document.
#[derive(Debug)]
pub enum FetchError {
    /// The effective configuration does not enable this source.
    Disabled {
        id: SourceId,
    },
    Request {
        url: String,
        source: ureq::Error,
    },
    Status {
        url: String,
        status: u16,
        retry_after: Option<String>,
    },
    Body {
        url: String,
        source: ureq::Error,
    },
    Json {
        url: String,
        source: serde_json::Error,
    },
    Shape {
        url: String,
        field: &'static str,
    },
    WorkerStopped,
}

impl FetchError {
    /// Whether trying the same request again may succeed.
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Request { .. }
                | Self::Body { .. }
                | Self::Status { status: 429, .. }
                | Self::Status {
                    status: 500..=599,
                    ..
                }
        )
    }

    /// Whether the provider says it holds nothing for this request, which for
    /// an archive product usually means the date is newer than the archive.
    pub const fn is_missing(&self) -> bool {
        matches!(
            self,
            Self::Status {
                status: 400 | 404,
                ..
            }
        )
    }

    /// The server's requested wait after a 429 or 503, when it supplied one.
    pub const fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Status {
                status: 429 | 503,
                retry_after: Some(value),
                ..
            } => Some(value.as_str()),
            _ => None,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Disabled { id } => format!(
                "{} is not enabled in this configuration, so it was not requested.",
                id.as_str()
            ),
            Self::Request { url, source } => {
                format!("The request to {url} failed: {source}")
            }
            Self::Status {
                url,
                status,
                retry_after,
            } => retry_after.as_ref().map_or_else(
                || format!("{url} returned {status}."),
                |value| format!("{url} returned {status} and asked to retry after {value}."),
            ),
            Self::Body { url, source } => {
                format!("The response from {url} could not be read: {source}")
            }
            Self::Json { url, source } => {
                format!("The response from {url} is not valid JSON: {source}")
            }
            Self::Shape { url, field } => {
                format!("The response from {url} has no usable {field}.")
            }
            Self::WorkerStopped => String::from("The provider worker stopped before answering."),
        }
    }
}

impl Display for FetchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for FetchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source, .. } | Self::Body { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Disabled { .. }
            | Self::Status { .. }
            | Self::Shape { .. }
            | Self::WorkerStopped => None,
        }
    }
}

/// Percent-encodes a query value, keeping only unreserved characters.
pub fn encode_query(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
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

/// Formats an ISO 8601 timestamp for display without inventing a zone.
pub fn timestamp_label(text: &str) -> String {
    let Some(head) = text.get(..16) else {
        return String::from(text);
    };
    let formatted = head.replacen('T', " ", 1);
    if text.ends_with('Z') || text.ends_with("+00:00") || text.ends_with("-00:00") {
        return format!("{formatted} UTC");
    }
    let offset = text.get(text.len().saturating_sub(6)..);
    offset
        .filter(|value| {
            let bytes = value.as_bytes();
            bytes.len() == 6
                && matches!(bytes.first(), Some(&b'+') | Some(&b'-'))
                && bytes.get(3) == Some(&b':')
                && bytes
                    .get(1..3)
                    .is_some_and(|hours| hours.iter().all(u8::is_ascii_digit))
                && bytes
                    .get(4..6)
                    .is_some_and(|minutes| minutes.iter().all(u8::is_ascii_digit))
        })
        .map_or_else(
            || format!("{formatted} local"),
            |zone| format!("{formatted} local (UTC{zone})"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode_query("Emporia, KS"), "Emporia%2C%20KS");
        assert_eq!(encode_query("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn iso_timestamps_keep_their_zone_language() {
        assert_eq!(
            timestamp_label("2026-10-30T11:45:00+00:00"),
            "2026-10-30 11:45 UTC"
        );
        assert_eq!(
            timestamp_label("2026-10-30T11:45:00-05:00"),
            "2026-10-30 11:45 local (UTC-05:00)"
        );
        assert_eq!(
            timestamp_label("2026-10-30T11:45:00Z"),
            "2026-10-30 11:45 UTC"
        );
        assert_eq!(timestamp_label("soon"), "soon");
    }

    #[test]
    fn a_disabled_source_is_not_transient() {
        let id = crate::sources::NIFC_WFIGS.id;
        assert!(!FetchError::Disabled { id }.is_transient());
        assert!(!FetchError::Disabled { id }.is_missing());
    }
}
