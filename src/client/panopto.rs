//! Panopto metadata and credential-free CDN media transport.
mod exchange;
mod folder;
use super::stream::{self, Expect, Limits};
use crate::{
    error::AppError,
    http::{self, Follow, Method, Payload},
    url::Url,
};
pub use exchange::launch;
use exchange::{Policy, request, text};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// The Panopto and media CDN origins the exchange and media policies trust,
/// and the ffmpeg used for remuxing. Production values are fixed; tests point
/// them at loopback fixtures.
#[derive(Clone, Debug)]
pub struct Config {
    panopto: Url,
    cdn: Url,
    ffmpeg: std::ffi::OsString,
}
impl Config {
    pub fn production() -> Result<Self, AppError> {
        Self::new(
            "https://kaist.ap.panopto.com/",
            "https://s-cloudfront.cdn.ap.panopto.com/",
        )
    }
    #[cfg(test)]
    pub fn fixture(panopto: &str, cdn: &str) -> Self {
        Self::new(panopto, cdn).expect("fixture config")
    }
    #[cfg(test)]
    pub fn with_ffmpeg(self, ffmpeg: impl Into<std::ffi::OsString>) -> Self {
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
    groups: Vec<SourceGroup>,
}
impl Delivery {
    pub fn needs_remux(&self) -> bool {
        !matches!(self.groups.as_slice(), [SourceGroup { mp4: Some(_), offset, .. }] if *offset == 0.0)
    }
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
    /// Copies a single unshifted MP4 directly; anything else (HLS, several
    /// tracks, start offsets) is fetched into `scratch`, a private directory
    /// the caller owns, and remuxed there by ffmpeg.
    pub fn download_to(
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
fn parse_delivery(id: &str, v: &Value, config: &Config) -> Result<Delivery, AppError> {
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
        output_stream_count: groups.len(),
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
type ByteRange = (u64, u64);
/// One credential-free CDN request, optionally for an exact byte range,
/// streamed through the shared bounded copy.
fn media_copy(
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
fn ffmpeg_error(e: &std::io::Error) -> AppError {
    if e.kind() == std::io::ErrorKind::NotFound {
        AppError::config("ffmpeg is required for HLS or multiple streams; install ffmpeg and retry")
    } else {
        AppError::config("could not start local ffmpeg")
    }
}
/// Fails before any media is fetched when the remux step could not run.
fn require_ffmpeg(ffmpeg: &std::ffi::OsStr, limits: Limits) -> Result<(), AppError> {
    let mut child = ChildGuard(
        Command::new(ffmpeg)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ffmpeg_error(&e))?,
    );
    let deadline = limits.deadline.min(Instant::now() + Duration::from_secs(2));
    loop {
        limits.remaining()?;
        if Instant::now() >= deadline {
            return Err(AppError::network("ffmpeg -version exceeded its time limit"));
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| AppError::config("could not monitor ffmpeg"))?
        {
            return if status.success() {
                Ok(())
            } else {
                Err(AppError::config(
                    "ffmpeg -version failed; check the ffmpeg install",
                ))
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Covers monitoring failures and every early return as well as normal completion.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const STDERR_TAIL: usize = 4096;
fn stderr_tail(mut reader: impl Read) -> Vec<u8> {
    let mut tail = std::collections::VecDeque::with_capacity(STDERR_TAIL);
    let mut buffer = [0; STDERR_TAIL];
    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            break;
        }
        let excess = (tail.len() + n).saturating_sub(STDERR_TAIL);
        tail.drain(..excess);
        tail.extend(&buffer[..n]);
    }
    tail.into_iter().collect()
}
/// Fetches one remote resource (or byte range) into a new private file and
/// charges it to `budget`.
fn fetch_local(
    url: &Url,
    path: &Path,
    budget: &mut usize,
    limits: Limits,
    fetch: &mut Fetch<'_>,
    range: Option<ByteRange>,
) -> Result<(), AppError> {
    let mut f = crate::private_fs::private_file_options()
        .create_new(true)
        .open(path)
        .map_err(|_| AppError::config("could not create local media fragment"))?;
    let d = fetch(url, &mut f, limits.with_max(*budget), range)?;
    *budget = budget
        .checked_sub(d.bytes)
        .ok_or_else(|| AppError::limit("video bytes exceeded budget"))?;
    Ok(())
}
fn quoted_attr<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_once(&format!("{key}=\""))?
        .1
        .split_once('"')
        .map(|(v, _)| v)
}
fn write_private(path: &Path, text: &str) -> Result<(), AppError> {
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
type Fetch<'a> =
    dyn FnMut(&Url, &mut dyn Write, Limits, Option<ByteRange>) -> Result<Download, AppError> + 'a;
fn local_playlist(
    url: &Url,
    scratch: &Path,
    index: &mut usize,
    budget: &mut usize,
    limits: Limits,
    depth: usize,
    fetch: &mut Fetch<'_>,
) -> Result<PathBuf, AppError> {
    if depth > 3 {
        return Err(AppError::limit("HLS playlist nesting exceeds limit"));
    }
    let mut raw = Vec::new();
    let cap = (*budget).min(1024 * 1024);
    let downloaded = fetch(url, &mut raw, limits.with_max(cap), None)?;
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
        let video = local_playlist(&child, scratch, index, budget, limits, depth + 1, fetch)?;
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
        let audio = local_playlist(&audio_url, scratch, index, budget, limits, depth + 1, fetch)?;
        let master = scratch.join(format!("playlist-{}.m3u8", *index));
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
    let out = scratch.join(format!("playlist-{}.m3u8", *index));
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
            fetch_local(&u, &scratch.join(&name), budget, limits, fetch, range)?;
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
            fetch_local(&u, &scratch.join(&name), budget, limits, fetch, range)?;
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
    ffmpeg: &std::ffi::OsStr,
    inputs: &[PathBuf],
    offsets: &[f64],
    scratch: &Path,
    writer: &mut impl Write,
    limits: Limits,
) -> Result<Download, AppError> {
    let max = limits.max_bytes;
    limits.remaining()?;
    let output = scratch.join("remux.mp4");
    let mut c = Command::new(ffmpeg);
    c.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-xerror"]);
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
        .stderr(Stdio::piped());
    let mut child = ChildGuard(c.spawn().map_err(|e| ffmpeg_error(&e))?);
    let stderr = child.0.stderr.take().expect("piped stderr");
    let tail = std::thread::spawn(move || stderr_tail(stderr));
    let status = loop {
        limits.remaining()?;
        if std::fs::metadata(&output).is_ok_and(|m| m.len() > max as u64) {
            return Err(AppError::limit("video remux exceeds byte limit"));
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| AppError::config("could not monitor ffmpeg"))?
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let tail = tail.join().unwrap_or_default();
    limits.remaining()?;
    if !status.success() {
        return Err(AppError::content_unavailable(format!(
            "ffmpeg could not remux downloaded video fragments: {}",
            String::from_utf8_lossy(&tail).trim()
        )));
    }
    let size = std::fs::metadata(&output)
        .map_err(|_| AppError::config("ffmpeg produced no output"))?
        .len();
    if size == 0 || size > max as u64 {
        return Err(AppError::limit(
            "remuxed video exceeds byte limit or is empty",
        ));
    }
    // Free the fetched inputs before the copy so peak disk use stays near
    // twice the media size rather than three times.
    for input in std::fs::read_dir(scratch).into_iter().flatten().flatten() {
        if input.path() != output {
            let _ = std::fs::remove_file(input.path());
        }
    }
    let mut f = std::fs::File::open(&output)
        .map_err(|_| AppError::config("could not read remuxed video"))?;
    let mut buffer = [0; 64 * 1024];
    loop {
        limits.remaining()?;
        let n = f
            .read(&mut buffer)
            .map_err(|_| AppError::config("could not read remuxed video"))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buffer[..n])
            .map_err(|_| AppError::config("could not write remuxed video"))?;
        limits.remaining()?;
    }
    drop(f);
    std::fs::remove_file(&output)
        .map_err(|_| AppError::config("could not remove remuxed video"))?;
    Ok(Download {
        bytes: size as usize,
        content_type: "video/mp4",
    })
}
#[cfg(test)]
mod tests;
