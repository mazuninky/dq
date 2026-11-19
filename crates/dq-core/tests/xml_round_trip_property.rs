//! Property: a well-formed XML document read by dq writes back as XML that
//! reads to the same tree, and writing that tree again gives the same
//! bytes — read → write → read is stable, write is a fixpoint after one
//! pass.
//!
//! The generator builds documents in the shape dq's reader produces from
//! real files: elements holding either text, a CDATA section or child
//! elements (no mixed content, which the reader flattens with a warning),
//! with attributes, comments and processing instructions, prefixed names
//! under a namespace the root declares, and an optional declaration. Text
//! and attribute values carry what the writer must escape: `<`, `&`,
//! quotes, tab, LF and CR (as character references, the way a file holds
//! them), and non-ASCII characters; text that is all whitespace is held as
//! character references, every character of it.

use dq_core::{Document, Format};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

fn xml() -> &'static dyn Format {
    dq_core::by_name("xml").expect("xml format must be registered")
}

/// What an element holds besides attributes, comments and PIs.
#[derive(Debug, Clone)]
enum Content {
    Empty,
    Text(String),
    Cdata(String),
    Children(Vec<Element>),
}

#[derive(Debug, Clone)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    comments: Vec<String>,
    pis: Vec<(String, String)>,
    content: Content,
}

/// An NCName that is never `xml…` / `xmlns` (no leading `x`).
fn ncname() -> impl Strategy<Value = String> {
    "[a-wA-W_][a-z0-9._-]{0,5}"
}

/// An element name, unprefixed or under the root's `p` namespace.
fn element_name() -> impl Strategy<Value = String> {
    (any::<bool>(), ncname()).prop_map(|(prefixed, local)| {
        if prefixed {
            format!("p:{local}")
        } else {
            local
        }
    })
}

/// Character data with every character the writer escapes or re-encodes.
fn char_data() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z0-9 <>&\"'\t\n\r#;=é☃\u{1F600}\u{85}\u{2028}]{0,10}",
        // All whitespace, which a file can hold as text only in references.
        "[ \t\n\r\u{85}\u{2028}]{1,4}",
    ]
}

fn attributes() -> impl Strategy<Value = Vec<(String, String)>> {
    proptest::collection::btree_map(ncname(), char_data(), 0..3)
        .prop_map(|map| map.into_iter().collect())
}

/// Comment text: no `--`, not ending in `-` (no `-` at all keeps it simple).
fn comments() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec("[a-z <>&é]{0,8}", 0..2)
}

/// A PI target (never `xml`, no colon) and data without `?>`.
fn pis() -> impl Strategy<Value = Vec<(String, String)>> {
    proptest::collection::vec((ncname(), "[a-z][a-z =\"]{0,6}"), 0..2)
}

fn leaf_content() -> impl Strategy<Value = Content> {
    prop_oneof![
        Just(Content::Empty),
        char_data().prop_map(Content::Text),
        // `]]>` inside CDATA is split by the writer; a file cannot hold it
        // in one section, so the generator does not either.
        "[a-z <>&é\\]]{0,8}"
            .prop_filter("no ]]> in one section", |s| !s.contains("]]>"))
            .prop_map(Content::Cdata),
    ]
}

fn element() -> impl Strategy<Value = Element> {
    let leaf = (
        element_name(),
        attributes(),
        comments(),
        pis(),
        leaf_content(),
    )
        .prop_map(|(name, attributes, comments, pis, content)| Element {
            name,
            attributes,
            comments,
            pis,
            content,
        });
    leaf.prop_recursive(3, 12, 3, |inner| {
        (
            element_name(),
            attributes(),
            comments(),
            pis(),
            proptest::collection::vec(inner, 1..4),
        )
            .prop_map(|(name, attributes, comments, pis, children)| Element {
                name,
                attributes,
                comments,
                pis,
                content: Content::Children(children),
            })
    })
}

/// `s` escaped for text (`in_attribute` false) or a double-quoted attribute
/// value, with CR — and tab and LF in an attribute — as character
/// references, the way a file keeps them through XML's normalisation.
fn escape(s: &str, in_attribute: bool) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '&' => out.push_str("&amp;"),
            '>' => out.push_str("&gt;"),
            '"' if in_attribute => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            '\t' if in_attribute => out.push_str("&#9;"),
            '\n' if in_attribute => out.push_str("&#10;"),
            c => out.push(c),
        }
    }
    out
}

fn render(element: &Element, root: bool, out: &mut String) {
    out.push('<');
    out.push_str(&element.name);
    if root {
        out.push_str(" xmlns:p=\"urn:p\"");
    }
    for (name, value) in &element.attributes {
        out.push_str(&format!(" {name}=\"{}\"", escape(value, true)));
    }
    out.push('>');
    for comment in &element.comments {
        out.push_str(&format!("<!--{comment}-->"));
    }
    for (target, data) in &element.pis {
        out.push_str(&format!("<?{target} {data}?>"));
    }
    match &element.content {
        Content::Empty => {}
        // Written as it is, whitespace-only text is indentation, which the
        // reader drops; a file holds such text as character references.
        Content::Text(text) if text.trim().is_empty() => {
            for c in text.chars() {
                out.push_str(&format!("&#{};", u32::from(c)));
            }
        }
        Content::Text(text) => out.push_str(&escape(text, false)),
        Content::Cdata(text) => out.push_str(&format!("<![CDATA[{text}]]>")),
        Content::Children(children) => {
            for child in children {
                render(child, false, out);
            }
        }
    }
    out.push_str(&format!("</{}>", element.name));
}

fn document() -> impl Strategy<Value = String> {
    (any::<bool>(), comments(), element()).prop_map(|(declared, comments, root)| {
        let mut out = String::new();
        if declared {
            out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        }
        for comment in &comments {
            out.push_str(&format!("<!--{comment}-->"));
        }
        render(&root, true, &mut out);
        out
    })
}

fn write(doc: &Document) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    xml()
        .write(doc, &mut out)
        .map_err(|e| format!("write: {e}"))?;
    Ok(out)
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Documents are small (depth 3, a dozen elements); 128 cases run
        // in well under a second.
        cases: 128,
        ..ProptestConfig::default()
    })]

    #[test]
    fn read_write_read_is_stable(source in document()) {
        let first = xml()
            .parse(source.as_bytes())
            .map_err(|e| TestCaseError::fail(format!("generated XML must read: {e}")))?;
        let written = write(&first).map_err(TestCaseError::fail)?;
        let second = xml().parse(&written).map_err(|e| {
            TestCaseError::fail(format!(
                "dq's own output must read: {e}\n{}",
                String::from_utf8_lossy(&written)
            ))
        })?;
        prop_assert_eq!(
            first.value(),
            second.value(),
            "the tree changed through a write:\n{}",
            String::from_utf8_lossy(&written)
        );
        let rewritten = write(&second).map_err(TestCaseError::fail)?;
        prop_assert_eq!(
            String::from_utf8_lossy(&written),
            String::from_utf8_lossy(&rewritten),
            "a second write differs"
        );
    }
}
