//! SARIF 2.1.0 output reporter.
//!
//! Per the add-distribution section of `docs/decisions.md`, the
//! reporter expects an input value shaped as `{ "diagnostics": [...] }`
//! where each diagnostic carries `path`, `line`, `col`, `message`,
//! `severity` and `rule_id`. M6 ships exactly one consumer — the `validate`
//! handler — and M8's lint engine reuses the same shape so the two
//! producers don't diverge. Next to `diagnostics` the envelope may carry
//! the rule catalogue the reporter asks for through
//! [`Reporter::wants_rule_catalog`]: `"rules": [{ "id", "description",
//! "references" }]`.
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
//!     "tool": { "driver": {
//!       "name": "dq", "version": "X.Y.Z", "informationUri": "https://github.com/mazuninky/dq",
//!       "rules": [{
//!         "id": "k8s.no-latest-tag",
//!         "shortDescription": { "text": "..." },
//!         "fullDescription": { "text": "..." },
//!         "helpUri": "https://..."
//!       }]
//!     } },
//!     "results": [
//!       {
//!         "ruleId": "k8s.no-latest-tag",
//!         "ruleIndex": 0,
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
//!
//! # Rules
//!
//! A result's `ruleId` is its diagnostic's `rule_id` — the rule's `id:`
//! (`k8s.no-latest-tag`), a plugin's id, `<outer>.parse-failed`, or
//! `validate.parse` — and `ruleIndex` points at its descriptor in
//! `tool.driver.rules`. That array describes each rule the results name,
//! once, in order of first appearance; a run without results has an empty
//! one. A descriptor takes from the catalogue entry with the same id:
//!
//! - `shortDescription` — the first sentence of the description's first
//!   paragraph, its lines joined with spaces;
//! - `fullDescription` — the whole description, trimmed, when it says more
//!   than the short one;
//! - `helpUri` — the first reference that is an `http://` / `https://` URL.
//!
//! A rule without a catalogue entry (a plugin's, a composite's
//! `parse-failed`) is described by its `id` alone. A diagnostic without a
//! `rule_id` gets neither `ruleId` nor `ruleIndex`; no producer emits one.

use std::io::Write;

use indexmap::IndexSet;

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
        let catalog = value
            .get("rules")
            .and_then(|v| v.as_array())
            .map_or(&[][..], Vec::as_slice);
        let sarif = build_sarif(diagnostics, catalog);
        serde_json::to_writer_pretty(&mut *w, &sarif)?;
        w.write_all(b"\n")?;
        Ok(())
    }

    fn wants_rule_catalog(&self) -> bool {
        true
    }
}

/// Build a SARIF 2.1.0 `serde_json::Value` from a slice of diagnostic
/// values, each shaped as `{ path, line, col, message, severity, rule_id }`,
/// and the rule catalogue `[{ id, description, references }]` the
/// `tool.driver.rules` descriptors are drawn from (see the module docs).
///
/// Pulled out of the `Reporter::report` impl so unit tests can exercise the
/// shape conversion without going through a `&mut dyn Write`.
#[must_use]
pub fn build_sarif(
    diagnostics: &[serde_json::Value],
    catalog: &[serde_json::Value],
) -> serde_json::Value {
    // The rule ids the results name, in order of first use; a rule's
    // position here is its `ruleIndex`.
    let mut rule_ids: IndexSet<&str> = IndexSet::new();
    let results: Vec<serde_json::Value> = diagnostics
        .iter()
        .map(|d| {
            let mut result = serde_json::Map::new();
            if let Some(id) = d.get("rule_id").and_then(|s| s.as_str()) {
                let (index, _) = rule_ids.insert_full(id);
                result.insert("ruleId".to_owned(), serde_json::Value::from(id));
                result.insert("ruleIndex".to_owned(), serde_json::Value::from(index));
            }
            result.extend(diagnostic_to_result(d));
            serde_json::Value::Object(result)
        })
        .collect();
    let rules: Vec<serde_json::Value> = rule_ids
        .iter()
        .map(|id| rule_descriptor(id, catalog))
        .collect();
    serde_json::json!({
        "$schema": "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.5.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "dq",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/mazuninky/dq",
                    "rules": rules,
                }
            },
            "results": results,
        }],
    })
}

/// The `reportingDescriptor` of rule `id`, described from the first
/// catalogue entry with that id; by `id` alone when there is none.
fn rule_descriptor(id: &str, catalog: &[serde_json::Value]) -> serde_json::Value {
    let mut descriptor = serde_json::Map::new();
    descriptor.insert("id".to_owned(), serde_json::Value::from(id));
    let Some(entry) = catalog
        .iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id))
    else {
        return serde_json::Value::Object(descriptor);
    };
    let description = entry
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim();
    let short = first_sentence(description);
    if !short.is_empty() {
        descriptor.insert("shortDescription".to_owned(), text_message(&short));
        if short != collapse_whitespace(description) {
            descriptor.insert("fullDescription".to_owned(), text_message(description));
        }
    }
    let help_uri = entry
        .get("references")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str())
        .find(|r| r.starts_with("https://") || r.starts_with("http://"));
    if let Some(uri) = help_uri {
        descriptor.insert("helpUri".to_owned(), serde_json::Value::from(uri));
    }
    serde_json::Value::Object(descriptor)
}

/// A SARIF `multiformatMessageString` holding plain `text`.
fn text_message(text: &str) -> serde_json::Value {
    serde_json::json!({ "text": text })
}

/// The first sentence of the first paragraph of `description`, its lines
/// joined with single spaces.
///
/// A sentence ends at `.`, `!` or `?` followed by whitespace and an
/// upper-case letter, so `e.g. via` and `1.22+` do not end one; without
/// such an end the whole paragraph is the sentence.
fn first_sentence(description: &str) -> String {
    let paragraph = description
        .split("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty())
        .unwrap_or_default();
    let joined = collapse_whitespace(paragraph);
    let chars: Vec<(usize, char)> = joined.char_indices().collect();
    for window in chars.windows(3) {
        let [(at, end), (_, space), (_, next)] = window else {
            continue;
        };
        if matches!(end, '.' | '!' | '?') && *space == ' ' && next.is_uppercase() {
            return joined[..at + end.len_utf8()].to_owned();
        }
    }
    joined
}

/// `text` with every run of whitespace, line breaks included, made one
/// space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `level`, `message` and `locations` of the result for `d`.
fn diagnostic_to_result(d: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
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

    let mut result = serde_json::Map::new();
    result.insert("level".to_owned(), serde_json::Value::from(level));
    result.insert("message".to_owned(), text_message(&message));
    result.insert(
        "locations".to_owned(),
        serde_json::json!([{
            "physicalLocation": {
                "artifactLocation": { "uri": path },
                "region": {
                    "startLine": start_line,
                    "startColumn": start_column,
                }
            }
        }]),
    );
    result
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
                "rule_id": "validate.parse",
            }],
            "rules": [{
                "id": "validate.parse",
                "description": "The file does not parse in its format.",
                "references": [],
            }],
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

    // -----------------------------------------------------------------
    // Rule ids and `tool.driver.rules`.
    // -----------------------------------------------------------------

    #[test]
    fn reporter_asks_for_the_rule_catalog() {
        assert!(SarifReporter.wants_rule_catalog());
    }

    #[test]
    fn rule_without_catalog_entry_is_described_by_id_alone() {
        let out = sarif_for(serde_json::json!({
            "diagnostics": [
                { "path": "a", "message": "x", "rule_id": "plugin.p" },
                { "path": "a", "message": "y", "rule_id": "plugin.p" },
            ]
        }));
        assert_eq!(
            out["runs"][0]["tool"]["driver"]["rules"],
            serde_json::json!([{ "id": "plugin.p" }])
        );
        for result in out["runs"][0]["results"].as_array().unwrap() {
            assert_eq!(result["ruleId"], "plugin.p");
            assert_eq!(result["ruleIndex"], 0);
        }
    }

    #[test]
    fn diagnostic_without_rule_id_gets_no_rule_fields() {
        let out = sarif_for(serde_json::json!({
            "diagnostics": [{ "path": "a", "message": "x" }]
        }));
        let result = out["runs"][0]["results"][0].as_object().unwrap();
        assert!(!result.contains_key("ruleId"), "{result:?}");
        assert!(!result.contains_key("ruleIndex"), "{result:?}");
        assert_eq!(
            out["runs"][0]["tool"]["driver"]["rules"],
            serde_json::json!([])
        );
    }

    #[test]
    fn rule_descriptor_takes_the_first_http_reference_as_help_uri() {
        let catalog = [serde_json::json!({
            "id": "r", "description": "",
            "references": ["RFC 6901", "ftp://x", "http://a.example/r", "https://b.example"],
        })];
        assert_eq!(
            rule_descriptor("r", &catalog),
            serde_json::json!({ "id": "r", "helpUri": "http://a.example/r" })
        );
    }

    #[test]
    fn first_sentence_ends_at_a_full_stop_before_a_capital() {
        let cases = [
            ("One. Two.", "One."),
            ("Set it (e.g. via env). Then go.", "Set it (e.g. via env)."),
            (
                "Kubernetes 1.22+ removed it.\nSee docs.",
                "Kubernetes 1.22+ removed it.",
            ),
            (
                "Wrapped\n  over lines\n\nNext paragraph.",
                "Wrapped over lines",
            ),
            ("Really? Yes.", "Really?"),
            (
                "\n\n  Leading blank paragraph.  ",
                "Leading blank paragraph.",
            ),
            ("", ""),
        ];
        for (description, expected) in cases {
            assert_eq!(first_sentence(description), expected, "{description:?}");
        }
    }
}
