//! M9 Markdown parser — CommonMark + GFM AST as a typed-discriminator-Map tree.
//!
//! See `openspec/changes/add-markdown-tree-format/{proposal,design,specs}.md`
//! for the full contract. The short version:
//!
//! - Parsing produces a `Document` whose `value()` is a top-level
//!   `Value::Map { "type": "document", "frontmatter": …, "children": …,
//!   "position": … }`.
//! - Each AST node is a `Value::Map` carrying a `"type"` discriminator string
//!   and node-specific fields (`heading.level`, `code_block.lang`, …). Children
//!   live under `"children"` as `Value::Array<Map>`.
//! - Frontmatter (`---…---`, `+++…+++`, or `{…}` + blank line) is folded into
//!   the document node's `"frontmatter"` field as
//!   `Map { "kind": "yaml"|"toml"|"json", "value": <parsed-header-value> }`.
//!   Detection runs BEFORE comrak (we own the YAML/TOML/JSON branch); the
//!   delimiter-scanner is shared with the M5 [`super::frontmatter`] parser
//!   via the `pub(crate)` helpers.
//! - GFM extensions enabled unconditionally: tables, strikethrough, autolink,
//!   tasklist, footnotes, header-id prefix.
//! - Block-level position tracking only (`render.sourcepos = true`). Inline
//!   nodes do NOT carry `position` per the M9 spec — that's deferred to a
//!   future milestone.
//!
//! ## Round-trip contract (M9 read-only-on-mutation)
//!
//! - `write()` for an unmutated document emits `original_bytes()` verbatim.
//!   "Unmutated" is detected by re-parsing the original bytes and comparing
//!   the resulting `Value` for structural equality with `doc.value()`.
//! - `write()` for a mutated document returns `Error::Format` — textual-edit
//!   splicing for markdown is M11+ work. The error message names this as
//!   the M9 contract so callers can decide whether to bail or to ignore.
//!
//! See design D6 (round-trip strategy) for the rationale: M9 markdown is
//! primarily a lint-target format, and a "canonical" emission via
//! `comrak::format_commonmark` would silently strip trailing whitespace,
//! flip fence characters, and reorder reference-link definitions.

use std::io::Write;

use camino::Utf8PathBuf;
use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::{Arena, Options, parse_document};
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;
use crate::parsers::frontmatter::{
    detect_json_frontmatter, detect_toml_frontmatter, detect_yaml_frontmatter,
};
use crate::parsers::{Json, Toml, Yaml};

/// Markdown format implementation.
///
/// Registered in [`super::registry`] BEFORE [`super::Frontmatter`] so default
/// extension dispatch for `.md` / `.markdown` resolves here. The M5
/// `Frontmatter` parser remains reachable via `-F frontmatter`.
#[derive(Debug, Clone, Copy)]
pub struct Markdown;

impl Format for Markdown {
    fn name(&self) -> &'static str {
        "markdown"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["md", "markdown"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let original = bytes.to_vec();
        let value = parse_to_value(bytes)?;
        Ok(Document::with_value_and_bytes(
            value,
            original,
            FormatTag::Markdown,
        ))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        // Verbatim path: the source bytes are preserved when the in-memory
        // value tree still matches what the parser produced from those bytes.
        let original = doc.original_bytes();
        if original.is_empty() {
            // No baseline to compare against — accept only when the value
            // tree is the canonical empty document. Otherwise we'd silently
            // emit nothing on a synthesised non-empty doc.
            if doc.value() == &empty_document_value() {
                return Ok(());
            }
            return Err(Error::Format {
                format: "markdown",
                message: "Markdown document has no source bytes; mutation re-emit is M11+ work \
                     (the in-memory value tree is read-only in M9)"
                    .to_owned(),
            });
        }
        match parse_to_value(original) {
            Ok(baseline) if &baseline == doc.value() => {
                w.write_all(original).map_err(|source| Error::WriteIo {
                    path: Utf8PathBuf::from("<markdown-writer>"),
                    source,
                })?;
                Ok(())
            }
            Ok(_) => Err(Error::Format {
                format: "markdown",
                message: "Markdown round-trip on mutated documents is M11+ work; the value tree \
                     is read-only in M9"
                    .to_owned(),
            }),
            Err(e) => Err(e),
        }
    }
}

/// Top-level `Value` shape returned by [`Markdown::parse`] for an empty
/// input. Used as the only acceptable value when `original_bytes` is empty
/// (the synthetic-doc edge case).
fn empty_document_value() -> Value {
    let mut doc_map = IndexMap::new();
    doc_map.insert("type".to_owned(), Value::String("document".to_owned()));
    doc_map.insert("frontmatter".to_owned(), Value::Null);
    doc_map.insert("children".to_owned(), Value::Array(Vec::new()));
    doc_map.insert("position".to_owned(), null_position());
    Value::Map(doc_map)
}

/// Parse `bytes` into the top-level `Value` shape (without wrapping it in a
/// `Document`). Used by both the parse path and the round-trip baseline
/// computation in [`Markdown::write`].
fn parse_to_value(bytes: &[u8]) -> Result<Value> {
    // 1. Frontmatter detection — pre-comrak, so we can route TOML / JSON
    //    headers through the existing parsers (comrak only natively supports
    //    YAML-style `---` delimiters).
    let (frontmatter_value, body_bytes) = detect_and_parse_frontmatter(bytes)?;

    // 2. Body via comrak.
    let arena = Arena::new();
    let opts = build_options();
    let body_str = std::str::from_utf8(body_bytes).map_err(|e| Error::Parse {
        file: None,
        line: 0,
        col: 0,
        span: 0..0,
        snippet: String::new(),
        message: format!("invalid UTF-8 in Markdown input: {e}"),
    })?;
    let root = parse_document(&arena, body_str, &opts);

    // 3. Walk the AST. The comrak top-level node is always `NodeValue::Document`;
    //    we want a `{ "type": "document", "frontmatter": …, "children": [...] }`
    //    shape so we synthesize the document map directly rather than letting
    //    `node_to_value` produce a generic shape for the root.
    let children = root.children().map(node_to_value).collect::<Vec<Value>>();

    let mut doc_map = IndexMap::new();
    doc_map.insert("type".to_owned(), Value::String("document".to_owned()));
    doc_map.insert("frontmatter".to_owned(), frontmatter_value);
    doc_map.insert("children".to_owned(), Value::Array(children));
    let pos = root.data.borrow().sourcepos;
    doc_map.insert("position".to_owned(), sourcepos_to_value(pos));

    Ok(Value::Map(doc_map))
}

/// Build the `comrak::Options` configuration shared by every parse.
fn build_options() -> Options<'static> {
    let mut opts = Options::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    opts.extension.footnotes = true;
    // Header IDs are required by some downstream renderers; the prefix is
    // empty so headings just gain stable `id` attributes when comrak emits
    // HTML elsewhere. The presence of this option does NOT add anything to
    // the AST shape we encode.
    opts.extension.header_id_prefix = Some(String::new());
    // Block-level position tracking. M9 contract pins this to always-on so
    // rules can address `.position.start.line` without negotiation.
    opts.render.sourcepos = true;
    opts
}

/// Detect frontmatter at the start of `bytes` and return `(frontmatter_value,
/// body_bytes)`. The frontmatter value is `Value::Null` when no recognised
/// delimiter is found (per spec).
fn detect_and_parse_frontmatter(bytes: &[u8]) -> Result<(Value, &[u8])> {
    if let Some((header_bytes, body_bytes)) = detect_yaml_frontmatter(bytes) {
        let header_doc = Yaml.parse(header_bytes)?;
        let mut wrap = IndexMap::new();
        wrap.insert("kind".to_owned(), Value::String("yaml".to_owned()));
        wrap.insert("value".to_owned(), header_doc.value().clone());
        return Ok((Value::Map(wrap), body_bytes));
    }
    if let Some((header_bytes, body_bytes)) = detect_toml_frontmatter(bytes) {
        let header_doc = Toml.parse(header_bytes)?;
        let mut wrap = IndexMap::new();
        wrap.insert("kind".to_owned(), Value::String("toml".to_owned()));
        wrap.insert("value".to_owned(), header_doc.value().clone());
        return Ok((Value::Map(wrap), body_bytes));
    }
    if let Some((header_bytes, body_bytes)) = detect_json_frontmatter(bytes) {
        let header_doc = Json.parse(header_bytes)?;
        let mut wrap = IndexMap::new();
        wrap.insert("kind".to_owned(), Value::String("json".to_owned()));
        wrap.insert("value".to_owned(), header_doc.value().clone());
        return Ok((Value::Map(wrap), body_bytes));
    }
    Ok((Value::Null, bytes))
}

/// Convert a comrak AST node to the typed-discriminator `Value::Map` shape.
fn node_to_value<'a>(node: &'a AstNode<'a>) -> Value {
    let data = node.data.borrow();
    let pos = data.sourcepos;
    // Each branch produces (type-discriminator, IndexMap of node-specific
    // fields, include_position_field). Inline nodes set
    // include_position=false per the M9 spec.
    let (type_name, mut fields, include_position) = node_fields(&data.value, node);

    fields.shift_insert(0, "type".to_owned(), Value::String(type_name.to_owned()));
    if include_position {
        fields.insert("position".to_owned(), sourcepos_to_value(pos));
    }
    Value::Map(fields)
}

/// Dispatch a comrak `NodeValue` to its (`type`, fields, include_position)
/// triple. Falls back to a `{"type":"unknown","value":"<debug>"}` shape with a
/// `tracing::warn!` for any variant we haven't explicitly mapped, so the
/// coverage gap surfaces in tests / production logs rather than silently
/// dropping content.
fn node_fields<'a>(
    node_value: &NodeValue,
    node: &'a AstNode<'a>,
) -> (&'static str, IndexMap<String, Value>, bool) {
    let mut fields = IndexMap::new();
    match node_value {
        NodeValue::Document => {
            // The walker handles the document root separately. If we ever
            // recurse into a nested Document (shouldn't happen) keep going.
            fields.insert("children".to_owned(), children_of(node));
            ("document", fields, true)
        }
        NodeValue::Heading(h) => {
            fields.insert("level".to_owned(), Value::Int(i64::from(h.level)));
            fields.insert("children".to_owned(), children_of(node));
            ("heading", fields, true)
        }
        NodeValue::Paragraph => {
            fields.insert("children".to_owned(), children_of(node));
            ("paragraph", fields, true)
        }
        NodeValue::CodeBlock(cb) => {
            fields.insert("fenced".to_owned(), Value::Bool(cb.fenced));
            // First whitespace-separated token of the info string, or null
            // when empty. CommonMark §4.5: "The first word of the info string
            // is typically used to specify the language of the code sample".
            let lang = first_whitespace_token(&cb.info);
            fields.insert("lang".to_owned(), lang.map_or(Value::Null, Value::String));
            fields.insert("info".to_owned(), Value::String(cb.info.clone()));
            fields.insert("value".to_owned(), Value::String(cb.literal.clone()));
            ("code_block", fields, true)
        }
        NodeValue::BlockQuote => {
            fields.insert("children".to_owned(), children_of(node));
            ("block_quote", fields, true)
        }
        NodeValue::List(list) => {
            let ordered = matches!(list.list_type, comrak::nodes::ListType::Ordered);
            fields.insert("ordered".to_owned(), Value::Bool(ordered));
            fields.insert(
                "start".to_owned(),
                if ordered {
                    Value::Int(list.start as i64)
                } else {
                    Value::Null
                },
            );
            fields.insert("tight".to_owned(), Value::Bool(list.tight));
            fields.insert("children".to_owned(), children_of(node));
            ("list", fields, true)
        }
        NodeValue::Item(_) => {
            // Plain (non-task) list item — `checked: null`.
            fields.insert("checked".to_owned(), Value::Null);
            fields.insert("children".to_owned(), children_of(node));
            ("list_item", fields, true)
        }
        NodeValue::TaskItem(t) => {
            // GFM task list item — `checked: bool`. comrak reports the
            // bracket symbol as `Some(_)` for checked, `None` for unchecked.
            fields.insert("checked".to_owned(), Value::Bool(t.symbol.is_some()));
            fields.insert("children".to_owned(), children_of(node));
            ("list_item", fields, true)
        }
        NodeValue::ThematicBreak => ("thematic_break", fields, true),
        NodeValue::HtmlBlock(h) => {
            fields.insert("value".to_owned(), Value::String(h.literal.clone()));
            ("html_block", fields, true)
        }
        NodeValue::Table(_) => {
            fields.insert("children".to_owned(), children_of(node));
            ("table", fields, true)
        }
        NodeValue::TableRow(is_header) => {
            fields.insert("header".to_owned(), Value::Bool(*is_header));
            fields.insert("children".to_owned(), children_of(node));
            ("table_row", fields, true)
        }
        NodeValue::TableCell => {
            fields.insert("children".to_owned(), children_of(node));
            ("table_cell", fields, true)
        }
        NodeValue::FootnoteDefinition(d) => {
            fields.insert("name".to_owned(), Value::String(d.name.clone()));
            fields.insert("children".to_owned(), children_of(node));
            ("footnote_definition", fields, true)
        }
        // Inline nodes — no `position` field per spec.
        NodeValue::Text(t) => {
            fields.insert("value".to_owned(), Value::String(t.to_string()));
            ("text", fields, false)
        }
        NodeValue::SoftBreak => ("soft_break", fields, false),
        NodeValue::LineBreak => ("line_break", fields, false),
        NodeValue::Code(c) => {
            fields.insert("value".to_owned(), Value::String(c.literal.clone()));
            ("code", fields, false)
        }
        NodeValue::HtmlInline(s) => {
            fields.insert("value".to_owned(), Value::String(s.clone()));
            ("html_inline", fields, false)
        }
        NodeValue::Emph => {
            fields.insert("children".to_owned(), children_of(node));
            ("emphasis", fields, false)
        }
        NodeValue::Strong => {
            fields.insert("children".to_owned(), children_of(node));
            ("strong", fields, false)
        }
        NodeValue::Strikethrough => {
            fields.insert("children".to_owned(), children_of(node));
            ("strikethrough", fields, false)
        }
        NodeValue::Link(l) => {
            fields.insert("url".to_owned(), Value::String(l.url.clone()));
            fields.insert(
                "title".to_owned(),
                if l.title.is_empty() {
                    Value::Null
                } else {
                    Value::String(l.title.clone())
                },
            );
            fields.insert("children".to_owned(), children_of(node));
            ("link", fields, false)
        }
        NodeValue::Image(l) => {
            fields.insert("url".to_owned(), Value::String(l.url.clone()));
            // Per spec, the image node carries an `alt` (concatenated text
            // descendants) instead of a `children` field.
            fields.insert("alt".to_owned(), Value::String(collect_text(node)));
            fields.insert(
                "title".to_owned(),
                if l.title.is_empty() {
                    Value::Null
                } else {
                    Value::String(l.title.clone())
                },
            );
            ("image", fields, false)
        }
        NodeValue::FootnoteReference(r) => {
            fields.insert("name".to_owned(), Value::String(r.name.clone()));
            ("footnote_reference", fields, false)
        }
        // Comrak emits a `FrontMatter` node when its own front-matter
        // option is enabled. We strip frontmatter pre-comrak, so this branch
        // should be unreachable; if it does fire, treat it as an html_block
        // fallback rather than a hard error.
        NodeValue::FrontMatter(s) => {
            fields.insert("value".to_owned(), Value::String(s.clone()));
            ("html_block", fields, true)
        }
        // Anything else — unmapped variant. Surface the gap.
        other => {
            tracing::warn!(
                "M9 markdown parser: unmapped comrak NodeValue variant {:?}; emitting `unknown` placeholder",
                other,
            );
            fields.insert("value".to_owned(), Value::String(format!("{other:?}")));
            ("unknown", fields, true)
        }
    }
}

/// Collect every block-level child of `node` into a `Value::Array`.
fn children_of<'a>(node: &'a AstNode<'a>) -> Value {
    Value::Array(node.children().map(node_to_value).collect())
}

/// Concatenate the `value` field of every `Text` descendant of `node`.
/// Used by the image AST node so `alt` carries the rendered alt text.
fn collect_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    for child in node.descendants() {
        let data = child.data.borrow();
        if let NodeValue::Text(t) = &data.value {
            out.push_str(t);
        }
    }
    out
}

/// First whitespace-separated token of `s`. CommonMark info-string lang
/// extraction. Returns `None` for an all-whitespace input.
fn first_whitespace_token(s: &str) -> Option<String> {
    let trimmed = s.trim_start();
    let token = trimmed.split_whitespace().next()?;
    Some(token.to_owned())
}

/// Convert a comrak `Sourcepos` into the spec'd `position` map.
fn sourcepos_to_value(pos: Sourcepos) -> Value {
    let mut start = IndexMap::new();
    start.insert("line".to_owned(), Value::Int(pos.start.line as i64));
    start.insert("column".to_owned(), Value::Int(pos.start.column as i64));
    let mut end = IndexMap::new();
    end.insert("line".to_owned(), Value::Int(pos.end.line as i64));
    end.insert("column".to_owned(), Value::Int(pos.end.column as i64));
    let mut out = IndexMap::new();
    out.insert("start".to_owned(), Value::Map(start));
    out.insert("end".to_owned(), Value::Map(end));
    Value::Map(out)
}

/// Position value for a synthetic empty document — `{1,1}..{0,0}`. Used only
/// by the `empty_document_value` baseline that compares structurally against
/// `parse_to_value(b"")`.
fn null_position() -> Value {
    let mut start = IndexMap::new();
    start.insert("line".to_owned(), Value::Int(1));
    start.insert("column".to_owned(), Value::Int(1));
    let mut end = IndexMap::new();
    end.insert("line".to_owned(), Value::Int(0));
    end.insert("column".to_owned(), Value::Int(0));
    let mut out = IndexMap::new();
    out.insert("start".to_owned(), Value::Map(start));
    out.insert("end".to_owned(), Value::Map(end));
    Value::Map(out)
}

// `Document::with_value_and_bytes` is a small extension of the existing
// `Document::with_spans` constructor: same shape, no spans. We define it
// inline as a trait extension so we don't need to touch document/mod.rs for
// every new parser. The compiler inlines the extension call.

trait DocumentExt {
    fn with_value_and_bytes(value: Value, bytes: Vec<u8>, format: FormatTag) -> Self;
}

impl DocumentExt for Document {
    fn with_value_and_bytes(value: Value, bytes: Vec<u8>, format: FormatTag) -> Self {
        Document::with_spans(value, bytes, crate::document::SpanMap::new(), format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Helper: borrow the `children` array of the top-level document map.
    fn doc_children(doc: &Document) -> &[Value] {
        let Value::Map(m) = doc.value() else {
            panic!("expected document map, got {:?}", doc.value());
        };
        let Value::Array(items) = m.get("children").expect("children present") else {
            panic!("children should be an array");
        };
        items
    }

    /// Helper: borrow a `type` discriminator string from a node map.
    fn node_type(v: &Value) -> &str {
        let Value::Map(m) = v else {
            panic!("expected node map");
        };
        let Value::String(s) = m.get("type").expect("type present") else {
            panic!("type should be a string");
        };
        s
    }

    /// Helper: lookup a field on a node map.
    fn field<'a>(v: &'a Value, key: &str) -> &'a Value {
        let Value::Map(m) = v else {
            panic!("expected node map");
        };
        m.get(key).unwrap_or_else(|| panic!("field {key} missing"))
    }

    #[test]
    fn parse_empty_input_produces_document_with_no_children() {
        let doc = Markdown.parse(b"").expect("parse empty");
        let Value::Map(m) = doc.value() else {
            panic!("expected document map");
        };
        assert_eq!(m.get("type"), Some(&Value::String("document".into())));
        assert_eq!(m.get("frontmatter"), Some(&Value::Null));
        let Value::Array(children) = m.get("children").unwrap() else {
            panic!("children")
        };
        assert!(children.is_empty(), "empty input → no children");
    }

    #[test]
    fn parse_single_h1_emits_heading_node_with_level_1() {
        let doc = Markdown.parse(b"# Hello\n").expect("parse");
        let children = doc_children(&doc);
        assert_eq!(children.len(), 1, "one block child");
        let h = &children[0];
        assert_eq!(node_type(h), "heading");
        assert_eq!(field(h, "level"), &Value::Int(1));
    }

    #[test]
    fn parse_all_six_heading_levels_each_carries_correct_level() {
        let src = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let children = doc_children(&doc);
        assert_eq!(children.len(), 6);
        for (i, child) in children.iter().enumerate() {
            assert_eq!(node_type(child), "heading");
            assert_eq!(field(child, "level"), &Value::Int(i as i64 + 1));
        }
    }

    #[test]
    fn parse_fenced_code_block_with_lang_exposes_lang_and_value() {
        let src = "```yaml\nfoo: bar\n```\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let children = doc_children(&doc);
        assert_eq!(children.len(), 1);
        let cb = &children[0];
        assert_eq!(node_type(cb), "code_block");
        assert_eq!(field(cb, "fenced"), &Value::Bool(true));
        assert_eq!(field(cb, "lang"), &Value::String("yaml".into()));
        assert_eq!(field(cb, "info"), &Value::String("yaml".into()));
        assert_eq!(field(cb, "value"), &Value::String("foo: bar\n".into()));
    }

    #[test]
    fn parse_fenced_code_block_without_lang_has_null_lang() {
        let src = "```\nfoo bar\n```\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let cb = &doc_children(&doc)[0];
        assert_eq!(field(cb, "fenced"), &Value::Bool(true));
        assert_eq!(field(cb, "lang"), &Value::Null);
        assert_eq!(field(cb, "info"), &Value::String("".into()));
    }

    #[test]
    fn parse_indented_code_block_is_unfenced() {
        // Four-space-indented => indented code block.
        let src = "    foo bar\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let cb = &doc_children(&doc)[0];
        assert_eq!(node_type(cb), "code_block");
        assert_eq!(field(cb, "fenced"), &Value::Bool(false));
        assert_eq!(field(cb, "lang"), &Value::Null);
    }

    #[test]
    fn parse_inline_code_in_paragraph_emits_code_inline_node() {
        let doc = Markdown.parse(b"Use `cargo run`.\n").expect("parse");
        let p = &doc_children(&doc)[0];
        assert_eq!(node_type(p), "paragraph");
        let Value::Array(inlines) = field(p, "children") else {
            panic!("children array");
        };
        // Find the inline code node.
        let code = inlines
            .iter()
            .find(|n| node_type(n) == "code")
            .expect("inline code present");
        assert_eq!(field(code, "value"), &Value::String("cargo run".into()));
    }

    #[test]
    fn parse_emphasis_strong_strikethrough_each_emit_correct_type() {
        let src = "Plain *italic* and **bold** and ~~strike~~.\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let p = &doc_children(&doc)[0];
        let Value::Array(inlines) = field(p, "children") else {
            panic!()
        };
        let kinds: Vec<&str> = inlines.iter().map(node_type).collect();
        assert!(kinds.contains(&"emphasis"));
        assert!(kinds.contains(&"strong"));
        assert!(kinds.contains(&"strikethrough"));
    }

    #[test]
    fn parse_link_with_title_carries_url_and_title() {
        let doc = Markdown
            .parse(b"See [docs](https://example.com \"Docs\").\n")
            .expect("parse");
        let p = &doc_children(&doc)[0];
        let Value::Array(inlines) = field(p, "children") else {
            panic!()
        };
        let link = inlines
            .iter()
            .find(|n| node_type(n) == "link")
            .expect("link present");
        assert_eq!(
            field(link, "url"),
            &Value::String("https://example.com".into()),
        );
        assert_eq!(field(link, "title"), &Value::String("Docs".into()));
    }

    #[test]
    fn parse_link_without_title_has_null_title() {
        let doc = Markdown
            .parse(b"See [docs](https://example.com).\n")
            .expect("parse");
        let p = &doc_children(&doc)[0];
        let Value::Array(inlines) = field(p, "children") else {
            panic!()
        };
        let link = inlines
            .iter()
            .find(|n| node_type(n) == "link")
            .expect("link present");
        assert_eq!(field(link, "title"), &Value::Null);
    }

    #[test]
    fn parse_image_concatenates_text_descendants_into_alt() {
        let doc = Markdown
            .parse(b"![alt text](img.png \"title\")\n")
            .expect("parse");
        let p = &doc_children(&doc)[0];
        let Value::Array(inlines) = field(p, "children") else {
            panic!()
        };
        let img = inlines
            .iter()
            .find(|n| node_type(n) == "image")
            .expect("image present");
        assert_eq!(field(img, "url"), &Value::String("img.png".into()));
        assert_eq!(field(img, "alt"), &Value::String("alt text".into()));
        assert_eq!(field(img, "title"), &Value::String("title".into()));
    }

    #[test]
    fn parse_unordered_list_emits_list_with_ordered_false() {
        let doc = Markdown.parse(b"- a\n- b\n").expect("parse");
        let l = &doc_children(&doc)[0];
        assert_eq!(node_type(l), "list");
        assert_eq!(field(l, "ordered"), &Value::Bool(false));
        assert_eq!(field(l, "start"), &Value::Null);
    }

    #[test]
    fn parse_ordered_list_emits_list_with_ordered_true_and_start() {
        let doc = Markdown.parse(b"1. a\n2. b\n").expect("parse");
        let l = &doc_children(&doc)[0];
        assert_eq!(node_type(l), "list");
        assert_eq!(field(l, "ordered"), &Value::Bool(true));
        assert_eq!(field(l, "start"), &Value::Int(1));
    }

    #[test]
    fn parse_nested_list_preserves_structure() {
        let src = "- a\n  - nested\n- b\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let outer = &doc_children(&doc)[0];
        assert_eq!(node_type(outer), "list");
        let Value::Array(items) = field(outer, "children") else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        // First item should contain a nested list as one of its children.
        let Value::Array(first_children) = field(&items[0], "children") else {
            panic!()
        };
        let has_nested = first_children.iter().any(|n| node_type(n) == "list");
        assert!(has_nested, "first list item should contain a nested list");
    }

    #[test]
    fn parse_task_list_items_carry_checked_field() {
        let doc = Markdown.parse(b"- [x] done\n- [ ] todo\n").expect("parse");
        let l = &doc_children(&doc)[0];
        let Value::Array(items) = field(l, "children") else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        assert_eq!(field(&items[0], "checked"), &Value::Bool(true));
        assert_eq!(field(&items[1], "checked"), &Value::Bool(false));
    }

    #[test]
    fn parse_block_quote_emits_block_quote_node() {
        let doc = Markdown.parse(b"> quoted line\n").expect("parse");
        let bq = &doc_children(&doc)[0];
        assert_eq!(node_type(bq), "block_quote");
    }

    #[test]
    fn parse_thematic_break_emits_thematic_break_node() {
        // Using `***` so the parser cannot mistake it for a setext underline.
        let doc = Markdown.parse(b"para\n\n***\n").expect("parse");
        let children = doc_children(&doc);
        let kinds: Vec<&str> = children.iter().map(node_type).collect();
        assert!(kinds.contains(&"thematic_break"), "kinds: {kinds:?}");
    }

    #[test]
    fn parse_gfm_table_emits_table_with_header_row() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let t = &doc_children(&doc)[0];
        assert_eq!(node_type(t), "table");
        let Value::Array(rows) = field(t, "children") else {
            panic!()
        };
        assert_eq!(rows.len(), 2, "header + one body row");
        assert_eq!(node_type(&rows[0]), "table_row");
        assert_eq!(field(&rows[0], "header"), &Value::Bool(true));
        assert_eq!(field(&rows[1], "header"), &Value::Bool(false));
    }

    #[test]
    fn parse_yaml_frontmatter_folds_into_document_field() {
        let doc = Markdown
            .parse(b"---\ntitle: Hello\n---\n# Body\n")
            .expect("parse");
        let Value::Map(m) = doc.value() else { panic!() };
        let Value::Map(fm) = m.get("frontmatter").expect("frontmatter present") else {
            panic!(
                "frontmatter should be a map, got {:?}",
                m.get("frontmatter")
            );
        };
        assert_eq!(fm.get("kind"), Some(&Value::String("yaml".into())));
        let Value::Map(inner) = fm.get("value").expect("value present") else {
            panic!()
        };
        assert_eq!(inner.get("title"), Some(&Value::String("Hello".into())));
        // First child is the heading (NOT a frontmatter node).
        let children = doc_children(&doc);
        assert_eq!(node_type(&children[0]), "heading");
    }

    #[test]
    fn parse_toml_frontmatter_folds_correctly() {
        let doc = Markdown
            .parse(b"+++\ntitle = \"Hello\"\n+++\n# Body\n")
            .expect("parse");
        let Value::Map(m) = doc.value() else { panic!() };
        let Value::Map(fm) = m.get("frontmatter").unwrap() else {
            panic!()
        };
        assert_eq!(fm.get("kind"), Some(&Value::String("toml".into())));
    }

    #[test]
    fn parse_json_frontmatter_folds_correctly() {
        // JSON frontmatter must be followed by a blank line per the M5
        // detector contract.
        let doc = Markdown
            .parse(b"{\n  \"title\": \"Hello\"\n}\n\n# Body\n")
            .expect("parse");
        let Value::Map(m) = doc.value() else { panic!() };
        let Value::Map(fm) = m.get("frontmatter").unwrap() else {
            panic!("expected map frontmatter, got {:?}", m.get("frontmatter"));
        };
        assert_eq!(fm.get("kind"), Some(&Value::String("json".into())));
    }

    #[test]
    fn parse_no_frontmatter_produces_null_frontmatter() {
        let doc = Markdown.parse(b"# Just text\n").expect("parse");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.get("frontmatter"), Some(&Value::Null));
    }

    #[test]
    fn parse_heading_position_starts_at_line_one() {
        let doc = Markdown.parse(b"# Hello\n").expect("parse");
        let h = &doc_children(&doc)[0];
        let Value::Map(pos) = field(h, "position") else {
            panic!()
        };
        let Value::Map(start) = pos.get("start").unwrap() else {
            panic!()
        };
        assert_eq!(start.get("line"), Some(&Value::Int(1)));
    }

    #[test]
    fn parse_html_block_emits_value_with_raw_html() {
        let doc = Markdown
            .parse(b"<div class=\"x\">\n  text\n</div>\n")
            .expect("parse");
        let block = &doc_children(&doc)[0];
        assert_eq!(node_type(block), "html_block");
        let Value::String(s) = field(block, "value") else {
            panic!()
        };
        assert!(
            s.contains("<div"),
            "html_block value should preserve HTML: {s}"
        );
    }

    #[test]
    fn write_unmutated_document_emits_original_bytes_verbatim() {
        let src = "# Title\n\nSome paragraph with **bold** text.\n\n```rust\nfn main() {}\n```\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let mut buf: Vec<u8> = Vec::new();
        Markdown.write(&doc, &mut buf).expect("write");
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            src,
            "verbatim round-trip must produce identical bytes",
        );
    }

    #[test]
    fn write_mutated_document_returns_format_error() {
        let src = "# Title\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        // Wrap the doc and mutate the value tree directly. We can't reuse
        // `Document::set_at` here because there are no spans on a markdown
        // doc. Instead, build a new Document with the mutated value but the
        // same original bytes; the write path will see the mismatch.
        let Value::Map(mut m) = doc.value().clone() else {
            panic!()
        };
        m.insert("type".to_owned(), Value::String("not-a-document".into()));
        let mutated = Document::with_value_and_bytes(
            Value::Map(m),
            doc.original_bytes().to_vec(),
            FormatTag::Markdown,
        );
        let mut buf: Vec<u8> = Vec::new();
        let err = Markdown.write(&mutated, &mut buf).unwrap_err();
        match err {
            Error::Format { format, message } => {
                assert_eq!(format, "markdown");
                assert!(
                    message.contains("M11+") || message.contains("read-only"),
                    "message should mention M9 read-only contract: {message}",
                );
            }
            other => panic!("expected Error::Format, got {other:?}"),
        }
    }

    #[test]
    fn parse_carries_format_tag_markdown() {
        let doc = Markdown.parse(b"# x\n").expect("parse");
        assert_eq!(doc.format(), FormatTag::Markdown);
    }

    #[test]
    fn parse_preserves_original_bytes_for_round_trip() {
        let src = b"# Title\n\nSome text.\n";
        let doc = Markdown.parse(src).expect("parse");
        assert_eq!(
            doc.original_bytes(),
            src,
            "original_bytes must be preserved for round-trip detection",
        );
    }

    #[test]
    fn first_whitespace_token_returns_none_for_whitespace_only() {
        assert_eq!(first_whitespace_token(""), None);
        assert_eq!(first_whitespace_token("   "), None);
        assert_eq!(first_whitespace_token("rust"), Some("rust".into()));
        assert_eq!(first_whitespace_token("  rust extra"), Some("rust".into()));
    }
}
