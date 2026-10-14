use super::{
    Config, Delivery, Download, Session,
    delivery::SourceGroup,
    hls::{ByteRange, fetch_local, local_playlist},
    remux::{remux, require_ffmpeg},
};
use crate::{
    client::stream::{self, Expect, Limits},
    error::AppError,
    http::{self, Follow, Method},
    url::Url,
};
use std::{io::Write, path::Path, time::Duration};

impl Session {
    /// Copies a single unshifted MP4 directly; anything else (HLS, several
    /// tracks, start offsets) is fetched into `scratch`, a private directory
    /// the caller owns, and remuxed there by ffmpeg.
    pub(crate) fn download_to(
        &self,
        d: &Delivery,
        writer: &mut impl Write,
        limits: Limits,
        scratch: &Path,
    ) -> Result<Download, AppError> {
        let config = &self.policy.config;
        let mut fetch = |u: &Url, w: &mut dyn Write, limits: Limits, range: Option<ByteRange>| {
            media_copy(config, u, w, limits, Expect::Media, range)
        };
        if let [
            SourceGroup {
                mp4: Some(source),
                offset,
                ..
            },
        ] = d.groups.as_slice()
        {
            if *offset == 0.0 {
                return media_copy(config, source, writer, limits, Expect::Mp4, None);
            }
        }
        // Each track's source, preferring MP4 (true) over HLS (false).
        let Some(sources) = d
            .groups
            .iter()
            .map(|g| {
                g.mp4
                    .as_ref()
                    .map(|u| (true, u))
                    .or(g.hls.as_ref().map(|u| (false, u)))
            })
            .collect::<Option<Vec<_>>>()
            .filter(|s| !s.is_empty())
        else {
            return Err(AppError::content_unavailable(
                "Panopto lacks a readable source for every recording stream; access may be restricted or expired",
            ));
        };
        require_ffmpeg(&config.ffmpeg, limits)?;
        let mut budget = limits.max_bytes;
        let mut index = 0;
        let mut inputs = Vec::new();
        for (i, (mp4, u)) in sources.into_iter().enumerate() {
            inputs.push(if mp4 {
                let p = scratch.join(format!("stream-{i}.mp4"));
                fetch_local(u, &p, &mut budget, limits, &mut fetch, None)?;
                p
            } else {
                local_playlist(u, scratch, &mut index, &mut budget, limits, 0, &mut fetch)?
            });
        }
        let offsets = d.groups.iter().map(|g| g.offset).collect::<Vec<_>>();
        remux(&config.ffmpeg, &inputs, &offsets, scratch, writer, limits)
    }
}
struct MediaPolicy<'a> {
    stall: Duration,
    config: &'a Config,
    range: Option<String>,
}
impl http::Policy for MediaPolicy<'_> {
    fn response_timeout(&self) -> Option<Duration> {
        Some(self.stall)
    }
    fn headers(&mut self, _: &Method, _: &Url) -> Vec<(&'static str, String)> {
        self.range
            .as_ref()
            .map(|r| vec![("range", r.clone())])
            .unwrap_or_default()
    }
    fn allow(&mut self, u: &Url) -> Result<(), AppError> {
        if self.config.is_cdn(u) {
            Ok(())
        } else {
            Err(AppError::content_unavailable(
                "Panopto media origin refused",
            ))
        }
    }
}
/// One credential-free CDN request, optionally for an exact byte range,
/// streamed through the shared bounded copy.
pub(super) fn media_copy(
    config: &Config,
    source: &Url,
    writer: &mut (impl Write + ?Sized),
    limits: Limits,
    expect: Expect,
    range: Option<ByteRange>,
) -> Result<Download, AppError> {
    let max = limits.max_bytes;
    if !config.is_cdn(source) {
        return Err(AppError::content_unavailable(
            "Panopto media origin refused",
        ));
    }
    let range_header = range
        .map(|(start, len)| {
            if len == 0 || len > max as u64 {
                return Err(AppError::limit("HLS byte range exceeds remaining limit"));
            }
            let end = start
                .checked_add(len - 1)
                .ok_or_else(|| AppError::shape("HLS byte range overflow"))?;
            Ok(format!("bytes={start}-{end}"))
        })
        .transpose()?;
    let agent = http::agent(limits.remaining()?, Some(Duration::from_secs(8)));
    let r = http::follow(
        &agent,
        Follow {
            method: Method::GET,
            url: source.clone(),
            payload: None,
            max_redirects: 3,
            strict: false,
        },
        &mut MediaPolicy {
            stall: limits.stall,
            config,
            range: range_header,
        },
    )
    .map_err(|_| AppError::network("Panopto media request failed"))?;
    if r.status() != if range.is_some() { 206 } else { 200 } {
        return Err(AppError::content_unavailable(format!(
            "Panopto media returned HTTP {}; source may be restricted or expired",
            r.status()
        )));
    }
    if let Some((start, len)) = range {
        let parsed = r
            .header("content-range")
            .and_then(|v| v.strip_prefix("bytes "))
            .and_then(|v| v.split_once('/'))
            .and_then(|(bounds, total)| {
                let (a, b) = bounds.split_once('-')?;
                Some((
                    a.parse::<u64>().ok()?,
                    b.parse::<u64>().ok()?,
                    total.parse::<u64>().ok()?,
                ))
            });
        if parsed.is_none_or(|(a, b, total)| {
            a != start || b.checked_sub(a).and_then(|n| n.checked_add(1)) != Some(len) || b >= total
        }) {
            return Err(AppError::content_unavailable(
                "HLS server returned an unexpected byte range",
            ));
        }
    }
    let bytes = stream::copy(r, writer, limits, expect, &|| match expect {
        Expect::Mp4 => AppError::content_unavailable("Panopto source returned non-MP4 content"),
        Expect::Media => {
            AppError::content_unavailable("Panopto media returned HTML instead of video")
        }
    })?;
    if range.is_some_and(|(_, len)| len != bytes as u64) {
        return Err(AppError::network("Panopto media download incomplete"));
    }
    Ok(Download {
        bytes,
        content_type: "video/mp4",
    })
}
