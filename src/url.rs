//! A deliberately small `http`/`https` URL type for a single-origin client.
//!
//! It follows the WHATWG normalisation the tool relies on (lowercase scheme
//! and host, default ports dropped, `/` for an empty path, dot-segment
//! removal, percent-encoding of unsafe bytes, backslash as slash) but accepts
//! only ASCII hosts and only the `http` and `https` schemes. Anything else is
//! a parse error, which every caller already treats as "not a usable link".
use std::{borrow::Cow, fmt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    serialization: String,
    scheme: String,
    username: String,
    password: String,
    host: String,
    port: Option<u16>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    RelativeUrlWithoutBase,
    UnsupportedScheme,
    EmptyHost,
    InvalidDomainCharacter,
    InvalidPort,
    InvalidIpv6Address,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RelativeUrlWithoutBase => "relative URL without a base",
            Self::UnsupportedScheme => "only http and https URLs are supported",
            Self::EmptyHost => "empty host",
            Self::InvalidDomainCharacter => "invalid domain character",
            Self::InvalidPort => "invalid port number",
            Self::InvalidIpv6Address => "invalid IPv6 address",
        })
    }
}

impl std::error::Error for ParseError {}

/// The scheme, host and port that bound a security decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    pub fn ascii_serialization(&self) -> String {
        let default = default_port(&self.scheme) == Some(self.port);
        if default {
            format!("{}://{}", self.scheme, self.host)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

impl Url {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let cleaned = clean(input);
        let (scheme, rest) = split_scheme(&cleaned).ok_or(ParseError::RelativeUrlWithoutBase)?;
        Self::from_parts(scheme, rest, true)
    }

    /// Resolves a reference (RFC 3986 section 5.2, with WHATWG leniency).
    pub fn join(&self, reference: &str) -> Result<Self, ParseError> {
        let cleaned = clean(reference);
        if let Some((scheme, rest)) = split_scheme(&cleaned) {
            // `https:x` against an https base is a relative reference.
            if scheme == self.scheme {
                return self.join(rest);
            }
            return Self::from_parts(scheme, rest, true);
        }
        let mut leading = cleaned.chars();
        if leading.next().is_some_and(|c| matches!(c, '/' | '\\'))
            && leading.next().is_some_and(|c| matches!(c, '/' | '\\'))
        {
            // Exactly two slashes introduce an authority; more means no host.
            return Self::from_parts(self.scheme.clone(), &cleaned, false);
        }
        let (rest, fragment) = split_once_char(&cleaned, '#');
        let (path_part, query) = split_once_char(rest, '?');
        let mut next = self.clone();
        next.fragment = fragment.map(|value| encode(value, FRAGMENT));
        if path_part.is_empty() {
            if query.is_some() {
                next.query = query.map(|value| encode(value, QUERY));
            }
        } else {
            next.query = query.map(|value| encode(value, QUERY));
            let slashed = path_part.replace('\\', "/");
            let merged = if slashed.starts_with('/') {
                slashed
            } else {
                let keep = self.path.rfind('/').map_or(0, |index| index + 1);
                format!("{}{}", &self.path[..keep], slashed)
            };
            next.path = normalize_path(&encode(&merged, PATH));
        }
        next.reserialize();
        Ok(next)
    }

    fn from_parts(
        scheme: String,
        after_scheme: &str,
        skip_all_slashes: bool,
    ) -> Result<Self, ParseError> {
        if scheme != "http" && scheme != "https" {
            return Err(ParseError::UnsupportedScheme);
        }
        let rest = if skip_all_slashes {
            after_scheme.trim_start_matches(['/', '\\'])
        } else {
            &after_scheme[2..]
        };
        let authority_end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
        let (authority, rest) = rest.split_at(authority_end);
        let (userinfo, host_port) = match authority.rfind('@') {
            Some(at) => (&authority[..at], &authority[at + 1..]),
            None => ("", authority),
        };
        let (username, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
        let (host, port) = parse_host_port(host_port, &scheme)?;
        let (rest, fragment) = split_once_char(rest, '#');
        let (path, query) = split_once_char(rest, '?');
        let mut url = Self {
            serialization: String::new(),
            scheme,
            username: encode(username, USERINFO),
            password: encode(password, USERINFO),
            host,
            port,
            path: normalize_path(&encode(&path.replace('\\', "/"), PATH)),
            query: query.map(|value| encode(value, QUERY)),
            fragment: fragment.map(|value| encode(value, FRAGMENT)),
        };
        url.reserialize();
        Ok(url)
    }

    fn reserialize(&mut self) {
        let mut out = format!("{}://", self.scheme);
        if !self.username.is_empty() || !self.password.is_empty() {
            out.push_str(&self.username);
            if !self.password.is_empty() {
                out.push(':');
                out.push_str(&self.password);
            }
            out.push('@');
        }
        out.push_str(&self.host);
        if let Some(port) = self.port {
            out.push(':');
            out.push_str(&port.to_string());
        }
        out.push_str(&self.path);
        if let Some(query) = &self.query {
            out.push('?');
            out.push_str(query);
        }
        if let Some(fragment) = &self.fragment {
            out.push('#');
            out.push_str(fragment);
        }
        self.serialization = out;
    }

    pub fn as_str(&self) -> &str {
        &self.serialization
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> Option<&str> {
        (!self.password.is_empty()).then_some(self.password.as_str())
    }

    /// The host as serialized: IPv6 addresses keep their brackets.
    pub fn host_str(&self) -> Option<&str> {
        Some(&self.host)
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    pub fn port_or_known_default(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    pub fn origin(&self) -> Origin {
        Origin {
            scheme: self.scheme.clone(),
            host: self.host.clone(),
            port: self.port_or_known_default().unwrap_or(0),
        }
    }

    /// Decoded `application/x-www-form-urlencoded` pairs of the query.
    pub fn query_pairs(&self) -> impl Iterator<Item = (Cow<'_, str>, Cow<'_, str>)> {
        self.query
            .as_deref()
            .unwrap_or("")
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                (Cow::Owned(form_decode(key)), Cow::Owned(form_decode(value)))
            })
    }

    pub fn query_pairs_mut(&mut self) -> QueryPairs<'_> {
        QueryPairs {
            buffer: self.query.clone().unwrap_or_default(),
            url: self,
        }
    }

    pub fn set_path(&mut self, path: &str) {
        let slashed = path.replace('\\', "/");
        let rooted = if slashed.starts_with('/') {
            slashed
        } else {
            format!("/{slashed}")
        };
        self.path = normalize_path(&encode(&rooted, PATH));
        self.reserialize();
    }

    pub fn set_query(&mut self, query: Option<&str>) {
        self.query = query.map(|value| encode(value, QUERY));
        self.reserialize();
    }

    /// The path's `/`-separated segments, without the leading slash.
    pub fn path_segments(&self) -> Option<std::str::Split<'_, char>> {
        Some(self.path.strip_prefix('/')?.split('/'))
    }

    pub fn set_fragment(&mut self, fragment: Option<&str>) {
        self.fragment = fragment.map(|value| encode(value, FRAGMENT));
        self.reserialize();
    }

    pub fn set_username(&mut self, username: &str) {
        self.username = encode(username, USERINFO);
        self.reserialize();
    }

    pub fn set_password(&mut self, password: Option<&str>) {
        self.password = password.map_or_else(String::new, |value| encode(value, USERINFO));
        self.reserialize();
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialization)
    }
}

impl From<Url> for String {
    fn from(url: Url) -> Self {
        url.serialization
    }
}

/// Appends or replaces query pairs; the URL is updated when this is dropped.
pub struct QueryPairs<'a> {
    url: &'a mut Url,
    buffer: String,
}

impl QueryPairs<'_> {
    pub fn append_pair(&mut self, key: &str, value: &str) -> &mut Self {
        if !self.buffer.is_empty() {
            self.buffer.push('&');
        }
        self.buffer.push_str(&form_encode_pair(key, value));
        self
    }

    pub fn extend_pairs<K: AsRef<str>, V: AsRef<str>>(
        &mut self,
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> &mut Self {
        for (key, value) in pairs {
            self.append_pair(key.as_ref(), value.as_ref());
        }
        self
    }

    pub fn clear(&mut self) -> &mut Self {
        self.buffer.clear();
        self
    }
}

impl Drop for QueryPairs<'_> {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        self.url.query = (!buffer.is_empty()).then_some(buffer);
        self.url.reserialize();
    }
}

/// `application/x-www-form-urlencoded` body from key/value pairs.
pub fn form_urlencode<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    pairs
        .into_iter()
        .map(|(key, value)| form_encode_pair(key, value))
        .collect::<Vec<_>>()
        .join("&")
}

fn form_encode_pair(key: &str, value: &str) -> String {
    format!("{}={}", form_encode(key), form_encode(value))
}

fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'*' | b'-' | b'.' | b'_' => out.push(byte as char),
            b' ' => out.push('+'),
            byte if byte.is_ascii_alphanumeric() => out.push(byte as char),
            byte => push_percent(&mut out, byte),
        }
    }
    out
}

fn form_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len()
                && hex(bytes[index + 1]).is_some()
                && hex(bytes[index + 2]).is_some() =>
            {
                out.push(
                    hex(bytes[index + 1]).unwrap_or(0) * 16 + hex(bytes[index + 2]).unwrap_or(0),
                );
                index += 2;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

fn push_percent(out: &mut String, byte: u8) {
    out.push_str(&format!("%{byte:02X}"));
}

type EncodeSet = fn(u8) -> bool;

const FRAGMENT: EncodeSet = |b| matches!(b, b' ' | b'"' | b'<' | b'>' | b'`');
const QUERY: EncodeSet = |b| matches!(b, b' ' | b'"' | b'#' | b'<' | b'>' | b'\'');
const PATH: EncodeSet = |b| FRAGMENT(b) || matches!(b, b'#' | b'?' | b'{' | b'}');
const USERINFO: EncodeSet = |b| {
    PATH(b)
        || matches!(
            b,
            b'/' | b':' | b';' | b'=' | b'@' | b'[' | b'\\' | b']' | b'^' | b'|'
        )
};

/// Percent-encodes controls, non-ASCII bytes and the set's bytes; existing
/// `%` sequences are left alone.
fn encode(input: &str, set: EncodeSet) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if !(0x20..0x7F).contains(&byte) || set(byte) {
            push_percent(&mut out, byte);
        } else {
            out.push(byte as char);
        }
    }
    out
}

/// Removes tab and newline anywhere, and C0 controls or spaces at the ends.
fn clean(input: &str) -> String {
    input
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

fn split_scheme(input: &str) -> Option<(String, &str)> {
    let colon = input.find(':')?;
    let scheme = &input[..colon];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then(|| (scheme.to_ascii_lowercase(), &input[colon + 1..]))
}

fn split_once_char(input: &str, separator: char) -> (&str, Option<&str>) {
    match input.split_once(separator) {
        Some((head, tail)) => (head, Some(tail)),
        None => (input, None),
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

fn parse_host_port(input: &str, scheme: &str) -> Result<(String, Option<u16>), ParseError> {
    let (host, port) = if let Some(rest) = input.strip_prefix('[') {
        let end = rest.find(']').ok_or(ParseError::InvalidIpv6Address)?;
        let address = &rest[..end];
        if address.is_empty()
            || !address.contains(':')
            || !address
                .chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
        {
            return Err(ParseError::InvalidIpv6Address);
        }
        let port = match &rest[end + 1..] {
            "" => None,
            tail => Some(tail.strip_prefix(':').ok_or(ParseError::InvalidPort)?),
        };
        (format!("[{}]", address.to_ascii_lowercase()), port)
    } else {
        let (host, port) = match input.rfind(':') {
            Some(colon) => (&input[..colon], Some(&input[colon + 1..])),
            None => (input, None),
        };
        if host.is_empty() {
            return Err(ParseError::EmptyHost);
        }
        if !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
        {
            return Err(ParseError::InvalidDomainCharacter);
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = match port {
        None | Some("") => None,
        Some(digits) => {
            if !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ParseError::InvalidPort);
            }
            let value: u16 = digits.parse().map_err(|_| ParseError::InvalidPort)?;
            (default_port(scheme) != Some(value)).then_some(value)
        }
    };
    Ok((host, port))
}

/// Resolves `.` and `..` segments (including `%2e` spellings). An empty path
/// becomes `/`.
fn normalize_path(path: &str) -> String {
    let path = path.strip_prefix('/').unwrap_or(path);
    let mut segments: Vec<&str> = Vec::new();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        let lower = part.to_ascii_lowercase();
        let last = parts.peek().is_none();
        match lower.as_str() {
            ".." | ".%2e" | "%2e." | "%2e%2e" => {
                segments.pop();
                if last {
                    segments.push("");
                }
            }
            "." | "%2e" => {
                if last {
                    segments.push("");
                }
            }
            _ => segments.push(part),
        }
    }
    format!("/{}", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> String {
        Url::parse(input).unwrap().to_string()
    }

    fn join(base: &str, reference: &str) -> String {
        Url::parse(base)
            .unwrap()
            .join(reference)
            .unwrap()
            .to_string()
    }

    #[test]
    fn normalizes_scheme_host_port_and_path() {
        assert_eq!(
            parse("HTTPS://KLMS.Kaist.AC.kr:443"),
            "https://klms.kaist.ac.kr/"
        );
        assert_eq!(parse("http://127.0.0.1:80/a"), "http://127.0.0.1/a");
        assert_eq!(parse("http://127.0.0.1:8080"), "http://127.0.0.1:8080/");
        assert_eq!(parse("http://[::1]:9/x"), "http://[::1]:9/x");
        assert_eq!(
            parse("  https://a.test/p q\t/r\n"),
            "https://a.test/p%20q/r"
        );
        assert_eq!(
            parse("https://a.test/a/./b/../c/%2e%2E/d"),
            "https://a.test/a/d"
        );
        assert_eq!(parse("https://a.test\\x\\y"), "https://a.test/x/y");
        assert_eq!(
            parse("https://a.test/%7Efoo/\u{d55c}"),
            "https://a.test/%7Efoo/%ED%95%9C"
        );
        assert_eq!(
            parse("https://a.test/a b?q=a b&r='#f g"),
            "https://a.test/a%20b?q=a%20b&r=%27#f%20g"
        );
    }

    #[test]
    fn rejects_unsupported_or_malformed_urls() {
        for input in [
            "",
            "/relative",
            "mailto:a@b.test",
            "javascript:void(0)",
            "ftp://a.test/",
            "https://",
            "https:///",
            "https://a.test:99999/",
            "https://a.test:x/",
            "https://a b.test/",
            "https://a%2ee.test/",
            "https://\u{d55c}.test/",
            "https://[::1/",
            "https://[nothex]/",
        ] {
            assert!(Url::parse(input).is_err(), "{input:?} should not parse");
        }
    }

    #[test]
    fn userinfo_is_split_at_the_last_at_sign() {
        let url = Url::parse("https://user:p%40ss@evil.test@klms.kaist.ac.kr/x").unwrap();
        assert_eq!(url.host_str(), Some("klms.kaist.ac.kr"));
        assert_eq!(url.username(), "user");
        assert_eq!(url.password(), Some("p%40ss%40evil.test"));
        let spoof = Url::parse("https://klms.kaist.ac.kr@evil.test/").unwrap();
        assert_eq!(spoof.host_str(), Some("evil.test"));
        assert_eq!(spoof.username(), "klms.kaist.ac.kr");
        assert_ne!(
            spoof.origin(),
            Url::parse("https://klms.kaist.ac.kr/").unwrap().origin()
        );
    }

    #[test]
    fn origin_compares_scheme_host_and_effective_port() {
        let base = Url::parse("https://klms.kaist.ac.kr/").unwrap();
        assert_eq!(
            base.origin(),
            Url::parse("https://KLMS.kaist.ac.kr:443/z")
                .unwrap()
                .origin()
        );
        assert_ne!(
            base.origin(),
            Url::parse("http://klms.kaist.ac.kr/").unwrap().origin()
        );
        assert_ne!(
            base.origin(),
            Url::parse("https://klms.kaist.ac.kr:444/")
                .unwrap()
                .origin()
        );
        assert_eq!(
            base.origin().ascii_serialization(),
            "https://klms.kaist.ac.kr"
        );
        assert_eq!(
            Url::parse("http://127.0.0.1:8080/")
                .unwrap()
                .origin()
                .ascii_serialization(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(base.port_or_known_default(), Some(443));
        assert_eq!(base.port(), None);
    }

    #[test]
    fn joins_references_like_rfc_3986() {
        let base = "https://klms.kaist.ac.kr/a/b/c.php?x=1#frag";
        assert_eq!(join(base, "d.php"), "https://klms.kaist.ac.kr/a/b/d.php");
        assert_eq!(
            join(base, "./d.php?y=2"),
            "https://klms.kaist.ac.kr/a/b/d.php?y=2"
        );
        assert_eq!(join(base, "../d.php"), "https://klms.kaist.ac.kr/a/d.php");
        assert_eq!(join(base, "../../../../d"), "https://klms.kaist.ac.kr/d");
        assert_eq!(
            join(base, "/root?z=3#h"),
            "https://klms.kaist.ac.kr/root?z=3#h"
        );
        assert_eq!(
            join(base, "?only=query"),
            "https://klms.kaist.ac.kr/a/b/c.php?only=query"
        );
        assert_eq!(
            join(base, "#new"),
            "https://klms.kaist.ac.kr/a/b/c.php?x=1#new"
        );
        assert_eq!(join(base, ""), "https://klms.kaist.ac.kr/a/b/c.php?x=1");
        assert_eq!(join(base, "//other.test/p"), "https://other.test/p");
        assert_eq!(join(base, "\\\\other.test\\p"), "https://other.test/p");
        assert_eq!(join(base, "http://other.test:80/p"), "http://other.test/p");
        assert_eq!(join(base, "x/.."), "https://klms.kaist.ac.kr/a/b/");
        assert_eq!(join("https://a.test", "b"), "https://a.test/b");
        assert_eq!(join(base, "https:x"), "https://klms.kaist.ac.kr/a/b/x");
        assert_eq!(join(base, "https:/x"), "https://klms.kaist.ac.kr/x");
        assert!(Url::parse(base).unwrap().join("///h.test/p").is_err());
        assert!(
            Url::parse(base)
                .unwrap()
                .join("javascript:alert(1)")
                .is_err()
        );
        assert!(Url::parse(base).unwrap().join("mailto:a@b.test").is_err());
    }

    #[test]
    fn query_pairs_decode_and_append_round_trip() {
        let mut url = Url::parse("https://a.test/p?a=1&b=x%20y+z&flag&=v&&c=%zz").unwrap();
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("a".into(), "1".into()),
                ("b".into(), "x y z".into()),
                ("flag".into(), "".into()),
                ("".into(), "v".into()),
                ("c".into(), "%zz".into()),
            ]
        );
        url.query_pairs_mut()
            .clear()
            .append_pair("k", "a b&c=d/\u{d55c}");
        assert_eq!(url.query(), Some("k=a+b%26c%3Dd%2F%ED%95%9C"));
        let (key, value) = url.query_pairs().next().unwrap();
        assert_eq!((&*key, &*value), ("k", "a b&c=d/\u{d55c}"));
        let mut empty = Url::parse("https://a.test/p?old=1").unwrap();
        empty.query_pairs_mut().clear();
        assert_eq!(empty.as_str(), "https://a.test/p");
        let mut appended = Url::parse("https://a.test/p?old=1").unwrap();
        appended.query_pairs_mut().append_pair("sesskey", "s e");
        assert_eq!(appended.as_str(), "https://a.test/p?old=1&sesskey=s+e");
    }

    #[test]
    fn form_bodies_use_urlencoded_rules() {
        assert_eq!(form_urlencode([]), "");
        assert_eq!(
            form_urlencode([("u", "a b"), ("p", "x&y=z*~\u{d55c}")]),
            "u=a+b&p=x%26y%3Dz*%7E%ED%95%9C"
        );
    }

    #[test]
    fn setters_renormalize() {
        let mut url = Url::parse("https://u:p@a.test/x?q=1#f").unwrap();
        url.set_username("");
        url.set_password(None);
        url.set_fragment(None);
        assert_eq!(url.as_str(), "https://a.test/x?q=1");
        url.set_path("/");
        assert_eq!(url.as_str(), "https://a.test/?q=1");
        url.set_path("a/../b c");
        assert_eq!(url.as_str(), "https://a.test/b%20c?q=1");
    }
}
