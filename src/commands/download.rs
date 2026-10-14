use super::resources::module_path;
use crate::{
    client::{DownloadResponse, KlmsClient},
    error::AppError,
    models::DownloadResult,
    output::{self, CommandResult},
    private_fs::{self, PublishError, ScratchDir, out_parent, temporary_name},
    safe_url,
    url::Url,
};
use std::path::Path;
const MAX_DOWNLOAD_BYTES: usize = 256 * 1024 * 1024;

pub(super) fn download(
    client: &KlmsClient,
    source: &str,
    out: &Path,
) -> Result<CommandResult, AppError> {
    let base = Url::parse("https://klms.invalid/").expect("fixed URL");
    let url = base.join(source).ok();
    let source = if let Some(url) = url.filter(|url| url.path().starts_with("/pluginfile.php/")) {
        crate::client::policy::read_url(&url)?;
        source.to_owned()
    } else {
        module_path(source, &["resource", "folder", "coursefile"])?
    };
    validate_out(out, false)?;
    publish("files.download", out, |file| {
        client.download_to(&source, MAX_DOWNLOAD_BYTES, file)
    })
}

pub(super) fn validate_out(out: &Path, video: bool) -> Result<(), AppError> {
    if if video {
        out.symlink_metadata().is_ok()
    } else {
        out.exists()
    } {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let parent = out_parent(out);
    if if video {
        !parent.is_dir()
    } else {
        !parent.exists()
    } {
        return Err(AppError::config(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }
    Ok(())
}

pub(super) fn workspace(out: &Path) -> Result<ScratchDir, AppError> {
    let parent = out_parent(out);
    let workspace = ScratchDir::create(parent).map_err(|(path, e)| {
        AppError::config(format!(
            "could not create video workspace {}: {e}",
            path.display()
        ))
    })?;
    if let Ok(own) = workspace.0.symlink_metadata() {
        private_fs::sweep_stale(parent, &own);
    }
    Ok(workspace)
}

pub(super) fn publish(
    command: &'static str,
    out: &Path,
    fill: impl FnOnce(&mut std::fs::File) -> Result<DownloadResponse, AppError>,
) -> Result<CommandResult, AppError> {
    let video = command == "videos.download";
    let temp = out_parent(out).join(temporary_name("part"));
    let (response, linked) = private_fs::publish_download(&temp, out, fill).map_err(|error| {
        let (context, error) = match error {
            PublishError::Fill(error) => return error,
            PublishError::Create(error) => ("cannot create temporary download", error),
            PublishError::Sync(error) => ("failed to write download", error),
            PublishError::Link(error) => ("failed to finalize download", error),
        };
        let context = if video {
            "could not publish video"
        } else {
            context
        };
        AppError::config(format!("{context}: {error}"))
    })?;
    if linked.existed {
        return Err(AppError::config(if video {
            "destination already exists".into()
        } else {
            format!("destination already exists: {}", out.display())
        }));
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
    let human = format!("Downloaded {} bytes to {}", model.bytes, model.path);
    let mut result = output::result(command, &model, human)?;
    let cleanup = linked.leftover.map(|error| {
        if video {
            "download published; temporary link cleanup failed".into()
        } else {
            format!(
                "download completed, but temporary link cleanup failed for {}: {error}",
                linked.temporary.display()
            )
        }
    });
    let copied = linked
        .copied
        .then(|| "download published by non-atomic copy because hard linking failed".to_owned());
    result.warnings.extend(
        if video {
            [copied, cleanup]
        } else {
            [cleanup, copied]
        }
        .into_iter()
        .flatten(),
    );
    Ok(result)
}
