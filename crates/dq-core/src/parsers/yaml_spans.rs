//! YAML write-pat span builder + textual-edit renderers.
//!
//! Read-pat YAML stays on `serde_norway` ([`crate::parsers::yaml`]). This module
//! is the **parallel** parser used by the M2 write path: it walks the same
//! source bytes through the low-level `saphyr-parser` event API to record a
//! [`SpanMap`] (canonical RFC 6901 pointer → [`ValueSpan`]) keyed off the
//! exact byte ranges that `Document::set_at` and `Document::del_at` splice.
//!
//! The architecture follows the saphyr spike at
//! `spikes/saphyr/src/main.rs` — see also OpenSpec change
//! `add-safe-writes/design.md` D1 / D4 / D11 for the rationale on why we
//! splice raw bytes instead of round-tripping through an emitter.
//!
//! # Public surface
//!
//! - [`parse_with_spans`] — `(Value, SpanMap)` from a byte slice. The `Value`
//!   is built by reusing the existing `serde_norway`-based [`crate::parsers::Yaml`]
//!   read path; spans come from a fresh `saphyr-parser` walk. The double
//!   parse trades ~2× CPU for a much simpler implementation — see
//!   [`parse_with_spans`] docs for the cost analysis.
//! - [`parse_yaml_with_spans`] — the same, packaged into a [`Document`] with
//!   `original_bytes` retained.
//! - [`YamlScalarRenderer`] / [`YamlInsertionRenderer`] — the [`ScalarRenderer`]
//!   / [`InsertionRenderer`] impls registered by [`crate::textual_edit::renderer_for_format`]
//!   / `insertion_renderer_for_format`.
//!
//! # What is NOT here
//!
//! - Multi-line block scalar (`|` / `>`) round-trip rendering. The M2 baseline
//!   downgrades any literal/folded scalar to double-quoted on replacement; the
//!   original bytes for unrelated values are byte-preserved so the cost is
//!   localized to the edited line. Round-tripping `|` / `>` lands in M3.
//! - Alias rewrites. `Event::Alias` is logged at debug level and skipped — no
//!   span is recorded, so `set_at` on an alias surfaces the missing-pointer
//!   path. The spike report covers this finding.
//! - Tagged scalars. The tag is preserved by virtue of being outside the
//!   value_range; replacement renders the value only. M3 will revisit if a
//!   user reports a real-world case where this matters.

use std::collections::HashSet;
use std::ops::Range;

use saphyr_parser::{Event, Parser as YamlParser, ScalarStyle, Span};

use crate::Result;
use crate::document::spans::{SpanContext, SpanMap, ValueSpan};
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;
use crate::ir::{InlineBaseline, Provenance, ProvenanceMap};
use crate::pointer::Pointer;
use crate::textual_edit::{InsertionRenderer, ScalarRenderer};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse `bytes` as YAML, returning the read-pat [`Value`] tree alongside the
/// write-pat [`SpanMap`].
///
/// The `Value` and the `SpanMap` are byte-equivalent views of the same source.
/// Callers that need both — primarily [`parse_yaml_with_spans`] — pay the cost
/// of two parses (one through `serde_norway`, one through `saphyr-parser`). For
/// the document sizes M2 targets (≤ 1 MB human-edited config files) the spike
/// measured ~46 ms total at 1 MB, well under the 100 ms budget; the
/// simplicity vs. building a single parser that produces both is the
/// deliberate tradeoff.
///
/// # Errors
///
/// - [`Error::Parse`] when the input is not valid UTF-8 or `saphyr-parser`
///   reports a `ScanError`. The error carries a 1-indexed line/column and a
///   short snippet matching the failing line, so the CLI's diagnostic
///   formatting works without further introspection.
pub fn parse_with_spans(bytes: &[u8]) -> Result<(Value, SpanMap)> {
    let (value, spans, _block_scalars) = parse_with_spans_and_block_scalars(bytes)?;
    Ok((value, spans))
}

/// Parse `bytes` as YAML and additionally surface the canonical pointer set of
/// every leaf whose underlying scalar uses a block style ([`ScalarStyle::Literal`]
/// or [`ScalarStyle::Folded`]).
///
/// Block-style scalars (`|`, `>`, `|-`, `>-`) are the YAML constructs that
/// composite-rule evaluation re-parses as another format (Phase 4 of
/// `add-validation-and-extended-formats`). The caller — [`parse_yaml_with_spans`]
/// today — uses the returned set to attach `inline_offset = Some(InlineBaseline { 0, 1, 1 })`
/// to each block-scalar leaf's [`Provenance::Original`] entry. Other parsers
/// route through [`parse_with_spans`] when they only need the value tree and
/// span map.
///
/// # Errors
///
/// Same as [`parse_with_spans`].
fn parse_with_spans_and_block_scalars(bytes: &[u8]) -> Result<(Value, SpanMap, HashSet<String>)> {
    // Reuse the M1 read path verbatim for the value tree. Any divergence
    // between this parser's view and the spike-validated spans would cause
    // `Document::set_at` to splice into bytes that the in-memory tree does
    // not know about — easier to surface that here than chase it down inside
    // `set_at` callers.
    let read_doc = crate::parsers::Yaml.parse(bytes)?;
    let value = if let Some(values) = read_doc.values() {
        // Multi-doc streams flatten into a `Value::Array(documents)` so the
        // single `Value` returned here matches the multi-doc pointer
        // namespace `/{doc_idx}/...` that the span builder produces.
        Value::Array(values.to_vec())
    } else {
        read_doc.value().clone()
    };
    let (spans, block_scalars) = build_span_map_and_block_scalars(bytes)?;
    Ok((value, spans, block_scalars))
}

/// Parse `bytes` as YAML and assemble a [`Document`] carrying the value tree,
/// the spans needed for the write path, and an inline-offset-aware
/// [`ProvenanceMap`].
///
/// Always returns a single-document `Document` (`is_multi() == false`). For
/// multi-document streams the value is a `Value::Array(documents)` and the
/// span map keys use a `/{doc_idx}/...` prefix — that is the canonical
/// shape `Document::set_at` walks.
///
/// # Inline-offset population
///
/// Phase 2 of `add-validation-and-extended-formats` requires every YAML block
/// scalar (`|`, `>`, `|-`, `>-`) to carry
/// `inline_offset = Some(InlineBaseline { byte_start: 0, line: 1, col: 1 })`
/// on its `Provenance::Original` entry — the body of a block scalar always
/// starts at line 1 col 1 of its own content. Composite-rule evaluation
/// projects inner-document line / column back to outer-file coordinates by
/// reading this baseline. Plain, single-quoted, and double-quoted scalars
/// keep `inline_offset = None`.
///
/// # Errors
///
/// Forwards [`parse_with_spans`] errors verbatim.
pub fn parse_yaml_with_spans(bytes: &[u8]) -> Result<Document> {
    let (value, spans, block_scalars) = parse_with_spans_and_block_scalars(bytes)?;
    let provenance = build_provenance_with_inline_offsets(&spans, &block_scalars);
    Ok(Document::with_spans_and_provenance(
        value,
        bytes.to_vec(),
        spans,
        FormatTag::Yaml,
        provenance,
    ))
}

/// Build a [`ProvenanceMap`] from `spans`, populating `inline_offset` for every
/// canonical pointer in `block_scalar_pointers`.
///
/// Mirrors [`crate::document::provenance_from_spans`] behaviour for the
/// non-inline-offset entries; the only difference is the
/// `inline_offset = Some(InlineBaseline { 0, 1, 1 })` override for block
/// scalars. Kept inside the YAML parser module so the contract — "block
/// scalars start at offset 0, line 1, col 1 of their content" — lives next
/// to the parser branch that detects them.
fn build_provenance_with_inline_offsets(
    spans: &SpanMap,
    block_scalar_pointers: &HashSet<String>,
) -> ProvenanceMap {
    let mut map = ProvenanceMap::with_capacity(spans.len());
    for (canonical, span) in spans {
        let pointer = Pointer::parse(canonical)
            .expect("canonical span keys are produced by Pointer::as_canonical");
        let inline_offset = if block_scalar_pointers.contains(canonical) {
            // YAML block-scalar contract: body starts at byte 0, line 1, col 1
            // of the inner content (after the indicator + newline). The
            // caller of composite-rule evaluation projects this baseline
            // through `final_line = anchor_line + inner_line - 1` etc.
            Some(InlineBaseline {
                byte_start: 0,
                line: 1,
                col: 1,
            })
        } else {
            None
        };
        map.insert(
            canonical.clone(),
            Provenance::Original {
                pointer,
                span: Some(span.clone()),
                inline_offset,
            },
        );
    }
    map
}

// ---------------------------------------------------------------------------
// Span builder — saphyr-parser event walk
// ---------------------------------------------------------------------------

/// Build a [`SpanMap`] paired with the canonical-pointer set of every leaf
/// whose underlying scalar uses a YAML block style ([`ScalarStyle::Literal`]
/// or [`ScalarStyle::Folded`]).
///
/// Implementation mirrors the spike's state machine (see file-level docs).
/// Step 1 counts documents (so we know whether to namespace pointers under
/// `/{doc_idx}`); step 2 walks events recording one [`ValueSpan`] per scalar
/// value (not per scalar key) and, alongside, recording the canonical pointer
/// of every block-style leaf for the inline-offset enrichment in
/// [`parse_yaml_with_spans`].
///
/// # saphyr-parser char-vs-byte caveat
///
/// `saphyr_parser::Marker::index()` reports a **character** index (count of
/// Unicode scalars seen so far) despite the rustdoc claiming bytes — see the
/// `index` field comment in `saphyr-parser` 0.0.6 `src/scanner.rs`. The splice
/// path needs byte offsets, so we precompute a char-index → byte-offset map
/// up front and translate every marker through [`char_index_to_byte`] before
/// constructing a [`Range<usize>`]. Without this, any non-ASCII byte before a
/// span (em-dash in a comment, accented identifier, …) shifts every later
/// span's `value_range` by `(utf8_len - 1)` bytes per multi-byte codepoint.
fn build_span_map_and_block_scalars(bytes: &[u8]) -> Result<(SpanMap, HashSet<String>)> {
    let text = std::str::from_utf8(bytes).map_err(|err| Error::Parse {
        file: None,
        line: 0,
        col: 0,
        span: 0..0,
        snippet: String::new(),
        message: format!("source is not valid UTF-8: {err}"),
    })?;

    let n_docs = count_documents(text, bytes)?;
    let multi_doc = n_docs > 1;

    // One-time O(N) precompute. Lookup by char-index then becomes O(1).
    let char_to_byte = build_char_to_byte_map(text);

    let mut spans = SpanMap::new();
    let mut block_scalars: HashSet<String> = HashSet::new();
    let mut state = State::new(bytes, multi_doc, &char_to_byte);

    let mut parser = YamlParser::new_from_str(text);
    while let Some(item) = parser.next_event() {
        let (event, span) =
            item.map_err(|scan_err| scan_to_parse_error(&scan_err, bytes, &char_to_byte))?;
        state.observe(event, span, &mut spans, &mut block_scalars)?;
    }

    Ok((spans, block_scalars))
}

/// Build a `Vec<usize>` whose entry `i` is the byte offset of the `i`-th
/// character in `text`. The final entry is `text.len()` so a marker that
/// points at "one past the last char" maps to "one past the last byte".
///
/// Saphyr-parser may report a marker at the EOF position; without the
/// trailing sentinel that would index out of bounds.
fn build_char_to_byte_map(text: &str) -> Vec<usize> {
    let mut map = Vec::with_capacity(text.len() + 1);
    for (byte_pos, _ch) in text.char_indices() {
        map.push(byte_pos);
    }
    map.push(text.len());
    map
}

/// Translate a `saphyr_parser::Marker::index()` (character count) into the
/// corresponding byte offset in the original source. Markers beyond the end
/// saturate at the source length so a stale marker can never trigger an
/// out-of-bounds slice.
fn char_index_to_byte(char_to_byte: &[usize], char_idx: usize) -> usize {
    char_to_byte
        .get(char_idx)
        .copied()
        .unwrap_or_else(|| char_to_byte.last().copied().unwrap_or(0))
}

/// First pass: count `Event::DocumentStart` occurrences. Required to decide
/// whether the second pass should prefix pointers with `/{doc_idx}`.
fn count_documents(text: &str, bytes: &[u8]) -> Result<usize> {
    // The first pass walks events purely to count documents. If a scan
    // error fires here, the marker is converted via a fresh char-to-byte
    // map: the cost is paid only on the (rare) error path, so we don't
    // bother threading the precomputed map through.
    let mut parser = YamlParser::new_from_str(text);
    let mut n = 0_usize;
    while let Some(item) = parser.next_event() {
        let (event, _) = item.map_err(|scan_err| {
            let char_to_byte = build_char_to_byte_map(text);
            scan_to_parse_error(&scan_err, bytes, &char_to_byte)
        })?;
        if matches!(event, Event::DocumentStart(_)) {
            n += 1;
        }
    }
    Ok(n)
}

/// Convert a `saphyr_parser::ScanError` into our domain [`Error::Parse`],
/// pulling a one-line snippet out of the source bytes for the CLI to render.
///
/// The marker's character index is translated to a byte offset (see
/// [`char_index_to_byte`]) so the snippet seek lands on the correct line
/// even when the source contains multi-byte UTF-8 before the failure point.
fn scan_to_parse_error(
    scan_err: &saphyr_parser::ScanError,
    bytes: &[u8],
    char_to_byte: &[usize],
) -> Error {
    let marker = scan_err.marker();
    let line = u32::try_from(marker.line()).unwrap_or(u32::MAX);
    let col = u32::try_from(marker.col().saturating_add(1)).unwrap_or(u32::MAX);
    let idx = char_index_to_byte(char_to_byte, marker.index());
    let snippet = extract_line_snippet(bytes, idx);
    Error::Parse {
        file: None,
        line,
        col,
        span: idx..idx,
        snippet,
        message: scan_err.info().to_owned(),
    }
}

/// Extract the line containing `idx` (or the empty string at EOF). Used only
/// for error rendering; never on the hot path.
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

#[derive(Debug)]
enum Frame {
    /// `pending_key` is `Some` once we've seen the key scalar and are waiting
    /// for the value event.
    Mapping {
        pending_key: Option<String>,
        style: ContextStyle,
        /// Byte offset of the `MappingStart` event — used to record an
        /// empty-container span when `MappingEnd` fires with no children.
        start_byte: usize,
        /// `true` once any key/value pair has been recorded inside this
        /// frame, so `MappingEnd` can tell empty from non-empty without
        /// re-walking the SpanMap.
        saw_child: bool,
    },
    /// `index` is the position of the *next* item.
    Sequence {
        index: usize,
        style: ContextStyle,
        /// Byte offset of the `SequenceStart` event — mirrors
        /// [`Frame::Mapping::start_byte`].
        start_byte: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextStyle {
    Block,
    Flow,
}

struct State<'src> {
    bytes: &'src [u8],
    /// Map from `Marker::index()` (character offset) to byte offset. See
    /// [`build_char_to_byte_map`] for the rationale.
    char_to_byte: &'src [usize],
    multi_doc: bool,
    doc_index: usize,
    seen_first_doc: bool,
    stack: Vec<Frame>,
    path: Vec<String>,
}

impl<'src> State<'src> {
    fn new(bytes: &'src [u8], multi_doc: bool, char_to_byte: &'src [usize]) -> Self {
        Self {
            bytes,
            char_to_byte,
            multi_doc,
            doc_index: 0,
            seen_first_doc: false,
            stack: Vec::new(),
            path: Vec::new(),
        }
    }

    fn observe(
        &mut self,
        event: Event<'_>,
        span: Span,
        spans: &mut SpanMap,
        block_scalars: &mut HashSet<String>,
    ) -> std::result::Result<(), Error> {
        match event {
            Event::StreamStart | Event::StreamEnd | Event::Nothing => {}
            Event::DocumentStart(_) => {
                if self.seen_first_doc {
                    self.doc_index += 1;
                }
                self.seen_first_doc = true;
                if self.multi_doc {
                    self.path.push(self.doc_index.to_string());
                }
            }
            Event::DocumentEnd => {
                if self.multi_doc {
                    self.path.pop();
                }
            }
            Event::Scalar(value, style, _anchor, _tag) => {
                let is_key = matches!(
                    self.stack.last(),
                    Some(Frame::Mapping {
                        pending_key: None,
                        ..
                    })
                );
                if is_key {
                    let key_str = pointer_escape(value.as_ref());
                    if let Some(Frame::Mapping {
                        pending_key,
                        saw_child,
                        ..
                    }) = self.stack.last_mut()
                    {
                        *pending_key = Some(key_str);
                        // A key event proves the mapping is non-empty, so
                        // `MappingEnd` should NOT record an empty-container
                        // span. Setting `saw_child` here keeps the
                        // bookkeeping consistent even if the matching value
                        // event turns out to be another container or scalar.
                        *saw_child = true;
                    }
                } else {
                    let pointer = self.value_pointer();
                    let value_range = span_to_range(span, self.char_to_byte);
                    let context = self.value_context();
                    let line_range = self.compute_line_range(&value_range, context, style);
                    let indent = u32::try_from(span.start.col()).unwrap_or(u32::MAX);
                    if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded) {
                        // Record the canonical pointer alongside the span
                        // so [`build_provenance_with_inline_offsets`] can
                        // attach `inline_offset = Some(InlineBaseline { 0, 1, 1 })`
                        // when it builds the `ProvenanceMap`. This covers
                        // every block-style indicator: `|`, `>`, `|-`, `>-`
                        // — saphyr-parser collapses the chomping/indent
                        // suffixes into the same `Literal` / `Folded`
                        // discriminant, which is the contract Phase 2 of
                        // `add-validation-and-extended-formats` requires.
                        block_scalars.insert(pointer.clone());
                    }
                    spans.insert(
                        pointer,
                        ValueSpan {
                            value_range,
                            line_range,
                            indent,
                            context,
                        },
                    );
                    self.complete_value();
                }
            }
            Event::Alias(_) => {
                tracing::debug!(
                    line = span.start.line(),
                    col = span.start.col(),
                    "skipping alias span (aliases are not editable in M2)",
                );
                self.complete_value();
            }
            Event::SequenceStart(_anchor, _tag) => {
                let start_byte = char_index_to_byte(self.char_to_byte, span.start.index());
                let style = detect_container_style(self.bytes, start_byte);
                self.enter_container();
                self.stack.push(Frame::Sequence {
                    index: 0,
                    style,
                    start_byte,
                });
            }
            Event::SequenceEnd => {
                let end_byte = char_index_to_byte(self.char_to_byte, span.end.index());
                self.leave_container_recording_empty(spans, end_byte);
            }
            Event::MappingStart(_anchor, _tag) => {
                let start_byte = char_index_to_byte(self.char_to_byte, span.start.index());
                let style = detect_container_style(self.bytes, start_byte);
                self.enter_container();
                self.stack.push(Frame::Mapping {
                    pending_key: None,
                    style,
                    start_byte,
                    saw_child: false,
                });
            }
            Event::MappingEnd => {
                let end_byte = char_index_to_byte(self.char_to_byte, span.end.index());
                self.leave_container_recording_empty(spans, end_byte);
            }
        }
        Ok(())
    }

    fn value_pointer(&mut self) -> String {
        match self.stack.last_mut() {
            Some(Frame::Mapping { pending_key, .. }) => {
                if let Some(key) = pending_key.take() {
                    self.path.push(key);
                }
            }
            Some(Frame::Sequence { index, .. }) => {
                let idx = *index;
                self.path.push(idx.to_string());
                *index = idx + 1;
            }
            None => {
                // Top-level scalar — pointer is empty (single-doc) or
                // `/<doc_idx>` (multi-doc, already pushed by DocumentStart).
            }
        }
        if self.path.is_empty() {
            String::new()
        } else {
            format!("/{}", self.path.join("/"))
        }
    }

    fn complete_value(&mut self) {
        if !self.path.is_empty()
            && matches!(
                self.stack.last(),
                Some(Frame::Mapping { .. } | Frame::Sequence { .. })
            )
            && self.path.len() > self.doc_prefix_len()
        {
            self.path.pop();
        }
    }

    fn enter_container(&mut self) {
        match self.stack.last_mut() {
            Some(Frame::Mapping { pending_key, .. }) => {
                if let Some(key) = pending_key.take() {
                    self.path.push(key);
                }
            }
            Some(Frame::Sequence { index, .. }) => {
                let idx = *index;
                self.path.push(idx.to_string());
                *index = idx + 1;
            }
            None => {
                // Top-level container — no path component to push.
            }
        }
    }

    fn leave_container(&mut self) {
        let _ = self.stack.pop();
        let target_len = self.doc_prefix_len();
        if self.path.len() > target_len {
            self.path.pop();
        }
    }

    /// Pop the top container frame; if it was empty, record an
    /// empty-container [`ValueSpan`] keyed at the container's own pointer.
    ///
    /// Mirrors the JSON scanner's `record_empty_container` — the empty
    /// `{}` / `[]` body is the splice anchor used by
    /// [`crate::document::Document::set_at`]'s empty-parent mkdir-p path.
    /// The pointer is read **before** popping `self.path` so we key the
    /// span at the empty container itself, not at its parent.
    fn leave_container_recording_empty(&mut self, spans: &mut SpanMap, end_byte: usize) {
        let top = self.stack.last();
        let (is_empty, start_byte, context) = match top {
            Some(Frame::Mapping {
                start_byte,
                style,
                saw_child,
                ..
            }) => (
                !*saw_child,
                *start_byte,
                match style {
                    ContextStyle::Block => SpanContext::BlockMapValue,
                    ContextStyle::Flow => SpanContext::FlowMapValue,
                },
            ),
            Some(Frame::Sequence {
                start_byte,
                style,
                index,
            }) => (
                *index == 0,
                *start_byte,
                match style {
                    ContextStyle::Block => SpanContext::BlockSeqItem,
                    ContextStyle::Flow => SpanContext::FlowSeqItem,
                },
            ),
            None => (false, 0, SpanContext::BlockMapValue),
        };
        if is_empty {
            let pointer = if self.path.is_empty() {
                String::new()
            } else {
                format!("/{}", self.path.join("/"))
            };
            // Don't clobber a span the scalar branch already recorded —
            // saphyr-parser only emits Mapping/SequenceEnd for *container*
            // frames, so collision should be unreachable; the guard
            // documents the invariant.
            if !spans.contains_key(&pointer) {
                let value_range = start_byte.min(self.bytes.len())..end_byte.min(self.bytes.len());
                let line_range = self.compute_line_range(&value_range, context, ScalarStyle::Plain);
                // `indent` is the column of the container's opening byte,
                // which mirrors the parser convention for scalar `indent`.
                let indent =
                    u32::try_from(start_to_col(self.bytes, start_byte)).unwrap_or(u32::MAX);
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
        self.leave_container();
    }

    fn doc_prefix_len(&self) -> usize {
        if self.multi_doc { 1 } else { 0 }
    }

    fn value_context(&self) -> SpanContext {
        match self.stack.last() {
            Some(Frame::Sequence {
                style: ContextStyle::Flow,
                ..
            }) => SpanContext::FlowSeqItem,
            Some(Frame::Sequence {
                style: ContextStyle::Block,
                ..
            }) => SpanContext::BlockSeqItem,
            Some(Frame::Mapping {
                style: ContextStyle::Flow,
                ..
            }) => SpanContext::FlowMapValue,
            Some(Frame::Mapping {
                style: ContextStyle::Block,
                ..
            })
            | None => SpanContext::BlockMapValue,
        }
    }

    /// Compute the line range for a value span. Block contexts span the full
    /// physical line(s); flow contexts have no logical line so the line range
    /// degenerates to the value range.
    fn compute_line_range(
        &self,
        value_range: &Range<usize>,
        context: SpanContext,
        scalar_style: ScalarStyle,
    ) -> Range<usize> {
        // For flow contexts a single value is part of `{a: 1, b: 2}` on one
        // line — there is no "delete this physical line" semantics. Returning
        // the value range itself keeps `del_at` from accidentally deleting
        // an entire flow container.
        if matches!(
            context,
            SpanContext::FlowMapValue | SpanContext::FlowSeqItem
        ) {
            return value_range.clone();
        }
        // Block scalars (`|` / `>`) already include trailing-newline content
        // in the value range. We still want `del_at` to pull the entire
        // logical entry; expand to the next newline boundary like a regular
        // value, since the value range typically ends at end-of-line already.
        let _ = scalar_style;

        let bytes = self.bytes;
        let mut start = value_range.start.min(bytes.len());
        while start > 0 && bytes[start - 1] != b'\n' {
            start -= 1;
        }
        let mut end = value_range.end.min(bytes.len());
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        if end < bytes.len() {
            end += 1; // include trailing newline
        }
        start..end
    }
}

/// Detect block-vs-flow container style by inspecting the byte at the
/// container's reported start position. Saphyr-parser 0.0.6 does not expose
/// this on the event itself, but the byte at the start of the container is
/// reliably `{` for flow mappings and `[` for flow sequences (block ones
/// start on either a key character or `-`).
///
/// `start` is a **byte** offset — callers must translate the marker's
/// character index through [`char_index_to_byte`] before invoking this.
fn detect_container_style(bytes: &[u8], start: usize) -> ContextStyle {
    match bytes.get(start) {
        Some(b'{' | b'[') => ContextStyle::Flow,
        _ => ContextStyle::Block,
    }
}

/// Compute the 1-indexed column of a byte offset by counting characters
/// from the previous `\n`. Used as the `indent` field of an empty-container
/// span — matches the convention saphyr-parser reports for scalar `indent`
/// (1-indexed column).
fn start_to_col(bytes: &[u8], byte_idx: usize) -> usize {
    let cap = bytes.len();
    let mut line_start = byte_idx.min(cap);
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    byte_idx.saturating_sub(line_start).saturating_add(1)
}

fn pointer_escape(segment: &str) -> String {
    // Order matters: `~` MUST be escaped before `/`; otherwise we'd
    // double-escape the `~` we just emitted for `/`.
    segment.replace('~', "~0").replace('/', "~1")
}

/// Convert a `saphyr_parser::Span` (char-indexed) into a byte-indexed
/// [`Range<usize>`] suitable for slicing the original source bytes.
///
/// `saphyr-parser` 0.0.6 reports `Marker::index()` as a character count, not
/// a byte offset. The splice path keys on byte offsets, so each marker's
/// char-index is translated through `char_to_byte` (built once per parse by
/// [`build_char_to_byte_map`]).
fn span_to_range(span: Span, char_to_byte: &[usize]) -> Range<usize> {
    let start = char_index_to_byte(char_to_byte, span.start.index());
    let end = char_index_to_byte(char_to_byte, span.end.index());
    start..end
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

/// Renderer for in-span scalar replacements in YAML documents.
///
/// Style preservation rules:
///
/// - Original `Plain` (bare) — emit bare unless the new value contains
///   characters that demand quoting (colons, hashes, leading whitespace),
///   then upgrade to double-quoted.
/// - Original `SingleQuoted` — emit single-quoted, doubling embedded `'`.
/// - Original `DoubleQuoted` — emit double-quoted with full escape rules.
/// - Original `Literal` (`|`) / `Folded` (`>`) — downgrade to double-quoted.
///   Round-tripping multi-line block scalars is a M3 polishing concern; the
///   M2 baseline keeps every other byte in the file intact and only the
///   replaced value's representation changes.
///
/// Flow contexts (`FlowMapValue`, `FlowSeqItem`) always quote when the value
/// contains structural characters (`,`, `]`, `}`).
#[derive(Debug, Default, Clone, Copy)]
pub struct YamlScalarRenderer;

impl ScalarRenderer for YamlScalarRenderer {
    fn render_replacement(&self, value: &Value, context: SpanContext, original: &[u8]) -> Vec<u8> {
        let original_style = detect_scalar_style(original);
        match value {
            Value::Null => render_null(original_style),
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
            Value::String(s) => render_string_in_style(s, original_style, context),
            // Replacing a scalar span with a structural value is
            // intentionally out of scope for the M2 baseline: the mkdir-p /
            // structural-edit path lives in §3.3+ for individual leaves, and
            // wholesale tree replacement at a scalar position would require
            // re-running the parser to refresh spans. We render a bare value
            // that keeps the splice well-formed (`null`) so callers see the
            // anomaly downstream rather than a panic mid-write.
            Value::Array(_) | Value::Map(_) => render_null(original_style),
        }
    }
}

/// Renderer for brand-new key/value pairs inserted into a parent container.
///
/// Produces a leading newline so the splice can simply be appended after the
/// parent's last existing line. Indentation is `parent_indent + 2`. Multi-line
/// values (Map / Array) are emitted in block style with one further indent
/// step. `parent_indent` is interpreted as the 1-indexed column of the
/// parent's children — the renderer subtracts 1 so it can build space prefixes
/// directly.
#[derive(Debug, Default, Clone, Copy)]
pub struct YamlInsertionRenderer;

impl InsertionRenderer for YamlInsertionRenderer {
    fn render_insertion(
        &self,
        key: &str,
        value: &Value,
        parent_indent: u32,
        parent_context: SpanContext,
    ) -> Vec<u8> {
        // Flow contexts use a different splice strategy (insert before `}` or
        // `]` with `, ` separators) which the M2 baseline does not yet
        // implement. For now we render a block-style fragment and let the
        // caller decide whether to splice it into a flow container; in
        // practice `Document::set_at`'s mkdir-p path only fires for block
        // contexts because that's what `value_only`-shaped docs lack and
        // those documents always reflect block-style configs.
        let _ = parent_context;

        let outer_spaces = parent_indent.saturating_sub(1) as usize;
        let inner_spaces = outer_spaces + 2;
        let key_escaped = yaml_key_token(key);
        let mut out = Vec::new();
        out.push(b'\n');
        out.extend(std::iter::repeat_n(b' ', outer_spaces));
        out.extend_from_slice(key_escaped.as_bytes());
        out.push(b':');
        match value {
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::BigInt(_)
            | Value::Float(_)
            | Value::BigFloat(_)
            | Value::String(_) => {
                out.push(b' ');
                let scalar = render_scalar_inline(value);
                out.extend_from_slice(&scalar);
                out.push(b'\n');
            }
            Value::Array(items) => {
                out.push(b'\n');
                if items.is_empty() {
                    // Empty sequences must still parse — `key: []` is the
                    // shortest form, but we already emitted `key:\n` so we
                    // backtrack to a flow-style empty literal.
                    out.pop();
                    out.extend_from_slice(b" []\n");
                } else {
                    for item in items {
                        out.extend(std::iter::repeat_n(b' ', inner_spaces));
                        out.extend_from_slice(b"- ");
                        let rendered = render_scalar_inline(item);
                        out.extend_from_slice(&rendered);
                        out.push(b'\n');
                    }
                }
            }
            Value::Map(map) => {
                out.push(b'\n');
                if map.is_empty() {
                    out.pop();
                    out.extend_from_slice(b" {}\n");
                } else {
                    for (k, v) in map {
                        out.extend(std::iter::repeat_n(b' ', inner_spaces));
                        out.extend_from_slice(yaml_key_token(k).as_bytes());
                        out.push(b':');
                        out.push(b' ');
                        let rendered = render_scalar_inline(v);
                        out.extend_from_slice(&rendered);
                        out.push(b'\n');
                    }
                }
            }
        }
        out
    }
}

/// Render a scalar (leaf) value as its inline YAML form. Containers fall back
/// to a flow-style empty literal to keep the result well-formed; full nested
/// rendering is the caller's responsibility.
fn render_scalar_inline(value: &Value) -> Vec<u8> {
    match value {
        Value::Null => b"null".to_vec(),
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
        Value::String(s) => {
            render_string_in_style(s, ScalarStyle::Plain, SpanContext::BlockMapValue)
        }
        Value::Array(_) => b"[]".to_vec(),
        Value::Map(_) => b"{}".to_vec(),
    }
}

/// Render a YAML mapping key. Most keys are bare-safe; we quote conservatively
/// when special characters demand it.
fn yaml_key_token(key: &str) -> String {
    if needs_quoting_for_plain(key) {
        let mut out = String::with_capacity(key.len() + 2);
        out.push('"');
        for c in key.chars() {
            push_double_quoted_char(&mut out, c);
        }
        out.push('"');
        out
    } else {
        key.to_owned()
    }
}

/// Render a `null` scalar matching the original style. Plain → `null`,
/// double-quoted → `"null"`, single-quoted → `'null'`, block scalars
/// downgrade to plain.
fn render_null(original_style: ScalarStyle) -> Vec<u8> {
    match original_style {
        ScalarStyle::DoubleQuoted => b"\"null\"".to_vec(),
        ScalarStyle::SingleQuoted => b"'null'".to_vec(),
        _ => b"null".to_vec(),
    }
}

/// Render an `f64`. NaN/Inf are emitted as YAML's `.nan` / `.inf` (with sign),
/// finite values use the standard `Display` form which already matches what
/// YAML accepts as a `Float` token.
fn render_float(f: f64) -> Vec<u8> {
    if f.is_nan() {
        b".nan".to_vec()
    } else if f.is_infinite() {
        if f.is_sign_negative() {
            b"-.inf".to_vec()
        } else {
            b".inf".to_vec()
        }
    } else {
        // `1.0_f64.to_string()` returns `"1"` — that loses the float-ness in
        // YAML round-trip. Force a fractional component when it's missing.
        let s = f.to_string();
        if s.contains('.') || s.contains('e') || s.contains('E') {
            s.into_bytes()
        } else {
            format!("{s}.0").into_bytes()
        }
    }
}

// ---------------------------------------------------------------------------
// Scalar style detection / string rendering helpers
// ---------------------------------------------------------------------------

/// Detect the style of a scalar from its original byte slice. The probe looks
/// at the first non-whitespace byte — saphyr's scalar spans include
/// surrounding quotes, so a leading `"` / `'` is a reliable signal.
fn detect_scalar_style(bytes: &[u8]) -> ScalarStyle {
    let first = bytes.iter().find(|b| !b.is_ascii_whitespace());
    match first {
        Some(b'"') => ScalarStyle::DoubleQuoted,
        Some(b'\'') => ScalarStyle::SingleQuoted,
        Some(b'|') => ScalarStyle::Literal,
        Some(b'>') => ScalarStyle::Folded,
        _ => ScalarStyle::Plain,
    }
}

/// Render `s` as a YAML scalar, matching `style` when feasible.
///
/// - `Plain`: emit bare unless `s` triggers `needs_quoting_for_plain` or the
///   `context` is flow (flow context promotes any bare scalar containing `,`,
///   `]`, `}` to double-quoted).
/// - `SingleQuoted`: wrap in single quotes; embedded `'` is doubled.
/// - `DoubleQuoted`: wrap in double quotes; control chars escape per YAML
///   1.2 §5.7.
/// - `Literal` / `Folded`: downgrade to double-quoted (lose multi-line
///   semantics — see module docs).
fn render_string_in_style(s: &str, style: ScalarStyle, context: SpanContext) -> Vec<u8> {
    let flow = matches!(
        context,
        SpanContext::FlowMapValue | SpanContext::FlowSeqItem
    );
    match style {
        ScalarStyle::Plain => {
            if needs_quoting_for_plain(s) || (flow && needs_flow_quoting(s)) {
                render_double_quoted(s)
            } else {
                s.as_bytes().to_vec()
            }
        }
        ScalarStyle::SingleQuoted => render_single_quoted(s),
        ScalarStyle::DoubleQuoted => render_double_quoted(s),
        ScalarStyle::Literal | ScalarStyle::Folded => render_double_quoted(s),
    }
}

fn render_single_quoted(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    out.push(b'\'');
    for ch in s.chars() {
        if ch == '\'' {
            // YAML single-quoted strings escape `'` by doubling it.
            out.extend_from_slice(b"''");
        } else {
            // chars are valid UTF-8; encode in place.
            let mut buf = [0_u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
    out.push(b'\'');
    out
}

fn render_double_quoted(s: &str) -> Vec<u8> {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        push_double_quoted_char(&mut out, ch);
    }
    out.push('"');
    out.into_bytes()
}

fn push_double_quoted_char(out: &mut String, ch: char) {
    match ch {
        '\\' => out.push_str(r"\\"),
        '"' => out.push_str("\\\""),
        '\n' => out.push_str(r"\n"),
        '\r' => out.push_str(r"\r"),
        '\t' => out.push_str(r"\t"),
        '\x00' => out.push_str(r"\0"),
        '\x07' => out.push_str(r"\a"),
        '\x08' => out.push_str(r"\b"),
        '\x0b' => out.push_str(r"\v"),
        '\x0c' => out.push_str(r"\f"),
        '\x1b' => out.push_str(r"\e"),
        c if (c as u32) < 0x20 => {
            // Other ASCII control chars: \xNN form.
            use std::fmt::Write as _;
            let _ = write!(out, "\\x{:02x}", c as u32);
        }
        c => out.push(c),
    }
}

/// True when `s` cannot be rendered as a plain (bare) scalar in any context.
///
/// The list is conservative — anything ambiguous gets quoted. We do not
/// attempt to handle every corner of YAML 1.2 §6.5; we just ensure round-trip
/// validity for the values M2 actually writes.
fn needs_quoting_for_plain(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // YAML "type tags" — values that would parse as a different scalar type
    // if emitted bare. Listing the canonical forms; `parse_scalar_as_type`
    // would catch more, but the cost of the extra quoting is negligible.
    if matches!(
        s,
        "null"
            | "Null"
            | "NULL"
            | "~"
            | "true"
            | "True"
            | "TRUE"
            | "false"
            | "False"
            | "FALSE"
            | "yes"
            | "Yes"
            | "YES"
            | "no"
            | "No"
            | "NO"
            | "on"
            | "On"
            | "ON"
            | "off"
            | "Off"
            | "OFF"
    ) {
        return true;
    }
    if s.starts_with(' ') || s.ends_with(' ') {
        return true;
    }
    let first = s.chars().next().expect("non-empty");
    // Indicators that must not start a plain scalar.
    if matches!(
        first,
        '!' | '&'
            | '*'
            | '['
            | ']'
            | '{'
            | '}'
            | ','
            | '#'
            | '|'
            | '>'
            | '\''
            | '"'
            | '%'
            | '@'
            | '`'
            | ':'
            | '?'
            | '-'
    ) {
        // `-` is allowed as a leading char if not followed by whitespace,
        // but the safe choice is to quote.
        return true;
    }
    if s.contains('\n') || s.contains('\r') || s.contains('\t') {
        return true;
    }
    if contains_unsafe_colon_or_hash(s) {
        return true;
    }
    if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok() {
        // A numeric literal would round-trip as a number, not a string.
        return true;
    }
    false
}

/// `: ` and ` #` cause YAML key/comment confusion in plain scalars.
fn contains_unsafe_colon_or_hash(s: &str) -> bool {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b':' && bytes.get(i + 1).is_some_and(|n| *n == b' ' || *n == b'\t') {
            return true;
        }
        if b == b'#' && i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
            return true;
        }
    }
    false
}

/// Plain scalars in flow contexts must additionally avoid `,`, `]`, `}`.
fn needs_flow_quoting(s: &str) -> bool {
    s.contains([',', ']', '}'])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pointer;
    use indexmap::IndexMap;

    fn assert_span(spans: &SpanMap, pointer: &str, bytes: &[u8], expected_value: &[u8]) {
        let span = spans.get(pointer).unwrap_or_else(|| {
            panic!(
                "missing span for {pointer}; have keys: {:?}",
                spans.keys().collect::<Vec<_>>()
            )
        });
        let slice = &bytes[span.value_range.clone()];
        assert_eq!(slice, expected_value, "value bytes mismatch at {pointer}");
    }

    // -- span builder ---------------------------------------------------

    #[test]
    fn parse_with_spans_records_block_map_value() {
        let bytes = b"a: 1\n";
        let (value, spans) = parse_with_spans(bytes).expect("parse_with_spans");
        // Value tree shape via M1 read path — single map with one int.
        match &value {
            Value::Map(map) => assert_eq!(map.get("a"), Some(&Value::Int(1))),
            other => panic!("expected map, got: {other:?}"),
        }
        // Span covers exactly the `1`.
        assert_span(&spans, "/a", bytes, b"1");
        let span = spans.get("/a").expect("/a span");
        assert_eq!(
            span.context,
            SpanContext::BlockMapValue,
            "single-line block mapping value must be tagged BlockMapValue",
        );
        assert_eq!(
            span.line_range,
            0..5,
            "line_range covers `a: 1\\n` entirely"
        );
    }

    #[test]
    fn parse_with_spans_quoted_string_includes_quotes() {
        let bytes = b"title: \"Hello, dq\"\n";
        let (_, spans) = parse_with_spans(bytes).expect("parse");
        // The value range MUST include the surrounding quotes — saphyr-parser
        // reports it that way and the splice path counts on it.
        assert_span(&spans, "/title", bytes, b"\"Hello, dq\"");
    }

    #[test]
    fn parse_with_spans_block_sequence_items() {
        let bytes = b"tags:\n  - rust\n  - cli\n";
        let (_, spans) = parse_with_spans(bytes).expect("parse");
        assert_span(&spans, "/tags/0", bytes, b"rust");
        assert_span(&spans, "/tags/1", bytes, b"cli");
        assert_eq!(
            spans.get("/tags/0").unwrap().context,
            SpanContext::BlockSeqItem
        );
        assert_eq!(
            spans.get("/tags/1").unwrap().context,
            SpanContext::BlockSeqItem
        );
    }

    #[test]
    fn parse_with_spans_flow_mapping_uses_flow_context() {
        let bytes = b"data: {a: 1, b: 2}\n";
        let (_, spans) = parse_with_spans(bytes).expect("parse");
        assert_span(&spans, "/data/a", bytes, b"1");
        assert_span(&spans, "/data/b", bytes, b"2");
        assert_eq!(
            spans.get("/data/a").unwrap().context,
            SpanContext::FlowMapValue
        );
    }

    #[test]
    fn parse_with_spans_flow_sequence_uses_flow_context() {
        let bytes = b"items: [1, 2, 3]\n";
        let (_, spans) = parse_with_spans(bytes).expect("parse");
        assert_span(&spans, "/items/0", bytes, b"1");
        assert_span(&spans, "/items/1", bytes, b"2");
        assert_eq!(
            spans.get("/items/0").unwrap().context,
            SpanContext::FlowSeqItem
        );
    }

    #[test]
    fn parse_with_spans_multi_doc_namespaces_pointers() {
        let bytes = b"---\nname: a\n---\nname: b\n";
        let (_, spans) = parse_with_spans(bytes).expect("parse");
        assert!(
            spans.contains_key("/0/name"),
            "missing /0/name; have: {:?}",
            spans.keys().collect::<Vec<_>>()
        );
        assert!(
            spans.contains_key("/1/name"),
            "missing /1/name; have: {:?}",
            spans.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_with_spans_invalid_utf8_returns_parse_error() {
        let bytes: &[u8] = &[0xFF, 0xFE, b'a', b':', b' ', b'1'];
        let err = parse_with_spans(bytes).expect_err("invalid UTF-8 must error");
        assert!(matches!(err, Error::Parse { .. }), "got: {err:?}");
    }

    #[test]
    fn parse_with_spans_scan_error_carries_position() {
        // Unbalanced flow: `[a, b` with no closing `]`.
        let bytes = b"key: [a, b\n";
        let err = parse_with_spans(bytes).expect_err("unbalanced flow must error");
        match err {
            Error::Parse { line, .. } => {
                assert!(
                    line >= 1,
                    "Parse error must carry a non-zero line; got line={line}"
                );
            }
            other => panic!("expected Parse, got: {other:?}"),
        }
    }

    #[test]
    fn parse_yaml_with_spans_returns_write_aware_document() {
        let bytes = b"a: 1\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse_yaml_with_spans");
        assert_eq!(doc.original_bytes(), bytes);
        assert_eq!(doc.format(), FormatTag::Yaml);
        assert!(doc.spans().contains_key("/a"));
    }

    // -- scalar style detection ----------------------------------------

    #[test]
    fn detect_scalar_style_classifies_each_form() {
        assert_eq!(detect_scalar_style(b"plain"), ScalarStyle::Plain);
        assert_eq!(
            detect_scalar_style(b"\"quoted\""),
            ScalarStyle::DoubleQuoted
        );
        assert_eq!(detect_scalar_style(b"'quoted'"), ScalarStyle::SingleQuoted);
        assert_eq!(detect_scalar_style(b"|\n  text"), ScalarStyle::Literal);
        assert_eq!(detect_scalar_style(b"> folded"), ScalarStyle::Folded);
    }

    // -- ScalarRenderer -------------------------------------------------

    #[test]
    fn renderer_replaces_int_with_int() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(&Value::Int(5), SpanContext::BlockMapValue, b"3");
        assert_eq!(out, b"5");
    }

    #[test]
    fn renderer_replaces_bool_plain() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(&Value::Bool(true), SpanContext::BlockMapValue, b"false");
        assert_eq!(out, b"true");
    }

    #[test]
    fn renderer_preserves_double_quoted_string_style() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("Updated".into()),
            SpanContext::BlockMapValue,
            b"\"Hello\"",
        );
        assert_eq!(out, b"\"Updated\"");
    }

    #[test]
    fn renderer_preserves_single_quoted_string_style() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("Updated".into()),
            SpanContext::BlockMapValue,
            b"'Hello'",
        );
        assert_eq!(out, b"'Updated'");
    }

    #[test]
    fn renderer_escapes_quote_in_double_quoted() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("a\"b".into()),
            SpanContext::BlockMapValue,
            b"\"x\"",
        );
        assert_eq!(out, b"\"a\\\"b\"");
    }

    #[test]
    fn renderer_doubles_quote_in_single_quoted() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("it's".into()),
            SpanContext::BlockMapValue,
            b"'x'",
        );
        assert_eq!(out, b"'it''s'");
    }

    #[test]
    fn renderer_upgrades_bare_to_quoted_for_unsafe_chars() {
        let r = YamlScalarRenderer;
        // `:` followed by space is unsafe in plain YAML — must quote.
        let out = r.render_replacement(
            &Value::String("a: b".into()),
            SpanContext::BlockMapValue,
            b"plain",
        );
        assert_eq!(out, b"\"a: b\"");
    }

    #[test]
    fn renderer_preserves_bare_for_safe_strings() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("Hello".into()),
            SpanContext::BlockMapValue,
            b"World",
        );
        assert_eq!(out, b"Hello");
    }

    #[test]
    fn renderer_quotes_yaml_keyword_strings() {
        let r = YamlScalarRenderer;
        // Bare `true` would deserialize as a boolean, not the string "true".
        let out = r.render_replacement(
            &Value::String("true".into()),
            SpanContext::BlockMapValue,
            b"x",
        );
        assert_eq!(out, b"\"true\"");
    }

    #[test]
    fn renderer_renders_null() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(&Value::Null, SpanContext::BlockMapValue, b"x");
        assert_eq!(out, b"null");
    }

    #[test]
    fn renderer_renders_float_with_fractional() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(&Value::Float(1.0), SpanContext::BlockMapValue, b"0.5");
        // `1.0_f64.to_string()` is `"1"`; we add `.0` to keep the float-ness.
        assert_eq!(out, b"1.0");
    }

    #[test]
    fn renderer_downgrades_literal_block_to_double_quoted() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("new content".into()),
            SpanContext::BlockMapValue,
            b"|\n  old\n",
        );
        assert_eq!(out, b"\"new content\"");
    }

    #[test]
    fn renderer_quotes_string_with_comma_in_flow_context() {
        let r = YamlScalarRenderer;
        let out = r.render_replacement(
            &Value::String("a,b".into()),
            SpanContext::FlowSeqItem,
            b"plain",
        );
        // `,` ends a flow sequence item; bare would corrupt the structure.
        assert_eq!(out, b"\"a,b\"");
    }

    // -- InsertionRenderer ----------------------------------------------

    #[test]
    fn insertion_renderer_produces_block_scalar_pair() {
        let r = YamlInsertionRenderer;
        let out = r.render_insertion(
            "name",
            &Value::String("dq".into()),
            3, // parent's children indent at column 3 (1-indexed) → 2 spaces
            SpanContext::BlockMapValue,
        );
        assert_eq!(out, b"\n  name: dq\n");
    }

    #[test]
    fn insertion_renderer_renders_nested_map() {
        let r = YamlInsertionRenderer;
        let mut inner = IndexMap::new();
        inner.insert("type".into(), Value::String("RollingUpdate".into()));
        let out = r.render_insertion(
            "strategy",
            &Value::Map(inner),
            3,
            SpanContext::BlockMapValue,
        );
        assert_eq!(out, b"\n  strategy:\n    type: RollingUpdate\n");
    }

    #[test]
    fn insertion_renderer_renders_array() {
        let r = YamlInsertionRenderer;
        let out = r.render_insertion(
            "tags",
            &Value::Array(vec![
                Value::String("rust".into()),
                Value::String("cli".into()),
            ]),
            3,
            SpanContext::BlockMapValue,
        );
        assert_eq!(out, b"\n  tags:\n    - rust\n    - cli\n");
    }

    #[test]
    fn insertion_renderer_renders_empty_array_inline() {
        let r = YamlInsertionRenderer;
        let out = r.render_insertion(
            "tags",
            &Value::Array(Vec::new()),
            3,
            SpanContext::BlockMapValue,
        );
        assert_eq!(out, b"\n  tags: []\n");
    }

    // -- end-to-end smoke ----------------------------------------------

    #[test]
    fn document_set_at_byte_perfect_replacement() {
        // The smoke test the user's prompt asked for: set_at on a YAML
        // document built via `parse_yaml_with_spans` must splice the new
        // bytes into the original buffer with no other changes.
        let bytes = b"a: 3\n";
        let mut doc = parse_yaml_with_spans(bytes).expect("parse");
        let pointer = Pointer::parse("/a").expect("pointer");
        doc.set_at(&pointer, Value::Int(5)).expect("set_at");
        assert_eq!(
            doc.original_bytes(),
            b"a: 5\n",
            "set_at must replace exactly the value bytes",
        );
        // The in-memory tree mirrors the byte buffer.
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(5))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn document_set_at_preserves_comments_and_other_lines() {
        // Round-trip property the spike validated: the splice changes one
        // line and leaves every other byte alone — including same-line
        // trailing comments.
        let bytes = b"# header\nport: 8080 # default\nname: dq\n";
        let mut doc = parse_yaml_with_spans(bytes).expect("parse");
        let pointer = Pointer::parse("/port").expect("pointer");
        doc.set_at(&pointer, Value::Int(9090)).expect("set_at");
        assert_eq!(
            doc.original_bytes(),
            b"# header\nport: 9090 # default\nname: dq\n",
            "set_at must only touch the value bytes; comments, whitespace, \
             and other lines must remain byte-identical",
        );
    }

    #[test]
    fn document_set_at_preserves_quote_style() {
        let bytes = b"title: \"Hello\"\n";
        let mut doc = parse_yaml_with_spans(bytes).expect("parse");
        let pointer = Pointer::parse("/title").expect("pointer");
        doc.set_at(&pointer, Value::String("Updated".into()))
            .expect("set_at");
        assert_eq!(doc.original_bytes(), b"title: \"Updated\"\n");
    }

    // -- mkdir-p (Bug #1) -----------------------------------------------

    #[test]
    fn insertion_renderer_inserts_with_correct_block_indent() {
        // Bug #1: nested map mkdir-p must produce well-formed YAML with
        // the children's indent matching the existing siblings (not the
        // siblings' value column, which is what `ValueSpan::indent`
        // records — `Document::try_single_level_mkdir_p` re-derives the
        // key column by scanning back through the source line).
        let bytes = b"a:\n  b: 1\n";
        let mut doc = parse_yaml_with_spans(bytes).expect("parse");
        let pointer = Pointer::parse("/a/c").expect("pointer");
        doc.set_at(&pointer, Value::Int(42))
            .expect("mkdir-p set_at must succeed");
        // The rendered bytes must re-parse to a tree containing both /a/b
        // and the newly-inserted /a/c with the right indent (otherwise
        // YAML's "mapping values are not allowed" error fires on re-parse).
        let reparsed = parse_yaml_with_spans(doc.original_bytes()).expect("re-parse");
        let c = pointer
            .resolve(reparsed.value())
            .expect("reparsed has /a/c");
        assert_eq!(c, &Value::Int(42));
        let b = Pointer::parse("/a/b")
            .unwrap()
            .resolve(reparsed.value())
            .expect("reparsed still has /a/b");
        assert_eq!(b, &Value::Int(1));
    }

    #[test]
    fn insertion_renderer_into_empty_yaml_map_succeeds() {
        // Empty parent (`a: {}`) → `set_at(/a/b, 42)` must splice the new
        // key inside the empty flow map. Pre-fix this returned MissingKey;
        // the YAML span builder now records an empty-container span at
        // the parent's pointer so the splicer can anchor between the `{`
        // and `}`. The reparse round-trips both keys.
        let bytes = b"a: {}\n";
        let mut doc = parse_yaml_with_spans(bytes).expect("parse");
        let pointer = Pointer::parse("/a/b").expect("pointer");
        doc.set_at(&pointer, Value::Int(42))
            .expect("empty-parent mkdir-p on `a: {}` must succeed");
        let reparsed = parse_yaml_with_spans(doc.original_bytes()).expect("re-parse");
        let added = pointer
            .resolve(reparsed.value())
            .expect("reparsed has /a/b");
        assert_eq!(added, &Value::Int(42));
    }

    #[test]
    fn insertion_renderer_into_root_yaml_map_succeeds() {
        // Root-level mkdir-p — same shape as the nested case but parent
        // is the implicit root mapping.
        let bytes = b"a: 1\n";
        let mut doc = parse_yaml_with_spans(bytes).expect("parse");
        let pointer = Pointer::parse("/b").expect("pointer");
        doc.set_at(&pointer, Value::Int(42))
            .expect("root mkdir-p set_at must succeed");
        let reparsed = parse_yaml_with_spans(doc.original_bytes()).expect("re-parse");
        let b = pointer.resolve(reparsed.value()).expect("reparsed has /b");
        assert_eq!(b, &Value::Int(42));
    }

    // -- Phase 2 (`add-validation-and-extended-formats`) ------------------
    //
    // Inline-offset population for YAML block scalars. Phase 2 spec
    // ("YAML block scalar carries inline-offset") requires every leaf whose
    // backing scalar uses `|`, `>`, `|-`, or `>-` to surface
    // `inline_offset = Some(InlineBaseline { 0, 1, 1 })` on its
    // `Provenance::Original` entry; every other style — plain, single-quoted,
    // double-quoted — keeps `inline_offset = None`.

    /// Helper: pull the `inline_offset` field out of the Original provenance
    /// entry for `pointer_str`. Panics if the entry is missing or Synthetic
    /// — the YAML write-aware path is supposed to emit Original for every
    /// leaf, so a missing entry indicates a regression worth surfacing.
    fn inline_offset_for(doc: &Document, pointer_str: &str) -> Option<InlineBaseline> {
        let pointer = Pointer::parse(pointer_str).expect("pointer parses");
        match doc.as_ir().provenance_for(&pointer) {
            Some(Provenance::Original { inline_offset, .. }) => *inline_offset,
            other => panic!("expected Original for `{pointer_str}`, got: {other:?}"),
        }
    }

    #[test]
    fn yaml_literal_block_scalar_carries_inline_offset() {
        // `|` indicator → ScalarStyle::Literal. The body starts at line 1
        // col 1 of its own content — that is the contract every composite
        // rule projection depends on.
        let bytes = b"script: |\n  echo 1\n  echo 2\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse");
        assert_eq!(
            inline_offset_for(&doc, "/script"),
            Some(InlineBaseline {
                byte_start: 0,
                line: 1,
                col: 1,
            }),
            "literal block scalar (`|`) MUST carry inline_offset = Some(0,1,1)",
        );
    }

    #[test]
    fn yaml_folded_block_scalar_carries_inline_offset() {
        // `>` indicator → ScalarStyle::Folded. Same contract as Literal —
        // the projection treats every block-style as inline-content with
        // baseline (0, 1, 1).
        let bytes = b"description: >\n  multi-line\n  folded\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse");
        assert_eq!(
            inline_offset_for(&doc, "/description"),
            Some(InlineBaseline {
                byte_start: 0,
                line: 1,
                col: 1,
            }),
            "folded block scalar (`>`) MUST carry inline_offset = Some(0,1,1)",
        );
    }

    #[test]
    fn yaml_strip_chomping_block_scalar_carries_inline_offset() {
        // `|-` and `>-` are block scalars with strip-chomping. saphyr-parser
        // collapses them onto the same Literal/Folded discriminants, so the
        // contract is identical.
        for bytes in [
            &b"script: |-\n  one\n  two\n"[..],
            &b"script: >-\n  one\n  two\n"[..],
        ] {
            let doc = parse_yaml_with_spans(bytes).expect("parse");
            assert_eq!(
                inline_offset_for(&doc, "/script"),
                Some(InlineBaseline {
                    byte_start: 0,
                    line: 1,
                    col: 1,
                }),
                "strip-chomping block scalar variant MUST carry inline_offset = Some(0,1,1) \
                 for input: {:?}",
                std::str::from_utf8(bytes).unwrap_or("<non-utf8>"),
            );
        }
    }

    #[test]
    fn yaml_plain_scalar_carries_no_inline_offset() {
        // ScalarStyle::Plain → no block-style → inline_offset = None. The
        // negative case is just as load-bearing as the positive case: a
        // regression that started populating inline_offset for plain scalars
        // would corrupt every composite-rule coordinate projection.
        let bytes = b"name: foo\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse");
        assert_eq!(
            inline_offset_for(&doc, "/name"),
            None,
            "plain scalar MUST NOT carry an inline-offset baseline",
        );
    }

    #[test]
    fn yaml_double_quoted_scalar_carries_no_inline_offset() {
        let bytes = b"title: \"Hello\"\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse");
        assert_eq!(
            inline_offset_for(&doc, "/title"),
            None,
            "double-quoted scalar MUST NOT carry an inline-offset baseline",
        );
    }

    #[test]
    fn yaml_single_quoted_scalar_carries_no_inline_offset() {
        let bytes = b"name: 'foo'\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse");
        assert_eq!(
            inline_offset_for(&doc, "/name"),
            None,
            "single-quoted scalar MUST NOT carry an inline-offset baseline",
        );
    }

    #[test]
    fn yaml_inline_offset_lookup_via_ir_helper_matches_provenance() {
        // Cross-check the public lookup helper `Ir::inline_offset_for`
        // against the underlying `Provenance::Original.inline_offset`
        // field. A drift between the two would mean callers using the
        // helper get different answers than callers pattern-matching
        // directly — the kind of subtle bug a contract test pins down.
        let bytes = b"script: |\n  echo 1\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse");
        let pointer = Pointer::parse("/script").expect("pointer");
        let helper = doc.as_ir().inline_offset_for(&pointer);
        let expected = InlineBaseline {
            byte_start: 0,
            line: 1,
            col: 1,
        };
        assert_eq!(helper, Some(&expected));
    }

    #[test]
    fn yaml_block_scalar_inline_offset_in_nested_path() {
        // Block scalars also live deep inside container structures. Pin
        // that the canonical path is constructed correctly when the leaf
        // is reached through nested mappings.
        let bytes = b"jobs:\n  build:\n    script: |\n      cargo test\n";
        let doc = parse_yaml_with_spans(bytes).expect("parse");
        assert_eq!(
            inline_offset_for(&doc, "/jobs/build/script"),
            Some(InlineBaseline {
                byte_start: 0,
                line: 1,
                col: 1,
            }),
            "block scalar inside nested mapping MUST still carry inline_offset",
        );
    }
}
