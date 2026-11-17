//! CLI argument types.
//!
//! `Cli` is the top-level clap parser; `Command` is the subcommand enum.
//! Per-subcommand argument structs live under [`args`] in their own files so
//! adding or modifying a single command never forces re-reading every other
//! subcommand's args.

pub mod args;

pub use args::{
    CheckArgs, Cli, Command, CompletionsArgs, ConvertArgs, DelArgs, DiffArgs, ExistsArgs,
    ExplainArgs, FixArgs, FmtArgs, GenerateDocsArgs, GetArgs, KeysArgs, LenArgs, LintArgs, ManArgs,
    MergeArgs, PatchArgs, PathsArgs, QueryArgs, RulesAddArgs, RulesArgs, RulesCommand,
    RulesListArgs, SelectArgs, SelfArgs, SelfCommand, SelfUpdateArgs, SetArgs, TestArgs, TypeArgs,
    ValidateArgs, ValuesArgs,
};
