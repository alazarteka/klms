//! Panopto metadata and credential-free CDN media transport.
#[path = "panopto_folder.rs"]
mod folder;
use crate::{
    auth::panopto::{Policy, request, text},
    error::AppError,
    http::{self, Follow, Method, Payload},
    parse::video,
    url::{Url, form_urlencode},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

pub struct Session {
    agent: ureq::Agent,
    policy: Policy,
    pub recording_id: Option<String>,
    folder: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Recording {
    pub id: String,
    pub title: String,
    pub duration_seconds: Option<f64>,
}
#[derive(Serialize)]
pub struct Delivery {
    pub recording: Recording,
    pub official_download_available: bool,
    pub mp4_available: bool,
    pub hls_available: bool,
    pub stream_count: usize,
    pub composition: &'static str,
    pub output_stream_count: usize,
    #[serde(skip)]
    mp4: Vec<Url>,
    #[serde(skip)]
    hls: Vec<Url>,
    #[serde(skip)]
    groups: Vec<SourceGroup>,
}
struct SourceGroup {
    mp4: Option<Url>,
    hls: Option<Url>,
    offset: f64,
}
pub struct Recordings {
    pub rows: Vec<Recording>,
    pub complete: bool,
}
pub struct Download {
    pub bytes: usize,
    pub content_type: &'static str,
}
impl Session {
    pub(crate) fn new(
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
    pub fn delivery(&mut self, id: Option<&str>) -> Result<Delivery, AppError> {
        let id = id
            .map(str::to_owned)
            .or_else(|| self.recording_id.clone())
            .ok_or_else(|| {
                AppError::usage(
                    "this Panopto activity is a collection; choose a recording reference",
                )
            })?;
        if !video::uuid(&id) {
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
        let p = Payload {
            content_type: "application/x-www-form-urlencoded",
            bytes: form_urlencode(pairs).into_bytes(),
        };
        let r = request(
            &self.agent,
            &mut self.policy,
            Method::POST,
            Url::parse("https://kaist.ap.panopto.com/Panopto/Pages/Viewer/DeliveryInfo.aspx")
                .unwrap(),
            Some(p),
        )?;
        let value: Value = serde_json::from_str(&text(r)?)
            .map_err(|_| AppError::shape("Panopto delivery metadata was not JSON"))?;
        parse_delivery(&id, &value)
    }
    pub fn recordings(&mut self) -> Result<Recordings, AppError> {
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
        if !video::uuid(&folder) {
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
            let r = request(
                &self.agent,
                &mut self.policy,
                Method::POST,
                Url::parse("https://kaist.ap.panopto.com/Panopto/Services/Data.svc/GetSessions")
                    .expect("built-in endpoint"),
                Some(Payload {
                    content_type: "application/json; charset=utf-8",
                    bytes: payload,
                }),
            )?;
            serde_json::from_str(&text(r)?)
                .map_err(|_| AppError::shape("Panopto folder response was not JSON"))
        })
    }
    pub fn download_to(
        &self,
        d: &Delivery,
        writer: &mut impl Write,
        max_bytes: usize,
        max_seconds: u64,
    ) -> Result<Download, AppError> {
        if d.mp4.len() == 1 && d.groups.len() == 1 && d.groups[0].offset == 0.0 {
            return media_to(&d.mp4[0], writer, max_bytes, max_seconds);
        }
        if d.groups.is_empty() || d.groups.iter().any(|g| g.mp4.is_none() && g.hls.is_none()) {
            return Err(AppError::content_unavailable(
                "Panopto lacks a readable source for every recording stream; access may be restricted or expired",
            ));
        }
        if d.groups.iter().all(|g| g.mp4.is_none()) && d.groups.iter().all(|g| g.offset == 0.0) {
            return hls_to(&d.hls, writer, max_bytes, max_seconds);
        }
        let scratch = Scratch::new()?;
        let deadline = Instant::now() + Duration::from_secs(max_seconds);
        let mut budget = max_bytes;
        let mut index = 0;
        let mut inputs = Vec::new();
        for (i, g) in d.groups.iter().enumerate() {
            let p = if let Some(u) = &g.mp4 {
                let p = scratch.0.join(format!("stream-{i}.mp4"));
                fetch_local(u, &p, &mut budget, deadline)?;
                p
            } else {
                local_playlist(
                    g.hls.as_ref().unwrap(),
                    &scratch,
                    &mut index,
                    &mut budget,
                    deadline,
                    0,
                )?
            };
            inputs.push(p);
        }
        remux_with_offsets(
            &inputs,
            &d.groups.iter().map(|g| g.offset).collect::<Vec<_>>(),
            &scratch,
            writer,
            max_bytes,
            deadline,
        )
    }
}
fn recording(id: &str, v: &Value) -> Recording {
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
fn parse_delivery(id: &str, v: &Value) -> Result<Delivery, AppError> {
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
        .map(source_group)
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
        streams.iter().map(source_group).collect()
    };
    let mp4 = groups
        .iter()
        .filter_map(|g| g.mp4.clone())
        .collect::<Vec<_>>();
    let hls = groups
        .iter()
        .filter_map(|g| g.hls.clone())
        .collect::<Vec<_>>();
    let complete = !groups.is_empty() && groups.iter().all(|g| g.mp4.is_some() || g.hls.is_some());
    let official = v
        .get("DownloadUrl")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    Ok(Delivery {
        recording: recording(id, d),
        official_download_available: official,
        mp4_available: complete && mp4.len() == groups.len(),
        hls_available: complete && !hls.is_empty(),
        stream_count: streams.len(),
        composition: if composed_available {
            "composed_podcast"
        } else if groups.len() > 1 {
            "separate_tracks"
        } else {
            "single_stream"
        },
        output_stream_count: groups.len(),
        mp4,
        hls,
        groups,
    })
}
fn source_group(s: &Value) -> SourceGroup {
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
            .filter(allowed_media)
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
fn allowed_media(u: &Url) -> bool {
    u.scheme() == "https"
        && u.host_str() == Some("s-cloudfront.cdn.ap.panopto.com")
        && u.port_or_known_default() == Some(443)
        && u.username().is_empty()
        && u.password().is_none()
}
struct MediaPolicy {
    range: Option<String>,
}
impl http::Policy for MediaPolicy {
    fn headers(&mut self, _: &Method, _: &Url) -> Vec<(&'static str, String)> {
        self.range
            .as_ref()
            .map(|r| vec![("range", r.clone())])
            .unwrap_or_default()
    }
    fn allow(&mut self, u: &Url) -> Result<(), AppError> {
        if allowed_media(u) {
            Ok(())
        } else {
            Err(AppError::content_unavailable(
                "Panopto media origin refused",
            ))
        }
    }
}
fn media_to(
    source: &Url,
    writer: &mut impl Write,
    max: usize,
    seconds: u64,
) -> Result<Download, AppError> {
    media_copy(source, writer, max, seconds, true)
}
type ByteRange = (u64, u64);
fn media_copy(
    source: &Url,
    writer: &mut (impl Write + ?Sized),
    max: usize,
    seconds: u64,
    verify_mp4: bool,
) -> Result<Download, AppError> {
    media_copy_range(source, writer, max, seconds, verify_mp4, None)
}
fn media_copy_range(
    source: &Url,
    writer: &mut (impl Write + ?Sized),
    max: usize,
    seconds: u64,
    verify_mp4: bool,
    range: Option<ByteRange>,
) -> Result<Download, AppError> {
    if !allowed_media(source) {
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
    let agent = http::agent(Duration::from_secs(seconds), Some(Duration::from_secs(8)));
    let mut r = http::follow(
        &agent,
        Follow {
            method: Method::GET,
            url: source.clone(),
            payload: None,
            max_redirects: 3,
            strict: false,
        },
        &mut MediaPolicy {
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
    let expected = r.content_length();
    if expected.is_some_and(|n| n > max as u64) {
        return Err(AppError::limit("video exceeds byte limit"));
    }
    let mut bytes = 0;
    if verify_mp4 {
        let mut prefix = Vec::new();
        r.by_ref()
            .take(12)
            .read_to_end(&mut prefix)
            .map_err(|_| AppError::network("Panopto MP4 header interrupted"))?;
        if prefix.len() < 12 || &prefix[4..8] != b"ftyp" {
            return Err(AppError::content_unavailable(
                "Panopto source returned non-MP4 content",
            ));
        }
        if prefix.len() > max {
            return Err(AppError::limit("video exceeds byte limit"));
        }
        writer
            .write_all(&prefix)
            .map_err(|_| AppError::config("failed to write video"))?;
        bytes = prefix.len();
    }
    let mut buf = [0u8; 65536];
    loop {
        let n = r
            .read(&mut buf)
            .map_err(|_| AppError::network("Panopto media download interrupted"))?;
        if n == 0 {
            break;
        }
        if bytes == 0 {
            let p = String::from_utf8_lossy(&buf[..n.min(512)]).to_ascii_lowercase();
            if p.contains("<html")
                || p.contains("<!doctype")
                || r.header("content-type").is_some_and(|t| t.contains("html"))
            {
                return Err(AppError::content_unavailable(
                    "Panopto media returned HTML instead of video",
                ));
            }
        }
        bytes += n;
        if bytes > max {
            return Err(AppError::limit("video exceeds byte limit"));
        }
        writer
            .write_all(&buf[..n])
            .map_err(|_| AppError::config("failed to write video"))?
    }
    if bytes == 0
        || expected.is_some_and(|n| n != bytes as u64)
        || range.is_some_and(|(_, len)| len != bytes as u64)
    {
        return Err(AppError::network("Panopto media download incomplete"));
    }
    Ok(Download {
        bytes,
        content_type: "video/mp4",
    })
}
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl Scratch {
    fn new() -> Result<Self, AppError> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| AppError::config("system clock precedes epoch"))?
            .as_nanos();
        let p = std::env::temp_dir().join(format!("klms-video-{}-{stamp}", std::process::id()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&p)
            .map_err(|_| AppError::config("could not create private video workspace"))?;
        Ok(Self(p))
    }
}
fn remaining(deadline: Instant) -> Result<u64, AppError> {
    deadline
        .checked_duration_since(Instant::now())
        .map(|d| d.as_secs().max(1))
        .ok_or_else(|| AppError::limit("video time limit exceeded"))
}
fn fetch_local(
    url: &Url,
    path: &std::path::Path,
    budget: &mut usize,
    deadline: Instant,
) -> Result<usize, AppError> {
    let mut f = crate::private_fs::private_file_options()
        .create_new(true)
        .open(path)
        .map_err(|_| AppError::config("could not create local media fragment"))?;
    let d = media_copy(url, &mut f, *budget, remaining(deadline)?, false)?;
    *budget -= d.bytes;
    Ok(d.bytes)
}
fn quoted_attr<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_once(&format!("{key}=\""))?
        .1
        .split_once('"')
        .map(|(v, _)| v)
}
fn write_private(path: &std::path::Path, text: &str) -> Result<(), AppError> {
    let mut f = crate::private_fs::private_file_options()
        .create_new(true)
        .open(path)
        .map_err(|_| AppError::config("could not create local playlist"))?;
    f.write_all(text.as_bytes())
        .map_err(|_| AppError::config("could not write local playlist"))
}
fn parse_byte_range(raw: &str) -> Result<(u64, Option<u64>), AppError> {
    let (len, start) = raw
        .split_once('@')
        .map_or((raw, None), |(len, start)| (len, Some(start)));
    let len = len
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| AppError::shape("invalid HLS byte range length"))?;
    let start = start
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| AppError::shape("invalid HLS byte range offset"))
        })
        .transpose()?;
    Ok((len, start))
}
type Fetch<'a> = dyn FnMut(&Url, &mut dyn Write, usize, u64, Option<ByteRange>) -> Result<Download, AppError>
    + 'a;
fn local_playlist(
    url: &Url,
    scratch: &Scratch,
    index: &mut usize,
    budget: &mut usize,
    deadline: Instant,
    depth: usize,
) -> Result<std::path::PathBuf, AppError> {
    local_playlist_with(
        url,
        scratch,
        index,
        budget,
        deadline,
        depth,
        &mut |u, w, max, seconds, range| media_copy_range(u, w, max, seconds, false, range),
    )
}
fn fetch_local_with(
    url: &Url,
    path: &std::path::Path,
    budget: &mut usize,
    deadline: Instant,
    fetch: &mut Fetch<'_>,
    range: Option<ByteRange>,
) -> Result<(), AppError> {
    let mut f = crate::private_fs::private_file_options()
        .create_new(true)
        .open(path)
        .map_err(|_| AppError::config("could not create local fragment"))?;
    let d = fetch(url, &mut f, *budget, remaining(deadline)?, range)?;
    *budget = budget
        .checked_sub(d.bytes)
        .ok_or_else(|| AppError::limit("video bytes exceeded budget"))?;
    Ok(())
}
fn local_playlist_with(
    url: &Url,
    scratch: &Scratch,
    index: &mut usize,
    budget: &mut usize,
    deadline: Instant,
    depth: usize,
    fetch: &mut Fetch<'_>,
) -> Result<std::path::PathBuf, AppError> {
    if depth > 3 {
        return Err(AppError::limit("HLS playlist nesting exceeds limit"));
    }
    let mut raw = Vec::new();
    let cap = (*budget).min(1024 * 1024);
    let downloaded = fetch(url, &mut raw, cap, remaining(deadline)?, None)?;
    *budget -= downloaded.bytes;
    let data = String::from_utf8(raw).map_err(|_| AppError::shape("HLS playlist is not UTF-8"))?;
    if data.len() > 1024 * 1024 || !data.starts_with("#EXTM3U") {
        return Err(AppError::shape("invalid or oversized HLS playlist"));
    }
    let lines: Vec<_> = data
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.iter().any(|l| l.starts_with("#EXT-X-STREAM-INF:")) {
        let mut variants = Vec::new();
        for pair in lines.windows(2) {
            if pair[0].starts_with("#EXT-X-STREAM-INF:") && !pair[1].starts_with('#') {
                let bandwidth = pair[0]
                    .split(',')
                    .find_map(|s| {
                        s.split_once("BANDWIDTH=")
                            .and_then(|(_, v)| v.parse::<u64>().ok())
                    })
                    .unwrap_or(0);
                variants.push((bandwidth, pair[0], pair[1]));
            }
        }
        let (_, info, variant) = variants
            .into_iter()
            .max_by_key(|(b, _, _)| *b)
            .ok_or_else(|| AppError::shape("HLS master omitted variants"))?;
        let child = url
            .join(variant)
            .map_err(|_| AppError::shape("invalid HLS variant URL"))?;
        let video =
            local_playlist_with(&child, scratch, index, budget, deadline, depth + 1, fetch)?;
        let Some(group) = quoted_attr(info, "AUDIO") else {
            return Ok(video);
        };
        let audio_line = lines
            .iter()
            .filter(|l| {
                l.starts_with("#EXT-X-MEDIA:")
                    && l.contains("TYPE=AUDIO")
                    && quoted_attr(l, "GROUP-ID") == Some(group)
                    && quoted_attr(l, "URI").is_some()
            })
            .max_by_key(|l| l.contains("DEFAULT=YES"));
        let Some(audio_line) = audio_line else {
            return Ok(video);
        };
        let audio_url = url
            .join(quoted_attr(audio_line, "URI").unwrap())
            .map_err(|_| AppError::shape("invalid HLS audio URL"))?;
        let audio = local_playlist_with(
            &audio_url,
            scratch,
            index,
            budget,
            deadline,
            depth + 1,
            fetch,
        )?;
        let master = scratch.0.join(format!("playlist-{}.m3u8", *index));
        *index += 1;
        let audio_name = audio.file_name().unwrap().to_string_lossy();
        let video_name = video.file_name().unwrap().to_string_lossy();
        write_private(
            &master,
            &format!(
                "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"audio\",DEFAULT=YES,AUTOSELECT=YES,URI=\"{audio_name}\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO=\"audio\"\n{video_name}\n"
            ),
        )?;
        return Ok(master);
    }
    if !lines.contains(&"#EXT-X-ENDLIST") {
        return Err(AppError::content_unavailable(
            "HLS stream is live or incomplete; finite recording required",
        ));
    }
    let out = scratch.0.join(format!("playlist-{}.m3u8", *index));
    *index += 1;
    let mut local = String::new();
    let mut fragments = 0;
    let mut pending_range = None;
    let mut previous_range: Option<(Url, u64)> = None;
    for line in lines {
        if line.starts_with("#EXT-X-KEY:") && !line.contains("METHOD=NONE") {
            return Err(AppError::content_unavailable(
                "encrypted HLS recording is unsupported",
            ));
        }
        if let Some(raw) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(parse_byte_range(raw)?);
            continue;
        }
        if line.starts_with("#EXT-X-MAP:") {
            let remote = quoted_attr(line, "URI")
                .ok_or_else(|| AppError::shape("HLS initialization omitted URI"))?;
            let range = quoted_attr(line, "BYTERANGE")
                .map(parse_byte_range)
                .transpose()?
                .map(|(len, start)| {
                    start.map(|start| (start, len)).ok_or_else(|| {
                        AppError::shape("HLS initialization byte range needs an explicit offset")
                    })
                })
                .transpose()?;
            let u = url
                .join(remote)
                .map_err(|_| AppError::shape("invalid HLS initialization URL"))?;
            let name = format!("fragment-{}.mp4", *index);
            *index += 1;
            fetch_local_with(&u, &scratch.0.join(&name), budget, deadline, fetch, range)?;
            local.push_str(&format!("#EXT-X-MAP:URI=\"{name}\"\n"));
        } else if line.starts_with('#') {
            if line.contains("URI=") {
                return Err(AppError::content_unavailable(
                    "HLS playlist contains an unsupported external resource",
                ));
            }
            local.push_str(line);
            local.push('\n');
        } else {
            fragments += 1;
            if fragments > 10000 {
                return Err(AppError::limit("HLS segment count exceeds limit"));
            }
            let u = url
                .join(line)
                .map_err(|_| AppError::shape("invalid HLS segment URL"))?;
            let name = format!("fragment-{}.ts", *index);
            *index += 1;
            let range = pending_range
                .take()
                .map(|(len, start)| {
                    let start = start
                        .or_else(|| {
                            previous_range
                                .as_ref()
                                .filter(|(prior, _)| prior == &u)
                                .map(|(_, end)| *end)
                        })
                        .ok_or_else(|| {
                            AppError::shape(
                                "implicit HLS byte range has no previous range for this resource",
                            )
                        })?;
                    let end = start
                        .checked_add(len)
                        .ok_or_else(|| AppError::shape("HLS byte range overflow"))?;
                    previous_range = Some((u.clone(), end));
                    Ok::<ByteRange, AppError>((start, len))
                })
                .transpose()?;
            if range.is_none() {
                previous_range = None;
            }
            fetch_local_with(&u, &scratch.0.join(&name), budget, deadline, fetch, range)?;
            local.push_str(&name);
            local.push('\n');
        }
    }
    if fragments == 0 {
        return Err(AppError::shape("HLS playlist contained no segments"));
    }
    write_private(&out, &local)?;
    Ok(out)
}
fn remux(
    inputs: &[std::path::PathBuf],
    scratch: &Scratch,
    writer: &mut impl Write,
    max: usize,
    deadline: Instant,
) -> Result<Download, AppError> {
    remux_with_offsets(
        inputs,
        &vec![0.0; inputs.len()],
        scratch,
        writer,
        max,
        deadline,
    )
}
fn remux_with_offsets(
    inputs: &[std::path::PathBuf],
    offsets: &[f64],
    scratch: &Scratch,
    writer: &mut impl Write,
    max: usize,
    deadline: Instant,
) -> Result<Download, AppError> {
    use std::process::{Command, Stdio};
    let output = scratch.0.join("remux.mp4");
    let mut c = Command::new("ffmpeg");
    c.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    for (p, offset) in inputs.iter().zip(offsets) {
        if *offset != 0.0 {
            c.arg("-itsoffset").arg(offset.to_string());
        }
        c.args(["-protocol_whitelist", "file", "-i"]).arg(p);
    }
    for i in 0..inputs.len() {
        c.arg("-map")
            .arg(format!("{i}:v?"))
            .arg("-map")
            .arg(format!("{i}:a?"));
    }
    c.args(["-c", "copy", "-movflags", "+faststart", "-n"])
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::config(
                "ffmpeg is required for HLS or multiple streams; install ffmpeg and retry",
            )
        } else {
            AppError::config("could not start local ffmpeg remux")
        }
    })?;
    loop {
        if Instant::now() >= deadline
            || std::fs::metadata(&output).is_ok_and(|m| m.len() > max as u64)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::limit("video remux exceeds time or byte limit"));
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|_| AppError::config("could not monitor ffmpeg"))?
        {
            if !status.success() {
                return Err(AppError::content_unavailable(
                    "ffmpeg could not remux downloaded video fragments",
                ));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let size = std::fs::metadata(&output)
        .map_err(|_| AppError::config("ffmpeg produced no output"))?
        .len();
    if size == 0 || size > max as u64 {
        return Err(AppError::limit(
            "remuxed video exceeds byte limit or is empty",
        ));
    }
    let mut f = std::fs::File::open(output)
        .map_err(|_| AppError::config("could not read remuxed video"))?;
    std::io::copy(&mut f, writer).map_err(|_| AppError::config("could not write remuxed video"))?;
    Ok(Download {
        bytes: size as usize,
        content_type: "video/mp4",
    })
}
fn hls_to(
    sources: &[Url],
    writer: &mut impl Write,
    max: usize,
    seconds: u64,
) -> Result<Download, AppError> {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        return Err(AppError::config(
            "ffmpeg is required for HLS video; install ffmpeg and retry",
        ));
    }
    let scratch = Scratch::new()?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut budget = max;
    let mut index = 0;
    let mut inputs = Vec::new();
    for source in sources {
        inputs.push(local_playlist(
            source,
            &scratch,
            &mut index,
            &mut budget,
            deadline,
            0,
        )?);
    }
    remux(&inputs, &scratch, writer, max, deadline)
}

#[cfg(test)]
#[path = "panopto_tests.rs"]
mod tests;

/// Origin/path policy for the fixed authentication and metadata endpoints.
pub(crate) fn allow_exchange(u: &Url, klms: &Url, delivery_only: bool) -> Result<(), AppError> {
    if u.origin() == klms.origin()
        && (u.path() == "/enrol/index.php" || u.path().starts_with("/login/"))
    {
        return Err(AppError::content_unavailable(
            "Panopto launch requires enrollment or renewed KLMS sign-in",
        ));
    }
    let klms = u.origin() == klms.origin()
        && matches!(
            u.path(),
            "/blocks/panopto/lib/lti/auth.php"
                | "/mod/lti/auth.php"
                | "/mod/panoptocourseembed/view_content.php"
                | "/mod/lti/launch.php"
        );
    let pano = video::panopto_origin(u)
        && (matches!(
            u.path(),
            "/Panopto/Pages/Embed.aspx"
                | "/Panopto/Pages/Sessions/List.aspx"
                | "/Panopto/Pages/Auth/CookieCheck.aspx"
                | "/Panopto/Pages/EmbeddedList.aspx"
                | "/Panopto/LTI/LTI.aspx"
                | "/Panopto/Pages/Viewer/DeliveryInfo.aspx"
                | "/Panopto/Services/Data.svc/GetSessions"
        ) || (u
            .path()
            .to_ascii_lowercase()
            .starts_with("/panopto/lti/adv/platforms/")
            && u.path().ends_with("/login")))
        && (!delivery_only
            || matches!(
                u.path(),
                "/Panopto/Pages/Viewer/DeliveryInfo.aspx"
                    | "/Panopto/Services/Data.svc/GetSessions"
            ));
    if (!delivery_only && klms || pano) && u.username().is_empty() && u.password().is_none() {
        Ok(())
    } else {
        Err(AppError::content_unavailable(
            "Panopto exchange refused an unexpected origin or endpoint",
        ))
    }
}
