//! M11 Phase 3 — `dq lint --rules @std/jsonschema/...` end-to-end.
//!
//! Pins the spec scenario "Schema variant parses + emits diagnostic"
//! through the production CLI dispatch path. The rule is the embedded
//! `@std/jsonschema/kubernetes-crd-shape` (an inline `check.schema:`
//! variant); the input is a kubernetes manifest missing
//! `metadata.name`, which the schema flags as a required-property
//! violation.

use std::io::Write;

use clap::Parser;
use tempfile::NamedTempFile;

#[test]
fn lint_with_kubernetes_crd_shape_emits_diagnostic_for_invalid_crd() {
    // Manifest missing `metadata.name` — the schema declares
    // `metadata.name` is required, so the rule must fire.
    let bad_crd = r#"apiVersion: apps/v1
kind: Deployment
metadata: {}
spec:
  replicas: 1
"#;
    let mut doc_tmp = NamedTempFile::with_suffix(".yaml").expect("doc tmp");
    doc_tmp.write_all(bad_crd.as_bytes()).expect("write doc");
    let doc_tmp = doc_tmp.into_temp_path();

    let cli = dq::Cli::try_parse_from([
        "dq",
        "-F",
        "json",
        "lint",
        "--rules",
        "@std/jsonschema",
        doc_tmp.to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    // The rule severity is `error`, so `dq lint` should signal a
    // validate-fail exit. We test the marker by checking the result
    // type rather than the exit code (the in-process `dq::run` returns
    // the marker error so the caller can route it).
    let result = dq::run(&cli, false, &mut out, &mut err);
    assert!(
        result.is_err(),
        "lint should fail because the schema rule fires on a missing metadata.name; \
         stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err),
    );
    // Confirm the JSON output (written before the marker error) carries
    // the expected diagnostic shape: at least one entry with
    // rule_id == "jsonschema.kubernetes-crd-shape".
    let parsed: serde_json::Value = serde_json::from_slice(&out)
        .unwrap_or_else(|jerr| panic!("expected valid JSON, got: {jerr}\n{out:?}"));
    let diagnostics = parsed
        .get("diagnostics")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected diagnostics array, got: {parsed}"));
    let crd_diags: Vec<&serde_json::Value> = diagnostics
        .iter()
        .filter(|d| {
            d.get("rule_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == "jsonschema.kubernetes-crd-shape")
        })
        .collect();
    assert!(
        !crd_diags.is_empty(),
        "expected at least one jsonschema.kubernetes-crd-shape diagnostic; got: {parsed}"
    );
}

#[test]
fn lint_with_kubernetes_crd_shape_silent_for_valid_crd() {
    let good_crd = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-app
spec:
  replicas: 1
"#;
    let mut doc_tmp = NamedTempFile::with_suffix(".yaml").expect("doc tmp");
    doc_tmp.write_all(good_crd.as_bytes()).expect("write doc");
    let doc_tmp = doc_tmp.into_temp_path();

    let cli = dq::Cli::try_parse_from([
        "dq",
        "-F",
        "json",
        "lint",
        "--rules",
        "@std/jsonschema",
        doc_tmp.to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    // We intentionally do NOT assert on the overall `dq::run` result.
    // `--rules @std/jsonschema` runs every rule in the namespace, so a
    // future schema rule unrelated to `kubernetes-crd-shape` (e.g.
    // `helm-values-against-schema`, `openapi-3.1-shape`) firing on this
    // manifest would flip `result` to `Err(LintFail)` even though the
    // rule under test stays correct. Scope the assertion to the rule
    // this test names: zero diagnostics with
    // `rule_id == "jsonschema.kubernetes-crd-shape"`. Other rules'
    // diagnostics, if any, pass through without affecting the assertion.
    let _ = dq::run(&cli, false, &mut out, &mut err);
    let parsed: serde_json::Value = serde_json::from_slice(&out)
        .unwrap_or_else(|jerr| panic!("expected valid JSON, got: {jerr}\n{out:?}"));
    let diagnostics = parsed
        .get("diagnostics")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected diagnostics array, got: {parsed}"));
    let crd_diags: Vec<&serde_json::Value> = diagnostics
        .iter()
        .filter(|d| {
            d.get("rule_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == "jsonschema.kubernetes-crd-shape")
        })
        .collect();
    assert!(
        crd_diags.is_empty(),
        "expected zero jsonschema.kubernetes-crd-shape diagnostics for a well-formed \
         CRD manifest; got: {crd_diags:?}; stderr={:?}",
        String::from_utf8_lossy(&err),
    );
}
