//! `dq len FILE POINTER` — print the length of the addressed array/string/object.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq len`.
#[derive(Debug, Args)]
pub struct LenArgs {
    /// File to read.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer to a sized node.
    pub pointer: String,
}
