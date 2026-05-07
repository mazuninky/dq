//! XML 1.0 parser and writer (M11) — `quick-xml`-backed, conventional-key
//! mapping onto [`Value`].
//!
//! XML documents map onto the existing `Value` enum using a small fixed
//! vocabulary of conventional keys, instead of introducing a new `Value`
//! variant for elements:
//!
//! | XML construct                          | `Value` mapping                                                   |
//! |----------------------------------------|-------------------------------------------------------------------|
//! | `<tag>` element                        | `Map { tag => Array<Map { ... }> }` on the parent                 |
//! | Attributes                             | `Map { "@attrs" => Map { name => string, ... } }` on the element  |
//! | Text content                           | `String` under key `"#text"` on the element                       |
//! | `<!-- comment -->`                     | `Array<String>` under key `"#comments"` on the parent element     |
//! | `<![CDATA[...]]>` block                 | `Array<String>` under key `"#cdata"` on the element               |
//! | `<?xml-stylesheet ...?>` PI            | `Array<String>` under key `"#pi"` on the parent element           |
//! | `<?xml version="1.0" encoding="..."?>` | `Map { "version", "encoding", "standalone" }` under top-level `"#xml"` |
//! | Namespace prefix on tag (`foo:tag`)    | retained verbatim in the tag name (`"foo:tag"`)                   |
//! | `xmlns:foo` attribute                  | retained verbatim in `@attrs`                                     |
//!
//! Multi-element children with the same tag are stored as a single `Array`
//! to preserve order — even single occurrences are wrapped in a one-element
//! array so `Pointer` indexing is stable across `<a><b/></a>` and
//! `<a><b/><b/></a>`.
//!
//! ## Round-trip contract: partial
//!
//! - Preserved on `parse → write`: element structure, attributes, comments,
//!   CDATA, processing instructions, namespace prefixes, the XML declaration.
//! - **NOT** preserved: mixed content (text interleaved with child elements
//!   in the same parent — e.g. `<p>Hello <b>world</b>!</p>`) is folded into
//!   `"#text"` and inner element positions are lost; whitespace-only
//!   pretty-printing between elements is normalised to compact-with-newlines.
//! - Mixed-content detection emits a `tracing::warn!` so users know their
//!   file is partially round-trippable.
//!
//! ## Read-only edit operations
//!
//! `XmlFormat` does **not** register a textual-edit `ScalarRenderer` /
//! `InsertionRenderer` — `Document::set_at` against an XML document falls
//! through to whatever the M2 contract dictates for non-span formats
//! (best-effort re-emit through `Format::write`).

use std::io::{Cursor, Write};

use camino::Utf8PathBuf;
use indexmap::IndexMap;
use quick_xml::Writer;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesCData, BytesDecl, BytesEnd, BytesPI, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;

use crate::Result;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;

/// Conventional key for the XML declaration block at the top level.
const KEY_XML_DECL: &str = "#xml";
/// Conventional key holding an element's attribute map.
const KEY_ATTRS: &str = "@attrs";
/// Conventional key holding an element's text body.
const KEY_TEXT: &str = "#text";
/// Conventional key holding an element's CDATA blocks (`Array<String>`).
const KEY_CDATA: &str = "#cdata";
/// Conventional key holding child-PI strings on the parent element
/// (`Array<String>`).
const KEY_PI: &str = "#pi";
/// Conventional key holding child-comment strings on the parent element
/// (`Array<String>`).
const KEY_COMMENTS: &str = "#comments";

/// XML 1.0 format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Xml;

impl Format for Xml {
    fn name(&self) -> &'static str {
        "xml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xml"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let value = parse_xml(bytes)?;
        Ok(Document::value_only(value, FormatTag::Xml))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        let buf = render_xml(doc.value())?;
        w.write_all(&buf).map_err(|source| Error::WriteIo {
            path: Utf8PathBuf::from("<xml-writer>"),
            source,
        })
    }
}

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

/// Frame on the parse stack. We accumulate child entries as we read events;
/// when we hit `End` we finalise the frame into a `Value::Map` and append it
/// to the parent's child list under the tag name.
struct Frame {
    /// Element tag name (with namespace prefix, if any), verbatim.
    tag: String,
    /// Attributes map; populated on Start and frozen by the time the
    /// element is finalised.
    attrs: IndexMap<String, Value>,
    /// Children grouped by tag name. The inner `Vec<Value>` preserves
    /// document order across same-tag siblings.
    children: IndexMap<String, Vec<Value>>,
    /// Concatenated text content (whitespace and entity-decoded).
    text: String,
    /// Whether any non-whitespace text has been observed.
    has_text: bool,
    /// CDATA blocks observed in this element's content.
    cdata: Vec<String>,
    /// `<?...?>` PIs observed inside this element (not the document
    /// declaration — that one lives at the document root).
    pi: Vec<String>,
    /// Comments observed inside this element.
    comments: Vec<String>,
    /// Whether we've seen an element child after observing non-whitespace
    /// text (or vice-versa). Used to detect mixed content.
    saw_element: bool,
    saw_mixed: bool,
}

impl Frame {
    fn new(tag: String) -> Self {
        Self {
            tag,
            attrs: IndexMap::new(),
            children: IndexMap::new(),
            text: String::new(),
            has_text: false,
            cdata: Vec::new(),
            pi: Vec::new(),
            comments: Vec::new(),
            saw_element: false,
            saw_mixed: false,
        }
    }

    fn finalise(self, path_for_warn: &str) -> Value {
        // Mixed-content warning fires if both element children AND
        // non-whitespace text were observed inside this element.
        if self.saw_mixed {
            tracing::warn!(
                "XML parse: mixed content detected at element path '{path_for_warn}'; \
                 inner element positions will not round-trip — text was folded into '{KEY_TEXT}'",
            );
        }
        let mut out: IndexMap<String, Value> = IndexMap::new();
        if !self.attrs.is_empty() {
            out.insert(KEY_ATTRS.to_owned(), Value::Map(self.attrs));
        }
        if self.has_text {
            out.insert(KEY_TEXT.to_owned(), Value::String(self.text));
        }
        if !self.cdata.is_empty() {
            out.insert(
                KEY_CDATA.to_owned(),
                Value::Array(self.cdata.into_iter().map(Value::String).collect()),
            );
        }
        if !self.pi.is_empty() {
            out.insert(
                KEY_PI.to_owned(),
                Value::Array(self.pi.into_iter().map(Value::String).collect()),
            );
        }
        if !self.comments.is_empty() {
            out.insert(
                KEY_COMMENTS.to_owned(),
                Value::Array(self.comments.into_iter().map(Value::String).collect()),
            );
        }
        for (name, group) in self.children {
            out.insert(name, Value::Array(group));
        }
        Value::Map(out)
    }
}

/// Stack-driven XML parser. Returns the top-level `Value::Map` shape.
fn parse_xml(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
        file: None,
        line: 0,
        col: 0,
        span: 0..0,
        snippet: String::new(),
        message: format!("invalid UTF-8 in XML input: {e}"),
    })?;
    let mut reader = Reader::from_str(text);
    let cfg = reader.config_mut();
    // We deliberately preserve text whitespace verbatim — XML config-doc
    // round-trip needs to keep meaningful body text intact (e.g.
    // `<name>Alice</name>`'s value is `"Alice"`, not `""` after a trim).
    cfg.trim_text(false);

    let mut top: IndexMap<String, Value> = IndexMap::new();
    let mut top_children: IndexMap<String, Vec<Value>> = IndexMap::new();
    let mut top_pi: Vec<String> = Vec::new();
    let mut top_comments: Vec<String> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();

    loop {
        match reader.read_event() {
            Err(e) => {
                // `buffer_position` returns the byte offset of the cursor at
                // the time of the error — that's the closest we can get to a
                // (line, col) without rebuilding line breaks ourselves.
                let pos = reader.buffer_position() as usize;
                let (line, col) = byte_pos_to_line_col(text, pos);
                return Err(Error::Parse {
                    file: None,
                    line,
                    col,
                    span: pos..pos,
                    snippet: snippet_for_pos(text, pos),
                    message: format!("XML parse error: {e}"),
                });
            }
            Ok(Event::Eof) => break,
            Ok(Event::Decl(decl)) => {
                if !stack.is_empty() {
                    // Inner declarations are not standard XML; treat as PI.
                    let raw = bytes_to_string(&decl);
                    push_pi(&mut stack, &mut top_pi, raw);
                    continue;
                }
                let mut decl_map: IndexMap<String, Value> = IndexMap::new();
                if let Ok(v) = decl.version() {
                    decl_map.insert(
                        "version".to_owned(),
                        Value::String(String::from_utf8_lossy(&v).into_owned()),
                    );
                }
                if let Some(Ok(enc)) = decl.encoding() {
                    decl_map.insert(
                        "encoding".to_owned(),
                        Value::String(String::from_utf8_lossy(&enc).into_owned()),
                    );
                }
                if let Some(Ok(sa)) = decl.standalone() {
                    decl_map.insert(
                        "standalone".to_owned(),
                        Value::String(String::from_utf8_lossy(&sa).into_owned()),
                    );
                }
                top.insert(KEY_XML_DECL.to_owned(), Value::Map(decl_map));
            }
            Ok(Event::DocType(_)) => {
                // M11 v1: DOCTYPE declarations are not preserved on
                // round-trip. Document the omission via tracing so the user
                // is aware before relying on this for DTD-bearing files.
                tracing::warn!(
                    "XML parse: <!DOCTYPE ...> declaration found but not preserved on round-trip",
                );
            }
            Ok(Event::PI(pi)) => {
                let raw = bytes_to_string(&pi);
                push_pi(&mut stack, &mut top_pi, raw);
            }
            Ok(Event::Comment(c)) => {
                let txt = decode_text(&c)?;
                push_comment(&mut stack, &mut top_comments, txt);
            }
            Ok(Event::Start(start)) => {
                let frame = build_start_frame(&start)?;
                if let Some(parent) = stack.last_mut() {
                    parent.saw_element = true;
                    if parent.has_text {
                        parent.saw_mixed = true;
                    }
                }
                stack.push(frame);
            }
            Ok(Event::Empty(start)) => {
                let frame = build_start_frame(&start)?;
                if let Some(parent) = stack.last_mut() {
                    parent.saw_element = true;
                    if parent.has_text {
                        parent.saw_mixed = true;
                    }
                }
                let path = element_path(&stack, &frame.tag);
                let tag = frame.tag.clone();
                let value = frame.finalise(&path);
                append_child(&mut stack, &mut top_children, &tag, value);
            }
            Ok(Event::End(end)) => {
                let Some(frame) = stack.pop() else {
                    let pos = reader.buffer_position() as usize;
                    let (line, col) = byte_pos_to_line_col(text, pos);
                    return Err(Error::Parse {
                        file: None,
                        line,
                        col,
                        span: pos..pos,
                        snippet: snippet_for_pos(text, pos),
                        message: format!(
                            "XML parse error: unexpected closing tag </{}>",
                            String::from_utf8_lossy(end.name().as_ref()),
                        ),
                    });
                };
                let end_name = String::from_utf8_lossy(end.name().as_ref()).into_owned();
                if end_name != frame.tag {
                    let pos = reader.buffer_position() as usize;
                    let (line, col) = byte_pos_to_line_col(text, pos);
                    return Err(Error::Parse {
                        file: None,
                        line,
                        col,
                        span: pos..pos,
                        snippet: snippet_for_pos(text, pos),
                        message: format!(
                            "XML parse error: closing tag </{}> does not match <{}>",
                            end_name, frame.tag,
                        ),
                    });
                }
                let path = element_path(&stack, &frame.tag);
                let tag = frame.tag.clone();
                let value = frame.finalise(&path);
                append_child(&mut stack, &mut top_children, &tag, value);
            }
            Ok(Event::Text(t)) => {
                let s = decode_text(&t)?;
                if let Some(frame) = stack.last_mut() {
                    if !s.trim().is_empty() {
                        if frame.saw_element {
                            frame.saw_mixed = true;
                        }
                        frame.has_text = true;
                    }
                    frame.text.push_str(&s);
                } else {
                    // Whitespace between top-level constructs is harmless;
                    // ignore non-whitespace text (it would be a malformed
                    // XML doc, but quick-xml will report that as Err
                    // separately).
                }
            }
            Ok(Event::CData(c)) => {
                // CData payload is stored verbatim (no escape decoding —
                // CDATA bodies are not entity-encoded).
                let s = String::from_utf8_lossy(c.as_ref()).into_owned();
                if let Some(frame) = stack.last_mut() {
                    frame.cdata.push(s);
                }
                // CDATA at top level is unusual; ignore (well-formed
                // XML cannot contain it outside an element).
            }
        }
    }

    // Drain top-level commentary into the result map. Top-level processing
    // instructions and comments live next to (or before) the root element.
    if !top_pi.is_empty() {
        top.insert(
            KEY_PI.to_owned(),
            Value::Array(top_pi.into_iter().map(Value::String).collect()),
        );
    }
    if !top_comments.is_empty() {
        top.insert(
            KEY_COMMENTS.to_owned(),
            Value::Array(top_comments.into_iter().map(Value::String).collect()),
        );
    }
    // Well-formed XML requires exactly one root element. The writer
    // (`render_xml`) silently emits only the first non-conventional key
    // when more than one is present, so we fail loudly here during parse
    // instead of accepting input we cannot round-trip. Conventional keys
    // (`#xml`, `#pi`, `#comments`) are not "elements" and don't count.
    let root_count: usize = top_children.values().map(Vec::len).sum();
    if root_count != 1 {
        let pos = reader.buffer_position() as usize;
        let (line, col) = byte_pos_to_line_col(text, pos);
        return Err(Error::Parse {
            file: None,
            line,
            col,
            span: pos..pos,
            snippet: snippet_for_pos(text, pos),
            message: format!("XML must have exactly one root element; found {root_count}",),
        });
    }
    for (name, group) in top_children {
        top.insert(name, Value::Array(group));
    }
    Ok(Value::Map(top))
}

/// Build a [`Frame`] from a `<tag attr="...">` event.
fn build_start_frame(start: &BytesStart<'_>) -> Result<Frame> {
    let tag = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut frame = Frame::new(tag);
    for attr in start.attributes() {
        let attr = attr.map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("XML parse error: malformed attribute: {e}"),
        })?;
        let name = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = decode_attr(&attr)?;
        frame.attrs.insert(name, Value::String(value));
    }
    Ok(frame)
}

fn decode_text(t: &BytesText<'_>) -> Result<String> {
    t.unescape()
        .map(|c| c.into_owned())
        .map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("XML parse error: malformed text/comment: {e}"),
        })
}

fn decode_attr(attr: &Attribute<'_>) -> Result<String> {
    attr.unescape_value()
        .map(|c| c.into_owned())
        .map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("XML parse error: malformed attribute value: {e}"),
        })
}

fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn push_pi(stack: &mut [Frame], top_pi: &mut Vec<String>, raw: String) {
    if let Some(frame) = stack.last_mut() {
        frame.pi.push(raw);
    } else {
        top_pi.push(raw);
    }
}

fn push_comment(stack: &mut [Frame], top_comments: &mut Vec<String>, txt: String) {
    if let Some(frame) = stack.last_mut() {
        frame.comments.push(txt);
    } else {
        top_comments.push(txt);
    }
}

fn append_child(
    stack: &mut [Frame],
    top_children: &mut IndexMap<String, Vec<Value>>,
    tag: &str,
    value: Value,
) {
    if let Some(parent) = stack.last_mut() {
        parent
            .children
            .entry(tag.to_owned())
            .or_default()
            .push(value);
    } else {
        top_children.entry(tag.to_owned()).or_default().push(value);
    }
}

fn element_path(stack: &[Frame], current_tag: &str) -> String {
    let mut path = String::new();
    for f in stack {
        path.push('/');
        path.push_str(&f.tag);
    }
    path.push('/');
    path.push_str(current_tag);
    path
}

fn byte_pos_to_line_col(text: &str, pos: usize) -> (u32, u32) {
    let pos = pos.min(text.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for (i, ch) in text.char_indices() {
        if i >= pos {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn snippet_for_pos(text: &str, pos: usize) -> String {
    let pos = pos.min(text.len());
    let start = pos.saturating_sub(20);
    let end = (pos + 20).min(text.len());
    // Snap to char boundaries.
    let start = (0..=start)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let end = (end..=text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    text[start..end].replace('\n', "\\n")
}

// ---------------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------------

/// Render the value tree back into XML bytes.
///
/// Top-level value MUST be a `Value::Map`. The map may carry `#xml` (decl),
/// `#pi` and `#comments` entries plus exactly one element-shaped entry that
/// becomes the root `<tag>...</tag>`. Other shapes return `Error::Format`.
fn render_xml(top: &Value) -> Result<Vec<u8>> {
    let Value::Map(top) = top else {
        return Err(Error::Format {
            format: "xml",
            message: "top-level XML value must be a map (object)".to_owned(),
        });
    };
    let mut writer = Writer::new(Cursor::new(Vec::<u8>::new()));

    // 1. XML declaration.
    if let Some(Value::Map(decl)) = top.get(KEY_XML_DECL) {
        let version = match decl.get("version") {
            Some(Value::String(s)) => s.clone(),
            _ => "1.0".to_owned(),
        };
        let encoding = match decl.get("encoding") {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let standalone = match decl.get("standalone") {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        writer
            .write_event(Event::Decl(BytesDecl::new(
                &version,
                encoding.as_deref(),
                standalone.as_deref(),
            )))
            .map_err(map_write_err)?;
    }

    // 2. Top-level processing instructions and comments (kept above root).
    if let Some(Value::Array(items)) = top.get(KEY_PI) {
        for it in items {
            if let Value::String(s) = it {
                writer
                    .write_event(Event::PI(BytesPI::new(s.as_str())))
                    .map_err(map_write_err)?;
            }
        }
    }
    if let Some(Value::Array(items)) = top.get(KEY_COMMENTS) {
        for it in items {
            if let Value::String(s) = it {
                writer
                    .write_event(Event::Comment(BytesText::new(s.as_str())))
                    .map_err(map_write_err)?;
            }
        }
    }

    // 3. Root element. Pick the first non-conventional key as the root tag.
    let mut root_tag: Option<&str> = None;
    for (k, _) in top {
        if !is_conventional(k) {
            root_tag = Some(k.as_str());
            break;
        }
    }
    let Some(root_tag) = root_tag else {
        return Err(Error::Format {
            format: "xml",
            message: "XML write requires a root element; top-level map has no element-shaped key"
                .to_owned(),
        });
    };
    let Some(Value::Array(roots)) = top.get(root_tag) else {
        return Err(Error::Format {
            format: "xml",
            message: format!(
                "XML write: root '{root_tag}' must be a one-element Array (got non-array)"
            ),
        });
    };
    if roots.len() != 1 {
        return Err(Error::Format {
            format: "xml",
            message: format!(
                "XML write: root element '{root_tag}' must occur exactly once at the top level (got {})",
                roots.len(),
            ),
        });
    }
    let Value::Map(root_body) = &roots[0] else {
        return Err(Error::Format {
            format: "xml",
            message: format!("XML write: root element '{root_tag}' must be a Map"),
        });
    };
    write_element(&mut writer, root_tag, root_body)?;

    Ok(writer.into_inner().into_inner())
}

fn write_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    tag: &str,
    body: &IndexMap<String, Value>,
) -> Result<()> {
    let mut start = BytesStart::new(tag);
    if let Some(Value::Map(attrs)) = body.get(KEY_ATTRS) {
        for (k, v) in attrs {
            let v = match v {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Int(i) => i.to_string(),
                Value::Float(f) => f.to_string(),
                Value::BigInt(s) | Value::BigFloat(s) => s.clone(),
                Value::Null => String::new(),
                _ => {
                    return Err(Error::Format {
                        format: "xml",
                        message: format!(
                            "XML write: attribute '{k}' on <{tag}> must be a scalar (got non-scalar)",
                        ),
                    });
                }
            };
            start.push_attribute(Attribute::from((k.as_str(), v.as_str())));
        }
    }

    // Determine if the element has any body content. If not, emit as
    // self-closing `<tag/>`.
    let has_text = body
        .get(KEY_TEXT)
        .is_some_and(|v| !matches!(v, Value::Null));
    let has_cdata = body.get(KEY_CDATA).is_some_and(non_empty_array);
    let has_pi = body.get(KEY_PI).is_some_and(non_empty_array);
    let has_comments = body.get(KEY_COMMENTS).is_some_and(non_empty_array);
    let has_children = body
        .iter()
        .any(|(k, v)| !is_conventional(k) && non_empty_array(v));

    if !has_text && !has_cdata && !has_pi && !has_comments && !has_children {
        writer
            .write_event(Event::Empty(start))
            .map_err(map_write_err)?;
        return Ok(());
    }

    writer
        .write_event(Event::Start(start))
        .map_err(map_write_err)?;

    // Comments first, then PIs, then text, then CDATA, then children.
    // Inner ordering is best-effort only; mixed content is documented as
    // lossy on round-trip (per spec D5).
    if let Some(Value::Array(items)) = body.get(KEY_COMMENTS) {
        for it in items {
            if let Value::String(s) = it {
                writer
                    .write_event(Event::Comment(BytesText::new(s.as_str())))
                    .map_err(map_write_err)?;
            }
        }
    }
    if let Some(Value::Array(items)) = body.get(KEY_PI) {
        for it in items {
            if let Value::String(s) = it {
                writer
                    .write_event(Event::PI(BytesPI::new(s.as_str())))
                    .map_err(map_write_err)?;
            }
        }
    }
    if let Some(Value::String(s)) = body.get(KEY_TEXT) {
        writer
            .write_event(Event::Text(BytesText::new(s.as_str())))
            .map_err(map_write_err)?;
    }
    if let Some(Value::Array(items)) = body.get(KEY_CDATA) {
        for it in items {
            if let Value::String(s) = it {
                writer
                    .write_event(Event::CData(BytesCData::new(s.as_str())))
                    .map_err(map_write_err)?;
            }
        }
    }

    for (k, v) in body {
        if is_conventional(k) {
            continue;
        }
        let Value::Array(items) = v else {
            return Err(Error::Format {
                format: "xml",
                message: format!(
                    "XML write: child '{k}' under <{tag}> must be an Array of element maps",
                ),
            });
        };
        for child in items {
            match child {
                Value::Map(child_body) => {
                    write_element(writer, k, child_body)?;
                }
                Value::String(s) => {
                    // String-shaped child: best-effort emit as
                    // `<k>text</k>`. This shape arises when JSON input is
                    // converted to XML — `{"name": ["Alice"]}` becomes
                    // `<name>Alice</name>`.
                    let mut tmp = IndexMap::new();
                    tmp.insert(KEY_TEXT.to_owned(), Value::String(s.clone()));
                    write_element(writer, k, &tmp)?;
                }
                Value::Null => {
                    // Empty child element.
                    let tmp = IndexMap::new();
                    write_element(writer, k, &tmp)?;
                }
                Value::Bool(b) => {
                    let mut tmp = IndexMap::new();
                    tmp.insert(KEY_TEXT.to_owned(), Value::String(b.to_string()));
                    write_element(writer, k, &tmp)?;
                }
                Value::Int(i) => {
                    let mut tmp = IndexMap::new();
                    tmp.insert(KEY_TEXT.to_owned(), Value::String(i.to_string()));
                    write_element(writer, k, &tmp)?;
                }
                Value::Float(f) => {
                    let mut tmp = IndexMap::new();
                    tmp.insert(KEY_TEXT.to_owned(), Value::String(f.to_string()));
                    write_element(writer, k, &tmp)?;
                }
                Value::BigInt(s) | Value::BigFloat(s) => {
                    let mut tmp = IndexMap::new();
                    tmp.insert(KEY_TEXT.to_owned(), Value::String(s.clone()));
                    write_element(writer, k, &tmp)?;
                }
                Value::Array(_) => {
                    return Err(Error::Format {
                        format: "xml",
                        message: format!(
                            "XML write: nested arrays are not representable; child '{k}' under <{tag}>",
                        ),
                    });
                }
            }
        }
    }

    writer
        .write_event(Event::End(BytesEnd::new(tag)))
        .map_err(map_write_err)?;
    Ok(())
}

fn is_conventional(k: &str) -> bool {
    matches!(
        k,
        KEY_XML_DECL | KEY_ATTRS | KEY_TEXT | KEY_CDATA | KEY_PI | KEY_COMMENTS
    )
}

fn non_empty_array(v: &Value) -> bool {
    matches!(v, Value::Array(items) if !items.is_empty())
}

fn map_write_err(e: quick_xml::Error) -> Error {
    Error::Format {
        format: "xml",
        message: format!("XML write error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        parse_xml(s.as_bytes()).expect("parse xml")
    }

    fn render(v: &Value) -> String {
        let bytes = render_xml(v).expect("render xml");
        String::from_utf8(bytes).expect("utf-8")
    }

    fn root_element_body<'a>(v: &'a Value, tag: &str) -> &'a IndexMap<String, Value> {
        let Value::Map(top) = v else {
            panic!("expected top map")
        };
        let Some(Value::Array(items)) = top.get(tag) else {
            panic!("expected /{tag} array")
        };
        let Value::Map(body) = items.first().expect("at least one") else {
            panic!("expected element map")
        };
        body
    }

    #[test]
    fn parse_element_with_attribute_and_text() {
        let v = parse("<user id=\"42\"><name>Alice</name></user>");
        let user = root_element_body(&v, "user");
        let Some(Value::Map(attrs)) = user.get(KEY_ATTRS) else {
            panic!("missing @attrs")
        };
        assert_eq!(attrs.get("id"), Some(&Value::String("42".into())));
        let Some(Value::Array(names)) = user.get("name") else {
            panic!("missing name array")
        };
        let Value::Map(name) = &names[0] else {
            panic!("expected name map")
        };
        assert_eq!(name.get(KEY_TEXT), Some(&Value::String("Alice".into())));
    }

    #[test]
    fn parse_multi_child_same_tag_preserves_order() {
        let v = parse("<list><item>A</item><item>B</item><item>C</item></list>");
        let list = root_element_body(&v, "list");
        let Some(Value::Array(items)) = list.get("item") else {
            panic!("missing item array")
        };
        assert_eq!(items.len(), 3);
        let extract_text = |idx: usize| {
            let Value::Map(m) = &items[idx] else { panic!() };
            match m.get(KEY_TEXT) {
                Some(Value::String(s)) => s.clone(),
                _ => panic!("missing #text"),
            }
        };
        assert_eq!(extract_text(0), "A");
        assert_eq!(extract_text(1), "B");
        assert_eq!(extract_text(2), "C");
    }

    #[test]
    fn parse_then_write_round_trip_attribute_and_text() {
        let v = parse("<user id=\"42\"><name>Alice</name></user>");
        let out = render(&v);
        let v2 = parse(&out);
        assert_eq!(v, v2, "round-trip must preserve the value tree");
    }

    #[test]
    fn parse_xml_declaration_preserved() {
        let v = parse(r#"<?xml version="1.0" encoding="UTF-8"?><root/>"#);
        let Value::Map(top) = &v else { panic!() };
        let Some(Value::Map(decl)) = top.get(KEY_XML_DECL) else {
            panic!("missing #xml")
        };
        assert_eq!(decl.get("version"), Some(&Value::String("1.0".into())));
        assert_eq!(decl.get("encoding"), Some(&Value::String("UTF-8".into())));
        let out = render(&v);
        assert!(
            out.contains("<?xml") && out.contains("version=\"1.0\"") && out.contains("UTF-8"),
            "rendered output must contain the declaration; got: {out:?}",
        );
    }

    #[test]
    fn parse_comment_is_attached_to_parent() {
        let v = parse("<root><!-- top note --><a/></root>");
        let root = root_element_body(&v, "root");
        let Some(Value::Array(comments)) = root.get(KEY_COMMENTS) else {
            panic!("missing #comments")
        };
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0], Value::String(" top note ".to_owned()));
        // Round-trip preserves the comment text.
        let out = render(&v);
        assert!(
            out.contains("<!-- top note -->"),
            "comment must round-trip; got: {out:?}",
        );
    }

    #[test]
    fn parse_cdata_preserves_inner_bytes() {
        let v = parse("<script><![CDATA[if (a < b) {}]]></script>");
        let script = root_element_body(&v, "script");
        let Some(Value::Array(cdata)) = script.get(KEY_CDATA) else {
            panic!("missing #cdata")
        };
        assert_eq!(cdata.len(), 1);
        assert_eq!(cdata[0], Value::String("if (a < b) {}".to_owned()));
        let out = render(&v);
        assert!(
            out.contains("<![CDATA[if (a < b) {}]]>"),
            "cdata block must round-trip byte-identically inside; got: {out:?}",
        );
    }

    #[test]
    fn parse_namespace_prefix_retained_verbatim() {
        let v = parse(r#"<svg:rect xmlns:svg="http://www.w3.org/2000/svg" width="10"/>"#);
        let rect = root_element_body(&v, "svg:rect");
        let Some(Value::Map(attrs)) = rect.get(KEY_ATTRS) else {
            panic!("missing @attrs")
        };
        assert!(
            attrs.contains_key("xmlns:svg"),
            "xmlns:svg attr must be retained verbatim",
        );
        assert_eq!(attrs.get("width"), Some(&Value::String("10".into())));
    }

    #[test]
    fn parse_empty_element_round_trips_as_self_closing() {
        let v = parse("<root><a/></root>");
        let out = render(&v);
        assert!(
            out.contains("<a/>") || out.contains("<a />"),
            "empty element must round-trip as self-closing; got: {out:?}",
        );
    }

    #[test]
    fn parse_invalid_xml_returns_parse_error() {
        let err = Xml
            .parse(b"<root><a></b></root>")
            .expect_err("malformed must surface as parse error");
        assert!(
            matches!(err, Error::Parse { .. }),
            "expected Error::Parse, got: {err:?}"
        );
        assert_eq!(err.kind_name(), "parse");
    }

    #[test]
    fn write_top_level_non_map_is_format_error() {
        let v = Value::String("not a map".into());
        let err = render_xml(&v).expect_err("top-level non-map must reject");
        assert!(matches!(err, Error::Format { format: "xml", .. }));
    }

    #[test]
    fn write_top_level_no_root_element_is_format_error() {
        // Top-level map carrying ONLY conventional keys (e.g. only `#xml`)
        // has no element to emit. Reject with `Error::Format` rather than
        // emitting an empty document.
        let mut top = IndexMap::new();
        let mut decl = IndexMap::new();
        decl.insert("version".into(), Value::String("1.0".into()));
        top.insert(KEY_XML_DECL.into(), Value::Map(decl));
        let err = render_xml(&Value::Map(top)).expect_err("no root must reject");
        assert!(
            matches!(err, Error::Format { format: "xml", message } if message.contains("root")),
            "expected Error::Format mentioning 'root'",
        );
    }

    #[test]
    fn parse_mixed_content_logs_warn_and_succeeds() {
        // Mixed content: text interleaved with child elements. We can't
        // capture the tracing log here without setting up a subscriber, so
        // pin the *behaviour*: parse succeeds and the body is folded into
        // `#text`.
        let v = parse("<p>Hello <b>world</b>!</p>");
        let p = root_element_body(&v, "p");
        // Text is preserved (concatenated text nodes around `<b>`).
        let Some(Value::String(text)) = p.get(KEY_TEXT) else {
            panic!("expected #text on <p>")
        };
        assert!(
            text.contains("Hello") && text.contains("!"),
            "mixed-content text must be retained; got: {text:?}",
        );
        // Inner element is still recorded — only its position relative to
        // the text is lost.
        assert!(
            p.contains_key("b"),
            "inner <b> element must still be recorded"
        );
    }
}
