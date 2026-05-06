//! `dq diff A B` — emit a structural diff between two documents.
//!
//! `diff` is read-only: it does not accept the global write-mode flags
//! (`-i`/`--diff`/`--backup`/`--check`). The handler in
//! [`crate::commands::diff`] gates on [`crate::cli::Cli::ensure_no_write_flags`]
//! before doing any I/O.
//!
//! Naming-conflict note: `Command::Diff` (this subcommand) shares the word
//! "diff" with the global `--diff` flag (write-mode used by `set`/`del`). Clap
//! disambiguates by position — `dq diff a.yaml b.yaml` parses as the
//! subcommand, while `dq set f.yaml /x 1 --diff` parses the same word as the
//! flag. The unit test in [`crate::commands::diff`] pins the contract.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq diff`.
#[derive(Debug, Args, Clone)]
pub struct DiffArgs {
    /// First file (the "before" document).
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub a: Utf8PathBuf,

    /// Second file (the "after" document).
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub b: Utf8PathBuf,

    /// Emit a textual unified diff over the rendered representations
    /// instead of an RFC 6902 JSON Patch.
    #[arg(long = "unified")]
    pub unified: bool,
}
