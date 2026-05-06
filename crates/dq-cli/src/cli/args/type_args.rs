//! `dq type FILE POINTER` — print the type name of the addressed node.
//!
//! Lives in `type_args.rs` instead of `type.rs` so it does not collide with
//! the Rust keyword.

use camino::Utf8PathBuf;
use clap::Args;

/// Arguments for `dq type`.
#[derive(Debug, Args)]
pub struct TypeArgs {
    /// File to read.
    #[arg(value_parser = clap::value_parser!(Utf8PathBuf))]
    pub file: Utf8PathBuf,

    /// JSON Pointer to inspect.
    pub pointer: String,
}
