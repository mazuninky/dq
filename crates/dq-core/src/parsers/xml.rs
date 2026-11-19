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
//! ## Writing a tree that did not come from XML
//!
//! The writer takes any scalar where the reader produces a string: an
//! attribute value, `#text`, the `#xml` fields, and the items of `#comments`
//! / `#pi` / `#cdata` (a lone scalar there counts as a one-item list) are
//! written as the scalar's text — a float in its shortest round-trip
//! spelling. A null is no content: no text, an empty attribute value, an
//! item skipped, no declaration. Anything the writer could not put into the
//! XML is refused with `Error::Format` instead of dropped: a container where
//! a scalar belongs, an `#xml` key other than `version` / `encoding` /
//! `standalone`, a second root element, and `@attrs` / `#text` / `#cdata`
//! with content at the top level, outside the root.
//!
//! The writer never emits XML that is not well-formed, and never alters
//! content to make it so. It refuses: an element or attribute name that is
//! not a namespace `QName` (one `prefix:` at most), a PI target that is not
//! an `NCName` or is `xml` in any case, a character XML 1.0 does not allow
//! (a C0 control other than tab, LF, CR), a comment with `--` or a trailing
//! `-`, a PI with `?>`, declaration fields outside its grammar, and a
//! declared encoding the UTF-8 bytes would contradict — anything but UTF-8,
//! except an ASCII-compatible one (US-ASCII, ISO-8859-*, windows-125x) on
//! an all-ASCII document. A boolean `standalone` is `yes` / `no`. CDATA
//! holding `]]>` is split across adjacent sections, which read back as
//! separate `#cdata` items with the same text. Comments are written as they
//! are: XML has no escapes inside a comment. A CR in text and a tab, LF or
//! CR in an attribute value are written as character references, so XML's
//! end-of-line and attribute normalization give them back unchanged.
//!
//! ## Read-only edit operations
//!
//! `XmlFormat` does **not** register a textual-edit `ScalarRenderer` /
//! `InsertionRenderer` — `Document::set_at` against an XML document falls
//! through to whatever the M2 contract dictates for non-span formats
//! (best-effort re-emit through `Format::write`).

use std::borrow::Cow;
use std::io::{Cursor, Write};

use camino::Utf8PathBuf;
use indexmap::IndexMap;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{
    BytesCData, BytesDecl, BytesEnd, BytesPI, BytesRef, BytesStart, BytesText, Event,
};
use quick_xml::name::QName;
use quick_xml::reader::Reader;
use quick_xml::{Writer, XmlVersion};

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
                    let raw = (*decl).to_owned();
                    push_pi(&mut stack, &mut top_pi, raw);
                    continue;
                }
                let mut decl_map: IndexMap<String, Value> = IndexMap::new();
                if let Ok(v) = decl.version() {
                    decl_map.insert("version".to_owned(), Value::String(v.into_owned()));
                }
                if let Some(Ok(enc)) = decl.encoding() {
                    decl_map.insert("encoding".to_owned(), Value::String(enc.into_owned()));
                }
                if let Some(Ok(sa)) = decl.standalone() {
                    decl_map.insert("standalone".to_owned(), Value::String(sa.into_owned()));
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
                let raw = (*pi).to_owned();
                push_pi(&mut stack, &mut top_pi, raw);
            }
            Ok(Event::Comment(c)) => {
                let txt = decode_text(&c);
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
                            end.name().as_ref(),
                        ),
                    });
                };
                let end_name = end.name().as_ref().to_owned();
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
                let s = decode_text(&t);
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
                let s = (*c).to_owned();
                if let Some(frame) = stack.last_mut() {
                    frame.cdata.push(s);
                }
                // CDATA at top level is unusual; ignore (well-formed
                // XML cannot contain it outside an element).
            }
            Ok(Event::GeneralRef(r)) => {
                // quick-xml 0.38+ reports `&amp;`, `&lt;`, etc. as a separate
                // event instead of expanding them inside `Event::Text`. Resolve
                // the reference here and append to the current frame's text so
                // the parsed `Value` matches the pre-0.38 behaviour (entities
                // are folded into the surrounding text body).
                let s = resolve_general_ref(&r, text, reader.buffer_position() as usize)?;
                if let Some(frame) = stack.last_mut() {
                    if frame.saw_element {
                        frame.saw_mixed = true;
                    }
                    frame.has_text = true;
                    frame.text.push_str(&s);
                }
                // GeneralRef outside any element would be a malformed document;
                // quick-xml reports the structural error separately.
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
    let tag = start.name().as_ref().to_owned();
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
        let name = attr.key.as_ref().to_owned();
        let value = decode_attr(&attr)?;
        frame.attrs.insert(name, Value::String(value));
    }
    Ok(frame)
}

fn decode_text(t: &BytesText<'_>) -> String {
    // quick-xml 0.38+ split entity resolution out of `Event::Text`: text events
    // now carry the raw (un-entity-expanded) text and entities are reported as
    // a separate `Event::GeneralRef`. `xml_content` still applies EOL
    // normalisation per the XML 1.0 spec, which matches the pre-0.38 behaviour
    // for everything except entities (those are handled in the `GeneralRef`
    // arm above).
    //
    // Since quick-xml 0.42 event content is already `str`, so `xml_content`
    // is infallible. The pre-0.42 decoding error was unreachable for us
    // anyway: `parse_xml` rejects non-UTF-8 input before the reader runs.
    t.xml_content(XmlVersion::Implicit1_0).into_owned()
}

fn decode_attr(attr: &Attribute<'_>) -> Result<String> {
    // `unescape_value` was deprecated in quick-xml 0.38 in favour of
    // `normalized_value`, which does the same predefined-entity resolution
    // plus the spec-mandated whitespace normalisation. The wire-level output
    // is the same for the predefined-entity set we care about.
    attr.normalized_value(XmlVersion::Implicit1_0)
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

/// Resolve an `Event::GeneralRef(&entity;)` payload into its textual
/// replacement.
///
/// Handles the five predefined XML entities (`amp`, `lt`, `gt`, `quot`,
/// `apos`) plus numeric character references (`&#48;` / `&#x30;`). Anything
/// else is an undeclared entity — without a DTD we cannot resolve it, so we
/// surface a `Error::Parse` rather than silently swallowing the reference.
fn resolve_general_ref(r: &BytesRef<'_>, text: &str, pos: usize) -> Result<String> {
    let make_parse_err = |msg: String| {
        let (line, col) = byte_pos_to_line_col(text, pos);
        Error::Parse {
            file: None,
            line,
            col,
            span: pos..pos,
            snippet: snippet_for_pos(text, pos),
            message: msg,
        }
    };
    // quick-xml 0.42: `BytesRef` derefs to the entity name as `str`; the
    // fallible `decode()` step (UTF-8 only, so it never failed for us) is gone.
    let name: &str = r;
    if r.is_char_ref() {
        // Numeric character reference — `&#48;` or `&#x30;`.
        let ch = r.resolve_char_ref().map_err(|e| {
            make_parse_err(format!(
                "XML parse error: malformed character reference: {e}",
            ))
        })?;
        return Ok(ch.map_or_else(String::new, |c| c.to_string()));
    }
    if let Some(resolved) = quick_xml::escape::resolve_predefined_entity(name) {
        return Ok(resolved.to_owned());
    }
    Err(make_parse_err(format!(
        "XML parse error: undeclared entity reference '&{name};'",
    )))
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

    // Outside the root there is nowhere to put attributes, text or CDATA:
    // refuse them rather than drop them. A null or an empty container
    // carries nothing and passes.
    for key in [KEY_ATTRS, KEY_TEXT, KEY_CDATA] {
        if let Some(v) = top.get(key)
            && !is_empty_content(v)
        {
            return Err(Error::Format {
                format: "xml",
                message: format!(
                    "XML write: '{key}' cannot stand at the top level, outside the root element (got {})",
                    v.type_name(),
                ),
            });
        }
    }

    // 1. XML declaration.
    let decl = declaration(top.get(KEY_XML_DECL))?;
    if let Some(decl) = &decl {
        writer
            .write_event(Event::Decl(BytesDecl::new(
                &decl.version,
                decl.encoding.as_deref(),
                decl.standalone.as_deref(),
            )))
            .map_err(map_write_err)?;
    }

    // 2. Top-level processing instructions and comments (kept above root).
    const TOP: &str = "at the top level";
    let mut non_ascii = FirstNonAscii::default();
    for s in item_texts(KEY_PI, TOP, top.get(KEY_PI))? {
        non_ascii.note(&s, || TOP.to_owned());
        writer
            .write_event(Event::PI(BytesPI::new(s.as_str())))
            .map_err(map_write_err)?;
    }
    for s in item_texts(KEY_COMMENTS, TOP, top.get(KEY_COMMENTS))? {
        non_ascii.note(&s, || TOP.to_owned());
        writer
            .write_event(Event::Comment(comment_event(&s)))
            .map_err(map_write_err)?;
    }

    // 3. Root element: the one non-conventional key. An XML document has
    // exactly one root, so a second element-shaped key is refused rather
    // than dropped.
    let mut element_keys = top.keys().filter(|k| !is_conventional(k));
    let root_tag = element_keys.next().map(String::as_str);
    if let Some(second) = element_keys.next()
        && let Some(first) = root_tag
    {
        return Err(Error::Format {
            format: "xml",
            message: format!(
                "XML write: a document has one root element; the top-level map holds '{first}' and '{second}'",
            ),
        });
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
    write_element(&mut writer, root_tag, root_body, 0, &mut non_ascii)?;

    if let Some(encoding) = decl.as_ref().and_then(|d| d.encoding.as_deref()) {
        check_declared_encoding(encoding, &non_ascii)?;
    }
    Ok(writer.into_inner().into_inner())
}

/// The first non-ASCII character the writer emitted and where: `in <tag>`
/// or `at the top level`.
#[derive(Default)]
struct FirstNonAscii(Option<(char, String)>);

impl FirstNonAscii {
    /// Remember the first non-ASCII character of `text`, unless one is
    /// already remembered. `place` is built only when one is found.
    fn note(&mut self, text: &str, place: impl FnOnce() -> String) {
        if self.0.is_none()
            && let Some(c) = text.chars().find(|c| !c.is_ascii())
        {
            self.0 = Some((c, place()));
        }
    }
}

/// dq writes UTF-8 whatever `#xml` declares, so a declared encoding must
/// name UTF-8 — or name an ASCII-compatible encoding (US-ASCII,
/// ISO-8859-*, windows-125x) of a document that is all ASCII, whose bytes
/// are the same in either. Anything else would label the bytes wrongly.
fn check_declared_encoding(encoding: &str, non_ascii: &FirstNonAscii) -> Result<()> {
    let lower = encoding.to_ascii_lowercase();
    if matches!(lower.as_str(), "utf-8" | "utf8") {
        return Ok(());
    }
    let ascii_compatible = matches!(lower.as_str(), "us-ascii" | "ascii")
        || lower.strip_prefix("iso-8859-").is_some_and(|n| {
            matches!(
                n,
                "1" | "2"
                    | "3"
                    | "4"
                    | "5"
                    | "6"
                    | "7"
                    | "8"
                    | "9"
                    | "10"
                    | "11"
                    | "13"
                    | "14"
                    | "15"
                    | "16"
            )
        })
        || lower
            .strip_prefix("windows-125")
            .is_some_and(|n| matches!(n, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8"));
    let message = match &non_ascii.0 {
        None if ascii_compatible => return Ok(()),
        None => format!("'{KEY_XML_DECL}' declares encoding {encoding:?} but dq writes UTF-8"),
        Some((c, place)) => format!(
            "'{KEY_XML_DECL}' declares encoding {encoding:?} but dq writes UTF-8 and the document \
             holds U+{:04X} {place}",
            u32::from(*c),
        ),
    };
    Err(Error::Format {
        format: "xml",
        message: format!("XML write: {message}"),
    })
}

/// Indent width for pretty-printed XML output (2 spaces per level).
///
/// Hardcoded — the value tree does not carry the original inter-element
/// whitespace, so on `parse → write` we always synthesise this layout.
const XML_INDENT_WIDTH: usize = 2;

/// Emit `\n` + `indent_level * XML_INDENT_WIDTH` spaces as a text event.
///
/// We pass the whitespace through `BytesText::from_escaped` so `quick-xml`
/// writes it verbatim (no entity escaping — there is nothing to escape).
/// Pure-whitespace text between element siblings parses back into
/// `Frame::text` but is discarded during `Frame::finalise` (because
/// `has_text` only flips on non-whitespace text), so round-trip stays
/// structurally stable.
fn write_indent_break(writer: &mut Writer<Cursor<Vec<u8>>>, indent_level: usize) -> Result<()> {
    let mut s = String::with_capacity(1 + indent_level * XML_INDENT_WIDTH);
    s.push('\n');
    for _ in 0..(indent_level * XML_INDENT_WIDTH) {
        s.push(' ');
    }
    writer
        .write_event(Event::Text(BytesText::from_escaped(s)))
        .map_err(map_write_err)
}

fn write_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    tag: &str,
    body: &IndexMap<String, Value>,
    indent_level: usize,
    non_ascii: &mut FirstNonAscii,
) -> Result<()> {
    check_name(tag, "an element")?;
    let place = || format!("in <{tag}>");
    non_ascii.note(tag, place);
    let mut start = BytesStart::new(tag);
    match body.get(KEY_ATTRS) {
        None | Some(Value::Map(_)) => {}
        Some(other) => {
            return Err(Error::Format {
                format: "xml",
                message: format!(
                    "XML write: '{KEY_ATTRS}' of <{tag}> must be a map of attributes (got {})",
                    other.type_name(),
                ),
            });
        }
    }
    if let Some(Value::Map(attrs)) = body.get(KEY_ATTRS) {
        for (k, v) in attrs {
            let v = match v {
                // An attribute is present or not: a null one is empty.
                Value::Null => String::new(),
                scalar => scalar_text(scalar).ok_or_else(|| Error::Format {
                    format: "xml",
                    message: format!(
                        "XML write: attribute '{k}' on <{tag}> must be a scalar (got {})",
                        scalar.type_name(),
                    ),
                })?,
            };
            check_name(k, &format!("an attribute (on <{tag}>)"))?;
            check_chars(&format!("attribute '{k}' on <{tag}>"), &v)?;
            non_ascii.note(k, place);
            non_ascii.note(&v, place);
            start.push_attribute(Attribute {
                key: QName(k.as_str()),
                value: escape_attr(&v).into_owned().into(),
            });
        }
    }

    // The text of a scalar `#text`: a number or a boolean as it is
    // spelled, a null as no text (an empty element). An array or a map has
    // no text: refuse it rather than drop it.
    let text = match body.get(KEY_TEXT) {
        None | Some(Value::Null) => None,
        Some(v) => Some(scalar_text(v).ok_or_else(|| Error::Format {
            format: "xml",
            message: format!(
                "XML write: '{KEY_TEXT}' of <{tag}> must be a scalar (got {})",
                v.type_name(),
            ),
        })?),
    };
    if let Some(text) = &text {
        check_chars(&format!("'{KEY_TEXT}' of <{tag}>"), text)?;
        non_ascii.note(text, place);
    }

    // Determine if the element has any body content. If not, emit as
    // self-closing `<tag/>`.
    let owner = format!("of <{tag}>");
    let comments = item_texts(KEY_COMMENTS, &owner, body.get(KEY_COMMENTS))?;
    let pis = item_texts(KEY_PI, &owner, body.get(KEY_PI))?;
    let cdata = item_texts(KEY_CDATA, &owner, body.get(KEY_CDATA))?;
    for s in comments.iter().chain(&pis).chain(&cdata) {
        non_ascii.note(s, place);
    }
    let has_text = text.is_some();
    let has_cdata = !cdata.is_empty();
    let has_pi = !pis.is_empty();
    let has_comments = !comments.is_empty();
    let has_children = body
        .iter()
        .any(|(k, v)| !is_conventional(k) && non_empty_array(v));

    if !has_text && !has_cdata && !has_pi && !has_comments && !has_children {
        writer
            .write_event(Event::Empty(start))
            .map_err(map_write_err)?;
        return Ok(());
    }

    // Pretty-print only when this element has child elements AND no text
    // content of its own. Leaf elements (`<name>Alice</name>`) stay inline,
    // and mixed-content elements (text + children, which are documented as
    // round-trip-lossy already) keep the existing compact layout so we
    // don't widen the lossy surface area.
    //
    // CDATA, PIs, and comments without element children also stay inline:
    // adding whitespace around them would still round-trip cleanly (pure
    // whitespace text events get dropped on re-parse), but the current
    // tests pin their inline form and there is no user-visible win from
    // changing it.
    let pretty = has_children && !has_text;
    let child_indent = indent_level + 1;

    writer
        .write_event(Event::Start(start))
        .map_err(map_write_err)?;

    // Comments first, then PIs, then text, then CDATA, then children.
    // Inner ordering is best-effort only; mixed content is documented as
    // lossy on round-trip (per spec D5).
    for s in &comments {
        if pretty {
            write_indent_break(writer, child_indent)?;
        }
        writer
            .write_event(Event::Comment(comment_event(s)))
            .map_err(map_write_err)?;
    }
    for s in &pis {
        if pretty {
            write_indent_break(writer, child_indent)?;
        }
        writer
            .write_event(Event::PI(BytesPI::new(s.as_str())))
            .map_err(map_write_err)?;
    }
    if let Some(s) = &text {
        writer
            .write_event(Event::Text(text_event(s)))
            .map_err(map_write_err)?;
    }
    for s in &cdata {
        // `]]>` would end the section early: quick-xml splits it across
        // adjacent sections (`]]]]><![CDATA[>`), the same text on re-read.
        for section in BytesCData::escaped(s) {
            writer
                .write_event(Event::CData(section))
                .map_err(map_write_err)?;
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
            if pretty {
                write_indent_break(writer, child_indent)?;
            }
            match child {
                Value::Map(child_body) => {
                    write_element(writer, k, child_body, child_indent, non_ascii)?;
                }
                Value::Null => {
                    // Empty child element.
                    let tmp = IndexMap::new();
                    write_element(writer, k, &tmp, child_indent, non_ascii)?;
                }
                // Scalar-shaped child: emitted as `<k>text</k>`. This shape
                // arises when JSON input is converted to XML —
                // `{"name": ["Alice"]}` becomes `<name>Alice</name>`.
                scalar @ (Value::String(_)
                | Value::Bool(_)
                | Value::Int(_)
                | Value::Float(_)
                | Value::BigInt(_)
                | Value::BigFloat(_)) => {
                    let mut tmp = IndexMap::new();
                    tmp.insert(KEY_TEXT.to_owned(), scalar.clone());
                    write_element(writer, k, &tmp, child_indent, non_ascii)?;
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

    // Close on its own line, indented to this element's level, when we
    // pretty-printed the body. Inline-emitted bodies (leaves, mixed content)
    // close immediately after the last inner event.
    if pretty {
        write_indent_break(writer, indent_level)?;
    }

    writer
        .write_event(Event::End(BytesEnd::new(tag)))
        .map_err(map_write_err)?;
    Ok(())
}

/// The text a scalar is written as: a string as is, a boolean or an
/// integer as it prints, a float its [shortest round-trip
/// spelling](crate::document::shortest_float) (`1.5e20`, `3.0`) — a NaN or
/// an infinity `.nan` / `.inf` / `-.inf` — and a big number as dq holds its
/// literal. `None` for a null, an array or a map.
fn scalar_text(v: &Value) -> Option<String> {
    if let Some(spelled) = v.non_finite_spelling() {
        return Some(spelled.to_owned());
    }
    match v {
        Value::String(s) | Value::BigInt(s) | Value::BigFloat(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Int(i) => Some(i.to_string()),
        Value::Float(f) => Some(crate::document::shortest_float(*f)),
        Value::Null | Value::Array(_) | Value::Map(_) => None,
    }
}

/// The texts of a `#comments` / `#pi` / `#cdata` entry, in order. The
/// reader always produces a list of strings; a tree from elsewhere may hold
/// any scalar there, written as its [`scalar_text`], and a lone scalar
/// counts as a one-item list. A null — the entry or an item — is no content.
/// A container, as the entry or as an item, is refused: it has no text.
///
/// `owner` completes the message: `of <tag>` or `at the top level`.
fn item_texts(key: &str, owner: &str, entry: Option<&Value>) -> Result<Vec<String>> {
    let items = match entry {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items.as_slice(),
        Some(lone) => std::slice::from_ref(lone),
    };
    let mut texts = Vec::with_capacity(items.len());
    for item in items {
        if matches!(item, Value::Null) {
            continue;
        }
        let text = scalar_text(item).ok_or_else(|| Error::Format {
            format: "xml",
            message: format!(
                "XML write: '{key}' {owner} must hold scalars (got {})",
                item.type_name(),
            ),
        })?;
        check_item(key, owner, &text)?;
        texts.push(text);
    }
    Ok(texts)
}

/// Refuse an item that cannot be written as well-formed XML of its kind —
/// never alter it. Every kind: a character XML 1.0 does not allow. A
/// comment: `--` anywhere or `-` at the end. A processing instruction: a
/// target (up to the first whitespace) that is not an NCName (an XML name
/// without a colon) or is `xml` in any case, or `?>` inside. CDATA has no such limit: `]]>` is split
/// across adjacent sections when written.
fn check_item(key: &str, owner: &str, text: &str) -> Result<()> {
    check_chars(&format!("'{key}' {owner}"), text)?;
    let refuse = |message: String| {
        Err(Error::Format {
            format: "xml",
            message,
        })
    };
    match key {
        KEY_COMMENTS if text.contains("--") => refuse(format!(
            "XML write: a comment in '{key}' {owner} contains '--', which XML does not allow in a comment",
        )),
        KEY_COMMENTS if text.ends_with('-') => refuse(format!(
            "XML write: a comment in '{key}' {owner} ends in '-', which XML does not allow in a comment",
        )),
        KEY_PI => {
            let target = text
                .split([' ', '\t', '\n', '\r'])
                .next()
                .unwrap_or_default();
            if !is_ncname(target) {
                let hint = if target.contains(':') {
                    ", a target holds no ':'"
                } else {
                    ""
                };
                refuse(format!(
                    "XML write: '{target}' is not a valid XML name for a processing-instruction \
                     target ('{key}' {owner}{hint})",
                ))
            } else if target.eq_ignore_ascii_case("xml") {
                refuse(format!(
                    "XML write: '{target}' cannot be a processing-instruction target, XML reserves \
                     it for the declaration ('{key}' {owner})",
                ))
            } else if text.contains("?>") {
                refuse(format!(
                    "XML write: a processing instruction in '{key}' {owner} contains '?>', which \
                     would end it early",
                ))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// Refuse `name` unless it is a namespace-well-formed `QName`: an NCName,
/// or two joined by one `:` (`svg:rect`, `xmlns:svg`). `what` says what it
/// names (`an element`, `an attribute (on <r>)`). Whether a prefix is
/// declared is not checked.
fn check_name(name: &str, what: &str) -> Result<()> {
    if is_qname(name) {
        return Ok(());
    }
    let hint = if name.contains(':') {
        " (a name holds at most one ':', between a prefix and a local name)"
    } else {
        ""
    };
    Err(Error::Format {
        format: "xml",
        message: format!("XML write: '{name}' is not a valid XML name for {what}{hint}"),
    })
}

/// Namespaces in XML 1.0 `QName`: `NCName` or `NCName ':' NCName`.
fn is_qname(name: &str) -> bool {
    match name.split_once(':') {
        None => is_ncname(name),
        Some((prefix, local)) => is_ncname(prefix) && is_ncname(local),
    }
}

/// Namespaces in XML 1.0 `NCName`: an XML `Name` without a colon.
fn is_ncname(name: &str) -> bool {
    !name.contains(':') && is_xml_name(name)
}

/// The XML 1.0 (fifth edition) `Name` production: a `NameStartChar`, then
/// `NameChar`s. A colon is a `NameStartChar` here; [`is_ncname`] drops it.
fn is_xml_name(name: &str) -> bool {
    let start = |c: char| {
        matches!(c,
            ':' | 'A'..='Z' | '_' | 'a'..='z'
            | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
            | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
            | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
    };
    let mut chars = name.chars();
    chars.next().is_some_and(start)
        && chars.all(|c| {
            start(c)
                || matches!(c,
                    '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
        })
}

/// Refuse `text` if it holds a character outside XML 1.0's `Char`: a C0
/// control other than tab, LF and CR, or U+FFFE / U+FFFF. XML has no escape
/// for them — a character reference to one is not well-formed either.
/// `what` names the place (`'#text' of <r>`).
fn check_chars(what: &str, text: &str) -> Result<()> {
    let allowed = |c: char| {
        matches!(c,
            '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
    };
    match text.chars().find(|&c| !allowed(c)) {
        None => Ok(()),
        Some(c) => Err(Error::Format {
            format: "xml",
            message: format!(
                "XML write: {what} holds U+{:04X}, a character XML 1.0 does not allow",
                u32::from(c),
            ),
        }),
    }
}

/// The fields of an `<?xml …?>` declaration.
struct Declaration {
    version: String,
    encoding: Option<String>,
    standalone: Option<String>,
}

/// The declaration a top-level `#xml` entry asks for: `None` without one
/// (absent or null). Each field is written as its [`scalar_text`] — a YAML
/// `version: 1.1` is a float and stays `1.1` — and a missing or null
/// `version` is `1.0`; a boolean `standalone` is `yes` / `no`. A non-map
/// entry, a key other than `version` / `encoding` / `standalone`, a
/// container as a field, and a field the declaration grammar does not allow
/// (`version` not `1.` and digits, `encoding` not an encoding name,
/// `standalone` not `yes` / `no`) are refused.
fn declaration(entry: Option<&Value>) -> Result<Option<Declaration>> {
    let decl = match entry {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Map(decl)) => decl,
        Some(other) => {
            return Err(Error::Format {
                format: "xml",
                message: format!(
                    "XML write: '{KEY_XML_DECL}' must be a map of declaration fields (got {})",
                    other.type_name(),
                ),
            });
        }
    };
    let mut fields = Declaration {
        version: "1.0".to_owned(),
        encoding: None,
        standalone: None,
    };
    for (k, v) in decl {
        let slot = match k.as_str() {
            "version" => None,
            "encoding" => Some(&mut fields.encoding),
            "standalone" => Some(&mut fields.standalone),
            _ => {
                return Err(Error::Format {
                    format: "xml",
                    message: format!(
                        "XML write: '{KEY_XML_DECL}' has no field '{k}' (only version, encoding, standalone)",
                    ),
                });
            }
        };
        if matches!(v, Value::Null) {
            continue;
        }
        let text = scalar_text(v).ok_or_else(|| Error::Format {
            format: "xml",
            message: format!(
                "XML write: '{KEY_XML_DECL}' field '{k}' must be a scalar (got {})",
                v.type_name(),
            ),
        })?;
        let text = match (k.as_str(), v) {
            // `standalone` takes `yes` / `no` only; a boolean is the same
            // answer.
            ("standalone", Value::Bool(b)) => if *b { "yes" } else { "no" }.to_owned(),
            ("standalone", _) if text != "yes" && text != "no" => {
                return Err(decl_field_error(k, "must be yes or no", &text));
            }
            ("version", _) if !is_version_num(&text) => {
                return Err(decl_field_error(k, "must be 1. and digits", &text));
            }
            ("encoding", _) if !is_enc_name(&text) => {
                return Err(decl_field_error(k, "is not an encoding name", &text));
            }
            _ => text,
        };
        match slot {
            Some(slot) => *slot = Some(text),
            None => fields.version = text,
        }
    }
    Ok(Some(fields))
}

fn decl_field_error(field: &str, rule: &str, got: &str) -> Error {
    Error::Format {
        format: "xml",
        message: format!("XML write: '{KEY_XML_DECL}' field '{field}' {rule} (got {got:?})"),
    }
}

/// XML 1.0 `VersionNum`: `1.` and one or more digits.
fn is_version_num(s: &str) -> bool {
    s.strip_prefix("1.")
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// XML 1.0 `EncName`: a Latin letter, then Latin letters, digits, `.`, `_`
/// and `-`.
fn is_enc_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A top-level value that carries nothing: a null or an empty container.
fn is_empty_content(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Map(map) => map.is_empty(),
        _ => false,
    }
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

/// Escape `raw` for a text body: `<`, `>`, `&`, `'` and `"` — the set
/// quick-xml's `escape` used up to 0.40, kept so output stays byte-stable —
/// and CR as `&#13;`. A raw CR in text reads back as LF (XML end-of-line
/// handling); the reference reads back as CR.
fn escape_text(raw: &str) -> Cow<'_, str> {
    escape_with(raw, |c| match c {
        '\r' => Some("&#13;"),
        _ => None,
    })
}

/// Escape `raw` for an attribute value: [`escape_text`]'s set plus LF and
/// tab as `&#10;` / `&#9;`. Attribute-value normalization reads a raw tab,
/// LF or CR back as a space; the references read back as themselves.
fn escape_attr(raw: &str) -> Cow<'_, str> {
    escape_with(raw, |c| match c {
        '\r' => Some("&#13;"),
        '\n' => Some("&#10;"),
        '\t' => Some("&#9;"),
        _ => None,
    })
}

/// Replace the five markup characters and whatever `extra` maps.
fn escape_with(raw: &str, extra: impl Fn(char) -> Option<&'static str>) -> Cow<'_, str> {
    let escaped = |c: char| match c {
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        '&' => Some("&amp;"),
        '\'' => Some("&apos;"),
        '"' => Some("&quot;"),
        other => extra(other),
    };
    let Some(first) = raw.find(|c| escaped(c).is_some()) else {
        return Cow::Borrowed(raw);
    };
    let mut out = String::with_capacity(raw.len() + 8);
    out.push_str(&raw[..first]);
    for c in raw[first..].chars() {
        match escaped(c) {
            Some(entity) => out.push_str(entity),
            None => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// `BytesText` for a text body, escaped with [`escape_text`].
fn text_event(s: &str) -> BytesText<'_> {
    BytesText::from_escaped(escape_text(s))
}

/// `BytesText` for a comment body, written as it is: XML has no escapes in
/// a comment — `&lt;` there is four characters — and the reader takes a
/// comment's text as written, so escaping would change it on every round
/// trip. [`check_item`] has refused what a comment cannot hold.
fn comment_event(s: &str) -> BytesText<'_> {
    BytesText::from_escaped(s)
}

fn map_write_err(e: std::io::Error) -> Error {
    // quick-xml 0.37 changed `Writer::write_event` to return
    // `std::io::Result<()>` instead of its own error type, so the closures
    // passed to `.map_err` now see a `std::io::Error`. We render the same
    // `XML write error: ...` text regardless — the only XML-write path that
    // can fail in practice is the `Cursor<Vec<u8>>` underneath, and surfacing
    // it as `Error::Format` keeps the error category stable for downstream
    // consumers.
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

    #[test]
    fn a_scalar_text_is_written_and_a_container_one_refused() {
        let element = |text: Value| {
            let mut body = IndexMap::new();
            body.insert(KEY_TEXT.to_owned(), text);
            let mut top = IndexMap::new();
            top.insert("x".to_owned(), Value::Array(vec![Value::Map(body)]));
            Value::Map(top)
        };
        for (text, xml) in [
            (Value::Float(1.5e20), "<x>1.5e20</x>"),
            (Value::Bool(false), "<x>false</x>"),
            (Value::Int(-2), "<x>-2</x>"),
            (Value::Null, "<x/>"),
        ] {
            assert_eq!(render(&element(text)), xml);
        }
        let err = render_xml(&element(Value::Array(vec![]))).expect_err("refused");
        assert!(
            err.to_string().contains("must be a scalar (got array)"),
            "{err}"
        );
    }

    #[test]
    fn scalar_items_are_written_and_a_container_item_refused() {
        let element = |key: &str, entry: Value| {
            let mut body = IndexMap::new();
            body.insert(key.to_owned(), entry);
            let mut top = IndexMap::new();
            top.insert("x".to_owned(), Value::Array(vec![Value::Map(body)]));
            Value::Map(top)
        };
        for (key, entry, xml) in [
            (
                KEY_CDATA,
                Value::Array(vec![Value::Float(3.0), Value::Null, Value::Int(1)]),
                "<x><![CDATA[3.0]]><![CDATA[1]]></x>",
            ),
            (KEY_COMMENTS, Value::Bool(true), "<x><!--true--></x>"),
            (KEY_PI, Value::Array(vec![Value::Null]), "<x/>"),
        ] {
            assert_eq!(render(&element(key, entry)), xml);
        }
        let err = render_xml(&element(KEY_PI, Value::Array(vec![Value::Array(vec![])])))
            .expect_err("refused");
        assert!(
            err.to_string()
                .contains("'#pi' of <x> must hold scalars (got array)"),
            "{err}"
        );
    }

    #[test]
    fn declaration_fields_are_scalar_text_and_a_foreign_one_refused() {
        let doc = |decl: Value| {
            let mut top = IndexMap::new();
            top.insert(KEY_XML_DECL.to_owned(), decl);
            top.insert(
                "r".to_owned(),
                Value::Array(vec![Value::Map(IndexMap::new())]),
            );
            Value::Map(top)
        };
        let mut decl = IndexMap::new();
        decl.insert("version".to_owned(), Value::Float(1.1));
        assert_eq!(
            render(&doc(Value::Map(decl.clone()))),
            "<?xml version=\"1.1\"?><r/>"
        );
        assert_eq!(render(&doc(Value::Null)), "<r/>");
        decl.insert("doctype".to_owned(), Value::String("html".to_owned()));
        let err = render_xml(&doc(Value::Map(decl))).expect_err("refused");
        assert!(err.to_string().contains("has no field 'doctype'"), "{err}");
        let err = render_xml(&doc(Value::Int(1))).expect_err("refused");
        assert!(err.to_string().contains("(got int)"), "{err}");
    }

    #[test]
    fn xml_names_follow_the_name_production() {
        for name in [
            "r", "_a", "svg:rect", "xmlns", "xmlns:p", "a-b.c1", "é", "a\u{B7}",
        ] {
            assert!(is_qname(name), "{name:?} is a QName");
        }
        for name in [
            "", "1x", "-a", ".a", "a b", "a\tb", "a/b", "\u{B7}a", ":a", "a:", "x:y:z", "a:1",
        ] {
            assert!(!is_qname(name), "{name:?} is not a QName");
        }
        assert!(is_xml_name("x:y:z") && !is_ncname("x:y") && is_ncname("t"));
    }

    #[test]
    fn declaration_grammar_accepts_only_its_own_values() {
        assert!(is_version_num("1.0") && is_version_num("1.10"));
        assert!(!is_version_num("1.") && !is_version_num("2.0") && !is_version_num("1.0a"));
        assert!(is_enc_name("UTF-8") && is_enc_name("iso_8859.1"));
        assert!(!is_enc_name("") && !is_enc_name("8bit") && !is_enc_name("a b"));
    }

    #[test]
    fn items_xml_cannot_hold_are_refused_and_cdata_split() {
        let owner = "of <r>";
        assert!(check_item(KEY_COMMENTS, owner, "a < b & c -x").is_ok());
        assert!(check_item(KEY_COMMENTS, owner, "a--b").is_err());
        assert!(check_item(KEY_COMMENTS, owner, "a-").is_err());
        assert!(check_item(KEY_PI, owner, "xml-stylesheet href='a'").is_ok());
        assert!(check_item(KEY_PI, owner, "t\tdata ? >").is_ok());
        for pi in ["xml v", "XML", "1t", "", " t", "t a?>"] {
            assert!(check_item(KEY_PI, owner, pi).is_err(), "{pi:?}");
        }
        assert!(check_item(KEY_CDATA, owner, "a]]>b").is_ok());
        let err = check_item(KEY_CDATA, owner, "\u{FFFE}").expect_err("refused");
        assert!(err.to_string().contains("holds U+FFFE"), "{err}");

        let mut body = IndexMap::new();
        body.insert(KEY_CDATA.to_owned(), Value::String("a]]>b]]>".to_owned()));
        let mut top = IndexMap::new();
        top.insert("r".to_owned(), Value::Array(vec![Value::Map(body)]));
        let xml = render(&Value::Map(top));
        assert_eq!(xml, "<r><![CDATA[a]]]]><![CDATA[>b]]]]><![CDATA[>]]></r>");
        let back = parse(&xml);
        let body = root_element_body(&back, "r");
        let Some(Value::Array(sections)) = body.get(KEY_CDATA) else {
            panic!("#cdata")
        };
        let text: String = sections
            .iter()
            .map(|s| match s {
                Value::String(s) => s.as_str(),
                _ => panic!("string section"),
            })
            .collect();
        assert_eq!(text, "a]]>b]]>");
    }

    #[test]
    fn a_declared_encoding_must_agree_with_utf8_bytes() {
        let ascii = FirstNonAscii::default();
        let mut accented = FirstNonAscii::default();
        accented.note("café", || "in <r>".to_owned());
        for enc in ["UTF-8", "utf8", "Utf-8"] {
            assert!(check_declared_encoding(enc, &accented).is_ok(), "{enc}");
        }
        for enc in [
            "US-ASCII",
            "iso-8859-15",
            "ISO-8859-1",
            "windows-1250",
            "Windows-1258",
        ] {
            assert!(check_declared_encoding(enc, &ascii).is_ok(), "{enc}");
            assert!(check_declared_encoding(enc, &accented).is_err(), "{enc}");
        }
        for enc in [
            "UTF-16",
            "ISO-8859-12",
            "ISO-8859-01",
            "windows-1259",
            "Shift_JIS",
        ] {
            assert!(check_declared_encoding(enc, &ascii).is_err(), "{enc}");
        }
        let err = check_declared_encoding("US-ASCII", &accented).expect_err("refused");
        assert!(err.to_string().ends_with("holds U+00E9 in <r>"), "{err}");
    }

    #[test]
    fn whitespace_xml_would_normalise_is_escaped() {
        assert_eq!(escape_text("a\tb\nc\rd<"), "a\tb\nc&#13;d&lt;");
        assert_eq!(escape_attr("a\tb\nc\rd\""), "a&#9;b&#10;c&#13;d&quot;");
        assert!(matches!(escape_attr("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_second_root_and_top_level_text_are_refused() {
        let empty = || Value::Array(vec![Value::Map(IndexMap::new())]);
        let mut top = IndexMap::new();
        top.insert("a".to_owned(), empty());
        top.insert("b".to_owned(), empty());
        let err = render_xml(&Value::Map(top)).expect_err("refused");
        assert!(err.to_string().contains("holds 'a' and 'b'"), "{err}");

        let mut top = IndexMap::new();
        top.insert(KEY_CDATA.to_owned(), Value::Array(vec![Value::Int(1)]));
        top.insert("r".to_owned(), empty());
        let err = render_xml(&Value::Map(top)).expect_err("refused");
        assert!(
            err.to_string()
                .contains("'#cdata' cannot stand at the top level"),
            "{err}"
        );
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
    fn parse_predefined_entity_references_are_resolved_into_text() {
        // quick-xml 0.38+ surfaces `&amp;` / `&lt;` / `&gt;` / `&quot;` /
        // `&apos;` as separate `Event::GeneralRef` events instead of
        // expanding them inside the surrounding `Event::Text`. Pin the
        // pre-0.38 behaviour: the entities must be folded back into the
        // element body so callers see the resolved text in `#text`.
        let v = parse("<msg>A &amp; B &lt; C &gt; D &quot;E&quot; &apos;F&apos;</msg>");
        let body = root_element_body(&v, "msg");
        let Some(Value::String(text)) = body.get(KEY_TEXT) else {
            panic!("expected #text on <msg>")
        };
        assert_eq!(
            text, "A & B < C > D \"E\" 'F'",
            "predefined entities must be resolved into the text body",
        );
    }

    #[test]
    fn parse_numeric_character_reference_is_resolved() {
        // Numeric character refs (`&#48;` decimal, `&#x30;` hex) take the
        // separate `GeneralRef::is_char_ref` branch of the resolver. Both
        // forms must resolve to the same ASCII '0'.
        let v = parse("<m>x&#48;y&#x30;z</m>");
        let body = root_element_body(&v, "m");
        let Some(Value::String(text)) = body.get(KEY_TEXT) else {
            panic!("expected #text on <m>")
        };
        assert_eq!(
            text, "x0y0z",
            "decimal and hex character refs must resolve to '0'",
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

    #[test]
    fn write_xml_indents_nested_elements() {
        // Bug #9 regression: the writer must pretty-print nested element
        // structure with a `\n` + 2-space indent per level so that
        // `dq fmt` on a multi-line XML file no longer collapses to a
        // single line. Pin the literal layout of the indent markers for
        // the deepest leaf (`<c>`) to avoid silently regressing back to
        // back-to-back tag emission.
        let v = parse("<a><b><c>v</c></b></a>");
        let out = render(&v);
        assert!(
            out.contains("\n  <b>"),
            "child `<b>` of root must be on its own line indented 2 spaces; got: {out:?}",
        );
        assert!(
            out.contains("\n    <c>v</c>"),
            "leaf `<c>` must sit one level deeper (4 spaces) and stay inline; got: {out:?}",
        );
        assert!(
            out.contains("\n  </b>"),
            "closing `</b>` must align with its opening tag at depth 1; got: {out:?}",
        );
        assert!(
            out.ends_with("</a>") || out.ends_with("</a>\n"),
            "root close must end the document; got: {out:?}",
        );
    }

    #[test]
    fn write_xml_leaf_element_stays_inline() {
        // Leaf elements (#text only, no child elements) must NOT get
        // synthetic indentation. `<name>Alice</name>` round-trips with no
        // whitespace inserted between the open tag, the text, and the
        // close tag — anything else would corrupt the text body.
        //
        // We synthesise the value directly rather than parsing because
        // `<name>...</name>` alone is a complete document — exactly the
        // shape the writer's leaf branch handles.
        let mut top = IndexMap::new();
        let mut leaf = IndexMap::new();
        leaf.insert(KEY_TEXT.to_owned(), Value::String("Alice".into()));
        top.insert("name".into(), Value::Array(vec![Value::Map(leaf)]));
        let out = render(&Value::Map(top));
        assert!(
            out.contains("<name>Alice</name>"),
            "leaf element must stay inline with no inter-tag whitespace; got: {out:?}",
        );
        assert!(
            !out.contains("\n  Alice") && !out.contains("Alice\n"),
            "no newline/indent must surround the leaf text body; got: {out:?}",
        );
    }

    #[test]
    fn write_xml_root_with_two_children() {
        // Two same-tag children of the root must each land on their own
        // line, indented 2 spaces, with the closing root tag flush-left.
        let v = parse("<list><item>A</item><item>B</item></list>");
        let out = render(&v);
        assert!(
            out.contains("\n  <item>A</item>"),
            "first child must start on a fresh line indented 2 spaces; got: {out:?}",
        );
        assert!(
            out.contains("\n  <item>B</item>"),
            "second child must start on a fresh line indented 2 spaces; got: {out:?}",
        );
        assert!(
            out.contains("\n</list>"),
            "closing `</list>` must sit on its own line at depth 0; got: {out:?}",
        );
    }

    #[test]
    fn write_xml_mixed_attributes_and_children() {
        // Attributes on a parent element must coexist with the pretty-
        // printed child layout — the attribute stays on the open tag and
        // the self-closing child still goes on its own indented line.
        let v = parse(r#"<a id="1"><b/></a>"#);
        let out = render(&v);
        assert!(
            out.contains(r#"<a id="1">"#),
            "attribute must survive on the open tag; got: {out:?}",
        );
        assert!(
            out.contains("\n  <b/>") || out.contains("\n  <b />"),
            "self-closing child `<b/>` must be indented 2 spaces on its own line; got: {out:?}",
        );
        assert!(
            out.contains("\n</a>"),
            "closing `</a>` must be flush-left on its own line; got: {out:?}",
        );
    }
}
