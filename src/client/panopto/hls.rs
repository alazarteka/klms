use super::Download;
use crate::{client::stream::Limits, error::AppError, url::Url};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub(super) type ByteRange = (u64, u64);
/// Fetches one remote resource (or byte range) into a new private file and
/// charges it to `budget`.
pub(super) fn fetch_local(
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
pub(super) fn local_playlist(
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
