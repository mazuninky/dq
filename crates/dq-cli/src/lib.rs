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
pub mod skill;

pub use cli::{Cli, Command};
pub use error::{
    DocOutOfRange, DocOutOfRangeEverywhere, LintFail, SilentError, ValidateFail, WrongType,
};

use std::io::Write;

use cli::InputFormat;
use dq_core::one_line;
use output::{
    ConsoleReporter, JsonReporter, JsonlReporter, JunitReporter, OutputFormat, Reporter,
    SarifReporter, StreamColors, TapReporter, TomlReporter, ToonReporter, YamlReporter,
};

/// CLI entry point used by both `main.rs` and integration tests.
///
/// Performs reporter selection from `cli.format` + `colors`, then
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
    colors: impl Into<StreamColors>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> anyhow::Result<()> {
    let colors = colors.into();
    let reporter = reporter_for_format(cli.format, colors.stdout);
    dispatch(cli, colors, reporter.as_ref(), stdout, stderr)
}

fn dispatch(
    cli: &Cli,
    colors: StreamColors,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let use_color = colors.stdout;
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
        Command::Validate(args) => {
            commands::validate::run(cli, args, input_format, colors.stderr, reporter, err)
        }
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
                cli::SelfCommand::Check => {
                    let env = skill::Environment::from_process();
                    commands::self_cmd::run_check(
                        commands::self_cmd::GITHUB_RELEASES_LATEST_URL,
                        &env,
                        out,
                    )
                }
                cli::SelfCommand::Update(u) => {
                    commands::self_cmd::run_update(u, &skill::Environment::from_process(), out)
                }
            }
        }
        Command::Skill(args) => {
            cli.ensure_no_write_flags()?;
            let env = skill::Environment::from_process().with_current_dir();
            commands::skill::run(args, cli.quiet, &env, reporter, out)
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

/// Render an `anyhow::Error` to a writer in human-friendly form, with ANSI
/// escapes when `use_color` is set (the colour of stderr,
/// [`output::resolve_log_color`]).
///
/// Behaviour:
/// - For `dq_core::Error::Path` errors, the renderer expands the structured
///   fields (matched prefix, did_you_mean) onto separate lines so the spec's
///   "did_you_mean" affordance is visible to users without `-vv`.
/// - For [`ValidateFail`] errors, the rendering is suppressed because the
///   validate handler has already written the error to stderr; printing
///   again would duplicate the message.
/// - For all other errors, the standard anyhow chain (`{:?}`) is written; a
///   parse error's position (`  --> FILE:LINE:COL`) and the source line with
///   a caret under the character at fault follow the message. A cause the
///   message above it names already is left out.
///
/// Every line is written escaped as the console reporter escapes text
/// ([`output::console`]): a message echoes what the user typed and what
/// the file holds — a pointer, a `did_you_mean` key, a path — and a
/// control character or a bidi override in it must not reach the terminal
/// raw. The message's own line breaks stay.
///
/// Called from `main.rs` after `run` returns an error; tests can call it
/// directly with a `Vec<u8>` writer to assert on the rendered shape.
///
/// # Errors
///
/// Any error of `w`.
pub fn render_error(
    err: &anyhow::Error,
    use_color: bool,
    w: &mut dyn Write,
) -> std::io::Result<()> {
    if err.downcast_ref::<ValidateFail>().is_some() {
        // The validate handler already wrote the error to the
        // caller-supplied stderr writer. Don't print again.
        return Ok(());
    }
    if let Some(usage) = err.downcast_ref::<clap::Error>() {
        // Rendered the way clap renders its own usage errors: `error: …`,
        // the usage line, the `--help` hint.
        return write_escaped(&usage.to_string(), w);
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
        let mut text = format!("{}\n", one_line(&err.to_string()));
        if !matched_prefix.is_empty() {
            text.push_str(&format!("  matched prefix: {}\n", one_line(matched_prefix)));
        }
        if !did_you_mean.is_empty() {
            let keys: Vec<_> = did_you_mean.iter().map(|key| one_line(key)).collect();
            text.push_str(&format!("  did_you_mean: {}\n", keys.join(", ")));
        }
        // Echo the original pointer for copy-paste convenience.
        text.push_str(&format!("  pointer: {}\n", one_line(pointer)));
        return write_escaped(&text, w);
    }
    write_chain(err.chain(), use_color, w)?;
    write_backtrace(err, w)?;
    writeln!(w)
}

/// [`render_error`] of a `dq_core` error that is in no `anyhow::Error`, so
/// with no backtrace: the one `validate` prints itself.
pub(crate) fn render_core_error(
    err: &dq_core::Error,
    use_color: bool,
    w: &mut dyn Write,
) -> std::io::Result<()> {
    write_chain(anyhow::Chain::new(err), use_color, w)?;
    writeln!(w)
}

/// `text` with the controls of every line escaped; its line breaks stay.
fn write_escaped(text: &str, w: &mut dyn Write) -> std::io::Result<()> {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            w.write_all(b"\n")?;
        }
        w.write_all(output::console::escape_control(line).as_bytes())?;
    }
    Ok(())
}

/// The errors of `chain`, as anyhow's `{:?}` writes them — the message, then
/// `Caused by:` and each cause indented, numbered when there are several —
/// each message as [`terminal_message`] gives it, with no line break at the
/// end. A parse error's position and source line follow the message; a cause
/// that says nothing new ([`error::telling_errors`]) is left out.
fn write_chain(
    chain: anyhow::Chain<'_>,
    use_color: bool,
    w: &mut dyn Write,
) -> std::io::Result<()> {
    let position = error::parse_position(chain.clone());
    let mut errors = error::telling_errors(chain);
    if let Some(top) = errors.next() {
        write_escaped(&terminal_message(top), w)?;
    }
    if let Some(at) = position {
        write!(w, "\n{}", output::error::source_block(&at, use_color))?;
    }
    let causes: Vec<String> = errors.map(terminal_message).collect();
    if causes.is_empty() {
        return Ok(());
    }
    let mut text = String::from("\n\nCaused by:");
    let numbered = causes.len() > 1;
    for (n, cause) in causes.iter().enumerate() {
        text.push('\n');
        for (i, line) in cause.split('\n').enumerate() {
            match (numbered, i) {
                (true, 0) => text.push_str(&format!("{n:>5}: ")),
                (true, _) => text.push_str("\n       "),
                (false, 0) => text.push_str("    "),
                (false, _) => text.push_str("\n    "),
            }
            text.push_str(line);
        }
    }
    write_escaped(&text, w)
}

/// The backtrace anyhow captured (`RUST_BACKTRACE`), as its `{:?}` writes
/// it; nothing when it captured none.
fn write_backtrace(err: &anyhow::Error, w: &mut dyn Write) -> std::io::Result<()> {
    let backtrace = err.backtrace();
    if backtrace.status() != std::backtrace::BacktraceStatus::Captured {
        return Ok(());
    }
    let mut text = backtrace.to_string();
    if text.starts_with("stack backtrace:") {
        text.replace_range(0..1, "S");
    } else {
        text.insert_str(0, "Stack backtrace:\n");
    }
    write_escaped(&format!("\n\n{}", text.trim_end()), w)
}

/// The message of `err` for the terminal. A `dq_core` error dq knows to be
/// one line — a parse error, a writer's refusal, a rejected edit, a path or
/// I/O error — and the CLI's own are put on one line
/// ([`dq_core::one_line`]), so what they quote of the user or a file cannot
/// break them, and the place of a parse error stays on the line after it. In
/// the others only what the user or a file put there goes on one line — a
/// template refusal's line, a rule's id and paths, a plugin's rule id and
/// path — and the rest keeps its line breaks: a wasm backtrace in a plugin's
/// error. Any other error's
/// message stays as it is. [`render_error`] escapes the controls of every
/// line. The messages themselves keep the text as it is, for the
/// machine-readable outputs that carry them.
fn terminal_message(err: &(dyn std::error::Error + 'static)) -> String {
    use dq_exec::ExecError as X;
    use dq_plugin::PluginError as P;
    let text = err.to_string();
    if let Some(err) = err.downcast_ref::<dq_core::Error>() {
        return match err {
            dq_core::Error::TemplatedFile { snippet, .. } => one_line_fields(text, &[snippet]),
            _ => one_line(&text).into_owned(),
        };
    }
    if let Some(err) = err.downcast_ref::<X>() {
        let fields: Vec<&str> = match err {
            X::Parse { .. } => Vec::new(),
            X::UnknownRule { id, .. } => vec![id],
            X::Io { path, .. }
            | X::TestFixture { path, .. }
            | X::NoRules { path }
            | X::NotADirectory { path } => vec![path.as_str()],
            X::CompositeExtractUnknownFormat {
                rule_id, format, ..
            }
            | X::UnknownFormat {
                rule_id, format, ..
            } => vec![rule_id, format],
            X::SchemaFileAbsolutePath { rule_id, path, .. }
            | X::SchemaFileEscapesRuleDir { rule_id, path, .. } => vec![rule_id, path.as_str()],
            X::RuleCompile { rule_id, .. }
            | X::NoFormat { rule_id }
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
        || err.is::<skill::SkillError>()
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

    /// `render_error`'s text without the backtrace section: whether anyhow
    /// captures one depends on `RUST_BACKTRACE`, which CI sets. The CLI test
    /// pins the backtrace itself.
    fn rendered(err: impl std::error::Error + Send + Sync + 'static) -> String {
        let mut out = Vec::new();
        render_error(&anyhow::Error::new(err), false, &mut out).expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        match text.find("\n\nStack backtrace:") {
            Some(at) => format!("{}\n", &text[..at]),
            None => text,
        }
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

    /// A parse error of `f.yaml` at `line`, column 4.
    fn parse_err(line: u32) -> dq_core::Error {
        dq_core::Error::Parse {
            file: Some("f.yaml".into()),
            line,
            col: 4,
            span: 9..11,
            snippet: "a: [b".into(),
            message: "bad".into(),
        }
    }

    #[test]
    fn a_parse_error_is_placed_on_the_line_after_its_message() {
        assert_eq!(
            rendered(parse_err(3)),
            "parse error in f.yaml: bad\n  --> f.yaml:3:4\n   |\n 3 | a: [b\n   |    ^~\n"
        );
        // No position in a file.
        assert_eq!(rendered(parse_err(0)), "parse error in f.yaml: bad\n");
        // `validate` has reported it.
        assert_eq!(
            rendered(ValidateFail {
                source: parse_err(3)
            }),
            ""
        );
    }

    #[test]
    fn a_parse_error_below_another_error_is_placed_too() {
        let err = anyhow::Error::new(parse_err(3)).context("while reading");
        let mut out = Vec::new();
        render_error(&err, false, &mut out).expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.starts_with(
                "while reading\n  --> f.yaml:3:4\n   |\n 3 | a: [b\n   |    ^~\n\n\
                 Caused by:\n    parse error in f.yaml: bad\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn colour_adds_styles_to_the_block_only() {
        let err = anyhow::Error::new(parse_err(3));
        let mut out = Vec::new();
        render_error(&err, true, &mut out).expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.starts_with(
                "parse error in f.yaml: bad\n\u{1b}[2m  --> f.yaml:3:4\u{1b}[0m\n\
                 \u{1b}[2m   |\u{1b}[0m\n"
            ),
            "{text:?}"
        );
        // Any other error has no colour.
        let err = anyhow::Error::new(dq_core::Error::UnsupportedFormat {
            name: "\u{1b}x".into(),
        });
        let mut out = Vec::new();
        render_error(&err, true, &mut out).expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.starts_with("unsupported format '\\x1bx'\n"),
            "{text:?}"
        );
    }

    #[test]
    fn a_cause_the_message_above_it_holds_is_left_out() {
        let io = |message: &str| dq_core::Error::Io {
            path: "m.json".into(),
            source: std::io::Error::other(message.to_owned()),
        };
        assert_eq!(rendered(io("gone")), "I/O error reading 'm.json': gone\n");
        let wrapped = anyhow::Error::new(io("gone")).context("cannot start");
        let mut out = Vec::new();
        render_error(&wrapped, false, &mut out).expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.starts_with("cannot start\n\nCaused by:\n    I/O error reading 'm.json': gone\n")
                && text.matches("gone").count() == 1,
            "{text}"
        );
        // Causes that say something new are numbered as before.
        let wrapped = wrapped.context("top");
        let mut out = Vec::new();
        render_error(&wrapped, false, &mut out).expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(
            text.starts_with(
                "top\n\nCaused by:\n    0: cannot start\n    1: I/O error reading 'm.json': gone\n"
            ) && text.matches("gone").count() == 1,
            "{text}"
        );
    }

    /// The text of `render_error` of an error of `messages`, the cause of the
    /// others first, without the backtrace.
    fn rendered_chain(messages: &[&'static str]) -> String {
        let (cause, above) = messages.split_first().expect("a message");
        let err = above
            .iter()
            .fold(anyhow::Error::msg(*cause), |err, message| {
                err.context(*message)
            });
        let mut out = Vec::new();
        render_error(&err, false, &mut out).expect("render");
        let text = String::from_utf8(out).expect("utf-8");
        match text.find("\n\nStack backtrace:") {
            Some(at) => format!("{}\n", &text[..at]),
            None => text,
        }
    }

    #[test]
    fn a_cause_is_compared_with_the_message_right_above_it() {
        assert_eq!(rendered_chain(&["a: b"]), "a: b\n");
        assert_eq!(rendered_chain(&["b", "a: b"]), "a: b\n");
        assert_eq!(
            rendered_chain(&["c", "b", "a: b: c"]),
            "a: b: c\n\nCaused by:\n    c\n"
        );
        // Left out or not, a cause is what the one below it is compared with.
        assert_eq!(rendered_chain(&["c", "b: c", "a: b: c"]), "a: b: c\n");
        // One left is not numbered, two are, from 0.
        assert_eq!(
            rendered_chain(&["more", "b", "a: b"]),
            "a: b\n\nCaused by:\n    more\n"
        );
        assert_eq!(
            rendered_chain(&["two", "b", "a: b", "one"]),
            "one\n\nCaused by:\n    0: a: b\n    1: two\n"
        );
        // Held by the cause, not by the message.
        assert_eq!(
            rendered_chain(&["a: b", "a"]),
            "a\n\nCaused by:\n    a: b\n"
        );
    }

    #[test]
    fn a_cause_of_a_rule_or_a_plugin_error_is_left_out_when_the_message_holds_it() {
        assert_eq!(
            rendered(dq_exec::ExecError::Io {
                path: "r.yml".into(),
                source: std::io::Error::other("gone"),
            }),
            "io error reading r.yml: gone\n"
        );
        assert_eq!(
            rendered(dq_core::Error::WriteIo {
                path: "f.yaml".into(),
                source: std::io::Error::other("full"),
            }),
            "I/O error writing 'f.yaml': full\n"
        );
        // The message of this one says nothing of why.
        let source = serde_json::from_str::<serde_json::Value>("[").expect_err("malformed");
        assert_eq!(
            rendered(dq_plugin::PluginError::MalformedFix {
                rule_id: "p.r".into(),
                source,
            }),
            "plugin rule p.r returned malformed fix payload\n\nCaused by:\n    \
             EOF while parsing a list at line 1 column 1\n"
        );
    }

    /// What a terminal acts on — ESC with a whole colour sequence, C1 CSI, a
    /// direction control, a line separator, NUL — and the escapes of it.
    const RAW: &str = "a\u{1b}[2mb\u{9b}2Jc\u{202e}d\u{2028}e\0f";
    const SHOWN: &str = "a\\x1b[2mb\\x9b2Jc\\u202ed\\u2028e\\x00f";

    /// Whether a terminal acts on a character of `text`, its line breaks
    /// apart.
    fn holds_a_control(text: &str) -> bool {
        text.chars()
            .any(|c| c != '\n' && (c.is_control() || matches!(c, '\u{202e}' | '\u{2028}')))
    }

    /// `render` without colour, checked: no control is in it, and with colour
    /// it is the same but for the styles of the source block, when there is
    /// one.
    fn checked(render: impl Fn(bool, &mut Vec<u8>) -> std::io::Result<()>) -> String {
        let text = |use_color| {
            let mut out = Vec::new();
            render(use_color, &mut out).expect("render");
            let text = String::from_utf8(out).expect("utf-8");
            match text.find("\n\nStack backtrace:") {
                Some(at) => format!("{}\n", &text[..at]),
                None => text,
            }
        };
        let plain = text(false);
        assert!(!holds_a_control(&plain), "{plain:?}");
        let coloured = text(true);
        let unstyled = ["\u{1b}[2m", "\u{1b}[1;31m", "\u{1b}[0m"]
            .iter()
            .fold(coloured.clone(), |text, style| text.replace(style, ""));
        // A style of the text itself would be gone from `unstyled` and is
        // escaped in `plain`.
        assert_eq!(unstyled, plain, "{coloured:?}");
        assert_eq!(
            coloured != plain,
            plain.contains("\n  --> "),
            "only a source block has styles: {coloured:?}"
        );
        plain
    }

    /// [`checked`] of [`render_error`].
    fn escaped(err: impl Into<anyhow::Error>) -> String {
        let err = err.into();
        checked(|use_color, w| render_error(&err, use_color, w))
    }

    #[test]
    fn controls_of_every_message_are_escaped() {
        use dq_core::Error as E;
        let raw = || RAW.to_owned();
        let io = || std::io::Error::other(raw());
        let cases: Vec<(anyhow::Error, String)> = vec![
            (anyhow::anyhow!(raw()), SHOWN.into()),
            // The CLI's own.
            (error::InvalidInput::new(raw()).into(), SHOWN.into()),
            (
                error::WrongType {
                    action: "list the keys of",
                    pointer: raw(),
                    found: "int",
                    expected: "an object",
                }
                .into(),
                format!("cannot list the keys of '{SHOWN}': it is int, not an object"),
            ),
            (
                DocOutOfRange {
                    index: 3,
                    count: 1,
                    file: raw(),
                }
                .into(),
                format!("--doc 3 is out of range: {SHOWN} holds 1 document(s) (--doc 0)"),
            ),
            // `dq_core`'s.
            (
                E::Io {
                    path: raw().into(),
                    source: io(),
                }
                .into(),
                format!("I/O error reading '{SHOWN}': {SHOWN}"),
            ),
            (
                E::WriteIo {
                    path: raw().into(),
                    source: io(),
                }
                .into(),
                format!("I/O error writing '{SHOWN}': {SHOWN}"),
            ),
            (
                E::UnsupportedFormat { name: raw() }.into(),
                format!("unsupported format '{SHOWN}'"),
            ),
            (
                E::Format {
                    format: "csv",
                    message: raw(),
                }
                .into(),
                format!("csv format error: {SHOWN}"),
            ),
            (
                E::EditRejected {
                    action: "set",
                    pointer: raw(),
                    reason: raw(),
                }
                .into(),
                format!("cannot set '{SHOWN}': {SHOWN}"),
            ),
            // A template refusal: its message has two lines.
            (
                E::TemplatedFile {
                    line: 2,
                    snippet: raw(),
                    hint: raw(),
                }
                .into(),
                format!("file contains Go template syntax (line 2): {SHOWN}\n  hint: {SHOWN}"),
            ),
            // A rule's and a plugin's keep their line breaks.
            (
                dq_exec::ExecError::FixApply {
                    rule_id: raw(),
                    message: format!("{RAW}\n{RAW}"),
                }
                .into(),
                format!("rule {SHOWN} fix failed: {SHOWN}\n{SHOWN}"),
            ),
            (
                dq_plugin::PluginError::Load {
                    path: raw().into(),
                    message: format!("{RAW}\n{RAW}"),
                }
                .into(),
                format!("failed to load plugin {SHOWN}: {SHOWN}\n{SHOWN}"),
            ),
        ];
        for (err, shown) in cases {
            assert_eq!(escaped(err), format!("{shown}\n"));
        }
    }

    #[test]
    fn controls_of_a_cause_are_escaped() {
        assert_eq!(
            escaped(anyhow::anyhow!(RAW).context("top")),
            format!("top\n\nCaused by:\n    {SHOWN}\n")
        );
        // Several, of several lines: every line of every one.
        assert_eq!(
            escaped(
                anyhow::anyhow!("last {RAW}\n{RAW}")
                    .context(format!("first {RAW}\n{RAW}"))
                    .context(format!("top {RAW}"))
            ),
            format!(
                "top {SHOWN}\n\nCaused by:\n    0: first {SHOWN}\n       {SHOWN}\n    \
                 1: last {SHOWN}\n       {SHOWN}\n"
            )
        );
        // Below the source block, too.
        let parse = dq_core::Error::parse_at(b"[", 0..1, RAW);
        assert_eq!(
            escaped(anyhow::Error::new(parse).context("reading")),
            format!(
                "reading\n  --> <stdin>:1:1\n   |\n 1 | [\n   | ^\n\n\
                 Caused by:\n    parse error: {SHOWN}\n"
            )
        );
    }

    /// clap takes a terminal's sequences out of its text itself, and leaves
    /// what is no such sequence.
    #[test]
    fn controls_of_a_usage_error_are_escaped() {
        for control in ['\u{1b}', '\u{9b}', '\u{202e}', '\u{2028}', '\0', '\u{7}'] {
            let usage = cli::usage_error(
                clap::error::ErrorKind::ArgumentConflict,
                format!("no a{control}[b here"),
            );
            let text = escaped(usage);
            assert!(text.starts_with("error: no a"), "{control:?}: {text:?}");
            assert!(text.contains("\n\nUsage: "), "{control:?}: {text:?}");
            // The others are part of a sequence clap takes out, or make one.
            if let Some(escape) = match control {
                '\u{9b}' => Some("\\x9b"),
                '\u{202e}' => Some("\\u202e"),
                '\u{2028}' => Some("\\u2028"),
                _ => None,
            } {
                let want = format!("error: no a{escape}[b here\n\n");
                assert!(text.starts_with(&want), "{text:?}");
            }
        }
    }

    #[test]
    fn controls_of_every_field_of_a_path_error_are_escaped() {
        let raw = || RAW.to_owned();
        let path = |kind| dq_core::Error::Path {
            pointer: raw(),
            matched_prefix: raw(),
            kind,
            did_you_mean: vec![raw(), raw()],
        };
        let fields = format!(
            "\n  matched prefix: {SHOWN}\n  did_you_mean: {SHOWN}, {SHOWN}\n  pointer: {SHOWN}\n"
        );
        assert_eq!(
            escaped(path(dq_core::PathErrorKind::MissingKey)),
            format!("path '{SHOWN}' not found (matched up to '{SHOWN}'){fields}")
        );
        assert_eq!(
            escaped(path(dq_core::PathErrorKind::IntoItsOwnChild {
                from: raw()
            })),
            format!(
                "cannot move '{SHOWN}' to '{SHOWN}': a value cannot move into one of its own \
                 children{fields}"
            )
        );
    }

    #[test]
    fn controls_of_a_parse_error_are_escaped() {
        // The error is at the last character of the line.
        let err = || dq_core::Error::Parse {
            file: Some(RAW.into()),
            line: 1,
            col: u32::try_from(RAW.len()).expect("short"),
            span: RAW.len() - 1..RAW.len(),
            snippet: RAW.to_owned(),
            message: RAW.to_owned(),
        };
        let want = format!(
            "parse error in {SHOWN}: {SHOWN}\n  --> {SHOWN}:1:{}\n   |\n 1 | {SHOWN}\n   | {:2$}^\n",
            RAW.len(),
            "",
            SHOWN.len() - 1,
        );
        assert_eq!(escaped(err()), want);
        // The one `validate` prints itself.
        let err = err();
        assert_eq!(
            checked(|use_color, w| render_core_error(&err, use_color, w)),
            want
        );
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
