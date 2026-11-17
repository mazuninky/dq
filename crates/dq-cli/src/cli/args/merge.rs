//! `dq merge FILE [PATCH]` — apply an RFC 7396 JSON Merge Patch.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq merge`.
#[derive(Debug, Args, Clone)]
pub struct MergeArgs {
    /// File (or glob pattern) to mutate. Format is detected from extension or `-F`.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// Inline JSON merge patch, `-` for stdin, or `@<path>` for a file.
    /// Mutually exclusive with `--patch-from`.
    #[arg(conflicts_with = "patch_from")]
    pub patch: Option<String>,

    /// Read the JSON merge patch from a file (alternative to inline / stdin / `@<path>`).
    #[arg(long = "patch-from", value_parser = clap::value_parser!(Utf8PathBuf))]
    pub patch_from: Option<Utf8PathBuf>,
}
