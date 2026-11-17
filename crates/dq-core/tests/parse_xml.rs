//! Component tests for the XML (M11) parser/writer.
//!
//! Mirror of `parse_hcl.rs`: tests go through the registry-resolved
//! [`dq_core::Format`] trait object so they exercise the same surface end
//! users hit through `dq -F xml …`. The inline `#[cfg(test)] mod tests`
//! inside `parsers/xml.rs` already covers per-event sanity (parse, attrs,
//! cdata, comments, mixed-content, write rejections); these tests pin the
//! end-to-end contract: extension detection, scenario coverage from the
//! M11 spec, and round-trip semantic equivalence on the checked-in XML
//! fixtures.
//!
//! Sanity coverage only — comprehensive testing (golden snapshot fixtures,
//! property-based round-trip) is the responsibility of a follow-up
//! `rust-cli-test-writer` task.

use std::fs;
use std::path::PathBuf;

use camino::Utf8Path;
use dq_core::{Document, Format, FormatTag, Value};

/// Resolve the XML format through the registry so the test fails closed if
/// the format is ever de-registered.
fn xml() -> &'static dyn Format {
    dq_core::by_name("xml").expect("xml format must be registered")
}

/// Path to a fixture under `tests/fixtures/golden/xml/`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("xml")
        .join(name)
}

fn read_fixture(name: &str) -> Vec<u8> {
    fs::read(fixture(name))
        .unwrap_or_else(|e| panic!("fixture must exist: {} ({e})", fixture(name).display(),))
}

fn parse(bytes: &[u8]) -> Document {
    xml()
        .parse(bytes)
        .unwrap_or_else(|e| panic!("xml parse failed: {e}"))
}

#[test]
fn registry_detects_xml_extension() {
    // M11 contract: `pom.xml` (and any `.xml` file) resolves to the
    // registered `XmlFormat` parser by extension.
    let fmt = dq_core::detect(Utf8Path::new("pom.xml"))
        .expect("dq_core::detect must resolve `.xml` to the XML parser");
    assert_eq!(fmt.name(), "xml");
}

#[test]
fn from_name_xml_returns_xml_tag() {
    // M11 spec scenario `from_name maps "xml"`.
    assert_eq!(FormatTag::from_name("xml"), Some(FormatTag::Xml));
    assert_eq!(FormatTag::Xml.name(), "xml");
}

#[test]
fn parse_simple_user_element_carries_attribute_and_text() {
    // Spec scenario "Element with attribute and text round-trips".
    let bytes = read_fixture("simple.xml");
    let doc = parse(&bytes);
    let Value::Map(top) = doc.value() else {
        panic!("expected top map")
    };
    let Some(Value::Array(users)) = top.get("user") else {
        panic!("missing /user array")
    };
    assert_eq!(
        users.len(),
        1,
        "single root must wrap into a one-element array"
    );
    let Value::Map(user) = &users[0] else {
        panic!("expected user element map")
    };
    let Some(Value::Map(attrs)) = user.get("@attrs") else {
        panic!("expected @attrs on <user>")
    };
    assert_eq!(attrs.get("id"), Some(&Value::String("42".into())));
    assert_eq!(doc.format(), FormatTag::Xml);
}

#[test]
fn parse_multi_child_same_tag_preserves_order_via_pointer_indexing() {
    // Spec scenario "Multi-child same-tag preserves order": `/list/item`
    // is an Array of three elements addressable via /list/item/0, /1, /2.
    let bytes = read_fixture("multi_child.xml");
    let doc = parse(&bytes);
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    let Some(Value::Array(list_arr)) = top.get("list") else {
        panic!("missing /list array")
    };
    let Value::Map(list) = &list_arr[0] else {
        panic!()
    };
    let Some(Value::Array(items)) = list.get("item") else {
        panic!("missing /list/item array")
    };
    assert_eq!(items.len(), 3, "three same-tag siblings");
    let extract_text = |idx: usize| -> String {
        let Value::Map(m) = &items[idx] else { panic!() };
        match m.get("#text") {
            Some(Value::String(s)) => s.clone(),
            _ => panic!("missing #text on item[{idx}]"),
        }
    };
    assert_eq!(extract_text(0), "A");
    assert_eq!(extract_text(1), "B");
    assert_eq!(extract_text(2), "C");
}

#[test]
fn parse_xml_declaration_round_trips_through_writer() {
    // Spec scenario "XML declaration preserved".
    let bytes = read_fixture("with_decl.xml");
    let doc = parse(&bytes);
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    let Some(Value::Map(decl)) = top.get("#xml") else {
        panic!("missing #xml top-level decl")
    };
    assert_eq!(decl.get("version"), Some(&Value::String("1.0".into())));
    assert_eq!(decl.get("encoding"), Some(&Value::String("UTF-8".into())));

    let mut buf: Vec<u8> = Vec::new();
    xml().write(&doc, &mut buf).expect("write must succeed");
    let out = String::from_utf8(buf).expect("utf-8 output");
    assert!(
        out.starts_with("<?xml")
            && out.contains("version=\"1.0\"")
            && out.contains("encoding=\"UTF-8\""),
        "rendered output must begin with an equivalent declaration; got: {out:?}",
    );
}

#[test]
fn parse_comment_is_attached_to_parent_and_round_trips() {
    // Spec scenario "Comments preserved on round-trip".
    let bytes = read_fixture("with_comment.xml");
    let doc = parse(&bytes);

    let mut buf: Vec<u8> = Vec::new();
    xml().write(&doc, &mut buf).expect("write must succeed");
    let out = String::from_utf8(buf).expect("utf-8");
    assert!(
        out.contains("<!-- top note -->"),
        "comment must round-trip verbatim; got: {out:?}",
    );
}

#[test]
fn parse_cdata_is_preserved_byte_identical_inside_block() {
    // Spec scenario "CDATA preserved on round-trip".
    let bytes = read_fixture("with_cdata.xml");
    let doc = parse(&bytes);
    let mut buf: Vec<u8> = Vec::new();
    xml().write(&doc, &mut buf).expect("write must succeed");
    let out = String::from_utf8(buf).expect("utf-8");
    assert!(
        out.contains("<![CDATA[if (a < b) {}]]>"),
        "CDATA block bytes must round-trip identically; got: {out:?}",
    );
}

#[test]
fn parse_namespace_prefix_and_xmlns_attr_retained_verbatim() {
    // Spec contract: namespace prefixes survive in tag names and as
    // `xmlns:foo` attribute keys.
    let bytes = read_fixture("namespaces.xml");
    let doc = parse(&bytes);
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    assert!(
        top.contains_key("svg:rect"),
        "tag prefix `svg:` must be retained verbatim",
    );
    let Some(Value::Array(rects)) = top.get("svg:rect") else {
        panic!()
    };
    let Value::Map(rect) = &rects[0] else {
        panic!()
    };
    let Some(Value::Map(attrs)) = rect.get("@attrs") else {
        panic!()
    };
    assert!(
        attrs.contains_key("xmlns:svg"),
        "xmlns:svg attr must be retained verbatim",
    );
}

#[test]
fn parse_mixed_content_succeeds_with_text_folded_into_text_key() {
    // Spec scenario "Mixed content emits warning": parse succeeds; we don't
    // capture the tracing log here without setting up a subscriber, but pin
    // the *behaviour* — the body is folded into `#text` and the inner
    // element is still recorded.
    let bytes = read_fixture("mixed_content.xml");
    let doc = parse(&bytes);
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    let Some(Value::Array(ps)) = top.get("p") else {
        panic!()
    };
    let Value::Map(p) = &ps[0] else { panic!() };
    let Some(Value::String(text)) = p.get("#text") else {
        panic!("expected #text on <p> for mixed content")
    };
    assert!(
        text.contains("Hello") && text.contains("!"),
        "mixed-content text must be retained; got: {text:?}",
    );
    assert!(
        p.contains_key("b"),
        "inner <b> element must still be recorded"
    );
}

#[test]
fn pom_xml_get_project_version_returns_expected_string() {
    // Spec scenario "Auto-detection by extension" — covers the typical
    // Maven `pom.xml` use case at a value-tree level. The CLI integration
    // test (`unit_format_extensions::xml_*`) covers the `dq get` path.
    let bytes = read_fixture("pom.xml");
    let doc = parse(&bytes);
    let Value::Map(top) = doc.value() else {
        panic!()
    };
    let Some(Value::Array(projects)) = top.get("project") else {
        panic!("missing /project array")
    };
    let Value::Map(project) = &projects[0] else {
        panic!()
    };
    let Some(Value::Array(versions)) = project.get("version") else {
        panic!("missing /project/version array")
    };
    let Value::Map(version) = &versions[0] else {
        panic!()
    };
    assert_eq!(
        version.get("#text"),
        Some(&Value::String("1.2.3".into())),
        "/project/version/0/#text must equal '1.2.3'",
    );
}

#[test]
fn round_trip_is_structurally_equal_after_parse_then_write_then_parse() {
    // Round-trip is partial (whitespace between elements normalised) but
    // structural equality must hold for the supported features.
    for fixture_name in [
        "simple.xml",
        "multi_child.xml",
        "with_decl.xml",
        "with_comment.xml",
        "with_cdata.xml",
        "namespaces.xml",
        "pom.xml",
    ] {
        let bytes = read_fixture(fixture_name);
        let doc1 = parse(&bytes);
        let mut buf: Vec<u8> = Vec::new();
        xml()
            .write(&doc1, &mut buf)
            .unwrap_or_else(|e| panic!("write failed for {fixture_name}: {e}"));
        let doc2 = parse(&buf);
        assert_eq!(
            doc1.value(),
            doc2.value(),
            "round-trip must preserve the value tree exactly for {fixture_name}",
        );
    }
}

#[test]
fn parse_invalid_xml_returns_parse_error_variant() {
    let err = xml()
        .parse(b"<root><a></b></root>")
        .expect_err("malformed XML must surface as a parse error");
    assert!(
        matches!(err, dq_core::Error::Parse { .. }),
        "expected Error::Parse, got {err:?}",
    );
    assert_eq!(err.kind_name(), "parse");
}

#[test]
fn parse_rejects_multiple_root_elements() {
    // Well-formed XML requires exactly one root element. Multi-root inputs
    // like `<a/><b/>` would otherwise silently lose the second root through
    // the writer (which only emits the first non-conventional key).
    let err = xml()
        .parse(b"<a/><b/>")
        .expect_err("multi-root XML must surface as a parse error");
    let dq_core::Error::Parse { message, .. } = &err else {
        panic!("expected Error::Parse, got {err:?}");
    };
    assert!(
        message.contains("exactly one root element"),
        "expected message about exactly one root element, got: {message}",
    );
}

#[test]
fn parse_rejects_zero_root_elements() {
    // Empty top-level (only PI/comments, no element) must also fail with
    // the same "exactly one root element" parse error.
    let err = xml()
        .parse(b"<!-- comment only --><?pi?>")
        .expect_err("zero-root XML must surface as a parse error");
    let dq_core::Error::Parse { message, .. } = &err else {
        panic!("expected Error::Parse, got {err:?}");
    };
    assert!(
        message.contains("exactly one root element"),
        "expected message about exactly one root element, got: {message}",
    );
}

#[test]
fn write_top_level_non_map_is_format_error() {
    // The XML writer cannot serialise a non-map root — this surfaces as
    // `Error::Format { format: "xml", ... }`, not a panic.
    let doc = Document::value_only(Value::String("not a doc".into()), FormatTag::Xml);
    let mut buf: Vec<u8> = Vec::new();
    let err = xml()
        .write(&doc, &mut buf)
        .expect_err("must reject non-map");
    assert!(
        matches!(err, dq_core::Error::Format { format: "xml", .. }),
        "expected Error::Format with format=xml, got {err:?}",
    );
}
