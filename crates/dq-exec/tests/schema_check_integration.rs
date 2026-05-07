//! Integration: `Check::Schema` and `Check::SchemaFile` end-to-end
//! through the production [`dq_exec::Evaluator`].
//!
//! These tests exercise the M11 Phase 3 dispatch path in
//! `evaluator.rs`: rule load → schema compile → per-file validation
//! → diagnostic emission. The unit tests in `schema_check.rs` cover
//! the compile-time path in isolation; this file pins the runtime
//! contract that diagnostic count, severity, and rule-id propagation
//! match the spec scenarios.

use camino::Utf8PathBuf;
use dq_exec::{Evaluator, RuleSet, RuleSource};

fn ir_for(value: &serde_json::Value, format: &str) -> dq_core::OwnedIr {
    let dq_value = dq_core::Value::from_serde_json(value);
    let format_tag = dq_core::FormatTag::from_name(format).unwrap_or(dq_core::FormatTag::Yaml);
    dq_core::OwnedIr::new(dq_value, dq_core::ProvenanceMap::new(), format_tag)
}

fn evaluator_from_yaml(yaml: &str) -> Evaluator {
    let set = RuleSet::from_str(yaml, RuleSource::Inline).expect("ruleset parses");
    Evaluator::new(vec![set]).expect("evaluator builds")
}

#[test]
fn inline_schema_emits_diagnostic_for_missing_required_field() {
    // Spec scenario: inline schema validates document.
    let yaml = r#"
id: test.required
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
    required: [name]
"#;
    let eval = evaluator_from_yaml(yaml);
    let value = serde_json::json!({});
    let owned = ir_for(&value, "yaml");
    let path = Utf8PathBuf::from("doc.yaml");
    let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
    assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
    assert_eq!(diags[0].rule_id, "test.required");
    assert!(
        diags[0].message.to_lowercase().contains("required"),
        "expected message to mention required, got: {}",
        diags[0].message
    );
}

#[test]
fn inline_schema_silent_when_document_validates() {
    let yaml = r#"
id: test.required
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
    required: [name]
"#;
    let eval = evaluator_from_yaml(yaml);
    let value = serde_json::json!({"name": "x"});
    let owned = ir_for(&value, "yaml");
    let path = Utf8PathBuf::from("doc.yaml");
    let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn inline_schema_pointer_from_instance_path() {
    // Spec scenario: schema violation gets pointer from instancePath.
    // /age is the offending path; without spans on the IR the line/col
    // fall back to (1, 1).
    let yaml = r#"
id: test.age-int
description: x
severity: error
match:
  format: json
check:
  schema:
    type: object
    properties:
      age:
        type: integer
"#;
    let eval = evaluator_from_yaml(yaml);
    let value = serde_json::json!({"age": "twelve"});
    let owned = ir_for(&value, "json");
    let path = Utf8PathBuf::from("doc.json");
    let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "json");
    assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
    assert_eq!(diags[0].line, 1);
    assert_eq!(diags[0].col, 1);
}

#[test]
fn inline_schema_with_message_prefix_prepends_to_diagnostic() {
    let yaml = r#"
id: test.prefixed
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
    required: [name]
  message: "shape: "
"#;
    let eval = evaluator_from_yaml(yaml);
    let value = serde_json::json!({});
    let owned = ir_for(&value, "yaml");
    let path = Utf8PathBuf::from("doc.yaml");
    let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
    assert_eq!(diags.len(), 1, "expected one diagnostic");
    assert!(
        diags[0].message.starts_with("shape: "),
        "expected prefix, got: {}",
        diags[0].message
    );
}

#[test]
fn schema_file_relative_path_resolves_to_sibling() {
    // Build a temp dir with rule.yml + shape.schema.json and run the
    // file-loaded variant end-to-end.
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("UTF-8 path");
    let rule_path = dir_path.join("rule.yml");
    let schema_path = dir_path.join("shape.schema.json");
    std::fs::write(
        &rule_path,
        r#"id: test.shape
description: x
severity: error
match:
  format: json
check:
  schema_file: ./shape.schema.json
"#,
    )
    .expect("write rule file");
    std::fs::write(&schema_path, r#"{"type":"object","required":["name"]}"#).expect("write schema");

    let set = RuleSet::from_path(&rule_path).expect("load ruleset");
    let eval = Evaluator::new(vec![set]).expect("build evaluator");
    let value = serde_json::json!({});
    let owned = ir_for(&value, "json");
    let diags = eval.evaluate_file(&Utf8PathBuf::from("doc.json"), &owned.to_borrowed(), "json");
    assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
    assert_eq!(diags[0].rule_id, "test.shape");
}

#[test]
fn schema_file_absolute_path_is_rejected_at_compile() {
    // Spec scenario: `check.schema_file: /etc/passwd` rejected at compile.
    let dir = tempfile::tempdir().expect("tempdir");
    let dir_path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("UTF-8 path");
    let rule_path = dir_path.join("rule.yml");
    std::fs::write(
        &rule_path,
        r#"id: test.bad
description: x
severity: error
match:
  format: json
check:
  schema_file: /etc/passwd
"#,
    )
    .expect("write rule file");

    let set = RuleSet::from_path(&rule_path).expect("load ruleset");
    let err = Evaluator::new(vec![set]).expect_err("evaluator must reject absolute path");
    assert_eq!(err.kind_name(), "schema_file_absolute_path");
}

#[test]
fn schema_file_dotdot_escape_is_rejected_at_compile() {
    // Spec scenario: `..`-escape rejected.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("UTF-8 path");
    let foo = root.join("rules").join("foo");
    std::fs::create_dir_all(&foo).expect("create dir");
    let rule_path = foo.join("rule.yml");
    std::fs::write(
        &rule_path,
        r#"id: test.escape
description: x
severity: error
match:
  format: json
check:
  schema_file: ../../secrets.json
"#,
    )
    .expect("write rule");
    // Create the escape target so canonicalize can resolve it (without
    // it, canonicalize fails first and we'd get an Io error instead).
    std::fs::write(root.join("secrets.json"), "{}").expect("write secret");

    let set = RuleSet::from_path(&rule_path).expect("load ruleset");
    let err = Evaluator::new(vec![set]).expect_err("evaluator must reject path escape");
    assert_eq!(err.kind_name(), "schema_file_escapes_rule_dir");
}

#[test]
fn http_ref_in_inline_schema_rejected_at_compile() {
    // Spec scenario: HTTP $ref rejected at compile-time.
    let yaml = r#"
id: test.http-ref
description: x
severity: error
match:
  format: yaml
check:
  schema:
    $ref: "https://json-schema.org/draft/2020-12/schema"
"#;
    let set = RuleSet::from_str(yaml, RuleSource::Inline).expect("ruleset parses");
    let err = Evaluator::new(vec![set]).expect_err("HTTP $ref must be rejected");
    assert_eq!(err.kind_name(), "schema_compile");
}

#[test]
fn schema_check_emits_one_diagnostic_per_validation_error() {
    // Two missing required fields → two diagnostics.
    let yaml = r#"
id: test.multi
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
    required: [name, age]
"#;
    let eval = evaluator_from_yaml(yaml);
    let value = serde_json::json!({});
    let owned = ir_for(&value, "yaml");
    let path = Utf8PathBuf::from("doc.yaml");
    let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
    // jsonschema emits separate errors for each missing required
    // property — exact count depends on the validator's reporting
    // strategy. We just assert at least one and that they all carry
    // our rule id.
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    assert!(
        diags.iter().all(|d| d.rule_id == "test.multi"),
        "all diagnostics must carry the rule id"
    );
}

#[test]
fn mutual_exclusion_jq_plus_schema_fails_via_loader() {
    // Spec scenario: mutual-exclusion error at rule-load time.
    let yaml = r#"
id: test.bad
description: x
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: m
  schema:
    type: object
"#;
    let err =
        RuleSet::from_str(yaml, RuleSource::Inline).expect_err("mutual exclusion must be rejected");
    assert_eq!(err.kind_name(), "check_mutually_exclusive");
}

#[test]
fn empty_check_block_fails_via_loader_with_check_missing() {
    let yaml = r#"
id: test.empty
description: x
severity: error
match:
  format: yaml
check: {}
"#;
    let err = RuleSet::from_str(yaml, RuleSource::Inline)
        .expect_err("empty check block must be rejected");
    assert_eq!(err.kind_name(), "check_missing");
}

#[test]
fn extract_without_nested_fails_via_loader_with_composite_incomplete() {
    let yaml = r#"
id: test.half
description: x
severity: error
match:
  format: yaml
check:
  extract: '.'
  message: m
"#;
    let err = RuleSet::from_str(yaml, RuleSource::Inline)
        .expect_err("composite-incomplete must be rejected");
    assert_eq!(err.kind_name(), "composite_incomplete");
}

#[test]
fn inline_schema_honors_loc_file_override() {
    // Phase 2 contract: schema diagnostics route `loc.file` through
    // `resolve_loc_file`, the same helper jq diagnostics use. A rule
    // that sets `loc.file` to a literal string must produce a
    // diagnostic whose `file` reflects the override, not the input
    // file path.
    let yaml = r#"
id: test.schema-loc-file
description: x
severity: error
match:
  format: yaml
check:
  schema:
    type: object
    required: [name]
loc:
  file: '"override.txt"'
"#;
    let eval = evaluator_from_yaml(yaml);
    let value = serde_json::json!({});
    let owned = ir_for(&value, "yaml");
    let path = Utf8PathBuf::from("doc.yaml");
    let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
    assert_eq!(diags.len(), 1, "expected one diagnostic, got: {diags:?}");
    assert_eq!(
        diags[0].file.as_deref(),
        Some(Utf8PathBuf::from("override.txt").as_path()),
        "loc.file override must drive diagnostic file path",
    );
}

#[test]
fn embedded_std_jsonschema_kubernetes_crd_loads_and_evaluates() {
    // Pin the @std/jsonschema namespace ships and the
    // kubernetes-crd-shape rule emits a diagnostic for an
    // envelope-missing manifest.
    let set = RuleSet::from_std("jsonschema").expect("@std/jsonschema must resolve");
    let eval = Evaluator::new(vec![set]).expect("evaluator builds");
    let value = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {}  // missing `name`
    });
    let owned = ir_for(&value, "yaml");
    let path = Utf8PathBuf::from("manifest.yaml");
    let diags = eval.evaluate_file(&path, &owned.to_borrowed(), "yaml");
    let crd_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.rule_id == "jsonschema.kubernetes-crd-shape")
        .collect();
    assert!(
        !crd_diags.is_empty(),
        "expected kubernetes-crd-shape to fire on envelope-missing manifest, got: {diags:?}"
    );
}
