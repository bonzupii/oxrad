// oxrad - A storm-chaser-grade NEXRAD weather radar viewer
// Copyright (C) 2026 Micah Ostrow <bonzupii@protonmail.com>
// SPDX-License-Identifier: GPL-3.0-or-later
//! Text encoding shared by URL templates and query builders.

/// Percent-encodes a component, keeping only unreserved characters.
pub(crate) fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unreserved_characters_survive() {
        assert_eq!(encode_component("Emporia, KS"), "Emporia%2C%20KS");
        assert_eq!(encode_component("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_component("a/b"), "a%2Fb");
    }
}
