//! Ephemeral LTI exchange. No Panopto credential is persisted or exposed.
#[cfg(test)]
#[path = "panopto_tests.rs"]
mod tests;
use super::cookies::Jar;
use crate::{
    client::panopto::Session,
    error::AppError,
    http::{self, Follow, Method, Payload, Response},
    parse::video,
    url::{Url, form_urlencode},
};
use std::{io::Read, time::Duration};

pub(crate) struct Policy {
    pub klms: Url,
    pub cookie: String,
    pub jar: Jar,
    pub bearer: Option<String>,
    pub delivery_only: bool,
}
impl http::Policy for Policy {
    fn allow(&mut self, u: &Url) -> Result<(), AppError> {
        crate::client::panopto::allow_exchange(u, &self.klms, self.delivery_only)
    }

    fn headers(&mut self, _: &Method, u: &Url) -> Vec<(&'static str, String)> {
        if u.origin() == self.klms.origin() {
            return vec![("cookie", self.cookie.clone())];
        }
        let mut h = Vec::new();
        if video::panopto_origin(u) {
            if let Some(c) = self.jar.header(u) {
                if self.delivery_only {
                    if let Some(token) = c
                        .split(';')
                        .filter_map(|p| p.trim().split_once('='))
                        .find_map(|(name, value)| (name == "csrfToken").then_some(value))
                    {
                        h.push(("x-csrf-token", token.to_owned()));
                    }
                }
                h.push(("cookie", c))
            }
            if self.delivery_only {
                if let Some(t) = &self.bearer {
                    h.push(("authorization", format!("Bearer {t}")))
                }
            }
        }
        h
    }
    fn inspect(&mut self, r: &Response) -> Result<(), AppError> {
        if matches!(r.status(), 307 | 308) && !self.delivery_only {
            return Err(AppError::content_unavailable(
                "Panopto launch refused a redirect that could repost credentials",
            ));
        }
        if video::panopto_origin(r.url()) {
            self.jar.capture(r.url(), r.headers())?
        }
        Ok(())
    }
}
pub(crate) fn request(
    agent: &ureq::Agent,
    policy: &mut Policy,
    method: Method,
    url: Url,
    payload: Option<Payload>,
) -> Result<Response, AppError> {
    http::Policy::allow(policy, &url)?;
    let r = http::follow(
        agent,
        Follow {
            method,
            url,
            payload,
            max_redirects: 8,
            strict: false,
        },
        policy,
    )
    .map_err(|e| match e {
        http::Failure::Refused(e) => e,
        _ => AppError::network("Panopto request or redirect failed"),
    })?;
    if r.status() != 200 {
        return Err(AppError::content_unavailable(format!(
            "Panopto exchange returned HTTP {}; access may be restricted or expired",
            r.status()
        )));
    }
    Ok(r)
}
pub(crate) fn text(mut r: Response) -> Result<String, AppError> {
    let mut b = Vec::new();
    r.by_ref()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut b)
        .map_err(|_| AppError::network("Panopto response interrupted"))?;
    if b.len() > 8 * 1024 * 1024 {
        return Err(AppError::limit("Panopto response too large"));
    }
    String::from_utf8(b).map_err(|_| AppError::shape("Panopto returned non-text metadata"))
}
pub fn launch(
    klms: &Url,
    cookie: &str,
    timeout: u64,
    html: &str,
    url: &Url,
) -> Result<Session, AppError> {
    let agent = http::agent(Duration::from_secs(timeout), Some(Duration::from_secs(8)));
    let mut policy = Policy {
        klms: klms.clone(),
        cookie: cookie.to_owned(),
        jar: Jar::default(),
        bearer: None,
        delivery_only: false,
    };
    let mut page = html.to_owned();
    let mut current = url.clone();
    for _ in 0..5 {
        if video::panopto_origin(&current)
            && matches!(
                current.path(),
                "/Panopto/Pages/Embed.aspx"
                    | "/Panopto/Pages/Sessions/List.aspx"
                    | "/Panopto/Pages/EmbeddedList.aspx"
            )
        {
            policy.bearer = current.query_value("access_token");
            let recording = current.query_value("id").filter(|id| video::uuid(id));
            let folder = video::folder_id(&current);
            policy.delivery_only = true;
            policy.cookie.clear();
            return Ok(Session::new(agent, policy, recording, folder));
        }
        let response = if let Some(form) = video::launch_form(&page, &current)? {
            let payload = Payload {
                content_type: "application/x-www-form-urlencoded",
                bytes: form_urlencode(form.fields.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .into_bytes(),
            };
            request(
                &agent,
                &mut policy,
                Method::POST,
                form.action,
                Some(payload),
            )?
        } else if let Some(embed) = video::embedded_launch(&page, &current) {
            request(&agent, &mut policy, Method::GET, embed, None)?
        } else {
            if video::non_panopto_lti_form(&page, &current) {
                return Err(AppError::content_unavailable(
                    "activity launches a non-Panopto external tool",
                )
                .with_details(serde_json::json!({"non_video":true})));
            }
            return Err(AppError::content_unavailable(
                "Panopto launch did not yield a recording or collection; access may be restricted",
            ));
        };
        current = response.url().clone();
        page = text(response)?;
    }
    Err(AppError::shape(
        "Panopto launch exceeded its bounded exchange",
    ))
}
