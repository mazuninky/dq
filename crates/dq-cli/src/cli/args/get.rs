//! `dq get FILE POINTER` — print the value addressed by the pointer.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq get`.
#[derive(Debug, Args)]
pub struct GetArgs {
    /// File to read (format detected by extension or `-F`).
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer (RFC 6901) — empty string addresses the root.
    pub pointer: String,
}
