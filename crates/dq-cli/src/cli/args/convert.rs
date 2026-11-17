//! `dq convert FILE` — re-emit the file in the format selected by `-F`.
//!
//! M3 §8 adds `--keep-source`, which only takes effect when paired with `-i`
//! (in-place format conversion). With `-i`, the default behaviour is to
//! remove the source file once the converted target has been written
//! atomically; `--keep-source` preserves both files instead.

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
}
