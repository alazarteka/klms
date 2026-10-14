use super::Config;
use crate::{error::AppError, url::Url};
use serde_json::Value;

#[derive(Debug)]
pub(crate) struct Recording {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) duration_seconds: Option<f64>,
}
pub(crate) struct Delivery {
    pub(crate) recording: Recording,
    pub(crate) official_download_available: bool,
    pub(crate) mp4_available: bool,
    pub(crate) hls_available: bool,
    pub(crate) stream_count: usize,
    pub(crate) composition: &'static str,
    pub(super) groups: Vec<SourceGroup>,
}
impl Delivery {
    pub(crate) fn needs_remux(&self) -> bool {
        !matches!(self.groups.as_slice(), [SourceGroup { mp4: Some(_), offset, .. }] if *offset == 0.0)
    }
}
pub(super) struct SourceGroup {
    pub(super) mp4: Option<Url>,
    pub(super) hls: Option<Url>,
    pub(super) offset: f64,
}
pub(super) fn recording(id: &str, v: &Value) -> Recording {
    Recording {
        id: id.to_owned(),
        title: v
            .get("SessionName")
            .or_else(|| v.get("Name"))
            .and_then(Value::as_str)
            .unwrap_or("Panopto recording")
            .to_owned(),
        duration_seconds: v.get("Duration").and_then(Value::as_f64),
    }
}
pub(super) fn parse_delivery(id: &str, v: &Value, config: &Config) -> Result<Delivery, AppError> {
    let d = v.get("Delivery").unwrap_or(v);
    let streams = d.get("Streams").and_then(Value::as_array).ok_or_else(|| {
        AppError::content_unavailable(
            "Panopto recording metadata omitted streams; access may be restricted or expired",
        )
    })?;
    let mut podcast_groups = d
        .get("PodcastStreams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|s| source_group(s, config))
        .collect::<Vec<_>>();
    podcast_groups.sort_by_key(|g| g.mp4.is_none());
    let composed_available = podcast_groups
        .iter()
        .any(|g| g.mp4.is_some() || g.hls.is_some());
    let groups = if let Some(composed) = podcast_groups
        .into_iter()
        .find(|g| g.mp4.is_some() || g.hls.is_some())
    {
        vec![composed]
    } else {
        streams.iter().map(|s| source_group(s, config)).collect()
    };
    let mp4 = groups.iter().filter(|g| g.mp4.is_some()).count();
    let hls = groups.iter().filter(|g| g.hls.is_some()).count();
    let complete = !groups.is_empty() && groups.iter().all(|g| g.mp4.is_some() || g.hls.is_some());
    let official = v
        .get("DownloadUrl")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    Ok(Delivery {
        recording: recording(id, d),
        official_download_available: official,
        mp4_available: complete && mp4 == groups.len(),
        hls_available: complete && hls > 0,
        stream_count: streams.len(),
        composition: if composed_available {
            "composed_podcast"
        } else if groups.len() > 1 {
            "separate_tracks"
        } else {
            "single_stream"
        },
        groups,
    })
}
fn source_group(s: &Value, config: &Config) -> SourceGroup {
    let mut group = SourceGroup {
        mp4: None,
        hls: None,
        offset: s
            .get("RelativeStart")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0),
    };
    for key in [
        "StreamUrl",
        "StreamHttpUrl",
        "StreamURL",
        "Url",
        "VideoUrl",
        "HttpUrl",
        "HlsUrl",
    ] {
        if let Some(u) = s
            .get(key)
            .and_then(Value::as_str)
            .and_then(|v| Url::parse(v).ok())
            .filter(|u| config.is_cdn(u))
        {
            let path = u.path().to_ascii_lowercase();
            if path.ends_with(".mp4") {
                group.mp4.get_or_insert(u);
            } else if path.ends_with(".m3u8") {
                group.hls.get_or_insert(u);
            }
        }
    }
    group
}
