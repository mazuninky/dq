//! `dq validate FILE` — exit 0 if the file parses, exit 4 otherwise.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq validate`.
#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// File to validate.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,
}
