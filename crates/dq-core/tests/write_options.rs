//! Integration tests for [`dq_core::Format::write_with_options`] (M4 §2).
//!
//! These tests exercise the four registered formats end-to-end through the
//! public API: parse a fixture, render via `write_with_options`, and assert
//! on the bytes. The contract is twofold:
//!
//! 1. `WriteOptions::default()` must produce **byte-identical** output to the
//!    legacy `write` method. M2's textual-edit splice path and the M2-era
//!    pretty-printer both rely on this — any divergence would break the
//!    `dq fmt` no-op invariant on already-canonical files.
//! 2. Opted-in options (`sort_keys`, `indent`) must produce the documented
//!    transformation: deep alphabetical key sort, configurable JSON indent
//!    step, and (for M4) a no-op for YAML/TOML on `--indent`.

use dq_core::{Document, WriteOptions, by_name};
use pretty_assertions::assert_eq;

/// Render `doc` via `write_with_options` and return the bytes.
///
/// Centralising the boilerplate keeps each test focused on the transformation
/// under inspection rather than the format-lookup dance.
fn write_with(format: &str, doc: &Document, opts: &WriteOptions) -> Vec<u8> {
    let fmt = by_name(format).expect("format must be registered");
    let mut buf: Vec<u8> = Vec::new();
    fmt.write_with_options(doc, &mut buf, opts)
        .unwrap_or_else(|e| panic!("{format} write_with_options failed: {e}"));
    buf
}

/// Render `doc` via the legacy `write` method.
fn write_default(format: &str, doc: &Document) -> Vec<u8> {
    let fmt = by_name(format).expect("format must be registered");
    let mut buf: Vec<u8> = Vec::new();
    fmt.write(doc, &mut buf)
        .unwrap_or_else(|e| panic!("{format} write failed: {e}"));
    buf
}

/// Parse `s` via the registry-resolved format named `format`.
fn parse(format: &str, s: &str) -> Document {
    let fmt = by_name(format).expect("format must be registered");
    fmt.parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("{format} parse failed: {e}"))
}

#[test]
fn json_write_with_options_default_matches_write() {
    // Several shapes covering the M2 baseline: write-aware (parsed JSON
    // with original_bytes populated) and value-only (constructed without a
    // source buffer). Every fixture must produce byte-identical output for
    // `WriteOptions::default()` so `dq fmt` on a canonical file is a no-op.
    let fixtures = [
        r#"{"a":1}"#,
        r#"{"name": "dq", "version": 1}"#,
        r#"{
  "deeply": {
    "nested": {
      "value": 42
    }
  }
}"#,
        // Big-int literal must round-trip through the default path verbatim.
        r#"{"id": 4722366482869645213696}"#,
    ];
    for input in fixtures {
        let doc = parse("json", input);
        let default_bytes = write_default("json", &doc);
        let opts_bytes = write_with("json", &doc, &WriteOptions::default());
        assert_eq!(
            default_bytes, opts_bytes,
            "WriteOptions::default() must be byte-identical to write() for input: {input}",
        );
    }
}

#[test]
fn json_sort_keys_round_trip() {
    // `{"z":1,"a":2}` → after sort_keys, key `a` precedes `z` in the output.
    // The presence of `"a"` followed by `"z"` (in source order) confirms the
    // canonicalisation took effect; we don't assert a specific whitespace
    // shape because `--sort-keys` alone keeps the M2 default 2-space indent.
    let doc = parse("json", r#"{"z":1,"a":2}"#);
    let mut opts = WriteOptions::default();
    opts.sort_keys = true;
    let out = write_with("json", &doc, &opts);
    let s = String::from_utf8(out).expect("UTF-8 JSON output");
    let pos_a = s.find("\"a\"").expect("key 'a' present");
    let pos_z = s.find("\"z\"").expect("key 'z' present");
    assert!(
        pos_a < pos_z,
        "after --sort-keys, 'a' must precede 'z' in output: {s}",
    );
}

#[test]
fn json_indent_4_uses_four_space_steps() {
    // 4-space indent means the inner key's leading whitespace is exactly
    // 4 spaces. Pinning the literal byte sequence catches any regression in
    // the indent-step plumbing (e.g. forgetting to multiply by N).
    let doc = parse("json", r#"{"a":1,"b":2}"#);
    let mut opts = WriteOptions::default();
    opts.indent = Some(4);
    let out = write_with("json", &doc, &opts);
    let s = String::from_utf8(out).expect("UTF-8 output");
    // The opening `{\n    "a"` substring uniquely pins 4-space indent.
    assert!(
        s.contains("{\n    \"a\""),
        "expected 4-space indent before first key; got: {s}",
    );
    assert!(
        s.contains("\n    \"b\""),
        "expected 4-space indent before second key; got: {s}",
    );
}

#[test]
fn json_indent_0_emits_compact_single_line() {
    // `Some(0)` → compact: no inner whitespace, no newlines. The only
    // newlines that may appear are inside string values (none here).
    let doc = parse("json", r#"{"a":1,"b":[1,2,3]}"#);
    let mut opts = WriteOptions::default();
    opts.indent = Some(0);
    let out = write_with("json", &doc, &opts);
    let s = String::from_utf8(out).expect("UTF-8 output");
    assert!(
        !s.contains('\n'),
        "indent=0 must produce single-line output; got: {s}",
    );
    // Compact shape: `{"a":1,"b":[1,2,3]}`.
    assert_eq!(s, r#"{"a":1,"b":[1,2,3]}"#);
}

#[test]
fn jsonl_sort_keys_per_line() {
    // JSONL = one JSON record per line. Each line must be sorted
    // independently. We construct three records with mixed key orders and
    // assert that each output line has its keys alphabetised.
    let doc = parse(
        "jsonl",
        "{\"z\":1,\"a\":2}\n{\"y\":3,\"b\":4}\n{\"x\":5,\"c\":6}\n",
    );
    let mut opts = WriteOptions::default();
    opts.sort_keys = true;
    let out = write_with("jsonl", &doc, &opts);
    let s = String::from_utf8(out).expect("UTF-8 output");
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines.len(), 3, "must emit one record per line");
    // Each line: the alphabetically-first key in its source must precede
    // the alphabetically-last key. A simple "find both, compare positions"
    // suffices because the keys are distinct single chars.
    for (line, (small, big)) in lines.iter().zip([("a", "z"), ("b", "y"), ("c", "x")]) {
        let pos_small = line.find(&format!("\"{small}\"")).expect("small key");
        let pos_big = line.find(&format!("\"{big}\"")).expect("big key");
        assert!(
            pos_small < pos_big,
            "line `{line}`: '{small}' must precede '{big}' after --sort-keys",
        );
    }
}

#[test]
fn yaml_sort_keys_round_trip() {
    // YAML keys are sorted by `canonicalize_keys` before serde_norway emits.
    // The output is whatever shape serde_norway produces (no comments,
    // canonical block style); the only invariant we pin is the order of
    // the top-level keys.
    let doc = parse("yaml", "z: 1\na: 2\nm: 3\n");
    let mut opts = WriteOptions::default();
    opts.sort_keys = true;
    let out = write_with("yaml", &doc, &opts);
    let s = String::from_utf8(out).expect("UTF-8 output");
    let pos_a = s.find("a:").expect("key 'a' present");
    let pos_m = s.find("m:").expect("key 'm' present");
    let pos_z = s.find("z:").expect("key 'z' present");
    assert!(
        pos_a < pos_m && pos_m < pos_z,
        "after --sort-keys: a < m < z in YAML output: {s}",
    );
}

#[test]
fn toml_sort_keys_round_trip() {
    // TOML re-emit through `toml::to_string_pretty` after canonicalize.
    // Top-level scalar keys come first in alphabetical order.
    let input = "z = 1\na = 2\nm = 3\n";
    let doc = parse("toml", input);
    let mut opts = WriteOptions::default();
    opts.sort_keys = true;
    let out = write_with("toml", &doc, &opts);
    let s = String::from_utf8(out).expect("UTF-8 output");
    let pos_a = s.find("a =").expect("key 'a' present");
    let pos_m = s.find("m =").expect("key 'm' present");
    let pos_z = s.find("z =").expect("key 'z' present");
    assert!(
        pos_a < pos_m && pos_m < pos_z,
        "after --sort-keys: a < m < z in TOML output: {s}",
    );
}

#[test]
fn json_sort_keys_preserves_big_int() {
    // Big-numeric literals MUST survive a `--sort-keys` re-emit. The
    // Value tree carries them as `Value::BigInt(literal)`; the JSON
    // writer's custom path emits them as raw bytes (not as JSON strings).
    // If a future refactor accidentally routed BigInt through serde_json's
    // generic `Serialize` impl, the literal would become a quoted string
    // and this test would fail.
    let big = "4722366482869645213696";
    let input = format!(r#"{{"z":1,"id":{big},"a":2}}"#);
    let doc = parse("json", &input);
    let mut opts = WriteOptions::default();
    opts.sort_keys = true;
    let out = write_with("json", &doc, &opts);
    let s = String::from_utf8(out).expect("UTF-8 output");
    assert!(
        s.contains(big),
        "big-int literal must survive --sort-keys: {s}",
    );
    // It must be emitted as a numeric token, not a JSON string. The
    // literal preceded by `"` would mean it became a string. Asserting on
    // the surrounding context pins the numeric-token contract.
    let big_start = s.find(big).expect("big-int substring present");
    assert!(
        s.as_bytes()[big_start.saturating_sub(1)] != b'"',
        "big-int must NOT be emitted as a quoted string; got: {s}",
    );
}

#[test]
fn yaml_write_with_options_default_matches_write() {
    // YAML default-equivalence: M2's `serde_norway` emitter must keep producing
    // identical bytes when no options are set. The CLI dispatch layer relies
    // on this when threading WriteOptions through every write command.
    let doc = parse("yaml", "name: dq\nversion: 1\n");
    let default_bytes = write_default("yaml", &doc);
    let opts_bytes = write_with("yaml", &doc, &WriteOptions::default());
    assert_eq!(
        default_bytes, opts_bytes,
        "YAML WriteOptions::default() must be byte-identical to write()",
    );
}

#[test]
fn toml_write_with_options_default_matches_write() {
    // TOML default-equivalence: write-aware documents return their
    // `original_bytes` verbatim through both paths.
    let input = "name = \"dq\"\nversion = 1\n";
    let doc = parse("toml", input);
    let default_bytes = write_default("toml", &doc);
    let opts_bytes = write_with("toml", &doc, &WriteOptions::default());
    assert_eq!(
        default_bytes, opts_bytes,
        "TOML WriteOptions::default() must be byte-identical to write()",
    );
}
