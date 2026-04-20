use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result, bail};
use toml_edit::{Array, DocumentMut, Item, Table, Value};

use super::model::UpnowConfig;
use super::path::config_path;
use crate::util::text::is_blank;

impl UpnowConfig {
    pub fn set_manager_pins(&mut self, manager_id: &str, pins: BTreeSet<String>) {
        let section = self.sections.entry(manager_id.to_string()).or_default();
        section.pinned = pins.into_iter().collect();
    }

    pub fn persist_manager_pins(&self, manager_id: &str) -> Result<()> {
        let Some(path) = config_path() else {
            bail!("cannot determine config path from environment");
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }

        let mut doc = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read config file {}", path.display()))?;
            if is_blank(&raw) {
                DocumentMut::new()
            } else {
                raw.parse::<DocumentMut>()
                    .with_context(|| format!("failed to parse config TOML at {}", path.display()))?
            }
        } else {
            DocumentMut::new()
        };

        let pins: Vec<String> = self
            .sections
            .get(manager_id)
            .map(|cfg| cfg.pinned.clone())
            .unwrap_or_default();

        if pins.is_empty() {
            if let Some(item) = doc.get_mut(manager_id) {
                let Some(table) = item.as_table_like_mut() else {
                    bail!("failed to persist pins: key '{manager_id}' is not a table");
                };
                table.remove("pinned");
            }
        } else {
            if !doc.contains_key(manager_id) {
                doc[manager_id] = Item::Table(Table::new());
            }

            let Some(table) = doc[manager_id].as_table_like_mut() else {
                bail!("failed to persist pins: key '{manager_id}' is not a table");
            };

            let mut array = Array::default();
            for pin in pins {
                array.push(Value::from(pin));
            }
            table.insert("pinned", Item::Value(Value::Array(array)));
        }

        fs::write(&path, doc.to_string())
            .with_context(|| format!("failed to write config file {}", path.display()))
    }
}
