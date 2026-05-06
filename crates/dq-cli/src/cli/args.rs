//! `Cli` and the `Command` enum + a thin re-export of every per-command args struct.
//!
//! Conventions:
//!
//! - Globals live on `Cli` with `global = true` so they apply to every subcommand.
//! - Each `*Args` lives in its own file under `args/` for parallelism with future
//!   commands and to keep diffs surgical.
//! - Write-mode flags (`-i`, `--diff`, `--backup`) parse globally and are only
//!   meaningful for write subcommands (`set`, `del`). Read subcommands gate
//!   themselves with [`Cli::ensure_no_write_flags`], rejecting any write flag
//!   with a structured `InvalidInput` so the exit-code mapper produces 6.

use clap::{Parser, Subcommand};

use crate::error::InvalidInput;
use crate::output::OutputFormat;

mod check;
mod completions;
mod convert;
mod del;
mod diff;
mod exists;
mod explain;
mod fix;
mod fmt;
mod generate_docs;
mod get;
mod keys;
mod len;
mod lint;
mod man;
mod merge;
mod patch;
mod paths;
mod query;
mod rules;
mod select;
mod self_cmd;
mod set;
mod test_cmd;
mod type_args;
mod validate;
mod values;

pub use check::CheckArgs;
pub use completions::CompletionsArgs;
pub use convert::ConvertArgs;
pub use del::DelArgs;
pub use diff::DiffArgs;
pub use exists::ExistsArgs;
pub use explain::ExplainArgs;
pub use fix::FixArgs;
pub use fmt::FmtArgs;
pub use generate_docs::GenerateDocsArgs;
pub use get::GetArgs;
pub use keys::KeysArgs;
pub use len::LenArgs;
pub use lint::LintArgs;
pub use man::ManArgs;
pub use merge::MergeArgs;
pub use patch::PatchArgs;
pub use paths::PathsArgs;
pub use query::QueryArgs;
pub use rules::{RulesAddArgs, RulesArgs, RulesCommand, RulesListArgs};
pub use select::SelectArgs;
pub use self_cmd::{SelfArgs, SelfCommand, SelfUpdateArgs};
pub use set::SetArgs;
pub use test_cmd::TestArgs;
pub use type_args::TypeArgs;
pub use validate::ValidateArgs;
pub use values::ValuesArgs;

/// `dq` — query and convert structured data files.
#[derive(Debug, Parser)]
#[command(name = "dq")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Output format.
    #[arg(short = 'F', long, global = true, value_enum, default_value_t = OutputFormat::Console)]
    pub format: OutputFormat,

    /// Increase logging verbosity (`-v`, `-vv`, `-vvv`).
    #[arg(short, long, action = clap::ArgAction::Count, global = true, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Suppress all output except errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Disable coloured output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Disable automatic paging of long output.
    #[arg(long, global = true)]
    pub no_pager: bool,

    /// Document index for multi-document streams ("0", "1", ...) or "all".
    ///
    /// Parsed in M1 but not yet wired into command handlers; semantics arrive
    /// in M2.
    #[arg(long, value_name = "IDX", global = true)]
    pub doc: Option<String>,

    /// Edit the file in place. Only meaningful for write subcommands
    /// (`set`, `del`); read subcommands reject this flag.
    #[arg(short = 'i', long, global = true)]
    pub in_place: bool,

    /// Show a diff of the change instead of writing. Only meaningful for
    /// write subcommands (`set`, `del`); read subcommands reject this flag.
    #[arg(long, global = true)]
    pub diff: bool,

    /// Create a backup before write. Only meaningful for write subcommands
    /// (`set`, `del`); read subcommands reject this flag.
    #[arg(long, global = true)]
    pub backup: bool,

    /// Allow parsing files containing Go template syntax (Helm/Argo/GitHub
    /// Actions). The parser will likely fail on `{{ }}` constructs unless the
    /// file happens to also be valid YAML/TOML/JSON; use this only when you
    /// know the format will round-trip in spite of the templates.
    #[arg(
        long = "allow-templates",
        global = true,
        conflicts_with = "raw_template_strings"
    )]
    pub allow_templates: bool,

    /// Substitute `{{ ... }}` template blocks with opaque placeholder strings
    /// before parsing, then restore them on output. Lets `dq set` / `dq del`
    /// modify Helm values files without breaking the templates.
    #[arg(long = "raw-template-strings", global = true)]
    pub raw_template_strings: bool,

    /// In bulk mode (glob matches multiple files), do not abort on the first
    /// failing file. Collect failures into the summary; return exit 7 if any
    /// failed. No-op when the glob matches a single file.
    #[arg(long = "continue-on-error", global = true)]
    pub continue_on_error: bool,

    /// Number of parallel workers for bulk write operations. `0` uses
    /// `rayon::current_num_threads()`. Default is sequential (`1`).
    #[arg(long = "parallel", global = true, value_name = "N")]
    pub parallel: Option<usize>,

    /// Idempotency gate: exit 1 if any matched file would be modified, do
    /// not write. Mutually exclusive with `-i`, `--diff`, and `--backup`.
    #[arg(long = "check", global = true)]
    pub check: bool,

    /// Sort map keys alphabetically when re-emitting (no-op for textual-edit
    /// splice paths like `set`/`del`/`patch`/`merge`; use `dq fmt` to
    /// canonicalize after such edits).
    #[arg(long = "sort-keys", global = true)]
    pub sort_keys: bool,

    /// Indentation width for indented formats (json/jsonl honour the value;
    /// yaml/toml ignore it for the M4 baseline).
    #[arg(long = "indent", global = true, value_name = "N")]
    pub indent: Option<u8>,

    /// Treat warn-severity violations as errors when computing the lint
    /// exit code. Read commands silently ignore this flag — it only affects
    /// `lint` / `check` exit-code computation.
    #[arg(long = "strict", global = true)]
    pub strict: bool,

    /// Directory containing `*.wasm` plugins to load alongside `@std/*` rules
    /// and `./.dq/rules/`. Plugins are loaded in lexical order by file name.
    /// Without `--features plugins`, using this flag with at least one
    /// resolved `*.wasm` file errors with exit code 6.
    #[arg(long = "plugins", global = true, value_name = "DIR")]
    pub plugins: Option<camino::Utf8PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Reject any of the write-mode flags (`-i`/`--in-place`, `--diff`,
    /// `--backup`) — used by **read** subcommands to signal "this verb does
    /// not write, please drop the flag".
    ///
    /// Called from each read handler's first line. Write subcommands (`set`,
    /// `del`, added in M2 §9/§10) deliberately do *not* call this — they use
    /// the flags for their normal operation.
    ///
    /// # Errors
    ///
    /// Returns an [`InvalidInput`] error (mapped to exit code 6 via
    /// [`crate::exit_code::exit_code_for_error`]) whose message names every
    /// offending flag in a single line, so users do not have to retry to
    /// discover additional rejections.
    pub fn ensure_no_write_flags(&self) -> anyhow::Result<()> {
        let mut offenders: Vec<&'static str> = Vec::new();
        if self.in_place {
            offenders.push("--in-place");
        }
        if self.diff {
            offenders.push("--diff");
        }
        if self.backup {
            offenders.push("--backup");
        }
        if self.check {
            offenders.push("--check");
        }
        if self.continue_on_error {
            offenders.push("--continue-on-error");
        }
        // Read commands have no parallel work to do; reject any value of
        // `--parallel` (including `0`) so users notice the typo instead of
        // having it silently ignored.
        if self.parallel.is_some() {
            offenders.push("--parallel");
        }
        if offenders.is_empty() {
            return Ok(());
        }
        Err(anyhow::Error::new(InvalidInput::new(format!(
            "write-mode flag {} is not accepted by this read-only subcommand (use `set`/`del` for in-place edits)",
            offenders.join(", "),
        ))))
    }

    /// Validate write-mode flag combinations for `set`/`del` handlers.
    ///
    /// Rejects: `-i` together with `--diff`, `--backup` without `-i`, and
    /// `-i` together with `-F` (in-place format change is deferred to M3).
    /// All rejections carry the [`InvalidInput`] marker so the exit-code
    /// mapper produces 6.
    ///
    /// # Errors
    ///
    /// Returns an [`InvalidInput`] error for any of the rejected
    /// combinations above.
    pub fn ensure_write_flags_consistent(&self) -> anyhow::Result<()> {
        if self.in_place && self.diff {
            return Err(anyhow::Error::new(InvalidInput::new(
                "-i/--in-place and --diff are mutually exclusive output modes",
            )));
        }
        // M3 §3 — `--check` is the third output mode and must not be paired
        // with `-i` (no write happens), `--diff` (diff vs check are
        // different intents), or `--backup` (no write to back up).
        if self.in_place && self.check {
            return Err(anyhow::Error::new(InvalidInput::new(
                "-i/--in-place and --check are mutually exclusive output modes",
            )));
        }
        if self.diff && self.check {
            return Err(anyhow::Error::new(InvalidInput::new(
                "--diff and --check are mutually exclusive output modes",
            )));
        }
        if self.backup && self.check {
            return Err(anyhow::Error::new(InvalidInput::new(
                "--backup cannot be combined with --check (no write happens)",
            )));
        }
        if self.backup && !self.in_place {
            return Err(anyhow::Error::new(InvalidInput::new(
                "--backup requires -i/--in-place",
            )));
        }
        // M2 baseline: -i + -F is rejected for `set`/`del`. The format is
        // implied by the file extension when writing back. The M3 `convert`
        // handler (§8) needs `-i + -F` to express in-place format conversion;
        // its handler will perform its own targeted validation rather than
        // calling this method, so this rule stays as-is for set/del.
        // `OutputFormat::default() == Console`, so anything else means the
        // user explicitly passed `-F`.
        if self.in_place && self.format != OutputFormat::Console {
            return Err(anyhow::Error::new(InvalidInput::new(
                "-i/--in-place cannot be combined with -F (format is implied by file extension)",
            )));
        }
        // NOTE: `--parallel > 1` without a glob (single-file invocation) is
        // a runtime decision in `bulk::run_per_file` — the validator does
        // not have access to the matched-file count at this point. The
        // driver downgrades silently in that case (parallel of 1 file is
        // identical to sequential).
        Ok(())
    }

    /// Reject every write-mode flag except `--check`.
    ///
    /// Used by `validate` (M4 §5): the validate verb is read-only, so
    /// `-i`/`--diff`/`--backup` etc. still make no sense, but `--check` is
    /// reinterpreted as "exit 4 on parse failure, exit 0 otherwise" — which
    /// is exactly what plain `validate` already does. Tolerating `--check`
    /// here lets users compose `dq validate --check` into the same CI
    /// pipelines that run `dq fmt --check` and `dq set --check` without a
    /// flag-shuffling dance.
    ///
    /// # Errors
    ///
    /// Returns an [`InvalidInput`] error for any rejected flag — same
    /// shape as [`Cli::ensure_no_write_flags`], minus the `--check` entry.
    pub fn ensure_no_write_flags_except_check(&self) -> anyhow::Result<()> {
        let mut offenders: Vec<&'static str> = Vec::new();
        if self.in_place {
            offenders.push("--in-place");
        }
        if self.diff {
            offenders.push("--diff");
        }
        if self.backup {
            offenders.push("--backup");
        }
        if self.continue_on_error {
            offenders.push("--continue-on-error");
        }
        if self.parallel.is_some() {
            offenders.push("--parallel");
        }
        if offenders.is_empty() {
            return Ok(());
        }
        Err(anyhow::Error::new(InvalidInput::new(format!(
            "write-mode flag {} is not accepted by `validate` (only --check is honoured)",
            offenders.join(", "),
        ))))
    }

    /// Build a [`dq_core::WriteOptions`] snapshot from the current global
    /// flags.
    ///
    /// Uses mutate-via-default rather than struct-update syntax because
    /// [`dq_core::WriteOptions`] is `#[non_exhaustive]` and `..Default::default()`
    /// across crate boundaries fails to compile. New M5+ knobs land here as
    /// extra `opts.field = ...` lines.
    #[must_use]
    pub fn write_options(&self) -> dq_core::WriteOptions {
        let mut opts = dq_core::WriteOptions::default();
        opts.sort_keys = self.sort_keys;
        opts.indent = self.indent;
        opts
    }
}

/// Subcommand surface for M1.
///
/// Variants beyond M1 (e.g. `set`, `del`, `lint`) are deliberately omitted —
/// the CLI rejects them via clap's standard "unknown subcommand" error,
/// which is what the spec calls out.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Get the value at a JSON Pointer.
    Get(GetArgs),
    /// Test whether a JSON Pointer addresses an existing node (silent on success).
    Exists(ExistsArgs),
    /// List the keys of an object.
    Keys(KeysArgs),
    /// List the values of an object.
    Values(ValuesArgs),
    /// Print the length of an array, string, or object.
    Len(LenArgs),
    /// Print the type name of the addressed node.
    Type(TypeArgs),
    /// List every JSON Pointer addressable in the document.
    Paths(PathsArgs),
    /// Run a JSONPath expression and print matching values.
    Select(SelectArgs),
    /// Convert between formats.
    Convert(ConvertArgs),
    /// Re-emit the file through its native writer (canonical formatting).
    Fmt(FmtArgs),
    /// Parse the file and report parse errors.
    Validate(ValidateArgs),
    /// Emit a structural diff between two documents.
    Diff(DiffArgs),
    /// Set a value at a JSON Pointer. Intermediate map/array nodes are created
    /// by default; `--no-create` rejects missing intermediates with exit 2.
    Set(SetArgs),
    /// Remove the value at a JSON Pointer. Missing pointer is not silent —
    /// produces a structured Path error and exit 2.
    Del(DelArgs),
    /// Apply an RFC 6902 JSON Patch.
    Patch(PatchArgs),
    /// Evaluate a jq expression over the document and emit the result stream.
    Query(QueryArgs),
    /// Apply an RFC 7396 JSON Merge Patch.
    Merge(MergeArgs),
    /// Emit a shell completion script to stdout.
    Completions(CompletionsArgs),
    /// Emit a troff man page to stdout.
    Man(ManArgs),
    /// Operate on the `dq` binary itself (`check`, `update`).
    ///
    /// Renamed from the natural `Self` to avoid the Rust keyword collision;
    /// clap surfaces it as `dq self` via the `#[command(name = "self")]`
    /// attribute on [`SelfArgs`].
    #[command(name = "self")]
    Self_(SelfArgs),
    /// Lint files with one or more rulesets (M8).
    Lint(LintArgs),
    /// Run a single rule against files (or `--inline` an ad-hoc rule) (M8).
    Check(CheckArgs),
    /// Run rule fixture tests under a directory (M8).
    Test(TestArgs),
    /// Print a rule's description, severity, and references (M8).
    Explain(ExplainArgs),
    /// Manage rule sources — `list`, `add` (M8).
    Rules(RulesArgs),
    /// Apply every applicable rule's `fix.jq` to the given files (M10).
    Fix(FixArgs),
    /// Generate man pages and shell completions (hidden).
    #[command(hide = true)]
    GenerateDocs(GenerateDocsArgs),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_no_write_flags_rejects_in_place_short_form() {
        let cli = Cli::try_parse_from(["dq", "-i", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(err.to_string().contains("--in-place"));
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn ensure_no_write_flags_rejects_in_place_long_form() {
        let cli = Cli::try_parse_from(["dq", "--in-place", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(err.to_string().contains("--in-place"));
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn ensure_no_write_flags_rejects_diff_flag() {
        let cli = Cli::try_parse_from(["dq", "--diff", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(err.to_string().contains("--diff"));
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn ensure_no_write_flags_rejects_backup_flag() {
        let cli = Cli::try_parse_from(["dq", "--backup", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(err.to_string().contains("--backup"));
        assert!(err.to_string().contains("read-only"));
    }

    #[test]
    fn ensure_no_write_flags_lists_every_offender_in_one_message() {
        // When more than one write flag is set the error names all of them
        // joined by `, ` so users do not have to re-run to discover the
        // additional rejections.
        let cli = Cli::try_parse_from(["dq", "-i", "--diff", "--backup", "get", "x.yaml", "/foo"])
            .unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--in-place"), "missing --in-place: {msg}");
        assert!(msg.contains("--diff"), "missing --diff: {msg}");
        assert!(msg.contains("--backup"), "missing --backup: {msg}");
    }

    #[test]
    fn ensure_no_write_flags_returns_invalid_input_marker() {
        // The returned error must carry the `InvalidInput` marker so the
        // exit-code mapper produces 6 (INVALID_INPUT) instead of the
        // GENERIC catch-all (1).
        let cli = Cli::try_parse_from(["dq", "--diff", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker so exit-code mapper produces 6, got: {err:?}",
        );
    }

    #[test]
    fn ensure_no_write_flags_accepts_no_write_flags() {
        let cli = Cli::try_parse_from(["dq", "get", "x.yaml", "/foo"]).unwrap();
        cli.ensure_no_write_flags().unwrap();
    }

    #[test]
    fn quiet_conflicts_with_verbose() {
        let result = Cli::try_parse_from(["dq", "-v", "-q", "get", "x.yaml", "/foo"]);
        assert!(
            result.is_err(),
            "clap should reject conflicting verbose+quiet"
        );
    }

    #[test]
    fn ensure_write_flags_consistent_rejects_in_place_with_diff() {
        // `-i` and `--diff` describe two mutually exclusive output modes —
        // accepting both would put the user in a state where neither stdout
        // nor the file received the canonical result. Reject up front.
        let cli = Cli::try_parse_from(["dq", "-i", "--diff", "set", "f.yaml", "/x", "1"]).unwrap();
        let err = cli.ensure_write_flags_consistent().unwrap_err();
        assert!(
            err.to_string().contains("mutually exclusive"),
            "error should explain the conflict, got: {err}",
        );
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "rejection must carry InvalidInput so exit-code maps to 6, got: {err:?}",
        );
    }

    #[test]
    fn ensure_write_flags_consistent_rejects_backup_without_in_place() {
        // `--backup` only makes sense as a side effect of an in-place write;
        // setting it when we're streaming to stdout would silently allocate
        // a `.bak` even though no replacement happens.
        let cli = Cli::try_parse_from(["dq", "--backup", "set", "f.yaml", "/x", "1"]).unwrap();
        let err = cli.ensure_write_flags_consistent().unwrap_err();
        assert!(
            err.to_string().contains("--backup"),
            "error should mention --backup, got: {err}",
        );
        assert!(
            err.to_string().contains("--in-place") || err.to_string().contains("-i"),
            "error should mention -i/--in-place as the missing dependency, got: {err}",
        );
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_write_flags_consistent_rejects_in_place_with_format_override() {
        // The M2 baseline writes back in the source format (extension-driven).
        // Combining `-i` with `-F` would imply an in-place format conversion,
        // which is M3 territory.
        let cli =
            Cli::try_parse_from(["dq", "-i", "-F", "json", "set", "f.yaml", "/x", "1"]).unwrap();
        let err = cli.ensure_write_flags_consistent().unwrap_err();
        assert!(
            err.to_string().contains("-F") || err.to_string().contains("format"),
            "error should mention -F, got: {err}",
        );
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_write_flags_consistent_accepts_set_only_in_place() {
        let cli = Cli::try_parse_from(["dq", "-i", "set", "f.yaml", "/x", "1"]).unwrap();
        cli.ensure_write_flags_consistent().unwrap();
    }

    #[test]
    fn ensure_write_flags_consistent_accepts_set_only_diff() {
        let cli = Cli::try_parse_from(["dq", "--diff", "set", "f.yaml", "/x", "1"]).unwrap();
        cli.ensure_write_flags_consistent().unwrap();
    }

    #[test]
    fn ensure_write_flags_consistent_rejects_in_place_with_check() {
        // M3 §3: `-i` writes to disk, `--check` is a no-write idempotency
        // gate. Combining them is incoherent.
        let cli = Cli::try_parse_from(["dq", "-i", "--check", "set", "f.yaml", "/x", "1"]).unwrap();
        let err = cli.ensure_write_flags_consistent().unwrap_err();
        assert!(
            err.to_string().contains("--check"),
            "error should mention --check, got: {err}",
        );
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_write_flags_consistent_rejects_diff_with_check() {
        // `--diff` shows the change, `--check` is a yes/no gate — different
        // intents.
        let cli =
            Cli::try_parse_from(["dq", "--diff", "--check", "set", "f.yaml", "/x", "1"]).unwrap();
        let err = cli.ensure_write_flags_consistent().unwrap_err();
        assert!(
            err.to_string().contains("--check"),
            "error should mention --check, got: {err}",
        );
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_write_flags_consistent_rejects_backup_with_check() {
        // No write happens in `--check` mode, so `--backup` would create a
        // `.bak` of nothing.
        let cli =
            Cli::try_parse_from(["dq", "--backup", "--check", "set", "f.yaml", "/x", "1"]).unwrap();
        let err = cli.ensure_write_flags_consistent().unwrap_err();
        assert!(
            err.to_string().contains("--check"),
            "error should mention --check, got: {err}",
        );
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_write_flags_consistent_accepts_check_alone() {
        let cli = Cli::try_parse_from(["dq", "--check", "set", "f.yaml", "/x", "1"]).unwrap();
        cli.ensure_write_flags_consistent().unwrap();
    }

    #[test]
    fn ensure_no_write_flags_rejects_check_flag() {
        // M3 §3: `--check` is a write-mode-adjacent flag and must be
        // rejected by read commands.
        let cli = Cli::try_parse_from(["dq", "--check", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(err.to_string().contains("--check"));
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_no_write_flags_rejects_continue_on_error_flag() {
        let cli =
            Cli::try_parse_from(["dq", "--continue-on-error", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(err.to_string().contains("--continue-on-error"));
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_no_write_flags_rejects_parallel_flag() {
        // Read commands have no parallel work; reject any `--parallel` value.
        let cli = Cli::try_parse_from(["dq", "--parallel", "4", "get", "x.yaml", "/foo"]).unwrap();
        let err = cli.ensure_no_write_flags().unwrap_err();
        assert!(err.to_string().contains("--parallel"));
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn ensure_no_write_flags_accepts_sort_keys_and_indent() {
        // M4 §3: `--sort-keys` and `--indent` are inert for read commands
        // (they only matter on the re-emit path that `dq fmt` and `dq
        // convert` use). Read handlers MUST NOT reject them — silently
        // tolerating the flags lets users compose them into shell aliases
        // (`alias dq='dq --sort-keys'`) without breaking every read verb.
        let cli = Cli::try_parse_from([
            "dq",
            "--sort-keys",
            "--indent",
            "2",
            "get",
            "x.yaml",
            "/foo",
        ])
        .unwrap();
        cli.ensure_no_write_flags()
            .expect("--sort-keys and --indent must be accepted by read commands");
    }

    #[test]
    fn cli_write_options_method_returns_struct_with_flags() {
        // `Cli::write_options()` is the bridge between the global flags and
        // the `dq_core::WriteOptions` consumed by `Format::write_with_options`.
        // It must propagate both `--sort-keys` and `--indent` verbatim.
        let cli =
            Cli::try_parse_from(["dq", "--sort-keys", "--indent", "4", "fmt", "x.yaml"]).unwrap();
        let opts = cli.write_options();
        assert!(opts.sort_keys, "sort_keys must be threaded through");
        assert_eq!(opts.indent, Some(4), "indent must be threaded through");
    }

    #[test]
    fn cli_write_options_default_when_flags_absent() {
        // The complementary case to `cli_write_options_method_returns_struct_with_flags`:
        // when neither flag is set, `write_options()` returns the M2-baseline-
        // compatible defaults (`sort_keys = false`, `indent = None`) so the
        // re-emit path is byte-identical to the legacy `Format::write` method.
        let cli = Cli::try_parse_from(["dq", "fmt", "x.yaml"]).unwrap();
        let opts = cli.write_options();
        assert!(!opts.sort_keys);
        assert!(opts.indent.is_none());
    }
}
