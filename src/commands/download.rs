use super::resources::module_path;
use crate::{
    client::{DownloadResponse, KlmsClient},
    error::AppError,
    models::DownloadResult,
    output::{self, CommandResult},
    private_fs::{self, PublishError, out_parent, temporary_name},
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
    let source = if let Some(url) = url.filter(crate::client::policy::pluginfile) {
        crate::client::policy::read_url(&url).map_err(|error| AppError::usage(error.message))?;
        source.to_owned()
    } else {
        module_path(source, &["resource", "coursefile"])?
    };
    client.validate_operand(&source)?;
    validate_out(out, false)?;
    publish("files.download", out, |file| {
        client.download_to(&source, MAX_DOWNLOAD_BYTES, file)
    })
}

pub(super) fn validate_out(out: &Path, video: bool) -> Result<(), AppError> {
    let parent = out_parent(out);
    let (occupied, parent_exists) = match video {
        true => (out.symlink_metadata().is_ok(), parent.is_dir()),
        false => (out.exists(), parent.exists()),
    };
    if occupied {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    if !parent_exists {
        return Err(AppError::config(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }
    Ok(())
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
    let path = out.canonicalize().unwrap_or_else(|_| out.to_path_buf());
    let model = DownloadResult {
        path: path.display().to_string(),
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
    result
        .warnings
        .extend([cleanup, copied].into_iter().flatten());
    if video {
        result.warnings.reverse();
    }
    Ok(result)
}
