//! `dq self check` and `dq self update [--to <ver>]` — operations on the
//! `dq` binary itself.
//!
//! Mirrors `rustup self check` / `rustup self update`. The module is named
//! `self_cmd` because `self` is a Rust keyword; the subcommand still surfaces
//! as `dq self` thanks to `#[command(name = "self")]` on [`SelfArgs`].

use clap::{Args, Subcommand};

/// Arguments for `dq self <SUBCOMMAND>`.
#[derive(Debug, Args)]
#[command(name = "self")]
pub struct SelfArgs {
    /// Operation to perform on the `dq` binary itself.
    #[command(subcommand)]
    pub command: SelfCommand,
}

/// Subcommands of `dq self`.
#[derive(Debug, Subcommand)]
pub enum SelfCommand {
    /// Check whether a newer release is available on GitHub.
    Check,
    /// Download and atomically replace the running binary.
    Update(SelfUpdateArgs),
}

/// Arguments for `dq self update`.
#[derive(Debug, Args)]
pub struct SelfUpdateArgs {
    /// Specific version tag (e.g. `v0.2.0`) to install. Defaults to latest.
    ///
    /// Sideways downgrades are allowed — passing a version older than the
    /// current binary just installs that older release.
    #[arg(long, value_name = "VER")]
    pub to: Option<String>,
}
