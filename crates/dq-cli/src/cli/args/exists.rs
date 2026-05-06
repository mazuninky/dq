//! `dq exists FILE POINTER` — exit 0 if the pointer addresses something, exit 1 otherwise.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq exists`.
#[derive(Debug, Args)]
pub struct ExistsArgs {
    /// File to read.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer to test.
    pub pointer: String,
}
