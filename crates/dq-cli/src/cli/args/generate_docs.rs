//! `dq generate-docs --output-dir DIR` — emit man pages and shell completions.
//!
//! Hidden (`#[command(hide = true)]`) on the parent enum so it does not
//! appear in `--help` for end users.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for the hidden `dq generate-docs` command.
#[derive(Debug, Args)]
pub struct GenerateDocsArgs {
    /// Directory to write `man/` and `completions/` into.
    #[arg(long, value_parser = clap::value_parser!(Utf8PathBuf))]
    pub output_dir: Utf8PathBuf,
}
