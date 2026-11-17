//! `dq fmt FILE` — re-emit the file through its native writer.
//!
//! No fmt-specific flags. The behaviour is controlled entirely by global
//! flags: `-i`/`--diff`/`--check`/`--backup` for the output mode (shared
//! with `set`/`del`/`patch`/`merge` via [`crate::cli::Cli`]); and the M4
//! re-emit knobs `--sort-keys` / `--indent` (also global). The handler
//! routes through the format's [`dq_core::Format::write_with_options`]
//! method to pick up those knobs.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq fmt`.
#[derive(Debug, Args)]
pub struct FmtArgs {
    /// File or glob pattern to format.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,
}
