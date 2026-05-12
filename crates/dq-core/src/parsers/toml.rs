//! TOML 1.0 parser and writer (textual-edit round-trip via `toml_edit`).
//!
//! Read-pat M1 used the serde-style `toml` crate. M2 §4 replaces it with
//! `toml_edit::DocumentMut`, which is itself a textual-edit DOM: it stores
//! byte spans alongside every parsed node so the `Document::set_at` splice
//! path can splice replacement bytes into `original_bytes` while preserving
//! comments, key order, quote style, and number / datetime literals — the
//! same way `cargo` mutates `Cargo.toml`.
//!
//! # Pointer namespace
//!
//! Tables nest, so `[server.tls]` with `cert = "x"` produces pointer
//! `/server/tls/cert`. Arrays of tables (`[[products]]`) use the canonical
//! `/products/0/name` form, the same shape YAML sequences use.
//!
//! # mkdir-p / structural inserts
//!
//! Per design D2 the §4 baseline takes a pragmatic shortcut for missing
//! pointers: rather than synthesizing `key = value\n` insertion bytes the
//! way the YAML path does, we let `toml_edit` mutate the parsed
//! `DocumentMut` in place, re-render the whole document, and rebuild the
//! `SpanMap`. The §3 YAML path stays span-edit-only (it has to — its
//! parser does not own a writer); TOML benefits from the `toml_edit`
//! internal API and the cost is paid only on inserts. **The §4
//! implementation does NOT yet exercise this path** — `Document::set_at`'s
//! mkdir-p branch returns `Error::Path { kind: MissingKey }` for both YAML
//! and TOML in the M2 baseline; the renderer factory only supplies the
//! in-span splice path. The pragma is documented here so the §4-follow-up
//! that wires up TOML insertion knows to plug into `DocumentMut::insert`
//! / re-render rather than synthesizing bytes.
//!
//! # What is NOT here
//!
//! - Round-trip rendering of `toml_edit::Datetime`. The §4 baseline maps
//!   datetime literals to `Value::String(literal_text)` so the `dq` model
//!   can carry them; replacement of a datetime span downgrades the value
//!   to a basic-string scalar (the literal text is preserved when the
//!   value is unchanged because the splice never fires). M3 will revisit
//!   if a real-world case demands a dedicated `Value::Datetime` variant.
//! - Multi-line literal-string round-trip on replacement. The §4 baseline
//!   downgrades any replaced string to a basic `"..."` form (or to
//!   `'...'` when the original was single-quoted / literal-quoted on a
//!   single line). M3 will revisit.

use std::io::Write;
use std::ops::Range;

use indexmap::IndexMap;
use toml_edit::{
    Array as TomlArray, ArrayOfTables, ImDocument, InlineTable, Item, Table, Value as TomlValue,
};

use crate::Result;
use crate::WriteOptions;
use crate::document::spans::{SpanContext, SpanMap, ValueSpan};
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;
use crate::textual_edit::{InsertionRenderer, ScalarRenderer};
use crate::write_options::canonicalize_keys;

/// TOML format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Toml;

impl Format for Toml {
    fn name(&self) -> &'static str {
        "toml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in TOML input: {e}"),
        })?;
        // We parse via `ImDocument` (immutable) rather than `DocumentMut`
        // because only `ImDocument::parse` preserves the byte spans on
        // every node (see toml_edit 0.22 docs — `DocumentMut::from_str`
        // explicitly drops span information). The §4 baseline never
        // mutates the parsed DOM — splices land in `original_bytes` —
        // so the lack of mutation API is no constraint.
        let doc = ImDocument::parse(text).map_err(|e| {
            // `toml_edit::TomlError::span()` returns the byte range of the
            // failing construct. The 0.22 API does not expose a `line_col`
            // helper, so we re-derive the 1-indexed position by counting
            // newlines up to the span's start. `message()` is the canonical
            // human-friendly summary the CLI's diagnostic renderer expects.
            let span = e.span().map(|r| r.start..r.end).unwrap_or(0..0);
            let (line, col) = derive_line_col(bytes, span.start);
            let snippet = extract_line_snippet(bytes, span.start);
            Error::Parse {
                file: None,
                line,
                col,
                span,
                snippet,
                message: e.message().to_owned(),
            }
        })?;

        let mut spans = SpanMap::new();
        let value = build_value_and_spans(&doc, bytes, &mut spans);

        Ok(Document::with_spans(
            value,
            bytes.to_vec(),
            spans,
            FormatTag::Toml,
        ))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        if doc.is_multi() {
            return Err(Error::Format {
                format: "toml",
                message: "multi-document streams are not representable in TOML".to_owned(),
            });
        }
        // Write-aware path: `original_bytes` already reflects every prior
        // `set_at` / `del_at` splice, so a verbatim copy is the round-trip
        // contract. This is the cargo-style preservation of comments / key
        // order / formatting that the §4 design called for.
        if !doc.original_bytes().is_empty() {
            return w
                .write_all(doc.original_bytes())
                .map_err(|source| Error::Io {
                    path: camino::Utf8PathBuf::from("<toml-writer>"),
                    source,
                });
        }
        // `value_only` fallback: the document was not produced by this
        // parser (e.g. `dq convert` from YAML into TOML, or the CLI's
        // `TomlReporter` rendering an arbitrary `serde_json::Value`). We
        // fall back to the serde-style `toml` crate's pretty-printer; it
        // does not carry formatting metadata, but the input doesn't
        // either, so there's nothing to preserve.
        let v = doc.value();
        let toml_value = value_to_toml(v)?;
        let s = toml::to_string_pretty(&toml_value).map_err(|e| Error::Format {
            format: "toml",
            message: e.to_string(),
        })?;
        w.write_all(s.as_bytes()).map_err(|source| Error::Io {
            path: camino::Utf8PathBuf::from("<toml-writer>"),
            source,
        })
    }

    fn write_with_options(
        &self,
        doc: &Document,
        w: &mut dyn Write,
        opts: &WriteOptions,
    ) -> Result<()> {
        // TOML ignores `opts.indent` in M4 (the `toml` crate's pretty-printer
        // does not expose a configurable indent step). When `sort_keys` is
        // false we delegate to `write` so the bytes match the M2 baseline
        // (including `original_bytes` verbatim for write-aware documents).
        if !opts.sort_keys {
            return self.write(doc, w);
        }
        if doc.is_multi() {
            return Err(Error::Format {
                format: "toml",
                message: "multi-document streams are not representable in TOML".to_owned(),
            });
        }
        // Sort the value tree at every depth, then re-emit through the
        // serde-style `toml` crate. Re-emitting through the `value_only`
        // fallback path is the documented strategy for `--sort-keys`:
        // canonicalising the source `Value` tree first is cleaner than
        // asking `toml_edit` to sort in place because the latter would
        // require special-casing tables / arrays-of-tables / inline tables,
        // and the round-trip through `toml::to_string_pretty` produces a
        // canonical TOML document with no comments preserved (which is the
        // expected tradeoff: `--sort-keys` is a re-emit knob, not a splice).
        let canon = canonicalize_keys(doc.value());
        let toml_value = value_to_toml(&canon)?;
        let s = toml::to_string_pretty(&toml_value).map_err(|e| Error::Format {
            format: "toml",
            message: e.to_string(),
        })?;
        w.write_all(s.as_bytes()).map_err(|source| Error::Io {
            path: camino::Utf8PathBuf::from("<toml-writer>"),
            source,
        })
    }
}

// ---------------------------------------------------------------------------
// Span builder + Value construction
// ---------------------------------------------------------------------------

/// Walk the parsed document, populating `spans` with one [`ValueSpan`] per
/// scalar leaf and returning the equivalent [`Value`] tree.
fn build_value_and_spans<S: AsRef<str>>(
    doc: &ImDocument<S>,
    bytes: &[u8],
    spans: &mut SpanMap,
) -> Value {
    let mut path: Vec<String> = Vec::new();
    let mut map: IndexMap<String, Value> = IndexMap::new();
    walk_table(doc.as_table(), bytes, &mut path, spans, &mut map);
    Value::Map(map)
}

/// Walk a `[table]` (or the top-level document table). Children are pushed
/// onto `out` in source order; nested tables and arrays-of-tables descend
/// recursively, scalars record a span and produce a leaf value.
fn walk_table(
    table: &Table,
    bytes: &[u8],
    path: &mut Vec<String>,
    spans: &mut SpanMap,
    out: &mut IndexMap<String, Value>,
) {
    for (key, item) in table.iter() {
        path.push(pointer_escape(key));
        match item {
            Item::None => {
                // `Item::None` is `toml_edit`'s placeholder for keys removed
                // during mutation; on a freshly parsed document this is
                // unreachable, but matching it explicitly keeps the walk
                // total. Skip silently — no value to record.
            }
            Item::Value(v) => {
                let value = walk_value(v, bytes, path, spans);
                out.insert(key.to_owned(), value);
            }
            Item::Table(child) => {
                let mut child_map: IndexMap<String, Value> = IndexMap::new();
                walk_table(child, bytes, path, spans, &mut child_map);
                out.insert(key.to_owned(), Value::Map(child_map));
            }
            Item::ArrayOfTables(arr) => {
                let value = walk_array_of_tables(arr, bytes, path, spans);
                out.insert(key.to_owned(), value);
            }
        }
        path.pop();
    }
}

/// Walk an inline `Value` (right-hand side of `key = value`).
fn walk_value(
    value: &TomlValue,
    bytes: &[u8],
    path: &mut Vec<String>,
    spans: &mut SpanMap,
) -> Value {
    match value {
        TomlValue::String(s) => {
            record_scalar(value.span(), bytes, path, SpanContext::BlockMapValue, spans);
            Value::String(s.value().clone())
        }
        TomlValue::Integer(i) => {
            record_scalar(value.span(), bytes, path, SpanContext::BlockMapValue, spans);
            Value::Int(*i.value())
        }
        TomlValue::Float(f) => {
            record_scalar(value.span(), bytes, path, SpanContext::BlockMapValue, spans);
            Value::Float(*f.value())
        }
        TomlValue::Boolean(b) => {
            record_scalar(value.span(), bytes, path, SpanContext::BlockMapValue, spans);
            Value::Bool(*b.value())
        }
        TomlValue::Datetime(dt) => {
            // Datetime literals are preserved as the original textual form
            // (date-time, local-date-time, date, time). Record the span so
            // the literal byte range is known; downgrade to `Value::String`
            // since the `dq` model has no `Datetime` variant. When the
            // value is read and written back unchanged, the splice never
            // fires and the literal is preserved byte-for-byte.
            record_scalar(value.span(), bytes, path, SpanContext::BlockMapValue, spans);
            Value::String(dt.value().to_string())
        }
        TomlValue::Array(arr) => walk_inline_array(arr, bytes, path, spans),
        TomlValue::InlineTable(it) => walk_inline_table(it, bytes, path, spans),
    }
}

/// Record a scalar's `ValueSpan` if `toml_edit` reported a byte span. The
/// 0.22 API guarantees `span()` is `Some` for parsed values; the `None`
/// branch only fires for synthesized values produced by mutation, which
/// cannot occur here because we only walk freshly-parsed documents.
fn record_scalar(
    span: Option<Range<usize>>,
    bytes: &[u8],
    path: &[String],
    context: SpanContext,
    spans: &mut SpanMap,
) {
    let Some(value_range) = span else { return };
    let pointer = pointer_for(path);
    let line_range = compute_line_range(bytes, &value_range, context);
    let indent = compute_indent(bytes, value_range.start);
    spans.insert(
        pointer,
        ValueSpan {
            value_range,
            line_range,
            indent,
            context,
        },
    );
}

fn walk_inline_array(
    arr: &TomlArray,
    bytes: &[u8],
    path: &mut Vec<String>,
    spans: &mut SpanMap,
) -> Value {
    if arr.is_empty() {
        // Record an empty-container span for the empty inline array `[]`
        // so the empty-parent mkdir-p path in
        // [`crate::document::Document::set_at`] can locate the `[]` body.
        record_empty_container(arr.span(), bytes, path, SpanContext::FlowSeqItem, spans);
        return Value::Array(Vec::new());
    }
    let mut out: Vec<Value> = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        path.push(idx.to_string());
        // Inline-array items live inside `[ ... ]` — flow context.
        let value = walk_value_with_context(item, bytes, path, spans, SpanContext::FlowSeqItem);
        out.push(value);
        path.pop();
    }
    Value::Array(out)
}

fn walk_inline_table(
    it: &InlineTable,
    bytes: &[u8],
    path: &mut Vec<String>,
    spans: &mut SpanMap,
) -> Value {
    if it.is_empty() {
        // Empty inline table `{}` — see [`walk_inline_array`] for the
        // rationale. The splicer uses the recorded span as its anchor.
        record_empty_container(it.span(), bytes, path, SpanContext::FlowMapValue, spans);
        return Value::Map(IndexMap::new());
    }
    let mut out: IndexMap<String, Value> = IndexMap::new();
    for (key, value) in it.iter() {
        path.push(pointer_escape(key));
        let v = walk_value_with_context(value, bytes, path, spans, SpanContext::FlowMapValue);
        out.insert(key.to_owned(), v);
        path.pop();
    }
    Value::Map(out)
}

/// Record an empty-container [`ValueSpan`] at the current `path`'s pointer.
///
/// Mirrors [`record_scalar`] but is invoked for empty `{}` / `[]` literals.
/// The span's `value_range` covers the literal **including** both
/// delimiters; the empty-parent splicer in
/// [`crate::document::Document::set_at`] anchors the new key inside it.
fn record_empty_container(
    span: Option<Range<usize>>,
    bytes: &[u8],
    path: &[String],
    context: SpanContext,
    spans: &mut SpanMap,
) {
    let Some(value_range) = span else { return };
    let pointer = pointer_for(path);
    let line_range = compute_line_range(bytes, &value_range, context);
    let indent = compute_indent(bytes, value_range.start);
    spans.insert(
        pointer,
        ValueSpan {
            value_range,
            line_range,
            indent,
            context,
        },
    );
}

fn walk_value_with_context(
    value: &TomlValue,
    bytes: &[u8],
    path: &mut Vec<String>,
    spans: &mut SpanMap,
    context: SpanContext,
) -> Value {
    match value {
        TomlValue::String(s) => {
            record_scalar(value.span(), bytes, path, context, spans);
            Value::String(s.value().clone())
        }
        TomlValue::Integer(i) => {
            record_scalar(value.span(), bytes, path, context, spans);
            Value::Int(*i.value())
        }
        TomlValue::Float(f) => {
            record_scalar(value.span(), bytes, path, context, spans);
            Value::Float(*f.value())
        }
        TomlValue::Boolean(b) => {
            record_scalar(value.span(), bytes, path, context, spans);
            Value::Bool(*b.value())
        }
        TomlValue::Datetime(dt) => {
            record_scalar(value.span(), bytes, path, context, spans);
            Value::String(dt.value().to_string())
        }
        TomlValue::Array(arr) => walk_inline_array(arr, bytes, path, spans),
        TomlValue::InlineTable(it) => walk_inline_table(it, bytes, path, spans),
    }
}

fn walk_array_of_tables(
    arr: &ArrayOfTables,
    bytes: &[u8],
    path: &mut Vec<String>,
    spans: &mut SpanMap,
) -> Value {
    let mut out: Vec<Value> = Vec::with_capacity(arr.len());
    for (idx, table) in arr.iter().enumerate() {
        path.push(idx.to_string());
        let mut child_map: IndexMap<String, Value> = IndexMap::new();
        walk_table(table, bytes, path, spans, &mut child_map);
        out.push(Value::Map(child_map));
        path.pop();
    }
    Value::Array(out)
}

/// Build the canonical RFC 6901 pointer for `path`. Empty path → root (`""`).
fn pointer_for(path: &[String]) -> String {
    if path.is_empty() {
        String::new()
    } else {
        let mut out = String::new();
        for seg in path {
            out.push('/');
            out.push_str(seg);
        }
        out
    }
}

/// RFC 6901 escaping: `~` → `~0`, `/` → `~1`. Order matters.
fn pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Compute the physical line range covering `value_range`. For block
/// (`BlockMapValue`) values this expands to start-of-line / end-of-line +
/// trailing newline so `del_at` can splice out the whole logical entry.
/// Flow contexts (inside `{}` / `[]`) degenerate to the value range — there
/// is no "delete this physical line" semantics inside an inline container.
fn compute_line_range(
    bytes: &[u8],
    value_range: &Range<usize>,
    context: SpanContext,
) -> Range<usize> {
    if matches!(
        context,
        SpanContext::FlowMapValue | SpanContext::FlowSeqItem
    ) {
        return value_range.clone();
    }
    let mut start = value_range.start.min(bytes.len());
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    let mut end = value_range.end.min(bytes.len());
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    if end < bytes.len() {
        end += 1; // include trailing newline so del_at removes the whole line
    }
    start..end
}

/// Compute the indentation (in source bytes) of the line that contains
/// `index`. Used to seed `ValueSpan.indent` for renderers that emit
/// inserted siblings at the same column.
fn compute_indent(bytes: &[u8], index: usize) -> u32 {
    let cap = bytes.len();
    let mut line_start = index.min(cap);
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    let mut indent = 0_u32;
    for &b in &bytes[line_start..cap.min(index)] {
        if b == b' ' || b == b'\t' {
            indent = indent.saturating_add(1);
        } else {
            break;
        }
    }
    indent
}

/// Derive a 1-indexed `(line, col)` for byte offset `idx`. Used by the
/// parse-error path because `toml_edit::TomlError` only exposes a byte span,
/// not a logical position.
fn derive_line_col(bytes: &[u8], idx: usize) -> (u32, u32) {
    let cap = idx.min(bytes.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for &b in &bytes[..cap] {
        if b == b'\n' {
            line = line.saturating_add(1);
            col = 1;
        } else {
            col = col.saturating_add(1);
        }
    }
    (line, col)
}

/// Extract the line containing byte offset `idx` for error rendering.
fn extract_line_snippet(bytes: &[u8], idx: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let mut start = idx.min(bytes.len().saturating_sub(1));
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    let mut end = idx.min(bytes.len());
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

// ---------------------------------------------------------------------------
// `value_only` write fallback (toml crate)
// ---------------------------------------------------------------------------

/// Map `dq_core::Value` → `toml::Value` for the `value_only` write fallback.
fn value_to_toml(v: &Value) -> Result<toml::Value> {
    Ok(match v {
        Value::Null => {
            return Err(Error::Format {
                format: "toml",
                message: "TOML cannot represent null values".to_owned(),
            });
        }
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Int(i) => toml::Value::Integer(*i),
        Value::BigInt(s) | Value::BigFloat(s) => {
            return Err(Error::Format {
                format: "toml",
                message: format!("TOML cannot represent arbitrary-precision number {s}"),
            });
        }
        Value::Float(f) => toml::Value::Float(*f),
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(value_to_toml(item)?);
            }
            toml::Value::Array(out)
        }
        Value::Map(map) => {
            let mut table = toml::map::Map::new();
            for (k, v) in map {
                table.insert(k.clone(), value_to_toml(v)?);
            }
            toml::Value::Table(table)
        }
    })
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

/// Renderer for in-span scalar replacements in TOML documents.
///
/// Style preservation:
///
/// - Strings: keep the original quote style when possible. `'literal'`
///   stays single-quoted; `"basic"` stays double-quoted. Multi-line forms
///   (`'''...'''` / `"""..."""`) downgrade to single-line on replacement —
///   the §4 baseline keeps every other byte intact, so the cost of the
///   downgrade is localised to the edited line. M3 revisits.
/// - Bool / Int / Float: rendered via standard `Display`.
/// - BigInt / BigFloat: rendered as the literal text. TOML accepts integer
///   and float literals that fit `i64` / `f64`; values outside that range
///   round-trip through `Value::BigInt(s)` and the splice writes `s`
///   verbatim, so a parser-aware `dq` user can preserve precision by
///   passing the literal as a string.
#[derive(Debug, Default, Clone, Copy)]
pub struct TomlScalarRenderer;

impl ScalarRenderer for TomlScalarRenderer {
    fn render_replacement(&self, value: &Value, context: SpanContext, original: &[u8]) -> Vec<u8> {
        let original_style = detect_string_style(original);
        match value {
            Value::Null => {
                // TOML has no null. Emit an empty basic string so the splice
                // is at least well-formed; the in-memory `Value` mismatch
                // surfaces downstream as a normal lookup result.
                b"\"\"".to_vec()
            }
            Value::Bool(b) => {
                if *b {
                    b"true".to_vec()
                } else {
                    b"false".to_vec()
                }
            }
            Value::Int(i) => i.to_string().into_bytes(),
            Value::BigInt(s) => s.clone().into_bytes(),
            Value::Float(f) => render_float(*f),
            Value::BigFloat(s) => s.clone().into_bytes(),
            Value::String(s) => render_string(s, original_style, context),
            // Replacing a scalar span with a structural value at this layer
            // is out of scope (D14 / mkdir-p path). Fall through to a
            // well-formed empty string to keep the splice valid.
            Value::Array(_) | Value::Map(_) => b"\"\"".to_vec(),
        }
    }
}

/// Renderer for brand-new key/value pairs inserted into a parent container.
///
/// The §4 baseline produces a minimal `key = value\n` form for top-level /
/// nested-table contexts. Inline (flow) contexts are not yet wired into the
/// `Document::set_at` mkdir-p path for TOML; if they ever fire, the renderer
/// emits the same `key = value\n` shape (the caller is responsible for
/// stripping the newline before splicing into a `{ ... }` body).
#[derive(Debug, Default, Clone, Copy)]
pub struct TomlInsertionRenderer;

impl InsertionRenderer for TomlInsertionRenderer {
    fn render_insertion(
        &self,
        key: &str,
        value: &Value,
        parent_indent: u32,
        parent_context: SpanContext,
    ) -> Vec<u8> {
        let _ = parent_indent;
        let _ = parent_context;
        let mut out = Vec::new();
        out.extend_from_slice(toml_key_token(key).as_bytes());
        out.extend_from_slice(b" = ");
        out.extend_from_slice(&render_inline_scalar(value));
        out.push(b'\n');
        out
    }
}

/// Render a TOML key. Bare keys are `[A-Za-z0-9_-]+`; anything else is
/// quoted as a basic string.
fn toml_key_token(key: &str) -> String {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        let mut out = String::with_capacity(key.len() + 2);
        out.push('"');
        for c in key.chars() {
            push_basic_string_char(&mut out, c);
        }
        out.push('"');
        out
    } else {
        key.to_owned()
    }
}

/// Render a leaf value as its inline TOML form. Containers fall back to an
/// empty inline literal so the result is well-formed; full nested rendering
/// is the caller's responsibility.
fn render_inline_scalar(value: &Value) -> Vec<u8> {
    match value {
        Value::Null => b"\"\"".to_vec(),
        Value::Bool(b) => {
            if *b {
                b"true".to_vec()
            } else {
                b"false".to_vec()
            }
        }
        Value::Int(i) => i.to_string().into_bytes(),
        Value::BigInt(s) => s.clone().into_bytes(),
        Value::Float(f) => render_float(*f),
        Value::BigFloat(s) => s.clone().into_bytes(),
        Value::String(s) => render_basic_string(s),
        Value::Array(_) => b"[]".to_vec(),
        Value::Map(_) => b"{}".to_vec(),
    }
}

/// Style detected from the first non-whitespace byte of an existing scalar
/// span. Used by `TomlScalarRenderer` to keep quote style stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringStyle {
    /// Bare scalar (number / bool / datetime / unquoted identifier — the
    /// renderer never returns this for `Value::String`, but the discriminant
    /// is useful when classifying the original literal).
    Bare,
    /// Basic string (`"..."`).
    Basic,
    /// Literal string (`'...'`).
    Literal,
}

fn detect_string_style(bytes: &[u8]) -> StringStyle {
    let first = bytes.iter().find(|b| !b.is_ascii_whitespace());
    match first {
        Some(b'\'') => StringStyle::Literal,
        Some(b'"') => StringStyle::Basic,
        _ => StringStyle::Bare,
    }
}

/// Render an `f64` as a TOML float literal. NaN / infinity use TOML's
/// canonical forms; finite values use `Display` and add `.0` when the
/// printed form would otherwise look like an integer.
fn render_float(f: f64) -> Vec<u8> {
    if f.is_nan() {
        return b"nan".to_vec();
    }
    if f.is_infinite() {
        return if f.is_sign_negative() {
            b"-inf".to_vec()
        } else {
            b"inf".to_vec()
        };
    }
    let s = f.to_string();
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s.into_bytes()
    } else {
        format!("{s}.0").into_bytes()
    }
}

/// Render a string scalar matching the original quote style. Falls back to
/// a basic string when the original was bare or when the literal style
/// cannot represent the new contents (literal strings cannot contain `'`).
fn render_string(s: &str, original_style: StringStyle, context: SpanContext) -> Vec<u8> {
    let _ = context;
    match original_style {
        StringStyle::Literal if !s.contains('\'') && !s.contains('\n') => render_literal_string(s),
        // Any other case: emit a basic (`"..."`) string.
        _ => render_basic_string(s),
    }
}

fn render_basic_string(s: &str) -> Vec<u8> {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        push_basic_string_char(&mut out, c);
    }
    out.push('"');
    out.into_bytes()
}

fn render_literal_string(s: &str) -> Vec<u8> {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    out.push_str(s);
    out.push('\'');
    out.into_bytes()
}

/// Push a single character into a TOML basic string, escaping per
/// TOML 1.0 §6 (\b, \t, \n, \f, \r, \", \\, and \uXXXX for other controls).
fn push_basic_string_char(out: &mut String, ch: char) {
    match ch {
        '\\' => out.push_str(r"\\"),
        '"' => out.push_str("\\\""),
        '\n' => out.push_str(r"\n"),
        '\r' => out.push_str(r"\r"),
        '\t' => out.push_str(r"\t"),
        '\x08' => out.push_str(r"\b"),
        '\x0c' => out.push_str(r"\f"),
        c if (c as u32) < 0x20 || c as u32 == 0x7f => {
            use std::fmt::Write as _;
            let _ = write!(out, "\\u{:04X}", c as u32);
        }
        c => out.push(c),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pointer;

    fn parse_doc(s: &str) -> Document {
        Toml.parse(s.as_bytes()).expect("parse")
    }

    // -- parse / span builder ------------------------------------------

    #[test]
    fn parse_simple_top_level_keys() {
        let doc = parse_doc("a = 1\nb = \"x\"\n");
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.get("a"), Some(&Value::Int(1)));
        assert_eq!(m.get("b"), Some(&Value::String("x".into())));
        assert!(doc.spans().contains_key("/a"));
        assert!(doc.spans().contains_key("/b"));
    }

    #[test]
    fn parse_records_value_byte_range_for_int() {
        // Source: `a = 1\n` — the integer literal `1` is at byte 4.
        let bytes = b"a = 1\n";
        let doc = Toml.parse(bytes).expect("parse");
        let span = doc.spans().get("/a").expect("/a span");
        let slice = &bytes[span.value_range.clone()];
        assert_eq!(
            slice, b"1",
            "value_range must cover exactly the literal `1`"
        );
        assert_eq!(span.context, SpanContext::BlockMapValue);
    }

    #[test]
    fn parse_records_value_byte_range_for_string_including_quotes() {
        let bytes = b"name = \"dq\"\n";
        let doc = Toml.parse(bytes).expect("parse");
        let span = doc.spans().get("/name").expect("/name span");
        let slice = &bytes[span.value_range.clone()];
        assert_eq!(
            slice, b"\"dq\"",
            "string value_range must include surrounding quotes",
        );
    }

    #[test]
    fn parse_nested_tables_produce_nested_pointers() {
        let doc = parse_doc("[server]\nhost = \"localhost\"\n\n[server.tls]\ncert = \"x\"\n");
        let Value::Map(m) = doc.value() else { panic!() };
        let server = m.get("server").expect("server");
        let Value::Map(server_map) = server else {
            panic!("server is not a table")
        };
        assert!(server_map.contains_key("tls"));
        // Pointer for the nested scalar uses the `/server/host` shape.
        assert!(doc.spans().contains_key("/server/host"));
        assert!(doc.spans().contains_key("/server/tls/cert"));
    }

    #[test]
    fn parse_arrays_of_tables_use_index_pointers() {
        let doc = parse_doc("[[products]]\nname = \"a\"\n\n[[products]]\nname = \"b\"\n");
        let Value::Map(m) = doc.value() else { panic!() };
        let Value::Array(items) = m.get("products").unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        assert!(doc.spans().contains_key("/products/0/name"));
        assert!(doc.spans().contains_key("/products/1/name"));
    }

    #[test]
    fn parse_inline_array_elements_have_flow_context() {
        let doc = parse_doc("nums = [1, 2, 3]\n");
        assert!(doc.spans().contains_key("/nums/0"));
        let span = doc.spans().get("/nums/0").unwrap();
        assert_eq!(span.context, SpanContext::FlowSeqItem);
    }

    #[test]
    fn parse_inline_table_values_have_flow_context() {
        let doc = parse_doc("point = { x = 1, y = 2 }\n");
        assert!(doc.spans().contains_key("/point/x"));
        let span = doc.spans().get("/point/x").unwrap();
        assert_eq!(span.context, SpanContext::FlowMapValue);
    }

    #[test]
    fn parse_datetime_literals_become_strings_with_spans() {
        let bytes = b"when = 2024-01-02T03:04:05Z\n";
        let doc = Toml.parse(bytes).expect("parse");
        // Datetime is exposed as a string in the dq model (see module docs).
        let Value::Map(m) = doc.value() else { panic!() };
        match m.get("when") {
            Some(Value::String(s)) => {
                assert!(s.contains("2024-01-02"), "got: {s:?}");
            }
            other => panic!("expected string, got: {other:?}"),
        }
        // The span covers the literal text of the datetime.
        assert!(doc.spans().contains_key("/when"));
    }

    #[test]
    fn parse_invalid_toml_returns_parse_error_with_position() {
        let err = Toml
            .parse(b"a = \n")
            .expect_err("incomplete value must error");
        match err {
            Error::Parse { line, .. } => {
                assert!(line >= 1, "Parse error must carry a line; got: {line}");
            }
            other => panic!("expected Parse, got: {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_utf8_returns_parse_error() {
        let bytes: &[u8] = &[0xFF, 0xFE, b'a', b' ', b'=', b' ', b'1'];
        let err = Toml.parse(bytes).expect_err("invalid UTF-8 must error");
        assert!(matches!(err, Error::Parse { .. }), "got: {err:?}");
    }

    // -- write ----------------------------------------------------------

    #[test]
    fn write_round_trip_preserves_source_bytes() {
        // `original_bytes` non-empty → write is a verbatim copy of the
        // source. This is the cargo-style preservation contract.
        let bytes = b"# header\na = 1\nb = \"x\"\n";
        let doc = Toml.parse(bytes).expect("parse");
        let mut buf: Vec<u8> = Vec::new();
        Toml.write(&doc, &mut buf).expect("write");
        assert_eq!(buf, bytes);
    }

    #[test]
    fn write_value_only_uses_serde_fallback() {
        // `Document::single` produces a `value_only`-shaped doc with empty
        // `original_bytes`. The writer falls back to `toml::to_string_pretty`
        // and produces parseable output; round-trip through `parse` recovers
        // the same in-memory tree.
        let mut map = IndexMap::new();
        map.insert("a".into(), Value::Int(1));
        let doc = Document::single(Value::Map(map));
        let mut buf: Vec<u8> = Vec::new();
        Toml.write(&doc, &mut buf).expect("write");
        let again = Toml.parse(&buf).expect("re-parse");
        match again.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(1))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn write_rejects_multi_doc() {
        let multi = Document::multi(vec![Value::Int(1), Value::Int(2)]);
        let mut buf: Vec<u8> = Vec::new();
        let err = Toml.write(&multi, &mut buf).unwrap_err();
        assert_eq!(err.kind_name(), "format");
    }

    // -- set_at / del_at -----------------------------------------------

    #[test]
    fn document_set_at_replaces_int_byte_perfect() {
        let bytes = b"a = 1\nb = 2\n";
        let mut doc = Toml.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/a").expect("pointer");
        doc.set_at(&pointer, Value::Int(99))
            .expect("set_at must succeed once TOML renderer is registered");
        assert_eq!(
            doc.original_bytes(),
            b"a = 99\nb = 2\n",
            "set_at must splice exactly the value bytes; the rest is preserved",
        );
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(99))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn document_set_at_preserves_comments() {
        // Comments and other lines must remain byte-identical after set_at.
        let bytes = b"# header\nport = 8080 # default\nname = \"dq\"\n";
        let mut doc = Toml.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/port").expect("pointer");
        doc.set_at(&pointer, Value::Int(9090)).expect("set_at");
        assert_eq!(
            doc.original_bytes(),
            b"# header\nport = 9090 # default\nname = \"dq\"\n",
        );
    }

    #[test]
    fn document_set_at_preserves_quote_style_for_strings() {
        let bytes = b"title = \"Hello\"\n";
        let mut doc = Toml.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/title").expect("pointer");
        doc.set_at(&pointer, Value::String("Updated".into()))
            .expect("set_at");
        assert_eq!(doc.original_bytes(), b"title = \"Updated\"\n");
    }

    #[test]
    fn document_set_at_preserves_literal_quote_style() {
        let bytes = b"path = 'C:\\\\Users'\n";
        let mut doc = Toml.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/path").expect("pointer");
        doc.set_at(&pointer, Value::String("D:\\Other".into()))
            .expect("set_at");
        // Literal strings keep single quotes when the new value has no `'`.
        assert_eq!(doc.original_bytes(), b"path = 'D:\\Other'\n");
    }

    #[test]
    fn document_del_at_removes_full_line() {
        let bytes = b"a = 1\nb = 2\nc = 3\n";
        let mut doc = Toml.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/b").expect("pointer");
        doc.del_at(&pointer).expect("del_at");
        assert_eq!(doc.original_bytes(), b"a = 1\nc = 3\n");
    }

    // -- Renderer -------------------------------------------------------

    #[test]
    fn scalar_renderer_renders_int() {
        let r = TomlScalarRenderer;
        let out = r.render_replacement(&Value::Int(42), SpanContext::BlockMapValue, b"1");
        assert_eq!(out, b"42");
    }

    #[test]
    fn scalar_renderer_renders_bool() {
        let r = TomlScalarRenderer;
        let out = r.render_replacement(&Value::Bool(true), SpanContext::BlockMapValue, b"false");
        assert_eq!(out, b"true");
    }

    #[test]
    fn scalar_renderer_renders_basic_string_keeping_double_quotes() {
        let r = TomlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("hello".into()),
            SpanContext::BlockMapValue,
            b"\"old\"",
        );
        assert_eq!(out, b"\"hello\"");
    }

    #[test]
    fn scalar_renderer_escapes_special_chars_in_basic_string() {
        let r = TomlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("a\"b\nc".into()),
            SpanContext::BlockMapValue,
            b"\"x\"",
        );
        assert_eq!(out, b"\"a\\\"b\\nc\"");
    }

    #[test]
    fn scalar_renderer_renders_float_with_fractional() {
        let r = TomlScalarRenderer;
        let out = r.render_replacement(&Value::Float(1.0), SpanContext::BlockMapValue, b"0.5");
        assert_eq!(out, b"1.0");
    }

    #[test]
    fn insertion_renderer_emits_simple_key_value_pair() {
        let r = TomlInsertionRenderer;
        let out = r.render_insertion(
            "name",
            &Value::String("dq".into()),
            0,
            SpanContext::BlockMapValue,
        );
        assert_eq!(out, b"name = \"dq\"\n");
    }

    #[test]
    fn insertion_renderer_quotes_keys_with_special_chars() {
        let r = TomlInsertionRenderer;
        let out = r.render_insertion("with space", &Value::Int(1), 0, SpanContext::BlockMapValue);
        assert_eq!(out, b"\"with space\" = 1\n");
    }

    // -- mkdir-p (Bug #1) -----------------------------------------------

    #[test]
    fn insertion_renderer_inserts_new_root_key() {
        // Bug #1: root-level mkdir-p must append a new `key = value` line
        // after the last existing sibling.
        let bytes = b"a = 1\nb = 2\n";
        let mut doc = Toml.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/c").expect("pointer");
        doc.set_at(&pointer, Value::Int(42))
            .expect("mkdir-p set_at must succeed");
        let reparsed = Toml.parse(doc.original_bytes()).expect("re-parse");
        let c = pointer.resolve(reparsed.value()).expect("reparsed has /c");
        assert_eq!(c, &Value::Int(42));
        let a = Pointer::parse("/a")
            .unwrap()
            .resolve(reparsed.value())
            .expect("reparsed still has /a");
        assert_eq!(a, &Value::Int(1));
    }
}
