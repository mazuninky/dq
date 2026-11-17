//! `dq fix FILE...` — apply every applicable rule's `fix.jq` to the
//! given files.
//!
//! Mirrors [`crate::cli::LintArgs`] in shape but is **read-write**: the
//! command honours the global `-i` / `--diff` / `--check` /
//! `--continue-on-error` / `--parallel` flags via the bulk driver, the
//! same way `dq set` / `dq del` do.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq fix`.
///
/// Fix runs every rule from each `--rules` source over the given files
/// (or glob patterns) and applies the rule's `fix.jq` whole-document
/// transform. When `--rules` is empty, the loader auto-binds every
/// `@std/<namespace>` whose rules apply to at least one of the
/// discovered formats plus `<cwd>/.dq/rules/` if that directory exists
/// (same auto-bind contract as `dq lint`).
#[derive(Debug, Args)]
pub struct FixArgs {
    /// File paths or glob patterns to fix. At least one required.
    #[arg(required = true, value_parser = clap::value_parser!(Utf8PathBuf))]
    pub files: Vec<Utf8PathBuf>,

    /// Rule source(s): `@std/<namespace>`, file path, or directory.
    ///
    /// Repeatable. With no `--rules`, the loader auto-binds every
    /// `@std/*` matching the input file formats plus
    /// `./.dq/rules/*.yml` if the directory exists — same auto-bind
    /// contract as `dq lint`.
    #[arg(long)]
    pub rules: Vec<String>,
}
