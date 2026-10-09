use std::{
    fs,
    path::{Path, PathBuf},
};

// Each integration target uses a different subset of this shared harness.
#[allow(dead_code)]
pub mod server;

/// Creates `<state_root>/klms/session.json` with the given cookie value and returns `state_root`.
#[allow(dead_code)]
pub fn seed_session(state_root: &Path, cookie: &str) -> PathBuf {
    fs::create_dir_all(state_root.join("klms")).unwrap();
    fs::write(
        state_root.join("klms/session.json"),
        format!(
            r#"{{"version":1,"origin":"http://127.0.0.1:0","created_at":1,"cookies":[{{"name":"MoodleSession","value":"{cookie}"}}],"devices":[]}}"#
        ),
    )
    .unwrap();
    state_root.to_path_buf()
}
