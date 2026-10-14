use std::{path::Path, process::Command};

/// Write in a child so concurrent test forks cannot inherit a writable script
/// descriptor and cause ETXTBSY when the caller executes it.
pub fn write_executable(path: &Path, contents: &str) {
    let status = Command::new("sh")
        .args([
            "-c",
            "printf '%s' \"$2\" > \"$1\" && chmod 700 \"$1\"",
            "fixture-executable",
        ])
        .arg(path)
        .arg(contents)
        .status()
        .unwrap();
    assert!(status.success(), "cannot write fixture {}", path.display());
}
