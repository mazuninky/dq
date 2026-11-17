//! `dq select FILE EXPR` — JSONPath query over the document.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq select`.
#[derive(Debug, Args)]
pub struct SelectArgs {
    /// File to query.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSONPath expression (RFC 9535 subset, as supported by `jsonpath-rust`).
    pub jsonpath: String,
}
