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

use indexmap::IndexMap;

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
/// read-only formats per design D9 (`openspec/changes/add-format-extensions/design.md`).
/// Because `clap::ValueEnum` only accepts variants present in this enum, the
/// `-F dockerfile` / `-F ignore-list` invocations are rejected at the clap
/// parse step with the standard "invalid value" error — no runtime guard
/// needed in the convert handler.
///
/// M6 adds the [`OutputFormat::Sarif`] variant — see design D4 in
/// `openspec/changes/add-distribution/design.md`. SARIF is **output-only**
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
/// fed any other value. See design D8 / D9 in
/// `openspec/changes/add-exec-engine/design.md`.
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
    /// Per design D8 in `openspec/changes/add-exec-engine/design.md`, M8's
    /// lint engine populates the `{ "diagnostics": [...] }` shape and the
    /// reporter renders one `<testcase>` per diagnostic. Selecting
    /// `-F junit` for a non-diagnostic command surfaces an `InvalidInput`
    /// error from inside [`junit::JunitReporter`], same shape as the SARIF
    /// fallback.
    Junit,
    /// TAP version 13 — diagnostic-only output format consumed by `prove`,
    /// GitLab CI, and other TAP-aware harnesses.
    ///
    /// Per design D9 in `openspec/changes/add-exec-engine/design.md`, M8's
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
    if no_color_flag {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR_FORCE").is_some() {
        return true;
    }
    std::io::stdout().is_terminal()
}

/// Convert a `serde_json::Value` into a `dq_core::Value` so the reporter can
/// reuse `dq-core`'s format implementations for YAML / TOML / JSONL output.
///
/// The conversion is lossy in the same direction `dq-core::parsers::json` is
/// lossy when *reading* user JSON: numeric precision beyond `i64`/`f64` is
/// preserved as `BigInt`/`BigFloat` if the input came from `arbitrary_precision`
/// `serde_json`, otherwise it falls back to whichever variant fits.
#[must_use]
pub fn from_serde_value(v: &serde_json::Value) -> dq_core::Value {
    match v {
        serde_json::Value::Null => dq_core::Value::Null,
        serde_json::Value::Bool(b) => dq_core::Value::Bool(*b),
        serde_json::Value::Number(n) => number_to_value(n),
        serde_json::Value::String(s) => dq_core::Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            dq_core::Value::Array(items.iter().map(from_serde_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, child) in map {
                out.insert(k.clone(), from_serde_value(child));
            }
            dq_core::Value::Map(out)
        }
    }
}

fn number_to_value(n: &serde_json::Number) -> dq_core::Value {
    if let Some(i) = n.as_i64() {
        return dq_core::Value::Int(i);
    }
    if let Some(f) = n.as_f64() {
        return dq_core::Value::Float(f);
    }
    // `arbitrary_precision` / u64 > i64::MAX path: keep the original literal.
    dq_core::Value::BigInt(n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_default_is_console() {
        assert_eq!(OutputFormat::default(), OutputFormat::Console);
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
