//! `dq patch FILE [OPS]` — apply an RFC 6902 JSON Patch.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq patch`.
#[derive(Debug, Args, Clone)]
pub struct PatchArgs {
    /// File (or glob pattern) to mutate. Format is detected from extension or `-F`.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// Inline JSON Patch (an array of ops), `-` for stdin, or `@<path>` for a file.
    /// Mutually exclusive with `--ops-from`.
    #[arg(conflicts_with = "ops_from")]
    pub ops: Option<String>,

    /// Read the JSON Patch from a file (alternative to inline / stdin / `@<path>`).
    #[arg(long = "ops-from", value_parser = clap::value_parser!(Utf8PathBuf))]
    pub ops_from: Option<Utf8PathBuf>,

    /// Parse `ops` (or stdin/file) as the simplified line format
    /// `<op> <pointer> [json-value]` — one op per line, instead of a JSON array.
    #[arg(long = "line-format")]
    pub line_format: bool,

    /// Reject the operation if any intermediate node along an `add`/`replace`
    /// pointer is missing. Equivalent to set's `--no-create`.
    #[arg(long = "no-create")]
    pub no_create: bool,
}
