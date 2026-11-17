//! `dq test RULES_DIR` — run rule fixture tests under a directory.
//!
//! Module is named `test_cmd` rather than `test` to avoid colliding with the
//! per-module `mod tests` convention (the file would shadow the test module).

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq test`.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// Directory containing rule files and `*.test.yml` fixtures.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub rules_dir: Utf8PathBuf,
}
