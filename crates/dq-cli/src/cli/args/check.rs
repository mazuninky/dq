//! `dq check --rule RULE FILE...` — run a single rule against files.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq check`.
///
/// Either `--rule <id-or-path>` or `--inline <yaml>` must be provided —
/// the handler errors with `InvalidInput` otherwise. Mixing the two is
/// rejected by clap (`conflicts_with`).
///
/// The rule input is a flag rather than a positional argument so the
/// trailing `<files>...` positional remains unambiguous and required.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Rule path or fully-qualified id (e.g. `k8s.no-latest-tag`).
    ///
    /// Mutually exclusive with `--inline`.
    #[arg(long)]
    pub rule: Option<String>,

    /// Inline rule YAML, mutually exclusive with `--rule`.
    #[arg(long, conflicts_with = "rule")]
    pub inline: Option<String>,

    /// File paths or glob patterns to check. At least one required.
    #[arg(required = true, value_parser = clap::value_parser!(Utf8PathBuf))]
    pub files: Vec<Utf8PathBuf>,
}
