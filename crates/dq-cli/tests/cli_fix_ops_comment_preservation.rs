//! Snapshot: `dq fix` with a `fix.ops` rule preserves comments / surrounding
//! bytes byte-for-byte.
//!
//! Phase 4 of `add-ir-foundation` introduced the `fix.ops` branch: rules
//! emit a JSON Patch array and the patch is applied via
//! `EditScript::apply` against the parsed `Document`. Because the patch
//! routes through `Document::set_at` and ultimately through the per-format
//! `ScalarRenderer`, only the byte range of the targeted scalar is
//! replaced — comments and surrounding whitespace are untouched.
//!
//! This file pins the byte-preservation contract end-to-end through the
//! CLI for both JSON and YAML documents. `dq fix` routes the parse
//! through `commands::io_helpers::load_document_for_lint`, which picks
//! up the span-aware parsers (`parse_yaml_with_spans` /
//! `parse_json_with_spans`) so the `fix.ops` `EditScript::apply` path
//! has the byte-level provenance it needs to splice individual scalar
//! ranges without touching surrounding comments or whitespace.
//!
//! ## What this file pins
//!
//! 1. JSON happy path: a `fix.ops` rule rewrites a leaf scalar inside a
//!    JSON document and the surrounding bytes (whitespace, key order,
//!    array layout) are preserved.
//! 2. YAML comment preservation: a `fix.ops` rule rewrites a leaf
//!    scalar inside a YAML document with line / trailing comments and
//!    every comment is preserved byte-for-byte.

use std::io::Write;

use clap::Parser;
use pretty_assertions::assert_eq;
use tempfile::NamedTempFile;

/// Inline `fix.ops` rule body — replace `/data/greeting` with `world`.
const FIX_OPS_RULE: &str = r#"
id: test.fix-ops-comment-preservation
description: replace /data/greeting with 'world' (comment-preserving)
severity: warn
match:
  format: json
check:
  jq: 'select(.data.greeting != "world")'
  message: "data.greeting must be 'world'"
fix:
  ops: |
    if .data.greeting != "world"
    then [{"op":"replace","path":"/data/greeting","value":"world"}]
    else [] end
"#;

/// Same rule body, retargeted at YAML for the YAML-flavoured (ignored)
/// reproducer below. Kept as a separate constant so each test can read
/// like a self-contained scenario without sharing wiring.
const FIX_OPS_RULE_YAML: &str = r#"
id: test.fix-ops-comment-preservation
description: replace /data/greeting with 'world' (comment-preserving)
severity: warn
match:
  format: yaml
check:
  jq: 'select(.data.greeting != "world")'
  message: "data.greeting must be 'world'"
fix:
  ops: |
    if .data.greeting != "world"
    then [{"op":"replace","path":"/data/greeting","value":"world"}]
    else [] end
"#;

#[test]
fn dq_fix_with_fix_ops_preserves_surrounding_bytes_in_json() {
    // JSON doc with deliberate non-canonical whitespace (extra blank
    // line, trailing newline, multi-line array literal). A re-emit
    // through `Format::write` would normalise these; the OPS path must
    // leave them untouched.
    let doc_json = "\
{
  \"apiVersion\": \"v1\",
  \"kind\": \"ConfigMap\",
  \"metadata\": {
    \"name\": \"foo\"
  },
  \"data\": {
    \"greeting\": \"hello\",
    \"numbers\": [
      1,
      2,
      3
    ]
  }
}
";

    let mut rule_tmp = NamedTempFile::with_suffix(".yml").expect("rule tempfile");
    rule_tmp
        .write_all(FIX_OPS_RULE.as_bytes())
        .expect("write rule yaml");
    let rule_path = rule_tmp.into_temp_path();

    let mut doc_tmp = NamedTempFile::with_suffix(".json").expect("doc tempfile");
    doc_tmp
        .write_all(doc_json.as_bytes())
        .expect("write doc json");
    let doc_path = doc_tmp.into_temp_path();

    // Run `dq -i fix --rules <rule> <doc>` through the library entry
    // point. `-i` writes the result back to disk; the call should
    // succeed and the on-disk bytes should reflect the patch with
    // surrounding bytes intact.
    let cli = dq::Cli::try_parse_from([
        "dq",
        "-i",
        "--no-color",
        "fix",
        "--rules",
        rule_path.to_str().expect("UTF-8 rule path"),
        doc_path.to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    assert!(
        result.is_ok(),
        "dq fix must succeed; got err={result:?}, stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err),
    );

    // Read the post-fix bytes off disk. Byte-level expected literal:
    // every space, brace, bracket, blank line preserved; only the
    // single `"hello"` token flipped to `"world"`.
    let post = std::fs::read_to_string(&doc_path).expect("read post-fix json");
    let expected = "\
{
  \"apiVersion\": \"v1\",
  \"kind\": \"ConfigMap\",
  \"metadata\": {
    \"name\": \"foo\"
  },
  \"data\": {
    \"greeting\": \"world\",
    \"numbers\": [
      1,
      2,
      3
    ]
  }
}
";
    assert_eq!(
        post, expected,
        "fix.ops must preserve surrounding bytes byte-for-byte; only the targeted scalar should change",
    );

    // Sanity: stdout under `-i` mode is empty (the bulk driver writes to
    // disk, not stdout).
    assert!(
        out.is_empty(),
        "stdout must be empty under -i; got: {:?}",
        String::from_utf8_lossy(&out),
    );
}

/// YAML-flavoured version of the comment-preservation test. `dq fix`
/// routes the YAML parse through `parse_yaml_with_spans` (via
/// `commands::io_helpers::load_document_for_lint`), so `EditScript::apply`
/// can splice the targeted scalar's byte range while leaving every
/// surrounding comment and line-break untouched.
#[test]
fn dq_fix_with_fix_ops_preserves_comments_byte_for_byte_in_yaml() {
    let doc_yaml = "\
# top-level comment
apiVersion: v1
kind: ConfigMap
metadata:
  name: foo  # trailing comment on metadata.name
data:
  greeting: hello
# trailing comment
";

    let mut rule_tmp = NamedTempFile::with_suffix(".yml").expect("rule tempfile");
    rule_tmp
        .write_all(FIX_OPS_RULE_YAML.as_bytes())
        .expect("write rule yaml");
    let rule_path = rule_tmp.into_temp_path();

    let mut doc_tmp = NamedTempFile::with_suffix(".yaml").expect("doc tempfile");
    doc_tmp
        .write_all(doc_yaml.as_bytes())
        .expect("write doc yaml");
    let doc_path = doc_tmp.into_temp_path();

    let cli = dq::Cli::try_parse_from([
        "dq",
        "-i",
        "--no-color",
        "fix",
        "--rules",
        rule_path.to_str().expect("UTF-8 rule path"),
        doc_path.to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    assert!(
        result.is_ok(),
        "dq fix must succeed on YAML; got err={result:?}, stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err),
    );

    let post = std::fs::read_to_string(&doc_path).expect("read post-fix yaml");
    let expected = "\
# top-level comment
apiVersion: v1
kind: ConfigMap
metadata:
  name: foo  # trailing comment on metadata.name
data:
  greeting: world
# trailing comment
";
    assert_eq!(post, expected, "fix.ops should preserve every comment");
}
