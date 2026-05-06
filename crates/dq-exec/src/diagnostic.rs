//! Structured diagnostic emitted by the lint engine.
//!
//! The serde-flat representation produced by [`Diagnostic::to_serde_json`]
//! is the canonical envelope — every reporter (console / json / sarif /
//! junit / tap) consumes the same `{ path, line, col, message, severity,
//! rule_id, references }` shape. M6's SARIF reporter (in
//! `crates/dq-cli/src/output/sarif.rs`) was the first consumer; M8's lint
//! engine is the second producer and re-uses the field names verbatim so
//! the two producers stay in sync.

use std::ops::Range;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::rule::RuleFix;

/// Severity classification for a [`Diagnostic`].
///
/// Serializes / deserializes as a lowercase string (`"error"` / `"warn"` /
/// `"info"`) so the YAML rule schema and the JSON diagnostic envelope
/// agree on the wire format.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The most severe class — drives lint exit code 4 (`VALIDATE_FAIL`).
    Error,
    /// Mid-tier severity — only drives a non-zero exit code under `--strict`.
    Warn,
    /// Informational — never drives a non-zero exit code.
    Info,
}

impl Severity {
    /// Stable lowercase string identifier for this severity.
    ///
    /// Used by the JSON envelope and the SARIF reporter's severity-mapping
    /// table. Must match the `#[serde(rename_all = "lowercase")]` rendering.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }
}

/// One violation report emitted by an evaluator run.
///
/// Carries the rule identity, the formatted message, an optional file
/// location, and the per-rule metadata needed by `dq explain`-style
/// consumers (`references`, `rule_id`).
///
/// `file == None` means the diagnostic isn't tied to a source artifact
/// (e.g. it was produced by a test runner). `to_serde_json` omits the
/// `path` field in that case.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// The `id:` of the rule that produced this diagnostic.
    pub rule_id: String,
    /// Severity classification (drives exit-code routing).
    pub severity: Severity,
    /// Formatted user-facing message — already templated through the
    /// `{{ .field }}` renderer.
    pub message: String,
    /// File the diagnostic refers to, if any.
    pub file: Option<Utf8PathBuf>,
    /// 1-based line. The serializer does not clamp `0` — that's the SARIF
    /// reporter's job (it must always emit `>= 1`).
    pub line: u32,
    /// 1-based column. Same non-clamping policy as `line`.
    pub col: u32,
    /// Byte range of the offending construct, when the parser tracks it.
    pub span: Option<Range<usize>>,
    /// External references for `dq explain` (URLs, RFC numbers, etc.).
    pub references: Vec<String>,
    /// Optional fix payload — the rule's typed [`RuleFix`] when present.
    /// `dq fix` consumes this through [`crate::Fixer`]; the lint reporters
    /// do not currently render it (see `dq fix` for autofix execution).
    pub fix: Option<RuleFix>,
}

impl Diagnostic {
    /// Render the diagnostic as the canonical JSON envelope.
    ///
    /// Output shape:
    ///
    /// ```json
    /// {
    ///   "path": "config.yaml",   // omitted when `file == None`
    ///   "line": 3,
    ///   "col": 7,
    ///   "message": "...",
    ///   "severity": "error",
    ///   "rule_id": "k8s.no-latest-tag",
    ///   "references": ["https://..."]
    /// }
    /// ```
    ///
    /// `line` and `col` round-trip as `serde_json::Value::Number` (not as
    /// strings). The SARIF reporter clamps `0` up to `1` itself, so this
    /// method emits the raw values without rewriting them.
    #[must_use]
    pub fn to_serde_json(&self) -> serde_json::Value {
        let mut object = serde_json::Map::new();
        if let Some(path) = self.file.as_ref() {
            object.insert(
                "path".to_owned(),
                serde_json::Value::String(path.to_string()),
            );
        }
        object.insert(
            "line".to_owned(),
            serde_json::Value::Number(self.line.into()),
        );
        object.insert("col".to_owned(), serde_json::Value::Number(self.col.into()));
        object.insert(
            "message".to_owned(),
            serde_json::Value::String(self.message.clone()),
        );
        object.insert(
            "severity".to_owned(),
            serde_json::Value::String(self.severity.as_str().to_owned()),
        );
        object.insert(
            "rule_id".to_owned(),
            serde_json::Value::String(self.rule_id.clone()),
        );
        let refs: Vec<serde_json::Value> = self
            .references
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect();
        object.insert("references".to_owned(), serde_json::Value::Array(refs));
        serde_json::Value::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn sample_diagnostic() -> Diagnostic {
        Diagnostic {
            rule_id: "k8s.no-latest-tag".to_owned(),
            severity: Severity::Error,
            message: "Container 'web' uses :latest tag".to_owned(),
            file: Some(Utf8PathBuf::from("deploy.yaml")),
            line: 12,
            col: 7,
            span: Some(0..32),
            references: vec!["https://kubernetes.io/docs/concepts/containers/images/".to_owned()],
            fix: None,
        }
    }

    #[test]
    fn severity_round_trips_via_serde_lowercase() {
        // Serialize each variant to a JSON scalar and round-trip back.
        for &(variant, expected) in &[
            (Severity::Error, "\"error\""),
            (Severity::Warn, "\"warn\""),
            (Severity::Info, "\"info\""),
        ] {
            let json = serde_json::to_string(&variant).expect("serialize");
            assert_eq!(json, expected, "severity must serialize lowercase");
            let back: Severity = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn severity_as_str_returns_lowercase_strings() {
        assert_eq!(Severity::Error.as_str(), "error");
        assert_eq!(Severity::Warn.as_str(), "warn");
        assert_eq!(Severity::Info.as_str(), "info");
    }

    #[test]
    fn to_serde_json_emits_canonical_shape() {
        let value = sample_diagnostic().to_serde_json();
        assert_eq!(value["path"], "deploy.yaml");
        assert_eq!(value["line"], 12);
        assert_eq!(value["col"], 7);
        assert_eq!(value["message"], "Container 'web' uses :latest tag");
        assert_eq!(value["severity"], "error");
        assert_eq!(value["rule_id"], "k8s.no-latest-tag");
        assert_eq!(
            value["references"][0],
            "https://kubernetes.io/docs/concepts/containers/images/"
        );
    }

    #[test]
    fn to_serde_json_omits_path_when_file_is_none() {
        // When the diagnostic carries no source-file association, the
        // envelope must omit the `path` field entirely (rather than emit
        // `null` or `""`). The SARIF reporter falls back to `""` for a
        // missing path; this keeps the JSON shape clean for renderers that
        // want to distinguish "unknown source" from "empty path".
        let mut diag = sample_diagnostic();
        diag.file = None;
        let value = diag.to_serde_json();
        let object = value.as_object().expect("envelope must be a JSON object");
        assert!(
            !object.contains_key("path"),
            "expected `path` to be omitted, got: {value}",
        );
    }

    #[test]
    fn to_serde_json_does_not_clamp_zero_line_or_col() {
        // The SARIF reporter is responsible for clamping `0` up to `1` — the
        // `Diagnostic::to_serde_json` envelope must emit raw values so
        // future reporters can choose their own policy.
        let mut diag = sample_diagnostic();
        diag.line = 0;
        diag.col = 0;
        let value = diag.to_serde_json();
        assert_eq!(value["line"], 0);
        assert_eq!(value["col"], 0);
    }

    #[test]
    fn to_serde_json_emits_empty_references_array_when_unset() {
        // Even with no references, the field is present as an empty array
        // — keeps consumer code uniform (no `Option<Vec<_>>` branching).
        let mut diag = sample_diagnostic();
        diag.references.clear();
        let value = diag.to_serde_json();
        assert!(
            value["references"].is_array(),
            "references must always be an array, got: {value}",
        );
        assert_eq!(value["references"].as_array().expect("array").len(), 0);
    }

    /// Static assertion that [`Diagnostic`] is `Send + Sync`.
    ///
    /// The lint engine wants to share diagnostics across rayon workers via
    /// `Arc<Vec<Diagnostic>>` once parallel evaluation lands; forgetting
    /// the bound surfaces here as a non-trivial trait-bound error.
    #[test]
    fn assert_diagnostic_send_sync() {
        fn assert_send_sync<T: Send + Sync>(_: &T) {}
        let diag = sample_diagnostic();
        assert_send_sync(&diag);
    }
}
