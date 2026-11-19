//! Sanity tests for edits dq used to refuse: implicit-null entries of YAML
//! flow mappings, a byte order mark, lone-`\r` line breaks, a `%TAG`
//! rebinding of `!!`, flow-mapping deletes next to collection keys, a
//! document of a stream as a whole, and the root of a file.

use dq_core::{Document, Error, Pointer, Value, by_name, parse_yaml_stream_with_spans};
use pretty_assertions::assert_eq;

fn doc(format: &str, src: &str) -> Document {
    match format {
        "yaml" => parse_yaml_stream_with_spans(src.as_bytes()).expect("YAML source parses"),
        "json" => dq_core::parse_json_with_spans(src.as_bytes()).expect("JSON source parses"),
        _ => by_name(format)
            .expect("format registered")
            .parse(src.as_bytes())
            .unwrap_or_else(|err| panic!("{format} source parses: {err}")),
    }
}

fn ptr(pointer: &str) -> Pointer {
    Pointer::parse(pointer).expect("pointer parses")
}

fn json(literal: &str) -> Value {
    let parsed: serde_json::Value = serde_json::from_str(literal).expect("JSON literal");
    Value::from_serde_json(&parsed)
}

fn text(doc: &Document) -> String {
    String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
}

fn set(format: &str, src: &str, pointer: &str, value: &str) -> String {
    let mut doc = doc(format, src);
    doc.set_at(&ptr(pointer), json(value))
        .unwrap_or_else(|err| panic!("set {pointer} on {src:?}: {err}"));
    text(&doc)
}

fn del(format: &str, src: &str, pointer: &str) -> String {
    let mut doc = doc(format, src);
    doc.del_at(&ptr(pointer))
        .unwrap_or_else(|err| panic!("del {pointer} on {src:?}: {err}"));
    text(&doc)
}

#[test]
fn an_implicit_null_flow_entry_takes_a_value_and_goes() {
    assert_eq!(set("yaml", "z: {n}\n", "/z/n", "1"), "z: {n: 1}\n");
    assert_eq!(del("yaml", "z: {n}\n", "/z/n"), "z: {}\n");
    assert_eq!(set("yaml", "z: {n}\n", "/z/m", "1"), "z: {n, m: 1}\n");
    assert_eq!(
        set("yaml", "z: {a, b: 1}\n", "/z/a", "2"),
        "z: {a: 2, b: 1}\n"
    );
    assert_eq!(del("yaml", "z: {a, b: 1}\n", "/z/a"), "z: {b: 1}\n");
    assert_eq!(set("yaml", "z: [{n}]\n", "/z/0/n", "1"), "z: [{n: 1}]\n");
    assert_eq!(set("yaml", "z: {? n}\n", "/z/n", "1"), "z: {? n: 1}\n");
    assert_eq!(set("yaml", "z: {n: }\n", "/z/n", "1"), "z: {n: 1}\n");
    assert_eq!(set("yaml", "z: [a: ]\n", "/z/0/a", "1"), "z: [a: 1]\n");
}

#[test]
fn a_flow_delete_leaves_no_blank_before_the_comma_or_brace() {
    assert_eq!(del("yaml", "m: {a: '', b: 1}\n", "/m/b"), "m: {a: ''}\n");
    assert_eq!(del("yaml", "m: {a: '' , b: 1}\n", "/m/b"), "m: {a: ''}\n");
    assert_eq!(
        del("yaml", "m: {[a]: 1, ['}', b]: 2}\n", "/m/[a]"),
        "m: {['}', b]: 2}\n"
    );
    assert_eq!(
        del("yaml", "m: {[a]: 1, # c\n  b: 2}\n", "/m/b"),
        "m: {[a]: 1, # c\n  }\n"
    );
}

#[test]
fn a_byte_order_mark_before_an_indented_root_keeps_the_columns() {
    let src = "\u{feff}  a: 1\n  b:\n    c: 2\n";
    assert_eq!(
        set("yaml", src, "/d", "3"),
        "\u{feff}  a: 1\n  b:\n    c: 2\n  d: 3\n"
    );
    assert_eq!(del("yaml", src, "/a"), "\u{feff}  b:\n    c: 2\n");
    assert_eq!(
        set("yaml", src, "/b/e", "4"),
        "\u{feff}  a: 1\n  b:\n    c: 2\n    e: 4\n"
    );
    assert_eq!(
        set("yaml", "\u{feff}- 1\n- 2\n", "/1", "3"),
        "\u{feff}- 1\n- 3\n"
    );
    assert_eq!(del("yaml", "\u{feff}- 1\n- 2\n", "/0"), "\u{feff}- 2\n");
}

#[test]
fn lone_carriage_returns_stay_the_line_breaks() {
    assert_eq!(set("yaml", "a: 1\rb: 2\r", "/c", "3"), "a: 1\rb: 2\rc: 3\r");
    assert_eq!(del("yaml", "a: 1\rb: 2\r", "/a"), "b: 2\r");
    let block = "a:\r  x: |\r    one\r  z: 1\r";
    assert_eq!(
        set("yaml", block, "/a/w", "1"),
        "a:\r  x: |\r    one\r  z: 1\r  w: 1\r"
    );
    assert_eq!(del("yaml", block, "/a/x"), "a:\r  z: 1\r");
    assert_eq!(
        set("json", "{\r  \"a\": 1\r}\r", "/b", "2"),
        "{\r  \"a\": 1,\r  \"b\": 2\r}\r"
    );
}

#[test]
fn a_rebound_tag_handle_reads_and_writes_like_a_local_tag() {
    let src = "%TAG !! tag:example.com,2000:\n---\na: !!str 1\nb: !!str \"1\"\n";
    let read = doc("yaml", src);
    assert_eq!(read.value(), &json(r#"{"a": 1, "b": "1"}"#));
    assert_eq!(
        set("yaml", src, "/a", "2"),
        "%TAG !! tag:example.com,2000:\n---\na: !!str 2\nb: !!str \"1\"\n"
    );
    // The core `!!str` still makes a string.
    assert_eq!(doc("yaml", "a: !!str 1\n").value(), &json(r#"{"a": "1"}"#));
}

#[test]
fn a_document_of_a_stream_is_replaced_as_a_whole() {
    let src = "a: 1 # keep\n---\nb: 2\n";
    assert_eq!(
        set("yaml", src, "/1", r#"{"c": 3}"#),
        "a: 1 # keep\n---\nc: 3\n"
    );
    assert_eq!(set("yaml", src, "/0", r#"{"c": 3}"#), "c: 3\n---\nb: 2\n");
    // A scalar document stays a textual edit, comment and end marker kept.
    assert_eq!(
        set("yaml", "a: 1\n---\n5 # c\n...\n", "/1", "7"),
        "a: 1\n---\n7 # c\n...\n"
    );
    assert_eq!(
        set("yaml", "a: 1\r\n---\r\nb: 2\r\n", "/1", "[1]"),
        "a: 1\r\n---\r\n- 1\r\n"
    );
    assert_eq!(
        set("yaml", "a: 1\r---\rb: 2\r", "/1", "[1]"),
        "a: 1\r---\r- 1\r"
    );
}

#[test]
fn a_document_of_a_stream_is_removed_with_its_separator() {
    let src = "# head\na: 1\n# about b\n---\nb: 2\n# about c\n---\nc: 3\n";
    assert_eq!(
        del("yaml", src, "/1"),
        "# head\na: 1\n# about c\n---\nc: 3\n"
    );
    assert_eq!(
        del("yaml", src, "/0"),
        "# head\n# about b\n---\nb: 2\n# about c\n---\nc: 3\n"
    );
    assert_eq!(
        del("yaml", src, "/2"),
        "# head\na: 1\n# about b\n---\nb: 2\n"
    );
    assert_eq!(del("yaml", "a: 1\n...\n---\nb: 2\n", "/1"), "a: 1\n...\n");

    // Down to one document: the file is no stream any more.
    let mut two = doc("yaml", "a: 1\n---\nb: 2\n");
    two.del_at(&ptr("/0")).expect("del /0");
    assert_eq!(text(&two), "---\nb: 2\n");
    assert!(!two.is_multi());
    assert_eq!(two.value(), &json(r#"{"b": 2}"#));
}

#[test]
fn set_of_the_root_replaces_the_document_and_keeps_its_head() {
    assert_eq!(
        set("yaml", "# head\n\na: 1 # c\n# tail\n", "", r#"{"b": 2}"#),
        "# head\n\nb: 2\n# tail\n"
    );
    assert_eq!(
        set("yaml", "--- !!map\na: 1\n", "", r#"{"b": 2}"#),
        "--- !!map\nb: 2\n"
    );
    assert_eq!(set("yaml", "# only\n", "", r#"{"b": 2}"#), "# only\nb: 2\n");
    assert_eq!(
        set("json", "  {\n  \"a\": 1\n}\n", "", r#"{"b": [1]}"#),
        "  {\"b\":[1]}\n"
    );
    assert_eq!(
        set("toml", "# head\n\na = 1\n[t]\nx = 1\n", "", r#"{"b": 2}"#),
        "# head\n\nb = 2\n"
    );
    assert_eq!(set("toml", "a = 1\r\n", "", r#"{"b": 2}"#), "b = 2\r\n");
}

#[test]
fn set_of_the_root_refuses_what_the_file_cannot_hold() {
    let mut toml = doc("toml", "a = 1\n");
    let err = toml.set_at(&ptr(""), Value::Int(5)).expect_err("refused");
    assert!(
        matches!(&err, Error::EditRejected { reason, .. } if reason.contains("a TOML document is a table")),
        "{err}"
    );
    assert_eq!(text(&toml), "a = 1\n");

    let mut stream = doc("yaml", "a: 1\n---\nb: 2\n");
    let err = stream.set_at(&ptr(""), Value::Int(5)).expect_err("refused");
    assert!(matches!(err, Error::Path { .. }), "{err}");
    assert!(err.to_string().contains("set one document as /N"), "{err}");
}

#[test]
fn del_of_the_root_says_why_it_cannot() {
    let mut single = doc("yaml", "a: 1\n");
    let err = single.del_at(&ptr("")).expect_err("refused");
    assert!(matches!(err, Error::Path { .. }), "{err}");
    assert!(
        err.to_string().starts_with("cannot delete the root ''"),
        "{err}"
    );
}
