use super::resources::module_path;
use crate::{
    client::KlmsClient,
    error::AppError,
    models::DownloadResult,
    output::{self, CommandResult},
    private_fs::{PublishError, publish_download},
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
    if out.exists() {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let parent =
        (out.parent().filter(|path| !path.as_os_str().is_empty())).unwrap_or(Path::new("."));
    if !parent.exists() {
        return Err(AppError::config(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }
    let temp = parent.join(format!(".klms-download-{}.part", std::process::id()));
    let (response, linked) = publish_download(&temp, out, |file| {
        client.download_to(&source, MAX_DOWNLOAD_BYTES, file)
    })
    .map_err(|error| match error {
        PublishError::Fill(error) => error,
        PublishError::Create(error) => {
            AppError::config(format!("cannot create temporary download: {error}"))
        }
        PublishError::Sync(error) => AppError::config(format!("failed to write download: {error}")),
        PublishError::Link(error) => {
            AppError::config(format!("failed to finalize download: {error}"))
        }
    })?;
    if linked.existed {
        return Err(AppError::config(format!(
            "destination already exists: {}",
            out.display()
        )));
    }
    let cleanup_warning = linked.leftover.map(|error| {
        format!(
            "download completed, but temporary link cleanup failed for {}: {error}",
            linked.temporary.display()
        )
    });
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
    let mut result = output::result("files.download", &model, human)?;
    result.warnings.extend(cleanup_warning);
    if linked.copied {
        result
            .warnings
            .push("download published by non-atomic copy because hard linking failed".into());
    }
    Ok(result)
}
