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
mod doc;
mod exists;
mod explain;
mod fix;
mod fmt;
mod generate_docs;
mod get;
mod input_format;
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
pub use doc::DocSelector;
pub use exists::ExistsArgs;
pub use explain::ExplainArgs;
pub use fix::FixArgs;
pub use fmt::FmtArgs;
pub use generate_docs::GenerateDocsArgs;
pub use get::GetArgs;
pub use input_format::InputFormat;
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
    /// Output format; never the format a file is read in (see
    /// --input-format).
    ///
    /// convert: the format to convert to. set, del, patch, merge, fmt, fix:
    /// the file keeps its own format, so a -F naming another one is refused
    /// (exit 2) and the same one is accepted. exists, test, completions,
    /// man, self and rules add print no formatted value and do not use it.
    #[arg(short = 'F', long, global = true, value_enum, default_value_t = OutputFormat::Console)]
    pub format: OutputFormat,

    /// The format to read files, globs and stdin (`-`) in, instead of the
    /// one their extension or name implies.
    ///
    /// Stdin has no extension: a command reading `-` needs this flag (exit
    /// 2 without it). test, explain, rules, completions, man and self read
    /// no data file and refuse it (exit 2).
    #[arg(long, global = true, value_enum, value_name = "FORMAT")]
    pub input_format: Option<InputFormat>,

    /// Increase logging verbosity (`-v`, `-vv`, `-vvv`).
    #[arg(short, long, action = clap::ArgAction::Count, global = true, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Suppress all output except errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Disable coloured output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Accepted for compatibility; does nothing — dq has no pager.
    ///
    /// Hidden from `--help` so nobody reaches for it, kept so scripts that
    /// pass it do not break.
    #[arg(long, global = true, hide = true)]
    pub no_pager: bool,

    /// Which document of a multi-document YAML stream (`---`-separated) to
    /// work on: an index counting from 0, or `all`.
    ///
    /// A file that is not a stream is document 0; any other index is out of
    /// range (exit 2).
    ///
    /// get, exists, keys, values, len, type, paths, select, query: read
    /// document 0 unless told otherwise; `--doc all` reads the stream as an
    /// array of its documents (`/1/...` is the second one).
    ///
    /// convert: writes only the chosen document (`--doc all`: every
    /// document); not with -i, which would delete the other documents
    /// along with the source.
    ///
    /// set, del: the pointer is relative to the chosen document. Without
    /// `--doc` a stream is addressed as `/N/...`. `--doc all` is refused.
    ///
    /// lint, check, fix: every document of a stream is linted and fixed as
    /// a document of its own; `--doc N` limits them to one.
    ///
    /// Every other command refuses `--doc` (exit 2).
    #[arg(long, value_name = "N|all", global = true)]
    pub doc: Option<DocSelector>,

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
    /// How a command that reads a file treats its templates: the guard,
    /// unless `--allow-templates` or `--raw-template-strings` picks an
    /// escape hatch (clap keeps the two flags exclusive).
    #[must_use]
    pub fn template_mode(&self) -> dq_core::template_guard::TemplateMode {
        use dq_core::template_guard::TemplateMode;
        if self.raw_template_strings {
            TemplateMode::RawStrings
        } else if self.allow_templates {
            TemplateMode::Allow
        } else {
            TemplateMode::Guard
        }
    }

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
    /// Rejects: `-i` together with `--diff` or `--check`, `--diff` with
    /// `--check`, and `--backup` with `--check` or without `-i`. All
    /// rejections carry the [`InvalidInput`] marker so the exit-code mapper
    /// produces 6. A `-F` naming another format than the file's is refused
    /// per file by [`Cli::ensure_format_kept`].
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
        // `-F` is checked per file, once its format is known: see
        // `Cli::ensure_format_kept`.
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

    /// The usage error (exit 2) for a `--doc` value `command` cannot
    /// honour, built by clap so it reads like every other usage error.
    ///
    /// Returned instead of ignoring the flag: a command that silently reads
    /// document 0, or the whole stream, when asked for document 3 answers a
    /// question nobody asked.
    #[must_use]
    pub fn doc_usage_error(&self, command: &str, why: &str) -> anyhow::Error {
        let shown = self.doc.map_or_else(String::new, |doc| format!(" {doc}"));
        let message = format!("`--doc{shown}` is not supported by `dq {command}`: {why}");
        usage_error(clap::error::ErrorKind::ArgumentConflict, message)
    }

    /// The usage error (exit 2) for `--input-format` on a command that
    /// cannot honour it, worded as [`Cli::doc_usage_error`] words `--doc`'s.
    ///
    /// Returned instead of ignoring the flag, as `--doc` is: a format named
    /// for nothing to read is a mistake worth saying so.
    #[must_use]
    pub fn input_format_usage_error(&self, command: &str, why: &str) -> anyhow::Error {
        let shown = self
            .input_format
            .map_or_else(String::new, |f| format!(" {}", f.parser_name()));
        let message = format!("`--input-format{shown}` is not supported by `dq {command}`: {why}");
        usage_error(clap::error::ErrorKind::ArgumentConflict, message)
    }

    /// Refuse a `-F` that names another format than the one `dq {command}`
    /// writes `file` in — its own, `format`.
    ///
    /// `set`, `del`, `patch`, `merge`, `fmt` and `fix` put a file back in
    /// the format it was read in, whether to the file (`-i`), to stdout or
    /// as a diff; `-F` can only ever be the output format, so one that
    /// differs cannot be honoured, and reading it as the input format —
    /// what it once did — is `--input-format`'s job. The same format, and
    /// `-F` left at its `console` default, are accepted: scripts that
    /// passed `-F yaml -i` on a YAML file keep working.
    ///
    /// # Errors
    ///
    /// A usage error (exit 2) naming `--input-format` when `-F` names a
    /// format other than `format`.
    pub fn ensure_format_kept(
        &self,
        command: &str,
        file: &camino::Utf8Path,
        format: &dyn dq_core::Format,
    ) -> anyhow::Result<()> {
        if self.format == OutputFormat::Console
            || self.format.file_format_name() == Some(format.name())
        {
            return Ok(());
        }
        let shown = clap::ValueEnum::to_possible_value(&self.format)
            .map_or_else(String::new, |v| v.get_name().to_owned());
        let file = if file == "-" { "stdin" } else { file.as_str() };
        Err(usage_error(
            clap::error::ErrorKind::ArgumentConflict,
            format!(
                "-F {shown} is the output format, but `dq {command}` keeps {file} in its own \
                 format, {name}: drop -F (to read a file as another format, use \
                 --input-format; to change its format, use `dq convert`)",
                name = format.name(),
            ),
        ))
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

/// A usage error (exit 2) raised after parsing, built by clap so it reads
/// like the ones clap raises itself: `error: …`, the usage line, the
/// `--help` hint.
#[must_use]
pub fn usage_error(kind: clap::error::ErrorKind, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(<Cli as clap::CommandFactory>::command().error(kind, message))
}

/// Why a command that reads no data file refuses `--doc` and
/// `--input-format`.
const NO_DATA_FILE: &str = "it reads no data file";

/// The usage error (exit 2) for `dq {command}` given stdin (`-`) for each
/// of `inputs` ([`Command::stdin_inputs`], more than one): the first read
/// would drain it, and every other input would read nothing.
#[must_use]
pub fn stdin_read_twice_error(command: &str, inputs: &[&str]) -> anyhow::Error {
    let named = match inputs {
        [first, rest @ ..] if rest.iter().all(|name| name == first) => {
            if inputs.len() == 2 {
                format!("{first} twice")
            } else {
                format!("{first} {} times", inputs.len())
            }
        }
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
        [] => String::new(),
    };
    usage_error(
        clap::error::ErrorKind::ArgumentConflict,
        format!(
            "stdin (`-`) can be read only once, but `dq {command}` is given it for {named}: \
             pass all but one of them as a file"
        ),
    )
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
    /// List the keys of an object, or the indices of an array.
    Keys(KeysArgs),
    /// List the values of an object, or the items of an array.
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
    /// Re-emit FILE through its native writer, dropping comments. JSON gets
    /// a two-space indent (--indent N to change it); TOML is kept as it is
    /// unless --sort-keys.
    Fmt(FmtArgs),
    /// Parse the file and report parse errors.
    Validate(ValidateArgs),
    /// Emit a structural diff between two documents.
    Diff(DiffArgs),
    /// Set a value at a JSON Pointer. Missing intermediate nodes are created
    /// as mappings by default; `--no-create` rejects them with exit 2.
    ///
    /// YAML, JSON and TOML files are edited in place: comments and
    /// formatting stay. XML, HCL, INI, dotenv, CSV/TSV and JSON Lines files are written
    /// whole anew through the format's writer, as `--jq` does: formatting
    /// is not kept, and neither are comments, except XML's.
    Set(SetArgs),
    /// Remove the value at a JSON Pointer. Missing pointer is not silent —
    /// produces a structured Path error and exit 2.
    ///
    /// YAML, JSON and TOML files are edited in place: comments and
    /// formatting stay. XML, HCL, INI, dotenv, CSV/TSV and JSON Lines files are written
    /// whole anew through the format's writer: formatting is not kept,
    /// and neither are comments, except XML's.
    Del(DelArgs),
    /// Apply an RFC 6902 JSON Patch.
    ///
    /// Edits YAML, JSON and TOML in place, comments kept; writes XML, HCL,
    /// INI, dotenv, CSV/TSV and JSON Lines whole anew, formatting and
    /// comments (but XML's) dropped.
    Patch(PatchArgs),
    /// Evaluate a jq expression over the document and emit the result stream.
    Query(QueryArgs),
    /// Apply an RFC 7396 JSON Merge Patch.
    ///
    /// Edits YAML, JSON and TOML in place, comments kept; writes XML, HCL,
    /// INI, dotenv, CSV/TSV and JSON Lines whole anew, formatting and
    /// comments (but XML's) dropped.
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
    /// Run rule fixture tests under a directory or of `@std/<ns>` (M8).
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

impl Command {
    /// Why this command cannot take `--doc` at all, or `None` when it
    /// honours the flag (see [`Cli::doc`] for what each of them does with
    /// it). The dispatcher turns a `Some` into [`Cli::doc_usage_error`].
    #[must_use]
    pub fn doc_unsupported_reason(&self) -> Option<&'static str> {
        match self {
            Self::Get(_)
            | Self::Exists(_)
            | Self::Keys(_)
            | Self::Values(_)
            | Self::Len(_)
            | Self::Type(_)
            | Self::Paths(_)
            | Self::Select(_)
            | Self::Query(_)
            | Self::Convert(_)
            | Self::Set(_)
            | Self::Del(_)
            | Self::Lint(_)
            | Self::Check(_)
            | Self::Fix(_) => None,
            Self::Fmt(_) => Some("it re-emits the whole file"),
            Self::Validate(_) => Some("it checks that the whole file parses"),
            Self::Diff(_) => Some("it compares two whole files"),
            Self::Patch(_) => Some("address a document in the patch paths as /N/..."),
            Self::Merge(_) => Some("it merges into the whole file"),
            Self::Completions(_)
            | Self::Man(_)
            | Self::Self_(_)
            | Self::Test(_)
            | Self::Explain(_)
            | Self::Rules(_)
            | Self::GenerateDocs(_) => Some(NO_DATA_FILE),
        }
    }

    /// Why this command cannot take `--input-format`, or `None` when it
    /// reads a data file the flag names the format of. The dispatcher turns
    /// a `Some` into [`Cli::input_format_usage_error`].
    ///
    /// `test` reads the inputs of rule fixtures, but each fixture case
    /// names its own format (or takes the rule's): one flag for every case
    /// of every fixture would override what they say.
    #[must_use]
    pub fn input_format_unsupported_reason(&self) -> Option<&'static str> {
        match self {
            Self::Completions(_)
            | Self::Man(_)
            | Self::Self_(_)
            | Self::Test(_)
            | Self::Explain(_)
            | Self::Rules(_)
            | Self::GenerateDocs(_) => Some(NO_DATA_FILE),
            Self::Get(_)
            | Self::Exists(_)
            | Self::Keys(_)
            | Self::Values(_)
            | Self::Len(_)
            | Self::Type(_)
            | Self::Paths(_)
            | Self::Select(_)
            | Self::Convert(_)
            | Self::Fmt(_)
            | Self::Validate(_)
            | Self::Diff(_)
            | Self::Set(_)
            | Self::Del(_)
            | Self::Patch(_)
            | Self::Query(_)
            | Self::Merge(_)
            | Self::Lint(_)
            | Self::Check(_)
            | Self::Fix(_) => None,
        }
    }

    /// The inputs this command is asked to read from stdin, named as its
    /// usage line names them — one entry per `-` (`FILES` once for every
    /// `-` among them). The dispatcher refuses more than one
    /// ([`stdin_read_twice_error`]): stdin can be read only once.
    #[must_use]
    pub fn stdin_inputs(&self) -> Vec<&'static str> {
        let dash = |p: &camino::Utf8Path| p == "-";
        let mut inputs = Vec::new();
        let mut file = |p: &camino::Utf8Path, name: &'static str| {
            if dash(p) {
                inputs.push(name);
            }
        };
        match self {
            Self::Get(GetArgs { file: f, .. })
            | Self::Exists(ExistsArgs { file: f, .. })
            | Self::Keys(KeysArgs { file: f, .. })
            | Self::Values(ValuesArgs { file: f, .. })
            | Self::Len(LenArgs { file: f, .. })
            | Self::Type(TypeArgs { file: f, .. })
            | Self::Paths(PathsArgs { file: f, .. })
            | Self::Select(SelectArgs { file: f, .. })
            | Self::Query(QueryArgs { file: f, .. })
            | Self::Convert(ConvertArgs { file: f, .. })
            | Self::Fmt(FmtArgs { file: f, .. })
            | Self::Validate(ValidateArgs { file: f, .. })
            | Self::Del(DelArgs { file: f, .. }) => file(f, "FILE"),
            Self::Diff(args) => {
                file(&args.a, "A");
                file(&args.b, "B");
            }
            Self::Set(args) => {
                file(&args.file, "FILE");
                // `@-` names the file `-`, which is stdin.
                if matches!(args.value.as_deref(), Some("-" | "@-")) {
                    inputs.push("VALUE");
                }
                if args.value_from.as_deref().is_some_and(dash) {
                    inputs.push("--value-from");
                }
            }
            Self::Patch(args) => {
                file(&args.file, "FILE");
                if args.ops.as_deref() == Some("-") {
                    inputs.push("OPS");
                }
            }
            Self::Merge(args) => {
                file(&args.file, "FILE");
                if args.patch.as_deref() == Some("-") {
                    inputs.push("PATCH");
                }
            }
            Self::Lint(LintArgs { files, .. })
            | Self::Check(CheckArgs { files, .. })
            | Self::Fix(FixArgs { files, .. }) => {
                for f in files {
                    file(f, "FILES");
                }
            }
            Self::Completions(_)
            | Self::Man(_)
            | Self::Self_(_)
            | Self::Test(_)
            | Self::Explain(_)
            | Self::Rules(_)
            | Self::GenerateDocs(_) => {}
        }
        inputs
    }

    /// The command's name as typed on the command line.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Get(_) => "get",
            Self::Exists(_) => "exists",
            Self::Keys(_) => "keys",
            Self::Values(_) => "values",
            Self::Len(_) => "len",
            Self::Type(_) => "type",
            Self::Paths(_) => "paths",
            Self::Select(_) => "select",
            Self::Convert(_) => "convert",
            Self::Fmt(_) => "fmt",
            Self::Validate(_) => "validate",
            Self::Diff(_) => "diff",
            Self::Set(_) => "set",
            Self::Del(_) => "del",
            Self::Patch(_) => "patch",
            Self::Query(_) => "query",
            Self::Merge(_) => "merge",
            Self::Completions(_) => "completions",
            Self::Man(_) => "man",
            Self::Self_(_) => "self",
            Self::Lint(_) => "lint",
            Self::Check(_) => "check",
            Self::Test(_) => "test",
            Self::Explain(_) => "explain",
            Self::Rules(_) => "rules",
            Self::Fix(_) => "fix",
            Self::GenerateDocs(_) => "generate-docs",
        }
    }
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
    fn ensure_write_flags_consistent_leaves_dash_f_to_the_per_file_check() {
        // `-F` with `-i` is no flag conflict: whether it is refused depends
        // on the file's format, which `ensure_format_kept` sees.
        let cli =
            Cli::try_parse_from(["dq", "-i", "-F", "json", "set", "f.yaml", "/x", "1"]).unwrap();
        cli.ensure_write_flags_consistent().unwrap();
    }

    #[test]
    fn ensure_format_kept_refuses_another_format_and_accepts_the_same() {
        let yaml = dq_core::by_name("yaml").unwrap();
        let file = camino::Utf8Path::new("f.yaml");
        let cli =
            Cli::try_parse_from(["dq", "-i", "-F", "json", "set", "f.yaml", "/x", "1"]).unwrap();
        let err = cli.ensure_format_kept("set", file, yaml).unwrap_err();
        assert_eq!(crate::exit_code::exit_code_for_error(&err), 2, "{err}");
        assert!(err.to_string().contains("--input-format"), "{err}");

        let same =
            Cli::try_parse_from(["dq", "-i", "-F", "yaml", "set", "f.yaml", "/x", "1"]).unwrap();
        same.ensure_format_kept("set", file, yaml).unwrap();
        let unset = Cli::try_parse_from(["dq", "-i", "set", "f.yaml", "/x", "1"]).unwrap();
        unset.ensure_format_kept("set", file, yaml).unwrap();
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

    #[test]
    fn stdin_inputs_name_every_input_given_dash() {
        let cli = Cli::try_parse_from(["dq", "lint", "-", "a.json", "-", "-"]).unwrap();
        assert_eq!(cli.command.stdin_inputs(), ["FILES"; 3]);
        let err = stdin_read_twice_error("lint", &cli.command.stdin_inputs());
        assert!(err.to_string().contains("for FILES 3 times"), "{err}");

        let cli = Cli::try_parse_from(["dq", "set", "-", "/a", "@-"]).unwrap();
        assert_eq!(cli.command.stdin_inputs(), ["FILE", "VALUE"]);
        let err = stdin_read_twice_error("x", &["A", "B", "C"]);
        assert!(err.to_string().contains("for A, B and C"), "{err}");
        assert_eq!(crate::exit_code::exit_code_for_error(&err), 2);

        let cli = Cli::try_parse_from(["dq", "get", "x.json", "/a"]).unwrap();
        assert!(cli.command.stdin_inputs().is_empty());
    }
}
