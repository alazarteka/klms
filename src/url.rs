//! Standard URL parsing, with shared origin and query helpers.
use ::url::{Host, form_urlencoded};
pub use ::url::{Origin, Url};

pub fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

/// Loopback hosts never use the environment's proxy.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

/// Plain HTTP is accepted only for local fixture servers.
pub fn is_http_loopback(url: &Url) -> bool {
    url.scheme() == "http" && is_loopback(url)
}

/// The first decoded query value with this name.
pub fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

/// `application/x-www-form-urlencoded` body from key/value pairs.
pub fn form_urlencode<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn repeated_same_scheme_join_is_bounded_and_keeps_the_remaining_path() {
        let base = Url::parse("https://klms.kaist.ac.kr/a/b?old=1").unwrap();
        let reference = format!("{}x", "https:".repeat(100_000));
        let start = Instant::now();
        let joined = base.join(&reference).unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(joined.origin(), base.origin());
        assert_eq!(joined.path(), format!("/a/{}", &reference[6..]));
        assert_eq!(joined.query(), None);
    }

    #[test]
    fn hostile_references_preserve_origin_and_userinfo_boundaries() {
        let base = Url::parse("https://klms.kaist.ac.kr/a/b").unwrap();
        for (reference, same_origin, userinfo) in [
            ("https://evil.test@klms.kaist.ac.kr/p", true, true),
            ("https://klms.kaist.ac.kr@evil.test/p", false, true),
            ("https://user:p@evil.test@klms.kaist.ac.kr/p", true, true),
            ("https://:pass@klms.kaist.ac.kr/p", true, true),
            ("https://@klms.kaist.ac.kr/p", true, false),
            (r"https://klms.kaist.ac.kr\@evil.test/p", true, false),
            (r"\\evil.test\p", false, false),
            ("https://klms.kaist.ac.kr./p", false, false),
            ("https://klms.kaist.ac.kr:444/p", false, false),
            ("https://KLMS.KAIST.AC.KR:443/p", true, false),
            ("\0 \thttps://klms.kaist.ac.kr/p\r\n", true, false),
            ("https://kl\nms.kaist.ac.kr/p", true, false),
            ("https://klms.kaist.ac.kr/p%0d%0aX:evil", true, false),
        ] {
            let url = base.join(reference).unwrap();
            assert_eq!(url.origin() == base.origin(), same_origin, "{reference:?}");
            assert_eq!(has_userinfo(&url), userinfo, "{reference:?}");
        }
        for reference in [
            "https://klms.kaist.ac.kr\0.evil.test/p",
            "https://klms.kaist.ac.kr%2fevil/p",
            "https://[:::]/p",
        ] {
            assert!(base.join(reference).is_err(), "{reference:?}");
        }
    }

    #[test]
    fn queries_and_form_bodies_use_standard_urlencoded_rules() {
        let url = Url::parse("https://a.test/mod/quiz/view.php?id=%37&id=8").unwrap();
        assert_eq!(query_value(&url, "id").as_deref(), Some("7"));
        assert_eq!(query_value(&url, "missing"), None);
        assert_eq!(form_urlencode([]), "");
        assert_eq!(
            form_urlencode([("u", "a b"), ("p", "x&y=z*~한")]),
            "u=a+b&p=x%26y%3Dz*%7E%ED%95%9C"
        );
    }
}
