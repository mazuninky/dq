//! `dq completions <shell>` — emit a shell completion script to stdout.
//!
//! Distinct from the hidden `dq generate-docs --output-dir DIR` (which writes
//! a directory tree of files for packaging scripts). `completions` is the
//! documented end-user entry point — `dq completions zsh > ~/.zsh/completions/_dq`
//! drops one script for one shell to one place, no extra plumbing.

use clap::Args;
use clap_complete::Shell;

/// Arguments for `dq completions <shell>`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for (`bash`, `zsh`, `fish`, `powershell`, `elvish`).
    #[arg(value_enum)]
    pub shell: Shell,
}
