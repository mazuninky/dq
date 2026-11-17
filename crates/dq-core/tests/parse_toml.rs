//! Component tests for the TOML parser and writer.
//!
//! TOML's structural shapes (nested tables, arrays of tables) and its M1
//! simplifications (datetime preserved as `Value::String`, no multi-doc) get
//! their own tests here so any future change to that contract surfaces in CI.

use dq_core::{Document, Error, Pointer, Value, by_name};
use pretty_assertions::assert_eq;

fn parse_toml(s: &str) -> Document {
    let fmt = by_name("toml").expect("toml format must be registered");
    fmt.parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("toml parse failed: {e}"))
}

fn try_parse_toml(s: &str) -> dq_core::Result<Document> {
    let fmt = by_name("toml").expect("toml format must be registered");
    fmt.parse(s.as_bytes())
}

fn write_toml(doc: &Document) -> dq_core::Result<String> {
    let fmt = by_name("toml").expect("toml format must be registered");
    let mut buf: Vec<u8> = Vec::new();
    fmt.write(doc, &mut buf)?;
    Ok(String::from_utf8(buf).expect("toml writer must produce UTF-8"))
}

fn get<'a>(doc: &'a Document, pointer: &str) -> &'a Value {
    let p = Pointer::parse(pointer).expect("pointer parses");
    if doc.is_multi() {
        panic!("TOML never produces a multi-doc");
    }
    p.resolve(doc.value())
        .unwrap_or_else(|e| panic!("resolve failed for `{pointer}`: {e}"))
}

#[test]
fn nested_tables_become_nested_maps() {
    // `[server]` and `[server.tls]` are two distinct sections in source; the
    // parser must build them as nested objects (a `tls` key on the `server`
    // map), not as separate top-level keys.
    let src = r#"
[server]
host = "localhost"
port = 8080

[server.tls]
cert = "/etc/dq/cert.pem"
key  = "/etc/dq/key.pem"
"#;
    let doc = parse_toml(src);

    assert_eq!(
        get(&doc, "/server/host"),
        &Value::String("localhost".into())
    );
    assert_eq!(get(&doc, "/server/port"), &Value::Int(8080));
    assert_eq!(
        get(&doc, "/server/tls/cert"),
        &Value::String("/etc/dq/cert.pem".into()),
    );
    assert_eq!(
        get(&doc, "/server/tls/key"),
        &Value::String("/etc/dq/key.pem".into()),
    );
}

#[test]
fn arrays_of_tables_become_arrays_of_maps() {
    // `[[products]]` defines an array of tables. Each occurrence appends a
    // new `Map` to the `products` array.
    let src = r#"
[[products]]
name = "alpha"
sku  = 1

[[products]]
name = "beta"
sku  = 2
"#;
    let doc = parse_toml(src);

    let Value::Array(items) = get(&doc, "/products") else {
        panic!("/products must be an array");
    };
    assert_eq!(
        items.len(),
        2,
        "two `[[products]]` entries → two array items"
    );

    assert_eq!(
        get(&doc, "/products/0/name"),
        &Value::String("alpha".into())
    );
    assert_eq!(get(&doc, "/products/0/sku"), &Value::Int(1));
    assert_eq!(get(&doc, "/products/1/name"), &Value::String("beta".into()));
    assert_eq!(get(&doc, "/products/1/sku"), &Value::Int(2));
}

#[test]
fn datetime_literal_preserved_as_string_m1() {
    // M1 simplification: TOML datetime literals are flattened to their
    // textual form via `Value::String`. M2 will introduce a dedicated
    // datetime variant once the document model supports it. This test
    // exists to catch any silent regression of the M1 contract.
    let src = r#"
released = 2026-05-03T10:30:00Z
"#;
    let doc = parse_toml(src);

    let v = get(&doc, "/released");
    let Value::String(s) = v else {
        panic!(
            "M1 stores TOML datetimes as Value::String; got: {v:?}. \
             A new `Value::Datetime` variant is M2 work — update this test \
             only as part of that change."
        );
    };
    assert!(
        s.contains("2026-05-03"),
        "datetime literal must round-trip the date portion, got: {s:?}"
    );
}

#[test]
fn integer_overflow_surfaces_as_parse_error() {
    // TOML's spec says integers fit in `i64`. The `toml` crate honours that
    // by rejecting bigger literals at parse time, which our adapter wraps
    // into `Error::Parse`. We document that contract here — if a future
    // version of the `toml` crate gains BigInt support we can update this
    // test to assert `Value::BigInt` instead.
    let src = r#"id = 4722366482869645213696"#;
    let err = try_parse_toml(src).expect_err("overflowing literal must error");
    assert_eq!(
        err.kind_name(),
        "parse",
        "overflow surfaces as a Parse error, not a Format error"
    );
}

#[test]
fn writer_rejects_multi_document() {
    // TOML cannot represent a multi-document stream — feeding a multi-doc
    // `Document` to the TOML writer must yield `Error::Format` (NOT
    // `Error::Io`, `Error::Parse`, or a silent success).
    let multi = Document::multi(vec![Value::Int(1), Value::Int(2)]);
    let err = write_toml(&multi).expect_err("multi-doc TOML write must fail");
    assert!(
        matches!(
            &err,
            Error::Format { format, .. } if *format == "toml"
        ),
        "expected Error::Format {{ format: \"toml\", .. }}, got: {err:?}"
    );
}
