use std::path::PathBuf;

const CONFIG_RELATIVE_PATH: &str = "upnow/config.toml";

pub(super) fn config_path() -> Option<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg_config_home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join(CONFIG_RELATIVE_PATH));
        }
    }

    let home = std::env::var("HOME").ok()?;
    let trimmed = home.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(
        PathBuf::from(trimmed)
            .join(".config")
            .join(CONFIG_RELATIVE_PATH),
    )
}
