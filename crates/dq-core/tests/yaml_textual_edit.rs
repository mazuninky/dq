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

fn set(src: &str, pointer: &str, value: Value) -> String {
    let mut doc = doc(src);
    doc.set_at(&ptr(pointer), value).expect("set_at succeeds");
    String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
}

fn del(src: &str, pointer: &str) -> String {
    let mut doc = doc(src);
    doc.del_at(&ptr(pointer)).expect("del_at succeeds");
    String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
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

// --- block scalars and values on the next line -----------------------------

#[test]
fn new_key_after_a_block_scalar_lands_at_the_parent_indent() {
    assert_eq!(
        set("a: |\n  x\n", "/zz", Value::Int(1)),
        "a: |\n  x\nzz: 1\n"
    );
    assert_eq!(
        set("a: |\n  x\n\n\n", "/zz", Value::Int(1)),
        "a: |\n  x\n\n\nzz: 1\n"
    );
    assert_eq!(
        set("o:\n  a: |\n    x\nb: 1\n", "/o/zz", Value::Int(1)),
        "o:\n  a: |\n    x\n  zz: 1\nb: 1\n"
    );
}

#[test]
fn new_key_after_an_unterminated_block_scalar_switches_it_to_strip() {
    // Without a final line break the scalar is `x`; the line break the
    // insertion adds would make it `x\n` under clip / keep chomping.
    let out = set("a: |\n  x", "/zz", Value::Int(1));
    assert_eq!(out, "a: |-\n  x\nzz: 1\n");
    let reread = doc(&out);
    assert_eq!(
        ptr("/a").resolve(reread.value()).expect("/a"),
        &Value::String("x".to_owned())
    );
}

#[test]
fn del_of_a_block_scalar_removes_its_whole_entry() {
    assert_eq!(del("a: |\n  x\nb: 1\n", "/a"), "b: 1\n");
    assert_eq!(del("a:\n  x y\nb: 1\n", "/a"), "b: 1\n");
    assert_eq!(del("l:\n  - |\n    x\n  - 2\n", "/l/0"), "l:\n  - 2\n");
}
