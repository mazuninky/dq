//! `Document::set_at` / `Document::del_at` on entries that share a line
//! with others: any JSON object or array, a YAML flow collection, a TOML
//! inline table or array.

use dq_core::{Document, Pointer, Value, by_name, parse_yaml_with_spans};
use pretty_assertions::assert_eq;

fn doc(format: &str, src: &str) -> Document {
    if format == "yaml" {
        return parse_yaml_with_spans(src.as_bytes()).expect("YAML source parses");
    }
    by_name(format)
        .expect("format registered")
        .parse(src.as_bytes())
        .unwrap_or_else(|err| panic!("{format} source parses: {err}"))
}

fn ptr(pointer: &str) -> Pointer {
    Pointer::parse(pointer).expect("pointer parses")
}

fn del(format: &str, src: &str, pointer: &str) -> String {
    let mut doc = doc(format, src);
    doc.del_at(&ptr(pointer))
        .unwrap_or_else(|err| panic!("del {pointer} on {src:?}: {err}"));
    String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
}

fn set(format: &str, src: &str, pointer: &str, value: Value) -> String {
    let mut doc = doc(format, src);
    doc.set_at(&ptr(pointer), value)
        .unwrap_or_else(|err| panic!("set {pointer} on {src:?}: {err}"));
    String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
}

#[test]
fn del_in_single_line_json_removes_only_the_entry() {
    let src = r#"{"a": 1, "b": 2}"#;
    assert_eq!(del("json", src, "/a"), r#"{"b": 2}"#);
    assert_eq!(del("json", src, "/b"), r#"{"a": 1}"#);
    assert_eq!(del("json", r#"{"a": 1}"#, "/a"), "{}");
    assert_eq!(del("json", "[1, 2, 3]", "/1"), "[1, 3]");
    assert_eq!(del("json", "[1, 2, 3]", "/2"), "[1, 2]");
}

#[test]
fn del_of_the_last_json_member_takes_the_comma_before_it() {
    let src = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
    assert_eq!(del("json", src, "/b"), "{\n  \"a\": 1\n}\n");
    assert_eq!(del("json", src, "/a"), "{\n  \"b\": 2\n}\n");
}

#[test]
fn del_in_a_yaml_flow_collection_removes_the_entry_and_a_comma() {
    assert_eq!(del("yaml", "a: {x: 1, y: 2}\n", "/a/x"), "a: {y: 2}\n");
    assert_eq!(del("yaml", "a: {x: 1, y: 2}\n", "/a/y"), "a: {x: 1}\n");
    assert_eq!(del("yaml", "a: [1, 2]\n", "/a/0"), "a: [2]\n");
    assert_eq!(del("yaml", "a: [1, 2]\n", "/a/1"), "a: [1]\n");
    assert_eq!(del("yaml", "a: [1]\nb: 2\n", "/a/0"), "a: []\nb: 2\n");
    // Node properties and an explicit key go along with the entry.
    assert_eq!(
        del("yaml", "a: {\"x y\": &q 1, ? z : 2}\n", "/a/x y"),
        "a: {? z : 2}\n"
    );
    assert_eq!(
        del("yaml", "a: {x: !!str 1, y: 2}\n", "/a/x"),
        "a: {y: 2}\n"
    );
}

#[test]
fn del_of_a_collection_inside_a_flow_collection() {
    assert_eq!(del("yaml", "a: {x: {p: 1}, y: 2}\n", "/a/x"), "a: {y: 2}\n");
    assert_eq!(
        del("yaml", "a: {y: 2, x: &q {p: 1}}\n", "/a/x"),
        "a: {y: 2}\n"
    );
    assert_eq!(del("yaml", "a: [[1], 2]\n", "/a/0"), "a: [2]\n");
    assert_eq!(del("yaml", "[[1], 2]\n", "/0"), "[2]\n");
}

#[test]
fn del_in_a_multi_line_flow_collection() {
    assert_eq!(
        del("yaml", "a: [\n  1,\n  2,\n]\n", "/a/1"),
        "a: [\n  1\n]\n"
    );
    // The blank before `#` stays, so the comment still is one.
    assert_eq!(
        del("yaml", "a: [1, # c\n  2]\n", "/a/0"),
        "a: [ # c\n  2]\n"
    );
    assert_eq!(
        del("toml", "a = [\n  1,\n  2,\n]\n", "/a/1"),
        "a = [\n  1\n]\n"
    );
}

#[test]
fn del_in_a_toml_inline_table_or_array() {
    assert_eq!(del("toml", "a = [1, 2]\n", "/a/0"), "a = [2]\n");
    assert_eq!(del("toml", "a = {x = 1, y = 2}\n", "/a/x"), "a = {y = 2}\n");
    assert_eq!(
        del("toml", "a = {x.p = 1, \"y\" = 2}\n", "/a/y"),
        "a = {x.p = 1}\n"
    );
}

#[test]
fn set_of_a_new_key_in_single_line_json_stays_on_the_line() {
    assert_eq!(
        set("json", r#"{"a": 1, "b": 2}"#, "/c", Value::Int(3)),
        r#"{"a": 1, "b": 2, "c": 3}"#
    );
}

#[test]
fn set_of_a_new_key_in_multi_line_json_takes_the_sibling_column() {
    assert_eq!(
        set(
            "json",
            "{\n  \"a\": 1,\n  \"b\": 2\n}\n",
            "/c",
            Value::Int(3)
        ),
        "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n"
    );
    assert_eq!(
        set(
            "json",
            "{\n    \"o\": {\n        \"a\": 1\n    }\n}\n",
            "/o/c",
            Value::Int(3)
        ),
        "{\n    \"o\": {\n        \"a\": 1,\n        \"c\": 3\n    }\n}\n"
    );
    assert_eq!(
        set("json", "{\r\n  \"a\": 1\r\n}\r\n", "/c", Value::Int(3)),
        "{\r\n  \"a\": 1,\r\n  \"c\": 3\r\n}\r\n"
    );
}

#[test]
fn set_of_a_new_key_in_a_flow_mapping_stays_inside_it() {
    assert_eq!(
        set("yaml", "a: {x: 1}\n", "/a/z", Value::Int(3)),
        "a: {x: 1, z: 3}\n"
    );
    assert_eq!(
        set("toml", "a = {x = 1}\n", "/a/z", Value::Int(3)),
        "a = {x = 1, z = 3}\n"
    );
}

#[test]
fn set_of_a_new_key_after_an_empty_collection_in_a_block_takes_its_own_line() {
    // `[]` / `{}` is a flow node, but the entry holding it is a block one.
    assert_eq!(
        set("yaml", "e:\n  v: []\n", "/e/z", Value::Int(1)),
        "e:\n  v: []\n  z: 1\n"
    );
    assert_eq!(
        set("yaml", "e:\n  v: {} # c\n", "/e/z", Value::Int(1)),
        "e:\n  v: {} # c\n  z: 1\n"
    );
    assert_eq!(
        set("toml", "[t]\nv = []\n", "/t/z", Value::Int(1)),
        "[t]\nv = []\nz = 1\n"
    );
    assert_eq!(
        set("toml", "v = {}\n", "/z", Value::Int(1)),
        "v = {}\nz = 1\n"
    );
    // Inside a flow mapping it stays on the line.
    assert_eq!(
        set("yaml", "a: {x: [], y: {}}\n", "/a/z", Value::Int(3)),
        "a: {x: [], y: {}, z: 3}\n"
    );
}

#[test]
fn del_of_an_empty_toml_inline_value_removes_its_line() {
    assert_eq!(del("toml", "r0 = {}\nr1 = 1\n", "/r0"), "r1 = 1\n");
    assert_eq!(del("toml", "r1 = 1\nr0 = []", "/r0"), "r1 = 1\n");
    assert_eq!(
        del("toml", "r0 = {a = 1, b = {}}\n", "/r0/b"),
        "r0 = {a = 1}\n"
    );
}

#[test]
fn del_of_a_json_collection_removes_the_member_and_a_comma() {
    assert_eq!(
        del("json", r#"{"a": {"x": 1}, "b": 2}"#, "/a"),
        r#"{"b": 2}"#
    );
    assert_eq!(
        del("json", r#"{"b": 2, "a": {"x": 1}}"#, "/a"),
        r#"{"b": 2}"#
    );
    assert_eq!(
        del("json", r#"{"a": [1, [2, 3]], "b": 2}"#, "/a/1"),
        r#"{"a": [1], "b": 2}"#
    );
    assert_eq!(del("json", r#"[{"x": 1}]"#, "/0"), "[]");
    assert_eq!(
        del(
            "json",
            "{\n  \"a\": {\n    \"x\": 1\n  },\n  \"b\": 2\n}\n",
            "/a"
        ),
        "{\n  \"b\": 2\n}\n"
    );
    assert_eq!(
        del(
            "json",
            "{\n  \"b\": 2,\n  \"a\": {\n    \"x\": 1\n  }\n}\n",
            "/a"
        ),
        "{\n  \"b\": 2\n}\n"
    );
}

#[test]
fn set_of_a_new_json_key_goes_after_a_collection_member() {
    assert_eq!(
        set("json", r#"{"a": {"x": 1}}"#, "/c", Value::Int(3)),
        r#"{"a": {"x": 1}, "c": 3}"#
    );
    assert_eq!(
        set("json", r#"{"b": 1, "a": [1]}"#, "/c", Value::Int(3)),
        r#"{"b": 1, "a": [1], "c": 3}"#
    );
    assert_eq!(
        set(
            "json",
            "{\n  \"a\": {\n    \"x\": 1\n  }\n}\n",
            "/c",
            Value::Int(3)
        ),
        "{\n  \"a\": {\n    \"x\": 1\n  },\n  \"c\": 3\n}\n"
    );
}

#[test]
fn set_of_a_new_key_in_a_crlf_file_ends_its_lines_with_crlf() {
    assert_eq!(
        set("yaml", "a: 1\r\nb: 2\r\n", "/c", Value::Int(3)),
        "a: 1\r\nb: 2\r\nc: 3\r\n"
    );
    assert_eq!(
        set(
            "yaml",
            "a:\r\n  x: 1\r\n",
            "/a/y",
            Value::Array(vec![Value::Int(1)])
        ),
        "a:\r\n  x: 1\r\n  y:\r\n    - 1\r\n"
    );
    assert_eq!(
        set("yaml", "a:\r\n  b: |\r\n    x", "/c", Value::Int(1)),
        "a:\r\n  b: |-\r\n    x\r\nc: 1\r\n"
    );
    assert_eq!(
        set("toml", "a = 1\r\n", "/c", Value::Int(3)),
        "a = 1\r\nc = 3\r\n"
    );
    assert_eq!(
        set("toml", "[t]\r\nx = 1\r\n", "/t/y", Value::Int(3)),
        "[t]\r\nx = 1\r\ny = 3\r\n"
    );
    // A file with LF line breaks keeps them.
    assert_eq!(set("toml", "a = 1", "/c", Value::Int(3)), "a = 1\nc = 3\n");
}

#[test]
fn set_of_a_toml_collection_writes_it_inline() {
    let json = |v| Value::from_serde_json(&v);
    assert_eq!(
        set(
            "toml",
            "a = 1 # c\nb = 2\n",
            "/a",
            json(serde_json::json!({"x": 1}))
        ),
        "a = { x = 1 } # c\nb = 2\n"
    );
    assert_eq!(
        set("toml", "a = 1\n", "/a", json(serde_json::json!([1, "x"]))),
        "a = [1, \"x\"]\n"
    );
    assert_eq!(
        set("toml", "a = 1\n", "/b", json(serde_json::json!([{"c": 1}]))),
        "a = 1\nb = [{ c = 1 }]\n"
    );
    assert_eq!(
        set(
            "toml",
            "a = { p = 1 }\n",
            "/a/q",
            json(serde_json::json!({"c": {}}))
        ),
        "a = { p = 1, q = { c = {} } }\n"
    );
}

#[test]
fn del_of_the_only_dotted_toml_key_leaves_an_empty_table() {
    assert_eq!(del("toml", "a.b = 1\n", "/a/b"), "a = {}\n");
    assert_eq!(
        del("toml", "a.b.c = 1\na.d = 2\n", "/a/b/c"),
        "a.b = {}\na.d = 2\n"
    );
    assert_eq!(
        del("toml", "[t]\n  a . \"b.c\" . d = 1\n", "/t/a/b.c/d"),
        "[t]\n  a . \"b.c\" = {}\n"
    );
    // A key under a header leaves the header, which reads as `{}`.
    assert_eq!(del("toml", "[a]\nb = 1\n", "/a/b"), "[a]\n");
}

#[test]
fn set_of_a_new_toml_root_key_goes_before_the_first_table() {
    assert_eq!(
        set("toml", "[a]\nx = 1\n", "/c", Value::Int(3)),
        "c = 3\n[a]\nx = 1\n"
    );
    // The comment right above a header stays with it.
    assert_eq!(
        set(
            "toml",
            "# top\n\n# about t\n[t]\nx = 1\n",
            "/c",
            Value::Int(3)
        ),
        "# top\n\nc = 3\n# about t\n[t]\nx = 1\n"
    );
    assert_eq!(
        set("toml", "a.b = 1\n[[p]]\nx = 1\n", "/c", Value::Int(3)),
        "a.b = 1\nc = 3\n[[p]]\nx = 1\n"
    );
    assert_eq!(set("toml", "# c", "/a", Value::Int(1)), "# c\na = 1\n");
    assert_eq!(set("toml", "", "/a", Value::Int(1)), "a = 1\n");
}
