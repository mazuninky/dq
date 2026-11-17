//! JUnit XML output reporter.
//!
//! Per design D8 in `openspec/changes/add-exec-engine/design.md`, the reporter
//! consumes the canonical lint-engine output shape `{ "diagnostics": [...] }`
//! — the same shape produced by `dq_lint::Diagnostic::to_serde_json` and
//! consumed by [`crate::output::sarif::SarifReporter`] and
//! [`crate::output::tap::TapReporter`].
//!
//! The XML is hand-rolled (no `quick-xml`) — the schema is small and stable,
//! and the workspace deliberately keeps `dq-cli` dependency-light. Anything
//! that is not a `{ "diagnostics": [...] }` value errors with
//! [`crate::error::InvalidInput`] so the exit-code mapper picks 6, matching
//! the SARIF reporter's "wrong shape" path.
//!
//! # Output shape
//!
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <testsuites>
//!   <testsuite name="dq-lint" tests="1" failures="1" errors="0" skipped="0">
//!     <testcase classname="k8s/deploy.yaml" name="k8s.no-latest-tag">
//!       <failure type="error" message="...">k8s/deploy.yaml:12:1
//! Container 'app' uses :latest tag
//! References:
//!   - https://kubernetes.io/...</failure>
//!     </testcase>
//!   </testsuite>
//! </testsuites>
//! ```
//!
//! Severity → `<failure type="...">` mapping: `error → "error"`,
//! `warn` / `warning` → `"warning"`, `info` → `"info"`, anything unknown
//! collapses onto `"info"` (matching the SARIF reporter's "lenient default"
//! discipline — never escalate an unknown severity into something more
//! dangerous than the producer asked for).
//!
//! Per the M8 task spec, the reporter emits one `<testcase>` per diagnostic
//! and does NOT synthesize "passing" testcases for files with no findings —
//! the canonical input shape only carries diagnostics, so the reporter has no
//! visibility into clean files. The lint command driver is responsible for
//! generating "synthetic pass" diagnostics if a JUnit consumer needs to count
//! clean files.

use std::io::Write;

use crate::error::InvalidInput;
use crate::output::Reporter;

/// JUnit XML reporter — writes one `<testsuites>` document per call.
#[derive(Debug, Clone, Copy, Default)]
pub struct JunitReporter;

impl Reporter for JunitReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let diagnostics = value
            .get("diagnostics")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                anyhow::Error::new(InvalidInput::new(
                    "JUnit reporter expects an object with a `diagnostics` array; \
                     selecting `-F junit` is only valid for the M8+ lint commands",
                ))
            })?;
        let xml = build_junit(diagnostics);
        w.write_all(xml.as_bytes())?;
        Ok(())
    }
}

/// Render a JUnit XML document from a slice of diagnostic values, each shaped
/// as `{ rule_id, severity, message, path, line, col, references }`.
///
/// Pulled out of the [`Reporter::report`] impl so unit tests can exercise the
/// XML rendering without going through a `&mut dyn Write`.
#[must_use]
pub fn build_junit(diagnostics: &[serde_json::Value]) -> String {
    let total = diagnostics.len();
    let failures = diagnostics
        .iter()
        .filter(|d| {
            let sev = d.get("severity").and_then(|s| s.as_str()).unwrap_or("");
            // JUnit `failures` count covers `error` and `warn`; `info`
            // findings are reported as `<failure type="info">` but do NOT
            // bump the suite-level failure count (matching the convention
            // used by GitLab CI / Jenkins where info-level findings do not
            // fail the build).
            matches!(sev, "error" | "warn" | "warning")
        })
        .count();

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<testsuites>\n");
    out.push_str(&format!(
        "  <testsuite name=\"dq-lint\" tests=\"{total}\" failures=\"{failures}\" errors=\"0\" skipped=\"0\">\n"
    ));
    for d in diagnostics {
        write_testcase(&mut out, d);
    }
    out.push_str("  </testsuite>\n");
    out.push_str("</testsuites>\n");
    out
}

fn write_testcase(out: &mut String, d: &serde_json::Value) {
    let rule_id = d.get("rule_id").and_then(|s| s.as_str()).unwrap_or("");
    let path = d.get("path").and_then(|s| s.as_str()).unwrap_or("");
    let message = d.get("message").and_then(|s| s.as_str()).unwrap_or("");
    let severity = d.get("severity").and_then(|s| s.as_str());
    let failure_type = map_failure_type(severity);
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

    out.push_str(&format!(
        "    <testcase classname=\"{}\" name=\"{}\">\n",
        escape_attribute(path),
        escape_attribute(rule_id),
    ));
    // Failure body: `<file>:<line>:<col>\n<message>` and an optional
    // `References:` block when the diagnostic carries non-empty references.
    let mut body = format!("{path}:{line}:{col}\n{message}");
    if let Some(refs) = d.get("references").and_then(|v| v.as_array())
        && !refs.is_empty()
    {
        body.push_str("\nReferences:");
        for r in refs {
            if let Some(url) = r.as_str() {
                body.push_str("\n  - ");
                body.push_str(url);
            }
        }
    }
    out.push_str(&format!(
        "      <failure type=\"{}\" message=\"{}\">{}</failure>\n",
        escape_attribute(failure_type),
        escape_attribute(message),
        escape_text(&body),
    ));
    out.push_str("    </testcase>\n");
}

/// Map a producer severity onto the JUnit `<failure type="...">` enum.
///
/// Defaults to `"info"` for unknown severities — matches the SARIF reporter's
/// "lenient default" discipline so a future producer with a novel severity
/// surfaces visibly without being escalated into something more dangerous.
fn map_failure_type(sev: Option<&str>) -> &'static str {
    match sev {
        Some("error") => "error",
        Some("warn") | Some("warning") => "warning",
        Some("info") => "info",
        _ => "info",
    }
}

/// Escape XML-significant characters for an attribute value (between double
/// quotes). Attributes need the full set: `&`, `<`, `>`, `"`, `'`.
fn escape_attribute(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape XML-significant characters for text content (between tags). Text
/// only needs `&`, `<`, `>` — quotes are allowed unescaped.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Render a JUnit document via [`JunitReporter`] and return the produced
    /// bytes as a `String`.
    fn junit_text_for(input: serde_json::Value) -> String {
        let mut buf: Vec<u8> = Vec::new();
        JunitReporter
            .report(&input, &mut buf)
            .expect("JunitReporter::report must succeed");
        String::from_utf8(buf).expect("JUnit output must be valid UTF-8")
    }

    #[test]
    fn rejects_value_without_diagnostics_array() {
        // Mirrors `SarifReporter::rejects_value_without_diagnostics_array` —
        // a top-level array (or any value missing the `diagnostics` field) is
        // an `InvalidInput` so the exit-code mapper picks 6.
        let mut buf: Vec<u8> = Vec::new();
        let err = JunitReporter
            .report(&serde_json::json!([]), &mut buf)
            .unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker, got: {err:?}"
        );
    }

    #[test]
    fn renders_empty_diagnostics_to_empty_testsuite() {
        let out = junit_text_for(serde_json::json!({"diagnostics": []}));
        assert!(out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(out.contains("<testsuites>"));
        assert!(out.contains("tests=\"0\""));
        assert!(out.contains("failures=\"0\""));
        assert!(out.contains("errors=\"0\""));
        assert!(out.contains("skipped=\"0\""));
        // No `<testcase>` rows when the diagnostics array is empty.
        assert!(
            !out.contains("<testcase"),
            "empty diagnostics must produce no testcases, got: {out}"
        );
    }

    #[test]
    fn one_error_diagnostic_renders_as_failure_type_error() {
        let out = junit_text_for(serde_json::json!({
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
        assert!(out.contains("tests=\"1\""));
        assert!(out.contains("failures=\"1\""));
        assert!(
            out.contains("<testcase classname=\"k8s/deploy.yaml\" name=\"k8s.no-latest-tag\">")
        );
        assert!(out.contains("<failure type=\"error\""));
        assert!(out.contains("k8s/deploy.yaml:12:1"));
        assert!(out.contains("References:"));
        assert!(out.contains("https://kubernetes.io/example"));
    }

    #[test]
    fn warn_severity_renders_as_failure_type_warning() {
        // JUnit spells the failure type `warning`, while producers typically
        // emit the shorter `warn`. The reporter must translate so CI
        // dashboards see the canonical spelling.
        let out = junit_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "npm.has-license",
                "severity": "warn",
                "message": "package.json missing 'license' field",
                "path": "package.json",
                "line": 1,
                "col": 1,
            }]
        }));
        assert!(out.contains("<failure type=\"warning\""));
        // `warn` counts toward the suite-level `failures` attribute.
        assert!(out.contains("failures=\"1\""));
    }

    #[test]
    fn info_severity_renders_as_failure_type_info_without_failure_count() {
        // `info` findings are reported as `<failure type="info">` but do NOT
        // bump the suite-level `failures` count — matching the convention
        // where info-level findings do not fail the CI build.
        let out = junit_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "style.naming",
                "severity": "info",
                "message": "consider renaming",
                "path": "src/main.rs",
                "line": 1,
                "col": 1,
            }]
        }));
        assert!(out.contains("<failure type=\"info\""));
        assert!(out.contains("tests=\"1\""));
        assert!(
            out.contains("failures=\"0\""),
            "info-level findings must not bump suite-level failures, got: {out}"
        );
    }

    #[test]
    fn xml_special_chars_are_escaped_in_attributes_and_text() {
        // The message contains every XML-significant character; the reporter
        // must escape `&`, `<`, `>`, `"`, `'` in attributes and `&`, `<`,
        // `>` in text content. Without this, downstream XML parsers (Jenkins,
        // GitLab CI) would reject the document.
        let out = junit_text_for(serde_json::json!({
            "diagnostics": [{
                "rule_id": "rule.with-<>",
                "severity": "error",
                "message": "uses & < > \" ' chars",
                "path": "weird & file.yaml",
                "line": 1,
                "col": 1,
            }]
        }));
        // Attribute escaping — `&` always becomes `&amp;`.
        assert!(out.contains("classname=\"weird &amp; file.yaml\""));
        assert!(out.contains("name=\"rule.with-&lt;&gt;\""));
        assert!(out.contains("message=\"uses &amp; &lt; &gt; &quot; &apos; chars\""));
        // Text escaping — only `&`, `<`, `>` are escaped; `"` and `'` stay
        // unescaped to keep snapshots readable.
        assert!(out.contains("uses &amp; &lt; &gt; \" ' chars"));
    }

    #[test]
    fn multiple_diagnostics_on_different_files_produce_multiple_testcases() {
        let out = junit_text_for(serde_json::json!({
            "diagnostics": [
                {
                    "rule_id": "a.rule", "severity": "error",
                    "message": "first", "path": "a.yaml", "line": 1, "col": 1,
                },
                {
                    "rule_id": "b.rule", "severity": "warn",
                    "message": "second", "path": "b.yaml", "line": 2, "col": 1,
                },
                {
                    "rule_id": "c.rule", "severity": "info",
                    "message": "third", "path": "c.yaml", "line": 3, "col": 1,
                },
            ]
        }));
        assert!(out.contains("tests=\"3\""));
        // Two non-info findings → two failures.
        assert!(out.contains("failures=\"2\""));
        assert!(out.contains("classname=\"a.yaml\" name=\"a.rule\""));
        assert!(out.contains("classname=\"b.yaml\" name=\"b.rule\""));
        assert!(out.contains("classname=\"c.yaml\" name=\"c.rule\""));
        // Diagnostics come out in input order — important for consumer-side
        // deduplication on rule_id+path (same as the SARIF reporter).
        let pos_a = out.find("a.rule").expect("a.rule must appear");
        let pos_b = out.find("b.rule").expect("b.rule must appear");
        let pos_c = out.find("c.rule").expect("c.rule must appear");
        assert!(
            pos_a < pos_b && pos_b < pos_c,
            "diagnostics must come out in input order, got: {out}"
        );
    }

    #[test]
    fn build_junit_renders_without_writer() {
        // `build_junit` is the helper unit-test path so individual tests can
        // assert on the rendered XML without going through `&mut dyn Write`.
        let xml = build_junit(&[serde_json::json!({
            "rule_id": "x.y", "severity": "error",
            "message": "msg", "path": "p", "line": 1, "col": 1,
        })]);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.ends_with("</testsuites>\n"));
    }

    #[test]
    fn map_failure_type_collapses_unknown_to_info() {
        assert_eq!(map_failure_type(None), "info");
        assert_eq!(map_failure_type(Some("garbage")), "info");
        assert_eq!(map_failure_type(Some("error")), "error");
        assert_eq!(map_failure_type(Some("warn")), "warning");
        assert_eq!(map_failure_type(Some("warning")), "warning");
        assert_eq!(map_failure_type(Some("info")), "info");
    }
}
