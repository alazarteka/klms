use std::path::Path;

use super::*;
use crate::{cli::VideosCommand, models::Video};

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
                if !target.starts_with("vod:") && !target.contains("/mod/vod/") {
                    let (mut session, uuid, launch_ref) = self.panopto_video(target)?;
                    if uuid.is_some() || session.recording_id.is_some() {
                        let delivery = session.delivery(uuid.as_deref())?;
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
                        reference: launch_ref,
                        kind: "panopto".into(),
                        title: "Panopto collection".into(),
                        video_type: "collection",
                        course_ref: None,
                        duration_seconds: None,
                        downloadable: None,
                        official_download_available: None,
                        stream_count: None,
                        output_composition: None,
                    };
                    let mut result = output::result(
                        "videos.show",
                        &serde_json::json!({"collection": model, "recordings": recordings.rows.iter().map(|r| Ok(serde_json::json!({"ref":recording_ref(&model.reference,&r.id)?,"title":r.title,"duration_seconds":r.duration_seconds}))).collect::<Result<Vec<_>,AppError>>()?, "complete":recordings.complete}),
                        format!("Panopto collection: {} recordings", recordings.rows.len()),
                    )?;
                    if !recordings.complete {
                        result
                            .warnings
                            .push("collection enumeration is incomplete".into());
                    }
                    return Ok(result);
                }
                let (model, _) = self.native_video(target)?;
                let human = format!(
                    "{}\nRef: {}\nType: {}\nDownloadable: {}",
                    model.title,
                    model.reference,
                    model.video_type,
                    model
                        .downloadable
                        .map_or("unknown", |v| if v { "yes" } else { "no" })
                );
                output::result("videos.show", &model, human)
            }
            VideosCommand::Download {
                target,
                out,
                max_bytes,
                max_seconds,
            } => {
                validate_out(out)?;
                let cap = usize::try_from(*max_bytes)
                    .map_err(|_| AppError::usage("max-bytes exceeds this platform's capacity"))?;
                if target.starts_with("vod:") || target.contains("/mod/vod/") {
                    let (_, source) = self.native_video(target)?;
                    publish_video(out, |file| {
                        self.client
                            .download_video_to(source.as_str(), cap, *max_seconds, file)
                    })
                } else {
                    let (mut session, uuid, launch_ref) = self.panopto_video(target)?;
                    let delivery = session.delivery(uuid.as_deref())?;
                    publish_video(out, |file| {
                        let downloaded = session.download_to(&delivery, file, cap, *max_seconds)?;
                        Ok(crate::client::DownloadResponse {
                            bytes: downloaded.bytes,
                            content_type: Some(downloaded.content_type.into()),
                            url: self
                                .base
                                .join(&ResourceRef::parse(&launch_ref)?.path())
                                .expect("built-in path"),
                        })
                    })
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
        let (launch_ref, uuid) = match ResourceRef::parse(target) {
            Ok(ResourceRef::PanoptoRecording { kind, module, uuid }) => {
                (format!("{kind}:{module}"), Some(uuid))
            }
            _ => (target.to_owned(), None),
        };
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

    fn native_video(&self, target: &str) -> Result<(Video, Url), AppError> {
        let path = module_path(target, &["vod", "lti", "panopto", "panoptocourseembed"])?;
        let page = self.client.get(&path)?;
        self.validate_detail_identity(&path, &page.url)?;
        let reference = ResourceRef::from_url(&page.url)
            .ok_or_else(|| AppError::shape("video page has no supported identity"))?;
        if reference.activity_kind() != Some("vod") {
            return Err(AppError::content_unavailable(
                "video is not a native VOD recording",
            ));
        }
        let id = page
            .url
            .query_value("id")
            .ok_or_else(|| AppError::shape("VOD page has no numeric id"))?;
        let detail = parse::resource_detail(&page.text, self.base, &page.url, "vod")?;
        let viewer = self
            .client
            .get(&format!("/mod/vod/viewer/index.php?id={id}"))?;
        if viewer.url.path() != "/mod/vod/viewer/index.php" {
            return Err(AppError::shape(
                "native viewer redirected to an unexpected endpoint",
            ));
        }
        self.validate_detail_identity(&format!("/mod/vod/viewer/index.php?id={id}"), &viewer.url)?;
        let sources = parse::native_sources(&viewer.text, &viewer.url)?;
        if sources.len() != 1 {
            return Err(AppError::content_unavailable(
                "native recording has multiple MP4 sources; cannot choose a composition",
            ));
        }
        let model = Video {
            reference: reference.to_string(),
            kind: "vod".into(),
            title: detail.title,
            video_type: "recording",
            course_ref: None,
            duration_seconds: None,
            downloadable: Some(true),
            official_download_available: None,
            stream_count: Some(1),
            output_composition: Some("single_stream".into()),
        };
        Ok((model, sources.into_iter().next().expect("one source")))
    }
}

fn validate_out(out: &Path) -> Result<(), AppError> {
    if out.symlink_metadata().is_ok() {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
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
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp = parent.join(format!(".klms-video-{}.part", std::process::id()));
    let (response, linked) = publish_new(&temp, out, fill).map_err(|e| match e {
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
