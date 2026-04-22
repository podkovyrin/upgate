use anyhow::{Context, Result, bail};

use super::model::{ManagerMode, UpnowConfig};
use crate::managers::shared::versioning::policy::VersionPolicy;

impl UpnowConfig {
    pub fn apply_cli_override(&mut self, raw: &str, known_manager_ids: &[&str]) -> Result<()> {
        let (path, value) = raw.split_once('=').with_context(|| {
            format!("invalid override '{raw}': expected <manager>.<key>=<value>")
        })?;

        let (manager_id, key) = path.split_once('.').with_context(|| {
            format!("invalid override '{raw}': expected <manager>.<key>=<value>")
        })?;

        if manager_id.is_empty() || key.is_empty() || value.is_empty() {
            bail!("invalid override '{raw}': expected <manager>.<key>=<value>");
        }

        if manager_id == "upnow" {
            return match key {
                "scan_old_age_threshold" => {
                    self.upnow.scan_old_age_threshold = Some(value.to_string());
                    Ok(())
                }
                _ => bail!(
                    "invalid override '{raw}': unknown key '{key}' for global section 'upnow'"
                ),
            };
        }

        if !known_manager_ids.contains(&manager_id) {
            bail!("invalid override '{raw}': unknown manager '{manager_id}'");
        }

        match key {
            "min_release_age" => {
                self.sections
                    .entry(manager_id.to_string())
                    .or_default()
                    .min_release_age = Some(value.to_string());
                Ok(())
            }
            "version_policy" => {
                let parsed = VersionPolicy::parse_for(manager_id, value).with_context(|| {
                    format!(
                        "invalid override '{raw}': value for {manager_id}.version_policy must be one of stable, same-track, any"
                    )
                })?;
                self.sections
                    .entry(manager_id.to_string())
                    .or_default()
                    .version_policy = Some(parsed.as_str().to_string());
                Ok(())
            }
            "pinned" => bail!(
                "invalid override '{raw}': key 'pinned' is interactive-only in this iteration"
            ),
            "no_update" => {
                if manager_id != "brew" {
                    bail!(
                        "invalid override '{raw}': key 'no_update' is only valid for manager 'brew'"
                    );
                }

                let parsed = value.parse::<bool>().with_context(|| {
                    format!(
                        "invalid override '{raw}': value for brew.no_update must be true or false"
                    )
                })?;
                self.sections
                    .entry(manager_id.to_string())
                    .or_default()
                    .no_update = Some(parsed);
                Ok(())
            }
            "mode" => {
                let parsed = ManagerMode::parse_for(manager_id, value).with_context(|| {
                    format!(
                        "invalid override '{raw}': value for {manager_id}.mode must be one of off, plan, apply"
                    )
                })?;
                self.sections
                    .entry(manager_id.to_string())
                    .or_default()
                    .mode = Some(parsed.to_string());
                Ok(())
            }
            _ => bail!("invalid override '{raw}': unknown key '{key}' for manager '{manager_id}'"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_version_policy_override_for_known_manager() {
        let mut config = UpnowConfig::default();
        config
            .apply_cli_override("npm.version_policy=same-track", &["npm"])
            .expect("override should be accepted");

        let policy = config
            .sections
            .get("npm")
            .and_then(|section| section.version_policy.as_deref());
        assert_eq!(policy, Some("same-track"));
    }

    #[test]
    fn rejects_invalid_version_policy_override_value() {
        let mut config = UpnowConfig::default();
        let err = config
            .apply_cli_override("npm.version_policy=beta-only", &["npm"])
            .expect_err("invalid value should be rejected");

        assert_eq!(
            err.to_string(),
            "invalid override 'npm.version_policy=beta-only': value for npm.version_policy must be one of stable, same-track, any"
        );
    }
}
