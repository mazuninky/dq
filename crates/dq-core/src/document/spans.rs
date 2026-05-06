//! Span model used by the textual-edit write path (M2).
//!
//! A [`ValueSpan`] records *where in the original source* a `Value` lives —
//! its byte range, its enclosing line range (used by `del_at` to remove the
//! whole physical line), the indentation of its parent container, and the
//! syntactic context (block-vs-flow, mapping-vs-sequence) which the renderer
//! needs to decide quote style and separators when replacing the value.
//!
//! Spans are stored in a [`SpanMap`] keyed by the canonical RFC 6901 pointer
//! string (e.g. `/spec/replicas`). Lookup is therefore `O(1)` for the common
//! case where the user has just parsed a path. Keys are the same ones produced
//! by [`crate::Pointer::as_canonical`] — using a foreign convention here
//! would silently miss spans on every write.
//!
//! # Recompute on edit
//!
//! After a textual edit replaces bytes `[at, at+old_len)` with bytes of length
//! `new_len`, every span whose `value_range.start >= at + old_len` shifts by
//! `new_len - old_len`. Spans wholly to the left of the edit, or that overlap
//! the edit, are NOT shifted: the overlapped span is the one being replaced
//! and its caller updates it directly; spans to the left were not affected by
//! the splice. See [`apply_delta`] for the implementation.

use std::ops::Range;

use indexmap::IndexMap;

/// Where a [`crate::Value`] lives in the original source bytes.
///
/// `value_range` is the byte range of the value's textual representation
/// **including any surrounding quotes** (so `"hello"` covers the quotes too).
/// The saphyr-parser spike confirmed this: events report the quoted span as a
/// single unit, not the inner string. The renderer takes that into account
/// when reproducing or replacing the scalar.
///
/// `line_range` is the byte range of the entire physical line(s) the value
/// occupies — `del_at` uses this to remove the trailing newline along with
/// the value. For block-style values that span multiple lines, the range
/// covers all of them.
///
/// `indent` is the leading-whitespace count of the parent container. New
/// inserted siblings inherit this so output formatting matches the source.
///
/// `context` distinguishes block-vs-flow and mapping-vs-sequence, which
/// affects rendering: e.g. a flow-style sequence item needs `, ` separation
/// where a block-style sequence item needs `\n  - ` indentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueSpan {
    /// Byte range of the value's textual form (inclusive of quotes).
    pub value_range: Range<usize>,
    /// Byte range of the whole physical line(s) the value occupies.
    pub line_range: Range<usize>,
    /// Indentation (in source bytes) of the parent container.
    pub indent: u32,
    /// Syntactic context — affects renderer choices for quote style and
    /// separators.
    pub context: SpanContext,
}

/// Syntactic context of a span — block-vs-flow and mapping-vs-sequence.
///
/// The saphyr-parser 0.0.6 events do not carry an explicit block-vs-flow
/// flag, but the spike confirmed it can be derived from event sequences and
/// preceding source bytes. We materialize the result here so the renderer
/// does not re-derive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanContext {
    /// Value of a block-style mapping (`key: value` on its own line).
    BlockMapValue,
    /// Item of a block-style sequence (`- value` on its own line).
    BlockSeqItem,
    /// Value of a flow-style mapping (`{key: value, ...}`).
    FlowMapValue,
    /// Item of a flow-style sequence (`[value, ...]`).
    FlowSeqItem,
}

/// Map from canonical RFC 6901 pointer strings to spans.
///
/// `IndexMap` (rather than `HashMap`) keeps the insertion order stable so
/// debug output and snapshots stay deterministic. The order of spans does
/// not affect correctness — `apply_delta` does an unordered scan — but it
/// makes the data easier to inspect.
pub type SpanMap = IndexMap<String, ValueSpan>;

/// A textual edit's effect on byte positions.
///
/// `at` is the byte offset where the edit starts; `old_len` is the number
/// of bytes being replaced; `new_len` is the number of bytes replacing them.
/// The span shift is `new_len - old_len` (signed), applied to every span that
/// starts *after* the edit.
#[derive(Debug, Clone, Copy)]
pub struct SpanRecomputeDelta {
    /// Byte offset where the edit begins.
    pub at: usize,
    /// Length of the byte range being replaced.
    pub old_len: usize,
    /// Length of the bytes being inserted in its place.
    pub new_len: usize,
}

/// Apply `delta` to every span in `map` whose start is to the right of the
/// edited region.
///
/// Spans that overlap or precede the edit are not modified — the caller is
/// responsible for updating the span being replaced (and for the rare case
/// of an edit that overlaps multiple spans, which the M2 baseline does not
/// produce).
pub fn apply_delta(map: &mut SpanMap, delta: SpanRecomputeDelta) {
    let shift = delta.new_len as isize - delta.old_len as isize;
    if shift == 0 {
        return;
    }
    let edit_end = delta.at + delta.old_len;
    for (_, span) in map.iter_mut() {
        if span.value_range.start >= edit_end {
            span.value_range.start = signed_shift(span.value_range.start, shift);
            span.value_range.end = signed_shift(span.value_range.end, shift);
            span.line_range.start = signed_shift(span.line_range.start, shift);
            span.line_range.end = signed_shift(span.line_range.end, shift);
        }
    }
}

/// Apply a signed shift to a `usize` byte offset.
///
/// Negative shifts that would underflow are clamped at zero — this can only
/// happen when the input map already encoded an inconsistent state, but the
/// clamp keeps `apply_delta` panic-free regardless of caller bugs.
fn signed_shift(value: usize, shift: isize) -> usize {
    if shift >= 0 {
        value.saturating_add(shift as usize)
    } else {
        value.saturating_sub(shift.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> ValueSpan {
        ValueSpan {
            value_range: start..end,
            line_range: start..end,
            indent: 0,
            context: SpanContext::BlockMapValue,
        }
    }

    #[test]
    fn apply_delta_zero_shift_is_noop() {
        // When `new_len == old_len` the shift is zero and no span moves.
        // Verify byte-equality after the call to catch any accidental writes.
        let mut map = SpanMap::new();
        map.insert("/a".into(), span(0, 5));
        map.insert("/b".into(), span(10, 20));
        let snapshot = map.clone();
        apply_delta(
            &mut map,
            SpanRecomputeDelta {
                at: 5,
                old_len: 3,
                new_len: 3,
            },
        );
        assert_eq!(map, snapshot, "zero shift must not mutate any span");
    }

    #[test]
    fn apply_delta_positive_shift_moves_right_spans_right() {
        // Edit at 5..8 (old_len=3) replaced by 6 bytes (new_len=6).
        // shift = +3. Spans starting at >= 8 shift by +3.
        let mut map = SpanMap::new();
        map.insert("/before".into(), span(0, 4)); // ends at 4 < 8: untouched
        map.insert("/edit".into(), span(5, 8)); // overlaps edit: untouched (caller updates)
        map.insert("/after".into(), span(10, 20)); // starts at 10 >= 8: shifts +3
        apply_delta(
            &mut map,
            SpanRecomputeDelta {
                at: 5,
                old_len: 3,
                new_len: 6,
            },
        );
        assert_eq!(map.get("/before").unwrap().value_range, 0..4);
        assert_eq!(map.get("/edit").unwrap().value_range, 5..8);
        assert_eq!(map.get("/after").unwrap().value_range, 13..23);
        // `line_range` shifts in lockstep.
        assert_eq!(map.get("/after").unwrap().line_range, 13..23);
    }

    #[test]
    fn apply_delta_negative_shift_moves_right_spans_left() {
        // Edit at 5..15 (old_len=10) replaced by 4 bytes (new_len=4).
        // shift = -6. Span at 20..25 shifts to 14..19.
        let mut map = SpanMap::new();
        map.insert("/before".into(), span(0, 4));
        map.insert("/after".into(), span(20, 25));
        apply_delta(
            &mut map,
            SpanRecomputeDelta {
                at: 5,
                old_len: 10,
                new_len: 4,
            },
        );
        assert_eq!(map.get("/before").unwrap().value_range, 0..4);
        assert_eq!(map.get("/after").unwrap().value_range, 14..19);
    }

    #[test]
    fn apply_delta_leaves_left_and_overlapping_spans_alone() {
        // Three spans: one wholly to the left, one overlapping the edit, one
        // starting *exactly at* the edit start. Only spans whose start is
        // >= at + old_len shift; here the overlapping span and the
        // exactly-at-start span both stay put.
        let mut map = SpanMap::new();
        map.insert("/left".into(), span(0, 3));
        map.insert("/overlapping".into(), span(8, 14)); // crosses 10..12
        map.insert("/at_start".into(), span(10, 12)); // starts exactly at edit
        apply_delta(
            &mut map,
            SpanRecomputeDelta {
                at: 10,
                old_len: 2,
                new_len: 5,
            },
        );
        assert_eq!(map.get("/left").unwrap().value_range, 0..3);
        assert_eq!(map.get("/overlapping").unwrap().value_range, 8..14);
        assert_eq!(map.get("/at_start").unwrap().value_range, 10..12);
    }
}
