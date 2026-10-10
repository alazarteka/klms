//! The single cookie-acceptance predicate shared by capture and storage.
//!
//! Names must be RFC 6265 tokens. Values may hold any printable ASCII byte
//! (`0x20-0x7e`) except `;`, up to 4096 bytes: nothing that could break out of
//! a `Cookie` header, while still accepting everything earlier releases
//! captured or saved. An empty value never passes: a server sends one to
//! delete a cookie, so capture treats it as a deletion and nothing empty is
//! ever stored.

pub const MAX_VALUE_BYTES: usize = 4096;

/// Whether `name` is an RFC 6265 cookie name (an RFC 2616 token).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && !b"()<>@,;:\\\"/[]?={}".contains(&byte))
}

/// The one predicate: a cookie is storable when both parts pass.
pub fn valid_cookie(name: &str, value: &str) -> bool {
    valid_name(name)
        && !value.is_empty()
        && value.len() <= MAX_VALUE_BYTES
        && value
            .bytes()
            .all(|byte| (0x20..0x7f).contains(&byte) && byte != b';')
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
    fn values_accept_everything_older_releases_did_but_never_break_the_header() {
        for good in ["abc123", "a=b", "\"quoted\"", "a b", "a,b", "a\\b", "a\"b"] {
            assert!(valid_cookie("n", good), "{good:?}");
        }
        for bad in ["", "a;b", "a\r\nX: y", "a\tb", "caf\u{e9}"] {
            assert!(!valid_cookie("n", bad), "{bad:?}");
        }
        assert!(valid_cookie("n", &"a".repeat(4096)));
        assert!(!valid_cookie("n", &"a".repeat(4097)));
        assert!(!valid_cookie("n;", "v"));
    }
}
