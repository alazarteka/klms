use super::{
    Ctx,
    download::{publish, validate_out, workspace},
    resources::module_path,
};
use crate::{cli::VideosCommand, client::stream::Limits, models::Video, reference::module_kind};
use crate::{
    course_pages,
    error::AppError,
    output::{self, CommandResult, listing, row, table},
    parse,
    private_fs::out_parent,
    reference::ResourceRef,
    url::{Url, query_value},
};

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
                validate_out(out, true)?;
                let cap = usize::try_from(*max_bytes)
                    .map_err(|_| AppError::usage("max-bytes exceeds this platform's capacity"))?;
                match VideoTarget::of(target) {
                    VideoTarget::Native => {
                        let (_, source) = self.native_video(target)?;
                        let limits = Limits::new(cap, *max_seconds, *stall_seconds);
                        publish("videos.download", out, |file| {
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
                        let workspace =
                            delivery.needs_remux().then(|| workspace(out)).transpose()?;
                        publish("videos.download", out, |file| {
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
        if module_kind(&requested) == Some("panoptocourseembed")
            && page.url.path() == "/course/view.php"
        {
            let module = query_value(&requested, "id").ok_or_else(|| {
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
        if module_kind(&page.url) != Some("vod") {
            return Err(AppError::content_unavailable(
                "video is not a native VOD recording",
            ));
        }
        let id = query_value(&page.url, "id")
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
