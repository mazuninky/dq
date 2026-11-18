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

// --- an empty value -----------------------------------------------------------

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

// --- a collection where a scalar was ------------------------------------------

#[test]
fn set_of_a_collection_over_a_scalar_writes_it_in_flow_style() {
    let nested = Value::from_serde_json(&serde_json::json!({"nested": 1, "l": ["x"]}));
    assert_eq!(
        set("a: 3\nb: 4\n", "/a", nested.clone()),
        "a: {\"nested\":1,\"l\":[\"x\"]}\nb: 4\n"
    );
    let mut doc = doc("a: 3\nb: 4\n");
    doc.set_at(&ptr("/a"), nested.clone()).expect("set_at /a");
    assert_eq!(ptr("/a").resolve(doc.value()).expect("/a"), &nested);
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

#[test]
fn del_of_a_collection_under_a_compact_sequence_entry_is_refused() {
    // `- name: {…}`: the key shares its line with the item's `-`, and
    // removing the line would take the item with it.
    let mut doc = doc("l:\n  - name: {n: 1}\n    age: 2\n");
    assert!(doc.del_at(&ptr("/l/0/name")).is_err());
    assert_eq!(doc.original_bytes(), b"l:\n  - name: {n: 1}\n    age: 2\n");
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
