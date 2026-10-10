use crate::url::{Url, form_urlencode};

/// The URL without userinfo or fragment and with sensitive query values redacted.
pub fn display(url: &Url) -> String {
    let mut safe = url.clone();
    if safe.query().is_some() {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| {
                let value = if sensitive_key(&key) {
                    "[REDACTED]".into()
                } else {
                    value.into_owned()
                };
                (key.into_owned(), value)
            })
            .collect();
        let query = form_urlencode(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        safe.set_query((!query.is_empty()).then_some(query.as_str()));
    }
    safe.clear_userinfo();
    safe.set_fragment(None);
    safe.into()
}

pub fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "sesskey" | "logintoken" | "moodlesession" | "key" | "sig"
    ) || key.ends_with("token")
        || key.ends_with("signature")
}

#[cfg(test)]
mod tests {
    use super::display;
    use crate::url::Url;

    #[test]
    fn strips_userinfo_and_sensitive_query_values() {
        let url = Url::parse(
            "https://user:pass@klms.kaist.ac.kr/view.php?id=7&sesskey=secret&forcedownload=1#access_token=fragment-secret",
        )
        .unwrap();
        let safe = display(&url);
        for hidden in ["user", "pass", "secret", "#"] {
            assert!(!safe.contains(hidden), "{safe}");
        }
        assert!(safe.contains("id=7") && safe.contains("forcedownload=1"));
    }
}
