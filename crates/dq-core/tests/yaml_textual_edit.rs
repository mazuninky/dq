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
    // A new key goes after the collection holding it.
    assert_eq!(set("f: {? &a}\n", "/g", Value::Int(1)), "f: {? &a}\ng: 1\n");
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
    // Keep chomping turns to strip; an indentation indicator stays.
    assert_eq!(
        set("a: |+\n  x", "/zz", Value::Int(1)),
        "a: |-\n  x\nzz: 1\n"
    );
    assert_eq!(
        set("a: |2\n   x", "/zz", Value::Int(1)),
        "a: |-2\n   x\nzz: 1\n"
    );
    assert_eq!(
        set("a: >2+\n   x", "/zz", Value::Int(1)),
        "a: >2-\n   x\nzz: 1\n"
    );
    assert_eq!(
        set("a: |-\n  x", "/zz", Value::Int(1)),
        "a: |-\n  x\nzz: 1\n"
    );
}

#[test]
fn set_on_a_block_scalar_rewrites_it_inline() {
    assert_eq!(
        set("k: |\n  x\nr: 1\n", "/k", Value::Int(42)),
        "k: 42\nr: 1\n"
    );
    assert_eq!(
        set(
            "k: >-\n  x\n  y\n\nr: 1\n",
            "/k",
            Value::String("hi".to_owned())
        ),
        "k: hi\n\nr: 1\n"
    );
    // Node properties and a comment on the header line stay.
    assert_eq!(
        set("k: &a | # c\n  x\nr: *a\n", "/k", Value::Int(42)),
        "k: &a 42 # c\nr: *a\n"
    );
    assert_eq!(
        set("l:\n  - |2+\n     x\n  - 2\n", "/l/0", Value::Int(42)),
        "l:\n  - 42\n  - 2\n"
    );
    assert_eq!(set("k: |\n  x", "/k", Value::Int(42)), "k: 42");
    assert_eq!(
        set("k: |\n\nr: 1\n", "/k", Value::Int(42)),
        "k: 42\n\nr: 1\n"
    );
    assert_eq!(
        set("k: |\r\n  x\r\nr: 1\r\n", "/k", Value::Int(42)),
        "k: 42\r\nr: 1\r\n"
    );
}

#[test]
fn del_of_a_block_scalar_removes_its_whole_entry() {
    assert_eq!(del("a: |\n  x\nb: 1\n", "/a"), "b: 1\n");
    assert_eq!(del("a:\n  x y\nb: 1\n", "/a"), "b: 1\n");
    assert_eq!(del("l:\n  - |\n    x\n  - 2\n", "/l/0"), "l:\n  - 2\n");
}

// --- a new key goes after the last child ----------------------------------------

#[test]
fn set_of_a_new_key_goes_after_the_last_child_even_a_collection() {
    assert_eq!(
        set("root:\n  inner: x\n", "/zz", Value::Int(1)),
        "root:\n  inner: x\nzz: 1\n"
    );
    assert_eq!(
        set("a: 1\nb:\n  x: 1\n", "/c", Value::Int(3)),
        "a: 1\nb:\n  x: 1\nc: 3\n"
    );
    assert_eq!(
        set(
            "spec:\n  template:\n    metadata: {a: 1}\n",
            "/spec/replicas",
            Value::Int(3)
        ),
        "spec:\n  template:\n    metadata: {a: 1}\n  replicas: 3\n"
    );
    // A comment after the collection belongs to what follows it.
    assert_eq!(
        set("a: 1\nb:\n  - 1\n# tail\n", "/c", Value::Int(3)),
        "a: 1\nb:\n  - 1\nc: 3\n# tail\n"
    );
    assert_eq!(
        set("l:\n  - a:\n      x: 1\n", "/l/0/z", Value::Int(3)),
        "l:\n  - a:\n      x: 1\n    z: 3\n"
    );
    assert_eq!(
        set("a: {p: {x: 1}}\n", "/a/z", Value::Int(3)),
        "a: {p: {x: 1}, z: 3}\n"
    );
    // The collection's last scalar is an unterminated block scalar.
    assert_eq!(
        set("b:\n  s: |\n    x", "/c", Value::Int(3)),
        "b:\n  s: |-\n    x\nc: 3\n"
    );
}

// --- an empty value -----------------------------------------------------------

#[test]
fn set_on_an_explicit_key_without_a_value_adds_the_indicator() {
    assert_eq!(
        set("? v\na: 1\n", "/v", Value::Int(42)),
        "? v\n: 42\na: 1\n"
    );
    assert_eq!(
        set("? v\r\na: 1\r\n", "/v", Value::Int(42)),
        "? v\r\n: 42\r\na: 1\r\n"
    );
    assert_eq!(set("? &a v\n", "/v", Value::Int(42)), "? &a v\n: 42\n");
    assert_eq!(
        set("l:\n  - ? v\n", "/l/0/v", Value::Int(42)),
        "l:\n  - ? v\n    : 42\n"
    );
}

#[test]
fn set_on_an_empty_value_writes_it_after_the_indicator() {
    assert_eq!(set("a:\nb: 1\n", "/a", Value::Int(5)), "a: 5\nb: 1\n");
    assert_eq!(set("a:", "/a", Value::Int(5)), "a: 5");
    assert_eq!(
        set("a:\r\nb: 1\r\n", "/a", Value::Int(5)),
        "a: 5\r\nb: 1\r\n"
    );
    assert_eq!(
        set("a: # c\nb: 1\n", "/a", Value::Int(5)),
        "a: 5 # c\nb: 1\n"
    );
    assert_eq!(
        set("l:\n  -\n  - 2\n", "/l/0", Value::Int(5)),
        "l:\n  - 5\n  - 2\n"
    );
    assert_eq!(
        set("- a:\n  b: 1\n", "/0/a", Value::Int(5)),
        "- a: 5\n  b: 1\n"
    );
    // An anchor stays; a core-schema tag would retype the value and goes.
    assert_eq!(
        set("a: &x\nb: *x\n", "/a", Value::Int(5)),
        "a: &x 5\nb: *x\n"
    );
    assert_eq!(set("a: !!str\nb: 1\n", "/a", Value::Int(5)), "a: 5\nb: 1\n");
}

// --- aliases -----------------------------------------------------------------

#[test]
fn set_on_an_alias_value_rewrites_only_that_value() {
    let src = "a: &x 1\nb: *x\nc: 2\n";
    assert_eq!(set(src, "/b", Value::Int(9)), "a: &x 1\nb: 9\nc: 2\n");
    assert_eq!(set(src, "/c", Value::Int(7)), "a: &x 1\nb: *x\nc: 7\n");
}

#[test]
fn set_after_an_alias_sequence_item_addresses_the_next_item() {
    let src = "a: &x 1\nl:\n  - *x\n  - 2\nz: 3\n";
    assert_eq!(
        set(src, "/l/1", Value::Int(9)),
        "a: &x 1\nl:\n  - *x\n  - 9\nz: 3\n"
    );
}

#[test]
fn alias_key_and_merge_key_keep_later_pointers_in_place() {
    assert_eq!(
        set("a: &x 1\n*x : 2\nc: 3\n", "/c", Value::Int(9)),
        "a: &x 1\n*x : 2\nc: 9\n"
    );
    assert_eq!(
        set(
            "b: &b\n  x: 1\nd:\n  <<: *b\n  y: 2\n",
            "/d/y",
            Value::Int(9)
        ),
        "b: &b\n  x: 1\nd:\n  <<: *b\n  y: 9\n"
    );
    assert_eq!(
        set("a: &x 1\nm: {k: *x, z: 3}\n", "/m/z", Value::Int(9)),
        "a: &x 1\nm: {k: *x, z: 9}\n"
    );
}

// --- a tagged scalar -------------------------------------------------------------

#[test]
fn set_of_a_scalar_drops_its_core_tag_and_keeps_the_rest() {
    let hi = || Value::String("hi".into());
    assert_eq!(
        set("b: !!binary |\n  aGVsbG8=\nc: 1\n", "/b", hi()),
        "b: hi\nc: 1\n"
    );
    assert_eq!(set("b: !!int 5\n", "/b", hi()), "b: hi\n");
    assert_eq!(set("b: !!str 5\n", "/b", Value::Int(7)), "b: 7\n");
    assert_eq!(
        set("l: [!!str 1, 2]\n", "/l/0", Value::Int(3)),
        "l: [3, 2]\n"
    );
    assert_eq!(
        set("b: &a !!float 5\nc: *a\n", "/b", Value::Int(7)),
        "b: &a 7\nc: *a\n"
    );
    assert_eq!(
        set("b: !custom x\n", "/b", Value::String("z".into())),
        "b: !custom z\n"
    );
}

// --- a collection where a scalar was ------------------------------------------

#[test]
fn set_of_a_collection_over_a_scalar_writes_it_in_flow_style() {
    let nested = Value::from_serde_json(&serde_json::json!({"nested": 1, "l": ["x"]}));
    assert_eq!(
        set("a: 3\nb: 4\n", "/a", nested.clone()),
        "a: {\"nested\": 1, \"l\": [\"x\"]}\nb: 4\n"
    );
    let mut doc = doc("a: 3\nb: 4\n");
    doc.set_at(&ptr("/a"), nested.clone()).expect("set_at /a");
    assert_eq!(ptr("/a").resolve(doc.value()).expect("/a"), &nested);
}

#[test]
fn set_of_a_new_key_writes_nested_collections_in_flow_style() {
    let json = |v| Value::from_serde_json(&v);
    assert_eq!(
        set("a: 1\n", "/b", json(serde_json::json!([{"x": 1}]))),
        "a: 1\nb:\n  - {\"x\": 1}\n"
    );
    assert_eq!(
        set("a: 1\n", "/b", json(serde_json::json!({"c": {"d": 1}}))),
        "a: 1\nb:\n  c: {\"d\": 1}\n"
    );
    assert_eq!(
        set(
            "l:\n  - containerPort: 80\n",
            "/l/0/resources",
            json(serde_json::json!({"limits": {"cpu": "1"}}))
        ),
        "l:\n  - containerPort: 80\n    resources:\n      limits: {\"cpu\": \"1\"}\n"
    );
}

#[test]
fn set_of_a_new_key_in_a_flow_mapping_stays_on_its_line() {
    let json = |v| Value::from_serde_json(&v);
    assert_eq!(
        set("a: {x: 1}\n", "/a/z", json(serde_json::json!([1]))),
        "a: {x: 1, z: [1]}\n"
    );
    assert_eq!(
        set("a: {}\n", "/a/z", json(serde_json::json!({"q": [1]}))),
        "a: {z: {\"q\": [1]}}\n"
    );
    assert_eq!(
        set("a: {x: 1}\n", "/a/z", Value::String("a, b".into())),
        "a: {x: 1, z: \"a, b\"}\n"
    );
}

#[test]
fn a_string_the_reader_would_type_otherwise_is_quoted() {
    for text in [
        ".nan", ".inf", "-.inf", "0x10", "0o17", "~", "Null", "yes", "off", "+1", ".5", "1e3",
    ] {
        assert_eq!(
            set("j: 1\n", "/j", Value::String(text.into())),
            format!("j: \"{text}\"\n"),
            "{text}"
        );
    }
    // dq reads these as strings, a YAML 1.1 reader (go-yaml, PyYAML) does
    // not: ints, sexagesimals, floats, timestamps, bools, merge and value
    // keys.
    for text in [
        "1_000",
        "0b101",
        "017",
        "1:20",
        "12:30:45",
        "190:20:30.15",
        "1.",
        "1_000.5",
        "2001-12-14",
        "2001-12-14t21:59:43.10-05:00",
        "2001-12-14 21:59:43.10 -5",
        "2001-12-14T21:59:43Z",
        "y",
        "N",
        "<<",
        "=",
    ] {
        assert_eq!(
            set("j: 1\n", "/j", Value::String(text.into())),
            format!("j: \"{text}\"\n"),
            "{text}"
        );
    }
    // What every schema takes as a string stays plain.
    for text in [
        "12:30:4x", "2024-1-1", "1.2.3", "a-b", "yes!", "<<a", "v1.0",
    ] {
        assert_eq!(
            set("j: 1\n", "/j", Value::String(text.into())),
            format!("j: {text}\n"),
            "{text}"
        );
    }
    assert_eq!(set("a: 1\n", "/null", Value::Int(2)), "a: 1\n\"null\": 2\n");
}

// --- del leaves an empty collection ------------------------------------------

#[test]
fn del_of_the_last_child_leaves_an_empty_collection() {
    assert_eq!(del("a:\n  x: 1\nb: 2\n", "/a/x"), "a: {}\nb: 2\n");
    assert_eq!(del("a:\n  - 1\nb: 2\n", "/a/0"), "a: []\nb: 2\n");
    assert_eq!(del("x: 1\n", "/x"), "{}\n");
    // The marker goes after node properties and before a comment.
    assert_eq!(del("a: &m\n  x: 1\nb: 2\n", "/a/x"), "a: &m {}\nb: 2\n");
    assert_eq!(del("a: # c\n  x: 1\nb: 2\n", "/a/x"), "a: {} # c\nb: 2\n");
    assert_eq!(del("a: &m # c\n  - 1\n", "/a/0"), "a: &m [] # c\n");
}

#[test]
fn emptied_root_keeps_the_byte_order_mark_and_the_line_break() {
    assert_eq!(del("\u{feff}x: 1\n", "/x"), "\u{feff}{}\n");
    assert_eq!(
        del("x: 1\r\n---\r\ny: 2\r\n", "/0/x"),
        "{}\r\n---\r\ny: 2\r\n"
    );
}

// --- del of a collection value -----------------------------------------------

#[test]
fn del_of_a_collection_value_removes_the_whole_entry() {
    assert_eq!(del("a:\n  x: 1\nb: 2\n", "/a"), "b: 2\n");
    assert_eq!(del("k: v\na:\n  x: 1\nb: 2\n", "/a"), "k: v\nb: 2\n");
    assert_eq!(del("a:\n  - 1\nb: 2\n", "/a"), "b: 2\n");
    assert_eq!(del("a: {}\nb: 2\n", "/a"), "b: 2\n");
    assert_eq!(del("a:\r\n  x: 1\r\nb: 2\r\n", "/a"), "b: 2\r\n");
    assert_eq!(
        del("k: 0\r\na:\r\n  - 1\r\n  - 2\r\nb: 2\r\n", "/a"),
        "k: 0\r\nb: 2\r\n"
    );
    // A flow collection over several lines.
    assert_eq!(
        del("a: {\n  x: 1,\n  y: [1,\n    2]\n}\nb: 2\n", "/a"),
        "b: 2\n"
    );
    assert_eq!(del("a: [\n  1,\n  2\n]\nb: 2\n", "/a"), "b: 2\n");
    // A comment after the collection belongs to what follows it.
    assert_eq!(
        del("a:\n  x: 1\n# about b\nb: 2\n", "/a"),
        "# about b\nb: 2\n"
    );
}

#[test]
fn del_of_a_collection_ending_in_an_empty_node_keeps_the_next_entry() {
    // The last child has no content of its own — an empty block scalar, an
    // empty node with properties — and must not take the next line along.
    for last_child in [
        "  s: |\n",
        "  x: !!str\n",
        "  x: &q\n",
        "  - &m\n",
        "  s: >-\n",
    ] {
        let src = format!("k: 0\na:\n{last_child}b: 1\nc: 2\n");
        assert_eq!(del(&src, "/a"), "k: 0\nb: 1\nc: 2\n", "{src:?}");
    }
    assert_eq!(del("b: &q\n  - &m\ng: 1\nw: ~\n", "/b"), "g: 1\nw: ~\n");
    assert_eq!(del("? v\n  t\nb: 1\n", "/v t"), "b: 1\n");
}

#[test]
fn del_of_an_empty_node_removes_only_its_entry() {
    assert_eq!(del("a:\n  s: |\nb: 1\n", "/a/s"), "a: {}\nb: 1\n");
    assert_eq!(del("f:\n  - &m\n", "/f/0"), "f: []\n");
    assert_eq!(del("c: !!str\n\n", "/c"), "{}\n\n");
    assert_eq!(del("e: |\n# c\n", "/e"), "{}\n# c\n");
    assert_eq!(del("? v\n  t\n", "/v t"), "{}\n");
}

// --- compact entries (`- key: v`, `- - v`) ------------------------------------

#[test]
fn del_of_the_first_entry_of_a_compact_item_hands_the_dash_on() {
    assert_eq!(del("l:\n  - x: 1\n    y: 2\n", "/l/0/x"), "l:\n  - y: 2\n");
    assert_eq!(del("l:\n  - x: 1\n    y: 2\n", "/l/0/y"), "l:\n  - x: 1\n");
    assert_eq!(
        del("l:\r\n  - x: 1\r\n    y: 2\r\n", "/l/0/x"),
        "l:\r\n  - y: 2\r\n"
    );
    assert_eq!(
        del("c:\n  - name: web\n    image: x\n", "/c/0/name"),
        "c:\n  - image: x\n"
    );
    assert_eq!(del("- a: {x: 1}\n  b: 2\n", "/0/a"), "- b: 2\n");
    assert_eq!(
        del("l:\n  - a:\n      p: 1\n    b: 2\n", "/l/0/a"),
        "l:\n  - b: 2\n"
    );
    assert_eq!(del("l:\n  - a:\n    b: 2\n", "/l/0/a"), "l:\n  - b: 2\n");
    assert_eq!(
        del("l:\n  - x: |\n      t\n    y: 2\n", "/l/0/x"),
        "l:\n  - y: 2\n"
    );
    assert_eq!(del("- - 1\n  - 2\n", "/0/0"), "- - 2\n");
    assert_eq!(del("- - - 1\n    - 2\n", "/0/0/0"), "- - - 2\n");
}

#[test]
fn del_of_the_first_entry_of_a_compact_item_keeps_the_comments_after_it() {
    assert_eq!(
        del("l:\n  - a: 1\n    # mid\n    b: 2\n", "/l/0/a"),
        "l:\n  - # mid\n    b: 2\n"
    );
    assert_eq!(
        del("l:\n  - a: 1\n    # m1\n    # m2\n    b: 2\n", "/l/0/a"),
        "l:\n  - # m1\n    # m2\n    b: 2\n"
    );
    assert_eq!(
        del("l:\n  - a: 1\n\n    # mid\n\n    b: 2\n", "/l/0/a"),
        "l:\n  - # mid\n\n    b: 2\n"
    );
    // Blank lines alone go with the entry.
    assert_eq!(
        del("l:\n  - a: 1\n\n    b: 2\n", "/l/0/a"),
        "l:\n  - b: 2\n"
    );
    // After a collection or a block scalar; a `#` inside it is content.
    assert_eq!(
        del("l:\n  - a:\n      x: 1\n    # mid\n    b: 2\n", "/l/0/a"),
        "l:\n  - # mid\n    b: 2\n"
    );
    assert_eq!(
        del(
            "l:\n  - a: |\n      t\n      # text\n    # mid\n    b: 2\n",
            "/l/0/a"
        ),
        "l:\n  - # mid\n    b: 2\n"
    );
    assert_eq!(
        del("l:\r\n  - a: 1\r\n    # mid\r\n    b: 2\r\n", "/l/0/a"),
        "l:\r\n  - # mid\r\n    b: 2\r\n"
    );
}

#[test]
fn del_of_the_only_entry_of_a_compact_item_leaves_an_empty_collection() {
    assert_eq!(del("- a: 1\n- b: 2\n", "/0/a"), "- {}\n- b: 2\n");
    assert_eq!(del("- a: {x: 1}\n", "/0/a"), "- {}\n");
    assert_eq!(del("- a: {}\n", "/0/a"), "- {}\n");
    assert_eq!(del("- a: 1 # c\n", "/0/a"), "- {} # c\n");
    assert_eq!(del("- - 1\n", "/0/0"), "- []\n");
}

#[test]
fn set_of_a_new_key_in_a_compact_item_lines_up_with_its_keys() {
    assert_eq!(
        set("l:\n  - x: 1\n", "/l/0/z", Value::Int(3)),
        "l:\n  - x: 1\n    z: 3\n"
    );
    assert_eq!(
        set("- - x: 1\n", "/0/0/z", Value::Int(3)),
        "- - x: 1\n    z: 3\n"
    );
    assert_eq!(
        set("l:\n- x: 1\n", "/l/0/z", Value::Int(3)),
        "l:\n- x: 1\n  z: 3\n"
    );
}

// --- anchors and aliases -----------------------------------------------------

/// `del_at` refuses and leaves the bytes alone.
fn del_refused(src: &str, pointer: &str) {
    let mut doc = doc(src);
    let err = doc.del_at(&ptr(pointer)).expect_err("del_at refuses");
    assert_eq!(err.kind_name(), "edit_rejected", "{src:?}: {err}");
    assert_eq!(doc.original_bytes(), src.as_bytes(), "{src:?}");
}

#[test]
fn del_of_an_anchor_an_alias_outside_refers_to_is_refused() {
    del_refused("a: &m\n  x: 1\nb: *m\n", "/a");
    del_refused("a:\n  inner: &m 1\nb: *m\n", "/a");
    del_refused("l:\n  - &m 1\n  - *m\n", "/l/0");
    del_refused("base: &b {x: 1}\nsvc:\n  <<: *b\n", "/base");
    del_refused("&k foo: 1\nb: *k\n", "/foo");
    // The alias refers to the later definition; without it, to the first.
    del_refused("a: &m 1\nb: &m 2\nc: *m\n", "/b");
}

#[test]
fn del_of_an_alias_or_inside_an_anchor_is_allowed() {
    assert_eq!(del("a: &m 1\nb: *m\n", "/b"), "a: &m 1\n");
    assert_eq!(del("a: &m 1\nb: &m 2\nc: *m\n", "/a"), "b: &m 2\nc: *m\n");
    assert_eq!(
        del("base: &b\n  x: 1\n  y: 2\nsvc:\n  <<: *b\n", "/base/x"),
        "base: &b\n  y: 2\nsvc:\n  <<: *b\n"
    );
}

#[test]
fn edit_below_an_alias_is_refused_naming_the_anchored_node() {
    let src = "a: &m\n  x: 1\nb: *m\n";
    for (action, pointer) in [("set", "/b/x"), ("set", "/b/new"), ("delete", "/b/x")] {
        let mut yaml = doc(src);
        let result = if action == "set" {
            yaml.set_at(&ptr(pointer), Value::Int(5))
        } else {
            yaml.del_at(&ptr(pointer))
        };
        let err = result.expect_err("an edit below an alias is refused");
        assert_eq!(err.kind_name(), "edit_rejected", "{pointer}: {err}");
        let message = err.to_string();
        assert!(
            message.contains("under the alias on line 3, a copy of /a")
                && message.contains("`dq set --jq`"),
            "{message}"
        );
        assert_eq!(yaml.original_bytes(), src.as_bytes(), "{pointer}");
    }
    // A key that is not there is still not found.
    let err = doc(src).del_at(&ptr("/b/nope")).expect_err("no such key");
    assert_eq!(err.kind_name(), "path", "{err}");
}

// --- read-back check ---------------------------------------------------------

#[test]
fn edit_under_an_anchor_also_changes_its_aliases() {
    // compose / k8s: `<<: *base` shares the anchored node, so the edit
    // reaches the copy the value tree keeps under `<<`.
    let src = "base: &b\n  x: 1\nsvc:\n  <<: *b\n  y: 3\n";
    let mut yaml = doc(src);
    yaml.set_at(&ptr("/base/x"), Value::Int(5)).expect("set_at");
    assert_eq!(
        yaml.original_bytes(),
        b"base: &b\n  x: 5\nsvc:\n  <<: *b\n  y: 3\n"
    );
    assert_eq!(
        ptr("/svc/<</x").resolve(yaml.value()).ok(),
        Some(&Value::Int(5))
    );

    let mut yaml = doc("a: &x 1\nl: [*x, 2]\n");
    yaml.set_at(&ptr("/a"), Value::Int(7)).expect("set_at");
    assert_eq!(yaml.original_bytes(), b"a: &x 7\nl: [*x, 2]\n");
    assert_eq!(ptr("/l/0").resolve(yaml.value()).ok(), Some(&Value::Int(7)));

    let mut yaml = doc(src);
    yaml.del_at(&ptr("/base/x")).expect("del_at");
    assert_eq!(
        yaml.original_bytes(),
        b"base: &b {}\nsvc:\n  <<: *b\n  y: 3\n"
    );
    assert!(ptr("/svc/<</x").resolve(yaml.value()).is_err());
}
