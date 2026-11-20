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
//! | Byte order mark                        | `"bom": "UTF-8"` / `"UTF-16LE"` / `"UTF-16BE"` in `"#xml"` (only `bom` there: no declaration) |
//! | UTF-16 with no byte order mark         | `"detected": "UTF-16LE"` / `"UTF-16BE"` in `"#xml"`, unless the declaration names the byte order |
//! | 8-bit bytes under a UTF-16 label       | `"detected": "UTF-8"` in `"#xml"`                                   |
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
//! `standalone` / `bom` / `detected`, a second root element, and `@attrs` / `#text` / `#cdata`
//! with content at the top level, outside the root.
//!
//! The writer never emits XML that is not well-formed, and never alters
//! content to make it so. It refuses: an element or attribute name that is
//! not a namespace `QName` (one `prefix:` at most), a PI target that is not
//! an `NCName` or is `xml` in any case, a character XML 1.0 does not allow
//! (a C0 control other than tab, LF, CR), a comment with `--` or a trailing
//! `-`, a PI with `?>`, declaration fields outside its grammar, and a
//! character the declared encoding cannot hold (see Encodings below). A
//! boolean `standalone` is `yes` / `no`. CDATA
//! holding `]]>` is split across adjacent sections, which read back as
//! separate `#cdata` items with the same text. Comments are written as they
//! are: XML has no escapes inside a comment. A CR in text and a tab, LF or
//! CR in an attribute value are written as character references, so XML's
//! end-of-line and attribute normalization give them back unchanged.
//!
//! Names are checked against Namespaces in XML 1.0 too: a prefix must be
//! declared by an `xmlns:p` on the element or an ancestor (`xml` always
//! is), `xmlns` names only declarations, `xmlns:xml` binds only its fixed
//! URI, no other declaration binds the `xml` or `xmlns` namespace URI, and
//! `xmlns:p=""` (undeclaring) is refused unless the declaration says
//! `version="1.1"`: Namespaces in XML 1.1 allow it, and the prefix is then
//! undeclared within the element. The
//! reader refuses the same, as a parse error: an undeclared prefix on an
//! element (`<a:b/>`) or an attribute (`<r a:x="1"/>`), and a declaration
//! the writer would refuse.
//!
//! The XML declaration is read by its grammar (XML 1.0 §2.8), as libxml2
//! reads it: `version`, then an optional `encoding`, then an optional
//! `standalone`, each once and after white space, `version` `1.` and
//! digits, `encoding` an encoding name, `standalone` `yes` or `no`. A
//! declaration anywhere but the very start of the document is a parse
//! error, and so is a processing instruction named `xml` in any case
//! (`<?XML …?>`), which the writer refuses too.
//!
//! quick-xml reads some ill-formed documents without an error; the reader
//! refuses them itself. A document is one root element with only the
//! declaration, a DOCTYPE, comments, PIs and white space before it and
//! comments, PIs and white space after it (XML 1.0 §2.1): a second root,
//! text, CDATA or a reference outside the root, a misplaced or second
//! DOCTYPE, and an element the input leaves open are parse errors. So are
//! `]]>` in text (§2.4), a raw `<` in an attribute value (§3.1), and the
//! names the writer refuses: an element or attribute name that is no
//! `QName`, and a PI target that is no `NCName`.
//!
//! ## Encodings
//!
//! The reader decodes a file per its byte order mark, else per the
//! declaration's `encoding` when that declaration reads in ASCII
//! (US-ASCII, ISO-8859-1, windows-1252, Shift_JIS, ISO-2022-JP, …), else
//! as UTF-8; it refuses a label dq does not know (`UTF-32`) or cannot read
//! (`ISO-2022-KR`), as the writer does. The writer encodes its output in the
//! encoding `#xml` declares (UTF-8 without one), beginning with the BOM
//! `bom` names — UTF-16 always has one, little-endian by default — and
//! refuses a label dq does
//! not know, a character the encoding cannot hold, and a `bom` the label
//! contradicts. A UTF-16 file with no BOM — told by its leading `<?` — is
//! kept as `detected` and written back in its byte order with no BOM. A
//! file in 8-bit bytes that declares UTF-16 (.NET's `XmlSerializer` over a
//! `StringWriter` writes `encoding="utf-16"` into UTF-8) reads as UTF-8 and
//! is kept as `detected: UTF-8`, written back as UTF-8 under its label. The
//! writer refuses `detected` together with `bom`, one the label
//! contradicts (`UTF-8` only under a UTF-16 label), and one whose output
//! would not begin with `<?`. Labels are
//! encoding_rs's (the WHATWG Encoding
//! Standard) except two, which follow IANA and the XML spec as other XML
//! parsers do: `US-ASCII` (`ASCII`, …) is strict 7-bit — a byte from 0x80
//! up is a parse error — and `ISO-8859-1` (`latin1`, `l1`, …) maps byte to
//! code point, 0x80–0x9F being C1 controls. WHATWG would read both as
//! windows-1252, which keeps its own mapping under its own labels. The
//! reader refuses an `encoding` that is not an encoding name and one the
//! BOM, or the UTF-16 a file with no BOM begins in, contradicts (a UTF-16
//! BOM declaring `ISO-8859-1`, XML 1.0 §4.3.3), as the writer does.
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
use encoding_rs::{Encoding, REPLACEMENT, UTF_8, UTF_16BE, UTF_16LE};
use indexmap::IndexMap;
use quick_xml::events::attributes::{AttrError, Attribute};
use quick_xml::events::{
    BytesCData, BytesDecl, BytesEnd, BytesPI, BytesRef, BytesStart, BytesText, Event,
};
use quick_xml::name::QName;
use quick_xml::reader::Reader;
use quick_xml::{Writer, XmlVersion};

use crate::Result;
use crate::document::{Document, FormatTag, Layout, Value};
use crate::error::Error;
use crate::format::Format;

/// Conventional key for the XML declaration block at the top level.
const KEY_XML_DECL: &str = "#xml";
/// `#xml` field naming the byte order mark the file began with.
const KEY_BOM: &str = "bom";
/// `#xml` field naming the encoding told from the bytes where neither a
/// byte order mark nor the declaration names it: the UTF-16 byte order of
/// a file with no BOM whose declaration names no byte order, or `UTF-8`
/// for a file in 8-bit bytes that declares UTF-16.
const KEY_DETECTED: &str = "detected";
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
        &["xml", "pom", "xsd"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let value = parse_xml(bytes).map_err(with_position)?;
        // The line break the file uses, from its first: decoded, as the
        // file may be UTF-16.
        let crlf = decode_input(bytes)
            .is_ok_and(|(text, ..)| text.find('\n').is_some_and(|at| text[..at].ends_with('\r')));
        Ok(Document::value_only(value, FormatTag::Xml)
            .with_layout(Layout::Xml(XmlLayout { crlf }))
            .with_source_bytes(bytes))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        let crlf = matches!(doc.layout(), Layout::Xml(XmlLayout { crlf: true }));
        let buf = render_xml(doc.value(), crlf)?;
        w.write_all(&buf).map_err(|source| Error::WriteIo {
            path: Utf8PathBuf::from("<xml-writer>"),
            source,
        })
    }
}

/// What the XML reader saw of a file beyond its value tree
/// ([`Layout::Xml`]): whether its lines end in `\r\n`, which the writer
/// then ends the lines it writes with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct XmlLayout {
    crlf: bool,
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
    /// Where its start tag begins (its `<`), to place an element the
    /// input leaves open.
    start: usize,
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
    /// The namespace prefixes this element's `xmlns:p` attributes declare
    /// (`true`) or, in XML 1.1, undeclare with `xmlns:p=""` (`false`).
    prefixes: Vec<(String, bool)>,
}

impl Frame {
    fn new(tag: String, start: usize) -> Self {
        Self {
            tag,
            start,
            attrs: IndexMap::new(),
            children: IndexMap::new(),
            text: String::new(),
            has_text: false,
            cdata: Vec::new(),
            pi: Vec::new(),
            comments: Vec::new(),
            saw_element: false,
            saw_mixed: false,
            prefixes: Vec::new(),
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
    let (text, bom, detected) = decode_input(bytes)?;
    let text: &str = &text;
    // The characters a document may hold depend on its version, which
    // its declaration, ahead of every other character, gives.
    // Its line ends, and its namespaces (Namespaces in XML 1.1 for a 1.1
    // document only), too.
    let declaration =
        read_declaration(text.as_bytes()).map_err(|e| declaration_error(text.as_bytes(), e))?;
    let xml11 = declaration.is_some_and(|decl| decl.version == "1.1");
    check_raw_chars(text, xml11)?;
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
    // The root element's name, once its start tag is read: past it, only
    // comments, PIs and white space may stand at the top level (XML 1.0
    // §2.1, `document ::= prolog element Misc*`).
    let mut root: Option<String> = None;
    let mut doctype = false;

    loop {
        // Where the next event begins: its `<` (or `&`), or the first
        // character of a text.
        let event_start = reader.buffer_position() as usize;
        match reader.read_event() {
            Err(e) => {
                // The start of the markup the error is in (its `<`), where
                // `buffer_position` would be wherever reading stopped.
                let (pos, message) = reader_refusal(&e, text, reader.error_position() as usize);
                return Err(parse_error_at(text, pos, message));
            }
            Ok(Event::Eof) => {
                // quick-xml ends on `Eof` with elements still open; the
                // innermost is the one the input leaves unclosed first.
                if let Some(frame) = stack.last() {
                    return Err(parse_error_at(
                        text,
                        frame.start,
                        format!("element <{}> is not closed", frame.tag),
                    ));
                }
                break;
            }
            Ok(Event::Decl(_)) => {
                // The declaration `read_declaration` read, the document's
                // first characters. One anywhere else — after white space,
                // a comment or the root, a second one, one inside an
                // element — is not well-formed (XML 1.0 §2.8).
                let Some(decl) = declaration.filter(|_| event_start == 0) else {
                    return Err(parse_error_at(
                        text,
                        event_start,
                        "an XML declaration may stand only at the very start of the document"
                            .to_owned(),
                    ));
                };
                let mut decl_map: IndexMap<String, Value> = IndexMap::new();
                let fields = [
                    ("version", Some(decl.version)),
                    ("encoding", decl.encoding),
                    ("standalone", decl.standalone),
                ];
                for (key, value) in fields {
                    if let Some(value) = value {
                        decl_map.insert(key.to_owned(), Value::String(value.to_owned()));
                    }
                }
                top.insert(KEY_XML_DECL.to_owned(), Value::Map(decl_map));
            }
            Ok(Event::DocType(_)) => {
                // One DOCTYPE, in the prolog (XML 1.0 §2.8).
                let refusal = if let Some(frame) = stack.last() {
                    Some(format!(
                        "a DOCTYPE declaration inside element <{}>; it may stand only before \
                         the root element",
                        frame.tag
                    ))
                } else if root.is_some() {
                    Some(outside_root("a DOCTYPE declaration", root.as_deref()))
                } else if doctype {
                    Some("a second DOCTYPE declaration; a document has at most one".to_owned())
                } else {
                    None
                };
                if let Some(message) = refusal {
                    return Err(parse_error_at(text, event_start, message));
                }
                doctype = true;
                // M11 v1: DOCTYPE declarations are not preserved on
                // round-trip. Document the omission via tracing so the user
                // is aware before relying on this for DTD-bearing files.
                tracing::warn!(
                    "XML parse: <!DOCTYPE ...> declaration found but not preserved on round-trip",
                );
            }
            Ok(Event::PI(pi)) => {
                // A target is an `NCName` (XML 1.0 §2.6, Namespaces in XML
                // §7), right after the `<?`.
                let target = pi.target();
                if !is_ncname(target) {
                    let message = if target.is_empty() {
                        "a processing instruction must name its target right after '<?'".to_owned()
                    } else {
                        let hint = if target.contains(':') {
                            " (a target holds no ':')"
                        } else {
                            ""
                        };
                        format!(
                            "'{target}' is not a valid XML name for a processing-instruction \
                             target{hint}"
                        )
                    };
                    return Err(parse_error_at(text, event_start + "<?".len(), message));
                }
                // `<?XML …?>` and the like: XML reserves the target, in any
                // case, for the declaration (§2.6), and the writer refuses it.
                if pi.target().eq_ignore_ascii_case("xml") {
                    return Err(parse_error_at(
                        text,
                        event_start + "<?".len(),
                        format!(
                            "'{}' cannot be a processing-instruction target, XML reserves it \
                             for the declaration",
                            pi.target()
                        ),
                    ));
                }
                let raw = normalize_line_ends(&pi, xml11);
                push_pi(&mut stack, &mut top_pi, raw);
            }
            Ok(Event::Comment(c)) => {
                let txt = decode_text(&c, xml11);
                push_comment(&mut stack, &mut top_comments, txt);
            }
            Ok(Event::Start(start)) => {
                check_names(text, &start, event_start)?;
                if stack.is_empty() {
                    take_root(&mut root, &start, text, event_start)?;
                }
                check_attribute_values(text, &start, xml11)?;
                let mut frame = build_start_frame(&start, text, event_start, xml11)?;
                check_namespaces(&stack, &mut frame, xml11).map_err(|(message, attr)| {
                    let pos = name_offset(text, &start, event_start, attr.as_deref());
                    parse_error_at(text, pos, message)
                })?;
                if let Some(parent) = stack.last_mut() {
                    parent.saw_element = true;
                    if parent.has_text {
                        parent.saw_mixed = true;
                    }
                }
                stack.push(frame);
            }
            Ok(Event::Empty(start)) => {
                check_names(text, &start, event_start)?;
                if stack.is_empty() {
                    take_root(&mut root, &start, text, event_start)?;
                }
                check_attribute_values(text, &start, xml11)?;
                let mut frame = build_start_frame(&start, text, event_start, xml11)?;
                check_namespaces(&stack, &mut frame, xml11).map_err(|(message, attr)| {
                    let pos = name_offset(text, &start, event_start, attr.as_deref());
                    parse_error_at(text, pos, message)
                })?;
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
                    let pos = event_start;
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
                    let pos = event_start;
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
                if stack.is_empty() {
                    // Only white space may stand between top-level
                    // constructs: XML's four characters, and in 1.1 the
                    // NEL and LS its end-of-line handling makes LFs.
                    let raw: &str = &t;
                    let other = raw.char_indices().find(|&(_, c)| {
                        !(matches!(c, ' ' | '\t' | '\r' | '\n')
                            || xml11 && matches!(c, '\u{85}' | '\u{2028}'))
                    });
                    if let Some((offset, _)) = other {
                        return Err(parse_error_at(
                            text,
                            event_start + offset,
                            outside_root("text", root.as_deref()),
                        ));
                    }
                    continue;
                }
                // `]]>` only ends a CDATA section (XML 1.0 §2.4). A
                // reference cannot hold it, so it stands within one text.
                let raw: &str = &t;
                if let (Some(offset), Some(frame)) = (raw.find("]]>"), stack.last()) {
                    return Err(parse_error_at(
                        text,
                        event_start + offset,
                        format!(
                            "the text of <{}> holds ']]>', which XML allows only to end a CDATA \
                             section; write its '>' as '&gt;'",
                            frame.tag
                        ),
                    ));
                }
                let s = decode_text(&t, xml11);
                if let Some(frame) = stack.last_mut() {
                    if !is_xml_whitespace(&s) {
                        if frame.saw_element {
                            frame.saw_mixed = true;
                        }
                        frame.has_text = true;
                    }
                    frame.text.push_str(&s);
                }
            }
            Ok(Event::CData(c)) => {
                if stack.is_empty() {
                    return Err(parse_error_at(
                        text,
                        event_start,
                        outside_root("a CDATA section", root.as_deref()),
                    ));
                }
                // CData payload is stored verbatim (no escape decoding —
                // CDATA bodies are not entity-encoded), but for its line
                // ends.
                let s = c.xml_content(xml_version(xml11)).into_owned();
                if let Some(frame) = stack.last_mut() {
                    frame.cdata.push(s);
                }
            }
            Ok(Event::GeneralRef(r)) => {
                // quick-xml 0.38+ reports `&amp;`, `&lt;`, etc. as a separate
                // event instead of expanding them inside `Event::Text`. Resolve
                // the reference here and append to the current frame's text so
                // the parsed `Value` matches the pre-0.38 behaviour (entities
                // are folded into the surrounding text body).
                if stack.is_empty() {
                    let name: &str = &r;
                    return Err(parse_error_at(
                        text,
                        event_start,
                        outside_root(&format!("the reference '&{name};'"), root.as_deref()),
                    ));
                }
                let s = resolve_general_ref(&r, text, event_start, xml11)?;
                if let Some(frame) = stack.last_mut() {
                    if frame.saw_element {
                        frame.saw_mixed = true;
                    }
                    frame.has_text = true;
                    frame.text.push_str(&s);
                }
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
    // Well-formed XML requires exactly one root element: a second one is
    // refused where it begins (`take_root`), no root at the end of input.
    if root.is_none() {
        return Err(parse_error_at(
            text,
            reader.buffer_position() as usize,
            "a document must have exactly one root element; found 0".to_owned(),
        ));
    }
    for (name, group) in top_children {
        top.insert(name, Value::Array(group));
    }
    for (key, mark) in [(KEY_BOM, bom), (KEY_DETECTED, detected)] {
        let Some(mark) = mark else {
            continue;
        };
        let label = Value::String(mark.label().to_owned());
        match top.get_mut(KEY_XML_DECL) {
            Some(Value::Map(decl)) => {
                decl.insert(key.to_owned(), label);
            }
            _ => {
                let decl = IndexMap::from([(key.to_owned(), label)]);
                top.shift_insert(0, KEY_XML_DECL.to_owned(), Value::Map(decl));
            }
        }
    }
    Ok(Value::Map(top))
}

/// Refuse the name of `start`, a start or empty-element tag at
/// `event_start` in `text`, or of one of its attributes, at the name, when
/// it is no namespace-well-formed `QName` ([`name_refusal`]): the writer
/// would refuse it.
fn check_names(text: &str, start: &BytesStart<'_>, event_start: usize) -> Result<()> {
    let name = start.name();
    let tag: &str = name.as_ref();
    if tag.is_empty() {
        return Err(parse_error_at(
            text,
            event_start + "<".len(),
            "a tag must begin with its name right after '<'".to_owned(),
        ));
    }
    if let Some(message) = name_refusal(tag, "an element") {
        return Err(parse_error_at(text, event_start + "<".len(), message));
    }
    for attr in start.attributes().flatten() {
        let key: &str = attr.key.as_ref();
        if let Some(message) = name_refusal(key, &format!("an attribute (on <{tag}>)")) {
            let pos = name_offset(text, start, event_start, Some(key));
            return Err(parse_error_at(text, pos, message));
        }
    }
    Ok(())
}

/// Record `start`, a start or empty-element tag at the top level (at
/// `event_start` in `text`), as the root element, or refuse it as a second
/// one.
fn take_root(
    root: &mut Option<String>,
    start: &BytesStart<'_>,
    text: &str,
    event_start: usize,
) -> Result<()> {
    let name = start.name();
    let tag: &str = name.as_ref();
    if let Some(first) = root {
        return Err(parse_error_at(
            text,
            event_start,
            format!(
                "a document must have exactly one root element; <{tag}> is a second one, after <{first}>"
            ),
        ));
    }
    *root = Some(tag.to_owned());
    Ok(())
}

/// Why `what` (`text`, `a CDATA section`) cannot stand at the top level,
/// before the root element or after `root`.
fn outside_root(what: &str, root: Option<&str>) -> String {
    match root {
        Some(root) => format!(
            "{what} after the root element <{root}>; only comments, processing instructions \
             and white space may follow it"
        ),
        None => format!(
            "{what} before the root element; only the XML declaration, a DOCTYPE declaration, \
             comments, processing instructions and white space may precede it"
        ),
    }
}

/// The byte order mark an XML file began with. The reader keeps it as the
/// `#xml` field `bom` (`UTF-8` / `UTF-16LE` / `UTF-16BE`) — the one thing
/// the tree needs to write the file back byte for byte — and the writer
/// begins its output with it.
///
/// A UTF-16 file with no BOM, which begins `<?` in 16-bit units, has its
/// byte order kept the same way as the `#xml` field `detected`, unless the
/// declaration names it (`UTF-16LE`); the writer writes UTF-16 in that
/// order, with no BOM. A file in 8-bit bytes under a UTF-16 label is kept
/// as `detected: UTF-8`, and written back as UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bom {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl Bom {
    fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
        }
    }

    /// Whether a file beginning with this BOM may declare `label`, which
    /// names `charset`: a UTF-8 BOM only UTF-8, a UTF-16 one only UTF-16 —
    /// and `UTF-16LE` / `UTF-16BE` only in the BOM's byte order (XML 1.0
    /// §4.3.3 makes anything else a fatal error).
    fn admits(self, charset: Charset, label: &str) -> bool {
        let Charset::Whatwg(encoding) = charset else {
            return false;
        };
        match self {
            Self::Utf8 => encoding == UTF_8,
            Self::Utf16Le | Self::Utf16Be => {
                (encoding == UTF_16LE || encoding == UTF_16BE)
                    && (!names_utf16_byte_order(label)
                        || (self == Self::Utf16Be) == (encoding == UTF_16BE))
            }
        }
    }
}

/// Whether `label` is `UTF-16LE` / `UTF-16BE`, which fix the byte order,
/// rather than a UTF-16 label that leaves it to the BOM.
fn names_utf16_byte_order(label: &str) -> bool {
    label.eq_ignore_ascii_case("utf-16le") || label.eq_ignore_ascii_case("utf-16be")
}

/// The text of an XML file, the BOM it began with, and — for UTF-16 with
/// no BOM whose declaration names no byte order — the byte order it was
/// read in ([`Bom::Utf16Le`] / [`Bom::Utf16Be`]). The encoding is the
/// BOM's, else UTF-16 when the file starts `<?` in 16-bit units, else the
/// one the declaration names if its declaration reads in ASCII (US-ASCII,
/// ISO-8859-1, windows-1252, Shift_JIS, ISO-2022-JP, …), else UTF-8. A
/// UTF-16 label over 8-bit bytes reads as UTF-8. A parse error: an
/// `encoding` that is not an encoding name (` ISO-8859-1 `), one dq does
/// not know (`UTF-32`, `x-foo`) or cannot read (`ISO-2022-KR`, which
/// encoding_rs decodes to nothing but U+FFFD), and one the BOM, or the
/// UTF-16 the file begins in, contradicts. The writer refuses the same
/// labels.
fn decode_input(bytes: &[u8]) -> Result<(Cow<'_, str>, Option<Bom>, Option<Bom>)> {
    use quick_xml::encoding::{DetectedEncoding, detect_encoding};
    let (bom, detected, body, utf16) = match detect_encoding(bytes) {
        Some(DetectedEncoding::Utf16LeBom) => {
            (Some(Bom::Utf16Le), None, &bytes[2..], Some(UTF_16LE))
        }
        Some(DetectedEncoding::Utf16BeBom) => {
            (Some(Bom::Utf16Be), None, &bytes[2..], Some(UTF_16BE))
        }
        Some(DetectedEncoding::Utf8Bom) => (Some(Bom::Utf8), None, &bytes[3..], None),
        Some(DetectedEncoding::Utf16LeLike) => (None, Some(Bom::Utf16Le), bytes, Some(UTF_16LE)),
        Some(DetectedEncoding::Utf16BeLike) => (None, Some(Bom::Utf16Be), bytes, Some(UTF_16BE)),
        _ => (None, None, bytes, None),
    };
    let invalid = |message: String| Error::Parse {
        file: None,
        line: 0,
        col: 0,
        span: 0..0,
        snippet: String::new(),
        message,
    };
    if let Some(encoding) = utf16 {
        let text = quick_xml::encoding::decode(body, encoding)
            .map_err(|_| invalid(format!("invalid {} in XML input", encoding.name())))?;
        let label = declared_label(text.as_bytes(), bom, detected)?;
        // `UTF-16LE` / `UTF-16BE` keep the byte order themselves.
        let detected = detected.filter(|_| !label.is_some_and(names_utf16_byte_order));
        return Ok((text, bom, detected));
    }
    // Every 8-bit encoding read here spells the declaration in ASCII.
    let label = declared_label(body, bom, detected)?;
    // A UTF-16 label over 8-bit bytes reads as UTF-8, by the bytes, and
    // says so in `detected`. ISO-2022-JP is not ASCII-compatible to
    // encoding_rs, but it starts in ASCII, so its declaration reads as
    // written.
    let declared = label.filter(|_| bom.is_none()).and_then(Charset::for_label);
    let detected = declared
        .filter(|charset| charset.is_utf16())
        .map(|_| Bom::Utf8);
    let charset = declared
        .filter(|charset| !charset.is_utf16())
        .unwrap_or(Charset::Whatwg(UTF_8));
    let text = match charset {
        Charset::Ascii => {
            if let Some(index) = body.iter().position(|b| !b.is_ascii()) {
                return Err(invalid(format!(
                    "invalid US-ASCII in XML input: byte 0x{:02X} at index {index}",
                    body[index],
                )));
            }
            let text = std::str::from_utf8(body)
                .map_err(|e| invalid(format!("invalid US-ASCII in XML input: {e}")))?;
            Cow::Borrowed(text)
        }
        Charset::Latin1 => Cow::Owned(body.iter().map(|&b| char::from(b)).collect()),
        Charset::Whatwg(encoding) if encoding == UTF_8 => {
            let text = std::str::from_utf8(body)
                .map_err(|e| invalid(format!("invalid UTF-8 in XML input: {e}")))?;
            Cow::Borrowed(text)
        }
        Charset::Whatwg(encoding) => quick_xml::encoding::decode(body, encoding)
            .map_err(|_| invalid(format!("invalid {} in XML input", encoding.name())))?,
    };
    Ok((text, bom, detected))
}

/// The `encoding` label of the XML declaration `head` begins with, if any,
/// once the declaration reads whole by [`read_declaration`] and
/// [`label_refusal`] takes the label in a file that begins with `bom`, or
/// in the UTF-16 byte order `detected` with no BOM. A refusal is placed in
/// `head`, at the declaration's fault or at the label.
fn declared_label(head: &[u8], bom: Option<Bom>, detected: Option<Bom>) -> Result<Option<&str>> {
    // The declaration, through its `?>`: what a refusal shows around it.
    let shown = head
        .windows(2)
        .position(|pair| pair == b"?>")
        .map_or(head, |end| &head[..end + b"?>".len()]);
    let decl = read_declaration(head).map_err(|e| declaration_error(shown, e))?;
    let Some(XmlDecl {
        encoding: Some(label),
        ..
    }) = decl
    else {
        return Ok(None);
    };
    match label_refusal(label, bom, detected) {
        None => Ok(Some(label)),
        Some(message) => {
            let pos = (label.as_ptr() as usize).saturating_sub(head.as_ptr() as usize);
            Err(declaration_error(shown, (pos, message)))
        }
    }
}

/// The character set an `encoding` label names. US-ASCII and ISO-8859-1
/// follow IANA and the XML spec — strict 7-bit, and byte = code point —
/// where the WHATWG labels encoding_rs resolves would make both
/// windows-1252, which other XML parsers do not; every other label is
/// encoding_rs's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charset {
    Ascii,
    Latin1,
    Whatwg(&'static Encoding),
}

impl Charset {
    /// Whether this is UTF-16 in either byte order (`UTF-16`, `UTF-16LE`,
    /// `unicode`, `ucs-2`, …).
    fn is_utf16(self) -> bool {
        matches!(self, Self::Whatwg(encoding) if encoding == UTF_16LE || encoding == UTF_16BE)
    }
}

/// IANA names and aliases of US-ASCII, lower case.
const ASCII_LABELS: &[&str] = &[
    "us-ascii",
    "ascii",
    "ansi_x3.4-1968",
    "ansi_x3.4-1986",
    "iso-ir-6",
    "iso_646.irv:1991",
    "iso646-us",
    "us",
    "ibm367",
    "cp367",
    "csascii",
];

/// IANA names and aliases of ISO-8859-1, and the spellings WHATWG adds for
/// it, lower case.
const LATIN1_LABELS: &[&str] = &[
    "iso-8859-1",
    "iso_8859-1",
    "iso_8859-1:1987",
    "iso-ir-100",
    "iso8859-1",
    "iso88591",
    "latin1",
    "l1",
    "ibm819",
    "cp819",
    "csisolatin1",
];

impl Charset {
    /// The character set `label` names, ignoring case and surrounding ASCII
    /// whitespace; `None` for a label neither IANA's two nor encoding_rs
    /// knows.
    fn for_label(label: &str) -> Option<Self> {
        let trimmed = label.trim_matches(|c: char| c.is_ascii_whitespace());
        let is = |labels: &[&str]| labels.iter().any(|l| trimmed.eq_ignore_ascii_case(l));
        if is(ASCII_LABELS) {
            Some(Self::Ascii)
        } else if is(LATIN1_LABELS) {
            Some(Self::Latin1)
        } else {
            Encoding::for_label(label.as_bytes()).map(Self::Whatwg)
        }
    }

    /// Whether the character set holds `c`.
    fn holds(self, c: char) -> bool {
        match self {
            Self::Ascii => c.is_ascii(),
            Self::Latin1 => u32::from(c) <= 0xFF,
            Self::Whatwg(encoding) => {
                let mut buf = [0; 4];
                let (_, _, unmappable) = encoding.encode(c.encode_utf8(&mut buf));
                c.is_ascii() || !unmappable
            }
        }
    }

    /// `text` in this character set; `None` if a character does not fit.
    fn encode(self, text: &str) -> Option<Vec<u8>> {
        match self {
            Self::Ascii | Self::Latin1 => text
                .chars()
                .map(|c| u8::try_from(u32::from(c)).ok().filter(|_| self.holds(c)))
                .collect(),
            Self::Whatwg(encoding) => {
                let (bytes, _, unmappable) = encoding.encode(text);
                (!unmappable).then(|| bytes.into_owned())
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ascii => "US-ASCII",
            Self::Latin1 => "ISO-8859-1",
            Self::Whatwg(encoding) => encoding.name(),
        }
    }
}

/// The XML declaration a document begins with, as [`read_declaration`]
/// reads it. Each value is a slice of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct XmlDecl<'b> {
    version: &'b str,
    encoding: Option<&'b str>,
    standalone: Option<&'b str>,
}

/// The pseudo-attributes of an XML declaration, in the order XML 1.0 §2.8
/// fixes: `XMLDecl ::= '<?xml' VersionInfo EncodingDecl? SDDecl? S? '?>'`.
const DECL_PSEUDO_ATTRIBUTES: [&str; 3] = ["version", "encoding", "standalone"];

/// XML's white space: space, tab, CR and LF.
fn is_xml_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// The XML declaration `bytes` begin with, read by XML 1.0 §2.8 (XML 1.1's
/// is the same): `<?xml`, then `version`, an optional `encoding` and an
/// optional `standalone`, in that order, each once, each after white space,
/// each `name = 'value'` (either quote), then `?>`. `version` is `1.` and
/// digits, `encoding` an encoding name (`EncName`), `standalone` `yes` or
/// `no`. Every encoding dq reads spells the declaration in ASCII, so the
/// bytes are read as such.
///
/// `Ok(None)` when `bytes` do not begin with a declaration — `<?xml`
/// followed by white space or `?`, as quick-xml tells one (a
/// `<?xml-stylesheet …?>` is a processing instruction). `Err` names where
/// in `bytes` the declaration goes wrong, and how.
fn read_declaration(bytes: &[u8]) -> std::result::Result<Option<XmlDecl<'_>>, (usize, String)> {
    let Some(rest) = bytes.strip_prefix(b"<?xml") else {
        return Ok(None);
    };
    if !rest.first().is_some_and(|&b| is_xml_space(b) || b == b'?') {
        return Ok(None);
    }
    let Some(end) = bytes.windows(2).position(|pair| pair == b"?>") else {
        return Err((0, "the XML declaration has no closing '?>'".to_owned()));
    };
    let mut values: [Option<&str>; 3] = [None; 3];
    let mut last: Option<usize> = None;
    let mut i = b"<?xml".len();
    loop {
        let space_start = i;
        while i < end && is_xml_space(bytes[i]) {
            i += 1;
        }
        if i == end {
            break;
        }
        let name_start = i;
        while i < end && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'-')) {
            i += 1;
        }
        if i == name_start {
            let what = match bytes[i] {
                b if b.is_ascii_graphic() => format!("'{}'", char::from(b)),
                _ => "a character".to_owned(),
            };
            return Err((
                i,
                format!("the XML declaration holds {what} where a pseudo-attribute belongs"),
            ));
        }
        // ASCII alphanumerics, so UTF-8.
        let name = std::str::from_utf8(&bytes[name_start..i]).unwrap_or_default();
        let Some(rank) = DECL_PSEUDO_ATTRIBUTES.iter().position(|&n| n == name) else {
            return Err((
                name_start,
                format!(
                    "the XML declaration has no pseudo-attribute '{name}': it takes version, \
                     encoding and standalone, in that order"
                ),
            ));
        };
        if values[rank].is_some() {
            return Err((
                name_start,
                format!("the XML declaration gives '{name}' twice"),
            ));
        }
        if values[0].is_none() && rank != 0 {
            return Err((
                name_start,
                "the XML declaration must begin with its version (version=\"1.0\")".to_owned(),
            ));
        }
        if let Some(last) = last.filter(|&last| last > rank) {
            return Err((
                name_start,
                format!(
                    "the XML declaration gives '{name}' after '{}': the order is version, \
                     encoding, standalone",
                    DECL_PSEUDO_ATTRIBUTES[last]
                ),
            ));
        }
        if space_start == name_start {
            return Err((
                name_start,
                format!("the XML declaration needs white space before '{name}'"),
            ));
        }
        while i < end && is_xml_space(bytes[i]) {
            i += 1;
        }
        if i == end || bytes[i] != b'=' {
            return Err((
                i,
                format!("the XML declaration's '{name}' has no '=' and value"),
            ));
        }
        i += 1;
        while i < end && is_xml_space(bytes[i]) {
            i += 1;
        }
        let quote = bytes[i];
        if i == end || !matches!(quote, b'"' | b'\'') {
            return Err((
                i,
                format!("the XML declaration's {name} must be quoted, with \" or '"),
            ));
        }
        let value_start = i + 1;
        let Some(len) = bytes[value_start..end].iter().position(|&b| b == quote) else {
            return Err((
                i,
                format!(
                    "the XML declaration's {name} has no closing quote ({})",
                    char::from(quote)
                ),
            ));
        };
        let raw = &bytes[value_start..value_start + len];
        let shown = String::from_utf8_lossy(raw);
        let refusal = match rank {
            0 if !std::str::from_utf8(raw).is_ok_and(is_version_num) => Some(format!(
                "the XML declaration's version {shown:?} is not a version number (1. and digits, \
                 such as 1.0)"
            )),
            1 if !std::str::from_utf8(raw).is_ok_and(is_enc_name) => Some(format!(
                "the XML declaration's encoding {shown:?} is not an encoding name"
            )),
            2 if raw != b"yes" && raw != b"no" => Some(format!(
                "the XML declaration's standalone {shown:?} is neither \"yes\" nor \"no\""
            )),
            _ => None,
        };
        if let Some(message) = refusal {
            return Err((value_start, message));
        }
        // Checked: every accepted value is ASCII.
        values[rank] = Some(std::str::from_utf8(raw).unwrap_or_default());
        last = Some(rank);
        i = value_start + len + 1;
    }
    let [Some(version), encoding, standalone] = values else {
        return Err((
            end,
            "the XML declaration must begin with its version (version=\"1.0\")".to_owned(),
        ));
    };
    Ok(Some(XmlDecl {
        version,
        encoding,
        standalone,
    }))
}

/// [`read_declaration`]'s refusal as a parse error, placed in `bytes`.
/// Every byte before the place is ASCII (a non-ASCII value is refused at
/// its start), so it counts the same in the text `bytes` decode to.
fn declaration_error(bytes: &[u8], (pos, message): (usize, String)) -> Error {
    parse_error_at(&String::from_utf8_lossy(bytes), pos, message)
}

/// Why the reader refuses `label`, the declaration's `encoding`, in a file
/// that begins with `bom`, or in the UTF-16 byte order `detected` with no
/// BOM: not an encoding name, one dq does not know or cannot read, or one
/// the BOM or the byte order contradicts. `None` when it reads.
fn label_refusal(label: &str, bom: Option<Bom>, detected: Option<Bom>) -> Option<String> {
    if !is_enc_name(label) {
        return Some(format!(
            "the XML declaration's encoding {label:?} is not an encoding name"
        ));
    }
    let charset = Charset::for_label(label);
    match (charset, bom, detected) {
        (None, ..) => Some(format!(
            "the XML declaration's encoding {label:?} is one dq does not know"
        )),
        (Some(Charset::Whatwg(encoding)), ..) if encoding == REPLACEMENT => Some(format!(
            "the XML declaration's encoding {label:?} is one dq cannot read"
        )),
        (Some(charset), Some(bom), _) if !bom.admits(charset, label) => Some(format!(
            "the file begins with a {} byte order mark but declares encoding {label:?}",
            bom.label(),
        )),
        (Some(charset), _, Some(order)) if !order.admits(charset, label) => Some(format!(
            "the file begins in {} with no byte order mark but declares encoding {label:?}",
            order.label(),
        )),
        _ => None,
    }
}

/// Build a [`Frame`] from a `<tag attr="...">` event.
/// A parse error at byte `pos` of `text`, `XML parse error: ` + `message`.
/// Where in `text` the name at fault in the start tag `start` (which
/// begins at `event_start`, its `<`) begins: the attribute `attr`, else the
/// element's name, just past the `<`. The reader borrows its events from
/// `text`, so an attribute's key is a slice of it; should it not be, the
/// element's name stands in.
fn name_offset(
    text: &str,
    start: &BytesStart<'_>,
    event_start: usize,
    attr: Option<&str>,
) -> usize {
    let tag_name = event_start + 1;
    let Some(attr) = attr else {
        return tag_name;
    };
    let base = text.as_ptr() as usize;
    start
        .attributes()
        .flatten()
        .find(|a| a.key.as_ref() == attr)
        .and_then(|a| (a.key.as_ref().as_ptr() as usize).checked_sub(base))
        .filter(|&offset| offset < text.len())
        .unwrap_or(tag_name)
}

/// quick-xml's reading error `e`, raised at `at` in `text` (the `<` of the
/// markup, or the `&` of a reference), in dq's words: where to place it,
/// and what is wrong. quick-xml's own wording and byte offsets stay out
/// of the message.
fn reader_refusal(e: &quick_xml::Error, text: &str, at: usize) -> (usize, String) {
    use quick_xml::errors::{IllFormedError, SyntaxError};
    let message = match e {
        quick_xml::Error::Syntax(e) => match e {
            SyntaxError::InvalidBangMarkup => {
                "markup that begins '<!' is no comment, CDATA section or DOCTYPE declaration"
            }
            SyntaxError::UnclosedPI => "the processing instruction has no closing '?>'",
            SyntaxError::UnclosedXmlDecl => "the XML declaration has no closing '?>'",
            SyntaxError::UnclosedComment => "the comment has no closing '-->'",
            SyntaxError::UnclosedDoctype => "the DOCTYPE declaration has no closing '>'",
            SyntaxError::UnclosedCData => "the CDATA section has no closing ']]>'",
            SyntaxError::UnclosedTag => "the tag has no closing '>'",
            SyntaxError::UnclosedSingleQuotedAttributeValue
            | SyntaxError::UnclosedDoubleQuotedAttributeValue => {
                // quick-xml points at the tag; the value begins at the
                // quote that is still open at the end of the input. Its
                // `ElementParser` toggles quotes from the same `<`, so one
                // is found; should it not be, the tag stands in.
                let mut open = None;
                for (i, b) in text.get(at..).unwrap_or_default().bytes().enumerate() {
                    match open {
                        None if matches!(b, b'"' | b'\'') => open = Some((b, i)),
                        Some((quote, _)) if b == quote => open = None,
                        _ => {}
                    }
                }
                let (quote, pos) = open.map_or((b'"', at), |(quote, i)| (quote, at + i));
                return (
                    pos,
                    format!(
                        "the attribute value has no closing quote ({})",
                        char::from(quote)
                    ),
                );
            }
        }
        .to_owned(),
        quick_xml::Error::IllFormed(e) => match e {
            IllFormedError::MissingDeclVersion(_) => {
                "the XML declaration must begin with its version (version=\"1.0\")".to_owned()
            }
            IllFormedError::UnknownVersion => {
                "the XML declaration's version is not a version number".to_owned()
            }
            IllFormedError::MissingDoctypeName => {
                "the DOCTYPE declaration names no root element".to_owned()
            }
            IllFormedError::MissingEndTag(tag) => format!("element <{tag}> is not closed"),
            IllFormedError::UnmatchedEndTag(tag) => format!("unexpected closing tag </{tag}>"),
            IllFormedError::MismatchedEndTag { expected, found } => {
                format!("closing tag </{found}> does not match <{expected}>")
            }
            IllFormedError::DoubleHyphenInComment => "a comment holds '--'".to_owned(),
            IllFormedError::UnclosedReference => "the reference has no closing ';'".to_owned(),
        },
        // Raised by attribute and reference decoding, which dq does
        // itself (`build_start_frame`, `resolve_general_ref`); input that
        // is a `&str` cannot fail to read or decode.
        quick_xml::Error::InvalidAttr(_) => "a malformed attribute".to_owned(),
        quick_xml::Error::Escape(_) => "a malformed reference".to_owned(),
        quick_xml::Error::Namespace(_) => "a malformed namespace prefix".to_owned(),
        quick_xml::Error::Io(_) | quick_xml::Error::Encoding(_) => {
            "the document could not be read".to_owned()
        }
    };
    (at, message)
}

/// The attribute name that ends before `at` in the tag content `content`,
/// past white space and, when `eq`, an `=` and white space; and the
/// offset it begins at.
fn key_before(content: &str, at: usize, eq: bool) -> (usize, &str) {
    let bytes = content.as_bytes();
    let mut i = at.min(bytes.len());
    let skip_space = |mut i: usize| {
        while i > 0 && is_xml_space(bytes[i - 1]) {
            i -= 1;
        }
        i
    };
    i = skip_space(i);
    if eq && i > 0 && bytes[i - 1] == b'=' {
        i = skip_space(i - 1);
    }
    let end = i;
    while i > 0 && !is_xml_space(bytes[i - 1]) && !matches!(bytes[i - 1], b'=' | b'"' | b'\'') {
        i -= 1;
    }
    (i, &content[i..end])
}

/// quick-xml's attribute error `e` in the start tag `<{tag} …>` whose
/// content (past the `<`) is `content`, in dq's words: the offset in
/// `content` to place it at — the attribute's name, or its value — and
/// what is wrong.
fn attr_refusal(e: &AttrError, content: &str, tag: &str) -> (usize, String) {
    match *e {
        AttrError::ExpectedEq(at) => {
            let (pos, key) = key_before(content, at, false);
            (
                pos,
                format!("attribute '{key}' on <{tag}> has no '=' and value"),
            )
        }
        AttrError::ExpectedValue(at) => {
            let (pos, key) = key_before(content, at, true);
            (
                pos,
                format!("attribute '{key}' on <{tag}> has '=' but no value"),
            )
        }
        AttrError::UnquotedValue(at) => {
            let (_, key) = key_before(content, at, true);
            (
                at,
                format!("the value of attribute '{key}' on <{tag}> must be quoted, with \" or '"),
            )
        }
        AttrError::ExpectedQuote(at, quote) => (
            at,
            format!(
                "an attribute value on <{tag}> has no closing quote ({})",
                char::from(quote)
            ),
        ),
        AttrError::Duplicated(at, _) => {
            let key = content
                .get(at..)
                .unwrap_or_default()
                .split(|c: char| c == '=' || c.is_ascii_whitespace())
                .next()
                .unwrap_or_default();
            (at, format!("attribute '{key}' appears twice on <{tag}>"))
        }
    }
}

/// `e` with its line and column in the message too, `XML parse error at
/// line 2, column 4: …`, as the HCL and YAML readers put theirs — the
/// message is all `convert` or `get` print. An error with no position
/// (line 0: the encoding, told before there is text to count lines in)
/// is left as it is.
fn with_position(e: Error) -> Error {
    match e {
        Error::Parse {
            file,
            line,
            col,
            span,
            snippet,
            message,
        } if line > 0 => {
            let rest = message
                .strip_prefix("XML parse error: ")
                .unwrap_or(&message);
            Error::Parse {
                message: format!("XML parse error at line {line}, column {col}: {rest}"),
                file,
                line,
                col,
                span,
                snippet,
            }
        }
        other => other,
    }
}

fn parse_error_at(text: &str, pos: usize, message: String) -> Error {
    let (line, col) = byte_pos_to_line_col(text, pos);
    Error::Parse {
        file: None,
        line,
        col,
        span: pos..pos,
        snippet: snippet_for_pos(text, pos),
        message: format!("XML parse error: {message}"),
    }
}

/// Record on `frame` the prefixes its attributes declare or undeclare,
/// and refuse what the writer refuses ([`Scope`]): a declaration
/// Namespaces in XML forbids (1.1 when `xml11`, else 1.0), and an element
/// or attribute name whose prefix is not in scope — no `xmlns:p` on the
/// element or an ancestor (`stack`) declares it, or the nearest one
/// undeclares it; `xml` always is. Read, such a file could not be written
/// back.
///
/// The error names the attribute at fault, `None` for the element's name.
fn check_namespaces(
    stack: &[Frame],
    frame: &mut Frame,
    xml11: bool,
) -> std::result::Result<(), (String, Option<String>)> {
    let mut prefixes = Vec::new();
    for (name, value) in &frame.attrs {
        let Value::String(uri) = value else {
            continue;
        };
        if let Some((prefix, bound)) = declared_prefix(name, uri, &frame.tag, xml11)
            .map_err(|message| (message, Some(name.clone())))?
        {
            prefixes.push((prefix.to_owned(), bound));
        }
    }
    frame.prefixes = prefixes;
    let declares = |prefix: &str| {
        prefix == "xml"
            || std::iter::once(&*frame)
                .chain(stack.iter().rev())
                .find_map(|f| f.prefixes.iter().find(|(p, _)| p == prefix))
                .is_some_and(|(_, bound)| *bound)
    };
    if let Some(message) = undeclared_prefix(&frame.tag, None, declares) {
        return Err((message, None));
    }
    for name in frame.attrs.keys() {
        if !is_namespace_declaration(name)
            && let Some(message) = undeclared_prefix(name, Some(&frame.tag), declares)
        {
            return Err((message, Some(name.clone())));
        }
    }
    Ok(())
}

/// The frame of the start tag `start`, which begins at `event_start` (its
/// `<`) in `text`. A malformed attribute is placed at its name (or its
/// value), in dq's words.
fn build_start_frame(
    start: &BytesStart<'_>,
    text: &str,
    event_start: usize,
    xml11: bool,
) -> Result<Frame> {
    let tag = start.name().as_ref().to_owned();
    // quick-xml counts an attribute error's position from past the `<`.
    let content_start = event_start + "<".len();
    let content = text.get(content_start..).unwrap_or_default();
    let mut frame = Frame::new(tag, event_start);
    for attr in start.attributes() {
        let attr = attr.map_err(|e| {
            let (pos, message) = attr_refusal(&e, content, &frame.tag);
            parse_error_at(text, content_start + pos, message)
        })?;
        let name = attr.key.as_ref().to_owned();
        let value = decode_attr(&attr, xml11).map_err(|()| {
            let base = text.as_ptr() as usize;
            let pos = (attr.value.as_ptr() as usize)
                .checked_sub(base)
                .filter(|&offset| offset < text.len())
                .unwrap_or(event_start);
            parse_error_at(
                text,
                pos,
                format!(
                    "the value of attribute '{name}' on <{}> is malformed",
                    frame.tag
                ),
            )
        })?;
        frame.attrs.insert(name, Value::String(value));
    }
    Ok(frame)
}

fn decode_text(t: &BytesText<'_>, xml11: bool) -> String {
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
    t.xml_content(xml_version(xml11)).into_owned()
}

/// The version whose end-of-line handling and attribute-value
/// normalization quick-xml applies: XML 1.1's also reads NEL and LS, and
/// CR before NEL, as a line feed.
fn xml_version(xml11: bool) -> XmlVersion {
    if xml11 {
        XmlVersion::Explicit1_1
    } else {
        XmlVersion::Implicit1_0
    }
}

/// `s` with XML end-of-line handling applied, as quick-xml applies it to
/// text but not to a PI: CR LF and a lone CR read as LF, and in XML 1.1
/// (`xml11`) CR NEL, NEL and LS too. A parser applies it to the whole
/// document before it parses, so no place keeps a CR.
fn normalize_line_ends(s: &str, xml11: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CR LF, and CR NEL in 1.1, is one line end.
                chars.next_if(|&n| n == '\n' || (xml11 && n == '\u{85}'));
                out.push('\n');
            }
            '\u{85}' | '\u{2028}' if xml11 => out.push('\n'),
            c => out.push(c),
        }
    }
    out
}

/// The value of `attr`, its references resolved and normalized as the
/// version has it. `Err` only for a reference
/// [`check_attribute_values`] lets through, which it should not.
fn decode_attr(attr: &Attribute<'_>, xml11: bool) -> std::result::Result<String, ()> {
    // `unescape_value` was deprecated in quick-xml 0.38 in favour of
    // `normalized_value`, which does the same predefined-entity resolution
    // plus the spec-mandated whitespace normalisation. The wire-level output
    // is the same for the predefined-entity set we care about.
    attr.normalized_value(xml_version(xml11))
        .map(|c| c.into_owned())
        .map_err(|_| ())
}

/// Refuse a character `text`, a document of XML 1.1 when `xml11` (else
/// 1.0), holds that is not well-formed there, at the character: one
/// neither version allows (NUL, U+FFFE, U+FFFF), a C0 control but tab, LF
/// and CR in 1.0, and in 1.1 a `RestrictedChar` ([`is_restricted_11`]),
/// which 1.1 takes only as a character reference. C1 controls are
/// characters in 1.0; NEL is a line end in 1.1.
fn check_raw_chars(text: &str, xml11: bool) -> Result<()> {
    let refusal = |c: char| match c {
        '\u{0}' | '\u{FFFE}' | '\u{FFFF}' => Some(if xml11 { "1.1" } else { "1.0" }),
        '\t' | '\n' | '\r' => None,
        '\u{1}'..='\u{1F}' if !xml11 => Some("1.0"),
        _ => None,
    };
    for (pos, c) in text.char_indices() {
        let message = match refusal(c) {
            Some(version) => format!(
                "the document holds U+{:04X}, a character XML {version} does not allow",
                u32::from(c)
            ),
            None if xml11 && is_restricted_11(c) => format!(
                "the document holds U+{:04X}, which XML 1.1 allows only as a character \
                 reference",
                u32::from(c)
            ),
            None => continue,
        };
        return Err(parse_error_at(text, pos, message));
    }
    Ok(())
}

/// Why the reference `&name;` is not well-formed in XML 1.1 when `xml11`
/// (else 1.0), or not one dq can resolve: an entity other than the five
/// XML predefines (there is no DTD to declare one); a character reference
/// that is no number (`&#X41;`: only a lowercase `x` makes a hex one) or
/// one past U+10FFFF; one to NUL, a surrogate, U+FFFE or U+FFFF, or in
/// 1.0 to a C0 control but tab, LF and CR. `None` when it resolves.
fn reference_refusal(name: &str, xml11: bool) -> Option<String> {
    let Some(number) = name.strip_prefix('#') else {
        return quick_xml::escape::resolve_predefined_entity(name)
            .is_none()
            .then(|| format!("undeclared entity reference '&{name};'"));
    };
    let (digits, radix) = match number.strip_prefix('x') {
        Some(hex) => (hex, 16),
        None => (number, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return Some(format!(
            "the character reference '&{name};' is no number: it takes decimal digits \
             (&#65;) or, after a lowercase x, hex digits (&#x41;)"
        ));
    }
    let Some(code) = u32::from_str_radix(digits, radix)
        .ok()
        .filter(|&code| code <= 0x10_FFFF)
    else {
        return Some(format!(
            "the character reference '&{name};' is past U+10FFFF, the last Unicode code point"
        ));
    };
    let allowed = match code {
        0 | 0xD800..=0xDFFF | 0xFFFE | 0xFFFF => false,
        0x9 | 0xA | 0xD => true,
        0x1..=0x1F => xml11,
        _ => true,
    };
    let version = if xml11 { "1.1" } else { "1.0" };
    (!allowed).then(|| {
        format!(
            "the character reference '&{name};' refers to U+{code:04X}, a character XML \
             {version} does not allow"
        )
    })
}

/// Refuse, in an attribute value of `start`, a raw `<` (XML 1.0 §3.1
/// `AttValue` takes it only as a reference), and a reference that
/// [`reference_refusal`] refuses or one with no closing `;`, at its `&`.
/// The reader borrows the value from `text`.
fn check_attribute_values(text: &str, start: &BytesStart<'_>, xml11: bool) -> Result<()> {
    let base = text.as_ptr() as usize;
    for attr in start.attributes().flatten() {
        let value: &str = &attr.value;
        let at = (value.as_ptr() as usize).saturating_sub(base);
        for (index, markup) in value.match_indices(['&', '<']) {
            if markup == "<" {
                let name = start.name();
                let tag: &str = name.as_ref();
                let key: &str = attr.key.as_ref();
                return Err(parse_error_at(
                    text,
                    at + index,
                    format!(
                        "the value of attribute '{key}' on <{tag}> holds '<', which XML allows \
                         there only as '&lt;'"
                    ),
                ));
            }
            let Some(end) = value[index..].find(';') else {
                return Err(parse_error_at(
                    text,
                    at + index,
                    "the reference has no closing ';'".to_owned(),
                ));
            };
            if let Some(message) = reference_refusal(&value[index + 1..index + end], xml11) {
                return Err(parse_error_at(text, at + index, message));
            }
        }
    }
    Ok(())
}

/// Resolve an `Event::GeneralRef(&entity;)` payload into its textual
/// replacement.
///
/// Handles the five predefined XML entities (`amp`, `lt`, `gt`, `quot`,
/// `apos`) plus numeric character references (`&#48;` / `&#x30;`). Anything
/// else is an undeclared entity — without a DTD we cannot resolve it, so we
/// surface a `Error::Parse` rather than silently swallowing the reference.
/// [`reference_refusal`] words every refusal, placed at the `&` (`pos`).
fn resolve_general_ref(r: &BytesRef<'_>, text: &str, pos: usize, xml11: bool) -> Result<String> {
    // quick-xml 0.42: `BytesRef` derefs to the entity name as `str`; the
    // fallible `decode()` step (UTF-8 only, so it never failed for us) is gone.
    let name: &str = r;
    if let Some(message) = reference_refusal(name, xml11) {
        return Err(parse_error_at(text, pos, message));
    }
    let resolved = if r.is_char_ref() {
        // Numeric character reference — `&#48;` or `&#x30;`.
        r.resolve_char_ref()
            .ok()
            .map(|ch| ch.map_or_else(String::new, |c| c.to_string()))
    } else {
        quick_xml::escape::resolve_predefined_entity(name).map(str::to_owned)
    };
    // `reference_refusal` lets through only a reference that resolves.
    resolved.ok_or_else(|| {
        parse_error_at(
            text,
            pos,
            format!("the reference '&{name};' does not resolve"),
        )
    })
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
fn render_xml(top: &Value, crlf: bool) -> Result<Vec<u8>> {
    let Value::Map(top) = top else {
        return Err(Error::Format {
            format: "xml",
            message: format!(
                "XML write: the top-level value must be a map (got {})",
                what_it_is(top)
            ),
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
                    what_it_is(v),
                ),
            });
        }
    }

    // 1. XML declaration. A `#xml` holding only `bom` asks for no
    // declaration, just the BOM.
    let decl = declaration(top.get(KEY_XML_DECL))?;
    let output = Output::for_declaration(decl.as_ref())?;
    if let Some(decl) = decl.as_ref().filter(|decl| decl.written) {
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
    let xml11 = decl
        .as_ref()
        .is_some_and(|decl| decl.written && decl.version == "1.1");
    let mut unencodable = Unencodable::new(&output);
    for s in item_texts(KEY_PI, TOP, top.get(KEY_PI), xml11)? {
        unencodable.note(&s, || TOP.to_owned());
        writer
            .write_event(Event::PI(BytesPI::new(s.as_str())))
            .map_err(map_write_err)?;
    }
    for s in item_texts(KEY_COMMENTS, TOP, top.get(KEY_COMMENTS), xml11)? {
        unencodable.note(&s, || TOP.to_owned());
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
    let roots = match top.get(root_tag) {
        Some(Value::Array(roots)) => roots,
        other => {
            return Err(Error::Format {
                format: "xml",
                message: format!(
                    "XML write: root '{root_tag}' must be a one-element Array (got {})",
                    other.map_or_else(|| "nothing".to_owned(), what_it_is),
                ),
            });
        }
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
            message: format!(
                "XML write: root element '{root_tag}' must be a Map (got {})",
                what_it_is(&roots[0])
            ),
        });
    };
    write_element(
        &mut writer,
        root_tag,
        root_body,
        0,
        &mut unencodable,
        &Scope {
            xml11,
            crlf,
            ..Scope::default()
        },
    )?;
    // The output ends with one line break, as every re-emit does.
    write_line_break(&mut writer, 0, crlf)?;

    if let Some((c, place)) = unencodable.first {
        let label = decl
            .as_ref()
            .and_then(|d| d.encoding.as_deref())
            .unwrap_or_default();
        return Err(Error::Format {
            format: "xml",
            message: format!(
                "XML write: '{KEY_XML_DECL}' declares encoding {label:?}, which cannot hold U+{:04X} \
                 {place}",
                u32::from(c),
            ),
        });
    }
    output.encode(writer.into_inner().into_inner())
}

/// How the writer turns its UTF-8 output into the bytes `#xml` asks for.
enum Output {
    /// UTF-8, beginning with a BOM or not.
    Utf8 { bom: bool },
    /// UTF-16 in the given byte order, beginning with a BOM or not.
    Utf16 { big_endian: bool, bom: bool },
    /// Any other character set dq writes, ASCII-compatible or not.
    Other(Charset),
}

impl Output {
    /// The output a declaration asks for: UTF-8 without one or without an
    /// `encoding`, unless `bom` or `detected` asks for UTF-16; with a BOM if
    /// `bom` is `UTF-8`, which only a UTF-8 label admits. A generic UTF-16
    /// label (`UTF-16`) begins with a BOM — XML requires one — in the byte
    /// order `bom` names, little-endian by default; `UTF-16LE` / `UTF-16BE`
    /// fix the byte order and begin with a BOM only if `bom` says so.
    /// `detected` — a file read with no BOM — writes UTF-16 in its byte
    /// order with no BOM, as the file was; `detected: UTF-8` under a UTF-16
    /// label writes UTF-8, as a file that declared UTF-16 in 8-bit bytes
    /// was. Refused: a label dq does not know or cannot encode to, a `bom`
    /// or `detected` that contradicts the label (`UTF-8` with no UTF-16
    /// label), and both of them at once.
    fn for_declaration(decl: Option<&Declaration>) -> Result<Self> {
        let (label, bom, detected) = decl.map_or((None, None, None), |d| {
            (d.encoding.as_deref(), d.bom, d.detected)
        });
        let refuse = |message: String| {
            Err(Error::Format {
                format: "xml",
                message: format!("XML write: '{KEY_XML_DECL}' {message}"),
            })
        };
        if bom.is_some() && detected.is_some() {
            return refuse(format!(
                "has both '{KEY_BOM}' and '{KEY_DETECTED}', but a file begins with a byte order \
                 mark or does not"
            ));
        }
        if detected == Some(Bom::Utf8) {
            return match label {
                Some(label) if Charset::for_label(label).is_some_and(Charset::is_utf16) => {
                    Ok(Self::Utf8 { bom: false })
                }
                Some(label) => refuse(format!(
                    "field '{KEY_DETECTED}' is UTF-8, which only a UTF-16 label needs, but the \
                     declared encoding is {label:?}"
                )),
                None => refuse(format!(
                    "field '{KEY_DETECTED}' is UTF-8, which only a UTF-16 label needs, but there \
                     is no declared encoding"
                )),
            };
        }
        let Some(label) = label else {
            return Ok(match (bom, detected) {
                (None, None) => Self::Utf8 { bom: false },
                (Some(Bom::Utf8), _) => Self::Utf8 { bom: true },
                (Some(bom), _) => Self::Utf16 {
                    big_endian: bom == Bom::Utf16Be,
                    bom: true,
                },
                (None, Some(order)) => Self::Utf16 {
                    big_endian: order == Bom::Utf16Be,
                    bom: false,
                },
            });
        };
        let Some(charset) = Charset::for_label(label) else {
            return refuse(format!(
                "declares encoding {label:?}, which dq does not know"
            ));
        };
        for (key, mark) in [(KEY_BOM, bom), (KEY_DETECTED, detected)] {
            if let Some(mark) = mark
                && !mark.admits(charset, label)
            {
                return refuse(format!(
                    "field '{key}' is {} but the declared encoding is {label:?}",
                    mark.label(),
                ));
            }
        }
        let encoding = match charset {
            Charset::Whatwg(encoding) => encoding,
            Charset::Ascii | Charset::Latin1 => return Ok(Self::Other(charset)),
        };
        if encoding == UTF_16LE || encoding == UTF_16BE {
            // An admitted `bom` here is a UTF-16 one in the label's order.
            let big_endian = encoding == UTF_16BE;
            return Ok(if names_utf16_byte_order(label) {
                Self::Utf16 {
                    big_endian,
                    bom: bom.is_some(),
                }
            } else if let Some(order) = detected {
                Self::Utf16 {
                    big_endian: order == Bom::Utf16Be,
                    bom: false,
                }
            } else {
                Self::Utf16 {
                    big_endian: bom.map_or(big_endian, |bom| bom == Bom::Utf16Be),
                    bom: true,
                }
            });
        }
        if encoding == UTF_8 {
            Ok(Self::Utf8 { bom: bom.is_some() })
        } else if encoding == REPLACEMENT {
            refuse(format!(
                "declares encoding {label:?}, which dq cannot write"
            ))
        } else {
            Ok(Self::Other(charset))
        }
    }

    /// The character set whose characters need checking: `None` for UTF-8
    /// and UTF-16, which hold every character.
    fn limited_to(&self) -> Option<Charset> {
        match self {
            Self::Other(charset) => Some(*charset),
            Self::Utf8 { .. } | Self::Utf16 { .. } => None,
        }
    }

    /// Encode the writer's UTF-8 output. [`Unencodable`] has refused every
    /// character the target cannot hold before this runs.
    fn encode(&self, utf8: Vec<u8>) -> Result<Vec<u8>> {
        let text = String::from_utf8(utf8).map_err(|e| Error::Format {
            format: "xml",
            message: format!("XML write error: output is not UTF-8: {e}"),
        })?;
        match self {
            Self::Utf8 { bom } => {
                let mut bytes = if *bom {
                    "\u{feff}".to_owned()
                } else {
                    String::new()
                };
                bytes.push_str(&text);
                Ok(bytes.into_bytes())
            }
            Self::Utf16 { big_endian, bom } => {
                // A declaration naming UTF-16 always leads; only
                // `detected` without one can get here with other text.
                if !*bom && !text.starts_with("<?") {
                    return Err(Error::Format {
                        format: "xml",
                        message: format!(
                            "XML write: '{KEY_XML_DECL}' field '{KEY_DETECTED}' asks for UTF-16 \
                             with no byte order mark, which a reader can tell only in a file that \
                             begins with `<?`: add a declaration or a '{KEY_BOM}'"
                        ),
                    });
                }
                let units = bom.then_some(0xFEFF).into_iter().chain(text.encode_utf16());
                Ok(units
                    .flat_map(|unit| {
                        if *big_endian {
                            unit.to_be_bytes()
                        } else {
                            unit.to_le_bytes()
                        }
                    })
                    .collect())
            }
            Self::Other(charset) => charset.encode(&text).ok_or_else(|| Error::Format {
                format: "xml",
                message: format!(
                    "XML write error: the output holds a character {} cannot encode",
                    charset.name(),
                ),
            }),
        }
    }
}

/// The first character the writer emitted that the output encoding cannot
/// hold, and where: `in <tag>` or `at the top level`.
struct Unencodable {
    charset: Option<Charset>,
    first: Option<(char, String)>,
}

impl Unencodable {
    fn new(output: &Output) -> Self {
        Self {
            charset: output.limited_to(),
            first: None,
        }
    }

    /// Remember the first character of `text` the character set cannot
    /// hold, unless one is already remembered. `place` is built only then.
    fn note(&mut self, text: &str, place: impl FnOnce() -> String) {
        let Some(charset) = self.charset else {
            return;
        };
        if self.first.is_none()
            && let Some(c) = text.chars().find(|&c| !charset.holds(c))
        {
            self.first = Some((c, place()));
        }
    }
}

/// The namespace URI the `xml` prefix is bound to, always and only.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
/// The namespace URI of the `xmlns` prefix, which no declaration may bind.
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The namespace prefixes in scope while writing an element: the ones its
/// own `xmlns:p` attributes declare (`true`) or, in XML 1.1, undeclare
/// (`false`) on top of its ancestors'. `xml` is always in scope.
#[derive(Default)]
struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    prefixes: Vec<(&'a str, bool)>,
    /// Whether the document is XML 1.1, whose namespaces may undeclare a
    /// prefix.
    xml11: bool,
    /// Whether the lines the writer breaks end in `\r\n` rather than `\n`.
    crlf: bool,
}

impl<'a> Scope<'a> {
    /// The scope inside `<tag>`: `parent` plus the prefixes its attributes
    /// declare or undeclare. A declaration [`declared_prefix`] refuses is
    /// refused.
    fn declared_on(
        parent: &'a Scope<'a>,
        tag: &str,
        attributes: &'a [(&'a str, String)],
    ) -> Result<Self> {
        let mut prefixes = Vec::new();
        for (name, uri) in attributes {
            if let Some(binding) =
                declared_prefix(name, uri, tag, parent.xml11).map_err(write_refusal)?
            {
                prefixes.push(binding);
            }
        }
        Ok(Self {
            parent: Some(parent),
            prefixes,
            xml11: parent.xml11,
            crlf: parent.crlf,
        })
    }

    /// Whether `prefix` is in scope: `xml`, or a prefix the nearest
    /// `xmlns:prefix` declares rather than undeclares.
    fn declares(&self, prefix: &str) -> bool {
        if prefix == "xml" {
            return true;
        }
        match self.prefixes.iter().find(|(p, _)| *p == prefix) {
            Some((_, bound)) => *bound,
            None => self.parent.is_some_and(|parent| parent.declares(prefix)),
        }
    }

    /// Refuse an element name (`on` is `None`) or an attribute name on
    /// `<on>` whose prefix is not in scope, and an element named with the
    /// reserved `xmlns` prefix or `xmlns` itself.
    fn check_prefix(&self, name: &str, on: Option<&str>) -> Result<()> {
        match undeclared_prefix(name, on, |prefix| self.declares(prefix)) {
            Some(message) => Err(write_refusal(message)),
            None => Ok(()),
        }
    }
}

/// The writer's refusal: `Error::Format` with `XML write: ` + `message`.
fn write_refusal(message: String) -> Error {
    Error::Format {
        format: "xml",
        message: format!("XML write: {message}"),
    }
}

/// The prefix the attribute `name="uri"` on `<tag>` declares, `(p, true)`
/// for `xmlns:p`, or undeclares, `(p, false)` for `xmlns:p=""` in an XML
/// 1.1 document (`xml11`, Namespaces in XML 1.1); `None` for the default
/// `xmlns` and any other attribute. A declaration Namespaces in XML forbids
/// is an `Err` naming it: `xmlns:xmlns`, `xmlns:xml` bound anywhere but
/// [`XML_NAMESPACE`], another prefix (or the default) bound to that or to
/// [`XMLNS_NAMESPACE`], and an empty `xmlns:p` outside XML 1.1 (Namespaces
/// in XML 1.0 cannot undeclare a prefix).
fn declared_prefix<'n>(
    name: &'n str,
    uri: &str,
    tag: &str,
    xml11: bool,
) -> std::result::Result<Option<(&'n str, bool)>, String> {
    let prefix = match name.strip_prefix("xmlns") {
        Some("") => None,
        Some(rest) => match rest.strip_prefix(':') {
            Some(prefix) => Some(prefix),
            None => return Ok(None),
        },
        None => return Ok(None),
    };
    match prefix {
        Some("xmlns") => Err(format!(
            "'{name}' on <{tag}> cannot be declared: the xmlns prefix is reserved",
        )),
        Some("xml") if uri != XML_NAMESPACE => Err(format!(
            "'{name}' on <{tag}> must bind {XML_NAMESPACE} (got {uri:?})",
        )),
        Some("xml") => Ok(None),
        _ if uri == XML_NAMESPACE => Err(format!(
            "'{name}' on <{tag}> binds {XML_NAMESPACE}, which only the xml prefix may",
        )),
        _ if uri == XMLNS_NAMESPACE => Err(format!(
            "'{name}' on <{tag}> binds {XMLNS_NAMESPACE}, which nothing may",
        )),
        Some(prefix) if uri.is_empty() && xml11 => Ok(Some((prefix, false))),
        Some(_) if uri.is_empty() => Err(format!(
            "'{name}' on <{tag}> is empty: XML namespaces 1.0 cannot undeclare a prefix",
        )),
        prefix => Ok(prefix.map(|prefix| (prefix, true))),
    }
}

/// Why an element name (`on` is `None`) or an attribute name on `<on>`
/// cannot stand where `declares` says which prefixes are in scope: its
/// prefix is not, or it names an element with the reserved `xmlns` prefix
/// or `xmlns` itself. `None` when it can.
fn undeclared_prefix(
    name: &str,
    on: Option<&str>,
    declares: impl Fn(&str) -> bool,
) -> Option<String> {
    let prefix = name.split_once(':').map(|(prefix, _)| prefix);
    if on.is_none() && (name == "xmlns" || prefix == Some("xmlns")) {
        return Some(format!(
            "'{name}' cannot name an element: the xmlns prefix is reserved for namespace \
             declarations",
        ));
    }
    let prefix = prefix.filter(|prefix| !declares(prefix))?;
    let what = match on {
        None => format!("element <{name}>"),
        Some(tag) => format!("attribute '{name}' on <{tag}>"),
    };
    Some(format!(
        "{what} uses the prefix '{prefix}', which no 'xmlns:{prefix}' declares in scope",
    ))
}

/// `xmlns` or `xmlns:p`: an attribute that declares a namespace rather
/// than one that is in one.
fn is_namespace_declaration(name: &str) -> bool {
    name == "xmlns" || name.starts_with("xmlns:")
}

/// Indent width for pretty-printed XML output (2 spaces per level).
///
/// Hardcoded — the value tree does not carry the original inter-element
/// whitespace, so on `parse → write` we always synthesise this layout.
const XML_INDENT_WIDTH: usize = 2;

/// Emit a line break (`\r\n` when `crlf`, else `\n`) +
/// `indent_level * XML_INDENT_WIDTH` spaces as a text event.
///
/// We pass the whitespace through `BytesText::from_escaped` so `quick-xml`
/// writes it verbatim (no entity escaping — there is nothing to escape).
/// Pure-whitespace text between element siblings parses back into
/// `Frame::text` but is discarded during `Frame::finalise` (because
/// `has_text` only flips on non-whitespace text), so round-trip stays
/// structurally stable.
fn write_line_break(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    indent_level: usize,
    crlf: bool,
) -> Result<()> {
    let mut s = String::with_capacity(2 + indent_level * XML_INDENT_WIDTH);
    s.push_str(if crlf { "\r\n" } else { "\n" });
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
    unencodable: &mut Unencodable,
    scope: &Scope<'_>,
) -> Result<()> {
    check_name(tag, "an element")?;
    let place = || format!("in <{tag}>");
    unencodable.note(tag, place);
    let mut start = BytesStart::new(tag);
    match body.get(KEY_ATTRS) {
        // A null `@attrs` is no attributes, as every other null is no content.
        None | Some(Value::Null | Value::Map(_)) => {}
        Some(other) => {
            return Err(Error::Format {
                format: "xml",
                message: format!(
                    "XML write: '{KEY_ATTRS}' of <{tag}> must be a map of attributes (got {})",
                    what_it_is(other),
                ),
            });
        }
    }
    let mut attributes = Vec::new();
    if let Some(Value::Map(attrs)) = body.get(KEY_ATTRS) {
        for (k, v) in attrs {
            let v = match v {
                // An attribute is present or not: a null one is empty.
                Value::Null => String::new(),
                scalar => scalar_text(scalar).ok_or_else(|| Error::Format {
                    format: "xml",
                    message: format!(
                        "XML write: attribute '{k}' on <{tag}> must be a scalar (got {})",
                        what_it_is(scalar),
                    ),
                })?,
            };
            check_name(k, &format!("an attribute (on <{tag}>)"))?;
            check_chars(
                &format!("attribute '{k}' on <{tag}>"),
                &v,
                scope.xml11,
                Spelled::Escaped,
            )?;
            unencodable.note(k, place);
            unencodable.note(&v, place);
            attributes.push((k.as_str(), v));
        }
    }
    let scope = Scope::declared_on(scope, tag, &attributes)?;
    scope.check_prefix(tag, None)?;
    for (k, v) in &attributes {
        if !is_namespace_declaration(k) {
            scope.check_prefix(k, Some(tag))?;
        }
        start.push_attribute(Attribute {
            key: QName(k),
            value: escape_attr(v, scope.xml11).into_owned().into(),
        });
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
                what_it_is(v),
            ),
        })?),
    };
    if let Some(text) = &text {
        check_chars(
            &format!("'{KEY_TEXT}' of <{tag}>"),
            text,
            scope.xml11,
            Spelled::Escaped,
        )?;
        unencodable.note(text, place);
    }

    // Determine if the element has any body content. If not, emit as
    // self-closing `<tag/>`.
    let owner = format!("of <{tag}>");
    let xml11 = scope.xml11;
    let comments = item_texts(KEY_COMMENTS, &owner, body.get(KEY_COMMENTS), xml11)?;
    let pis = item_texts(KEY_PI, &owner, body.get(KEY_PI), xml11)?;
    let cdata = item_texts(KEY_CDATA, &owner, body.get(KEY_CDATA), xml11)?;
    for s in comments.iter().chain(&pis).chain(&cdata) {
        unencodable.note(s, place);
    }
    // A child entry is a list of elements; a null one is no content. Any
    // other value is refused here, before an element with no list child
    // would be written empty and the value dropped.
    if let Some((k, v)) = body
        .iter()
        .find(|(k, v)| !is_conventional(k) && !matches!(v, Value::Array(_) | Value::Null))
    {
        return Err(Error::Format {
            format: "xml",
            message: format!(
                "XML write: child '{k}' under <{tag}> must be an Array of element maps (got {})",
                what_it_is(v),
            ),
        });
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
            write_line_break(writer, child_indent, scope.crlf)?;
        }
        writer
            .write_event(Event::Comment(comment_event(s)))
            .map_err(map_write_err)?;
    }
    for s in &pis {
        if pretty {
            write_line_break(writer, child_indent, scope.crlf)?;
        }
        writer
            .write_event(Event::PI(BytesPI::new(s.as_str())))
            .map_err(map_write_err)?;
    }
    if let Some(s) = &text {
        writer
            .write_event(Event::Text(text_event(s, xml11)))
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
        // Checked above: anything else is a null, no content.
        let Value::Array(items) = v else {
            continue;
        };
        for child in items {
            if pretty {
                write_line_break(writer, child_indent, scope.crlf)?;
            }
            match child {
                Value::Map(child_body) => {
                    write_element(writer, k, child_body, child_indent, unencodable, &scope)?;
                }
                Value::Null => {
                    // Empty child element.
                    let tmp = IndexMap::new();
                    write_element(writer, k, &tmp, child_indent, unencodable, &scope)?;
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
                    write_element(writer, k, &tmp, child_indent, unencodable, &scope)?;
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
        write_line_break(writer, indent_level, scope.crlf)?;
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
fn item_texts(key: &str, owner: &str, entry: Option<&Value>, xml11: bool) -> Result<Vec<String>> {
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
                what_it_is(item),
            ),
        })?;
        check_item(key, owner, &text, xml11)?;
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
fn check_item(key: &str, owner: &str, text: &str, xml11: bool) -> Result<()> {
    check_chars(&format!("'{key}' {owner}"), text, xml11, Spelled::Raw)?;
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
    name_refusal(name, what).map_or(Ok(()), |message| {
        Err(Error::Format {
            format: "xml",
            message: format!("XML write: {message}"),
        })
    })
}

/// Why `name` cannot name `what`, the reader's and the writer's words:
/// it is no namespace-well-formed `QName`. `None` when it can.
fn name_refusal(name: &str, what: &str) -> Option<String> {
    if is_qname(name) {
        return None;
    }
    // The hint only where a colon is at fault: `xmlns:1p` is refused for
    // its `1`.
    let colon_at_fault =
        name.matches(':').count() > 1 || name.starts_with(':') || name.ends_with(':');
    let hint = if colon_at_fault {
        " (a name holds at most one ':', between a prefix and a local name)"
    } else {
        ""
    };
    Some(format!("'{name}' is not a valid XML name for {what}{hint}"))
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

/// How a place writes a character: escaped, so a character reference
/// can stand for it (text, an attribute value), or raw (a comment, a PI,
/// CDATA).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spelled {
    Escaped,
    Raw,
}

/// Refuse `text` if it holds a character the document's XML version does
/// not allow where it goes. XML 1.0's `Char` leaves out the C0 controls but
/// tab, LF and CR, and U+FFFE / U+FFFF; XML has no escape for them — a
/// character reference to one is not well-formed either. XML 1.1's `Char`
/// takes every C0 control but NUL, and takes them and the C1 controls but
/// NEL (its `RestrictedChar`, [`is_restricted_11`]) only as character
/// references, which a comment, a PI or CDATA cannot hold. Nor can they
/// hold a line end other than LF — a CR, and in 1.1 NEL and LS — which
/// end-of-line handling reads back as LF. `what` names the place
/// (`'#text' of <r>`).
fn check_chars(what: &str, text: &str, xml11: bool, spelled: Spelled) -> Result<()> {
    let char_10 = |c: char| {
        matches!(c,
            '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
    };
    let char_11 =
        |c: char| matches!(c, '\u{1}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..);
    let refuse = |c: char, why: &str| {
        Err(Error::Format {
            format: "xml",
            message: format!("XML write: {what} holds U+{:04X}, {why}", u32::from(c)),
        })
    };
    let version = if xml11 { "1.1" } else { "1.0" };
    // What end-of-line handling reads as a line feed.
    let line_end = |c: char| c == '\r' || (xml11 && matches!(c, '\u{85}' | '\u{2028}'));
    for c in text.chars() {
        if !(if xml11 { char_11(c) } else { char_10(c) }) {
            return refuse(c, &format!("a character XML {version} does not allow"));
        }
        if spelled == Spelled::Escaped {
            continue;
        }
        if xml11 && is_restricted_11(c) {
            return refuse(
                c,
                "which XML 1.1 allows only as a character reference, and a comment, a \
                 processing instruction or CDATA holds none",
            );
        }
        if line_end(c) {
            return refuse(
                c,
                &format!(
                    "which XML {version} reads back as a line feed, and a comment, a \
                     processing instruction or CDATA holds no character reference"
                ),
            );
        }
    }
    Ok(())
}

/// XML 1.1's `RestrictedChar`: the C0 controls but tab, LF and CR, and the
/// C1 controls but NEL, which a document holds only as character
/// references.
fn is_restricted_11(c: char) -> bool {
    matches!(c,
        '\u{1}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{7F}'..='\u{84}'
        | '\u{86}'..='\u{9F}')
}

/// The fields of an `<?xml …?>` declaration, and the UTF-16 BOM to begin
/// with. `written` is false for a `#xml` that holds only `bom` /
/// `detected`: a file that began with a BOM, or in UTF-16 without one, and
/// had no declaration. `detected` is the encoding a file with no BOM was
/// read in when its declaration does not say it: UTF-16 in a byte order,
/// or UTF-8 under a UTF-16 label.
struct Declaration {
    version: String,
    encoding: Option<String>,
    standalone: Option<String>,
    bom: Option<Bom>,
    /// The encoding a file with no BOM was read in, where its declaration
    /// does not say it.
    detected: Option<Bom>,
    written: bool,
}

/// The declaration a top-level `#xml` entry asks for: `None` without one
/// (absent or null). Each field is written as its [`scalar_text`] — a YAML
/// `version: 1.1` is a float and stays `1.1` — and a missing or null
/// `version` is `1.0`; a boolean `standalone` is `yes` / `no`. A non-map
/// entry, a key other than `version` / `encoding` / `standalone` / `bom` /
/// `detected`, a container as a field, and a field the declaration grammar does not allow
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
                    what_it_is(other),
                ),
            });
        }
    };
    let mut fields = Declaration {
        version: "1.0".to_owned(),
        encoding: None,
        standalone: None,
        bom: None,
        detected: None,
        written: decl.is_empty() || decl.keys().any(|k| k != KEY_BOM && k != KEY_DETECTED),
    };
    for (k, v) in decl {
        if k == KEY_BOM {
            fields.bom = match v {
                Value::Null => None,
                Value::String(s) if s == "UTF-8" => Some(Bom::Utf8),
                Value::String(s) if s == "UTF-16LE" => Some(Bom::Utf16Le),
                Value::String(s) if s == "UTF-16BE" => Some(Bom::Utf16Be),
                other => {
                    return Err(decl_field_error(
                        k,
                        "must be UTF-8, UTF-16LE or UTF-16BE",
                        other,
                    ));
                }
            };
            continue;
        }
        if k == KEY_DETECTED {
            fields.detected = match v {
                Value::Null => None,
                Value::String(s) if s == "UTF-8" => Some(Bom::Utf8),
                Value::String(s) if s == "UTF-16LE" => Some(Bom::Utf16Le),
                Value::String(s) if s == "UTF-16BE" => Some(Bom::Utf16Be),
                other => {
                    return Err(decl_field_error(
                        k,
                        "must be UTF-8, UTF-16LE or UTF-16BE",
                        other,
                    ));
                }
            };
            continue;
        }
        let slot = match k.as_str() {
            "version" => None,
            "encoding" => Some(&mut fields.encoding),
            "standalone" => Some(&mut fields.standalone),
            _ => {
                return Err(Error::Format {
                    format: "xml",
                    message: format!(
                        "XML write: '{KEY_XML_DECL}' has no field '{k}' (only version, encoding, standalone, \
                         bom, detected)",
                    ),
                });
            }
        };
        if matches!(v, Value::Null) {
            continue;
        }
        let text = scalar_text(v).ok_or_else(|| decl_field_error(k, "must be a scalar", v))?;
        let text = match (k.as_str(), v) {
            // `standalone` takes `yes` / `no` only; a boolean is the same
            // answer.
            ("standalone", Value::Bool(b)) => if *b { "yes" } else { "no" }.to_owned(),
            ("standalone", _) if text != "yes" && text != "no" => {
                return Err(decl_field_error(k, "must be yes or no", v));
            }
            ("version", _) if !is_version_num(&text) => {
                return Err(decl_field_error(k, "must be 1. and digits", v));
            }
            ("encoding", _) if !is_enc_name(&text) => {
                return Err(decl_field_error(k, "is not an encoding name", v));
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

fn decl_field_error(field: &str, rule: &str, got: &Value) -> Error {
    Error::Format {
        format: "xml",
        message: format!(
            "XML write: '{KEY_XML_DECL}' field '{field}' {rule} (got {})",
            what_it_is(got)
        ),
    }
}

/// `value` as a message names what it got: a string quoted (`"UTF-32"`),
/// a number or a boolean by its kind and its text (`a number 5`,
/// `a boolean true`), anything else by its kind (`a map`, `an array`,
/// `null`) — so that a map never reads as the string `"object"`.
fn what_it_is(value: &Value) -> String {
    match value {
        Value::String(s) => format!("{s:?}"),
        Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::BigFloat(_) => {
            format!("a number {}", scalar_text(value).unwrap_or_default())
        }
        Value::Bool(b) => format!("a boolean {b}"),
        Value::Null | Value::Array(_) | Value::Map(_) => value.kind_with_article().to_owned(),
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

/// Whether `s` is all XML whitespace — space, tab, CR and LF, the `S` of
/// XML 1.0 §2.3 — and so indentation the reader drops when it stands
/// alone. Unicode's other spaces (U+3000, U+2028, U+0085, which
/// [`str::trim`] would strip) are text.
fn is_xml_whitespace(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Escape `raw` for a text body: `<`, `>`, `&`, `'` and `"` — the set
/// quick-xml's `escape` used up to 0.40, kept so output stays byte-stable —
/// CR as `&#13;`, and NEL and LS as `&#x85;` / `&#x2028;`. A raw CR in text
/// reads back as LF (XML end-of-line handling); the reference reads back as
/// CR. XML 1.1 reads a raw NEL or LS back as LF too, so they are written as
/// references whatever the version; XML 1.0 reads the references the same.
///
/// Text that is all XML whitespace ([`is_xml_whitespace`]: `"\t"`,
/// `"\n "`) is written as character references, every character of it:
/// the reader drops whitespace-only text written as it is — it is
/// indentation — but keeps what references spell. Other Unicode spaces
/// (U+3000) are text to XML and are written as they are.
fn escape_text(raw: &str, xml11: bool) -> Cow<'_, str> {
    if !raw.is_empty() && is_xml_whitespace(raw) {
        return Cow::Owned(raw.chars().map(char_reference).collect());
    }
    escape_with(raw, |c| match c {
        '\r' | '\u{85}' | '\u{2028}' => true,
        c => xml11 && is_restricted_11(c),
    })
}

/// Escape `raw` for an attribute value: [`escape_text`]'s set plus LF and
/// tab as `&#10;` / `&#9;`. Attribute-value normalization reads a raw tab,
/// LF or CR back as a space — and in XML 1.1 a raw NEL or LS — the
/// references read back as themselves.
fn escape_attr(raw: &str, xml11: bool) -> Cow<'_, str> {
    escape_with(raw, |c| match c {
        '\r' | '\n' | '\t' | '\u{85}' | '\u{2028}' => true,
        c => xml11 && is_restricted_11(c),
    })
}

/// `c` as a character reference: decimal for ASCII (`&#9;`), hexadecimal
/// past it (`&#x2028;`).
fn char_reference(c: char) -> String {
    if c.is_ascii() {
        format!("&#{};", u32::from(c))
    } else {
        format!("&#x{:X};", u32::from(c))
    }
}

/// Replace the five markup characters by their entities, and every
/// character `referenced` picks by its [`char_reference`].
fn escape_with(raw: &str, referenced: impl Fn(char) -> bool) -> Cow<'_, str> {
    let entity = |c: char| match c {
        '<' => Some("&lt;"),
        '>' => Some("&gt;"),
        '&' => Some("&amp;"),
        '\'' => Some("&apos;"),
        '"' => Some("&quot;"),
        _ => None,
    };
    let Some(first) = raw.find(|c| entity(c).is_some() || referenced(c)) else {
        return Cow::Borrowed(raw);
    };
    let mut out = String::with_capacity(raw.len() + 8);
    out.push_str(&raw[..first]);
    for c in raw[first..].chars() {
        match entity(c) {
            Some(entity) => out.push_str(entity),
            None if referenced(c) => out.push_str(&char_reference(c)),
            None => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// `BytesText` for a text body, escaped with [`escape_text`].
fn text_event(s: &str, xml11: bool) -> BytesText<'_> {
    BytesText::from_escaped(escape_text(s, xml11))
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

    /// `v` written, without the line break every output ends with.
    fn render(v: &Value) -> String {
        let bytes = render_xml(v, false).expect("render xml");
        let text = String::from_utf8(bytes).expect("utf-8");
        let body = text.strip_suffix('\n').expect("ends with a line break");
        assert!(!body.ends_with('\n'), "{text:?}");
        body.to_owned()
    }

    #[test]
    fn a_rewrite_ends_with_one_line_break_the_file_s_own() {
        let pointer = |p: &str| crate::pointer::Pointer::parse(p).unwrap();
        for (src, want) in [
            ("<a><b>1</b></a>", "<a>\n  <b>2</b>\n</a>\n"),
            ("<a>\n  <b>1</b>\n</a>\n\n\n", "<a>\n  <b>2</b>\n</a>\n"),
            (
                "<a>\r\n  <b>1</b>\r\n</a>\r\n",
                "<a>\r\n  <b>2</b>\r\n</a>\r\n",
            ),
        ] {
            let mut doc = Xml.parse(src.as_bytes()).expect("parse");
            doc.set_at(&pointer("/a/0/b/0/#text"), Value::Int(2))
                .expect("set");
            assert_eq!(
                String::from_utf8(doc.original_bytes().to_vec()).unwrap(),
                want,
                "{src:?}"
            );
        }
        // In the encoding the declaration names, after a BOM.
        let mut doc = Xml
            .parse(b"\xEF\xBB\xBF<?xml version=\"1.0\" encoding=\"UTF-8\"?><a>1</a>")
            .expect("parse");
        doc.set_at(&pointer("/a/0/#text"), Value::Int(2))
            .expect("set");
        assert_eq!(
            doc.original_bytes(),
            b"\xEF\xBB\xBF<?xml version=\"1.0\" encoding=\"UTF-8\"?><a>2</a>\n"
        );
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
        let err = render_xml(&element(Value::Array(vec![])), false).expect_err("refused");
        assert!(
            err.to_string().contains("must be a scalar (got an array)"),
            "{err}"
        );
    }

    #[test]
    fn line_ends_are_read_as_the_version_has_them() {
        let all = "a\r\nb\rc\r\u{85}d\u{85}e\u{2028}f\n";
        assert_eq!(
            normalize_line_ends(all, false),
            "a\nb\nc\n\u{85}d\u{85}e\u{2028}f\n"
        );
        assert_eq!(normalize_line_ends(all, true), "a\nb\nc\nd\ne\nf\n");
        assert_eq!(normalize_line_ends("\r\r\n", false), "\n\n");
    }

    #[test]
    fn a_reference_or_character_a_version_does_not_allow_is_refused() {
        for (name, xml11, refused) in [
            ("#x1", false, true),
            ("#x1", true, false),
            ("#9", false, false),
            ("#0", true, true),
            ("#xD800", true, true),
            ("#65535", true, true),
            ("#x7F", false, false),
            ("amp", false, false),
            // No number, past U+10FFFF, an entity there is no DTD for.
            ("#x", false, true),
            ("#-1", false, true),
            ("#X41", false, true),
            ("#x110000", false, true),
            ("#99999999999", false, true),
            ("#x10FFFF", false, false),
            ("bogus", false, true),
        ] {
            assert_eq!(
                reference_refusal(name, xml11).is_some(),
                refused,
                "{name} {xml11}"
            );
        }
        assert!(check_raw_chars("<r>\u{80}\u{85}</r>", false).is_ok());
        assert!(check_raw_chars("<r>\u{85}\u{2028}</r>", true).is_ok());
        assert!(check_raw_chars("<r>\u{80}</r>", true).is_err());
        assert!(check_raw_chars("<r>\u{1}</r>", false).is_err());
        assert!(check_raw_chars("<r>\t\r\n</r>", true).is_ok());
    }

    #[test]
    fn a_child_entry_that_is_no_list_is_refused_and_a_null_one_is_empty() {
        let element = |child: Value| {
            let mut body = IndexMap::new();
            body.insert("c".to_owned(), child);
            let mut top = IndexMap::new();
            top.insert("x".to_owned(), Value::Array(vec![Value::Map(body)]));
            Value::Map(top)
        };
        assert_eq!(render(&element(Value::Null)), "<x/>");
        let err = render_xml(&element(Value::String("v".to_owned())), false).expect_err("refused");
        assert!(
            err.to_string()
                .contains("child 'c' under <x> must be an Array of element maps (got \"v\")"),
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
        let err = render_xml(
            &element(KEY_PI, Value::Array(vec![Value::Array(vec![])])),
            false,
        )
        .expect_err("refused");
        assert!(
            err.to_string()
                .contains("'#pi' of <x> must hold scalars (got an array)"),
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
        let err = render_xml(&doc(Value::Map(decl)), false).expect_err("refused");
        assert!(err.to_string().contains("has no field 'doctype'"), "{err}");
        let err = render_xml(&doc(Value::Int(1)), false).expect_err("refused");
        assert!(err.to_string().contains("(got a number 1)"), "{err}");
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
        assert!(check_item(KEY_COMMENTS, owner, "a < b & c -x", false).is_ok());
        assert!(check_item(KEY_COMMENTS, owner, "a--b", false).is_err());
        assert!(check_item(KEY_COMMENTS, owner, "a-", false).is_err());
        assert!(check_item(KEY_PI, owner, "xml-stylesheet href='a'", false).is_ok());
        assert!(check_item(KEY_PI, owner, "t\tdata ? >", false).is_ok());
        for pi in ["xml v", "XML", "1t", "", " t", "t a?>"] {
            assert!(check_item(KEY_PI, owner, pi, false).is_err(), "{pi:?}");
        }
        assert!(check_item(KEY_CDATA, owner, "a]]>b", false).is_ok());
        let err = check_item(KEY_CDATA, owner, "\u{FFFE}", false).expect_err("refused");
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
    fn a_file_is_decoded_per_its_bom_or_declared_encoding() {
        let latin = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r>caf\xe9</r>";
        let (text, bom, _) = decode_input(latin).expect("decodes");
        assert!(
            text.ends_with("<r>caf\u{e9}</r>") && bom.is_none(),
            "{text}"
        );

        let mut utf16 = vec![0xFE, 0xFF];
        utf16.extend("<r>\u{e9}</r>".encode_utf16().flat_map(u16::to_be_bytes));
        let (text, bom, _) = decode_input(&utf16).expect("decodes");
        assert_eq!((text.as_ref(), bom), ("<r>\u{e9}</r>", Some(Bom::Utf16Be)));

        // ISO-8859-1 is byte = code point (0x80 a C1 control), windows-1252
        // its own mapping (0x80 the euro sign), US-ASCII strictly 7-bit.
        for (label, expected) in [
            ("ISO-8859-1", '\u{80}'),
            ("latin1", '\u{80}'),
            ("windows-1252", '\u{20ac}'),
        ] {
            let mut xml = format!("<?xml version='1.0' encoding='{label}'?><r>").into_bytes();
            xml.extend(b"\x80</r>");
            let (text, ..) = decode_input(&xml).expect("decodes");
            assert!(
                text.ends_with(&format!("<r>{expected}</r>")),
                "{label}: {text}"
            );
        }
        let ascii = b"<?xml version='1.0' encoding='US-ASCII'?><r>caf\xe9</r>";
        let Err(Error::Parse { message, .. }) = decode_input(ascii) else {
            panic!("US-ASCII with a byte above 0x7F must be a parse error");
        };
        assert_eq!(
            message,
            "invalid US-ASCII in XML input: byte 0xE9 at index 47"
        );

        // A BOM keeps its own encoding: one the declaration contradicts, and
        // an `encoding` that is no encoding name, are parse errors.
        let (text, bom, _) = decode_input(b"\xEF\xBB\xBF<r/>").expect("decodes");
        assert_eq!((text.as_ref(), bom), ("<r/>", Some(Bom::Utf8)));
        for bytes in [
            &b"\xEF\xBB\xBF<?xml version='1.0' encoding='latin1'?><r/>"[..],
            &b"<?xml version='1.0' encoding=' latin1'?><r/>"[..],
        ] {
            assert!(
                matches!(decode_input(bytes), Err(Error::Parse { .. })),
                "{bytes:?}"
            );
        }

        // UTF-16 with no BOM keeps its byte order as `detected`, unless the
        // declaration names it; a declaration it contradicts is refused.
        let bare = |text: &str, big_endian: bool| -> Vec<u8> {
            text.encode_utf16()
                .flat_map(|u| {
                    if big_endian {
                        u.to_be_bytes()
                    } else {
                        u.to_le_bytes()
                    }
                })
                .collect()
        };
        for (decl, big_endian, detected) in [
            ("<?xml version='1.0'?>", false, Some(Bom::Utf16Le)),
            ("<?xml version='1.0'?>", true, Some(Bom::Utf16Be)),
            (
                "<?xml version='1.0' encoding='UTF-16'?>",
                true,
                Some(Bom::Utf16Be),
            ),
            ("<?xml version='1.0' encoding='utf-16le'?>", false, None),
            ("<?xml version='1.0' encoding='UTF-16BE'?>", true, None),
            ("<?pi x?>", true, Some(Bom::Utf16Be)),
        ] {
            let xml = format!("{decl}<r>\u{e9}</r>");
            let bytes = bare(&xml, big_endian);
            let (text, bom, order) = decode_input(&bytes).expect("decodes");
            assert_eq!((text.as_ref(), bom, order), (xml.as_str(), None, detected));
        }
        for (label, big_endian, order) in [
            ("ISO-8859-1", false, "UTF-16LE"),
            ("UTF-8", true, "UTF-16BE"),
            ("UTF-16BE", false, "UTF-16LE"),
            ("UTF-16LE", true, "UTF-16BE"),
        ] {
            let xml = format!("<?xml version='1.0' encoding='{label}'?><r/>");
            let Err(Error::Parse { message, .. }) = decode_input(&bare(&xml, big_endian)) else {
                panic!("{label} over {order} with no BOM must be a parse error");
            };
            assert_eq!(
                message,
                format!(
                    "XML parse error: the file begins in {order} with no byte order mark but \
                     declares encoding \"{label}\""
                )
            );
        }

        // A UTF-16 label over 8-bit bytes reads as UTF-8 and says so;
        // ISO-2022-JP reads as itself.
        for label in ["UTF-16", "utf-16le", "unicode"] {
            let xml = format!("<?xml version=\"1.0\" encoding='{label}'?><r>\u{e9}</r>");
            let (text, bom, detected) = decode_input(xml.as_bytes()).expect("decodes");
            assert_eq!(
                (text.as_ref(), bom, detected),
                (xml.as_str(), None, Some(Bom::Utf8))
            );
        }
        let xml = "<?xml version=\"1.0\" encoding='UTF-8'?><r/>";
        assert_eq!(decode_input(xml.as_bytes()).expect("decodes").2, None);
        let jis = b"<?xml version='1.0' encoding='ISO-2022-JP'?><r>\x1b$B$\x22\x1b(B</r>";
        let (text, ..) = decode_input(jis).expect("decodes");
        assert!(text.ends_with("<r>\u{3042}</r>"), "{text}");
        // A label dq does not know, or cannot read, is a parse error.
        for (label, why) in [
            ("no-such", "does not know"),
            ("UTF-32", "does not know"),
            ("ISO-2022-KR", "cannot read"),
        ] {
            let xml = format!("<?xml version=\"1.0\" encoding='{label}'?><r/>");
            let Err(Error::Parse { message, .. }) = decode_input(xml.as_bytes()) else {
                panic!("{label} must be a parse error");
            };
            assert_eq!(
                message,
                format!(
                    "XML parse error: the XML declaration's encoding \"{label}\" is one dq {why}"
                )
            );
        }
        fn label(bytes: &[u8]) -> Option<&str> {
            read_declaration(bytes)
                .expect("a well-formed declaration")
                .and_then(|decl| decl.encoding)
        }
        assert_eq!(
            label(b"<?xml version='1.0' encoding = \"x\"?><r/>"),
            Some("x")
        );
        assert_eq!(label(b"<?xml version='1.0'?><r encoding='x'/>"), None);
        assert_eq!(label(b"<?xml-stylesheet encoding='x'?>"), None);
    }

    #[test]
    fn the_declaration_is_read_by_its_grammar() {
        assert_eq!(
            read_declaration(b"<?xml\tversion = '1.1'\nencoding=\"UTF-8\" standalone='no' ?><r/>"),
            Ok(Some(XmlDecl {
                version: "1.1",
                encoding: Some("UTF-8"),
                standalone: Some("no"),
            }))
        );
        assert_eq!(read_declaration(b"<r/>"), Ok(None));
        assert_eq!(read_declaration(b"<?xml-stylesheet a='b'?><r/>"), Ok(None));
        // Each refusal, and the byte it is placed at.
        for (decl, pos, message) in [
            ("<?xml?>", 5, "must begin with its version"),
            (
                "<?xml encoding='UTF-8' version='1.0'?>",
                6,
                "must begin with its version",
            ),
            (
                "<?xml version='1.0' standalone='yes' encoding='UTF-8'?>",
                37,
                "after 'standalone'",
            ),
            (
                "<?xml version='1.0' version='1.0'?>",
                20,
                "gives 'version' twice",
            ),
            (
                "<?xml version='1.0' foo='1'?>",
                20,
                "no pseudo-attribute 'foo'",
            ),
            (
                "<?xml version='1.0'encoding='UTF-8'?>",
                19,
                "white space before 'encoding'",
            ),
            ("<?xml version?>", 13, "'version' has no '=' and value"),
            ("<?xml version=1.0?>", 14, "version must be quoted"),
            (
                "<?xml version='1.0\"?>",
                14,
                "version has no closing quote (')",
            ),
            (
                "<?xml version='2.0'?>",
                15,
                "\"2.0\" is not a version number",
            ),
            (
                "<?xml version='1.0' encoding='8bit'?>",
                30,
                "\"8bit\" is not an encoding name",
            ),
            (
                "<?xml version='1.0' standalone='YES'?>",
                32,
                "\"YES\" is neither",
            ),
            (
                "<?xml version='1.0' 'x'?>",
                20,
                "holds ''' where a pseudo-attribute",
            ),
            ("<?xml version='1.0'", 0, "has no closing '?>'"),
        ] {
            let Err((at, refusal)) = read_declaration(decl.as_bytes()) else {
                panic!("{decl} is accepted");
            };
            assert_eq!(at, pos, "{decl}: {refusal}");
            assert!(refusal.contains(message), "{decl}: {refusal}");
        }
    }

    #[test]
    fn the_output_encoding_follows_the_declaration() {
        let decl = |encoding: Option<&str>, bom: Option<Bom>| Declaration {
            version: "1.0".to_owned(),
            encoding: encoding.map(str::to_owned),
            standalone: None,
            bom,
            detected: None,
            written: true,
        };
        let utf16 = |decl: Declaration| match Output::for_declaration(Some(&decl)) {
            Ok(Output::Utf16 { big_endian, bom }) => (big_endian, bom),
            _ => panic!("UTF-16 output"),
        };
        assert!(matches!(
            Output::for_declaration(None),
            Ok(Output::Utf8 { bom: false })
        ));
        assert!(matches!(
            Output::for_declaration(Some(&decl(Some("utf8"), None))),
            Ok(Output::Utf8 { bom: false })
        ));
        for encoding in [None, Some("UTF-8")] {
            assert!(matches!(
                Output::for_declaration(Some(&decl(encoding, Some(Bom::Utf8)))),
                Ok(Output::Utf8 { bom: true })
            ));
        }
        assert_eq!(utf16(decl(Some("UTF-16"), None)), (false, true));
        assert_eq!(
            utf16(decl(Some("UTF-16"), Some(Bom::Utf16Be))),
            (true, true)
        );
        assert_eq!(utf16(decl(None, Some(Bom::Utf16Be))), (true, true));
        assert_eq!(utf16(decl(Some("utf-16be"), None)), (true, false));
        // A file read with no BOM is written back in its byte order, with
        // no BOM.
        let detected = |encoding: Option<&str>, order: Bom| Declaration {
            detected: Some(order),
            ..decl(encoding, None)
        };
        for encoding in [None, Some("UTF-16"), Some("utf-16")] {
            assert_eq!(utf16(detected(encoding, Bom::Utf16Be)), (true, false));
            assert_eq!(utf16(detected(encoding, Bom::Utf16Le)), (false, false));
        }
        // UTF-8 under a UTF-16 label is written as UTF-8, with no BOM.
        for label in ["UTF-16", "utf-16be"] {
            assert!(matches!(
                Output::for_declaration(Some(&detected(Some(label), Bom::Utf8))),
                Ok(Output::Utf8 { bom: false })
            ));
        }
        for (case, decl) in [
            detected(Some("UTF-8"), Bom::Utf8),
            detected(Some("ISO-8859-1"), Bom::Utf8),
            detected(None, Bom::Utf8),
            detected(Some("UTF-8"), Bom::Utf16Le),
            detected(Some("UTF-16BE"), Bom::Utf16Le),
            Declaration {
                bom: Some(Bom::Utf16Le),
                ..detected(None, Bom::Utf16Le)
            },
        ]
        .into_iter()
        .enumerate()
        {
            assert!(Output::for_declaration(Some(&decl)).is_err(), "case {case}");
        }
        for (encoding, bom) in [
            (Some("no-such"), None),
            (Some("ISO-2022-KR"), None),
            (Some("UTF-16LE"), Some(Bom::Utf16Be)),
            (Some("ISO-8859-1"), Some(Bom::Utf16Le)),
            (Some("UTF-16"), Some(Bom::Utf8)),
            (Some("ISO-8859-1"), Some(Bom::Utf8)),
        ] {
            assert!(
                Output::for_declaration(Some(&decl(encoding, bom))).is_err(),
                "{encoding:?} {bom:?}"
            );
        }
        let latin = Output::for_declaration(Some(&decl(Some("ISO-8859-1"), None))).expect("ok");
        assert_eq!(
            latin.encode("caf\u{e9}".into()).expect("encodes"),
            b"caf\xe9"
        );
        assert_eq!(latin.encode("\u{80}".into()).expect("encodes"), b"\x80");
        let mut unencodable = Unencodable::new(&latin);
        unencodable.note("caf\u{e9}\u{80}", || unreachable!("encodable"));
        // U+20AC is windows-1252's 0x80, not Latin-1's.
        unencodable.note("\u{20ac}\u{3c0}", || "in <k>".to_owned());
        assert_eq!(unencodable.first, Some(('\u{20ac}', "in <k>".to_owned())));

        let ascii = Output::for_declaration(Some(&decl(Some("ascii"), None))).expect("ok");
        let mut unencodable = Unencodable::new(&ascii);
        unencodable.note("cafe", || unreachable!("encodable"));
        unencodable.note("caf\u{e9}", || "in <r>".to_owned());
        assert_eq!(unencodable.first, Some(('\u{e9}', "in <r>".to_owned())));
        assert!(ascii.encode("\u{e9}".into()).is_err());
    }

    #[test]
    fn an_open_attribute_value_is_placed_at_its_quote_else_at_the_tag() {
        use quick_xml::errors::SyntaxError;
        let open = quick_xml::Error::Syntax(SyntaxError::UnclosedDoubleQuotedAttributeValue);
        let text = "<r>\n<c a=\"x/>";
        assert_eq!(
            reader_refusal(&open, text, 4),
            (
                9,
                "the attribute value has no closing quote (\")".to_owned()
            )
        );
        // Past the quote, no open one is found: the tag's `<`, not byte 0.
        assert_eq!(
            reader_refusal(&open, text, 10),
            (
                10,
                "the attribute value has no closing quote (\")".to_owned()
            )
        );
    }

    #[test]
    fn a_parse_error_names_where_the_name_at_fault_begins() {
        let at = |xml: &str| match Xml.parse(xml.as_bytes()) {
            Err(Error::Parse {
                line, col, message, ..
            }) => (line, col, message),
            other => panic!("{xml}: expected a parse error, got {other:?}"),
        };
        // An attribute whose name stands earlier inside a quoted value.
        let (line, col, message) = at("<r>\n  <k b=\" a:x=1\" a:x=\"2\"/>\n</r>");
        assert_eq!((line, col), (2, 17));
        assert!(
            message.starts_with("XML parse error at line 2, column 17: attribute 'a:x' on <k>"),
            "{message}"
        );
        let place = |xml: &str| {
            let (line, col, _) = at(xml);
            (line, col)
        };
        // An element: its prefix, past the `<`.
        assert_eq!(place("<r>\n<a:b/></r>"), (2, 2));
        // A second root, where it begins; a closing tag that does not match.
        assert_eq!(place("<r/>\n <s/>"), (2, 2));
        assert_eq!(place("<r>\n <a></b></r>"), (2, 5));
        // An entity, at its `&`; a malformed attribute, at its name, or
        // at its value; an unclosed value, at its quote.
        assert_eq!(place("<r>\n x&bogus;</r>"), (2, 3));
        assert_eq!(place("<r>\n<k a=\"1\" a=\"2\"/></r>"), (2, 10));
        assert_eq!(place("<r>\n<k  a = /></r>"), (2, 5));
        assert_eq!(place("<r>\n<k a=1/></r>"), (2, 6));
        assert_eq!(place("<r>\n<k b='1' a=\"x"), (2, 12));
        // A refused encoding, at its label — past a BOM, on the line the
        // label stands on.
        let (line, col, message) = at("<?xml version=\"1.0\" encoding=\"UTF-32\"?><r/>");
        assert_eq!((line, col), (1, 31));
        assert!(
            message.starts_with(
                "XML parse error at line 1, column 31: the XML declaration's encoding"
            ),
            "{message}"
        );
        assert_eq!(
            place("\u{FEFF}<?xml version=\"1.0\"\n  encoding='ISO-8859-1'?><r/>"),
            (2, 13)
        );
    }

    #[test]
    fn whitespace_xml_would_normalise_is_escaped() {
        assert_eq!(escape_text("a\tb\nc\rd<", false), "a\tb\nc&#13;d&lt;");
        // All whitespace: every character a reference.
        assert_eq!(escape_text("\t \n\r", false), "&#9;&#32;&#10;&#13;");
        assert_eq!(escape_text("\u{85}\u{2028}", false), "&#x85;&#x2028;");
        // Line ends in XML 1.1, wherever they stand; U+3000 is text.
        assert_eq!(escape_text("a\u{85}b\u{2028} ", false), "a&#x85;b&#x2028; ");
        assert!(matches!(
            escape_text("\u{3000}", false),
            Cow::Borrowed("\u{3000}")
        ));
        assert!(is_xml_whitespace(" \t\r\n") && !is_xml_whitespace("\u{3000}"));
        assert_eq!(escape_attr("a\u{85}b\u{2028}", false), "a&#x85;b&#x2028;");
        assert_eq!(escape_text("", false), "");
        assert_eq!(
            escape_attr("a\tb\nc\rd\"", false),
            "a&#9;b&#10;c&#13;d&quot;"
        );
        // XML 1.1 references its restricted characters; 1.0 writes C1 raw.
        assert_eq!(
            escape_text("\u{1}\u{7F}\u{85}\u{9F}\u{A0}", true),
            "&#1;&#127;&#x85;&#x9F;\u{A0}"
        );
        assert_eq!(escape_attr("\u{B}\u{86}", true), "&#11;&#x86;");
        assert_eq!(escape_text("\u{7F}\u{9F}", false), "\u{7F}\u{9F}");
        assert!(check_item(KEY_CDATA, "of <r>", "\u{7F}", true).is_err());
        assert!(check_item(KEY_CDATA, "of <r>", "\u{7F}", false).is_ok());
        assert!(check_item(KEY_CDATA, "of <r>", "a\r\nb", false).is_err());
        assert!(check_item(KEY_COMMENTS, "of <r>", "\u{85}\u{2028}", false).is_ok());
        assert!(check_item(KEY_COMMENTS, "of <r>", "\u{2028}", true).is_err());
        assert!(check_chars("'#text' of <r>", "\r\u{85}", true, Spelled::Escaped).is_ok());
        assert!(matches!(escape_attr("plain", false), Cow::Borrowed(_)));
    }

    #[test]
    fn prefixes_must_be_declared_in_scope() {
        let root = Scope::default();
        let decls = [("xmlns:p", "u".to_owned()), ("xmlns", String::new())];
        let outer = Scope::declared_on(&root, "r", &decls).expect("valid declarations");
        let inner = Scope::declared_on(&outer, "k", &[]).expect("nothing declared");
        assert!(inner.check_prefix("p:x", None).is_ok());
        assert!(inner.check_prefix("xml:lang", Some("k")).is_ok());
        assert!(inner.check_prefix("plain", None).is_ok());
        let err = inner
            .check_prefix("q:x", Some("k"))
            .expect_err("undeclared");
        assert!(err.to_string().contains("no 'xmlns:q' declares"), "{err}");
        assert!(root.check_prefix("p:x", None).is_err());
        assert!(root.check_prefix("xmlns:a", None).is_err());
        // XML 1.1 may undeclare a prefix; inside, it is out of scope.
        let root11 = Scope {
            xml11: true,
            ..Scope::default()
        };
        let decls11 = [("xmlns:p", "u".to_owned())];
        let outer11 = Scope::declared_on(&root11, "r", &decls11).expect("declares p");
        let undo = [("xmlns:p", String::new())];
        let inner11 = Scope::declared_on(&outer11, "k", &undo).expect("1.1 undeclares p");
        let below = Scope::declared_on(&inner11, "j", &[]).expect("nothing declared");
        assert!(outer11.check_prefix("p:x", None).is_ok());
        assert!(inner11.check_prefix("p:x", None).is_err());
        assert!(below.check_prefix("p:x", None).is_err());
        let redo = Scope::declared_on(&below, "i", &decls11).expect("declares p again");
        assert!(redo.check_prefix("p:x", None).is_ok());
        for (name, uri) in [
            ("xmlns:xmlns", "u"),
            ("xmlns:xml", "u"),
            ("xmlns:p", XML_NAMESPACE),
            ("xmlns", XMLNS_NAMESPACE),
            ("xmlns:p", ""),
        ] {
            let decls = [(name, uri.to_owned())];
            assert!(
                Scope::declared_on(&root, "r", &decls).is_err(),
                "{name}={uri}"
            );
        }
        let decls = [("xmlns:xml", XML_NAMESPACE.to_owned())];
        assert!(Scope::declared_on(&root, "r", &decls).is_ok());
    }

    #[test]
    fn the_reader_refuses_a_prefix_the_writer_would() {
        for xml in [
            "<r xmlns:p='u'><p:k xml:lang='en' p:x='1'/></r>",
            "<p:r xmlns:p='u'><k><p:k/></k></p:r>",
            "<r xmlns='u' xmlns:xml='http://www.w3.org/XML/1998/namespace'/>",
            "<r xml:space='preserve'/>",
            // XML 1.1 may undeclare a prefix, and declare it again below.
            "<?xml version='1.1'?><r xmlns:p='u'><k xmlns:p=''><j xmlns:p='v'><p:i/></j></k></r>",
        ] {
            let value = parse_xml(xml.as_bytes()).expect(xml);
            render_xml(&value, false).expect(xml);
        }
        // Undeclared, the prefix is out of scope — down to the next
        // declaration.
        let xml = "<?xml version='1.1'?><r xmlns:p='u'><k xmlns:p=''><j><p:i/></j></k></r>";
        let Err(Error::Parse { message, .. }) = parse_xml(xml.as_bytes()) else {
            panic!("{xml} must be a parse error");
        };
        assert_eq!(
            message,
            "XML parse error: element <p:i> uses the prefix 'p', which no 'xmlns:p' declares in \
             scope"
        );
        for (xml, message) in [
            (
                "<a:b/>",
                "element <a:b> uses the prefix 'a', which no 'xmlns:a' declares in scope",
            ),
            (
                "<r a:x='1'/>",
                "attribute 'a:x' on <r> uses the prefix 'a', which no 'xmlns:a' declares in scope",
            ),
            (
                "<r><k xmlns:p='u'/><p:k/></r>",
                "element <p:k> uses the prefix 'p', which no 'xmlns:p' declares in scope",
            ),
            (
                "<xmlns:k/>",
                "'xmlns:k' cannot name an element: the xmlns prefix is reserved for namespace \
                 declarations",
            ),
            (
                "<r xmlns:p=''/>",
                "'xmlns:p' on <r> is empty: XML namespaces 1.0 cannot undeclare a prefix",
            ),
        ] {
            let Err(Error::Parse { message: got, .. }) = parse_xml(xml.as_bytes()) else {
                panic!("{xml} must be a parse error");
            };
            assert_eq!(got, format!("XML parse error: {message}"), "{xml}");
        }
    }

    #[test]
    fn a_second_root_and_top_level_text_are_refused() {
        let empty = || Value::Array(vec![Value::Map(IndexMap::new())]);
        let mut top = IndexMap::new();
        top.insert("a".to_owned(), empty());
        top.insert("b".to_owned(), empty());
        let err = render_xml(&Value::Map(top), false).expect_err("refused");
        assert!(err.to_string().contains("holds 'a' and 'b'"), "{err}");

        let mut top = IndexMap::new();
        top.insert(KEY_CDATA.to_owned(), Value::Array(vec![Value::Int(1)]));
        top.insert("r".to_owned(), empty());
        let err = render_xml(&Value::Map(top), false).expect_err("refused");
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
        let err = render_xml(&v, false).expect_err("top-level non-map must reject");
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
        let err = render_xml(&Value::Map(top), false).expect_err("no root must reject");
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
