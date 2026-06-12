use std::collections::BTreeMap;
use std::path::PathBuf;

/// Environment source for code that needs deterministic tests.
#[derive(Debug, Clone, Default)]
pub enum Env {
    #[default]
    Real,
    Fixed(BTreeMap<String, String>),
}

impl Env {
    pub const fn real() -> Self {
        Self::Real
    }
    pub fn fixed(vars: impl IntoIterator<Item = (String, String)>) -> Self {
        Self::Fixed(vars.into_iter().collect())
    }
    pub fn var(&self, name: &str) -> Option<String> {
        match self {
            Self::Real => std::env::var(name).ok(),
            Self::Fixed(vars) => vars.get(name).cloned(),
        }
    }
    pub fn non_empty_var(&self, name: &str) -> Option<String> {
        let value = self.var(name)?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    }
    pub fn non_empty_path_var(&self, name: &str) -> Option<PathBuf> {
        self.non_empty_var(name).map(PathBuf::from)
    }
    pub fn home_dir(&self) -> Option<PathBuf> {
        self.non_empty_path_var("HOME")
    }
}
