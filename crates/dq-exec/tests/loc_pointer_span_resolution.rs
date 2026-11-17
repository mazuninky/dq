//! Integration: `loc.pointer` end-to-end against a real span-aware YAML
//! parse.
//!
//! Phase 2 of `add-ir-foundation` rewires `Evaluator::evaluate_file` to
//! consume a borrowed [`dq_core::Ir<'_>`] so rules can resolve diagnostic
//! `(line, col)` through `Ir::line_col_for(pointer)` instead of the legacy
//! `loc.line` jq override.
//!
//! This test pins the contract end-to-end:
//!   1. Parse a YAML document through the production span-aware parser
//!      (so the IR carries genuine `Provenance::Original` entries with
//!      `ValueSpan`s and `original_bytes`).
//!   2. Run a one-rule evaluator whose `loc.pointer` jq emits the canonical
//!      pointer to the offending leaf.
//!   3. Assert the diagnostic's `(line, col)` matches what
//!      `Document::span_at(pointer)`'s `value_range.start` resolves to.
//!
//! Critically, the test does NOT hard-code line/col values — it agrees with
//! the parser. A regression that, say, started counting from the wrong
//! span field would fail here even if the parser shifted to a different
//! YAML implementation.

use camino::Utf8PathBuf;
use dq_exec::{Evaluator, RuleSet, RuleSource};

#[test]
fn loc_pointer_resolves_to_real_yaml_span() {
    // Span-aware YAML parse via the production format registry — same path
    // `dq lint` takes for a `*.yaml` file. The leaf at
    // `/spec/template/spec/containers/0/name` is the resolution target.
    let yaml = r#"apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      containers:
        - name: web
          image: web:latest
"#;
    // The default `Yaml::parse` (via `dq_core::by_name("yaml")`) returns a
    // span-less `Document::value_only` today; the production span-aware
    // path is `parse_yaml_with_spans`. The `Document` it returns is the
    // same shape `Document::with_spans` produces — `original_bytes` plus a
    // populated `SpanMap` — which is exactly what `Document::as_ir`
    // forwards into the IR's provenance map. A future refactor that
    // wires `Yaml::parse` to the span-aware path would let this test
    // switch back to `by_name("yaml").parse(...)` without other changes.
    let doc = dq_core::parse_yaml_with_spans(yaml.as_bytes()).expect("yaml parses");

    // Source of truth for the expected `(line, col)`: the parser's own
    // `Document::span_at` lookup combined with byte → `(line, col)` mapping
    // over `Document::original_bytes`. The test agrees with the parser
    // rather than hard-coding line numbers; a future YAML-parser change
    // that legitimately shifted spans would still pin the contract that
    // `loc.pointer` resolution and `Document::span_at` agree.
    let pointer =
        dq_core::Pointer::parse("/spec/template/spec/containers/0/name").expect("pointer parses");
    let span = doc
        .span_at(&pointer)
        .expect("yaml parser emits span for /spec/template/spec/containers/0/name");
    let bytes = doc.original_bytes();
    let (expected_line, expected_col) = byte_to_line_col(bytes, span.value_range.start);

    // Sanity check the synthesised (line, col) is non-trivial — i.e. the
    // contract is meaningful only if the resolution moves off the (1, 1)
    // default. If the parser ever returned (1, 1) for this leaf, the
    // integration test would silently pass while testing nothing useful.
    assert_ne!(
        (expected_line, expected_col),
        (1, 1),
        "expected the YAML parser to produce a span beyond (1, 1) for \
         /spec/template/spec/containers/0/name; without that the test \
         is vacuous"
    );

    // One rule whose `check.jq` walks `.spec.template.spec.containers` and
    // emits `{name, pointer}` per element; `loc.pointer: '.pointer'` then
    // routes the violation's emitted pointer string through
    // `Ir::line_col_for`.
    let rule_yaml = r#"
id: test.loc-pointer-real-span
description: pin loc.pointer end-to-end against real YAML spans
severity: error
match:
  format: yaml
check:
  jq: '.spec.template.spec.containers | to_entries[] |
       {"name": .value.name, "pointer": "/spec/template/spec/containers/" + (.key|tostring) + "/name"}'
  message: "container '{{ .name }}'"
loc:
  pointer: '.pointer'
"#;
    let ruleset = RuleSet::from_str(rule_yaml, RuleSource::Inline).expect("rule parses");
    let evaluator = Evaluator::new(vec![ruleset]).expect("evaluator builds");
    let path = Utf8PathBuf::from("doc.yaml");
    let ir = doc.as_ir();
    let diags = evaluator.evaluate_file(&path, &ir, "yaml");

    assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
    assert_eq!(
        diags[0].rule_id, "test.loc-pointer-real-span",
        "diagnostic must come from the inline test rule"
    );
    assert_eq!(
        diags[0].line, expected_line,
        "loc.pointer (line) must agree with Document::span_at; \
         got line={} but Document::span_at maps to line={}",
        diags[0].line, expected_line,
    );
    assert_eq!(
        diags[0].col, expected_col,
        "loc.pointer (col) must agree with Document::span_at; \
         got col={} but Document::span_at maps to col={}",
        diags[0].col, expected_col,
    );
}

/// Map a byte offset to a 1-indexed `(line, col)` against `bytes`.
///
/// Mirrors the helper in `crates/dq-core/src/ir/mod.rs::derive_line_col`
/// byte-for-byte — the test re-implements it here rather than importing a
/// crate-private function so a future regression that diverged the two
/// surfaces here.
fn byte_to_line_col(bytes: &[u8], idx: usize) -> (u32, u32) {
    let cap = idx.min(bytes.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for &b in &bytes[..cap] {
        if b == b'\n' {
            line = line.saturating_add(1);
            col = 1;
        } else {
            col = col.saturating_add(1);
        }
    }
    (line, col)
}
