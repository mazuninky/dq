//! [`dq_core::apply_merge`] (RFC 7396) on parsed YAML and JSON files: a
//! nested map patch merged into a target that is no map, a `null` member
//! deleting a nested key, and the no-op cases.
//!
//! The engine asks at each level of the patch whether the target is a map;
//! these pin the answer's effect on the value and on the file bytes (YAML
//! comments survive the textual edit). Cases already covered by the unit
//! tests in `src/transform/merge.rs` — a root that is no map, a top-level
//! scalar target (value only), a top-level `null` delete and no-op, and a
//! nested patch into an empty JSON root — are not repeated.

use dq_core::{Document, Value, apply_merge, parse_json_with_spans, parse_yaml_with_spans};
use pretty_assertions::assert_eq;
use serde_json::json;

const YAML: &str = "\
# config
spec:
  b: 1 # keep b
  c: 2
  list:
    - 1
    - 2
  s: 3
  n: null
tail: 0 # keep tail
";

const JSON: &str = r#"{
  "spec": {
    "b": 1,
    "c": 2,
    "list": [1, 2],
    "s": 3,
    "n": null
  },
  "tail": 0
}
"#;

#[derive(Clone, Copy)]
enum Fmt {
    Yaml,
    Json,
}

fn parse(fmt: Fmt, bytes: &[u8]) -> Document {
    match fmt {
        Fmt::Yaml => parse_yaml_with_spans(bytes).expect("YAML parses"),
        Fmt::Json => parse_json_with_spans(bytes).expect("JSON parses"),
    }
}

/// Merge `patch` into `src`; check the merged value is `want`, the file
/// bytes are `want_bytes`, and the bytes parse back to `want`.
fn check(
    fmt: Fmt,
    src: &str,
    patch: &serde_json::Value,
    want: &serde_json::Value,
    want_bytes: &str,
) {
    let mut doc = parse(fmt, src.as_bytes());
    apply_merge(&mut doc, &Value::from_serde_json(patch)).expect("merge succeeds");
    let want = Value::from_serde_json(want);
    assert_eq!(doc.value(), &want, "merged value");
    let bytes = String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8");
    assert_eq!(bytes, want_bytes, "file bytes");
    assert_eq!(
        parse(fmt, bytes.as_bytes()).value(),
        &want,
        "bytes re-parse to the merged value"
    );
}

/// The source document's value with `spec` edited by `edit`.
fn source_with(
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> serde_json::Value {
    let mut doc = json!({
        "spec": {"b": 1, "c": 2, "list": [1, 2], "s": 3, "n": null},
        "tail": 0
    });
    edit(doc["spec"].as_object_mut().expect("spec is a map"));
    doc
}

// ---- a map patch into a target that is no map: the patch replaces it ----

#[test]
fn yaml_map_patch_into_nested_array_replaces_it() {
    check(
        Fmt::Yaml,
        YAML,
        &json!({"spec": {"list": {"x": 1}}}),
        &source_with(|spec| {
            spec["list"] = json!({"x": 1});
        }),
        "# config\nspec:\n  b: 1 # keep b\n  c: 2\n  list: {x: 1}\n  s: 3\n  n: null\ntail: 0 # keep tail\n",
    );
}

#[test]
fn json_map_patch_into_nested_array_replaces_it() {
    check(
        Fmt::Json,
        JSON,
        &json!({"spec": {"list": {"x": 1}}}),
        &source_with(|spec| {
            spec["list"] = json!({"x": 1});
        }),
        "{\n  \"spec\": {\n    \"b\": 1,\n    \"c\": 2,\n    \"list\": {\"x\":1},\n    \"s\": 3,\n    \"n\": null\n  },\n  \"tail\": 0\n}\n",
    );
}

#[test]
fn yaml_map_patch_into_nested_scalar_replaces_it() {
    check(
        Fmt::Yaml,
        YAML,
        &json!({"spec": {"s": {"x": 1}}}),
        &source_with(|spec| {
            spec["s"] = json!({"x": 1});
        }),
        "# config\nspec:\n  b: 1 # keep b\n  c: 2\n  list:\n    - 1\n    - 2\n  s: {x: 1}\n  n: null\ntail: 0 # keep tail\n",
    );
}

#[test]
fn json_map_patch_into_nested_scalar_replaces_it() {
    check(
        Fmt::Json,
        JSON,
        &json!({"spec": {"s": {"x": 1}}}),
        &source_with(|spec| {
            spec["s"] = json!({"x": 1});
        }),
        "{\n  \"spec\": {\n    \"b\": 1,\n    \"c\": 2,\n    \"list\": [1, 2],\n    \"s\": {\"x\":1},\n    \"n\": null\n  },\n  \"tail\": 0\n}\n",
    );
}

/// A `null` target is no map either: it takes the patch without its
/// `null` members.
#[test]
fn yaml_map_patch_into_nested_null_takes_patch_without_nulls() {
    check(
        Fmt::Yaml,
        YAML,
        &json!({"spec": {"n": {"x": 1, "y": null}}}),
        &source_with(|spec| {
            spec["n"] = json!({"x": 1});
        }),
        "# config\nspec:\n  b: 1 # keep b\n  c: 2\n  list:\n    - 1\n    - 2\n  s: 3\n  n: {x: 1}\ntail: 0 # keep tail\n",
    );
}

#[test]
fn json_map_patch_into_nested_null_takes_patch_without_nulls() {
    check(
        Fmt::Json,
        JSON,
        &json!({"spec": {"n": {"x": 1, "y": null}}}),
        &source_with(|spec| {
            spec["n"] = json!({"x": 1});
        }),
        "{\n  \"spec\": {\n    \"b\": 1,\n    \"c\": 2,\n    \"list\": [1, 2],\n    \"s\": 3,\n    \"n\": {\"x\":1}\n  },\n  \"tail\": 0\n}\n",
    );
}

/// A missing key under an existing map: the whole patch subtree is added,
/// its `null` members dropped at every depth.
#[test]
fn yaml_map_patch_under_missing_intermediate_key_adds_the_subtree() {
    check(
        Fmt::Yaml,
        YAML,
        &json!({"spec": {"new": {"deep": {"x": 1, "gone": null}}}}),
        &source_with(|spec| {
            spec.insert("new".into(), json!({"deep": {"x": 1}}));
        }),
        "# config\nspec:\n  b: 1 # keep b\n  c: 2\n  list:\n    - 1\n    - 2\n  s: 3\n  n: null\n  new:\n    deep: {x: 1}\ntail: 0 # keep tail\n",
    );
}

#[test]
fn json_map_patch_under_missing_intermediate_key_adds_the_subtree() {
    check(
        Fmt::Json,
        JSON,
        &json!({"spec": {"new": {"deep": {"x": 1, "gone": null}}}}),
        &source_with(|spec| {
            spec.insert("new".into(), json!({"deep": {"x": 1}}));
        }),
        "{\n  \"spec\": {\n    \"b\": 1,\n    \"c\": 2,\n    \"list\": [1, 2],\n    \"s\": 3,\n    \"n\": null,\n    \"new\": {\"deep\":{\"x\":1}}\n  },\n  \"tail\": 0\n}\n",
    );
}

// ---- null members ----

#[test]
fn yaml_null_member_deletes_nested_key() {
    check(
        Fmt::Yaml,
        YAML,
        &json!({"spec": {"c": null}}),
        &source_with(|spec| {
            spec.remove("c");
        }),
        "# config\nspec:\n  b: 1 # keep b\n  list:\n    - 1\n    - 2\n  s: 3\n  n: null\ntail: 0 # keep tail\n",
    );
}

#[test]
fn json_null_member_deletes_nested_key() {
    check(
        Fmt::Json,
        JSON,
        &json!({"spec": {"c": null}}),
        &source_with(|spec| {
            spec.remove("c");
        }),
        "{\n  \"spec\": {\n    \"b\": 1,\n    \"list\": [1, 2],\n    \"s\": 3,\n    \"n\": null\n  },\n  \"tail\": 0\n}\n",
    );
}

#[test]
fn yaml_null_member_for_missing_nested_key_is_a_no_op() {
    check(
        Fmt::Yaml,
        YAML,
        &json!({"spec": {"missing": null}}),
        &source_with(|_| {}),
        YAML,
    );
}

#[test]
fn json_null_member_for_missing_nested_key_is_a_no_op() {
    check(
        Fmt::Json,
        JSON,
        &json!({"spec": {"missing": null}}),
        &source_with(|_| {}),
        JSON,
    );
}
