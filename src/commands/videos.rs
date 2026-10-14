use std::path::Path;

use super::*;
use crate::{cli::VideosCommand, client::stream::Limits, models::Video};

/// Which transport a video reference uses: native KLMS VOD or Panopto.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VideoTarget {
    Native,
    Panopto,
}

impl VideoTarget {
    fn of(target: &str) -> Self {
        if target.starts_with("vod:") || target.contains("/mod/vod/") {
            Self::Native
        } else {
            Self::Panopto
        }
    }
}

/// Prefix of every temporary entry a download leaves next to `--out`.
const TEMP_PREFIX: &str = ".klms-video-";
/// Stale temporaries older than this are swept by the next download.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// A private scratch directory beside `--out` (same filesystem, not a RAM
/// tmpfs), removed when the download ends.
struct Workspace(std::path::PathBuf);

impl Workspace {
    fn create(out: &Path) -> Result<Self, AppError> {
        let parent = out_parent(out);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        let mut path = parent.join(temporary_name("scratch"));
        for attempt in 0..8 {
            match builder.create(&path) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 7 => {
                    path = parent.join(temporary_name("scratch"));
                }
                Err(e) => {
                    return Err(AppError::config(format!(
                        "could not create video workspace {}: {e}",
                        path.display()
                    )));
                }
            }
        }
        let workspace = Self(path);
        if let Ok(own) = workspace.0.symlink_metadata() {
            sweep_stale(parent, &own);
        }
        Ok(workspace)
    }
}

fn temporary_name(suffix: &str) -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
        + u128::from(SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    format!("{TEMP_PREFIX}{}-{nonce:x}.{suffix}", std::process::id())
}

/// Only generated names belonging to a dead process can be swept. Platforms
/// without a process-liveness check retain crash leftovers rather than risk active work.
fn sweep_stale(parent: &Path, own: &std::fs::Metadata) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        if !Path::new("/proc/self").is_dir() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(rest) = name.strip_prefix(TEMP_PREFIX) else {
                continue;
            };
            let Some((pid, rest)) = rest.split_once('-') else {
                continue;
            };
            if pid.is_empty()
                || !pid.bytes().all(|c| c.is_ascii_digit())
                || pid.parse::<u32>().is_err()
                || Path::new("/proc").join(pid).exists()
            {
                continue;
            }
            let Some((nonce, suffix)) = rest.split_once('.') else {
                continue;
            };
            if nonce.is_empty() || !nonce.bytes().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let part = suffix == "part"
                || suffix
                    .strip_prefix("part.")
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_hexdigit()));
            let path = entry.path();
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            if meta.uid() != own.uid()
                || meta
                    .modified()
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_none_or(|age| age <= STALE_AFTER)
            {
                continue;
            }
            if meta.file_type().is_file() && part {
                let _ = std::fs::remove_file(&path);
            } else if meta.file_type().is_dir() && suffix == "scratch" {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (parent, own);
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn out_parent(out: &Path) -> &Path {
    out.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

impl Ctx<'_> {
    pub(super) fn videos(&self, command: &VideosCommand) -> Result<CommandResult, AppError> {
        match command {
            VideosCommand::List { course, list } => {
                let resolved = self.resolve_course(course)?;
                let activities = course_pages::activities(self.client, self.base, &resolved.id)?;
                let mut videos = Vec::new();
                let mut complete = true;
                // Metadata only: opening an activity (mod/*/view.php or its LTI
                // launch) is logged by Moodle as a view, so `list` never does.
                for activity in activities {
                    let video_type = match activity.kind.as_str() {
                        "vod" => "recording",
                        "panopto" | "panoptocourseembed" => "panopto",
                        "lti" => "lti",
                        _ => continue,
                    };
                    let Some(reference) = activity.reference else {
                        complete = false;
                        continue;
                    };
                    videos.push(Video {
                        reference,
                        kind: activity.kind,
                        title: activity.title,
                        video_type,
                        course_ref: Some(resolved.reference.clone()),
                        duration_seconds: None,
                        downloadable: None,
                        official_download_available: None,
                        stream_count: None,
                        output_composition: None,
                    });
                }
                listing(
                    "videos.list",
                    videos,
                    list.limit,
                    complete,
                    |rows, available| {
                        table(rows, available, "Videos", "REF\tTYPE\tTITLE", |v| {
                            row(&[&v.reference, v.video_type, &v.title])
                        })
                    },
                )
            }
            VideosCommand::Show { target } => {
                if VideoTarget::of(target) == VideoTarget::Panopto {
                    let (launch_ref, uuid) = panopto_target(target);
                    let page = self.video_page(&launch_ref)?;
                    let model = self.activity_video(&page, &launch_ref)?;
                    let launch_ref = model.reference.clone();
                    let mut session = match self.client.panopto_launch(&page, self.timeout) {
                        Ok(session) => session,
                        Err(error) => return metadata_warning(model, error),
                    };
                    if uuid.is_some() || session.recording_id.is_some() {
                        let delivery = match session.delivery(uuid.as_deref()) {
                            Ok(delivery) => delivery,
                            Err(error) => return metadata_warning(model, error),
                        };
                        let model = panopto_model(&launch_ref, &delivery)?;
                        return output::result(
                            "videos.show",
                            &model,
                            format!(
                                "{}\nRef: {}\nStreams: {}\nReadable MP4: {}\nReadable HLS: {}",
                                model.title,
                                model.reference,
                                delivery.stream_count,
                                delivery.mp4_available,
                                delivery.hls_available
                            ),
                        );
                    }
                    let recordings = session.recordings()?;
                    let model = Video {
                        video_type: "collection",
                        ..model
                    };
                    let rows = recordings
                        .rows
                        .iter()
                        .map(|r| Ok((recording_ref(&model.reference, &r.id)?, r)))
                        .collect::<Result<Vec<_>, AppError>>()?;
                    let human = table(
                        &rows,
                        rows.len(),
                        "Panopto recordings",
                        "REF\tTITLE\tDURATION (SECONDS)",
                        |(reference, r)| {
                            row(&[
                                reference,
                                &r.title,
                                &r.duration_seconds
                                    .map_or("unknown".into(), |d| d.to_string()),
                            ])
                        },
                    );
                    let mut result = output::result(
                        "videos.show",
                        &serde_json::json!({"collection": model, "recordings": rows.iter().map(|(reference,r)| serde_json::json!({"ref":reference,"title":r.title,"duration_seconds":r.duration_seconds})).collect::<Vec<_>>(), "complete":recordings.complete}),
                        human,
                    )?;
                    if !recordings.complete {
                        result
                            .warnings
                            .push("collection enumeration is incomplete".into());
                    }
                    return Ok(result);
                }
                let page = self.video_page(target)?;
                let mut model = self.activity_video(&page, target)?;
                match self.native_source(&page) {
                    Ok(_) => {
                        model.downloadable = Some(true);
                        model.stream_count = Some(1);
                        model.output_composition = Some("single_stream".into());
                        output::result("videos.show", &model, video_human(&model))
                    }
                    Err(error) => metadata_warning(model, error),
                }
            }
            VideosCommand::Download {
                target,
                out,
                max_bytes,
                max_seconds,
                stall_seconds,
            } => {
                validate_out(out)?;
                let cap = usize::try_from(*max_bytes)
                    .map_err(|_| AppError::usage("max-bytes exceeds this platform's capacity"))?;
                match VideoTarget::of(target) {
                    VideoTarget::Native => {
                        let (_, source) = self.native_video(target)?;
                        let limits = Limits::new(cap, *max_seconds, *stall_seconds);
                        publish_video(out, |file| {
                            self.client.download_video_to(source.as_str(), limits, file)
                        })
                    }
                    VideoTarget::Panopto => {
                        let (mut session, uuid, launch_ref) = self.panopto_video(target)?;
                        let delivery = session.delivery(uuid.as_deref())?;
                        let url = self
                            .base
                            .join(&ResourceRef::parse(&launch_ref)?.path())
                            .map_err(|_| AppError::shape("Panopto launch locator is invalid"))?;
                        let limits = Limits::new(cap, *max_seconds, *stall_seconds);
                        let workspace = delivery
                            .needs_remux()
                            .then(|| Workspace::create(out))
                            .transpose()?;
                        publish_video(out, |file| {
                            let downloaded = session.download_to(
                                &delivery,
                                file,
                                limits,
                                workspace
                                    .as_ref()
                                    .map_or(out_parent(out), |w| w.0.as_path()),
                            )?;
                            Ok(crate::client::DownloadResponse {
                                bytes: downloaded.bytes,
                                content_type: Some(downloaded.content_type.into()),
                                url,
                            })
                        })
                    }
                }
            }
        }
    }

    fn video_page(&self, target: &str) -> Result<crate::client::HtmlResponse, AppError> {
        let path = module_path(target, &["vod", "lti", "panopto", "panoptocourseembed"])?;
        let mut page = self.client.get(&path)?;
        let requested = self
            .base
            .join(&path)
            .map_err(|_| AppError::usage("invalid video URL"))?;
        if requested.module_kind() == Some("panoptocourseembed")
            && page.url.path() == "/course/view.php"
        {
            let module = requested.query_value("id").ok_or_else(|| {
                AppError::usage("embedded video reference requires a numeric module id")
            })?;
            page.text = parse::embedded_activity(&page.text, &module)?;
        } else {
            self.validate_detail_identity(&path, &page.url)?;
        }
        Ok(page)
    }

    fn panopto_video(
        &self,
        target: &str,
    ) -> Result<(crate::client::panopto::Session, Option<String>, String), AppError> {
        let (launch_ref, uuid) = panopto_target(target);
        let page = self.video_page(&launch_ref)?;
        let requested = self
            .base
            .join(&module_path(
                &launch_ref,
                &["lti", "panopto", "panoptocourseembed"],
            )?)
            .map_err(|_| AppError::usage("invalid video URL"))?;
        let reference = ResourceRef::from_url(&requested)
            .ok_or_else(|| AppError::shape("video page has no identity"))?;
        if !reference.matches_module(&["lti", "panopto", "panoptocourseembed"]) {
            return Err(AppError::shape("video detail has unexpected resource kind"));
        }
        let session = self.client.panopto_launch(&page, self.timeout)?;
        Ok((session, uuid, reference.to_string()))
    }

    fn activity_video(
        &self,
        page: &crate::client::HtmlResponse,
        target: &str,
    ) -> Result<Video, AppError> {
        let requested = self
            .base
            .join(&module_path(
                target,
                &["vod", "lti", "panopto", "panoptocourseembed"],
            )?)
            .map_err(|_| AppError::usage("invalid video URL"))?;
        let reference = ResourceRef::from_url(&requested)
            .ok_or_else(|| AppError::shape("video page has no supported identity"))?;
        let kind = reference
            .activity_kind()
            .ok_or_else(|| AppError::shape("video activity has no kind"))?;
        let detail = parse::resource_detail(&page.text, self.base, &requested, kind)?;
        Ok(Video {
            reference: reference.to_string(),
            kind: kind.into(),
            title: detail.title,
            video_type: if kind == "vod" {
                "recording"
            } else {
                "panopto"
            },
            course_ref: None,
            duration_seconds: None,
            downloadable: None,
            official_download_available: None,
            stream_count: None,
            output_composition: None,
        })
    }

    fn native_video(&self, target: &str) -> Result<(Video, Url), AppError> {
        let page = self.video_page(target)?;
        let model = self.activity_video(&page, target)?;
        let source = self.native_source(&page)?;
        Ok((model, source))
    }

    fn native_source(&self, page: &crate::client::HtmlResponse) -> Result<Url, AppError> {
        if page.url.module_kind() != Some("vod") {
            return Err(AppError::content_unavailable(
                "video is not a native VOD recording",
            ));
        }
        let id = page
            .url
            .query_value("id")
            .ok_or_else(|| AppError::shape("VOD page has no numeric id"))?;
        let path = format!("/mod/vod/viewer/index.php?id={id}");
        let viewer = self.client.get(&path)?;
        if viewer.url.path() != "/mod/vod/viewer/index.php" {
            return Err(AppError::shape(
                "native viewer redirected to an unexpected endpoint",
            ));
        }
        self.validate_detail_identity(&path, &viewer.url)?;
        let sources = parse::native_sources(&viewer.text, &viewer.url)?;
        let [source] = <[Url; 1]>::try_from(sources).map_err(|_| {
            AppError::content_unavailable(
                "native recording has multiple MP4 sources; cannot choose a composition",
            )
        })?;
        Ok(source)
    }
}

fn panopto_target(target: &str) -> (String, Option<String>) {
    match ResourceRef::parse(target) {
        Ok(ResourceRef::PanoptoRecording { kind, module, uuid }) => {
            (format!("{kind}:{module}"), Some(uuid))
        }
        _ => (target.to_owned(), None),
    }
}

fn video_human(model: &Video) -> String {
    format!(
        "{}\nRef: {}\nType: {}\nDownloadable: {}",
        model.title,
        model.reference,
        model.video_type,
        model
            .downloadable
            .map_or("unknown", |v| if v { "yes" } else { "no" })
    )
}

fn metadata_warning(model: Video, error: AppError) -> Result<CommandResult, AppError> {
    let mut result = output::result("videos.show", &model, video_human(&model))?;
    result.warnings.push(format!(
        "playback inspection failed ({}): {}",
        error.code, error.message
    ));
    Ok(result)
}

fn validate_out(out: &Path) -> Result<(), AppError> {
    if out.symlink_metadata().is_ok() {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let parent = out_parent(out);
    if !parent.is_dir() {
        return Err(AppError::config(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }
    Ok(())
}

fn publish_video(
    out: &Path,
    fill: impl FnOnce(&mut std::fs::File) -> Result<crate::client::DownloadResponse, AppError>,
) -> Result<CommandResult, AppError> {
    let temp = out_parent(out).join(temporary_name("part"));
    let (response, linked) = publish_download(&temp, out, |file| {
        if let Ok(own) = file.metadata() {
            sweep_stale(out_parent(out), &own);
        }
        fill(file)
    })
    .map_err(|e| match e {
        PublishError::Fill(e) => e,
        PublishError::Create(e) | PublishError::Sync(e) | PublishError::Link(e) => {
            AppError::config(format!("could not publish video: {e}"))
        }
    })?;
    if linked.existed {
        return Err(AppError::config("destination already exists"));
    }
    let model = DownloadResult {
        path: out
            .canonicalize()
            .unwrap_or_else(|_| out.to_path_buf())
            .display()
            .to_string(),
        bytes: response.bytes,
        source_url: safe_url::display(&response.url),
        content_type: response.content_type,
    };
    let mut result = output::result(
        "videos.download",
        &model,
        format!("Downloaded {} bytes to {}", model.bytes, model.path),
    )?;
    if linked.copied {
        result
            .warnings
            .push("download published by non-atomic copy because hard linking failed".into());
    }
    if linked.leftover.is_some() {
        result
            .warnings
            .push("download published; temporary link cleanup failed".into());
    }
    Ok(result)
}

fn recording_ref(launch: &str, uuid: &str) -> Result<String, AppError> {
    match ResourceRef::parse(launch)? {
        ResourceRef::Video { kind, id }
            if ["lti", "panopto", "panoptocourseembed"].contains(&kind.as_str()) =>
        {
            Ok(format!(
                "panopto-recording:{kind}:{id}:{}",
                uuid.to_ascii_lowercase()
            ))
        }
        _ => Err(AppError::shape("recording launch identity is not Panopto")),
    }
}

fn panopto_model(launch: &str, d: &crate::client::panopto::Delivery) -> Result<Video, AppError> {
    Ok(Video {
        reference: recording_ref(launch, &d.recording.id)?,
        kind: "panopto".into(),
        title: d.recording.title.clone(),
        video_type: "recording",
        course_ref: None,
        duration_seconds: d.recording.duration_seconds,
        downloadable: Some(d.mp4_available || d.hls_available),
        official_download_available: Some(d.official_download_available),
        stream_count: Some(d.stream_count),
        output_composition: Some(d.composition.into()),
    })
}

#[cfg(test)]
#[path = "videos_tests.rs"]
mod tests;
