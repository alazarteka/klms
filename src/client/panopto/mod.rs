//! Panopto sessions and metadata; media processing lives in private siblings.
mod delivery;
mod exchange;
mod folder;
mod hls;
mod media;
mod remux;

use crate::{
    error::AppError,
    http::{Method, Payload},
    url::Url,
};
use delivery::parse_delivery;
pub(crate) use delivery::{Delivery, Recording};
pub(crate) use exchange::launch;
use exchange::{Policy, request, text};
use serde_json::Value;

/// The Panopto and media CDN origins the exchange and media policies trust,
/// and the ffmpeg used for remuxing. Production values are fixed; tests point
/// them at loopback fixtures.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    panopto: Url,
    cdn: Url,
    ffmpeg: std::ffi::OsString,
}
impl Config {
    pub(crate) fn production() -> Result<Self, AppError> {
        Self::new(
            "https://kaist.ap.panopto.com/",
            "https://s-cloudfront.cdn.ap.panopto.com/",
        )
    }
    #[cfg(test)]
    pub(crate) fn fixture(panopto: &str, cdn: &str) -> Self {
        Self::new(panopto, cdn).expect("fixture config")
    }
    #[cfg(test)]
    pub(crate) fn with_ffmpeg(self, ffmpeg: impl Into<std::ffi::OsString>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ..self
        }
    }
    fn new(panopto: &str, cdn: &str) -> Result<Self, AppError> {
        let parse =
            |u: &str| Url::parse(u).map_err(|_| AppError::internal("invalid Panopto origin"));
        Ok(Self {
            panopto: parse(panopto)?,
            cdn: parse(cdn)?,
            ffmpeg: "ffmpeg".into(),
        })
    }
    fn is_panopto(&self, u: &Url) -> bool {
        same_origin(u, &self.panopto)
    }
    fn is_cdn(&self, u: &Url) -> bool {
        same_origin(u, &self.cdn)
    }
    fn endpoint(&self, path: &str) -> Result<Url, AppError> {
        self.panopto
            .join(path)
            .map_err(|_| AppError::internal("invalid Panopto endpoint"))
    }
}
fn same_origin(u: &Url, origin: &Url) -> bool {
    u.origin() == origin.origin() && !crate::url::has_userinfo(u)
}

pub(crate) struct Session {
    agent: ureq::Agent,
    policy: Policy,
    pub(crate) recording_id: Option<String>,
    folder: Option<String>,
}
pub(crate) struct Recordings {
    pub(crate) rows: Vec<Recording>,
    pub(crate) complete: bool,
}
pub(crate) struct Download {
    pub(crate) bytes: usize,
    pub(crate) content_type: &'static str,
}
impl Session {
    fn new(
        agent: ureq::Agent,
        policy: Policy,
        recording_id: Option<String>,
        folder: Option<String>,
    ) -> Self {
        Self {
            agent,
            policy,
            recording_id,
            folder,
        }
    }
    pub(crate) fn delivery(&mut self, id: Option<&str>) -> Result<Delivery, AppError> {
        let id = id
            .map(str::to_owned)
            .or_else(|| self.recording_id.clone())
            .ok_or_else(|| {
                AppError::usage(
                    "this Panopto activity is a collection; choose a recording reference",
                )
            })?;
        if !crate::reference::valid_uuid(&id) {
            return Err(AppError::usage("invalid Panopto recording UUID"));
        }
        let pairs = [
            ("deliveryId", id.as_str()),
            ("isEmbed", "true"),
            ("responseType", "json"),
            ("isLiveNotes", "false"),
            ("refreshAuthCookie", "false"),
            ("isActiveBroadcast", "false"),
            ("isEditing", "false"),
            ("isKollectiveAgentInstalled", "false"),
        ];
        let p = Payload::form(pairs);
        let endpoint = self
            .policy
            .config
            .endpoint("/Panopto/Pages/Viewer/DeliveryInfo.aspx")?;
        let r = request(
            &self.agent,
            &mut self.policy,
            Method::POST,
            endpoint,
            Some(p),
        )?;
        let value: Value = serde_json::from_str(&text(r)?)
            .map_err(|_| AppError::shape("Panopto delivery metadata was not JSON"))?;
        parse_delivery(&id, &value, &self.policy.config)
    }
    pub(crate) fn recordings(&mut self) -> Result<Recordings, AppError> {
        if let Some(id) = self.recording_id.clone() {
            return Ok(Recordings {
                rows: vec![self.delivery(Some(&id))?.recording],
                complete: true,
            });
        }
        let folder = self
            .folder
            .clone()
            .ok_or_else(|| AppError::shape("Panopto collection omitted folder identifier"))?;
        if !crate::reference::valid_uuid(&folder) {
            return Err(AppError::shape("Panopto folder identifier was invalid"));
        }
        folder::collect(|page| {
            let body = folder::FolderRequest {
                query_parameters: folder::FolderParameters {
                    folder_id: &folder,
                    max_results: 50,
                    page,
                },
            };
            let payload = serde_json::to_vec(&body)
                .map_err(|_| AppError::internal("could not encode folder query"))?;
            let endpoint = self
                .policy
                .config
                .endpoint("/Panopto/Services/Data.svc/GetSessions")?;
            let r = request(
                &self.agent,
                &mut self.policy,
                Method::POST,
                endpoint,
                Some(Payload {
                    content_type: "application/json; charset=utf-8",
                    bytes: payload,
                }),
            )?;
            serde_json::from_str(&text(r)?)
                .map_err(|_| AppError::shape("Panopto folder response was not JSON"))
        })
    }
}
#[cfg(test)]
mod tests;
