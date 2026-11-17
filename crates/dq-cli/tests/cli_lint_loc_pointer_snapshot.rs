//! Snapshot: `dq lint` output for a rule using the Phase 2 `loc.pointer`
//! resolver against a real YAML document.
//!
//! Phase 2 of `add-ir-foundation` introduces `loc.pointer`: a jq expression
//! whose string output is looked up against the IR's provenance map via
//! `Ir::line_col_for(pointer)` to derive a diagnostic's `(line, col)`. This
//! test pins the canonical lint reporter output for a rule that uses
//! `loc.pointer` end-to-end through `dq::run`.
//!
//! ## A note on the `(line, col)` values in the snapshot
//!
//! The lint dispatcher (in `crates/dq-cli/src/commands/lint_core.rs`)
//! routes YAML / JSON parsing through `load_document_for_lint`, which
//! dispatches to `parse_yaml_with_spans` / `parse_json_with_spans`.
//! Those parsers populate the `Document`'s [`SpanMap`] / `ProvenanceMap`,
//! and `Ir::line_col_for(&Pointer)` resolves the byte range to a
//! `(line, col)` pair. The evaluator's `loc.pointer` chain therefore
//! emits the source position of the offending leaf instead of the
//! fallback `(1, 1)`.
//!
//! Note: the YAML span builder records spans for **scalar values only**
//! (see `crates/dq-core/src/parsers/yaml_spans.rs` — "one ValueSpan per
//! scalar value"). Pointers that resolve to a map or sequence node have
//! no recorded span, so the `loc.pointer` chain still falls through to
//! `(1, 1)` for those. This test deliberately drills the pointer down to
//! the `name` scalar inside the offending container so the snapshot
//! exercises the resolved-span path.

use std::io::Write;

use clap::Parser;
use tempfile::NamedTempFile;

/// Path-stable snapshot of `dq lint -F json` over a YAML doc with one
/// `loc.pointer`-using rule.
#[test]
fn snapshot_dq_lint_loc_pointer_against_real_yaml() {
    // Inline rule body — staged to a tempfile so the test does not depend
    // on `@std` discovery (whose discovered_formats / auto-bind logic has
    // shifted across milestones). The body mirrors the Phase 2 migration
    // of `@std/k8s/image-pull-policy-always`: emit `{name, image, pointer}`
    // per offending container, route `loc.pointer` to the canonical RFC
    // 6901 pointer for the offending leaf.
    //
    // The pointer drills down to the container's `/name` scalar (rather
    // than the container map itself) because the YAML span builder only
    // records `ValueSpan`s for scalar leaves — see the file-level note
    // above and `crates/dq-core/src/parsers/yaml_spans.rs`.
    let rule_yaml = r#"
id: test.k8s.image-pull-policy-always
description: pin loc.pointer behaviour through dq lint
severity: warn
match:
  format: yaml
  filter: '.kind == "Deployment"'
check:
  jq: |
    (.spec.template.spec.containers // [])
    | to_entries[]
    | select(.value.imagePullPolicy == "Always"
             and (.value.image | tostring | test(":latest$") | not))
    | { name: .value.name,
        image: .value.image,
        pointer: ("/spec/template/spec/containers/" + (.key | tostring) + "/name") }
  message: "container '{{ .name }}' uses imagePullPolicy=Always with a pinned tag"
loc:
  pointer: '.pointer'
references:
  - https://kubernetes.io/docs/concepts/containers/images/#updating-images
"#;
    let mut rule_tmp = NamedTempFile::with_suffix(".yml").expect("rule tmp");
    rule_tmp
        .write_all(rule_yaml.as_bytes())
        .expect("write rule");
    // Close the file handle before the binary opens it. Same Windows
    // ergonomics fix the rest of the cli_*_smoke tests apply.
    let rule_tmp = rule_tmp.into_temp_path();

    // Doc — one offending container under the workload-shaped path so
    // `loc.pointer` synthesises `/spec/template/spec/containers/0`.
    let doc_yaml = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  template:
    spec:
      containers:
        - name: web
          image: web:v1.2.3
          imagePullPolicy: Always
"#;
    let mut doc_tmp = NamedTempFile::with_suffix(".yaml").expect("doc tmp");
    doc_tmp.write_all(doc_yaml.as_bytes()).expect("write doc");
    let doc_tmp = doc_tmp.into_temp_path();

    let cli = dq::Cli::try_parse_from([
        "dq",
        "-F",
        "json",
        "--no-color",
        "lint",
        "--rules",
        rule_tmp.to_str().expect("UTF-8 rule path"),
        doc_tmp.to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    // The rule severity is `warn`, so the handler returns `Ok(())` without
    // `--strict` — the snapshot pins the JSON envelope, not the exit code.
    assert!(
        result.is_ok(),
        "dq lint must succeed; got err={result:?}, stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err),
    );

    let stdout = String::from_utf8(out).expect("stdout is utf-8");
    // Pretty-print the JSON for a readable snapshot diff. Use serde_json's
    // `to_string_pretty` so insta diffs surface field-level changes (added
    // / removed keys, value shifts) instead of one-line blob diffs.
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("dq lint -F json must produce valid JSON");
    let pretty = serde_json::to_string_pretty(&parsed).expect("pretty-print");

    // Filter the absolute tempdir path so the snapshot stays stable across
    // machines and runs. The path appears once in the `path` field of the
    // diagnostic — we redact it to `[TEMP_DOC]` via a literal string
    // replace rather than an insta regex filter (the path can contain
    // regex metacharacters on some platforms; literal replace is safer
    // and matches the `normalize_path` pattern used by `cli_snapshots.rs`).
    let doc_str = doc_tmp.to_str().expect("UTF-8 doc path");
    let normalized = pretty.replace(doc_str, "[TEMP_DOC]");
    insta::assert_snapshot!(
        "cli_lint_loc_pointer__deployment_with_pinned_tag",
        normalized,
    );
}
