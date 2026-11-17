//! Component tests for the HCL (Terraform) parser/writer.
//!
//! Mirror of `parse_yaml.rs` — go through the registry-resolved [`dq_core::Format`]
//! trait object so the test exercises the same surface end users hit through
//! `dq -F hcl …`. The Stage 2 inline `#[cfg(test)] mod tests` inside
//! `parsers/hcl.rs` covers minimal sanity (one parse, one block, one error).
//! These tests go deeper: extension detection, list/number scalars,
//! parse-error variant pinning, and round-trip equivalence (semantic, NOT
//! byte-identical — `hcl-rs` has no comment-preserving emitter).

use camino::Utf8Path;
use dq_core::{Document, Format, Value};
use pretty_assertions::assert_eq;

/// Resolve the HCL format through the registry so the test fails closed if
/// the format is ever de-registered.
fn hcl() -> &'static dyn Format {
    dq_core::by_name("hcl").expect("hcl format must be registered")
}

/// Sugar for "parse this string, panic with the parser message on failure".
fn parse_hcl(s: &str) -> Document {
    hcl()
        .parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("hcl parse failed: {e}\n---input---\n{s}\n-----------"))
}

#[test]
fn parse_top_level_attribute_produces_string_scalar() {
    // Per spec: `region = "us-east-1"` → Map { "region": String("us-east-1") }.
    let doc = parse_hcl(r#"region = "us-east-1""#);
    let Value::Map(top) = doc.value() else {
        panic!("expected top-level Map, got {:?}", doc.value());
    };
    assert_eq!(
        top.get("region"),
        Some(&Value::String("us-east-1".into())),
        "string attribute must round-trip without alteration",
    );
}

#[test]
fn parse_backend_block_with_label_nests_one_level_per_label() {
    // Spec scenario "HCL backend block parses to nested map":
    // `backend "s3" { bucket = "x" }` → `{ "backend": { "s3": { "bucket": "x" } } }`.
    let doc = parse_hcl(r#"backend "s3" { bucket = "x" }"#);
    let Value::Map(top) = doc.value() else {
        panic!("expected top-level map");
    };
    let Some(Value::Map(backend)) = top.get("backend") else {
        panic!("missing /backend, got: {top:?}");
    };
    let Some(Value::Map(s3)) = backend.get("s3") else {
        panic!("missing /backend/s3, got: {backend:?}");
    };
    assert_eq!(
        s3.get("bucket"),
        Some(&Value::String("x".into())),
        "label-as-key nesting must reach the leaf attribute",
    );
}

#[test]
fn parse_list_attribute_produces_array_of_strings() {
    // Lists are common in tfvars (`subnets = ["a", "b"]`). The parser must
    // produce a `Value::Array` of homogeneous `Value::String` cells.
    let doc = parse_hcl(r#"subnets = ["a", "b"]"#);
    let Value::Map(top) = doc.value() else {
        panic!("expected map");
    };
    let Some(Value::Array(items)) = top.get("subnets") else {
        panic!("missing /subnets array, got: {top:?}");
    };
    assert_eq!(
        items.len(),
        2,
        "expected 2 elements in /subnets, got {}",
        items.len()
    );
    assert_eq!(items[0], Value::String("a".into()));
    assert_eq!(items[1], Value::String("b".into()));
}

#[test]
fn parse_integer_attribute_produces_int_scalar() {
    // Numeric attributes that fit `i64` must surface as `Value::Int`, not as
    // a Float or BigInt placeholder. Pinning this prevents the `hcl-rs`
    // Number → Value mapping from regressing into the Float fallback.
    let doc = parse_hcl("replicas = 3");
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    assert_eq!(
        top.get("replicas"),
        Some(&Value::Int(3)),
        "small integer must use the i64 variant",
    );
}

#[test]
fn registry_detects_hcl_tf_and_tfvars_extensions() {
    // The dispatcher must recognise all three of HCL's extensions. A
    // regression that drops one would silently break Terraform users whose
    // files often differ only in extension.
    for ext in ["hcl", "tf", "tfvars"] {
        let path = format!("a.{ext}");
        let fmt = dq_core::detect(Utf8Path::new(&path)).unwrap_or_else(|| {
            panic!("dq_core::detect must resolve `.{ext}` to the HCL parser, got None")
        });
        assert_eq!(
            fmt.name(),
            "hcl",
            "extension `.{ext}` must resolve to the `hcl` format, got `{}`",
            fmt.name(),
        );
    }
}

#[test]
fn parse_invalid_syntax_returns_parse_error_variant() {
    // Malformed input must surface as `Error::Parse`, not as a panic, an Io
    // error, or an UnsupportedFormat. The exit-code mapper relies on this
    // variant to pick PARSE_ERROR (3).
    let err = hcl()
        .parse(b"this = =\n")
        .expect_err("malformed HCL must surface as a parse error");
    assert!(
        matches!(err, dq_core::Error::Parse { .. }),
        "expected Error::Parse, got {err:?}",
    );
    // The message must not be empty — the renderer needs *something* to
    // show users.
    assert!(
        !err.to_string().is_empty(),
        "parse-error message must not be empty",
    );
}

#[test]
fn round_trip_is_structurally_equal_after_parse_then_write_then_parse() {
    // `hcl-rs` has no comment/quote-preserving emitter, so a literal byte
    // round-trip is impossible. The contract is structural: parse → write →
    // parse must produce a `Value` equal to the first parse. Pinning this
    // catches regressions in `value_to_hcl_value` that would silently
    // round-trip values incorrectly (e.g. losing list cells).
    let source = r#"
region   = "us-east-1"
replicas = 3
subnets  = ["a", "b"]

backend "s3" {
  bucket = "tfstate"
}
"#;
    let doc1 = parse_hcl(source);
    let mut buf: Vec<u8> = Vec::new();
    hcl()
        .write(&doc1, &mut buf)
        .expect("hcl write must succeed for a parsed-then-clean tree");
    let rendered = String::from_utf8(buf).expect("hcl writer must produce utf-8");
    let doc2 = parse_hcl(&rendered);
    assert_eq!(
        doc1.value(),
        doc2.value(),
        "round-trip must preserve the value tree exactly",
    );
}
