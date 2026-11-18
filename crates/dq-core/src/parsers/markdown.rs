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
use crate::ir::{InlineBaseline, Provenance, ProvenanceMap};
use crate::parsers::frontmatter::{
    detect_json_frontmatter, detect_toml_frontmatter, detect_yaml_frontmatter,
};
use crate::parsers::{Json, Toml, Yaml};
use crate::pointer::Pointer;

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
        // Build a provenance map enriched with inline-offset metadata for
        // every fenced code block — Phase 2 of `add-validation-and-extended-formats`
        // requires `inline_offset = Some(InlineBaseline { 0, 1, 1 })` so
        // composite-rule evaluation can project inner-document line / column
        // back to the outer-file with sub-line precision. Indented code
        // blocks (CommonMark §4.4) get `None`; the spec is explicit that
        // only fenced blocks opt in.
        let provenance = build_provenance_for_fenced_code_blocks(&value);
        Ok(Document::with_value_bytes_and_provenance(
            value,
            original,
            FormatTag::Markdown,
            provenance,
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
    restore_link_definition_starts(root, body_str);

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

/// Move the start of every paragraph / setext heading that was preceded by
/// consumed link reference definitions back onto the first definition, the
/// way comrak 0.52 reported it.
///
/// comrak strips leading `[label]: url` definitions out of a paragraph, and
/// since 1.0 the paragraph's `sourcepos.start` points at the first line left
/// after them; 0.52 reported the line of the first definition and the byte
/// column where it begins. The M9 IR has no node for a definition, so keeping
/// the 1.0 value would make the definition lines look like blank lines to
/// rules that measure gaps between blocks
/// (`@std/markdown/no-multiple-blank-lines`).
///
/// The consumed definitions are recovered from the source: they are the
/// non-blank lines directly above the node that lie inside the node's
/// containers and are claimed by no block node other than those containers,
/// because nothing but a consumed definition can sit there without being a
/// node of its own. Only the start is touched; `end` is left as comrak
/// reports it.
fn restore_link_definition_starts<'a>(root: &'a AstNode<'a>, source: &str) {
    let lines = split_lines(source);
    let cover = block_cover(root, lines.len());
    for node in root.descendants() {
        let restorable = match &node.data.borrow().value {
            NodeValue::Paragraph => true,
            // ATX headings interrupt a paragraph, so a definition above one
            // is a standalone block and not part of the heading.
            NodeValue::Heading(h) => h.setext,
            _ => false,
        };
        if !restorable {
            continue;
        }
        let start_line = node.data.borrow().sourcepos.start.line;
        let scope = ContainerScope::of(node);
        let Some((line, column)) = first_definition(&lines, &cover, &scope, start_line) else {
            continue;
        };
        let mut data = node.data.borrow_mut();
        data.sourcepos.start.line = line;
        data.sourcepos.start.column = column;
    }
}

/// Split `source` into lines the way comrak does: `\n`, `\r\n` and a lone
/// `\r` each end a line. Terminators are not part of the returned lines.
fn split_lines(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut line_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&source[line_start..i]);
                i += 1;
                line_start = i;
            }
            b'\r' => {
                lines.push(&source[line_start..i]);
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                line_start = i;
            }
            _ => i += 1,
        }
    }
    if line_start < bytes.len() {
        lines.push(&source[line_start..]);
    }
    lines
}

/// For every 1-based line, the number of block nodes (the document aside)
/// whose source range covers it, as comrak reported the ranges.
///
/// Ranges are source ranges, so a footnote definition that comrak moved to
/// the end of the tree still covers the lines it was written on.
fn block_cover<'a>(root: &'a AstNode<'a>, line_count: usize) -> Vec<usize> {
    let mut delta = vec![0_isize; line_count + 2];
    for node in root.descendants().skip(1) {
        let data = node.data.borrow();
        if !data.value.block() {
            continue;
        }
        let pos = data.sourcepos;
        let first = pos.start.line;
        let last = pos.end.line.min(line_count);
        if first == 0 || first > last {
            continue;
        }
        delta[first] += 1;
        delta[last + 1] -= 1;
    }
    let mut cover = Vec::with_capacity(delta.len());
    let mut running = 0_isize;
    for d in delta {
        running += d;
        cover.push(running.unsigned_abs());
    }
    cover
}

/// A container whose marker or indentation prefixes the source lines of the
/// blocks inside it.
#[derive(Debug, Clone, Copy)]
enum Container {
    BlockQuote,
    /// A list item opened on `line`; a task item also carries its `[ ]` box.
    Item {
        line: usize,
        task: bool,
    },
    /// A footnote definition whose `[^label]:` sits on `line`.
    Footnote {
        line: usize,
    },
}

/// Where a restorable block sits: its prefixing containers (outermost
/// first), how many block ancestors cover its lines, and the first line it
/// may extend up to.
#[derive(Debug)]
struct ContainerScope {
    containers: Vec<Container>,
    depth: usize,
    first_line: usize,
}

impl ContainerScope {
    fn of<'a>(node: &'a AstNode<'a>) -> Self {
        let mut containers = Vec::new();
        let mut depth = 0;
        let mut first_line = 1;
        for ancestor in node.ancestors().skip(1) {
            let data = ancestor.data.borrow();
            let line = data.sourcepos.start.line;
            let container = match &data.value {
                NodeValue::Document => break,
                NodeValue::BlockQuote => Some(Container::BlockQuote),
                NodeValue::Item(_) => Some(Container::Item { line, task: false }),
                NodeValue::TaskItem(_) => Some(Container::Item { line, task: true }),
                NodeValue::FootnoteDefinition(_) => Some(Container::Footnote { line }),
                _ => None,
            };
            if depth == 0 {
                first_line = line.max(1);
            }
            depth += 1;
            containers.extend(container);
        }
        containers.reverse();
        Self {
            containers,
            depth,
            first_line,
        }
    }
}

/// `(line, column)` of the first consumed link reference definition directly
/// above a block that comrak starts on `start_line`: 1-based line and byte
/// column of the definition's `[`. `None` when no definition was consumed.
fn first_definition(
    lines: &[&str],
    cover: &[usize],
    scope: &ContainerScope,
    start_line: usize,
) -> Option<(usize, usize)> {
    let mut first = None;
    for line in (scope.first_line..start_line).rev() {
        // A line claimed by a block outside the node's containers — the
        // previous sibling, or a footnote definition comrak moved to the end
        // of the tree — is not one of the node's definitions.
        if cover.get(line).copied().unwrap_or(0) > scope.depth {
            break;
        }
        let Some(offset) = lines
            .get(line - 1)
            .and_then(|text| content_offset(text, line, &scope.containers))
        else {
            break;
        };
        first = Some((line, offset));
    }
    let (line, offset) = first?;
    // The topmost line must open a definition; otherwise the span belongs to
    // a previous block whose `end` comrak under-reported. `[^label]:` opens
    // a footnote definition, never a link reference definition.
    let rest = &lines[line - 1][offset..];
    (rest.starts_with('[') && !rest.starts_with("[^")).then_some((line, offset + 1))
}

/// Byte offset of the first content character on source line `line_no`
/// (`text`) once the prefixes of `containers` are stripped, or `None` when
/// nothing but container markers and whitespace is left.
fn content_offset(text: &str, line_no: usize, containers: &[Container]) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut pos = if line_no == 1 && text.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    for container in containers {
        pos = skip_blanks(bytes, pos);
        match *container {
            Container::BlockQuote => {
                if bytes.get(pos) != Some(&b'>') {
                    // A lazy continuation line carries no further markers.
                    break;
                }
                pos += 1;
            }
            Container::Item { line, task } if line == line_no => {
                pos = skip_list_marker(bytes, pos);
                if task {
                    pos = skip_task_box(text, skip_blanks(bytes, pos));
                }
            }
            Container::Footnote { line } if line == line_no => {
                pos = skip_footnote_label(bytes, pos);
            }
            // Continuation lines of items and footnotes are only indented.
            Container::Item { .. } | Container::Footnote { .. } => {}
        }
    }
    let pos = skip_blanks(bytes, pos);
    (pos < bytes.len()).then_some(pos)
}

fn skip_blanks(bytes: &[u8], pos: usize) -> usize {
    pos + bytes.get(pos..).map_or(0, |rest| {
        rest.iter()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count()
    })
}

/// Skip a bullet (`-`, `+`, `*`) or ordered (`1.`, `1)`) list marker.
fn skip_list_marker(bytes: &[u8], pos: usize) -> usize {
    let rest = bytes.get(pos..).unwrap_or_default();
    match rest {
        [b'-' | b'+' | b'*', ..] => pos + 1,
        _ => {
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            match rest.get(digits) {
                Some(b'.' | b')') if digits > 0 => pos + digits + 1,
                _ => pos,
            }
        }
    }
}

/// Skip a task box (`[ ]`, `[x]`, any one character between the brackets).
fn skip_task_box(text: &str, pos: usize) -> usize {
    let Some(rest) = text.get(pos..) else {
        return pos;
    };
    let mut chars = rest.char_indices();
    match (chars.next(), chars.next(), chars.next()) {
        (Some((_, '[')), Some(_), Some((close, ']'))) => pos + close + 1,
        _ => pos,
    }
}

/// Skip a `[^label]:` footnote definition label.
fn skip_footnote_label(bytes: &[u8], pos: usize) -> usize {
    let rest = bytes.get(pos..).unwrap_or_default();
    if !rest.starts_with(b"[^") {
        return pos;
    }
    match rest.iter().position(|&b| b == b']') {
        Some(close) if rest.get(close + 1) == Some(&b':') => pos + close + 2,
        _ => pos,
    }
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
        NodeValue::ThematicBreak(_) => ("thematic_break", fields, true),
        NodeValue::HtmlBlock(h) => {
            fields.insert("value".to_owned(), Value::String(h.literal.clone()));
            ("html_block", fields, true)
        }
        NodeValue::Table(_) => {
            fields.insert("children".to_owned(), children_of(node));
            ("table", fields, true)
        }
        NodeValue::TableRow(row) => {
            fields.insert("header".to_owned(), Value::Bool(row.header));
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
        NodeValue::Emph(_) => {
            fields.insert("children".to_owned(), children_of(node));
            ("emphasis", fields, false)
        }
        NodeValue::Strong(_) => {
            fields.insert("children".to_owned(), children_of(node));
            ("strong", fields, false)
        }
        NodeValue::Strikethrough(_) => {
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

/// Walk `value` and build a [`ProvenanceMap`] that carries
/// `inline_offset = Some(InlineBaseline { 0, 1, 1 })` for every fenced code
/// block leaf's `value` field.
///
/// Phase 2 of `add-validation-and-extended-formats` requires the markdown
/// parser to surface inline-offset metadata for fenced code blocks so
/// composite-rule evaluation can project inner-document coordinates back
/// to the outer markdown source. The pointer addressed is the leaf string
/// holding the code body (`<parent>/value`) — that is where Phase 4's
/// composite extract reads the inner content from.
///
/// `span` is `None` for every entry: the markdown parser does not yet
/// participate in the write path (M9 contract — see [`Markdown::write`]),
/// so there is no [`crate::document::ValueSpan`] to attach. Composite-rule
/// evaluation falls back to the markdown node's `position` field for the
/// anchor coordinates, which is the source-of-truth for fenced-block
/// positions in the M9 AST.
fn build_provenance_for_fenced_code_blocks(value: &Value) -> ProvenanceMap {
    let mut map = ProvenanceMap::new();
    let mut path: Vec<String> = Vec::new();
    collect_fenced_code_block_provenance(value, &mut path, &mut map);
    map
}

/// Recursively walk the markdown AST `value` tree, looking for nodes whose
/// shape matches `Map { "type": "code_block", "fenced": Bool(true), ... }`
/// and emitting an inline-offset-aware [`Provenance::Original`] entry for
/// the leaf at `<this-node-path>/value`.
fn collect_fenced_code_block_provenance(
    value: &Value,
    path: &mut Vec<String>,
    map: &mut ProvenanceMap,
) {
    match value {
        Value::Map(m) => {
            if is_fenced_code_block(m) {
                // Record the inline-offset entry for the `value` field —
                // that is the body text the composite extract re-parses.
                path.push("value".to_owned());
                let pointer_canonical = canonical_pointer(path);
                let pointer = Pointer::parse(&pointer_canonical)
                    .expect("constructed canonical pointer must round-trip through Pointer::parse");
                map.insert(
                    pointer_canonical,
                    Provenance::Original {
                        pointer,
                        span: None,
                        inline_offset: Some(InlineBaseline {
                            byte_start: 0,
                            line: 1,
                            col: 1,
                        }),
                    },
                );
                path.pop();
            }
            // Recurse into every field — code blocks can be nested inside
            // list items, block quotes, table cells, etc.
            for (k, v) in m {
                path.push(pointer_escape(k));
                collect_fenced_code_block_provenance(v, path, map);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                path.push(idx.to_string());
                collect_fenced_code_block_provenance(item, path, map);
                path.pop();
            }
        }
        // Leaves: nothing further to enumerate.
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::BigInt(_)
        | Value::Float(_)
        | Value::BigFloat(_)
        | Value::String(_) => {}
    }
}

/// True iff `m` looks like the markdown AST's fenced-code-block node:
/// `{"type": "code_block", "fenced": true, ...}`. Indented (non-fenced)
/// code blocks have `fenced: false` and SHALL not carry an inline-offset
/// per the spec.
fn is_fenced_code_block(m: &IndexMap<String, Value>) -> bool {
    matches!(m.get("type"), Some(Value::String(s)) if s == "code_block")
        && matches!(m.get("fenced"), Some(Value::Bool(true)))
}

/// Build the canonical RFC 6901 pointer string from `segments`.
/// Empty `segments` yields the empty string (root); otherwise the result
/// is `/seg1/seg2/...`. Mirrors [`Pointer::as_canonical`] without
/// allocating an intermediate `Pointer`.
fn canonical_pointer(segments: &[String]) -> String {
    if segments.is_empty() {
        String::new()
    } else {
        let mut out = String::new();
        for seg in segments {
            out.push('/');
            out.push_str(seg);
        }
        out
    }
}

/// Apply RFC 6901 escaping to a single pointer segment: `~` → `~0`, `/` → `~1`.
/// Order matters — `~` MUST be escaped before `/`, otherwise we double-escape
/// the `~` we just emitted for `/`.
fn pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
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
    fn indented_code_block_position_ends_at_its_last_character() {
        // comrak 1.0: the block ends on its last line, not at the start of
        // the next one.
        let doc = Markdown.parse(b"    code\n    more\n").expect("parse");
        let position = field(&doc_children(&doc)[0], "position");
        let point = |key: &str| {
            (
                field(field(position, key), "line").clone(),
                field(field(position, key), "column").clone(),
            )
        };
        assert_eq!(point("start"), (Value::Int(1), Value::Int(5)));
        assert_eq!(point("end"), (Value::Int(2), Value::Int(8)));
    }

    /// `(line, column)` of `position.start` for the block child at `index`.
    fn start_of(doc: &Document, index: usize) -> (Value, Value) {
        let start = field(field(&doc_children(doc)[index], "position"), "start");
        (field(start, "line").clone(), field(start, "column").clone())
    }

    #[test]
    fn paragraph_start_points_at_first_consumed_link_definition() {
        // comrak 1.0 reports the line after the definitions; 0.52 reported
        // the first definition. The IR has no definition node, so the gap to
        // the heading must stay one blank line.
        let doc = Markdown
            .parse(b"# T\n\n[a]: http://a\n[b]: http://b\nSee [here](x).\n")
            .expect("parse");
        assert_eq!(start_of(&doc, 1), (Value::Int(3), Value::Int(1)));
        let doc = Markdown.parse(b"[ref]: http://r\nSee x.\n").expect("parse");
        assert_eq!(start_of(&doc, 0), (Value::Int(1), Value::Int(1)));
    }

    #[test]
    fn paragraph_start_ignores_definitions_separated_by_a_blank_line() {
        let doc = Markdown.parse(b"[a]: http://a\n\nSee x.\n").expect("parse");
        assert_eq!(start_of(&doc, 0), (Value::Int(3), Value::Int(1)));
    }

    #[test]
    fn paragraph_start_after_definition_keeps_indentation_and_crlf() {
        let doc = Markdown
            .parse(b"# T\r\n\r\n  [a]: http://a\r\nSee x.\r\n")
            .expect("parse");
        assert_eq!(start_of(&doc, 1), (Value::Int(3), Value::Int(3)));
    }

    #[test]
    fn nested_paragraph_start_points_at_consumed_link_definition() {
        let doc = Markdown
            .parse(b"> [a]: http://a\n> See x.\n")
            .expect("parse");
        let quote = &doc_children(&doc)[0];
        let Value::Array(items) = field(quote, "children") else {
            panic!("block_quote children must be an array");
        };
        let start = field(field(&items[0], "position"), "start");
        assert_eq!(field(start, "line"), &Value::Int(1));
        assert_eq!(field(start, "column"), &Value::Int(3));
    }

    #[test]
    fn setext_heading_start_points_at_first_consumed_link_definition() {
        let doc = Markdown
            .parse(b"[a]: http://a\nTitle\n=====\n")
            .expect("parse");
        assert_eq!(start_of(&doc, 0), (Value::Int(1), Value::Int(1)));
    }

    #[test]
    fn atx_heading_start_is_not_moved_onto_a_preceding_definition() {
        let doc = Markdown.parse(b"[a]: http://a\n# H\n").expect("parse");
        assert_eq!(start_of(&doc, 0), (Value::Int(2), Value::Int(1)));
    }

    /// `(line, column)` of `position.start` for the node reached from the
    /// document by following `path` through `children` arrays.
    fn start_at(src: &[u8], path: &[usize]) -> (i64, i64) {
        let doc = Markdown.parse(src).expect("parse");
        let mut node = doc.value();
        for &index in path {
            let Value::Array(children) = field(node, "children") else {
                panic!("children must be an array");
            };
            node = &children[index];
        }
        let start = field(field(node, "position"), "start");
        let (Value::Int(line), Value::Int(column)) = (field(start, "line"), field(start, "column"))
        else {
            panic!("start must carry integer line and column");
        };
        (*line, *column)
    }

    // comrak moves footnote definitions to the end of the tree, so the tree
    // sibling before these paragraphs is the block above the footnote.

    #[test]
    fn paragraph_after_footnote_with_atx_heading_keeps_its_start() {
        let src = b"# T\n\nx[^1]\n\n[^1]: # H\npara\n";
        assert_eq!(start_at(src, &[2]), (6, 1));
    }

    #[test]
    fn paragraph_after_footnote_with_fenced_code_keeps_its_start() {
        let src = b"# T\n\nx[^1]\n\n[^1]:\n    ```\n    c\n    ```\npara\n";
        assert_eq!(start_at(src, &[2]), (9, 1));
    }

    #[test]
    fn definition_scan_stops_at_lines_of_a_preceding_block() {
        // The setext heading's lines are not definitions, even though its
        // first line opens with `[`.
        assert_eq!(start_at(b"[x] title\n===\n[a]: /u\npara\n", &[1]), (3, 1));
        // Same for a footnote definition comrak moved to the end of the tree.
        let src = b"# T\n\nx[^1]\n\n[^1]: [y] t\n    ===\n[a]: /u\npara\n";
        assert_eq!(start_at(src, &[2]), (7, 1));
    }

    #[test]
    fn block_inside_footnote_is_not_moved_onto_the_footnote_label() {
        assert_eq!(start_at(b"x[^1]\n\n[^1]:\n    para\n", &[1, 0]), (4, 5));
        assert_eq!(
            start_at(b"x[^1]\n\n[^1]:\n    T\n    ==\n", &[1, 0]),
            (4, 5)
        );
    }

    #[test]
    fn paragraph_inside_footnote_starts_at_its_consumed_definition() {
        let src = b"x[^1]\n\n[^1]:\n    [a]: /u\n    para\n";
        assert_eq!(start_at(src, &[1, 0]), (4, 5));
        // A definition on the label line itself, after `[^1]: `.
        let src = b"[^1]: [a]: /u\n    para\n\nx[^1]\n";
        assert_eq!(start_at(src, &[1, 0]), (1, 7));
    }

    #[test]
    fn item_paragraph_after_a_bare_marker_line_starts_at_its_definition() {
        assert_eq!(start_at(b"-\n  [a]: /u\n  b\n", &[0, 0, 0]), (2, 3));
        assert_eq!(start_at(b"- \n  [a]: /u\n  b\n", &[0, 0, 0]), (2, 3));
        assert_eq!(start_at(b"1.\n   [a]: /u\n   b\n", &[0, 0, 0]), (2, 4));
    }

    #[test]
    fn paragraph_start_after_definition_with_cr_only_line_breaks() {
        assert_eq!(start_at(b"# T\r\r[a]: /u\rpara\r", &[1]), (3, 1));
    }

    #[test]
    fn paragraph_start_after_definition_counts_the_bom_in_the_column() {
        assert_eq!(start_at("\u{feff}[a]: /u\npara\n".as_bytes(), &[0]), (1, 4));
    }

    #[test]
    fn nested_paragraph_start_takes_the_definition_column() {
        // Lazy continuation: the definition sits after `> `.
        assert_eq!(start_at(b"> [a]: /u\npara\n", &[0, 0]), (1, 3));
        assert_eq!(
            start_at(b"> > x\n> >\n> >  [a]: /u\n> > p\n", &[0, 0, 1]),
            (3, 6)
        );
        // The definition is indented past the item's content column.
        assert_eq!(start_at(b"- x\n\n   [a]: /u\n  p\n", &[0, 0, 1]), (3, 4));
    }

    #[test]
    fn split_lines_breaks_on_lf_crlf_and_lone_cr() {
        assert_eq!(split_lines("a\nb\r\nc\rd"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_lines("a\n\n"), vec!["a", ""]);
        assert!(split_lines("").is_empty());
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
        let mutated = Document::with_spans(
            Value::Map(m),
            doc.original_bytes().to_vec(),
            crate::document::SpanMap::new(),
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

    // -- Phase 2 (`add-validation-and-extended-formats`) ------------------
    //
    // Inline-offset population for markdown fenced code blocks. Spec
    // ("Markdown fenced code block carries inline-offset") requires every
    // fenced code block leaf to surface
    // `inline_offset = Some(InlineBaseline { byte_start: 0, line: 1, col: 1 })`
    // on its `Provenance::Original` entry; indented code blocks (CommonMark
    // §4.4) keep `inline_offset = None` because composite-rule extracts
    // address only fenced blocks via the language tag.

    /// Lookup helper: pattern-match the provenance entry at `pointer_str`
    /// and return its `inline_offset` (or `None` if the entry is absent or
    /// is `Synthetic`).
    fn inline_offset_for(doc: &Document, pointer_str: &str) -> Option<InlineBaseline> {
        let pointer = Pointer::parse(pointer_str).expect("pointer parses");
        match doc.as_ir().provenance_for(&pointer) {
            Some(Provenance::Original { inline_offset, .. }) => *inline_offset,
            Some(Provenance::Synthetic { .. }) => None,
            None => None,
        }
    }

    #[test]
    fn fenced_code_block_value_field_carries_inline_offset() {
        // Single fenced code block with a YAML payload — composite-rule
        // evaluation reads `/children/0/value` and re-parses the body as
        // YAML. The inline-offset baseline lets it project the YAML's
        // inner (line, col) back to the markdown source-file coordinates.
        let src = "```yaml\nfoo: bar\n```\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        assert_eq!(
            inline_offset_for(&doc, "/children/0/value"),
            Some(InlineBaseline {
                byte_start: 0,
                line: 1,
                col: 1,
            }),
            "fenced code block's `value` MUST carry inline_offset = Some(0,1,1)",
        );
    }

    #[test]
    fn indented_code_block_value_field_has_no_inline_offset() {
        // Four-space-indented code block (`fenced: false`). Spec is
        // explicit: only fenced blocks opt in. A regression that started
        // emitting an inline-offset for indented blocks would change the
        // composite-rule extract surface.
        let src = "    foo bar\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        // The indented code block lives at `/children/0` but should NOT
        // surface in the provenance map at all.
        let pointer = Pointer::parse("/children/0/value").expect("pointer parses");
        assert!(
            doc.as_ir().provenance_for(&pointer).is_none(),
            "indented code block MUST NOT carry inline-offset metadata",
        );
    }

    #[test]
    fn multiple_fenced_code_blocks_each_carry_inline_offset() {
        // Two top-level fenced code blocks. Both must carry the baseline,
        // addressed through their distinct child indices.
        let src = "```yaml\nfoo: bar\n```\n\n```toml\nkey = 1\n```\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        for pointer_str in ["/children/0/value", "/children/1/value"] {
            assert_eq!(
                inline_offset_for(&doc, pointer_str),
                Some(InlineBaseline {
                    byte_start: 0,
                    line: 1,
                    col: 1,
                }),
                "fenced code block at `{pointer_str}` MUST carry inline_offset",
            );
        }
    }

    #[test]
    fn fenced_code_block_inside_list_item_carries_inline_offset() {
        // The walker is recursive; a fenced block nested inside a list item
        // must also surface inline-offset. The exact pointer depends on the
        // markdown AST shape, so we walk the provenance map and assert at
        // least one entry exists with the expected baseline.
        let src = "- item\n\n  ```yaml\n  foo: bar\n  ```\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let baseline_count = doc
            .as_ir()
            .provenance()
            .values()
            .filter(|p| {
                matches!(
                    p,
                    Provenance::Original {
                        inline_offset: Some(InlineBaseline {
                            byte_start: 0,
                            line: 1,
                            col: 1,
                        }),
                        ..
                    },
                )
            })
            .count();
        assert!(
            baseline_count >= 1,
            "at least one fenced code block (nested or top-level) MUST surface \
             inline-offset metadata; got {baseline_count} entries",
        );
    }

    #[test]
    fn fenced_code_block_inline_offset_via_ir_helper() {
        // Cross-check that the public `Ir::inline_offset_for` helper
        // returns the same baseline as direct provenance pattern-matching.
        let src = "```yaml\nfoo: bar\n```\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let pointer = Pointer::parse("/children/0/value").expect("pointer");
        let expected = InlineBaseline {
            byte_start: 0,
            line: 1,
            col: 1,
        };
        assert_eq!(doc.as_ir().inline_offset_for(&pointer), Some(&expected));
    }

    #[test]
    fn paragraph_text_carries_no_inline_offset() {
        // Plain paragraphs / headings / etc. must NOT carry an inline-offset
        // entry — only fenced code blocks opt in. The provenance map for
        // a paragraph-only doc should be empty.
        let src = "# Title\n\nSome text.\n";
        let doc = Markdown.parse(src.as_bytes()).expect("parse");
        let any_inline_offset = doc.as_ir().provenance().values().any(|p| {
            matches!(
                p,
                Provenance::Original {
                    inline_offset: Some(_),
                    ..
                },
            )
        });
        assert!(
            !any_inline_offset,
            "paragraph-only / heading-only markdown documents MUST NOT \
             carry inline-offset metadata",
        );
    }
}
