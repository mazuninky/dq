//! `dq paths FILE` — list every JSON Pointer addressable in the document.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq paths`.
#[derive(Debug, Args)]
pub struct PathsArgs {
    /// File to enumerate.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,
}
