//! M8 lint dispatch — `-F` is OUTPUT format only, not the input parser.
//!
//! Regression test for the bug where `dq lint -F json file.yaml` passed
//! `cli.format.as_input_format_name()` to the lint handler, which forced
//! the YAML file through the JSON parser and surfaced as
//! `parse error in /tmp/...yaml: expected JSON value (object, array, scalar)`.
//!
//! The fix in `crates/dq-cli/src/lib.rs` mirrors the existing `convert` /
//! `diff` special case — `-F` selects the OUTPUT reporter only, and per-file
//! input format detection happens inside the lint pipeline via
//! `pick_format`. This test pins that contract by linting a YAML file with
//! `-F json` and asserting the output is a JSON document with the expected
//! `{"diagnostics": [...]}` shape (NOT a parse-error exit).
//!
//! Driven through `dq::run` in-process so the test has no binary-spawn
//! latency and integrates cleanly with the rest of the suite. The pattern
//! mirrors the M3+ smoke tests in `cli_smoke.rs`.

use std::io::Write;

use clap::Parser;
use tempfile::NamedTempFile;

/// `dq lint -F json file.yaml` must:
/// 1. Parse the YAML through the YAML parser (extension-driven), NOT the
///    JSON parser the `-F json` flag selects.
/// 2. Produce a JSON document on stdout with the canonical lint reporter
///    shape: a `diagnostics` array (possibly empty) plus optional summary
///    fields. The exact diagnostic content depends on whichever rule fires
///    against a `kind: Deployment` manifest, so we assert structure only.
///
/// Before the fix: this scenario failed with a YAML-parsed-as-JSON parse
/// error (exit 6, no JSON body). After the fix: stdout is valid JSON
/// containing `diagnostics`.
#[test]
fn lint_dash_f_json_against_yaml_does_not_force_json_input_parser() {
    // Inline rule pinned to YAML format so the test is independent of
    // `@std/k8s` resolution — that path depends on the embedded rules
    // catalogue and the user's `--rules` resolution layer, both of which
    // can vary across the M8 milestone iterations. An inline rule keeps
    // the assertion focused on the dispatch fix.
    let rule_yaml = r#"
id: test.always-fires
description: always fires for the regression smoke
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
"#;
    let mut rule_tmp = NamedTempFile::with_suffix(".yml").expect("rule tmp");
    rule_tmp
        .write_all(rule_yaml.as_bytes())
        .expect("write rule");

    // The doc is a minimal k8s-shaped YAML — nothing about the body matters
    // for the dispatch contract, but a structured map exercises the full
    // YAML parse path.
    let doc_yaml = r#"apiVersion: apps/v1
kind: Deployment
metadata: { name: web }
spec:
  template:
    spec:
      containers:
        - name: web
          image: web:latest
"#;
    let mut doc_tmp = NamedTempFile::with_suffix(".yaml").expect("doc tmp");
    doc_tmp.write_all(doc_yaml.as_bytes()).expect("write doc");

    let cli = dq::Cli::try_parse_from([
        "dq",
        "-F",
        "json",
        "lint",
        "--rules",
        rule_tmp.path().to_str().expect("UTF-8 rule path"),
        doc_tmp.path().to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    // The rule severity is `warn`, so the handler returns `Ok(())` without
    // `--strict` — the regression here is that BEFORE the fix, the call
    // returned a parse error long before any rule ran.
    let result = dq::run(&cli, false, &mut out, &mut err);
    assert!(
        result.is_ok(),
        "lint -F json over a YAML file must succeed (regression: \
         used to parse YAML as JSON and fail). got err={result:?}, \
         stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err),
    );

    // Reporter output must be valid JSON with the canonical lint shape.
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap_or_else(|jerr| {
        panic!(
            "lint -F json must emit valid JSON ({jerr}); stdout was:\n{}",
            String::from_utf8_lossy(&out),
        )
    });
    let diagnostics = parsed
        .get("diagnostics")
        .unwrap_or_else(|| panic!("expected `diagnostics` field, got: {parsed}"));
    let diagnostics = diagnostics
        .as_array()
        .unwrap_or_else(|| panic!("`diagnostics` must be an array, got: {parsed}"));
    assert!(
        !diagnostics.is_empty(),
        "the always-fires warn rule must produce at least one diagnostic; \
         got: {parsed}",
    );
}
