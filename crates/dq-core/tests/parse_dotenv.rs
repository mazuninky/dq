//! Component tests for the `.env` parser/writer.
//!
//! Stage 2's inline tests cover three minimal cases (simple, export, double-quoted
//! escapes). These tests pin the rest of the `.env` contract: single-quoted
//! literals, comments, blank lines, inline comments after unquoted values,
//! and the writer's quoting policy.

use dq_core::{Document, Format, Value};
use indexmap::IndexMap;
use pretty_assertions::assert_eq;

fn dotenv() -> &'static dyn Format {
    dq_core::by_name("dotenv").expect("dotenv format must be registered")
}

fn parse_env(s: &str) -> Document {
    dotenv()
        .parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("dotenv parse failed: {e}\n---input---\n{s}\n-----------"))
}

/// Extract the single string at `key`, panicking with a descriptive message
/// if the value is missing or not a string.
fn get_string<'a>(doc: &'a Document, key: &str) -> &'a str {
    let Value::Map(m) = doc.value() else {
        panic!("expected top Map, got: {:?}", doc.value());
    };
    match m.get(key) {
        Some(Value::String(s)) => s.as_str(),
        Some(other) => panic!("expected /{key} to be a String, got: {other:?}"),
        None => panic!(
            "expected /{key} present, keys = {:?}",
            m.keys().collect::<Vec<_>>()
        ),
    }
}

#[test]
fn parse_simple_assignment_produces_string_value() {
    let doc = parse_env("KEY=value\n");
    assert_eq!(get_string(&doc, "KEY"), "value");
}

#[test]
fn parse_double_quoted_value_with_whitespace() {
    // Spec: `KEY="hello world"` → value carries the literal "hello world"
    // (no trimming, no quote characters in the stored string).
    let doc = parse_env(r#"KEY="hello world""#);
    assert_eq!(
        get_string(&doc, "KEY"),
        "hello world",
        "double-quoted whitespace must survive verbatim",
    );
}

#[test]
fn parse_double_quoted_escape_sequences() {
    // `\n`/`\t`/`\r`/`\\`/`\"` must be expanded inside double-quoted values.
    // The hand-rolled scanner is small enough that bugs in the escape table
    // are easy to introduce; pin every documented escape.
    let doc = parse_env(r#"M="line1\nline2\ttab\\bs\"quote""#);
    assert_eq!(
        get_string(&doc, "M"),
        "line1\nline2\ttab\\bs\"quote",
        "every documented escape sequence must be expanded",
    );
}

#[test]
fn parse_export_prefix_strips_keyword_and_extra_whitespace() {
    // `export KEY=value` is shell convention. The parser must strip both
    // `export` and the following whitespace; `KEY` becomes the map key.
    let doc = parse_env("export PATH=/usr/bin\n");
    assert_eq!(get_string(&doc, "PATH"), "/usr/bin");
    // Ensure no key with the literal `export` prefix leaked through.
    let Value::Map(m) = doc.value() else { panic!() };
    assert!(
        !m.keys().any(|k| k.contains("export")),
        "`export` must be stripped, not stored as part of the key",
    );
}

#[test]
fn parse_skips_comments_and_blank_lines() {
    // `# comment` and blank lines must be ignored. Pinning this prevents a
    // regression where a trimmed-only line slips through and produces an
    // `Error::Parse` for the empty key.
    let source = "# leading comment\n\nKEY=value\n   # indented comment\n\nOTHER=x\n\n";
    let doc = parse_env(source);
    let Value::Map(m) = doc.value() else { panic!() };
    let keys: Vec<&str> = m.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["KEY", "OTHER"],
        "only KEY and OTHER must survive; got: {keys:?}",
    );
}

#[test]
fn write_quotes_value_containing_whitespace_and_uses_double_quotes() {
    // Per design D4: values needing quotes use `"…"`, never `'…'`. Round-
    // trip is "value bytes preserve, but the rendered line is canonical".
    let mut map = IndexMap::new();
    map.insert("M".to_string(), Value::String("a b".into()));
    let doc = Document::value_only(Value::Map(map), dq_core::FormatTag::DotEnv);
    let mut buf: Vec<u8> = Vec::new();
    dotenv()
        .write(&doc, &mut buf)
        .expect("dotenv write of a string with whitespace must succeed");
    let s = String::from_utf8(buf).expect("dotenv writer produces utf-8");
    assert_eq!(
        s, "M=\"a b\"\n",
        "a value containing whitespace must be wrapped in double quotes",
    );
}

#[test]
fn parse_unquoted_value_strips_inline_comment_after_whitespace() {
    // `KEY=value # comment` — the inline `#` is preceded by whitespace and
    // must be treated as the start of a comment, leaving the trimmed value.
    // The `#` inside a token (`KEY=v#alue`) is part of the value and must
    // NOT be stripped — which the implementation tracks via `prev_was_ws`.
    let doc = parse_env("KEY=value # trailing comment\n");
    assert_eq!(
        get_string(&doc, "KEY"),
        "value",
        "inline comment after whitespace must be stripped from the unquoted value",
    );

    // Anti-test: `#` glued onto the value is part of the value.
    let doc2 = parse_env("ANCHOR=v#alue\n");
    assert_eq!(
        get_string(&doc2, "ANCHOR"),
        "v#alue",
        "`#` without preceding whitespace is part of the value, not a comment",
    );
}
