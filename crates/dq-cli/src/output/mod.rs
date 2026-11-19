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
    /// CommonMark + GFM markdown (M9 — `convert -F markdown`).
    Markdown,
    /// XML 1.0 (M11 — read+write). Round-trippable for element structure,
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
    /// Map this format to a `dq-core` parser name when one applies.
    ///
    /// `-F` is overloaded: it picks both the renderer and the *input* parser
    /// when the file's extension does not match. Console and Toon are
    /// output-only — they map to `None` so input detection falls back to the
    /// file's extension.
    #[must_use]
    pub fn as_input_format_name(&self) -> Option<&'static str> {
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
            // M6: SARIF is a diagnostic *output* format with no source-side
            // parser. M8: Junit and Tap follow the same discipline.
            // Falling through to `None` keeps input detection on the
            // file-extension path, matching `Console` / `Toon`.
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
