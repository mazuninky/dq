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

use std::collections::{HashMap, HashSet};
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
        // An empty key (`"": v`, a null key `? &a`) gives a pointer with an
        // empty segment, which `Pointer::parse` rejects: no pointer can
        // address that node, so it gets no provenance.
        let Ok(pointer) = Pointer::parse(canonical) else {
            tracing::debug!(pointer = %canonical, "no provenance for an unaddressable span");
            continue;
        };
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
/// Step 1 ([`prescan`]) counts documents (so we know whether to namespace
/// pointers under `/{doc_idx}`) and collects the block scalars with node
/// properties; step 2 ([`replay_block_scalar_body_starts`]) finds their
/// 0.0.6 span starts in one extra pass, skipped when there are none; step 3
/// walks events recording one [`ValueSpan`] per scalar value (not per scalar
/// key) and, alongside, recording the canonical pointer of every block-style
/// leaf for the inline-offset enrichment in [`parse_yaml_with_spans`]. The
/// pass count does not grow with the number of scalars.
///
/// # saphyr-parser char-vs-byte caveat
///
/// `saphyr_parser::Marker::index()` reports a **character** index (count of
/// Unicode scalars seen so far) despite the rustdoc claiming bytes — see the
/// `index` field comment on `Marker` in `saphyr-parser`'s `src/scanner.rs`
/// (0.0.6 and 0.3 alike). The splice
/// path needs byte offsets, so we precompute a char-index → byte-offset map
/// up front and translate every marker through [`char_index_to_byte`] before
/// constructing a [`Range<usize>`]. Without this, any non-ASCII byte before a
/// span (em-dash in a comment, accented identifier, …) shifts every later
/// span's `value_range` by `(utf8_len - 1)` bytes per multi-byte codepoint.
fn build_span_map_and_block_scalars(bytes: &[u8]) -> Result<(SpanMap, HashSet<String>)> {
    let built = build_span_map_with_stats(bytes)?;
    tracing::trace!(
        parser_passes = built.parser_passes,
        spans = built.spans.len(),
        "built YAML span map",
    );
    Ok((built.spans, built.block_scalars))
}

/// Output of [`build_span_map_with_stats`].
struct BuiltSpans {
    spans: SpanMap,
    block_scalars: HashSet<String>,
    /// Number of `saphyr-parser` passes over the source (or a blanked copy
    /// of it). Two without block scalars carrying node properties, three
    /// with them — independent of how many there are.
    parser_passes: usize,
}

/// [`build_span_map_and_block_scalars`], also reporting how many parser
/// passes the build took.
///
/// The passes are: [`prescan`] (document count, block scalars with node
/// properties, aliases), at most one [`replay_block_scalar_body_starts`]
/// over a blanked copy, and the main [`State`] walk.
fn build_span_map_with_stats(bytes: &[u8]) -> Result<BuiltSpans> {
    let text = std::str::from_utf8(bytes).map_err(|err| Error::Parse {
        file: None,
        line: 0,
        col: 0,
        span: 0..0,
        snippet: String::new(),
        message: format!("source is not valid UTF-8: {err}"),
    })?;

    // One-time O(N) precompute. Lookup by char-index then becomes O(1).
    let char_to_byte = build_char_to_byte_map(text);

    let mut parser_passes = 1;
    let prescan = prescan(text, bytes, &char_to_byte)?;
    let multi_doc = prescan.n_docs > 1;
    let body_starts =
        replay_block_scalar_body_starts(text, &char_to_byte, &prescan, &mut parser_passes);

    let mut spans = SpanMap::new();
    let mut block_scalars: HashSet<String> = HashSet::new();
    let mut state = State::new(text, multi_doc, &char_to_byte, &body_starts);

    parser_passes += 1;
    let mut parser = YamlParser::new_from_str(text);
    while let Some(item) = parser.next_event() {
        let (event, span) =
            item.map_err(|scan_err| scan_to_parse_error(&scan_err, bytes, &char_to_byte))?;
        state.observe(event, span, &mut spans, &mut block_scalars)?;
    }

    Ok(BuiltSpans {
        spans,
        block_scalars,
        parser_passes,
    })
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

/// What the first pass learns before the main walk.
struct Prescan {
    /// Number of `Event::DocumentStart` events. Decides whether the main
    /// walk prefixes pointers with `/{doc_idx}`.
    n_docs: usize,
    /// Every block scalar (`|` / `>`) carrying node properties, in stream
    /// order — the scalars whose 0.0.6 span start needs the replay.
    propertied_block_scalars: Vec<PropertiedBlockScalar>,
    /// Byte range of every alias (`*name`), in stream order.
    aliases: Vec<Range<usize>>,
}

/// A block scalar with node properties, as seen by [`prescan`].
struct PropertiedBlockScalar {
    /// Position of the scalar's event in the stream (`StreamStart` is 0).
    ordinal: usize,
    /// Byte range of the node properties (`&a`, `!t`, `&a !t`, …).
    props: Range<usize>,
    /// Character index where the scalar's span ends. Blanking the properties
    /// leaves it unchanged, so the replay uses it to confirm it is looking
    /// at the same scalar.
    end_char: usize,
}

/// First pass: count documents and collect what
/// [`replay_block_scalar_body_starts`] needs.
fn prescan(text: &str, bytes: &[u8], char_to_byte: &[usize]) -> Result<Prescan> {
    let mut prescan = Prescan {
        n_docs: 0,
        propertied_block_scalars: Vec::new(),
        aliases: Vec::new(),
    };
    let mut parser = YamlParser::new_from_str(text);
    let mut ordinal = 0_usize;
    while let Some(item) = parser.next_event() {
        let (event, span) =
            item.map_err(|scan_err| scan_to_parse_error(&scan_err, bytes, char_to_byte))?;
        match event {
            Event::DocumentStart(_) => prescan.n_docs += 1,
            Event::Scalar(_, ScalarStyle::Literal | ScalarStyle::Folded, anchor, tag)
                if anchor != 0 || tag.is_some() =>
            {
                let n_properties = usize::from(anchor != 0) + usize::from(tag.is_some());
                let props_start = char_index_to_byte(char_to_byte, span.start.index());
                let props_end = skip_node_properties(bytes, props_start, n_properties).0;
                prescan
                    .propertied_block_scalars
                    .push(PropertiedBlockScalar {
                        ordinal,
                        props: props_start..props_end,
                        end_char: span.end.index(),
                    });
            }
            Event::Alias(_) => prescan.aliases.push(span_to_range(span, char_to_byte)),
            _ => {}
        }
        ordinal += 1;
    }
    Ok(prescan)
}

/// Byte offset where saphyr-parser 0.0.6 started the span of each block
/// scalar with node properties — the first content line, not the `|` / `>`
/// indicator — keyed by the scalar's event ordinal.
///
/// The scanner's block-scalar rules (indentation auto-detection, explicit
/// indentation indicators, leading blank lines) are not re-implemented
/// here. Instead the stream is replayed once over a copy of the source in
/// which every such scalar's properties are blanked out (line breaks kept,
/// so character indices and lines stay put): without properties
/// saphyr-parser reports the 0.0.6 start as it is. Blanking an anchor would
/// make its aliases dangling, so every alias is overwritten with a plain
/// scalar of the same length — one event for one event, so ordinals line
/// up.
///
/// A scalar the combined replay cannot confirm (wrong event at its ordinal,
/// a different span end, or a replay error) falls back to
/// [`replay_one_block_scalar`], which blanks only that scalar and stops at
/// it. A scalar missing from the map means even that diverged, which would
/// be a parser bug; the caller then uses the indicator position.
///
/// Costs one parser pass (none when there are no such scalars), plus one per
/// fallback.
fn replay_block_scalar_body_starts(
    text: &str,
    char_to_byte: &[usize],
    prescan: &Prescan,
    parser_passes: &mut usize,
) -> HashMap<usize, usize> {
    let targets = &prescan.propertied_block_scalars;
    let mut starts = HashMap::with_capacity(targets.len());
    if targets.is_empty() {
        return starts;
    }
    let mut unconfirmed: Vec<&PropertiedBlockScalar> = Vec::new();
    if let Some(blanked) = blank_for_replay(text, targets, &prescan.aliases) {
        *parser_passes += 1;
        let mut parser = YamlParser::new_from_str(&blanked);
        let mut pending = targets.iter().peekable();
        let mut ordinal = 0_usize;
        while let Some(target) = pending.peek().copied() {
            let Some(Ok((event, span))) = parser.next_event() else {
                break;
            };
            if ordinal == target.ordinal {
                match event {
                    Event::Scalar(_, ScalarStyle::Literal | ScalarStyle::Folded, _, _)
                        if span.end.index() == target.end_char =>
                    {
                        starts.insert(
                            target.ordinal,
                            char_index_to_byte(char_to_byte, span.start.index()),
                        );
                    }
                    other => {
                        tracing::debug!(
                            ?other,
                            ordinal,
                            "combined block-scalar span replay diverged; replaying the scalar alone",
                        );
                        unconfirmed.push(target);
                    }
                }
                pending.next();
            }
            ordinal += 1;
        }
        unconfirmed.extend(pending);
    } else {
        unconfirmed.extend(targets);
    }
    for target in unconfirmed {
        *parser_passes += 1;
        if let Some(start) = replay_one_block_scalar(text, char_to_byte, target) {
            starts.insert(target.ordinal, start);
        }
    }
    starts
}

/// Copy of `text` with the properties of every scalar in `targets` turned
/// into spaces and every alias in `aliases` turned into a run of `x` — each
/// character replaced by one ASCII character, line breaks kept, so character
/// indices and line numbers match the source. `None` if two ranges overlap
/// or one is not on a character boundary, which would be a prescan bug.
fn blank_for_replay(
    text: &str,
    targets: &[PropertiedBlockScalar],
    aliases: &[Range<usize>],
) -> Option<String> {
    let mut edits: Vec<(&Range<usize>, char)> = targets
        .iter()
        .map(|target| (&target.props, ' '))
        .chain(aliases.iter().map(|alias| (alias, 'x')))
        .collect();
    edits.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (range, fill) in edits {
        out.push_str(text.get(pos..range.start)?);
        for ch in text.get(range.clone())?.chars() {
            out.push(if matches!(ch, '\n' | '\r') { ch } else { fill });
        }
        pos = range.end;
    }
    out.push_str(text.get(pos..)?);
    Some(out)
}

/// The 0.0.6 span start of one block scalar with properties: replay the
/// stream with only its properties blanked, up to its ordinal. Events before
/// the ordinal are unaffected by the blanking — an alias of the blanked
/// anchor can only come later. Fallback of
/// [`replay_block_scalar_body_starts`]; `None` if this replay diverges too.
fn replay_one_block_scalar(
    text: &str,
    char_to_byte: &[usize],
    target: &PropertiedBlockScalar,
) -> Option<usize> {
    let blanked = blank_for_replay(text, std::slice::from_ref(target), &[])?;
    let mut parser = YamlParser::new_from_str(&blanked);
    for _ in 0..target.ordinal {
        parser.next_event()?.ok()?;
    }
    match parser.next_event()?.ok()? {
        (Event::Scalar(_, ScalarStyle::Literal | ScalarStyle::Folded, _, _), span) => {
            Some(char_index_to_byte(char_to_byte, span.start.index()))
        }
        (other, _) => {
            tracing::debug!(
                ?other,
                "block-scalar span replay diverged; using the indicator position",
            );
            None
        }
    }
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
        /// `true` for an indentless block sequence — a mapping value whose
        /// `-` entries sit at the key's column (`key:` then `- item`). The
        /// 0.0.6 scanner opens no indentation level for it; see
        /// [`State::block_indent`].
        indentless: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextStyle {
    Block,
    Flow,
}

struct State<'src> {
    /// The source text; `bytes` is the same buffer viewed as bytes.
    text: &'src str,
    bytes: &'src [u8],
    /// Map from `Marker::index()` (character offset) to byte offset. See
    /// [`build_char_to_byte_map`] for the rationale.
    char_to_byte: &'src [usize],
    multi_doc: bool,
    doc_index: usize,
    seen_first_doc: bool,
    stack: Vec<Frame>,
    path: Vec<String>,
    /// Position of the next event in the parser's stream (`StreamStart` is
    /// 0). Keys [`State::block_body_starts`].
    next_ordinal: usize,
    /// 0.0.6 span start of each block scalar with node properties, keyed by
    /// event ordinal — see [`replay_block_scalar_body_starts`].
    block_body_starts: &'src HashMap<usize, usize>,
}

impl<'src> State<'src> {
    fn new(
        text: &'src str,
        multi_doc: bool,
        char_to_byte: &'src [usize],
        block_body_starts: &'src HashMap<usize, usize>,
    ) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            char_to_byte,
            multi_doc,
            doc_index: 0,
            seen_first_doc: false,
            stack: Vec::new(),
            path: Vec::new(),
            next_ordinal: 0,
            block_body_starts,
        }
    }

    fn observe(
        &mut self,
        event: Event<'_>,
        span: Span,
        spans: &mut SpanMap,
        block_scalars: &mut HashSet<String>,
    ) -> std::result::Result<(), Error> {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
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
            Event::Scalar(value, style, anchor, tag) => {
                let n_properties = usize::from(anchor != 0) + usize::from(tag.is_some());
                let is_key = matches!(
                    self.stack.last(),
                    Some(Frame::Mapping {
                        pending_key: None,
                        ..
                    })
                );
                if is_key {
                    let key_str =
                        pointer_escape(legacy_key_text(value.as_ref(), style, n_properties));
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
                    let (value_range, col) = if n_properties == 0 {
                        (span_to_range(span, self.char_to_byte), span.start.col())
                    } else {
                        let is_empty_node = style == ScalarStyle::Plain && value.is_empty();
                        self.legacy_scalar_span(span, style, is_empty_node, n_properties, ordinal)
                    };
                    let context = self.value_context();
                    let line_range = self.compute_line_range(&value_range, context, style);
                    let indent = u32::try_from(col).unwrap_or(u32::MAX);
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
            Event::SequenceStart(anchor, tag) => {
                let n_properties = usize::from(anchor != 0) + usize::from(tag.is_some());
                let start_byte = self.legacy_container_start(span, n_properties);
                let style = detect_container_style(self.bytes, start_byte);
                let indentless = style == ContextStyle::Block
                    && matches!(
                        self.stack.last(),
                        Some(Frame::Mapping {
                            style: ContextStyle::Block,
                            ..
                        })
                    )
                    && !starts_line(self.bytes, start_byte);
                self.enter_container();
                self.stack.push(Frame::Sequence {
                    index: 0,
                    style,
                    start_byte,
                    indentless,
                });
            }
            Event::SequenceEnd => {
                let end_byte = char_index_to_byte(self.char_to_byte, span.end.index());
                self.leave_container_recording_empty(spans, end_byte);
            }
            Event::MappingStart(anchor, tag) => {
                let n_properties = usize::from(anchor != 0) + usize::from(tag.is_some());
                let start_byte = self.legacy_container_start(span, n_properties);
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

    /// Byte range and 0-based character column of a scalar that carries
    /// `n_properties` (> 0) node properties, in the saphyr-parser 0.0.6 shape.
    /// See the "saphyr-parser 0.0.6 span compatibility" section below.
    fn legacy_scalar_span(
        &self,
        span: Span,
        style: ScalarStyle,
        is_empty_node: bool,
        n_properties: usize,
        ordinal: usize,
    ) -> (Range<usize>, usize) {
        let props_start = char_index_to_byte(self.char_to_byte, span.start.index());
        let end = char_index_to_byte(self.char_to_byte, span.end.index());
        let content_start = skip_node_properties(self.bytes, props_start, n_properties).1;
        let range = if is_empty_node {
            // 0.0.6 reported the span of the token that follows the
            // properties.
            return self.legacy_empty_node_span(content_start);
        } else if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded) {
            self.block_body_starts
                .get(&ordinal)
                .copied()
                .unwrap_or(content_start)..end
        } else {
            content_start..end
        };
        let col = char_column(self.text, range.start);
        (range, col)
    }

    /// Start byte of a container that carries `n_properties` node properties,
    /// in the saphyr-parser 0.0.6 shape: the start of the container's first
    /// token.
    ///
    /// Since 0.1 the event span runs from the first property to the end of
    /// that token. A flow container's first token is the indicator plus the
    /// blanks after it, so its start is the `{` / `[` itself. The block
    /// tokens (`BlockMappingStart`, `BlockSequenceStart`, and the
    /// `BlockEntry` that opens an indentless sequence) are zero-width, so the
    /// end of the span is their start — for an indentless sequence that is
    /// past the `- `, not at the `-`.
    fn legacy_container_start(&self, span: Span, n_properties: usize) -> usize {
        let start = char_index_to_byte(self.char_to_byte, span.start.index());
        if n_properties == 0 {
            return start;
        }
        let content_start = skip_node_properties(self.bytes, start, n_properties).1;
        if matches!(self.bytes.get(content_start), Some(b'{' | b'[')) {
            content_start
        } else {
            char_index_to_byte(self.char_to_byte, span.end.index())
        }
    }

    /// Byte range and 0-based character column saphyr-parser 0.0.6 reported
    /// for an empty node with properties: the span of the token that follows
    /// the properties. `pos` is where the scanner resumes after them (past
    /// separating blanks, line breaks and comments); the parent container is
    /// the top of `self.stack`.
    ///
    /// Mirrors the 0.0.6 scanner's token spans (unchanged in 0.3, which no
    /// longer exposes them):
    ///
    /// - end of input (`StreamEnd`, or a `BlockEnd` emitted there): empty at
    ///   the end, column forced to 0;
    /// - a token left of the innermost block indentation is preceded by a
    ///   `BlockEnd`, empty at `pos`;
    /// - `-` (`BlockEntry`): empty, past the `-` and the blanks / comment
    ///   that follow it on the line;
    /// - `,` `]` `}` (`FlowEntry`, flow end): the indicator plus the blanks /
    ///   comment after it — unless an implicit flow mapping (`[a: &x, …]`)
    ///   ends first, which is an empty `FlowMappingEnd` at `pos`;
    /// - `?` (`Key`): the indicator plus the whitespace, line breaks and
    ///   comments after it;
    /// - `---` / `...` after a root node: the 3-byte marker;
    /// - anything else — `:` (`Value`), an implicit key, a `BlockEnd`: empty
    ///   at `pos`.
    fn legacy_empty_node_span(&self, pos: usize) -> (Range<usize>, usize) {
        let bytes = self.bytes;
        let Some(&first) = bytes.get(pos) else {
            return (bytes.len()..bytes.len(), 0);
        };
        let followed_by_blank = matches!(
            bytes.get(pos + 1),
            None | Some(b' ' | b'\t' | b'\n' | b'\r')
        );
        let in_flow = self.stack.iter().any(|frame| {
            matches!(
                frame,
                Frame::Mapping {
                    style: ContextStyle::Flow,
                    ..
                } | Frame::Sequence {
                    style: ContextStyle::Flow,
                    ..
                }
            )
        });
        let end = match self.stack.last() {
            None => {
                let is_document_marker = line_offset(bytes, pos) == 0
                    && matches!(bytes.get(pos..pos + 3), Some(b"---" | b"..."))
                    && matches!(
                        bytes.get(pos + 3),
                        None | Some(b' ' | b'\t' | b'\n' | b'\r')
                    );
                if is_document_marker { pos + 3 } else { pos }
            }
            Some(top) if in_flow => {
                // A block-styled mapping inside a flow collection is an
                // implicit flow mapping (`[a: b]`); `,` and `]` end it first.
                let implicit_mapping = matches!(
                    top,
                    Frame::Mapping {
                        style: ContextStyle::Block,
                        ..
                    }
                );
                match first {
                    b',' | b']' if implicit_mapping => pos,
                    b',' | b']' | b'}' => skip_ws_to_eol(bytes, pos + 1),
                    b'?' => skip_yaml_whitespace(bytes, pos + 1),
                    _ => pos,
                }
            }
            Some(_) if line_offset(bytes, pos) < self.block_indent() => pos,
            Some(_) => match first {
                b'-' if followed_by_blank => {
                    // `BlockEntry` is empty, at the end of what it skips.
                    let end = skip_ws_to_eol(bytes, pos + 1);
                    return (end..end, char_column(self.text, end));
                }
                b'?' if followed_by_blank => skip_yaml_whitespace(bytes, pos + 1),
                _ => pos,
            },
        };
        (pos..end, char_column(self.text, pos))
    }

    /// Byte column of the innermost block indentation level the 0.0.6
    /// scanner tracks: a block mapping's first key, or a block sequence's
    /// first `-`. An indentless sequence (`key:` followed by `- item` at the
    /// key's column) opens no level of its own.
    fn block_indent(&self) -> usize {
        for frame in self.stack.iter().rev() {
            match frame {
                Frame::Sequence {
                    indentless: true, ..
                } => {}
                Frame::Mapping { start_byte, .. } | Frame::Sequence { start_byte, .. } => {
                    return line_offset(self.bytes, *start_byte);
                }
            }
        }
        0
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
                ..
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
// saphyr-parser 0.0.6 span compatibility
// ---------------------------------------------------------------------------
//
// The span builder was validated against saphyr-parser 0.0.6. Later releases
// changed two things the `SpanMap` depends on; the helpers below map the new
// events back to the 0.0.6 shape so spans stay byte-identical:
//
// 1. Since 0.1 a node's span starts at its first node property (`&anchor`,
//    `!tag`), not at its content. 0.0.6 started it at the content token: the
//    first non-separation byte for flow scalars and containers, the first
//    content line for block scalars. An empty node with properties
//    (`key: &a` followed by a newline) got the span of the token after the
//    properties; 0.3 gives the span of the properties themselves.
// 2. Since 0.1 an empty node (`: value`, `key:`) is the scalar `""`; 0.0.6
//    reported `~`. Only keys use the scalar text.

/// Key text in the saphyr-parser 0.0.6 shape: an empty key node without
/// properties was reported as `~` (pointer segment `~0`).
fn legacy_key_text(value: &str, style: ScalarStyle, n_properties: usize) -> &str {
    // A plain scalar token is never empty, so a plain `""` is an empty node.
    if value.is_empty() && style == ScalarStyle::Plain && n_properties == 0 {
        "~"
    } else {
        value
    }
}

/// Skip `n_properties` node properties starting at byte `pos` (the first
/// property). Returns `(end of the last property, start of the content
/// token)`; the content start is past any separating whitespace, line breaks
/// and comments.
fn skip_node_properties(bytes: &[u8], pos: usize, n_properties: usize) -> (usize, usize) {
    let mut pos = pos;
    for i in 0..n_properties {
        if i > 0 {
            pos = skip_separation(bytes, pos);
        }
        pos = skip_property(bytes, pos);
    }
    (pos, skip_separation(bytes, pos))
}

/// Skip one `&anchor`, `!tag` or `!<verbatim>` property at byte `pos`.
/// Anchor and tag characters end at whitespace, a line break or a flow
/// indicator (YAML 1.2 `ns-anchor-char` / `ns-tag-char`); a verbatim tag ends
/// after its `>`.
fn skip_property(bytes: &[u8], pos: usize) -> usize {
    let ends_property = |b: u8| {
        matches!(
            b,
            b' ' | b'\t' | b'\n' | b'\r' | b',' | b'[' | b']' | b'{' | b'}'
        )
    };
    match (bytes.get(pos), bytes.get(pos + 1)) {
        (Some(b'!'), Some(b'<')) => bytes[pos..]
            .iter()
            .position(|&b| b == b'>')
            .map_or(bytes.len(), |off| pos + off + 1),
        (Some(b'&' | b'!'), _) => {
            let mut end = pos + 1;
            while end < bytes.len() && !ends_property(bytes[end]) {
                end += 1;
            }
            end
        }
        _ => pos,
    }
}

/// Skip whitespace, line breaks and comments starting at byte `pos`.
fn skip_separation(bytes: &[u8], pos: usize) -> usize {
    let mut pos = pos;
    while let Some(&b) = bytes.get(pos) {
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => pos += 1,
            b'#' => {
                while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
                    pos += 1;
                }
            }
            _ => break,
        }
    }
    pos
}

/// Skip the blanks and the comment that follow a token on its line, the way
/// the 0.0.6 scanner's `skip_ws_to_eol` does: spaces and tabs, then a `#`
/// comment if blanks preceded it. Stops before the line break.
fn skip_ws_to_eol(bytes: &[u8], pos: usize) -> usize {
    let mut pos = pos;
    let mut saw_blank = false;
    while let Some(&b) = bytes.get(pos) {
        match b {
            b' ' | b'\t' => {
                saw_blank = true;
                pos += 1;
            }
            b'#' if saw_blank => {
                while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
                    pos += 1;
                }
            }
            _ => break,
        }
    }
    pos
}

/// Skip spaces, line breaks and comments the way the 0.0.6 scanner's
/// `skip_yaml_whitespace` does after a `?` indicator. Tabs end the skip.
fn skip_yaml_whitespace(bytes: &[u8], pos: usize) -> usize {
    let mut pos = pos;
    while let Some(&b) = bytes.get(pos) {
        match b {
            b' ' | b'\n' | b'\r' => pos += 1,
            b'#' => {
                while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
                    pos += 1;
                }
            }
            _ => break,
        }
    }
    pos
}

/// `true` if only spaces and tabs precede byte `pos` on its line.
fn starts_line(bytes: &[u8], pos: usize) -> bool {
    let pos = pos.min(bytes.len());
    bytes[..pos]
        .iter()
        .rev()
        .take_while(|&&b| !matches!(b, b'\n' | b'\r'))
        .all(|&b| matches!(b, b' ' | b'\t'))
}

/// Byte offset of `pos` from the start of its line.
fn line_offset(bytes: &[u8], pos: usize) -> usize {
    let pos = pos.min(bytes.len());
    let line_start = bytes[..pos]
        .iter()
        .rposition(|&b| matches!(b, b'\n' | b'\r'))
        .map_or(0, |i| i + 1);
    pos - line_start
}

/// 0-based character column of byte `pos`, counted the way saphyr-parser
/// counts `Marker::col()`: a byte-order mark opening the stream is not a
/// column.
fn char_column(text: &str, pos: usize) -> usize {
    let pos = pos.min(text.len());
    let mut line_start = text.as_bytes()[..pos]
        .iter()
        .rposition(|&b| matches!(b, b'\n' | b'\r'))
        .map_or(0, |i| i + 1);
    if line_start == 0 && text.starts_with('\u{feff}') {
        line_start = '\u{feff}'.len_utf8();
    }
    text.get(line_start..pos)
        .map_or(0, |line| line.chars().count())
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

    // -- block scalars with node properties: one replay pass ------------

    #[test]
    fn propertied_block_scalars_span_from_first_content_line() {
        // The alias sits last: an alias value leaves the pending key in
        // place, so anything after it in the mapping would shift.
        let bytes = b"a: &x |\n  body\n  two\nb: !t >-\n  fold\nc: !!str &y |2\n   ind\nd: *x\n";
        let (_, spans) = parse_with_spans(bytes).expect("parse");
        assert_span(&spans, "/a", bytes, b"body\n  two\n");
        assert_span(&spans, "/b", bytes, b"fold\n");
        assert_span(&spans, "/c", bytes, b" ind\n");
    }

    #[test]
    fn propertied_block_scalar_replay_is_a_single_pass() {
        // Regression guard for the per-scalar replay, which re-parsed the
        // stream once per block scalar with properties (O(N * file size)).
        let mut src = String::new();
        for i in 0..500 {
            src.push_str(&format!(
                "k{i}: &a{i} |\n  body {i}\nt{i}: !Sub >-\n  fold {i}\n"
            ));
        }
        src.push_str("last: *a0\n");
        let built = build_span_map_with_stats(src.as_bytes()).expect("parse");
        assert_eq!(
            built.parser_passes, 3,
            "prescan + one combined replay + main walk"
        );
        assert_span(&built.spans, "/k0", src.as_bytes(), b"body 0\n");
        assert_span(&built.spans, "/t499", src.as_bytes(), b"fold 499\n");
    }

    #[test]
    fn span_build_without_propertied_block_scalars_skips_the_replay() {
        let bytes = b"a: &x 1\nb: |\n  body\nc: *x\n";
        let built = build_span_map_with_stats(bytes).expect("parse");
        assert_eq!(built.parser_passes, 2, "prescan + main walk only");
    }

    #[test]
    fn unconfirmed_block_scalar_falls_back_to_a_solo_replay() {
        let text = "a: &x |\n  body\nb: !t |\n  two\nc: *x\n";
        let char_to_byte = build_char_to_byte_map(text);
        let mut prescan = prescan(text, text.as_bytes(), &char_to_byte).expect("prescan");
        assert_eq!(prescan.propertied_block_scalars.len(), 2);
        let mut passes = 0;
        let confirmed = replay_block_scalar_body_starts(text, &char_to_byte, &prescan, &mut passes);
        assert_eq!(passes, 1);
        // A wrong span end makes the combined replay reject the first
        // scalar; the solo replay must still find the same start.
        prescan.propertied_block_scalars[0].end_char += 1;
        let mut passes = 0;
        let with_fallback =
            replay_block_scalar_body_starts(text, &char_to_byte, &prescan, &mut passes);
        assert_eq!(passes, 2, "combined replay + one solo replay");
        assert_eq!(with_fallback, confirmed);
        let first = prescan.propertied_block_scalars[0].ordinal;
        assert_eq!(
            with_fallback.get(&first),
            Some(&text.find("body").expect("body"))
        );
    }

    #[test]
    fn blank_for_replay_keeps_character_positions() {
        let text = "k: &аб |\n  é\nr: *аб\n";
        let char_to_byte = build_char_to_byte_map(text);
        let prescan = prescan(text, text.as_bytes(), &char_to_byte).expect("prescan");
        let blanked = blank_for_replay(text, &prescan.propertied_block_scalars, &prescan.aliases)
            .expect("ranges are disjoint");
        assert_eq!(blanked, "k:     |\n  é\nr: xxx\n");
        assert_eq!(blanked.chars().count(), text.chars().count());
    }
}
