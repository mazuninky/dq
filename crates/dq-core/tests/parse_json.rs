//! Component tests for the JSON / JSONL parsers and writers.
//!
//! These tests target behaviours that the JSON spec or `dq-core`'s contract
//! call out specifically: object key order, byte-for-byte round-trip of
//! arbitrary-precision integers, JSONL collapsing into an array, and the two
//! whitespace shapes the writer produces.

use dq_core::{Document, Value, by_name};
use pretty_assertions::assert_eq;

/// Parse `s` via the registry-resolved JSON format.
fn parse_json(s: &str) -> Document {
    let fmt = by_name("json").expect("json format must be registered");
    fmt.parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("json parse failed: {e}"))
}

/// Parse `s` via the registry-resolved JSONL format.
fn parse_jsonl(s: &str) -> Document {
    let fmt = by_name("jsonl").expect("jsonl format must be registered");
    fmt.parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("jsonl parse failed: {e}"))
}

/// Serialize `doc` via the named format and return as `String`.
fn write_to_string(format: &str, doc: &Document) -> String {
    let fmt = by_name(format).expect("format must be registered");
    let mut buf: Vec<u8> = Vec::new();
    fmt.write(doc, &mut buf)
        .unwrap_or_else(|e| panic!("{format} write failed: {e}"));
    String::from_utf8(buf).expect("writer must produce UTF-8")
}

#[test]
fn parse_preserves_object_key_order() {
    // Distinct keys in non-alphabetical order must round-trip in the original
    // sequence — this is the contract promised by `serde_json`'s
    // `preserve_order` feature plus our `IndexMap` storage.
    let doc = parse_json(r#"{"z": 1, "a": 2, "m": 3, "b": 4}"#);
    let Value::Map(m) = doc.value() else {
        panic!("expected Map, got: {doc:?}");
    };
    let keys: Vec<&str> = m.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["z", "a", "m", "b"],
        "key order must match input byte order"
    );
}

#[test]
fn big_int_round_trips_byte_for_byte() {
    // `4722366482869645213696` overflows i64::MAX (~9.22e18). The JSON parser
    // must keep the literal verbatim in `Value::BigInt`, and the writer must
    // emit the same digits back. This is the contract called out in the
    // `format-support` spec under "Number representation preservation".
    //
    // After M2 §5 the JSON writer is byte-preserving (it copies
    // `original_bytes` verbatim when populated); the contract still holds
    // because the original literal is part of those bytes.
    const BIG: &str = "4722366482869645213696";
    let input = format!(r#"{{"id": {BIG}}}"#);
    let doc = parse_json(&input);

    let Value::Map(m) = doc.value() else {
        panic!("expected Map");
    };
    assert_eq!(
        m.get("id"),
        Some(&Value::BigInt(BIG.to_owned())),
        "parser must preserve the original literal verbatim"
    );

    // Byte-preserving write: the literal must round-trip exactly.
    let out = write_to_string("json", &doc);
    assert!(
        out.contains(BIG),
        "writer must emit the big-int literal byte-for-byte. Got:\n{out}"
    );
}

#[test]
fn jsonl_stream_collapses_to_array_of_three_objects() {
    // JSONL parses to `Value::Array` of records. Each record must be a `Map`,
    // and the count is exact — blank lines are skipped (we have one).
    let stream = "{\"id\":1}\n{\"id\":2}\n\n{\"id\":3}\n";
    let doc = parse_jsonl(stream);

    let Value::Array(items) = doc.value() else {
        panic!("expected Array, got: {doc:?}");
    };
    assert_eq!(
        items.len(),
        3,
        "JSONL collapses to 3 records (blank line skipped)"
    );
    for (i, item) in items.iter().enumerate() {
        let Value::Map(m) = item else {
            panic!("expected Map at index {i}, got: {item:?}");
        };
        let expected_id = (i as i64) + 1;
        assert_eq!(
            m.get("id"),
            Some(&Value::Int(expected_id)),
            "record {i} should have id={expected_id}"
        );
    }
}

#[test]
fn json_compact_output_has_no_trailing_whitespace() {
    // The compact path is exposed through the `Jsonl` writer (which serializes
    // each top-level array element as a compact line). We test it via that
    // public surface so the test exercises the same code the CLI uses.
    let doc = parse_jsonl("{\"a\":1,\"b\":2}\n");
    let out = write_to_string("jsonl", &doc);

    // No leading/trailing space, no indent, no `\r`.
    assert!(
        !out.contains("  "),
        "compact JSONL must not contain double-space indents: {out:?}"
    );
    assert!(
        !out.contains('\r'),
        "compact output must not include CR: {out:?}"
    );
    // Every line is either empty or terminated by a newline. There must be
    // exactly one record (and so exactly one line). The line itself must end
    // immediately after the closing `}`.
    let line = out.trim_end_matches('\n');
    assert!(
        line.ends_with('}'),
        "compact line must end with `}}`, got: {line:?}"
    );
    // No space immediately before the closing brace.
    assert!(
        !line.ends_with(" }"),
        "compact line must not have whitespace before the closing brace"
    );
}

#[test]
fn json_writer_round_trips_source_bytes() {
    // After M2 §5 the JSON writer preserves the source byte-for-byte
    // (matching the TOML §4 contract): every byte of the input — including
    // whitespace, key order, indent style — round-trips through `parse` /
    // `write`. This replaces the M1-era "writer always pretty-prints"
    // behaviour; the pretty path is now only exercised when the document
    // was built without source bytes (e.g. via `dq convert` from a
    // different format), which is covered separately.
    let input = "{\n  \"a\": [\n    1,\n    2\n  ]\n}";
    let doc = parse_json(input);
    let out = write_to_string("json", &doc);
    assert_eq!(out, input, "writer must round-trip the source bytes");
}

#[test]
fn json_value_only_writer_uses_two_space_indent() {
    // The pretty writer still drives the `value_only` (no-source-bytes)
    // path — exercised by `dq convert` from another format. Build a
    // value-only Document explicitly to test it.
    use dq_core::FormatTag;
    use indexmap::IndexMap;
    let mut map = IndexMap::new();
    map.insert("a".into(), Value::Array(vec![Value::Int(1), Value::Int(2)]));
    let doc = Document::value_only(Value::Map(map), FormatTag::Json);
    let out = write_to_string("json", &doc);

    let expected = "{\n  \"a\": [\n    1,\n    2\n  ]\n}";
    assert_eq!(
        out, expected,
        "value-only writer must produce two-space indent"
    );
}
