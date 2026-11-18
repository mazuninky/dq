//! `Document::set_at` where a non-empty collection is, and `del_at` of
//! TOML tables written as `[table]` / `[[array]]` sections or under dotted
//! keys — values the span map records no span for.

use dq_core::{Document, Error, Pointer, Value, by_name, parse_yaml_with_spans};
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

fn json(value: serde_json::Value) -> Value {
    Value::from_serde_json(&value)
}

fn set(format: &str, src: &str, pointer: &str, value: Value) -> String {
    let mut doc = doc(format, src);
    doc.set_at(&ptr(pointer), value)
        .unwrap_or_else(|err| panic!("set {pointer} on {src:?}: {err}"));
    String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
}

fn del(format: &str, src: &str, pointer: &str) -> String {
    let mut doc = doc(format, src);
    doc.del_at(&ptr(pointer))
        .unwrap_or_else(|err| panic!("del {pointer} on {src:?}: {err}"));
    String::from_utf8(doc.original_bytes().to_vec()).expect("UTF-8")
}

fn del_refused(format: &str, src: &str, pointer: &str) -> String {
    let mut doc = doc(format, src);
    match doc.del_at(&ptr(pointer)) {
        Err(Error::EditRejected { reason, .. }) => {
            assert_eq!(doc.original_bytes(), src.as_bytes(), "bytes untouched");
            reason
        }
        other => panic!("del {pointer} on {src:?}: expected EditRejected, got {other:?}"),
    }
}

// --- set over a collection ------------------------------------------------

#[test]
fn set_over_a_json_collection_replaces_it_between_its_brackets() {
    assert_eq!(
        set("json", r#"{"k": 0, "kp": {"k": 0}}"#, "/kp", Value::Int(7)),
        r#"{"k": 0, "kp": 7}"#
    );
    assert_eq!(
        set(
            "json",
            "{\n  \"kp\": {\n    \"k\": 0\n  },\n  \"z\": 1\n}\n",
            "/kp",
            Value::Int(7)
        ),
        "{\n  \"kp\": 7,\n  \"z\": 1\n}\n"
    );
    assert_eq!(
        set(
            "json",
            r#"{"a": [[1], 2]}"#,
            "/a/0",
            json(serde_json::json!({"x": 1}))
        ),
        r#"{"a": [{"x":1}, 2]}"#
    );
}

#[test]
fn set_over_a_yaml_block_collection_writes_the_value_after_its_key() {
    assert_eq!(
        set("yaml", "a:\n  x: 1\nb: 2\n", "/a", Value::Int(5)),
        "a: 5\nb: 2\n"
    );
    assert_eq!(
        set(
            "yaml",
            "a:\n  x: 1\nb: 2\n",
            "/a",
            json(serde_json::json!([1]))
        ),
        "a: [1]\nb: 2\n"
    );
    // The key's line keeps its comment and anchor.
    assert_eq!(
        set(
            "yaml",
            "a: # c\n  x: 1\n  y:\n    - 2\nb: 2\n",
            "/a",
            Value::Int(5)
        ),
        "a: 5 # c\nb: 2\n"
    );
    assert_eq!(
        set("yaml", "a: &x\n  x: 1\nb: *x\n", "/a", Value::Int(5)),
        "a: &x 5\nb: *x\n"
    );
    assert_eq!(set("yaml", "a:\n  x: 1", "/a", Value::Int(5)), "a: 5\n");
    assert_eq!(
        set("yaml", "a:\r\n  x: 1\r\nb: 2\r\n", "/a", Value::Int(5)),
        "a: 5\r\nb: 2\r\n"
    );
}

#[test]
fn set_over_a_yaml_sequence_item_collection_keeps_the_dash() {
    assert_eq!(
        set(
            "yaml",
            "l:\n  - x: 1\n    y: 2\n  - 3\n",
            "/l/0",
            Value::Int(5)
        ),
        "l:\n  - 5\n  - 3\n"
    );
    assert_eq!(
        set("yaml", "l:\n  - - 1\n    - 2\n", "/l/0", Value::Int(5)),
        "l:\n  - 5\n"
    );
}

#[test]
fn set_over_a_yaml_flow_collection_replaces_its_brackets() {
    assert_eq!(
        set("yaml", "a: [1, 2] # c\nb: 2\n", "/a", Value::Int(5)),
        "a: 5 # c\nb: 2\n"
    );
    assert_eq!(
        set(
            "yaml",
            "a: {x: \"]\", y: 'a'']'} # c\n",
            "/a",
            Value::Int(5)
        ),
        "a: 5 # c\n"
    );
    assert_eq!(
        set(
            "yaml",
            "a: [\n  1, # one ]\n  2\n] # c\nb: 2\n",
            "/a",
            Value::Int(5)
        ),
        "a: 5 # c\nb: 2\n"
    );
    assert_eq!(
        set(
            "yaml",
            "a: {x: [1, 2]}\n",
            "/a/x",
            json(serde_json::json!({"q": 1}))
        ),
        "a: {x: {\"q\": 1}}\n"
    );
}

#[test]
fn set_over_a_toml_inline_collection_replaces_it() {
    assert_eq!(
        set("toml", "a = {x = 1}\nb = 2\n", "/a", Value::Int(5)),
        "a = 5\nb = 2\n"
    );
    assert_eq!(
        set(
            "toml",
            "a = [1, 2]\n",
            "/a",
            json(serde_json::json!({"x": 1}))
        ),
        "a = { x = 1 }\n"
    );
}

#[test]
fn set_over_a_toml_table_writes_the_key_anew() {
    assert_eq!(
        set("toml", "q = 1\n[a]\nb = 1\n", "/a", Value::Int(5)),
        "q = 1\na = 5\n"
    );
    // Its subtables go with it, wherever they are.
    assert_eq!(
        set(
            "toml",
            "[a]\nb = 1\n[a.c]\nd = 1\n[e]\nf = 1\n",
            "/a",
            Value::Int(5)
        ),
        "a = 5\n[e]\nf = 1\n"
    );
    assert_eq!(
        set("toml", "a.b = 1\na.c = 2\nz = 1\n", "/a", Value::Int(5)),
        "z = 1\na = 5\n"
    );
    assert_eq!(
        set(
            "toml",
            "[[arr]]\nx = 1\n[[arr]]\nx = 2\n",
            "/arr",
            Value::Int(5)
        ),
        "arr = 5\n"
    );
}

#[test]
fn set_over_a_toml_table_whose_parent_it_alone_makes_writes_the_parent_inline() {
    assert_eq!(
        set("toml", "x = 1\n[a.b]\nc = 1\n", "/a/b", Value::Int(5)),
        "x = 1\na = { b = 5 }\n"
    );
    assert_eq!(
        set(
            "toml",
            "a.b.c = 1\ne = 1\n",
            "/a/b",
            json(serde_json::json!({"z": 1}))
        ),
        "e = 1\na = { b = { z = 1 } }\n"
    );
    assert_eq!(
        set("toml", "[a.b.c]\nk = 1\n", "/a/b/c", Value::Int(5)),
        "a = { b = { c = 5 } }\n"
    );
    assert_eq!(
        set("toml", "[q]\n[q.a.b]\nc = 1\n", "/q/a/b", Value::Int(5)),
        "[q]\na = { b = 5 }\n"
    );
    assert_eq!(
        set("toml", "[a.b]\nc = 1\n[z]\nw = 1\n", "/a/b", Value::Int(5)),
        "a = { b = 5 }\n[z]\nw = 1\n"
    );
}

#[test]
fn set_over_a_toml_table_under_a_parent_with_no_header_is_refused() {
    // `a` stays, made by `[a.d]`, with no header for `b` to go under.
    let src = "[a.b]\nc = 1\n[a.d]\ne = 1\n";
    let mut doc = doc("toml", src);
    match doc.set_at(&ptr("/a/b"), Value::Int(5)) {
        Err(Error::EditRejected { reason, .. }) => {
            assert!(reason.contains("no header of its own"), "{reason}");
            assert!(reason.contains("`dq set --jq`"), "{reason}");
        }
        other => panic!("expected EditRejected, got {other:?}"),
    }
    assert_eq!(doc.original_bytes(), src.as_bytes(), "bytes untouched");
}

#[test]
fn set_of_a_new_key_in_a_toml_table_of_tables_goes_under_its_header() {
    assert_eq!(
        set("toml", "[t]\n[t.s]\nk = 1\n", "/t/z", Value::Int(3)),
        "[t]\nz = 3\n[t.s]\nk = 1\n"
    );
    assert_eq!(set("toml", "[t]", "/t/z", Value::Int(3)), "[t]\nz = 3\n");
    assert_eq!(
        set("toml", "[t] # c\n\n[u]\n", "/t/z", Value::Int(3)),
        "[t] # c\nz = 3\n\n[u]\n"
    );
    assert_eq!(
        set("toml", "[[a]]\n[[a]]\nx = 1\n", "/a/0/z", Value::Int(3)),
        "[[a]]\nz = 3\n[[a]]\nx = 1\n"
    );
    assert_eq!(
        set("toml", "[t]\r\n[t.s]\r\n", "/t/z", Value::Int(3)),
        "[t]\r\nz = 3\r\n[t.s]\r\n"
    );
}

// --- del of a TOML table ---------------------------------------------------

#[test]
fn del_of_a_toml_table_removes_its_sections() {
    assert_eq!(del("toml", "[a]\nb = 1\n", "/a"), "");
    // The comments above the next header stay with it.
    assert_eq!(
        del("toml", "z = 1\n[a]\nb = 1\n# about c\n[c]\nd = 1\n", "/a"),
        "z = 1\n# about c\n[c]\nd = 1\n"
    );
    assert_eq!(
        del("toml", "[a]\nb = 1\n[a.s]\nk = 1\n[c]\n[a.t]\n", "/a"),
        "[c]\n"
    );
}

#[test]
fn del_of_a_dotted_toml_table_removes_its_lines() {
    assert_eq!(del("toml", "a.b = 1\na.c.d = 2\nz = 1\n", "/a"), "z = 1\n");
    assert_eq!(del("toml", "[t]\na.b = 1\nz = 1\n", "/t/a"), "[t]\nz = 1\n");
}

#[test]
fn del_of_a_toml_array_of_tables_item_removes_its_section() {
    let src = "[[arr]]\nx = 1\n[arr.s]\ny = 1\n[[arr]]\nx = 2\n";
    assert_eq!(del("toml", src, "/arr/0"), "[[arr]]\nx = 2\n");
    assert_eq!(
        del("toml", "[[arr]]\nx = 1\n[[arr]]\nx = 2\n", "/arr/1"),
        "[[arr]]\nx = 1\n"
    );
}

#[test]
fn del_of_the_last_toml_table_under_another_keeps_it_empty() {
    assert_eq!(del("toml", "[[arr]]\nx = 1\n", "/arr/0"), "arr = []\n");
    assert_eq!(
        del("toml", "z = 1\n[[arr]]\nx = 1\n[u]\n", "/arr/0"),
        "z = 1\narr = []\n[u]\n"
    );
    assert_eq!(del("toml", "[a.b]\nk = 1\n", "/a/b"), "a = {}\n");
}

#[test]
fn del_of_a_dotted_key_inside_an_inline_table() {
    assert_eq!(
        del("toml", "a = {b = 2, x.\"y z\".w = 1}\n", "/a/x"),
        "a = {b = 2}\n"
    );
    assert_eq!(
        del("toml", "a = {x.\"y z\".w = 1, b = 2}\n", "/a/x"),
        "a = {b = 2}\n"
    );
    assert_eq!(del("toml", "a = {x.y = 1}\n", "/a/x"), "a = {}\n");
    // The last key under the dotted key leaves it empty, separators kept.
    assert_eq!(
        del("toml", "a = {x.y = 1, b = 2}\n", "/a/x/y"),
        "a = {x = {}, b = 2}\n"
    );
    assert_eq!(
        del("toml", "a = {b = 2, x.y = 1}\n", "/a/x/y"),
        "a = {b = 2, x = {}}\n"
    );
}

#[test]
fn del_of_a_toml_collection_dq_cannot_locate_is_refused() {
    let reason = del_refused("toml", "a = {x.p = 1, x.q = 2}\n", "/a/x");
    assert!(reason.contains("`dq set --jq`"), "{reason}");
    let reason = del_refused("toml", "[a.b.c]\nk = 1\n", "/a/b/c");
    assert!(reason.contains("`dq set --jq`"), "{reason}");
}

#[test]
fn set_over_a_yaml_collection_that_holds_an_anchor_in_use_is_refused() {
    for (src, pointer, line) in [
        ("a:\n  x: &q 1\n  y: 2\nb: *q\n", "/a", 4),
        ("a: {x: &q 1}\nb: *q\n", "/a", 2),
        ("l:\n  - x: &q 1\n    y: 2\nb: *q\n", "/l/0", 4),
    ] {
        let mut doc = doc("yaml", src);
        match doc.set_at(&ptr(pointer), Value::Int(5)) {
            Err(Error::EditRejected { reason, .. }) => assert!(
                reason.starts_with(&format!(
                    "it holds an anchor that the alias on line {line} refers to; "
                )),
                "{src:?}: {reason}"
            ),
            other => panic!("{src:?}: expected EditRejected, got {other:?}"),
        }
        assert_eq!(doc.original_bytes(), src.as_bytes(), "bytes untouched");
    }
    // An alias inside the collection goes with its anchor.
    assert_eq!(
        set(
            "yaml",
            "a:\n  x: &q 1\n  y: *q\nb: 2\n",
            "/a",
            Value::Int(5)
        ),
        "a: 5\nb: 2\n"
    );
}
