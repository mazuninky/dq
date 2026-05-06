//! `dq query EXPR FILE` — evaluate a jq expression over the document.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq query`.
///
/// Mirrors the shape of `jq EXPR FILE` so users with `jq` muscle memory can
/// reach for `dq query` without re-learning the argument order. The expression
/// is the jaq dialect — every `jq` filter from the `jaq-core` / `jaq-std` /
/// `jaq-json` definition sets is accepted; advanced surface like `--arg`,
/// `--argjson`, and `--slurpfile` is intentionally out of scope for M7.
#[derive(Debug, Args)]
pub struct QueryArgs {
    /// jq expression (jaq dialect — `jq -h` syntax minus `--arg`/`--slurpfile`).
    pub expression: String,

    /// File to query (or `-` for stdin, requiring `-F`).
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,
}
