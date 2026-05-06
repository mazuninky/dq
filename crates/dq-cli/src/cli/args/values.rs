//! `dq values FILE POINTER` — print the values of the addressed object.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq values`.
#[derive(Debug, Args)]
pub struct ValuesArgs {
    /// File to read.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer to an object node.
    pub pointer: String,
}
