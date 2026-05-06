//! Component tests for the Markdown-frontmatter parser/writer.
//!
//! Stage 2's inline tests cover YAML detection + the no-frontmatter
//! fallback. These tests pin TOML / JSON detection, body byte preservation,
//! and the 64 KiB scan-limit fallback.

use dq_core::{Document, Format, FrontmatterKind, Value};
use pretty_assertions::assert_eq;

fn frontmatter() -> &'static dyn Format {
    dq_core::by_name("frontmatter").expect("frontmatter format must be registered")
}

fn parse_md(s: &str) -> Document {
    frontmatter()
        .parse(s.as_bytes())
        .unwrap_or_else(|e| panic!("frontmatter parse failed: {e}\n--input--\n{s}\n----------"))
}

#[test]
fn parse_yaml_frontmatter_extracts_value_and_body() {
    let doc = parse_md("---\ntitle: Hello\n---\n# Body\n");
    let Value::Map(m) = doc.value() else {
        panic!("expected Map header, got: {:?}", doc.value());
    };
    assert_eq!(
        m.get("title"),
        Some(&Value::String("Hello".into())),
        "YAML header value must round-trip into the value tree",
    );
    let payload = doc
        .frontmatter_payload()
        .expect("YAML-frontmatter document must carry a FrontmatterPayload");
    assert_eq!(
        payload.kind,
        FrontmatterKind::Yaml,
        "kind must be Yaml for `---` delimiter",
    );
    assert_eq!(
        payload.body, b"# Body\n",
        "body bytes must equal everything after the closing `---\\n`",
    );
}

#[test]
fn parse_toml_frontmatter_uses_toml_kind() {
    // `+++ … +++` is the Hugo / mkdocs convention for TOML frontmatter.
    // Pin both the value (so the inner TOML parser actually ran) and the
    // kind (so the writer dispatch picks the right inner format on output).
    let doc = parse_md("+++\ntitle = \"Hello\"\n+++\n# Body\n");
    let Value::Map(m) = doc.value() else { panic!() };
    assert_eq!(m.get("title"), Some(&Value::String("Hello".into())));
    let payload = doc.frontmatter_payload().expect("payload must exist");
    assert_eq!(
        payload.kind,
        FrontmatterKind::Toml,
        "kind must be Toml for `+++` delimiter",
    );
    assert_eq!(payload.body, b"# Body\n");
}

#[test]
fn parse_json_frontmatter_uses_json_kind() {
    // Format: a JSON object as the first construct, terminated by `}\n` and a
    // blank line, before the body. Less common than YAML/TOML but a real
    // Hugo flavour.
    let doc = parse_md("{\n  \"title\": \"Hello\"\n}\n\n# Body\n");
    let Value::Map(m) = doc.value() else { panic!() };
    assert_eq!(m.get("title"), Some(&Value::String("Hello".into())));
    let payload = doc.frontmatter_payload().expect("payload must exist");
    assert_eq!(payload.kind, FrontmatterKind::Json);
    assert_eq!(
        payload.body, b"# Body\n",
        "JSON-frontmatter body starts after the blank line, not the `}}`",
    );
}

#[test]
fn parse_no_frontmatter_returns_empty_map_and_full_body() {
    // No recognised opening delimiter → empty map header, body = whole input.
    // The writer's empty-map shortcut emits just the body so a no-frontmatter
    // file round-trips byte-identical.
    let input = b"# Just markdown\n\nNo header.\n";
    let doc = frontmatter().parse(input).expect("parse must succeed");
    let Value::Map(m) = doc.value() else {
        panic!("expected empty Map fallback, got: {:?}", doc.value());
    };
    assert!(
        m.is_empty(),
        "fallback header value must be the empty map, got: {m:?}",
    );
    let payload = doc.frontmatter_payload().expect("payload must exist");
    assert_eq!(
        payload.body, input,
        "body equals the entire file when no frontmatter is recognised",
    );
}

#[test]
fn round_trip_preserves_body_bytes_verbatim() {
    // The frontmatter contract is "header is canonical-formatted, body is
    // byte-identical". The writer re-emits the header through the inner
    // format's writer, then concatenates the stored body bytes verbatim.
    // After parsing the round-tripped output, the BODY bytes must equal
    // the original body bytes byte-for-byte.
    let source = "---\ntitle: Hello\n---\nFirst body line.\n\nSecond body paragraph.\n";
    let doc = parse_md(source);
    let original_body = doc
        .frontmatter_payload()
        .expect("payload must exist")
        .body
        .clone();
    let mut buf: Vec<u8> = Vec::new();
    frontmatter()
        .write(&doc, &mut buf)
        .expect("frontmatter write must succeed");
    let doc2 = frontmatter()
        .parse(&buf)
        .expect("written bytes must re-parse as frontmatter");
    let new_body = &doc2
        .frontmatter_payload()
        .expect("re-parsed doc must still carry a payload")
        .body;
    assert_eq!(
        new_body, &original_body,
        "body must be byte-identical through write→parse",
    );
}

#[test]
fn parse_unterminated_open_delim_falls_back_to_empty_header_and_full_body() {
    // Edge case from the spec: a file starts with `---` but never has a
    // matching `---` within the 64 KiB scan limit. The parser must fall back
    // to "no frontmatter, body = whole file" rather than erroring out.
    let input = b"---\ntitle: x\n# some markdown without a closing delimiter\n";
    let doc = frontmatter()
        .parse(input)
        .expect("unterminated frontmatter must NOT error — fallback applies");
    let Value::Map(m) = doc.value() else {
        panic!("expected fallback empty Map, got: {:?}", doc.value());
    };
    assert!(
        m.is_empty(),
        "unterminated frontmatter must yield empty header, got: {m:?}",
    );
    let payload = doc.frontmatter_payload().expect("payload must exist");
    assert_eq!(
        payload.body, input,
        "body must equal the whole file in the unterminated-fallback path",
    );
}
