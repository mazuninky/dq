//! TAP (Test Anything Protocol) version 13 output reporter.
//!
//! Per design D9 in `openspec/changes/add-exec-engine/design.md`, the reporter
//! consumes the canonical lint-engine output shape `{ "diagnostics": [...] }`
//! — the same shape produced by `dq_lint::Diagnostic::to_serde_json` and
//! consumed by [`crate::output::sarif::SarifReporter`] and
//! [`crate::output::junit::JunitReporter`].
//!
//! The TAP body is hand-rolled — TAP 13 is a tiny line-based format and the
//! workspace deliberately keeps `dq-cli` dependency-light. Anything that is
//! not a `{ "diagnostics": [...] }` value errors with
//! [`crate::error::InvalidInput`] so the exit-code mapper picks 6, matching
//! the SARIF reporter's "wrong shape" path.
//!
//! # Output shape
//!
//! ```text
//! TAP version 13
//! 1..2
//! not ok 1 - k8s.no-latest-tag: Container 'app' uses :latest tag
//!   ---
//!   severity: error
//!   file: k8s/deploy.yaml
//!   line: 12
//!   col: 1
//!   message: "Container 'app' uses :latest tag"
//!   references:
//!     - "https://kubernetes.io/example"
//!   ...
//! not ok 2 - npm.has-license: package.json missing 'license' field
//!   ---
//!   severity: warn
//!   file: package.json
//!   line: 1
//!   col: 1
//!   message: "package.json missing 'license' field"
//!   ...
//! # 2 tests, 2 failures
//! ```
//!
//! Every diagnostic is reported as `not ok` because all diagnostics represent
//! findings — severity does NOT gate the line type. The footer prints
//! `# <N> tests, <N> failures` (both numbers equal because every diagnostic is
//! a failure under TAP semantics). The footer switches to the singular form
//! (`test` / `failure`) when `N == 1`.
//!
//! Empty `{ "diagnostics": [] }` produces:
//!
//! ```text
//! TAP version 13
//! 1..0
//! # 0 tests, 0 failures
//! ```

use std::io::Write;

use crate::error::InvalidInput;
use crate::output::Reporter;

/// TAP 13 reporter — writes one TAP document per call.
#[derive(Debug, Clone, Copy, Default)]
pub struct TapReporter;

impl Reporter for TapReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let diagnostics = value
            .get("diagnostics")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                anyhow::Error::new(InvalidInput::new(
                    "TAP reporter expects an object with a `diagnostics` array; \
                     selecting `-F tap` is only valid for the M8+ lint commands",
                ))
            })?;
        let tap = build_tap(diagnostics);
        w.write_all(tap.as_bytes())?;
        Ok(())
    }
}

/// Render a TAP 13 document from a slice of diagnostic values, each shaped as
/// `{ rule_id, severity, message, path, line, col, references }`.
///
/// Pulled out of the [`Reporter::report`] impl so unit tests can exercise the
/// rendering without going through a `&mut dyn Write`.
#[must_use]
pub fn build_tap(diagnostics: &[serde_json::Value]) -> String {
    let total = diagnostics.len();
    let mut out = String::new();
    out.push_str("TAP version 13\n");
    out.push_str(&format!("1..{total}\n"));
    for (idx, d) in diagnostics.iter().enumerate() {
        write_diagnostic(&mut out, idx + 1, d);
    }
    // Footer: under TAP semantics every reported diagnostic is a failure
    // (we never emit `ok`), so the two numbers are always equal. Including
    // the footer matches the design D9 example and keeps the output
    // self-describing for human readers. Switch to the singular form when
    // there's exactly one diagnostic so the line reads naturally.
    let test_word = if total == 1 { "test" } else { "tests" };
    let failure_word = if total == 1 { "failure" } else { "failures" };
    out.push_str(&format!("# {total} {test_word}, {total} {failure_word}\n"));
    out
}

fn write_diagnostic(out: &mut String, idx: usize, d: &serde_json::Value) {
    let rule_id = d.get("rule_id").and_then(|s| s.as_str()).unwrap_or("");
    let message = d.get("message").and_then(|s| s.as_str()).unwrap_or("");
    let severity = normalize_severity(d.get("severity").and_then(|s| s.as_str()));
    let path = d.get("path").and_then(|s| s.as_str()).unwrap_or("");
    let line = d
        .get("line")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(1);
    let col = d
        .get("col")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(1);

    // The TAP "test result" line — consumers parse this for the failure
    // count. We always emit `not ok` because every diagnostic represents a
    // finding under TAP semantics; severity is captured in the YAML block
    // below.
    //
    // `\n` inside the message would split the `not ok` line — strip those
    // out so the test description stays single-line. Multi-line context
    // belongs in the YAML block.
    let single_line_message = message.replace(['\n', '\r'], " ");
    out.push_str(&format!(
        "not ok {idx} - {rule_id}: {single_line_message}\n"
    ));

    // Diagnostic YAML block — TAP 13 wraps structured per-test diagnostic
    // data between `---` and `...`. Each line is indented by two spaces.
    out.push_str("  ---\n");
    out.push_str(&format!("  severity: {severity}\n"));
    out.push_str(&format!("  file: {path}\n"));
    out.push_str(&format!("  line: {line}\n"));
    out.push_str(&format!("  col: {col}\n"));
    out.push_str(&format!(
        "  message: \"{}\"\n",
        yaml_escape_double_quoted(message)
    ));
    if let Some(refs) = d.get("references").and_then(|v| v.as_array())
        && !refs.is_empty()
    {
        out.push_str("  references:\n");
        for r in refs {
            if let Some(url) = r.as_str() {
                out.push_str(&format!("    - \"{}\"\n", yaml_escape_double_quoted(url)));
            }
        }
    }
    out.push_str("  ...\n");
}

/// Normalise a producer severity for the YAML block.
///
/// Accepts `error`, `warn` (canonical), `warning` (alternate spelling), and
/// `info`. Anything else collapses to `info` — matches the SARIF / JUnit
/// reporters' "lenient default" discipline so a future producer with a novel
/// severity surfaces visibly without escalation.
fn normalize_severity(sev: Option<&str>) -> &'static str {
    match sev {
        Some("error") => "error",
        Some("warn") | Some("warning") => "warn",
        Some("info") => "info",
        _ => "info",
    }
}

/// Escape a string for inclusion inside a YAML double-quoted scalar.
///
/// YAML double-quoted strings interpret `\` and `"`, plus C-style escapes for
/// control characters. This helper handles the subset needed for diagnostic
/// messages and reference URLs: backslash, double quote, newline, carriage
/// return, and tab. Anything else passes through unchanged.
fn yaml_escape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Render a TAP document via [`TapReporter`] and return the produced
    /// bytes as a `String`.
    fn tap_text_for(input: serde_json::Value) -> String {
        let mut buf: Vec<u8> = Vec::new();
        TapReporter
            .report(&input, &mut buf)
            .expect("TapReporter::report must succeed");
        String::from_utf8(buf).expect("TAP output must be valid UTF-8")
    }

    #[test]
    fn rejects_value_without_diagnostics_array() {
        // Mirrors `SarifReporter::rejects_value_without_diagnostics_array`
        // and `JunitReporter::rejects_value_without_diagnostics_array` —
        // a top-level array (or any value missing the `diagnostics` field)
        // is an `InvalidInput` so the exit-code mapper picks 6.
        let mut buf: Vec<u8> = Vec::new();
        let err = TapReporter
            .report(&serde_json::json!([]), &mut buf)
            .unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker, got: {err:?}"
        );
    }

    #[test]
    fn renders_empty_diagnostics_to_zero_plan() {
        // Empty plan must still be a syntactically valid TAP 13 document so
        // `prove` and similar consumers don't choke on a clean lint pass.
        let out = tap_text_for(serde_json::json!({"diagnostics": []}));
        assert_eq!(out, "TAP version 13\n1..0\n# 0 tests, 0 failures\n");
    }

    #[test]
    fn one_diagnostic_renders_not_ok_with_yaml_block() {
        let out = tap_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "k8s.no-latest-tag",
                "severity": "error",
                "message": "Container 'app' uses :latest tag",
                "path": "k8s/deploy.yaml",
                "line": 12,
                "col": 1,
                "references": ["https://kubernetes.io/example"],
            }]
        }));
        assert!(out.starts_with("TAP version 13\n1..1\n"));
        assert!(out.contains("not ok 1 - k8s.no-latest-tag: Container 'app' uses :latest tag"));
        // YAML block carries severity, location, message, and references.
        assert!(out.contains("  ---\n"));
        assert!(out.contains("  severity: error\n"));
        assert!(out.contains("  file: k8s/deploy.yaml\n"));
        assert!(out.contains("  line: 12\n"));
        assert!(out.contains("  col: 1\n"));
        assert!(out.contains("  message: \"Container 'app' uses :latest tag\"\n"));
        assert!(out.contains("  references:\n"));
        assert!(out.contains("    - \"https://kubernetes.io/example\"\n"));
        assert!(out.contains("  ...\n"));
        // Footer prints both numbers — they're always equal under TAP
        // semantics because every diagnostic is reported as `not ok`. The
        // singular form ("test" / "failure") is used when there's exactly one.
        assert!(out.ends_with("# 1 test, 1 failure\n"));
    }

    #[test]
    fn two_diagnostics_are_numbered_one_and_two() {
        let out = tap_text_for(serde_json::json!({
            "diagnostics": [
                {
                    "rule_id": "a.rule", "severity": "error",
                    "message": "first", "path": "a.yaml", "line": 1, "col": 1,
                },
                {
                    "rule_id": "b.rule", "severity": "warn",
                    "message": "second", "path": "b.yaml", "line": 2, "col": 1,
                },
            ]
        }));
        assert!(out.contains("1..2"));
        assert!(out.contains("not ok 1 - a.rule: first"));
        assert!(out.contains("not ok 2 - b.rule: second"));
        // Order matters: parsers like `prove` rely on the indices matching
        // the plan and TAP itself is order-significant.
        let pos_one = out.find("not ok 1").expect("not ok 1 must appear");
        let pos_two = out.find("not ok 2").expect("not ok 2 must appear");
        assert!(
            pos_one < pos_two,
            "diagnostics must be numbered in input order, got: {out}"
        );
        // Both diagnostics also bumped the footer's failure count.
        assert!(out.ends_with("# 2 tests, 2 failures\n"));
    }

    #[test]
    fn special_chars_in_message_are_yaml_escaped() {
        // The message contains a colon, a backslash, a literal newline, and
        // a double quote — the YAML block uses double-quoted scalars, so the
        // reporter must escape `\` → `\\`, `"` → `\"`, `\n` → `\n` literal.
        // Without this, a downstream YAML parser would either reject the
        // block (unbalanced quote) or split the value mid-message.
        let out = tap_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "rule.x",
                "severity": "error",
                // \\n means backslash+n (NOT a literal newline) in the JSON
                // literal; the reporter must preserve it verbatim under
                // double-quote escaping (`\\` → `\\\\` then `n` → `n`).
                "message": "foo: bar with \\n and \"quote\"",
                "path": "p", "line": 1, "col": 1,
            }]
        }));
        // YAML block must re-encode backslash and quote.
        assert!(
            out.contains("message: \"foo: bar with \\\\n and \\\"quote\\\"\""),
            "yaml-escaped message missing or wrong, got: {out}"
        );
        // The single-line `not ok` description preserves the raw bytes (no
        // YAML escaping applies to the description).
        assert!(out.contains("not ok 1 - rule.x: foo: bar with \\n and \"quote\""));
    }

    #[test]
    fn newline_in_message_is_collapsed_on_test_line_and_escaped_in_yaml() {
        // A literal `\n` in the message must NOT split the `not ok` line —
        // that would break TAP's line-orientation. The reporter collapses
        // `\n`/`\r` to a single space on the description line and writes the
        // canonical `\n` escape inside the YAML block.
        let out = tap_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "rule.x",
                "severity": "error",
                "message": "first line\nsecond line",
                "path": "p", "line": 1, "col": 1,
            }]
        }));
        assert!(out.contains("not ok 1 - rule.x: first line second line"));
        assert!(out.contains("message: \"first line\\nsecond line\""));
    }

    #[test]
    fn references_field_renders_as_yaml_sequence() {
        // Multiple references render as a YAML sequence inside the diagnostic
        // block. Important for downstream tools that show clickable links.
        let out = tap_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "rule.x", "severity": "error",
                "message": "msg", "path": "p", "line": 1, "col": 1,
                "references": [
                    "https://example.com/a",
                    "https://example.com/b",
                ],
            }]
        }));
        assert!(out.contains("  references:\n"));
        assert!(out.contains("    - \"https://example.com/a\"\n"));
        assert!(out.contains("    - \"https://example.com/b\"\n"));
    }

    #[test]
    fn missing_references_field_omits_references_block() {
        // No `references` field → no `references:` line in the YAML block.
        let out = tap_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "rule.x", "severity": "error",
                "message": "msg", "path": "p", "line": 1, "col": 1,
            }]
        }));
        assert!(
            !out.contains("references:"),
            "missing references field must not produce a references block, got: {out}"
        );
    }

    #[test]
    fn build_tap_renders_without_writer() {
        // `build_tap` is the helper unit-test path so individual tests can
        // assert on the rendered TAP without going through `&mut dyn Write`.
        let tap = build_tap(&[serde_json::json!({
            "rule_id": "x.y", "severity": "error",
            "message": "msg", "path": "p", "line": 1, "col": 1,
        })]);
        assert!(tap.starts_with("TAP version 13\n"));
        assert!(tap.ends_with("# 1 test, 1 failure\n"));
    }

    #[test]
    fn normalize_severity_collapses_unknown_to_info() {
        assert_eq!(normalize_severity(None), "info");
        assert_eq!(normalize_severity(Some("garbage")), "info");
        assert_eq!(normalize_severity(Some("error")), "error");
        assert_eq!(normalize_severity(Some("warn")), "warn");
        assert_eq!(normalize_severity(Some("warning")), "warn");
        assert_eq!(normalize_severity(Some("info")), "info");
    }
}
