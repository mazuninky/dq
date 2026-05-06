//! Per-subcommand handler modules.
//!
//! Each module exposes a `pub fn run(args, reporter, out) -> anyhow::Result<()>`
//! that performs the actual work for one subcommand. In Section 4 these are
//! stubs that bail with "not yet implemented (Section 5)" — the dispatcher in
//! `lib::run` already routes every CLI variant to the right module so the
//! binary builds and `--help` works end-to-end.

pub mod check;
pub mod completions;
pub mod convert;
pub mod del;
pub mod diff;
pub mod exists;
pub mod explain;
pub mod fix;
pub mod fmt;
pub mod generate_docs;
pub mod get;
pub mod io_helpers;
pub mod keys;
pub mod len;
pub mod lint;
pub mod lint_core;
pub mod man;
pub mod merge;
pub mod patch;
pub mod paths;
pub mod query;
pub mod rules;
pub mod select;
pub mod self_cmd;
pub mod set;
pub mod test_cmd;
pub mod type_cmd;
pub mod validate;
pub mod values;
