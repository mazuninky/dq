//! Component tests for the INI / `.properties` parser/writer (`rust-ini`).
//!
//! Stage 2's inline `#[cfg(test)]` covers basic parse and section-order
//! preservation. These tests pin the rest of the documented contract:
//! anonymous sections under the empty-string key, the `:` separator,
//! round-trip via parse→write→parse, and parse-error variant pinning.

use camino::Utf8Path;
use dq_core::{Document, Format, Value};
use pretty_assertions::assert_eq;

fn ini() -> &'static dyn Format {
    dq_core::by_name("ini").expect("ini format must be registered")
}

fn parse_ini(s: &str) -> Document {
    ini()
        .parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("ini parse failed: {e}\n---input---\n{s}\n-----------"))
}

#[test]
fn parse_simple_section_and_key_pair() {
    // The fundamental `[section] key = value` shape — pin that the section
    // becomes a nested Map keyed by section name and that the value lands as
    // a plain `Value::String` (no numeric type inference).
    let doc = parse_ini("[server]\nport = 8080\n");
    let Value::Map(top) = doc.value() else {
        panic!("expected top-level Map, got: {:?}", doc.value());
    };
    let Some(Value::Map(server)) = top.get("server") else {
        panic!("missing [server] section, got: {top:?}");
    };
    assert_eq!(
        server.get("port"),
        Some(&Value::String("8080".into())),
        "INI is fundamentally string→string; the value must NOT be coerced to Int",
    );
}

#[test]
fn parse_anonymous_section_keys_under_empty_string() {
    // Keys appearing before any `[header]` are stored under the empty-string
    // section name `""` so callers can address them through a stable JSON
    // pointer like `//log_level`. (Two slashes — empty key + child key.)
    let doc = parse_ini("log_level = info\n[server]\nport = 80\n");
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    let Some(Value::Map(anon)) = top.get("") else {
        panic!("anonymous section must be keyed under empty string, got top: {top:?}");
    };
    assert_eq!(
        anon.get("log_level"),
        Some(&Value::String("info".into())),
        "key from the anonymous section must round-trip",
    );
    // The named section is also reachable.
    let Some(Value::Map(server)) = top.get("server") else {
        panic!("named section [server] missing");
    };
    assert!(server.contains_key("port"));
}

#[test]
fn parse_multiple_sections_preserve_source_order() {
    // Source order matters because INI files are usually read by humans —
    // arbitrary reordering would surprise users. `IndexMap` guarantees this
    // shape; the test pins it so a future swap to a `BTreeMap` would surface.
    //
    // The parser skips the always-present empty anonymous section that
    // `rust-ini` exposes for sources with no anonymous keys, so the top-level
    // map only carries the named sections — in source order.
    let doc = parse_ini("[c]\nx = 1\n[a]\ny = 2\n[b]\nz = 3\n");
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    let order: Vec<&str> = top.keys().map(String::as_str).collect();
    assert_eq!(
        order,
        vec!["c", "a", "b"],
        "named-section order must be preserved (insertion order, not alphabetic); \
         the empty anonymous section is dropped because the source has no \
         anonymous keys",
    );
}

#[test]
fn parse_colon_separator_works_like_equals() {
    // `.properties` files use `:` as the key/value separator. `rust-ini`
    // accepts both by default; pin the contract so a future config change
    // (e.g. `EscapePolicy::Basics` only) doesn't drop colon support silently.
    let doc = parse_ini("[s]\nkey: value\n");
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    let Some(Value::Map(s)) = top.get("s") else {
        panic!("missing [s]")
    };
    assert_eq!(
        s.get("key"),
        Some(&Value::String("value".into())),
        "`:` separator must produce the same value as `=`",
    );
}

#[test]
fn round_trip_through_parse_write_parse_is_semantically_equivalent() {
    // INI's writer drops comments and quote style by design (D5). The
    // structural contract is: keys, values, and section ORDER survive a
    // parse → write → parse trip. We pin the order via key-vector
    // comparison, the values via tree equality.
    let source = "log = info\n[server]\nport = 80\nhost = localhost\n[client]\ntimeout = 30\n";
    let doc1 = parse_ini(source);
    let mut buf: Vec<u8> = Vec::new();
    ini()
        .write(&doc1, &mut buf)
        .expect("ini write must succeed for a clean parsed tree");
    let rendered = String::from_utf8(buf).expect("ini writer produces utf-8");
    let doc2 = parse_ini(&rendered);
    assert_eq!(
        doc1.value(),
        doc2.value(),
        "round-trip must preserve the value tree exactly",
    );
    // And section order survives the trip too.
    let Value::Map(top) = doc2.value() else {
        panic!()
    };
    let order: Vec<&str> = top.keys().map(String::as_str).collect();
    assert_eq!(
        order,
        vec!["", "server", "client"],
        "section order must be preserved through the round trip",
    );
}

#[test]
fn parse_malformed_input_returns_parse_error_variant() {
    // `rust-ini` rejects an unterminated section header with a parse error;
    // we wrap that into our own `Error::Parse`. Pinning the variant ensures
    // exit-code mapping picks PARSE_ERROR (3).
    let err = ini()
        .parse(b"[unterminated\nkey = value\n")
        .expect_err("malformed [section] header must error");
    assert!(
        matches!(err, dq_core::Error::Parse { .. }),
        "expected Error::Parse, got: {err:?}",
    );
}

#[test]
fn registry_detects_ini_and_properties_extensions() {
    // Both `.ini` and `.properties` (and `.cfg`) must dispatch to the INI
    // parser. Catches a regression where the registry's `extensions()` slice
    // accidentally drops one variant.
    for ext in ["ini", "properties", "cfg"] {
        let path = format!("a.{ext}");
        let fmt = dq_core::detect(Utf8Path::new(&path))
            .unwrap_or_else(|| panic!("dq_core::detect must resolve `.{ext}` to INI"));
        assert_eq!(fmt.name(), "ini");
    }
}
