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
pub struct ParseError(&'static str);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
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
        if default_port(&self.scheme) == Some(self.port) {
            format!("{}://{}", self.scheme, self.host)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

impl Url {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let cleaned = clean(input);
        let (scheme, rest) =
            split_scheme(&cleaned).ok_or(ParseError("relative URL without a base"))?;
        Self::from_parts(scheme, rest, true)
    }

    /// Resolves a reference (RFC 3986 section 5.2, with WHATWG leniency).
    pub fn join(&self, reference: &str) -> Result<Self, ParseError> {
        let cleaned = clean(reference);
        if let Some((scheme, rest)) = split_scheme(&cleaned) {
            // `https:x` against an https base is a relative reference.
            return if scheme == self.scheme {
                self.join(rest)
            } else {
                Self::from_parts(scheme, rest, true)
            };
        }
        let is_slash = |c: Option<char>| matches!(c, Some('/' | '\\'));
        let mut leading = cleaned.chars();
        if is_slash(leading.next()) && is_slash(leading.next()) {
            // Exactly two slashes introduce an authority; more means no host.
            return Self::from_parts(self.scheme.clone(), &cleaned, false);
        }
        let (rest, fragment) = split_once_char(&cleaned, '#');
        let (path_part, query) = split_once_char(rest, '?');
        let mut next = self.clone();
        next.fragment = fragment.map(|value| encode(value, FRAGMENT));
        if !path_part.is_empty() || query.is_some() {
            next.query = query.map(|value| encode(value, QUERY));
        }
        if !path_part.is_empty() {
            let slashed = path_part.replace('\\', "/");
            let keep = self.path.rfind('/').map_or(0, |index| index + 1);
            let merged = if slashed.starts_with('/') {
                slashed
            } else {
                format!("{}{slashed}", &self.path[..keep])
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
            return Err(ParseError("only http and https URLs are supported"));
        }
        let rest = if skip_all_slashes {
            after_scheme.trim_start_matches(['/', '\\'])
        } else {
            &after_scheme[2..]
        };
        let end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
        let (authority, rest) = rest.split_at(end);
        let (userinfo, host_port) = authority.rsplit_once('@').unwrap_or(("", authority));
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
            out += &self.username;
            if !self.password.is_empty() {
                out += &format!(":{}", self.password);
            }
            out.push('@');
        }
        out += &self.host;
        if let Some(port) = self.port {
            out += &format!(":{port}");
        }
        out += &self.path;
        if let Some(query) = &self.query {
            out += &format!("?{query}");
        }
        if let Some(fragment) = &self.fragment {
            out += &format!("#{fragment}");
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

    /// Plain-HTTP loopback origin, the only non-HTTPS origin ever accepted.
    pub fn is_http_loopback(&self) -> bool {
        self.scheme == "http" && matches!(self.host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
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
        let pairs = self.query.as_deref().unwrap_or("").split('&');
        pairs.filter(|pair| !pair.is_empty()).map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (Cow::Owned(form_decode(key)), Cow::Owned(form_decode(value)))
        })
    }

    pub fn query_pairs_mut(&mut self) -> QueryPairs<'_> {
        QueryPairs(self)
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

    pub fn set_fragment(&mut self, fragment: Option<&str>) {
        self.fragment = fragment.map(|value| encode(value, FRAGMENT));
        self.reserialize();
    }

    pub fn clear_userinfo(&mut self) {
        self.username.clear();
        self.password.clear();
        self.reserialize();
    }

    /// The path's `/`-separated segments, without the leading slash.
    pub fn path_segments(&self) -> Option<std::str::Split<'_, char>> {
        Some(self.path.strip_prefix('/')?.split('/'))
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

/// Appends form-encoded query pairs to the URL.
pub struct QueryPairs<'a>(&'a mut Url);

impl QueryPairs<'_> {
    pub fn append_pair(&mut self, key: &str, value: &str) -> &mut Self {
        let query = self.0.query.get_or_insert_with(String::new);
        if !query.is_empty() {
            query.push('&');
        }
        query.push_str(&form_urlencode([(key, value)]));
        self.0.reserialize();
        self
    }
}

/// `application/x-www-form-urlencoded` body from key/value pairs.
pub fn form_urlencode<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let encode = |value: &str| {
        value.bytes().fold(String::new(), |mut out, byte| {
            match byte {
                b'*' | b'-' | b'.' | b'_' => out.push(byte as char),
                b' ' => out.push('+'),
                _ if byte.is_ascii_alphanumeric() => out.push(byte as char),
                _ => out += &format!("%{byte:02X}"),
            }
            out
        })
    };
    let pairs: Vec<String> = pairs
        .into_iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect();
    pairs.join("&")
}

fn form_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let hex = |index: usize| bytes.get(index).and_then(|&b| (b as char).to_digit(16));
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match (bytes[index], hex(index + 1), hex(index + 2)) {
            (b'+', ..) => out.push(b' '),
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                index += 2;
            }
            (byte, ..) => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// Bytes (besides controls and non-ASCII) each component percent-encodes; the
// sets nest as in the WHATWG URL standard.
const FRAGMENT: &str = " \"<>`";
const QUERY: &str = " \"#<>'";
const PATH: &str = " \"<>`#?{}";
const USERINFO: &str = " \"<>`#?{}/:;=@[\\]^|";

/// Percent-encodes controls, non-ASCII bytes and the set's bytes; existing
/// `%` sequences are left alone.
fn encode(input: &str, set: &str) -> String {
    input.bytes().fold(String::new(), |mut out, byte| {
        if (0x20..0x7F).contains(&byte) && !set.contains(byte as char) {
            out.push(byte as char);
        } else {
            out += &format!("%{byte:02X}");
        }
        out
    })
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
    let (scheme, rest) = input.split_once(':')?;
    let mut chars = scheme.chars();
    (chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then(|| (scheme.to_ascii_lowercase(), rest))
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
    const BAD_PORT: ParseError = ParseError("invalid port number");
    let (host, port) = if let Some(rest) = input.strip_prefix('[') {
        let invalid = ParseError("invalid IPv6 address");
        let (address, tail) = rest.split_once(']').ok_or(invalid.clone())?;
        if !address.contains(':')
            || !address
                .chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
        {
            return Err(invalid);
        }
        let port = match tail {
            "" => None,
            tail => Some(tail.strip_prefix(':').ok_or(BAD_PORT)?),
        };
        (format!("[{}]", address.to_ascii_lowercase()), port)
    } else {
        let (host, port) = match input.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (input, None),
        };
        if host.is_empty() {
            return Err(ParseError("empty host"));
        }
        if !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
        {
            return Err(ParseError("invalid domain character"));
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = match port {
        None | Some("") => None,
        Some(digits) if digits.bytes().all(|b| b.is_ascii_digit()) => {
            let value: u16 = digits.parse().map_err(|_| BAD_PORT)?;
            (default_port(scheme) != Some(value)).then_some(value)
        }
        Some(_) => return Err(BAD_PORT),
    };
    Ok((host, port))
}

/// Resolves `.` and `..` segments (including `%2e` spellings). An empty path
/// becomes `/`.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    let mut parts = path.strip_prefix('/').unwrap_or(path).split('/').peekable();
    while let Some(part) = parts.next() {
        let dots = match part.to_ascii_lowercase().as_str() {
            ".." | ".%2e" | "%2e." | "%2e%2e" => 2,
            "." | "%2e" => 1,
            _ => 0,
        };
        if dots == 2 {
            segments.pop();
        }
        if dots == 0 {
            segments.push(part);
        } else if parts.peek().is_none() {
            segments.push("");
        }
    }
    format!("/{}", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).unwrap()
    }

    fn join(base: &str, reference: &str) -> String {
        url(base).join(reference).unwrap().to_string()
    }

    #[test]
    fn parse_normalizes_scheme_host_port_path_and_encoding() {
        for (input, want) in [
            ("HTTPS://KLMS.Kaist.AC.kr:443", "https://klms.kaist.ac.kr/"),
            ("http://127.0.0.1:80/a", "http://127.0.0.1/a"),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080/"),
            ("http://[::1]:9/x", "http://[::1]:9/x"),
            ("  https://a.test/p q\t/r\n", "https://a.test/p%20q/r"),
            ("https://a.test/a/./b/../c/%2e%2E/d", "https://a.test/a/d"),
            ("https://a.test\\x\\y", "https://a.test/x/y"),
            (
                "https://a.test/%7Efoo/\u{d55c}",
                "https://a.test/%7Efoo/%ED%95%9C",
            ),
            (
                "https://a.test/a b?q=a b&r='#f g",
                "https://a.test/a%20b?q=a%20b&r=%27#f%20g",
            ),
        ] {
            assert_eq!(url(input).to_string(), want, "{input:?}");
        }
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
    fn userinfo_is_split_at_the_last_at_sign_and_origin_compares_effective_port() {
        let parsed = url("https://user:p%40ss@evil.test@klms.kaist.ac.kr/x");
        assert_eq!(parsed.host_str(), Some("klms.kaist.ac.kr"));
        assert_eq!(parsed.username(), "user");
        assert_eq!(parsed.password(), Some("p%40ss%40evil.test"));
        let base = url("https://klms.kaist.ac.kr/");
        let spoof = url("https://klms.kaist.ac.kr@evil.test/");
        assert_eq!(spoof.host_str(), Some("evil.test"));
        assert_eq!(spoof.username(), "klms.kaist.ac.kr");
        assert_ne!(spoof.origin(), base.origin());
        assert_eq!(
            base.origin(),
            url("https://KLMS.kaist.ac.kr:443/z").origin()
        );
        assert_ne!(base.origin(), url("http://klms.kaist.ac.kr/").origin());
        assert_ne!(base.origin(), url("https://klms.kaist.ac.kr:444/").origin());
        assert_eq!(
            base.origin().ascii_serialization(),
            "https://klms.kaist.ac.kr"
        );
        let local = url("http://127.0.0.1:8080/").origin().ascii_serialization();
        assert_eq!(local, "http://127.0.0.1:8080");
        assert_eq!(base.port_or_known_default(), Some(443));
    }

    #[test]
    fn joins_references_like_rfc_3986() {
        let base = "https://klms.kaist.ac.kr/a/b/c.php?x=1#frag";
        for (reference, want) in [
            ("d.php", "https://klms.kaist.ac.kr/a/b/d.php"),
            ("./d.php?y=2", "https://klms.kaist.ac.kr/a/b/d.php?y=2"),
            ("../d.php", "https://klms.kaist.ac.kr/a/d.php"),
            ("../../../../d", "https://klms.kaist.ac.kr/d"),
            ("/root?z=3#h", "https://klms.kaist.ac.kr/root?z=3#h"),
            (
                "?only=query",
                "https://klms.kaist.ac.kr/a/b/c.php?only=query",
            ),
            ("#new", "https://klms.kaist.ac.kr/a/b/c.php?x=1#new"),
            ("", "https://klms.kaist.ac.kr/a/b/c.php?x=1"),
            ("//other.test/p", "https://other.test/p"),
            ("\\\\other.test\\p", "https://other.test/p"),
            ("http://other.test:80/p", "http://other.test/p"),
            ("x/..", "https://klms.kaist.ac.kr/a/b/"),
            ("https:x", "https://klms.kaist.ac.kr/a/b/x"),
            ("https:/x", "https://klms.kaist.ac.kr/x"),
        ] {
            assert_eq!(join(base, reference), want, "{reference:?}");
        }
        assert_eq!(join("https://a.test", "b"), "https://a.test/b");
        for bad in ["///h.test/p", "javascript:alert(1)", "mailto:a@b.test"] {
            assert!(url(base).join(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn query_pairs_and_form_bodies_use_urlencoded_rules() {
        let mut parsed = url("https://a.test/p?a=1&b=x%20y+z&flag&=v&&c=%zz");
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let want = [
            ("a", "1"),
            ("b", "x y z"),
            ("flag", ""),
            ("", "v"),
            ("c", "%zz"),
        ];
        assert_eq!(pairs, want.map(|(k, v)| (k.to_owned(), v.to_owned())));
        let mut fresh = url("https://a.test/p");
        fresh.query_pairs_mut().append_pair("k", "a b&c=d/\u{d55c}");
        assert_eq!(fresh.query(), Some("k=a+b%26c%3Dd%2F%ED%95%9C"));
        let (key, value) = fresh.query_pairs().next().unwrap();
        assert_eq!((&*key, &*value), ("k", "a b&c=d/\u{d55c}"));
        parsed.set_query(Some("old=1"));
        parsed.query_pairs_mut().append_pair("sesskey", "s e");
        assert_eq!(parsed.as_str(), "https://a.test/p?old=1&sesskey=s+e");
        assert_eq!(form_urlencode([]), "");
        assert_eq!(
            form_urlencode([("u", "a b"), ("p", "x&y=z*~\u{d55c}")]),
            "u=a+b&p=x%26y%3Dz*%7E%ED%95%9C"
        );
    }

    #[test]
    fn setters_renormalize() {
        let mut parsed = url("https://u:p@a.test/x?q=1#f");
        parsed.clear_userinfo();
        parsed.set_fragment(None);
        assert_eq!(parsed.as_str(), "https://a.test/x?q=1");
        parsed.set_path("/");
        assert_eq!(parsed.as_str(), "https://a.test/?q=1");
        parsed.set_path("a/../b c");
        assert_eq!(parsed.as_str(), "https://a.test/b%20c?q=1");
        parsed.set_query(None);
        assert_eq!(parsed.as_str(), "https://a.test/b%20c");
    }
}
