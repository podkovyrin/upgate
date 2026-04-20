use std::path::PathBuf;

use crate::util::env::{home_dir, non_empty_path_var};

const CONFIG_RELATIVE_PATH: &str = "upnow/config.toml";

pub(super) fn config_path() -> Option<PathBuf> {
    if let Some(xdg_config_home) = non_empty_path_var("XDG_CONFIG_HOME") {
        return Some(xdg_config_home.join(CONFIG_RELATIVE_PATH));
    }

    home_dir().map(|home_dir| home_dir.join(".config").join(CONFIG_RELATIVE_PATH))
}
