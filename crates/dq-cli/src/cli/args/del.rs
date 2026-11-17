//! `dq del FILE POINTER` — remove the value at a JSON Pointer.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq del`.
#[derive(Debug, Args)]
pub struct DelArgs {
    /// File to mutate.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer (RFC 6901).
    pub pointer: String,
}
