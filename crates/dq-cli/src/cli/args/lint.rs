//! `dq lint FILE...` — run lint rules over one or more files.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq lint`.
///
/// Lint runs every rule from each `--rules` source over the given files (or
/// glob patterns). When `--rules` is empty, the loader auto-binds every
/// `@std/<namespace>` whose rules apply to at least one of the discovered
/// formats plus `<cwd>/.dq/rules/` if that directory exists.
#[derive(Debug, Args)]
pub struct LintArgs {
    /// File paths or glob patterns to lint. At least one required.
    #[arg(required = true, value_parser = clap::value_parser!(Utf8PathBuf))]
    pub files: Vec<Utf8PathBuf>,

    /// Rule source(s): `@std/<namespace>`, file path, or directory.
    ///
    /// Repeatable. With no `--rules`, the loader auto-binds every `@std/*`
    /// matching the input file formats plus `./.dq/rules/*.yml` if the
    /// directory exists.
    #[arg(long)]
    pub rules: Vec<String>,
}
