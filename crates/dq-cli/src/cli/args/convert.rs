//! `dq convert FILE` — re-emit the file in the format selected by `-F`.
//!
//! `--keep-source` and `--overwrite` are about `-i` (in-place format
//! conversion), which writes a file next to the source and then removes the
//! source: the one keeps the source, the other allows replacing a file that
//! is already where the new one goes.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq convert`.
#[derive(Debug, Args, Clone)]
pub struct ConvertArgs {
    /// File to convert. Output format is the global `-F`.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// With `-i`: keep the source file alongside the converted output
    /// instead of removing it. No effect without `-i`.
    #[arg(long = "keep-source")]
    pub keep_source: bool,

    /// With `-i`: replace a file that already exists at the target path.
    /// Without it (or `--backup`, which keeps a copy of that file as
    /// `<file>.bak`) such a target is refused and nothing is converted.
    /// Requires `-i`.
    #[arg(long = "overwrite")]
    pub overwrite: bool,
}
