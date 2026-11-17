//! `dq man [PAGE]` — emit a troff man page to stdout.
//!
//! With no `PAGE` argument, the top-level `dq.1` man page is rendered. With
//! `dq man get`, the `dq-get.1` page is rendered. Lets users pipe straight
//! into `man -l -` without installing anything system-wide.

use clap::Args;

/// Arguments for `dq man [PAGE]`.
#[derive(Debug, Args)]
pub struct ManArgs {
    /// Subcommand to render the man page for. Omit for the top-level `dq.1`.
    pub page: Option<String>,
}
