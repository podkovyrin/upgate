//! Manager adapter crate for the `upnow` rebuild.
#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

pub mod adapter;
pub mod brew;
pub mod bun;
pub mod cargo;
pub mod dotnet;
pub mod gem;
pub mod go;
pub mod mise;
pub mod npm;
pub mod pipx;
pub(crate) mod platform_artifacts;
pub mod pnpm;
pub mod uv;
pub mod yarn;
