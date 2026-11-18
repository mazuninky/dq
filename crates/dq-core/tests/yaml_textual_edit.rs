//! Regression tests for YAML textual-edit bugs that predate the
//! saphyr-parser 0.3 bump, driven through `Document::set_at` /
//! `Document::del_at` on a document from `parse_yaml_with_spans`.

use dq_core::{Document, Pointer, Value, parse_json_with_spans, parse_yaml_with_spans};
use pretty_assertions::assert_eq;

fn doc(src: &str) -> Document {
    parse_yaml_with_spans(src.as_bytes()).expect("source parses")
}

fn ptr(pointer: &str) -> Pointer {
    Pointer::parse(pointer).expect("pointer parses")
}

// --- empty keys ------------------------------------------------------------

#[test]
fn unaddressable_empty_keys_do_not_panic() {
    // A null key with properties and an empty string key both give a
    // pointer with an empty segment, which no pointer can spell.
    let mut yaml = doc("f: {? &a}\n");
    assert!(yaml.set_at(&ptr("/g"), Value::Int(1)).is_err());
    let mut yaml = doc("\"\": 1\nb: 2\n");
    yaml.set_at(&ptr("/b"), Value::Int(3)).expect("set_at /b");
    assert_eq!(yaml.original_bytes(), b"\"\": 1\nb: 3\n");
    let mut json = parse_json_with_spans(b"{\"\": 1, \"a\": 2}").expect("JSON parses");
    json.set_at(&ptr("/a"), Value::Int(3)).expect("set_at /a");
    assert_eq!(json.original_bytes(), b"{\"\": 1, \"a\": 3}");
}
