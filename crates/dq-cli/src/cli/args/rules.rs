//! `dq rules` — manage rule sources (`list`, `add`).

use clap::{Args, Subcommand};

/// Arguments for `dq rules`.
#[derive(Debug, Args)]
pub struct RulesArgs {
    /// Operation to perform on rule sources.
    #[command(subcommand)]
    pub command: RulesCommand,
}

/// Subcommands of `dq rules`.
#[derive(Debug, Subcommand)]
pub enum RulesCommand {
    /// List available rulesets and rule ids.
    List(RulesListArgs),
    /// Materialise an `@std/<ns>` ruleset under `./.dq/rules/`.
    Add(RulesAddArgs),
}

/// Arguments for `dq rules list`.
#[derive(Debug, Args)]
pub struct RulesListArgs {
    /// Filter by namespace (e.g. `@std/k8s` or `k8s`).
    #[arg(long)]
    pub namespace: Option<String>,
}

/// Arguments for `dq rules add`.
#[derive(Debug, Args)]
pub struct RulesAddArgs {
    /// `@std/<ns>` or path to a rule file/directory.
    pub ruleset: String,

    /// Overwrite existing destination files.
    #[arg(long)]
    pub force: bool,

    /// Symlink instead of copy (Unix only — falls back to copy on Windows).
    #[arg(long)]
    pub symlink: bool,
}
