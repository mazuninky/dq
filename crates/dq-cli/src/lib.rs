//! `dq` — the library half of the `dq-cli` crate.
//!
//! Exposing a library lets integration tests drive the CLI in-process by
//! calling [`run`] directly — no subprocess spawn, no stdout race, no SIGPIPE
//! quirks. The binary (`src/main.rs`) is a thin wrapper around the same entry
//! point.

pub mod bulk;
pub mod cli;
pub mod commands;
pub mod diff;
pub mod error;
pub mod exit_code;
mod glob;
pub mod output;

pub use cli::{Cli, Command};
pub use error::{
    DocOutOfRange, DocOutOfRangeEverywhere, LintFail, SilentError, ValidateFail, WrongType,
};

use std::io::Write;

use cli::InputFormat;
use dq_core::one_line;
use output::{
    ConsoleReporter, JsonReporter, JsonlReporter, JunitReporter, OutputFormat, Reporter,
    SarifReporter, TapReporter, TomlReporter, ToonReporter, YamlReporter,
};

/// CLI entry point used by both `main.rs` and integration tests.
///
/// Performs reporter selection from `cli.format` + `use_color`, then
/// dispatches into the matching `commands::*::run`. Write-mode flag
/// rejection lives in each read handler's first line via
/// [`Cli::ensure_no_write_flags`] — that placement means future write
/// subcommands (`set`, `del`, M2 §9/§10) can use the same flags without
/// having to disable a global check here.
///
/// I/O goes through the supplied `stdout` and `stderr` writers; tests can
/// pass `&mut Vec<u8>`, the binary passes locked `Stdout`/`Stderr` handles.
///
/// # Errors
///
/// Any error from a dispatched handler — including the per-handler write-
/// flag rejection — is bubbled up unchanged. The caller (`main.rs`) is
/// responsible for mapping the error to an exit code via
/// [`exit_code::exit_code_for_error`].
pub fn run(
    cli: &Cli,
    use_color: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> anyhow::Result<()> {
    let reporter = reporter_for_format(cli.format, use_color);
    dispatch(cli, use_color, reporter.as_ref(), stdout, stderr)
}

fn dispatch(
    cli: &Cli,
    use_color: bool,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    // `-F` is the output format only (the reporter above, or `convert`'s
    // target); the parser comes from `--input-format`, else detection.
    let input_format = cli.input_format.map(InputFormat::parser_name);
    // `--doc` is global, so clap accepts it on every command; refuse it
    // where the command would otherwise ignore it.
    if cli.doc.is_some()
        && let Some(why) = cli.command.doc_unsupported_reason()
    {
        return Err(cli.doc_usage_error(cli.command.name(), why));
    }
    // `--input-format` is global too: refused where there is no data file
    // for it to name the format of.
    if cli.input_format.is_some()
        && let Some(why) = cli.command.input_format_unsupported_reason()
    {
        return Err(cli.input_format_usage_error(cli.command.name(), why));
    }
    // Stdin can be read once: a command given `-` for two inputs would
    // find it empty for the second.
    let stdin_inputs = cli.command.stdin_inputs();
    if stdin_inputs.len() > 1 {
        return Err(cli::stdin_read_twice_error(
            cli.command.name(),
            &stdin_inputs,
        ));
    }
    let doc_arg = cli.doc;
    // M4 §3: snapshot the global re-emit flags into a `WriteOptions` so
    // every handler that touches `Format::write_with_options` shares the
    // same view. Read commands ignore it; `set`/`del`/`patch`/`merge`
    // ignore it (textual-edit splice path doesn't re-emit); `convert` and
    // `fmt` consume it.
    let opts = cli.write_options();
    match &cli.command {
        Command::Get(args) => commands::get::run(cli, args, input_format, doc_arg, reporter, out),
        Command::Exists(args) => commands::exists::run(cli, args, input_format, doc_arg),
        Command::Keys(args) => commands::keys::run(cli, args, input_format, doc_arg, reporter, out),
        Command::Values(args) => {
            commands::values::run(cli, args, input_format, doc_arg, reporter, out)
        }
        Command::Len(args) => commands::len::run(cli, args, input_format, doc_arg, reporter, out),
        Command::Type(args) => {
            commands::type_cmd::run(cli, args, input_format, doc_arg, reporter, out)
        }
        Command::Paths(args) => commands::paths::run(cli, args, input_format, reporter, out),
        Command::Select(args) => commands::select::run(cli, args, input_format, reporter, out),
        // `convert` renders `-F` itself (writers, not reporters), plus
        // `use_color` for its console branch.
        Command::Convert(args) => {
            commands::convert::run(cli, args, input_format, cli.format, use_color, &opts, out)
        }
        // M4 §3: `dq fmt FILE` — re-emit through the format's native
        // writer with the global `--sort-keys` / `--indent` knobs applied.
        Command::Fmt(args) => commands::fmt::run(cli, args, input_format, use_color, &opts, out),
        Command::Validate(args) => commands::validate::run(cli, args, input_format, reporter, err),
        Command::Diff(args) => {
            commands::diff::run(cli, args, input_format, use_color, reporter, out)
        }
        Command::Set(args) => commands::set::run(cli, args, input_format, use_color, out),
        Command::Del(args) => commands::del::run(cli, args, input_format, use_color, out),
        Command::Patch(args) => commands::patch::run(cli, args, input_format, use_color, out),
        Command::Query(args) => {
            commands::query::run(cli, args, input_format, doc_arg, reporter, out)
        }
        Command::Merge(args) => commands::merge::run(cli, args, input_format, use_color, out),
        // M6: read-only doc commands. They reject the global write flags
        // (`-i`/`--diff`/`--backup`/...) up front via the same gate every
        // other read verb uses, then write straight to the supplied stdout.
        Command::Completions(args) => {
            cli.ensure_no_write_flags()?;
            commands::completions::run(args, out)
        }
        Command::Man(args) => {
            cli.ensure_no_write_flags()?;
            commands::man::run(args, out)
        }
        Command::Self_(args) => {
            cli.ensure_no_write_flags()?;
            match &args.command {
                cli::SelfCommand::Check => commands::self_cmd::run_check(out),
                cli::SelfCommand::Update(u) => commands::self_cmd::run_update(u, out),
            }
        }
        // M8 Stage 6: lint engine. The `lint` and `check` handlers route
        // through the shared pipeline in `commands::lint_core` (glob
        // expansion → format detection → ruleset resolution → evaluation
        // → reporter); the other three (`test`, `explain`, `rules`) drive
        // their own logic but reuse the same write-flag rejection and
        // exit-code markers.
        Command::Lint(args) => commands::lint::run(cli, args, input_format, reporter, out),
        Command::Check(args) => commands::check::run(cli, args, input_format, reporter, out),
        Command::Test(args) => {
            cli.ensure_no_write_flags()?;
            commands::test_cmd::run(cli, args, out)
        }
        Command::Explain(args) => {
            cli.ensure_no_write_flags()?;
            commands::explain::run(cli, args, reporter, out)
        }
        Command::Rules(args) => {
            cli.ensure_no_write_flags()?;
            match &args.command {
                cli::RulesCommand::List(list_args) => {
                    commands::rules::run_list(list_args, reporter, out)
                }
                cli::RulesCommand::Add(add_args) => commands::rules::run_add(add_args),
            }
        }
        // M10: `dq fix` is a write-mode command — it honours `-i` /
        // `--diff` / `--check` / `--continue-on-error` / `--parallel`
        // through the same bulk driver as `dq set`. The handler does its
        // own `ensure_write_flags_consistent` check internally.
        Command::Fix(args) => commands::fix::run(cli, args, input_format, use_color, out),
        Command::GenerateDocs(args) => commands::generate_docs::run(args),
    }
}

/// Reporter factory — wires an [`OutputFormat`] selection to a concrete reporter.
///
/// Lives in `lib.rs` (the wiring layer) so command handlers stay
/// dependency-injected (see the add-exec-engine section of
/// `docs/decisions.md`): they receive a
/// `&dyn Reporter` and never decide which concrete implementation to
/// instantiate.
///
/// M5 Stage 3: the six new write-target formats (Hcl / Ini / DotEnv / Csv /
/// Tsv / Frontmatter) are NOT yet supported as Reporter formats for the
/// query commands (`get` / `paths` / `keys` / …). Selecting one via `-F`
/// builds a [`BannedReporter`] that returns a structured error the moment
/// any handler calls `.report(...)`. Convert paths bypass the reporter (see
/// `commands::convert::render_to_format`) and are unaffected.
///
/// M8 Stage 5: the two new diagnostic-only formats (`Junit` / `Tap`) are
/// wired here alongside `Sarif` for the lint engine. Query verbs that
/// select `-F junit` / `-F tap` against non-diagnostic data get the
/// `BannedReporter`-equivalent error from inside the reporter's `report`
/// method (no `{ "diagnostics": [...] }` shape → `InvalidInput`, exit 6).
fn reporter_for_format(format: OutputFormat, use_color: bool) -> Box<dyn Reporter> {
    match format {
        OutputFormat::Console => Box::new(ConsoleReporter::new(use_color)),
        OutputFormat::Json => Box::new(JsonReporter),
        OutputFormat::Yaml => Box::new(YamlReporter),
        OutputFormat::Toml => Box::new(TomlReporter),
        OutputFormat::Jsonl => Box::new(JsonlReporter),
        OutputFormat::Toon => Box::new(ToonReporter),
        OutputFormat::Hcl => Box::new(BannedReporter::new("hcl")),
        OutputFormat::Ini => Box::new(BannedReporter::new("ini")),
        OutputFormat::DotEnv => Box::new(BannedReporter::new("dotenv")),
        OutputFormat::Csv => Box::new(BannedReporter::new("csv")),
        OutputFormat::Tsv => Box::new(BannedReporter::new("tsv")),
        OutputFormat::Frontmatter => Box::new(BannedReporter::new("frontmatter")),
        // M9: markdown is supported as a `convert` write target only — query
        // verbs that select `-F markdown` get the same `BannedReporter` error
        // discipline as `frontmatter` / `hcl` / `csv` / etc.
        OutputFormat::Markdown => Box::new(BannedReporter::new("markdown")),
        // M11: XML is supported as a `convert` write target. Query verbs
        // that select `-F xml` follow the same `BannedReporter` discipline:
        // the conventional-key shape doesn't map cleanly onto the
        // single-value reporter contract (a query result is typically a
        // scalar / array, not a top-level XML document map).
        OutputFormat::Xml => Box::new(BannedReporter::new("xml")),
        // M6 §5: SARIF is wired here for `validate` and the future M8 lint
        // engine. Query verbs that select `-F sarif` get the
        // `BannedReporter`-equivalent error from inside `SarifReporter::report`
        // (no `{ "diagnostics": [...] }` shape → `InvalidInput`).
        OutputFormat::Sarif => Box::new(SarifReporter),
        // M8 §5: Junit / TAP are diagnostic-only output formats consumed by
        // CI test-result aggregators. Same `InvalidInput` discipline as
        // SARIF when fed a non-diagnostic shape.
        OutputFormat::Junit => Box::new(JunitReporter),
        OutputFormat::Tap => Box::new(TapReporter),
    }
}

/// Reporter stub that errors instead of rendering. Used for the M5 Stage 3
/// formats (hcl / ini / dotenv / csv / tsv / frontmatter) which are
/// supported as **write targets** for `dq convert -F <name>` but not yet
/// wired as **read targets** for query commands like `get`, `paths`, `keys`.
///
/// The error is structured (built via [`error::InvalidInput`]) so the
/// exit-code mapper picks 6 (`INVALID_INPUT`) rather than 1, matching the
/// "format selected but unsupported in this context" semantics rather than a
/// generic runtime failure.
#[derive(Debug, Clone)]
struct BannedReporter {
    format_name: &'static str,
}

impl BannedReporter {
    fn new(format_name: &'static str) -> Self {
        Self { format_name }
    }
}

impl Reporter for BannedReporter {
    fn report(&self, _value: &serde_json::Value, _w: &mut dyn Write) -> anyhow::Result<()> {
        Err(anyhow::Error::new(error::InvalidInput::new(format!(
            "output format '{name}' is supported as a `convert` target via `dq convert -F {name}`, but not as a query reporter format in this build",
            name = self.format_name,
        ))))
    }
}

/// Render an `anyhow::Error` to a writer in human-friendly form.
///
/// Behaviour:
/// - For `dq_core::Error::Path` errors, the renderer expands the structured
///   fields (matched prefix, did_you_mean) onto separate lines so the spec's
///   "did_you_mean" affordance is visible to users without `-vv`.
/// - For [`ValidateFail`] errors, the rendering is suppressed because the
///   validate handler has already streamed a structured error through its
///   reporter; printing again would duplicate the message.
/// - For all other errors, the standard anyhow chain (`{:?}`) is written.
///
/// Every line is written escaped as the console reporter escapes text
/// ([`output::console`]): a message echoes what the user typed and what
/// the file holds — a pointer, a `did_you_mean` key, a path — and a
/// control character or a bidi override in it must not reach the terminal
/// raw. The message's own line breaks stay.
///
/// Called from `main.rs` after `run` returns an error; tests can call it
/// directly with a `Vec<u8>` writer to assert on the rendered shape.
pub fn render_error(err: &anyhow::Error, w: &mut dyn Write) -> std::io::Result<()> {
    let mut raw = Vec::new();
    render_error_raw(err, &mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            w.write_all(b"\n")?;
        }
        w.write_all(output::console::escape_control(line).as_bytes())?;
    }
    Ok(())
}

/// [`render_error`] before the escaping.
fn render_error_raw(err: &anyhow::Error, w: &mut dyn Write) -> std::io::Result<()> {
    if err.downcast_ref::<ValidateFail>().is_some() {
        // The validate handler already wrote a structured error through its
        // reporter to the caller-supplied stderr writer. Don't print again.
        return Ok(());
    }
    if let Some(usage) = err.downcast_ref::<clap::Error>() {
        // Rendered the way clap renders its own usage errors: `error: …`,
        // the usage line, the `--help` hint.
        return write!(w, "{usage}");
    }
    if let Some(dq_core::Error::Path {
        pointer,
        matched_prefix,
        did_you_mean,
        ..
    }) = err.downcast_ref::<dq_core::Error>()
    {
        // What the user or the file put here goes on one line
        // ([`dq_core::one_line`]), so it cannot pass for a line of its own.
        writeln!(w, "{}", one_line(&err.to_string()))?;
        if !matched_prefix.is_empty() {
            writeln!(w, "  matched prefix: {}", one_line(matched_prefix))?;
        }
        if !did_you_mean.is_empty() {
            let keys: Vec<_> = did_you_mean.iter().map(|key| one_line(key)).collect();
            writeln!(w, "  did_you_mean: {}", keys.join(", "))?;
        }
        // Echo the original pointer for copy-paste convenience.
        writeln!(w, "  pointer: {}", one_line(pointer))?;
        return Ok(());
    }
    write_chain(err, w)
}

/// `err` and the errors that caused it, as anyhow's `{:?}` writes them —
/// the message, then `Caused by:` and each cause indented, numbered when
/// there are several, then the backtrace when one was captured — each
/// message as [`terminal_message`] gives it.
fn write_chain(err: &anyhow::Error, w: &mut dyn Write) -> std::io::Result<()> {
    let mut chain = err.chain();
    if let Some(top) = chain.next() {
        write!(w, "{}", terminal_message(top))?;
    }
    let causes: Vec<_> = chain.collect();
    if !causes.is_empty() {
        write!(w, "\n\nCaused by:")?;
        let numbered = causes.len() > 1;
        for (n, cause) in causes.into_iter().enumerate() {
            writeln!(w)?;
            for (i, line) in terminal_message(cause).split('\n').enumerate() {
                match (numbered, i) {
                    (true, 0) => write!(w, "{n:>5}: ")?,
                    (true, _) => write!(w, "\n       ")?,
                    (false, 0) => write!(w, "    ")?,
                    (false, _) => write!(w, "\n    ")?,
                }
                write!(w, "{line}")?;
            }
        }
    }
    // The backtrace anyhow captured (`RUST_BACKTRACE`), as its `{:?}`
    // writes it.
    let backtrace = err.backtrace();
    if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
        let mut backtrace = backtrace.to_string();
        write!(w, "\n\n")?;
        if backtrace.starts_with("stack backtrace:") {
            backtrace.replace_range(0..1, "S");
        } else {
            writeln!(w, "Stack backtrace:")?;
        }
        write!(w, "{}", backtrace.trim_end())?;
    }
    writeln!(w)
}

/// The message of `err` for the terminal. A `dq_core` error dq knows to be
/// one line — a writer's refusal, a rejected edit, a path or I/O error —
/// and the CLI's own are put on one line ([`dq_core::one_line`]), so a key,
/// a pointer or a path they quote cannot break them. In the others only
/// what the user or a file put there goes on one line — a parse error's
/// file name, a template refusal's line, a rule's id and paths, a plugin's
/// rule id and path — and the rest keeps its line breaks: a parse error's
/// snippet, a wasm backtrace in a plugin's error. Any other error's
/// message stays as it is. [`render_error`] escapes the controls of every
/// line. The messages themselves keep the text as it is, for the
/// machine-readable outputs that carry them.
fn terminal_message(err: &(dyn std::error::Error + 'static)) -> String {
    use dq_exec::ExecError as X;
    use dq_plugin::PluginError as P;
    let text = err.to_string();
    if let Some(err) = err.downcast_ref::<dq_core::Error>() {
        return match err {
            dq_core::Error::Parse {
                file: Some(file), ..
            } => one_line_fields(text, &[file.as_str()]),
            dq_core::Error::Parse { .. } => text,
            dq_core::Error::TemplatedFile { snippet, .. } => one_line_fields(text, &[snippet]),
            _ => one_line(&text).into_owned(),
        };
    }
    if let Some(err) = err.downcast_ref::<X>() {
        let fields: Vec<&str> = match err {
            X::Parse { .. } => Vec::new(),
            X::UnknownRule { id, .. } => vec![id],
            X::Io { path, .. } | X::TestFixture { path, .. } => vec![path.as_str()],
            X::CompositeExtractUnknownFormat {
                rule_id, format, ..
            } => vec![rule_id, format],
            X::SchemaFileAbsolutePath { rule_id, path, .. }
            | X::SchemaFileEscapesRuleDir { rule_id, path, .. } => vec![rule_id, path.as_str()],
            X::RuleCompile { rule_id, .. }
            | X::GlobCompile { rule_id, .. }
            | X::FixApply { rule_id, .. }
            | X::CheckMutuallyExclusive { rule_id, .. }
            | X::CheckMissing { rule_id, .. }
            | X::CompositeIncomplete { rule_id, .. }
            | X::CompositeExtractNotArray { rule_id, .. }
            | X::CompositeExtractMalformed { rule_id, .. }
            | X::CompositeDepthExceeded { rule_id, .. }
            | X::SchemaCompile { rule_id, .. } => vec![rule_id],
        };
        return one_line_fields(text, &fields);
    }
    if let Some(err) = err.downcast_ref::<P>() {
        let fields: Vec<&str> = match err {
            P::FeatureDisabled { .. } => Vec::new(),
            P::SchemaVersion { plugin_version, .. } => vec![plugin_version],
            P::DisallowedImport { interface, .. } => vec![interface],
            P::Load { path, .. } => vec![path.as_str()],
            P::Exhausted { rule_id, .. }
            | P::Memory { rule_id, .. }
            | P::MalformedFix { rule_id, .. }
            | P::Invoke { rule_id, .. } => vec![rule_id],
        };
        return one_line_fields(text, &fields);
    }
    if err.is::<error::InvalidInput>()
        || err.is::<error::DocOutOfRange>()
        || err.is::<error::WrongType>()
    {
        return one_line(&text).into_owned();
    }
    text
}

/// `text` with the first spelling of each of `fields` in it put on one
/// line ([`dq_core::one_line`]); the rest of `text` as it is.
fn one_line_fields(mut text: String, fields: &[&str]) -> String {
    for field in fields {
        if field.contains(['\n', '\r']) {
            text = text.replacen(field, &one_line(field), 1);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn rendered(err: impl std::error::Error + Send + Sync + 'static) -> String {
        let mut out = Vec::new();
        render_error(&anyhow::Error::new(err), &mut out).expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn a_plugin_error_keeps_its_line_breaks_but_the_rule_ids() {
        // Was: one line, every line break of wasmtime's backtrace `\x0a`.
        let err = dq_plugin::PluginError::Invoke {
            rule_id: "x.a\nb".into(),
            message: "error while executing at wasm backtrace:\n    0: 0x1 - f\u{1b}".into(),
        };
        assert_eq!(
            rendered(err),
            "plugin rule x.a\\x0ab invocation failed: error while executing at wasm \
             backtrace:\n    0: 0x1 - f\\x1b\n"
        );
        let err = dq_plugin::PluginError::Load {
            path: "p\nq.wasm".into(),
            message: "bad\nmodule".into(),
        };
        assert_eq!(
            rendered(err),
            "failed to load plugin p\\x0aq.wasm: bad\nmodule\n"
        );
    }

    #[test]
    fn a_rule_error_puts_its_rule_id_on_one_line() {
        let err = dq_exec::ExecError::FixApply {
            rule_id: "r\nx".into(),
            message: "two\nlines".into(),
        };
        assert_eq!(rendered(err), "rule r\\x0ax fix failed: two\nlines\n");
    }

    #[test]
    fn run_dispatches_to_get_against_missing_file() {
        // With Section-5 implementations in place, dispatching `get` against a
        // non-existent file returns an I/O error from the dq-core layer. This
        // is the closest thing to a smoke check we can do without a real
        // tempfile in lib-level tests.
        let cli = Cli::try_parse_from(["dq", "get", "/nope/missing.yaml", "/foo"]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let result = run(&cli, false, &mut out, &mut err);
        let e = result.unwrap_err();
        let domain = e
            .downcast_ref::<dq_core::Error>()
            .expect("dispatch should produce a dq-core error for missing file");
        assert_eq!(domain.kind_name(), "io");
    }

    #[test]
    fn run_rejects_in_place_flag_via_read_handler_gate() {
        // Each read handler (here: `get`) calls `Cli::ensure_no_write_flags`
        // on its first line, so passing `--in-place` to a read subcommand
        // surfaces the structured rejection message before any I/O happens.
        let cli = Cli::try_parse_from(["dq", "--in-place", "get", "x.yaml", "/foo"]).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let result = run(&cli, false, &mut out, &mut err);
        let e = result.unwrap_err();
        assert!(
            e.to_string().contains("--in-place"),
            "expected write-flag rejection message, got: {e:?}"
        );
        assert!(
            e.downcast_ref::<error::InvalidInput>().is_some(),
            "rejection must carry InvalidInput marker so exit-code mapper picks 6, got: {e:?}",
        );
    }
}
