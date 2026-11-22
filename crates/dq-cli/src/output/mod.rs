//! Output reporters and the `OutputFormat` enum.
//!
//! `dq-cli` separates *what* a command produces (a `serde_json::Value`) from
//! *how* it is rendered. The [`Reporter`] trait is the seam between handlers
//! and the chosen output format; per design D7 the factory that picks a
//! concrete reporter (`reporter_for_format`) lives in `main.rs` (the wiring
//! layer), and handlers receive a `&dyn Reporter` from the dispatcher rather
//! than constructing their own.

pub mod console;
pub mod json;
pub mod jsonl;
pub mod junit;
pub mod sarif;
pub mod tap;
pub mod toml;
pub mod toon;
pub mod yaml;

use std::borrow::Cow;
use std::io::IsTerminal;
use std::io::Write;

pub use console::ConsoleReporter;
pub use json::JsonReporter;
pub use jsonl::JsonlReporter;
pub use junit::JunitReporter;
pub use sarif::SarifReporter;
pub use tap::TapReporter;
pub use toml::TomlReporter;
pub use toon::ToonReporter;
pub use yaml::YamlReporter;

/// `text` — a file name, a key, a rule id, a test case's name: what the
/// user or a file put in a line dq prints outside a reporter (the bulk
/// summary, a `--diff` header, the `test` runner, a log line) — on one
/// line a terminal shows as it is: a line break and every other control
/// character as `\xNN`, the direction controls and separators as
/// `\uNNNN`, as the console reporter prints them
/// ([`console::escape_control`]). It cannot start a line of its own, nor
/// send the terminal a control sequence.
#[must_use]
pub fn terminal_text(text: &str) -> Cow<'_, str> {
    console::escape_control(text)
}

/// The field formatter of the log lines `tracing` writes on stderr: each
/// field as it prints, through [`terminal_text`] — a path, a key or a rule
/// id a warning quotes cannot break the line or reach the terminal raw —
/// the message alone, any other field as `name=value`, separated by a
/// space, as `tracing_subscriber`'s default formatter writes them. The
/// fields a line logged through the `log` crate carries (`log.target`,
/// `log.module_path`, `log.file`, `log.line`: jaq's `stderr`, globset,
/// ureq) are left out, as the default formatter leaves them: `log.file`
/// is a path on the machine dq was built on. With colour on, a field's
/// name is in italics and its `=` dimmed, as the default formatter writes
/// them; the level's colour is the event formatter's, outside the fields.
#[must_use]
pub fn log_fields() -> LogFields {
    LogFields
}

/// See [`log_fields`].
#[derive(Debug, Clone, Copy, Default)]
pub struct LogFields;

impl<'writer> tracing_subscriber::fmt::FormatFields<'writer> for LogFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: tracing_subscriber::fmt::format::Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        let mut visitor = LogVisitor {
            writer,
            first: true,
            result: Ok(()),
        };
        fields.record(&mut visitor);
        visitor.result
    }
}

/// Writes the fields of one log line for [`LogFields`].
struct LogVisitor<'writer> {
    writer: tracing_subscriber::fmt::format::Writer<'writer>,
    first: bool,
    result: std::fmt::Result,
}

impl tracing::field::Visit for LogVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if self.result.is_err() || name.starts_with("log.") {
            return;
        }
        let text = format!("{value:?}");
        let text = terminal_text(&text);
        let sep = if self.first { "" } else { " " };
        self.first = false;
        let name = name.strip_prefix("r#").unwrap_or(name);
        self.result = if name == "message" {
            write!(self.writer, "{sep}{text}")
        } else if self.writer.has_ansi_escapes() {
            // With colour, as tracing-subscriber's own formatter writes a
            // field: the name in italics, the `=` dimmed.
            write!(
                self.writer,
                "{sep}\x1b[3m{name}\x1b[0m\x1b[2m=\x1b[0m{text}"
            )
        } else {
            write!(self.writer, "{sep}{name}={text}")
        };
    }
}

/// Output format selectable via the global `-F/--format` flag.
///
/// M5 deliberately does NOT include `dockerfile` or `ignore-list`: both are
/// read-only formats (see the add-format-extensions section of
/// `docs/decisions.md`).
/// Because `clap::ValueEnum` only accepts variants present in this enum, the
/// `-F dockerfile` / `-F ignore-list` invocations are rejected at the clap
/// parse step with the standard "invalid value" error — no runtime guard
/// needed in the convert handler.
///
/// M6 adds the [`OutputFormat::Sarif`] variant — see the add-distribution
/// section of `docs/decisions.md`. SARIF is **output-only**
/// (it is a diagnostic report format, not a parseable source format) and is
/// only valid for diagnostic-shaped commands like `validate` and the future
/// M8 lint engine. Selecting `-F sarif` for a query verb produces the same
/// `BannedReporter` error path used by the M5 Stage 3 read-only formats.
///
/// M8 adds two more diagnostic-only variants — [`OutputFormat::Junit`] (JUnit
/// XML, consumed by GitLab CI / Jenkins) and [`OutputFormat::Tap`] (Test
/// Anything Protocol 13, consumed by `prove` and CI dashboards). Both follow
/// the same discipline as `Sarif`: they are output-only, expect the canonical
/// `{ "diagnostics": [...] }` shape, and surface `InvalidInput` (exit 6) when
/// fed any other value. See the add-exec-engine section of
/// `docs/decisions.md` and `docs/reference/cli.md`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable colored output (default).
    #[default]
    Console,
    /// Pretty-printed JSON.
    Json,
    /// YAML (round-tripped through `dq-core`).
    Yaml,
    /// TOML (round-tripped through `dq-core`).
    Toml,
    /// Newline-delimited JSON.
    Jsonl,
    /// Token-Oriented Object Notation, optimised for LLM consumption.
    Toon,
    /// HCL (HashiCorp Configuration Language) — Terraform et al.
    Hcl,
    /// INI / Java `.properties` files.
    Ini,
    /// `.env` files (`KEY=VALUE`).
    DotEnv,
    /// CSV (comma-separated values).
    Csv,
    /// TSV (tab-separated values).
    Tsv,
    /// Markdown with YAML / TOML / JSON frontmatter.
    Frontmatter,
    /// CommonMark + GFM markdown (`convert -F markdown`).
    Markdown,
    /// XML 1.0, read and write. Round-trippable for element structure,
    /// attributes, comments, CDATA, processing instructions, namespace
    /// prefixes, and the XML declaration. Mixed content (text interleaved
    /// with elements inside a single parent) is partial — see
    /// `dq-core/src/parsers/xml.rs` for the full conventional-key contract.
    Xml,
    /// SARIF 2.1.0 — Static Analysis Results Interchange Format.
    ///
    /// Output-only diagnostic format consumed by GitHub Code Scanning,
    /// GitLab Code Quality, etc. Per design D4, only diagnostic-producing
    /// commands (`validate` in M6, lint engine in M8) populate the
    /// `{ "diagnostics": [...] }` shape the reporter expects. Selecting
    /// `-F sarif` for a query command surfaces the same `InvalidInput`
    /// error path used by the M5 read-only-format reporters.
    Sarif,
    /// JUnit XML — diagnostic-only output format consumed by GitLab CI,
    /// Jenkins, and other test-result aggregators.
    ///
    /// Per the add-exec-engine section of `docs/decisions.md`, M8's
    /// lint engine populates the `{ "diagnostics": [...] }` shape and the
    /// reporter renders one `<testcase>` per diagnostic. Selecting
    /// `-F junit` for a non-diagnostic command surfaces an `InvalidInput`
    /// error from inside [`junit::JunitReporter`], same shape as the SARIF
    /// fallback.
    Junit,
    /// TAP version 13 — diagnostic-only output format consumed by `prove`,
    /// GitLab CI, and other TAP-aware harnesses.
    ///
    /// Per the add-exec-engine section of `docs/decisions.md`, M8's
    /// lint engine populates the `{ "diagnostics": [...] }` shape and the
    /// reporter renders each diagnostic as `not ok` with a YAML detail
    /// block. Selecting `-F tap` for a non-diagnostic command surfaces an
    /// `InvalidInput` error from inside [`tap::TapReporter`].
    Tap,
}

impl OutputFormat {
    /// The `dq-core` file format this output format writes, when it is one.
    ///
    /// `-F` never picks the parser (that is `--input-format`); this name is
    /// only compared with a file's own format by
    /// [`crate::cli::Cli::ensure_format_kept`], so `-F yaml` on a YAML file
    /// a write command keeps in place is accepted. Console, TOON and the
    /// diagnostic reports are no file format and map to `None`.
    #[must_use]
    pub fn file_format_name(&self) -> Option<&'static str> {
        match self {
            Self::Json => Some("json"),
            Self::Yaml => Some("yaml"),
            Self::Toml => Some("toml"),
            Self::Jsonl => Some("jsonl"),
            Self::Hcl => Some("hcl"),
            Self::Ini => Some("ini"),
            Self::DotEnv => Some("dotenv"),
            Self::Csv => Some("csv"),
            Self::Tsv => Some("tsv"),
            Self::Frontmatter => Some("frontmatter"),
            Self::Markdown => Some("markdown"),
            Self::Xml => Some("xml"),
            // SARIF, JUnit and TAP are diagnostic reports; console and TOON
            // have no file writer in dq-core.
            Self::Console | Self::Toon | Self::Sarif | Self::Junit | Self::Tap => None,
        }
    }
}

/// Render a [`serde_json::Value`] to a writer in some output format.
///
/// Implementations are stateless once constructed (the only exception is
/// [`ConsoleReporter`], which captures `use_color` at construction). All
/// I/O goes through `w` so unit tests can pass `&mut Vec<u8>`.
pub trait Reporter {
    /// Write `value` to `w` in the reporter's format.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from `w`, plus any format-specific encoding
    /// error (e.g. TOML cannot represent a top-level scalar).
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()>;

    /// Write a [`dq_core::Value`] to `w` — the shape the values `get`,
    /// `values`, `select` and `query` print come in, which may hold a NaN
    /// or an infinity no `serde_json::Value` can.
    ///
    /// The default renders [`dq_core::Value::to_serde_json`], which spells
    /// a non-finite float `.nan` / `.inf` / `-.inf` (console, TOON); a
    /// reporter whose format has (YAML, TOML) or forbids (JSON, JSONL) such
    /// a number overrides it.
    ///
    /// # Errors
    ///
    /// As [`Reporter::report`].
    fn report_value(&self, value: &dq_core::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        self.report(&value.to_serde_json(), w)
    }

    /// Whether the diagnostics envelope handed to [`Reporter::report`]
    /// should carry a rule catalogue next to `diagnostics`:
    /// `"rules": [{ "id", "description", "references" }]`, one entry per
    /// rule the producer knows of.
    ///
    /// Only SARIF describes its rules (`tool.driver.rules`); every other
    /// reporter prints the envelope as is or ignores the extra key, so the
    /// default is `false` and the producers (`lint`, `check`, `validate`)
    /// leave the key out.
    fn wants_rule_catalog(&self) -> bool {
        false
    }
}

/// Resolve whether output should be coloured.
///
/// Precedence (highest to lowest):
/// 1. `--no-color` flag  → `false`
/// 2. `NO_COLOR` env var present → `false`
/// 3. `CLICOLOR_FORCE` env var present → `true`
/// 4. Otherwise: stdout is a TTY → `true`, else `false`
///
/// Tests MUST pass the CLI flag (or use `Command::env(...)`) — never mutate
/// `NO_COLOR` via `std::env::set_var`, which is not thread-safe and breaks
/// parallel test execution.
#[must_use]
pub fn resolve_color(no_color_flag: bool) -> bool {
    color_for_stream(no_color_flag, std::io::stdout().is_terminal())
}

/// Resolve whether log output should be coloured.
///
/// Logs go to stderr, so this is [`resolve_color`] with stderr in place of
/// stdout: `--no-color`, then `NO_COLOR`, then `CLICOLOR_FORCE`, then
/// whether stderr is a TTY.
#[must_use]
pub fn resolve_log_color(no_color_flag: bool) -> bool {
    color_for_stream(no_color_flag, std::io::stderr().is_terminal())
}

/// Shared precedence of [`resolve_color`] and [`resolve_log_color`];
/// `is_terminal` is the answer for the stream being coloured.
fn color_for_stream(no_color_flag: bool, is_terminal: bool) -> bool {
    color_decision(
        no_color_flag,
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var_os("CLICOLOR_FORCE").is_some(),
        is_terminal,
    )
}

/// The colour precedence as a pure function of its inputs.
fn color_decision(
    no_color_flag: bool,
    no_color_env: bool,
    clicolor_force_env: bool,
    is_terminal: bool,
) -> bool {
    if no_color_flag || no_color_env {
        return false;
    }
    clicolor_force_env || is_terminal
}

/// Convert a `serde_json::Value` into a `dq_core::Value` so the reporter can
/// reuse `dq-core`'s format implementations for YAML / TOML / JSONL output.
///
/// A number keeps its exact value, as [`dq_core::Value::from_serde_json`]
/// reads it: with `serde_json`'s `arbitrary_precision`, a literal no `i64`
/// or `f64` holds stays a `BigInt` / `BigFloat`.
#[must_use]
pub fn from_serde_value(v: &serde_json::Value) -> dq_core::Value {
    dq_core::Value::from_serde_json(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fields of one `warn!` with a field, as the log line writes them
    /// with colour (`ansi`) or not.
    fn log_line(ansi: bool) -> String {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(ansi)
            .without_time()
            .with_target(false)
            .with_level(false)
            .fmt_fields(log_fields())
            .with_writer(move || SharedSink(sink.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(path = "a\nb", r#type = 1, "skipped");
        });
        let out = buf.lock().expect("lock").clone();
        String::from_utf8(out).expect("utf-8")
    }

    struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn log_fields_are_styled_with_colour_and_plain_without() {
        assert_eq!(log_line(false), "skipped path=\"a\\nb\" type=1\n");
        // Was, with colour: the same plain text. Field names are italic and
        // the `=` dimmed, as tracing-subscriber's own formatter writes them.
        assert_eq!(
            log_line(true),
            "skipped \u{1b}[3mpath\u{1b}[0m\u{1b}[2m=\u{1b}[0m\"a\\nb\" \
             \u{1b}[3mtype\u{1b}[0m\u{1b}[2m=\u{1b}[0m1\n"
        );
    }

    #[test]
    fn output_format_default_is_console() {
        assert_eq!(OutputFormat::default(), OutputFormat::Console);
    }

    #[test]
    fn color_decision_follows_the_precedence() {
        // (flag, NO_COLOR, CLICOLOR_FORCE, terminal) -> colour
        let cases = [
            ((false, false, false, true), true),
            ((false, false, false, false), false),
            ((false, false, true, false), true),
            ((false, true, true, true), false),
            ((true, false, true, true), false),
        ];
        for ((flag, no_color, force, tty), expected) in cases {
            assert_eq!(
                color_decision(flag, no_color, force, tty),
                expected,
                "flag={flag} NO_COLOR={no_color} CLICOLOR_FORCE={force} tty={tty}",
            );
        }
    }

    #[test]
    fn from_serde_value_round_trip_basic_types() {
        let json = serde_json::json!({"a": 1, "b": [true, "x", null]});
        let core = from_serde_value(&json);
        let dq_core::Value::Map(m) = core else {
            panic!("expected map");
        };
        assert_eq!(m.get("a"), Some(&dq_core::Value::Int(1)));
        let dq_core::Value::Array(items) = m.get("b").unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], dq_core::Value::Bool(true));
        assert_eq!(items[1], dq_core::Value::String("x".into()));
        assert_eq!(items[2], dq_core::Value::Null);
    }
}
