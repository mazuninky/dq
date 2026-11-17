//! SARIF 2.1.0 output reporter.
//!
//! Per design D4 in `openspec/changes/add-distribution/design.md`, the
//! reporter expects an input value shaped as `{ "diagnostics": [...] }`
//! where each diagnostic carries `path`, `line`, `col`, `message`, and
//! `severity`. M6 ships exactly one consumer — the `validate` handler —
//! and M8's lint engine reuses the same shape so the two producers don't
//! diverge.
//!
//! Anything else (a top-level array, a scalar, an object missing the
//! `diagnostics` field) errors with [`crate::error::InvalidInput`] so the
//! exit-code mapper picks 6 — the same `BannedReporter` discipline used by
//! the M5 read-only formats.
//!
//! # Output shape
//!
//! ```json
//! {
//!   "$schema": "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.5.json",
//!   "version": "2.1.0",
//!   "runs": [{
//!     "tool": { "driver": { "name": "dq", "version": "X.Y.Z", "informationUri": "https://github.com/mazuninky/dq" } },
//!     "results": [
//!       {
//!         "level": "error",
//!         "message": { "text": "..." },
//!         "locations": [{ "physicalLocation": {
//!           "artifactLocation": { "uri": "..." },
//!           "region": { "startLine": 1, "startColumn": 1 }
//!         }}]
//!       }
//!     ]
//!   }]
//! }
//! ```
//!
//! Severity mapping: `"error"` → `"error"`, `"warn"` / `"warning"` →
//! `"warning"`, `"info"` / `"note"` → `"note"`. Anything else collapses to
//! `"warning"` so an unknown severity from a future producer never lands as
//! the more dangerous `"error"`.

use std::io::Write;

use crate::error::InvalidInput;
use crate::output::Reporter;

/// SARIF 2.1.0 reporter — writes a single-run SARIF document per call.
#[derive(Debug, Clone, Copy, Default)]
pub struct SarifReporter;

impl Reporter for SarifReporter {
    fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
        let diagnostics = value
            .get("diagnostics")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                anyhow::Error::new(InvalidInput::new(
                    "SARIF reporter expects an object with a `diagnostics` array; \
                     selecting `-F sarif` is only valid for `validate` and the M8+ lint commands",
                ))
            })?;
        let sarif = build_sarif(diagnostics);
        serde_json::to_writer_pretty(&mut *w, &sarif)?;
        w.write_all(b"\n")?;
        Ok(())
    }
}

/// Build a SARIF 2.1.0 `serde_json::Value` from a slice of diagnostic
/// values, each shaped as `{ path, line, col, message, severity }`.
///
/// Pulled out of the `Reporter::report` impl so unit tests can exercise the
/// shape conversion without going through a `&mut dyn Write`.
#[must_use]
pub fn build_sarif(diagnostics: &[serde_json::Value]) -> serde_json::Value {
    let results: Vec<serde_json::Value> = diagnostics.iter().map(diagnostic_to_result).collect();
    serde_json::json!({
        "$schema": "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.5.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "dq",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/mazuninky/dq",
                }
            },
            "results": results,
        }],
    })
}

fn diagnostic_to_result(d: &serde_json::Value) -> serde_json::Value {
    let level = map_severity(d.get("severity").and_then(|s| s.as_str()));
    let message = d
        .get("message")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_owned();
    let path = d
        .get("path")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_owned();
    // SARIF requires startLine >= 1 — clamp anything missing or zero up to
    // 1 so the report stays schema-valid even when a producer lacks
    // line/column information.
    let start_line = d
        .get("line")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(1);
    let start_column = d
        .get("col")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(1);

    serde_json::json!({
        "level": level,
        "message": { "text": message },
        "locations": [{
            "physicalLocation": {
                "artifactLocation": { "uri": path },
                "region": {
                    "startLine": start_line,
                    "startColumn": start_column,
                }
            }
        }]
    })
}

/// Map a producer-level severity string onto the SARIF `level` enum.
///
/// Defaults to `"warning"` — see module-level note for the rationale.
fn map_severity(level: Option<&str>) -> &'static str {
    match level {
        Some("error") => "error",
        Some("warn") | Some("warning") => "warning",
        Some("info") | Some("note") => "note",
        _ => "warning",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Render a SARIF document via [`SarifReporter`] and parse the JSON back
    /// out so tests can run structural assertions on a `serde_json::Value`.
    fn sarif_for(input: serde_json::Value) -> serde_json::Value {
        let mut buf: Vec<u8> = Vec::new();
        SarifReporter
            .report(&input, &mut buf)
            .expect("SarifReporter::report must succeed");
        serde_json::from_slice(&buf).expect("SARIF output must be valid JSON")
    }

    /// Render the SARIF document and return the produced JSON bytes as a
    /// `String`. Used by the snapshot test so `insta::assert_snapshot!`
    /// sees the exact bytes the reporter wrote — sidesteps the
    /// `arbitrary_precision`-flavoured `Number` representation that
    /// `assert_json_snapshot!` produces when the workspace turns that
    /// `serde_json` feature on.
    fn sarif_text_for(input: serde_json::Value) -> String {
        let mut buf: Vec<u8> = Vec::new();
        SarifReporter
            .report(&input, &mut buf)
            .expect("SarifReporter::report must succeed");
        String::from_utf8(buf).expect("SARIF output must be valid UTF-8")
    }

    #[test]
    fn rejects_value_without_diagnostics_array() {
        let mut buf: Vec<u8> = Vec::new();
        let err = SarifReporter
            .report(&serde_json::json!({"foo": 1}), &mut buf)
            .unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker, got: {err:?}"
        );
    }

    #[test]
    fn renders_empty_diagnostics_to_empty_results() {
        let out = sarif_for(serde_json::json!({"diagnostics": []}));
        assert_eq!(out["version"], "2.1.0");
        let results = out["runs"][0]["results"].as_array().expect("results array");
        assert!(results.is_empty(), "expected empty results, got: {out}");
    }

    #[test]
    fn renders_single_diagnostic_with_location() {
        let out = sarif_for(serde_json::json!({
            "diagnostics": [{
                "path": "config.yaml",
                "line": 3,
                "col": 7,
                "message": "unexpected token",
                "severity": "error",
            }]
        }));
        let result = &out["runs"][0]["results"][0];
        assert_eq!(result["level"], "error");
        assert_eq!(result["message"]["text"], "unexpected token");
        let region = &result["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"], 3);
        assert_eq!(region["startColumn"], 7);
    }

    #[test]
    fn map_severity_defaults_to_warning_for_unknown() {
        assert_eq!(map_severity(None), "warning");
        assert_eq!(map_severity(Some("garbage")), "warning");
        assert_eq!(map_severity(Some("error")), "error");
        assert_eq!(map_severity(Some("warn")), "warning");
        assert_eq!(map_severity(Some("info")), "note");
    }

    // -----------------------------------------------------------------
    // Severity coverage at the reporter level — checks that the level
    // mapping is wired through `report` end-to-end (not just through the
    // internal `map_severity` helper) and surfaces in the `level` field of
    // the rendered SARIF result.
    // -----------------------------------------------------------------

    #[test]
    fn warn_severity_renders_as_warning_level() {
        // SARIF spells the level `warning`, while producers typically write
        // the shorter `warn`. The mapping is the documented contract; a
        // regression would silently downgrade severity in CI dashboards.
        let out = sarif_for(serde_json::json!({
            "diagnostics": [{
                "path": "x.yaml", "line": 1, "col": 1,
                "message": "stale anchor", "severity": "warn",
            }]
        }));
        assert_eq!(out["runs"][0]["results"][0]["level"], "warning");
    }

    #[test]
    fn info_severity_renders_as_note_level() {
        // `info` from the producer collapses onto SARIF `note`.
        let out = sarif_for(serde_json::json!({
            "diagnostics": [{
                "path": "x.yaml", "line": 1, "col": 1,
                "message": "consider renaming", "severity": "info",
            }]
        }));
        assert_eq!(out["runs"][0]["results"][0]["level"], "note");
    }

    #[test]
    fn missing_severity_defaults_to_warning_level() {
        // No `severity` field at all — the reporter must default to
        // `warning` (NOT `error`) so an unknown producer never lands as
        // the more dangerous level. Mirrors the `map_severity` default.
        let out = sarif_for(serde_json::json!({
            "diagnostics": [{
                "path": "x.yaml", "line": 1, "col": 1,
                "message": "no severity",
            }]
        }));
        assert_eq!(out["runs"][0]["results"][0]["level"], "warning");
    }

    // -----------------------------------------------------------------
    // Multi-diagnostic ordering and document-level invariants.
    // -----------------------------------------------------------------

    #[test]
    fn renders_three_diagnostics_in_input_order() {
        // Multiple diagnostics in one batch must come out in input order
        // (not, e.g., sorted by severity or path). Producers — including
        // `validate` and the future M8 lint engine — emit diagnostics in
        // file traversal order; consumers like GitHub Code Scanning rely on
        // that ordering for deduplication.
        let out = sarif_for(serde_json::json!({
            "diagnostics": [
                { "path": "a.yaml", "line": 1, "col": 1, "message": "first", "severity": "error" },
                { "path": "b.yaml", "line": 2, "col": 1, "message": "second", "severity": "warn" },
                { "path": "c.yaml", "line": 3, "col": 1, "message": "third", "severity": "info" },
            ]
        }));
        let results = out["runs"][0]["results"].as_array().expect("results array");
        assert_eq!(results.len(), 3, "expected 3 results, got: {out}");
        assert_eq!(results[0]["message"]["text"], "first");
        assert_eq!(results[1]["message"]["text"], "second");
        assert_eq!(results[2]["message"]["text"], "third");
        // And the level mapping must hold per-row.
        assert_eq!(results[0]["level"], "error");
        assert_eq!(results[1]["level"], "warning");
        assert_eq!(results[2]["level"], "note");
    }

    #[test]
    fn output_round_trips_through_serde_json() {
        // Document-level invariant: the bytes the reporter writes parse back
        // as valid JSON without `arbitrary_precision` or other tweaks. This
        // is a no-op given how `serde_json::to_writer_pretty` works, but the
        // assertion gives a nice failure message if a future refactor
        // introduces a non-JSON output path (e.g. embedded raw bytes).
        let mut buf: Vec<u8> = Vec::new();
        SarifReporter
            .report(
                &serde_json::json!({
                    "diagnostics": [{
                        "path": "x.yaml", "line": 1, "col": 1,
                        "message": "anything", "severity": "error",
                    }]
                }),
                &mut buf,
            )
            .expect("report should succeed");
        let parsed: serde_json::Value =
            serde_json::from_slice(&buf).expect("SARIF output must be parseable JSON");
        assert_eq!(parsed["version"], "2.1.0");
        assert!(
            parsed["runs"].is_array(),
            "`runs` must be a JSON array; got: {parsed}",
        );
    }

    #[test]
    fn snapshot_known_good_sarif_document() {
        // Golden snapshot for the canonical fixture from the task spec —
        // pins the exact JSON shape so any structural change (renamed
        // field, reordered key in the prelude, new tool metadata) requires
        // an explicit `cargo insta review`. Filter out the dq version field
        // because it tracks `CARGO_PKG_VERSION` and would churn on every
        // release.
        //
        // Snapshot uses `assert_snapshot!` against the raw text bytes
        // (rather than `assert_json_snapshot!` against a `serde_json::Value`)
        // because the workspace enables `serde_json/arbitrary_precision`,
        // which makes `assert_json_snapshot!` render numbers as
        // `{"$serde_json::private::Number": "5"}` instead of `5`. The text
        // form is what the reporter actually writes to disk.
        let out = sarif_text_for(serde_json::json!({
            "diagnostics": [{
                "path": "config.yaml",
                "line": 5,
                "col": 12,
                "message": "expected mapping value",
                "severity": "error",
            }]
        }));
        insta::with_settings!({
            filters => vec![
                // Mask the dq version so the snapshot doesn't churn on
                // every CARGO_PKG_VERSION bump.
                (r#""version": "\d+\.\d+\.\d+""#, r#""version": "[VERSION]""#),
            ],
        }, {
            insta::assert_snapshot!("sarif_known_good_document", out);
        });
    }
}
