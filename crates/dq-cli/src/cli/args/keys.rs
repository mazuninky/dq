//! `dq keys FILE POINTER` — print the keys of the addressed object.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq keys`.
#[derive(Debug, Args)]
pub struct KeysArgs {
    /// File to read.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer to an object node.
    pub pointer: String,
}
