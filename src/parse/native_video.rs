//! Static native Flowplayer sources. This never executes page scripts.
use scraper::Html;

use super::shared::sel;
use crate::{error::AppError, url::Url};

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    String(String),
    Punct(char),
}

fn tokens(input: &str) -> Result<Vec<Token>, AppError> {
    let mut chars = input.chars().peekable();
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = ' ';
            let mut closed = false;
            for c in chars.by_ref() {
                if previous == '*' && c == '/' {
                    closed = true;
                    break;
                }
                previous = c;
            }
            if !closed {
                return Err(AppError::shape("unterminated native player comment"));
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let mut value = String::new();
            let mut closed = false;
            while let Some(next) = chars.next() {
                if next == c {
                    closed = true;
                    break;
                }
                if next == '\\' {
                    let escaped = chars
                        .next()
                        .ok_or_else(|| AppError::shape("invalid player string"))?;
                    match escaped {
                        '\\' | '\'' | '"' | '/' => value.push(escaped),
                        _ => {
                            return Err(AppError::shape(
                                "unsupported escape in native player source",
                            ));
                        }
                    }
                } else {
                    value.push(next);
                }
            }
            if !closed {
                return Err(AppError::shape("unterminated native player string"));
            }
            out.push(Token::String(value));
        } else if c.is_ascii_alphanumeric() || matches!(c, '_' | '$') {
            let mut word = c.to_string();
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
            {
                word.push(chars.next().expect("peeked"));
            }
            out.push(Token::Word(word));
        } else {
            out.push(Token::Punct(c));
        }
    }
    Ok(out)
}

/// Only literal video/mp4 sources within a Flowplayer clip sources array.
pub fn native_sources(html: &str, page: &Url) -> Result<Vec<Url>, AppError> {
    let document = Html::parse_document(html);
    let mut sources = Vec::new();
    for script in document.select(&sel("script:not([src])")) {
        let text = script.text().collect::<String>();
        let Some(start) = text.find("flowplayer(") else {
            continue;
        };
        // Stop before event handlers/progress scripts following the config.
        let config = text[start..].split("});").next().unwrap_or_default();
        let ts = tokens(config)?;
        let Some(start) = ts.windows(3).position(|t| {
            matches!(&t[0], Token::Word(w) | Token::String(w) if w == "sources")
                && t[1] == Token::Punct(':')
                && t[2] == Token::Punct('[')
        }) else {
            continue;
        };
        let mut depth = 0;
        let mut object = Vec::new();
        for token in &ts[start + 3..] {
            match token {
                Token::Punct(']') if depth == 0 => break,
                Token::Punct('{') => {
                    depth += 1;
                    object.clear();
                }
                Token::Punct('}') => {
                    if depth != 1 {
                        return Err(AppError::shape("unsupported nested native source"));
                    }
                    depth = 0;
                    object.push(token);
                    let property = |key: &str| {
                        object.windows(4).find_map(|t: &[&Token]| {
                            if matches!(t[0], Token::Word(w) | Token::String(w) if w == key)
                                && *t[1] == Token::Punct(':')
                                && matches!(t[3], Token::Punct(',') | Token::Punct('}'))
                            {
                                if let Token::String(value) = t[2] {
                                    return Some(value.as_str());
                                }
                            }
                            None
                        })
                    };
                    if property("type") == Some("video/mp4") {
                        let src = property("src").ok_or_else(|| {
                            AppError::shape("native source is not a static string")
                        })?;
                        let url = page
                            .join(src)
                            .map_err(|_| AppError::shape("invalid native video source"))?;
                        if url.origin() != page.origin()
                            || !url.path().starts_with("/vod1/")
                            || !url.path().ends_with(".mp4")
                            || url.query().is_some()
                            || url.fragment().is_some()
                            || !url.username().is_empty()
                            || url.password().is_some()
                        {
                            return Err(AppError::shape(
                                "native video source is outside the allowed same-origin /vod1/ MP4 path",
                            ));
                        }
                        if !sources.contains(&url) {
                            sources.push(url);
                        }
                    }
                }
                _ if depth > 0 => object.push(token),
                _ => {}
            }
        }
    }
    if sources.is_empty() {
        return Err(AppError::shape(
            "native viewer contained no static Flowplayer MP4 sources",
        ));
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page() -> Url {
        Url::parse("https://klms.kaist.ac.kr/mod/vod/viewer/index.php?id=7").unwrap()
    }
    fn html(src: &str) -> String {
        format!(
            r#"<script>flowplayer('#player', {{ clip: {{ sources: [{{token: 123, type: "video/mp4", /*src: "bad"*/ src: {src}}}] }} }});</script>"#
        )
    }
    #[test]
    fn static_sources_ignore_comments_and_reject_dynamic_or_untrusted_sources() {
        assert_eq!(
            native_sources(&html("\"/vod1/synthetic.mp4\""), &page()).unwrap()[0].path(),
            "/vod1/synthetic.mp4"
        );
        for src in [
            "variable",
            "\"/vod1/a.mp4\" + secret",
            "\"https://evil.example/vod1/a.mp4\"",
            "\"/login/index.php\"",
            "\"/vod1/a.mp4?token=secret\"",
        ] {
            assert!(native_sources(&html(src), &page()).is_err(), "{src}");
        }
        assert!(native_sources("<script>const x='/vod1/a.mp4';</script>", &page()).is_err());
    }
}
