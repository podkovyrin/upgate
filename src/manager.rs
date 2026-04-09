use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Manager {
    Brew,
    Bun,
    Cargo,
    Npm,
    Yarn,
    Mise,
    Pipx,
    Pnpm,
    Uv,
}

impl Manager {
    pub(crate) fn default_managers() -> Vec<Self> {
        vec![
            Self::Brew,
            Self::Bun,
            Self::Cargo,
            Self::Npm,
            Self::Yarn,
            Self::Mise,
            Self::Pipx,
            Self::Pnpm,
            Self::Uv,
        ]
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Brew => "brew",
            Self::Bun => "bun",
            Self::Cargo => "cargo",
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Mise => "mise",
            Self::Pipx => "pipx",
            Self::Pnpm => "pnpm",
            Self::Uv => "uv",
        }
    }
}
