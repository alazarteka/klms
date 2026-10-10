//! The single cookie-acceptance predicate shared by capture and storage.
//!
//! Names must be RFC 6265 tokens. Values must be RFC 6265 `cookie-value`s:
//! cookie-octets (`0x21`, `0x23-0x2b`, `0x2d-0x3a`, `0x3c-0x5b`, `0x5d-0x7e`),
//! optionally wrapped in one pair of double quotes, at most 4096 bytes. An
//! empty value never passes: a server sends one to delete a cookie, so capture
//! treats it as a deletion and nothing empty is ever stored.
//!
//! Sessions saved by earlier releases were validated with a looser value rule
//! (any byte `0x21-0x7e` except `;`). [`Rules::Saved`] keeps reading those
//! files; [`Rules::Rfc6265`] is what everything newly captured or written
//! must satisfy. Both tiers forbid every byte that could break out of a
//! `Cookie` header (`;`, whitespace, controls, non-ASCII).

pub const MAX_VALUE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rules {
    /// Strict RFC 6265: required for capturing and for writing sessions.
    Rfc6265,
    /// Also accepts `"`, `,` and `\` inside values, as older releases saved.
    Saved,
}

/// Whether `name` is an RFC 6265 cookie name (an RFC 2616 token).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && !b"()<>@,;:\\\"/[]?={}".contains(&byte))
}

/// Whether a non-empty `value` is acceptable under `rules`.
pub fn valid_value(value: &str, rules: Rules) -> bool {
    if value.is_empty() || value.len() > MAX_VALUE_BYTES {
        return false;
    }
    match rules {
        Rules::Saved => value
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && byte != b';'),
        Rules::Rfc6265 => {
            let inner = value
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .filter(|_| value.len() >= 2)
                .unwrap_or(value);
            inner.bytes().all(cookie_octet)
        }
    }
}

/// The one predicate: a cookie is storable when both parts pass.
pub fn valid_cookie(name: &str, value: &str, rules: Rules) -> bool {
    valid_name(name) && valid_value(value, rules)
}

fn cookie_octet(byte: u8) -> bool {
    matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_tokens() {
        assert!(valid_name("MoodleSession"));
        assert!(valid_name("sso.cookie.device.1"));
        for bad in [
            "",
            "a b",
            "a;b",
            "a=b",
            "a\r\n",
            "na\u{e9}me",
            "a\"b",
            "a,b",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn rfc_values_follow_cookie_octets() {
        for good in ["abc123", "a=b", "\"quoted\"", "x/y+z==", "a:b"] {
            assert!(valid_value(good, Rules::Rfc6265), "{good:?}");
        }
        for bad in [
            "",
            "a b",
            "a;b",
            "a,b",
            "a\\b",
            "a\"b",
            "\"",
            "\"a",
            "a\r",
            "caf\u{e9}",
        ] {
            assert!(!valid_value(bad, Rules::Rfc6265), "{bad:?}");
        }
        assert!(valid_value(&"a".repeat(4096), Rules::Rfc6265));
        assert!(!valid_value(&"a".repeat(4097), Rules::Rfc6265));
    }

    #[test]
    fn saved_tier_is_a_superset_that_still_blocks_header_injection() {
        for value in ["a,b", "a\\b", "a\"b", "plain"] {
            assert!(valid_value(value, Rules::Saved), "{value:?}");
        }
        for bad in ["", "a;b", "a b", "a\r\nX: y", "caf\u{e9}"] {
            assert!(!valid_value(bad, Rules::Saved), "{bad:?}");
        }
        // Everything strict accepts, the saved tier accepts.
        for value in ["abc", "\"q\"", "a=b"] {
            assert!(valid_value(value, Rules::Rfc6265) && valid_value(value, Rules::Saved));
        }
    }

    #[test]
    fn one_predicate_covers_both_parts() {
        assert!(valid_cookie("n", "v", Rules::Rfc6265));
        assert!(!valid_cookie("n", "", Rules::Saved));
        assert!(!valid_cookie("n;", "v", Rules::Saved));
    }
}
