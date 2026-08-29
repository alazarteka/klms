use url::Url;

pub fn display(url: &Url) -> String {
    let mut safe = url.clone();
    let pairs: Vec<(String, String)> = safe
        .query_pairs()
        .map(|(key, value)| {
            let value = if sensitive_key(&key) {
                "[REDACTED]".to_owned()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    if safe.query().is_some() {
        safe.query_pairs_mut().clear().extend_pairs(pairs);
    }
    let _ = safe.set_username("");
    let _ = safe.set_password(None);
    safe.set_fragment(None);
    safe.into()
}

pub fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "sesskey"
            | "logintoken"
            | "moodlesession"
            | "token"
            | "access_token"
            | "signature"
            | "sig"
            | "key"
    ) || key.ends_with("token")
        || key.ends_with("signature")
}

#[cfg(test)]
mod tests {
    use super::display;
    use url::Url;

    #[test]
    fn strips_userinfo_and_sensitive_query_values() {
        let url = Url::parse(
            "https://user:pass@klms.kaist.ac.kr/view.php?id=7&sesskey=secret&forcedownload=1#access_token=fragment-secret",
        )
        .unwrap();
        let safe = display(&url);
        assert!(!safe.contains("user"));
        assert!(!safe.contains("pass"));
        assert!(!safe.contains("secret"));
        assert!(!safe.contains('#'));
        assert!(safe.contains("id=7"));
        assert!(safe.contains("forcedownload=1"));
    }
}
