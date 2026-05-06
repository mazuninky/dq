//! JSON (RFC 8259) parser and writer.
//!
//! Numbers preserve their original textual representation for any integer
//! that overflows `i64` and any float whose `f64` round-trip would be lossy.
//! Object key order is preserved through `serde_json`'s `preserve_order`
//! feature.
//!
//! # Write-pat (M2 §5)
//!
//! On top of the M1 read path the parser now builds a [`SpanMap`] keyed by
//! canonical RFC 6901 pointer — every scalar leaf records its exact byte
//! range so [`Document::set_at`] can splice replacement bytes into
//! `original_bytes` while leaving every other byte (including comments
//! captured as line content, whitespace, and key order) intact. The same
//! pattern as YAML §3 / TOML §4: parse twice, once for the value tree
//! (`serde_json` with `arbitrary_precision`) and once for the spans (a
//! purpose-built byte scanner that does not allocate `Value`s).
//!
//! JSONC (`//` line comments and `/* */` block comments) is rejected at
//! parse time with a clear [`Error::Parse`] message; the strict RFC 8259
//! grammar is the only one supported.
//!
//! ## Insertion-renderer indent
//!
//! [`JsonInsertionRenderer`] hard-codes a 2-space indent step. The parser
//! detects the source's indent style (2-space, 4-space, tab) for diagnostic
//! purposes but the renderer has no access to the parsed document — passing
//! it through the trait would require a wider API change. M3 will revisit;
//! the M2 baseline accepts that newly inserted keys may use a different
//! indent step than the surrounding source. Existing keys are byte-spliced
//! so their indent is byte-preserved.

use std::io::Write;
use std::ops::Range;
use std::str::FromStr;

use indexmap::IndexMap;
use serde_json::Value as JsonValue;
use serde_json::value::Number as JsonNumber;

use crate::Result;
use crate::WriteOptions;
use crate::document::spans::{SpanContext, SpanMap, ValueSpan};
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;
use crate::textual_edit::{InsertionRenderer, ScalarRenderer};
use crate::write_options::canonicalize_keys;

/// JSON format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Json;

impl Format for Json {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        // M2 §5: parse once for the value tree (M1 read path) and once for
        // the spans. Both fall through the same `Error::Parse` on malformed
        // input — the span scanner runs first because it has the strictest
        // contract (it rejects JSONC before serde_json would reject the
        // comment as unexpected token), giving a clearer diagnostic.
        let spans = build_span_map(bytes)?;
        let json: JsonValue = serde_json::from_slice(bytes).map_err(|e| {
            let line = e.line() as u32;
            let col = e.column() as u32;
            let (span, snippet) = compute_parse_anchor(bytes, e.line());
            Error::Parse {
                file: None,
                line,
                col,
                span,
                snippet,
                message: e.to_string(),
            }
        })?;
        Ok(Document::with_spans(
            json_value_to_value(json),
            bytes.to_vec(),
            spans,
            FormatTag::Json,
        ))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        // Write-aware path: `original_bytes` already reflects every prior
        // splice, so a verbatim copy is the round-trip contract. This is the
        // §3 / §4 baseline — every other byte (whitespace, key order,
        // number literals, indent style) is preserved.
        if !doc.original_bytes().is_empty() {
            return w
                .write_all(doc.original_bytes())
                .map_err(|source| Error::Io {
                    path: camino::Utf8PathBuf::from("<json-writer>"),
                    source,
                });
        }
        if let Some(values) = doc.values() {
            // JSON has no native multi-document concept; surface as an
            // array. Round-trip from multi-doc YAML is intentional.
            let arr = Value::Array(values.to_vec());
            write_value_pretty(&arr, w, 0)?;
        } else {
            write_value_pretty(doc.value(), w, 0)?;
        }
        Ok(())
    }

    fn write_with_options(
        &self,
        doc: &Document,
        w: &mut dyn Write,
        opts: &WriteOptions,
    ) -> Result<()> {
        // M4 §2 default-equivalence contract: when no options are set we
        // delegate to `write` so the byte-output is identical to the M2
        // baseline (including the `original_bytes` verbatim copy for write-
        // aware documents). Any opted-in change forces a re-emit through the
        // value tree — `original_bytes` is bypassed because options like
        // `--sort-keys` and `--indent` are inherently re-emit knobs.
        if !opts.sort_keys && opts.indent.is_none() {
            return self.write(doc, w);
        }

        // Compute the value tree to emit. Multi-doc JSON is collapsed into
        // an array (matching `write`'s behaviour); the canonicalize step
        // sorts keys at every depth when requested.
        let value: Value = if let Some(values) = doc.values() {
            Value::Array(values.to_vec())
        } else {
            doc.value().clone()
        };
        let value = if opts.sort_keys {
            canonicalize_keys(&value)
        } else {
            value
        };

        match opts.indent {
            // `Some(0)` → compact: no newlines, no indent. Matches the
            // shape `serde_json::to_writer` would emit with the default
            // `CompactFormatter`. We use the local helper so big-numeric
            // literals stay byte-perfect.
            Some(0) => write_value_compact(&value, w),
            // `Some(n)` (n >= 1) → pretty-print with `n` spaces per level.
            Some(n) => write_value_pretty_with_step(&value, w, 0, n as usize),
            // `None` → preserve the M2 default whitespace shape (2-space
            // indent, surrounding newlines). This is the shape `dq fmt`
            // produces when only `--sort-keys` was passed.
            None => write_value_pretty(&value, w, 0),
        }
    }
}

/// Parse `bytes` as JSON, returning the read-pat [`Value`] tree alongside the
/// write-pat [`SpanMap`].
///
/// Mirrors [`crate::parsers::yaml_spans::parse_with_spans`] for the JSON
/// path: callers that need both pay the cost of two parses (one through
/// `serde_json`, one through the byte scanner). The tradeoff is the same —
/// for the document sizes M2 targets the cost is well under budget and the
/// resulting code is much simpler than building a single parser that
/// produces both.
///
/// # Errors
///
/// - [`Error::Parse`] when the input is not valid UTF-8, contains JSONC
///   comments, or fails strict JSON parsing.
pub fn parse_with_spans(bytes: &[u8]) -> Result<(Value, SpanMap)> {
    let spans = build_span_map(bytes)?;
    let json: JsonValue = serde_json::from_slice(bytes).map_err(|e| {
        let line = e.line() as u32;
        let col = e.column() as u32;
        let (span, snippet) = compute_parse_anchor(bytes, e.line());
        Error::Parse {
            file: None,
            line,
            col,
            span,
            snippet,
            message: e.to_string(),
        }
    })?;
    Ok((json_value_to_value(json), spans))
}

/// Parse `bytes` as JSON and assemble a [`Document`] carrying both the value
/// tree and the spans needed for the write path.
///
/// # Errors
///
/// Forwards [`parse_with_spans`] errors verbatim.
pub fn parse_json_with_spans(bytes: &[u8]) -> Result<Document> {
    let (value, spans) = parse_with_spans(bytes)?;
    Ok(Document::with_spans(
        value,
        bytes.to_vec(),
        spans,
        FormatTag::Json,
    ))
}

// ---------------------------------------------------------------------------
// Span scanner
// ---------------------------------------------------------------------------

/// Build a [`SpanMap`] from JSON source bytes via a manual byte scanner.
///
/// The scanner walks `bytes` directly (no AST allocation), tracks a path
/// stack of pointer segments, and records one [`ValueSpan`] per scalar leaf
/// keyed by canonical RFC 6901 pointer. Container values (objects /
/// arrays) do not get their own span entry — only their leaf scalars do —
/// matching the §3 / §4 contract.
///
/// JSONC (`//` line / `/* */` block comments) is rejected at this layer
/// with a structured [`Error::Parse`] before the value tree parse runs.
fn build_span_map(bytes: &[u8]) -> Result<SpanMap> {
    if std::str::from_utf8(bytes).is_err() {
        return Err(Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: "source is not valid UTF-8".to_owned(),
        });
    }
    let mut scanner = Scanner::new(bytes);
    let mut spans = SpanMap::new();
    scanner.skip_ws_and_check_jsonc()?;
    if scanner.eof() {
        // Empty input — let the value tree parser surface its error.
        return Ok(spans);
    }
    scanner.scan_value(&mut Vec::new(), &mut spans, SpanContext::BlockMapValue)?;
    Ok(spans)
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<u8> {
        self.bytes.get(self.pos + off).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    /// Skip whitespace. Reject `//` and `/*` comments with a structured
    /// [`Error::Parse`] — JSONC is not supported.
    fn skip_ws_and_check_jsonc(&mut self) -> Result<()> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => {
                    self.pos += 1;
                }
                Some(b'/') => {
                    let next = self.peek_at(1);
                    if matches!(next, Some(b'/' | b'*')) {
                        return Err(self.parse_error(
                            self.pos,
                            "comments are not valid JSON; JSONC is not supported",
                        ));
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
    }

    /// Build an [`Error::Parse`] anchored at byte offset `at` with `message`.
    fn parse_error(&self, at: usize, message: &str) -> Error {
        let (line, col) = derive_line_col(self.bytes, at);
        let snippet = extract_line_snippet(self.bytes, at);
        let end = (at + 1).min(self.bytes.len());
        Error::Parse {
            file: None,
            line,
            col,
            span: at..end,
            snippet,
            message: message.to_owned(),
        }
    }

    /// Scan one JSON value at `self.pos`, recording a [`ValueSpan`] entry
    /// for scalars at the canonical pointer for `path`. Containers recurse;
    /// they themselves do not produce a span entry.
    fn scan_value(
        &mut self,
        path: &mut Vec<String>,
        spans: &mut SpanMap,
        context: SpanContext,
    ) -> Result<()> {
        self.skip_ws_and_check_jsonc()?;
        let start = self.pos;
        match self.peek() {
            Some(b'{') => {
                self.bump();
                self.scan_object(path, spans)?;
            }
            Some(b'[') => {
                self.bump();
                self.scan_array(path, spans)?;
            }
            Some(b'"') => {
                let end = self.scan_string()?;
                self.record_scalar(path, spans, start, end, context);
            }
            Some(b't') | Some(b'f') => {
                let end = self.scan_bool()?;
                self.record_scalar(path, spans, start, end, context);
            }
            Some(b'n') => {
                let end = self.scan_null()?;
                self.record_scalar(path, spans, start, end, context);
            }
            Some(b'-') | Some(b'0'..=b'9') => {
                let end = self.scan_number()?;
                self.record_scalar(path, spans, start, end, context);
            }
            Some(_) => {
                // Let serde_json produce the canonical diagnostic — we only
                // need a non-panicking exit here.
                return Err(
                    self.parse_error(self.pos, "expected JSON value (object, array, scalar)")
                );
            }
            None => {
                return Err(self.parse_error(self.pos, "unexpected EOF while parsing value"));
            }
        }
        Ok(())
    }

    fn scan_object(&mut self, path: &mut Vec<String>, spans: &mut SpanMap) -> Result<()> {
        // The `{` was already consumed.
        self.skip_ws_and_check_jsonc()?;
        if matches!(self.peek(), Some(b'}')) {
            self.bump();
            return Ok(());
        }
        loop {
            self.skip_ws_and_check_jsonc()?;
            // Key must be a string.
            if !matches!(self.peek(), Some(b'"')) {
                return Err(self.parse_error(self.pos, "expected string key in object"));
            }
            let key_start = self.pos;
            let key_end = self.scan_string()?;
            // Inner key text without surrounding quotes.
            let key_inner = &self.bytes[key_start + 1..key_end - 1];
            let key_str = decode_json_string(key_inner)
                .ok_or_else(|| self.parse_error(key_start, "invalid JSON string escape in key"))?;

            self.skip_ws_and_check_jsonc()?;
            if !matches!(self.peek(), Some(b':')) {
                return Err(self.parse_error(self.pos, "expected ':' after object key"));
            }
            self.bump();

            path.push(pointer_escape(&key_str));
            self.scan_value(path, spans, SpanContext::BlockMapValue)?;
            path.pop();

            self.skip_ws_and_check_jsonc()?;
            match self.peek() {
                Some(b',') => {
                    self.bump();
                }
                Some(b'}') => {
                    self.bump();
                    return Ok(());
                }
                _ => {
                    return Err(self.parse_error(self.pos, "expected ',' or '}' in object"));
                }
            }
        }
    }

    fn scan_array(&mut self, path: &mut Vec<String>, spans: &mut SpanMap) -> Result<()> {
        // The `[` was already consumed.
        self.skip_ws_and_check_jsonc()?;
        if matches!(self.peek(), Some(b']')) {
            self.bump();
            return Ok(());
        }
        let mut idx: usize = 0;
        loop {
            path.push(idx.to_string());
            self.scan_value(path, spans, SpanContext::BlockSeqItem)?;
            path.pop();
            idx += 1;
            self.skip_ws_and_check_jsonc()?;
            match self.peek() {
                Some(b',') => {
                    self.bump();
                }
                Some(b']') => {
                    self.bump();
                    return Ok(());
                }
                _ => {
                    return Err(self.parse_error(self.pos, "expected ',' or ']' in array"));
                }
            }
        }
    }

    /// Scan a JSON string literal starting at the opening `"`, returning the
    /// byte position one past the closing `"`. Validates the structure but
    /// does not decode escapes — callers that need the inner text use
    /// [`decode_json_string`].
    fn scan_string(&mut self) -> Result<usize> {
        // Opening `"`.
        if !matches!(self.peek(), Some(b'"')) {
            return Err(self.parse_error(self.pos, "expected '\"'"));
        }
        self.bump();
        loop {
            match self.bump() {
                Some(b'"') => return Ok(self.pos),
                Some(b'\\') => {
                    // Skip the escaped byte. \uXXXX has 4 more hex digits;
                    // we don't validate them — serde_json will reject if
                    // malformed.
                    if self.peek() == Some(b'u') {
                        self.bump();
                        for _ in 0..4 {
                            if self.bump().is_none() {
                                return Err(
                                    self.parse_error(self.pos, "unexpected EOF inside \\u escape")
                                );
                            }
                        }
                    } else if self.bump().is_none() {
                        return Err(
                            self.parse_error(self.pos, "unexpected EOF after backslash escape")
                        );
                    }
                }
                Some(_) => continue,
                None => {
                    return Err(self.parse_error(self.pos, "unterminated string literal"));
                }
            }
        }
    }

    fn scan_bool(&mut self) -> Result<usize> {
        let rest = &self.bytes[self.pos..];
        if rest.starts_with(b"true") {
            self.pos += 4;
            return Ok(self.pos);
        }
        if rest.starts_with(b"false") {
            self.pos += 5;
            return Ok(self.pos);
        }
        Err(self.parse_error(self.pos, "expected 'true' or 'false'"))
    }

    fn scan_null(&mut self) -> Result<usize> {
        let rest = &self.bytes[self.pos..];
        if rest.starts_with(b"null") {
            self.pos += 4;
            return Ok(self.pos);
        }
        Err(self.parse_error(self.pos, "expected 'null'"))
    }

    /// Scan a number literal — minus / digits / fraction / exponent. Returns
    /// the byte position one past the last digit. Validation is loose; the
    /// `serde_json` second pass produces the canonical error on malformed
    /// numbers.
    fn scan_number(&mut self) -> Result<usize> {
        if matches!(self.peek(), Some(b'-')) {
            self.bump();
        }
        // Integer part.
        match self.peek() {
            Some(b'0') => {
                self.bump();
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.bump();
                }
            }
            _ => return Err(self.parse_error(self.pos, "expected digit in number")),
        }
        // Fraction.
        if matches!(self.peek(), Some(b'.')) {
            self.bump();
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        // Exponent.
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        Ok(self.pos)
    }

    /// Record a [`ValueSpan`] for the scalar at `[start..end)`.
    fn record_scalar(
        &self,
        path: &[String],
        spans: &mut SpanMap,
        start: usize,
        end: usize,
        context: SpanContext,
    ) {
        let pointer = pointer_for(path);
        let value_range = start..end;
        let line_range = compute_line_range(self.bytes, &value_range);
        let indent = compute_indent(self.bytes, start);
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
}

/// Compute the physical line range covering `value_range`, including the
/// trailing newline so `del_at` removes the whole line.
fn compute_line_range(bytes: &[u8], value_range: &Range<usize>) -> Range<usize> {
    let mut start = value_range.start.min(bytes.len());
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    let mut end = value_range.end.min(bytes.len());
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    if end < bytes.len() {
        end += 1;
    }
    start..end
}

/// Compute the indent (in source bytes) of the line containing `index`.
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

/// Derive a 1-indexed `(line, col)` for byte offset `idx`.
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

/// Extract the line containing byte offset `idx`, used for error rendering.
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

/// Build the canonical RFC 6901 pointer for `path`. Empty path → root.
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

/// Decode a JSON string literal's inner bytes (without surrounding quotes)
/// into the string's actual character content. Returns `None` for malformed
/// escapes — caller produces a structured `Error::Parse` in that case.
fn decode_json_string(inner: &[u8]) -> Option<String> {
    // Use serde_json itself to decode: wrap with quotes and parse.
    let mut buf = Vec::with_capacity(inner.len() + 2);
    buf.push(b'"');
    buf.extend_from_slice(inner);
    buf.push(b'"');
    serde_json::from_slice::<String>(&buf).ok()
}

/// Compute the byte span and source snippet for a `serde_json` parse error.
///
/// `serde_json::Error::line()` is 1-based. We walk `bytes` to find the start
/// offset of that line and use its full text (without the trailing newline)
/// as the snippet. The span is one byte wide pointing at the line start —
/// `dq-core` does not yet render a column-anchored caret, but storing the
/// data in this shape lets M2 add the caret without re-traversing the
/// source.
///
/// Returns `(0..0, String::new())` when:
/// - the input is not valid UTF-8 (we still emit the structured error, just
///   without the snippet),
/// - or `line` is 0 / out of range, which `serde_json` emits for some
///   trailing-input errors.
fn compute_parse_anchor(bytes: &[u8], line_1based: usize) -> (std::ops::Range<usize>, String) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (0..0, String::new());
    };
    if line_1based == 0 {
        return (0..0, String::new());
    }
    // Walk the text, counting newlines, until we reach the line we want.
    let target_line_idx = line_1based - 1; // convert to 0-based
    let mut start_of_line = 0usize;
    let mut newlines_seen = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if newlines_seen == target_line_idx {
            start_of_line = i;
            break;
        }
        if b == b'\n' {
            newlines_seen += 1;
            // Tentative start: byte after the newline. If the next loop
            // iteration short-circuits on `newlines_seen == target_line_idx`,
            // this becomes the final value.
            start_of_line = i + 1;
        }
    }
    if newlines_seen < target_line_idx {
        // Requested line is past EOF; fall back gracefully.
        return (0..0, String::new());
    }
    let total_len = text.len();
    if start_of_line >= total_len {
        return (start_of_line..start_of_line, String::new());
    }
    // Snippet = the line text without the trailing newline.
    let line_end = text[start_of_line..]
        .find('\n')
        .map_or(total_len, |off| start_of_line + off);
    let snippet = text[start_of_line..line_end].to_owned();
    let span_end = (start_of_line + 1).min(total_len);
    (start_of_line..span_end, snippet)
}

fn json_value_to_value(v: JsonValue) -> Value {
    match v {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(b),
        JsonValue::Number(n) => json_number_to_value(&n),
        JsonValue::String(s) => Value::String(s),
        JsonValue::Array(items) => {
            Value::Array(items.into_iter().map(json_value_to_value).collect())
        }
        JsonValue::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, json_value_to_value(v));
            }
            Value::Map(out)
        }
    }
}

fn json_number_to_value(n: &JsonNumber) -> Value {
    // With `arbitrary_precision` enabled, the underlying representation is the
    // original textual literal; `as_str()` gives it back to us verbatim.
    let literal = n.as_str();
    if let Ok(i) = literal.parse::<i64>() {
        return Value::Int(i);
    }
    if literal.contains('.') || literal.contains('e') || literal.contains('E') {
        // Float branch: only use `f64` when round-trip is lossless.
        if let Ok(f) = f64::from_str(literal) {
            // The shortest-round-trip formatting (Rust's `f64::Display`) is
            // compared against the original literal. If they match we keep
            // the parsed `f64`; otherwise the literal is preserved verbatim.
            if f.is_finite() && f64_matches_literal(f, literal) {
                return Value::Float(f);
            }
        }
        return Value::BigFloat(literal.to_owned());
    }
    // Integer that overflowed `i64` — keep the literal verbatim.
    Value::BigInt(literal.to_owned())
}

fn f64_matches_literal(f: f64, literal: &str) -> bool {
    // Re-parse the shortest representation and check exact equality. This is
    // enough to detect precision loss without relying on string equality
    // (which would fail innocuous reformatting like `1e2` vs `100`).
    let formatted = f.to_string();
    f64::from_str(&formatted)
        .ok()
        .zip(f64::from_str(literal).ok())
        .is_some_and(|(a, b)| a.to_bits() == b.to_bits())
}

/// Write a `Value` as pretty-printed JSON with the M2 default 2-space indent
/// step, preserving big-numeric literals byte-for-byte.
pub(crate) fn write_value_pretty(v: &Value, w: &mut dyn Write, indent: usize) -> Result<()> {
    write_value_with(v, w, indent, true, 2)
}

/// Write a `Value` as compact JSON (no extra whitespace, no trailing newline).
pub(crate) fn write_value_compact(v: &Value, w: &mut dyn Write) -> Result<()> {
    write_value_with(v, w, 0, false, 2)
}

/// Write a `Value` as pretty-printed JSON with an explicit indent step.
///
/// `indent_step` is the number of spaces per indent level — the M4
/// `--indent N` flag drives this. The `indent` argument is the *starting*
/// nesting level (always `0` for top-level callers); the M2 callers pass
/// `0` and use `indent_step = 2` via [`write_value_pretty`]. Used by the
/// `Format::write_with_options` override on `Json` to honour `opts.indent`.
pub(crate) fn write_value_pretty_with_step(
    v: &Value,
    w: &mut dyn Write,
    indent: usize,
    indent_step: usize,
) -> Result<()> {
    write_value_with(v, w, indent, true, indent_step)
}

fn write_value_with(
    v: &Value,
    w: &mut dyn Write,
    indent: usize,
    pretty: bool,
    indent_step: usize,
) -> Result<()> {
    match v {
        Value::Null => write_io(w, b"null")?,
        Value::Bool(b) => write_io(w, if *b { b"true" } else { b"false" })?,
        Value::Int(n) => {
            let s = n.to_string();
            write_io(w, s.as_bytes())?;
        }
        Value::BigInt(s) | Value::BigFloat(s) => write_io(w, s.as_bytes())?,
        Value::Float(n) => {
            // NaN and ±Infinity are not valid JSON (RFC 8259 § 6) — fail
            // loudly instead of emitting `NaN`/`inf` literals that downstream
            // consumers would reject.
            if !n.is_finite() {
                return Err(Error::Format {
                    format: "json",
                    message: "non-finite float (NaN or Infinity) cannot be serialized as JSON"
                        .to_owned(),
                });
            }
            // Use the shortest-round-trip representation. If the value is
            // integral, append `.0` to keep it parseable as float.
            let mut s = n.to_string();
            if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                s.push_str(".0");
            }
            write_io(w, s.as_bytes())?;
        }
        Value::String(s) => {
            write_io(w, b"\"")?;
            write_escaped_string(w, s)?;
            write_io(w, b"\"")?;
        }
        Value::Array(items) => write_array(items, w, indent, pretty, indent_step)?,
        Value::Map(map) => write_object(map, w, indent, pretty, indent_step)?,
    }
    Ok(())
}

fn write_array(
    items: &[Value],
    w: &mut dyn Write,
    indent: usize,
    pretty: bool,
    indent_step: usize,
) -> Result<()> {
    if items.is_empty() {
        return write_io(w, b"[]");
    }
    write_io(w, b"[")?;
    let next_indent = indent + 1;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            write_io(w, b",")?;
        }
        if pretty {
            write_io(w, b"\n")?;
            write_indent(w, next_indent, indent_step)?;
        }
        write_value_with(item, w, next_indent, pretty, indent_step)?;
    }
    if pretty {
        write_io(w, b"\n")?;
        write_indent(w, indent, indent_step)?;
    }
    write_io(w, b"]")
}

fn write_object(
    map: &IndexMap<String, Value>,
    w: &mut dyn Write,
    indent: usize,
    pretty: bool,
    indent_step: usize,
) -> Result<()> {
    if map.is_empty() {
        return write_io(w, b"{}");
    }
    write_io(w, b"{")?;
    let next_indent = indent + 1;
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            write_io(w, b",")?;
        }
        if pretty {
            write_io(w, b"\n")?;
            write_indent(w, next_indent, indent_step)?;
        }
        write_io(w, b"\"")?;
        write_escaped_string(w, k)?;
        write_io(w, if pretty { b"\": " } else { b"\":" })?;
        write_value_with(v, w, next_indent, pretty, indent_step)?;
    }
    if pretty {
        write_io(w, b"\n")?;
        write_indent(w, indent, indent_step)?;
    }
    write_io(w, b"}")
}

fn write_indent(w: &mut dyn Write, level: usize, step: usize) -> Result<()> {
    let total = level.saturating_mul(step);
    // Reuse a stack-allocated chunk of spaces to avoid an allocation for the
    // common indent widths. Up to 64 spaces per `write_io` call covers every
    // realistic depth/step combination dq targets.
    const SPACES: &[u8; 64] = &[b' '; 64];
    let mut remaining = total;
    while remaining > 0 {
        let chunk = remaining.min(SPACES.len());
        write_io(w, &SPACES[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn write_escaped_string(w: &mut dyn Write, s: &str) -> Result<()> {
    for c in s.chars() {
        match c {
            '"' => write_io(w, br#"\""#)?,
            '\\' => write_io(w, br"\\")?,
            '\n' => write_io(w, br"\n")?,
            '\r' => write_io(w, br"\r")?,
            '\t' => write_io(w, br"\t")?,
            '\u{08}' => write_io(w, br"\b")?,
            '\u{0c}' => write_io(w, br"\f")?,
            c if (c as u32) < 0x20 => {
                let s = format!("\\u{:04x}", c as u32);
                write_io(w, s.as_bytes())?;
            }
            c => {
                let mut buf = [0u8; 4];
                write_io(w, c.encode_utf8(&mut buf).as_bytes())?;
            }
        }
    }
    Ok(())
}

fn write_io(w: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    w.write_all(bytes).map_err(|source| Error::Io {
        path: camino::Utf8PathBuf::from("<json-writer>"),
        source,
    })
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

/// Renderer for in-span scalar replacements in JSON documents.
///
/// JSON has only one string quote style (`"..."`) and no comment syntax, so
/// the renderer's job is simpler than YAML's or TOML's: emit a strict
/// RFC 8259-conformant literal for the new value. Big-numeric values
/// (`BigInt` / `BigFloat`) are emitted as their literal text — `dq` users
/// preserve precision by passing the literal through `Value::BigInt(s)`.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonScalarRenderer;

impl ScalarRenderer for JsonScalarRenderer {
    fn render_replacement(
        &self,
        value: &Value,
        _context: SpanContext,
        _original: &[u8],
    ) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        // The inner write helpers can only fail on I/O; writing into a
        // `Vec<u8>` is infallible. `expect` keeps the trait signature simple
        // (no `Result` propagation) without hiding a real bug.
        write_value_compact(value, &mut buf).expect("writing to Vec<u8> is infallible");
        buf
    }
}

/// Renderer for brand-new key/value pairs inserted into a parent JSON
/// container.
///
/// The §5 baseline emits a leading newline + 2-space indent + `"key": value`
/// fragment with no trailing comma — the splice caller is responsible for
/// fixing up commas (per design D14 the caller already inserts a `,` after
/// the previous sibling when the parent had one). The 2-space indent is
/// hardcoded; see the module docs for the rationale.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsonInsertionRenderer;

impl InsertionRenderer for JsonInsertionRenderer {
    fn render_insertion(
        &self,
        key: &str,
        value: &Value,
        parent_indent: u32,
        _parent_context: SpanContext,
    ) -> Vec<u8> {
        // Hardcoded 2-space indent step. See module docs.
        let outer_spaces = parent_indent.saturating_add(2) as usize;
        let mut out = Vec::new();
        out.push(b'\n');
        out.extend(std::iter::repeat_n(b' ', outer_spaces));
        // Emit the key as a JSON string.
        let mut key_buf: Vec<u8> = Vec::with_capacity(key.len() + 2);
        key_buf.push(b'"');
        // Escape into a temporary writer.
        write_escaped_string(&mut key_buf, key).expect("writing to Vec<u8> is infallible");
        key_buf.push(b'"');
        out.extend_from_slice(&key_buf);
        out.extend_from_slice(b": ");
        let mut val_buf: Vec<u8> = Vec::new();
        write_value_compact(value, &mut val_buf).expect("writing to Vec<u8> is infallible");
        out.extend_from_slice(&val_buf);
        out
    }
}

// ---------------------------------------------------------------------------
// Indent style detection (used by diagnostics; renderer hardcodes 2-space)
// ---------------------------------------------------------------------------

/// Indent style detected from a JSON source's first indented line.
///
/// Currently only used by tests / diagnostic surfaces — the
/// [`JsonInsertionRenderer`] hard-codes 2-space indent because the trait
/// has no access to per-document state. Exposed as `pub(crate)` so future
/// section that widens the trait can plumb this through without an API
/// shape change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndentStyle {
    /// Two-space indent (the default RFC 7159-era convention).
    TwoSpace,
    /// Four-space indent (PEP-8-style).
    FourSpace,
    /// Tab indent.
    Tab,
}

/// Detect the indent style from the first indented line of `bytes`.
///
/// Returns `IndentStyle::TwoSpace` as the fallback when no indentation is
/// observed (e.g. the source is single-line or empty). Walks every line
/// looking for the first one that starts with whitespace; classifies a
/// tab-indented line as [`IndentStyle::Tab`], a space-indented line of
/// four-or-more spaces (multiple of 4) as [`IndentStyle::FourSpace`], and
/// every other space-indented line as [`IndentStyle::TwoSpace`].
#[allow(dead_code)] // used only by tests; see module docs for the rationale.
pub(crate) fn detect_indent_style(bytes: &[u8]) -> IndentStyle {
    let mut i = 0_usize;
    while i < bytes.len() {
        // A "line start" is byte 0 or one immediately after a newline.
        if i == 0 || bytes[i - 1] == b'\n' {
            match bytes[i] {
                b'\t' => return IndentStyle::Tab,
                b' ' => {
                    let mut count: usize = 0;
                    while i + count < bytes.len() && bytes[i + count] == b' ' {
                        count += 1;
                    }
                    return if count >= 4 && count.is_multiple_of(4) {
                        IndentStyle::FourSpace
                    } else {
                        IndentStyle::TwoSpace
                    };
                }
                _ => {}
            }
        }
        i += 1;
    }
    IndentStyle::TwoSpace
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pointer;

    fn parse(s: &str) -> Document {
        Json.parse(s.as_bytes()).unwrap()
    }

    fn write(doc: &Document) -> String {
        let mut buf: Vec<u8> = Vec::new();
        Json.write(doc, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    // -- M1 read-pat (preserved) ---------------------------------------

    #[test]
    fn parse_basic_types() {
        let doc = parse(r#"{"a": 1, "b": "x", "c": true, "d": null}"#);
        let Value::Map(m) = doc.value() else {
            panic!("expected map");
        };
        assert_eq!(m.get("a"), Some(&Value::Int(1)));
        assert_eq!(m.get("b"), Some(&Value::String("x".into())));
        assert_eq!(m.get("c"), Some(&Value::Bool(true)));
        assert_eq!(m.get("d"), Some(&Value::Null));
    }

    #[test]
    fn parse_preserves_key_order() {
        let doc = parse(r#"{"z": 1, "a": 2, "m": 3}"#);
        let Value::Map(m) = doc.value() else { panic!() };
        let keys: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }

    #[test]
    fn parse_preserves_big_int_literal() {
        let big = "4722366482869645213696";
        let s = format!(r#"{{"id": {big}}}"#);
        let doc = parse(&s);
        let Value::Map(m) = doc.value() else { panic!() };
        assert_eq!(m.get("id"), Some(&Value::BigInt(big.to_owned())));
    }

    #[test]
    fn write_pretty_is_two_space_indent() {
        // After §5 the writer is byte-preserving when `original_bytes` is
        // populated. Build a value-only doc to exercise the pretty path.
        let mut map = IndexMap::new();
        let arr = Value::Array(vec![Value::Int(1), Value::Int(2)]);
        map.insert("a".into(), arr);
        let doc = Document::single(Value::Map(map));
        let out = write(&doc);
        assert_eq!(out, "{\n  \"a\": [\n    1,\n    2\n  ]\n}");
    }

    #[test]
    fn big_int_round_trip_byte_for_byte() {
        let big = "4722366482869645213696";
        let s = format!(r#"{{"id":{big}}}"#);
        let doc = parse(&s);
        let mut buf: Vec<u8> = Vec::new();
        // JSON parser never produces a multi-document; assert that
        // invariant explicitly so the test breaks loudly if the contract
        // changes.
        assert!(
            !doc.is_multi(),
            "JSON parser must never produce a multi-document"
        );
        write_value_compact(doc.value(), &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains(big), "expected literal preserved: got {out}");
    }

    #[test]
    fn writer_rejects_non_finite_floats() {
        // NaN, +Inf, -Inf are all invalid in JSON. The writer must surface
        // them as `Error::Format` rather than emit a literal that downstream
        // parsers would reject.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut buf: Vec<u8> = Vec::new();
            let result = write_value_compact(&Value::Float(bad), &mut buf);
            let err = result.expect_err("non-finite float must error");
            match err {
                Error::Format { format, .. } => assert_eq!(format, "json"),
                other => panic!("expected Format error, got {other:?}"),
            }
        }
    }

    #[test]
    fn writer_accepts_finite_floats_round_trip() {
        let mut buf: Vec<u8> = Vec::new();
        write_value_compact(&Value::Float(3.5), &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "3.5");

        let mut buf: Vec<u8> = Vec::new();
        write_value_compact(&Value::Float(2.0), &mut buf).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "2.0");
    }

    #[test]
    fn parse_error_carries_line_col_and_snippet() {
        // Trailing comma — invalid JSON. The error should carry the line, col,
        // and offending source line as the snippet.
        let input = "{\n  \"a\": 1,\n}";
        let err = Json.parse(input.as_bytes()).unwrap_err();
        let Error::Parse {
            line,
            col,
            span,
            snippet,
            ..
        } = err
        else {
            panic!("expected Parse error");
        };
        assert!(line >= 1, "line should be 1-based and non-zero");
        assert!(col >= 1, "col should be 1-based and non-zero");
        assert!(!snippet.is_empty(), "snippet should be populated");
        assert!(
            span.start <= input.len(),
            "span start must be in-bounds: got {span:?} for len={}",
            input.len()
        );
    }

    #[test]
    fn parse_error_on_non_utf8_input_falls_back_gracefully() {
        // Non-UTF-8 input — JSON requires UTF-8, so this errors. The snippet
        // should be empty and the span 0..0 (no caret data possible).
        let input: [u8; 4] = [0xFF, 0xFE, 0xFD, 0xFC];
        let err = Json.parse(&input).unwrap_err();
        let Error::Parse { span, snippet, .. } = err else {
            panic!("expected Parse error");
        };
        assert!(snippet.is_empty(), "snippet must be empty for non-UTF-8");
        assert_eq!(span, 0..0);
    }

    // -- M2 §5 span builder ---------------------------------------------

    #[test]
    fn parse_records_value_byte_range_for_int() {
        // Input: `{"x": 42}` — the integer `42` lives at bytes 6..8.
        let bytes = b"{\"x\": 42}";
        let doc = Json.parse(bytes).expect("parse");
        let span = doc.spans().get("/x").expect("/x span");
        assert_eq!(&bytes[span.value_range.clone()], b"42");
        assert_eq!(span.context, SpanContext::BlockMapValue);
    }

    #[test]
    fn parse_records_value_byte_range_for_string_including_quotes() {
        let bytes = b"{\"name\": \"dq\"}";
        let doc = Json.parse(bytes).expect("parse");
        let span = doc.spans().get("/name").expect("/name span");
        assert_eq!(&bytes[span.value_range.clone()], b"\"dq\"");
    }

    #[test]
    fn parse_records_array_items_with_block_seq_context() {
        let bytes = b"{\"tags\": [\"rust\", \"cli\"]}";
        let doc = Json.parse(bytes).expect("parse");
        let span0 = doc.spans().get("/tags/0").expect("/tags/0");
        assert_eq!(&bytes[span0.value_range.clone()], b"\"rust\"");
        assert_eq!(span0.context, SpanContext::BlockSeqItem);
    }

    #[test]
    fn parse_records_nested_object_pointers() {
        let bytes = b"{\"server\": {\"host\": \"localhost\", \"port\": 8080}}";
        let doc = Json.parse(bytes).expect("parse");
        let span_host = doc.spans().get("/server/host").expect("/server/host");
        assert_eq!(&bytes[span_host.value_range.clone()], b"\"localhost\"");
        let span_port = doc.spans().get("/server/port").expect("/server/port");
        assert_eq!(&bytes[span_port.value_range.clone()], b"8080");
    }

    #[test]
    fn parse_jsonc_double_slash_returns_parse_error() {
        // `//` line comment is not valid JSON.
        let bytes = b"{\n  // comment\n  \"a\": 1\n}";
        let err = Json.parse(bytes).expect_err("JSONC must error");
        match err {
            Error::Parse { message, .. } => assert!(
                message.contains("JSONC is not supported"),
                "diagnostic must mention JSONC; got: {message}",
            ),
            other => panic!("expected Parse error, got: {other:?}"),
        }
    }

    #[test]
    fn parse_jsonc_block_comment_returns_parse_error() {
        let bytes = b"{ /* block */ \"a\": 1}";
        let err = Json.parse(bytes).expect_err("JSONC block must error");
        match err {
            Error::Parse { message, .. } => assert!(
                message.contains("JSONC is not supported"),
                "diagnostic must mention JSONC; got: {message}",
            ),
            other => panic!("expected Parse error, got: {other:?}"),
        }
    }

    #[test]
    fn parse_with_spans_returns_write_aware_document() {
        let bytes = b"{\"a\": 1}";
        let doc = parse_json_with_spans(bytes).expect("parse_json_with_spans");
        assert_eq!(doc.original_bytes(), bytes);
        assert_eq!(doc.format(), FormatTag::Json);
        assert!(doc.spans().contains_key("/a"));
    }

    // -- M2 §5 ScalarRenderer -------------------------------------------

    #[test]
    fn scalar_renderer_renders_int() {
        let r = JsonScalarRenderer;
        let out = r.render_replacement(&Value::Int(42), SpanContext::BlockMapValue, b"1");
        assert_eq!(out, b"42");
    }

    #[test]
    fn scalar_renderer_renders_string_with_quotes() {
        let r = JsonScalarRenderer;
        let out = r.render_replacement(
            &Value::String("hello".into()),
            SpanContext::BlockMapValue,
            b"\"old\"",
        );
        assert_eq!(out, b"\"hello\"");
    }

    #[test]
    fn scalar_renderer_renders_bool() {
        let r = JsonScalarRenderer;
        let out = r.render_replacement(&Value::Bool(true), SpanContext::BlockMapValue, b"false");
        assert_eq!(out, b"true");
    }

    #[test]
    fn scalar_renderer_renders_null() {
        let r = JsonScalarRenderer;
        let out = r.render_replacement(&Value::Null, SpanContext::BlockMapValue, b"42");
        assert_eq!(out, b"null");
    }

    #[test]
    fn scalar_renderer_escapes_special_chars_in_string() {
        let r = JsonScalarRenderer;
        let out = r.render_replacement(
            &Value::String("a\"b\nc".into()),
            SpanContext::BlockMapValue,
            b"\"x\"",
        );
        assert_eq!(out, b"\"a\\\"b\\nc\"");
    }

    #[test]
    fn scalar_renderer_renders_float_with_fractional() {
        let r = JsonScalarRenderer;
        let out = r.render_replacement(&Value::Float(1.0), SpanContext::BlockMapValue, b"0.5");
        assert_eq!(out, b"1.0");
    }

    #[test]
    fn scalar_renderer_renders_big_int_literal() {
        let r = JsonScalarRenderer;
        let out = r.render_replacement(
            &Value::BigInt("4722366482869645213696".into()),
            SpanContext::BlockMapValue,
            b"0",
        );
        assert_eq!(out, b"4722366482869645213696");
    }

    // -- M2 §5 InsertionRenderer ----------------------------------------

    #[test]
    fn insertion_renderer_emits_indented_key_value_pair() {
        let r = JsonInsertionRenderer;
        let out = r.render_insertion(
            "name",
            &Value::String("dq".into()),
            0,
            SpanContext::BlockMapValue,
        );
        assert_eq!(out, b"\n  \"name\": \"dq\"");
    }

    #[test]
    fn insertion_renderer_escapes_special_chars_in_keys() {
        let r = JsonInsertionRenderer;
        let out = r.render_insertion("with\"quote", &Value::Int(1), 0, SpanContext::BlockMapValue);
        assert_eq!(out, b"\n  \"with\\\"quote\": 1");
    }

    // -- M2 §5 set_at end-to-end ----------------------------------------

    #[test]
    fn document_set_at_replaces_int_byte_perfect() {
        let bytes = b"{\"a\": 1, \"b\": 2}";
        let mut doc = Json.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/a").expect("pointer");
        doc.set_at(&pointer, Value::Int(99))
            .expect("set_at must succeed once JSON renderer is registered");
        assert_eq!(
            doc.original_bytes(),
            b"{\"a\": 99, \"b\": 2}",
            "set_at must splice exactly the value bytes",
        );
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(99))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn document_set_at_preserves_two_space_indent() {
        // 2-space indented JSON: every byte except the value must be
        // identical after set_at.
        let bytes = b"{\n  \"x\": 1\n}";
        let mut doc = Json.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/x").expect("pointer");
        doc.set_at(&pointer, Value::Int(42)).expect("set_at");
        assert_eq!(doc.original_bytes(), b"{\n  \"x\": 42\n}");
    }

    #[test]
    fn document_set_at_preserves_four_space_indent() {
        // 4-space indented JSON is preserved byte-for-byte by the splice.
        let bytes = b"{\n    \"x\": 1\n}";
        let mut doc = Json.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/x").expect("pointer");
        doc.set_at(&pointer, Value::Int(42)).expect("set_at");
        assert_eq!(doc.original_bytes(), b"{\n    \"x\": 42\n}");
    }

    #[test]
    fn document_set_at_preserves_tab_indent() {
        let bytes = b"{\n\t\"x\": 1\n}";
        let mut doc = Json.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/x").expect("pointer");
        doc.set_at(&pointer, Value::Int(42)).expect("set_at");
        assert_eq!(doc.original_bytes(), b"{\n\t\"x\": 42\n}");
    }

    #[test]
    fn document_set_at_preserves_quote_style_for_strings() {
        let bytes = b"{\"title\": \"Hello\"}";
        let mut doc = Json.parse(bytes).expect("parse");
        let pointer = Pointer::parse("/title").expect("pointer");
        doc.set_at(&pointer, Value::String("Updated".into()))
            .expect("set_at");
        assert_eq!(doc.original_bytes(), b"{\"title\": \"Updated\"}");
    }

    // -- M2 §5 indent style detection ----------------------------------

    #[test]
    fn detect_indent_style_identifies_two_space() {
        assert_eq!(
            detect_indent_style(b"{\n  \"a\": 1\n}"),
            IndentStyle::TwoSpace
        );
    }

    #[test]
    fn detect_indent_style_identifies_four_space() {
        assert_eq!(
            detect_indent_style(b"{\n    \"a\": 1\n}"),
            IndentStyle::FourSpace
        );
    }

    #[test]
    fn detect_indent_style_identifies_tab() {
        assert_eq!(detect_indent_style(b"{\n\t\"a\": 1\n}"), IndentStyle::Tab);
    }

    #[test]
    fn detect_indent_style_falls_back_to_two_space_when_no_indent() {
        assert_eq!(detect_indent_style(b"{\"a\": 1}"), IndentStyle::TwoSpace);
    }
}
