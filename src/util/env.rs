use std::path::PathBuf;

use super::text::trim_non_empty;

pub fn non_empty_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .and_then(|raw| trim_non_empty(&raw).map(ToOwned::to_owned))
}

pub fn non_empty_path_var(name: &str) -> Option<PathBuf> {
    non_empty_var(name).map(PathBuf::from)
}

pub fn home_dir() -> Option<PathBuf> {
    non_empty_path_var("HOME")
}
