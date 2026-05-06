//! Intermediate Representation (IR) layer — a borrowed view of a parsed
//! [`crate::Document`] paired with a *provenance* side-channel.
//!
//! # Why an IR layer
//!
//! `dq` carries three parallel value representations in flight: `dq_core::Value`
//! (this crate), `serde_json::Value` (jq adapter input), and `jaq_json::Val`
//! (jaq runtime). Lint diagnostics historically lost the parser-provided byte
//! spans on the way through that pipeline, defaulting `line` / `col` to `1`
//! and forcing rule authors to recover positions via a `loc.line` jq override
//! ([`crates/dq-exec/src/evaluator.rs`](../../../dq-exec/src/evaluator.rs)).
//!
//! The [`Ir`] type pairs an existing `&Value` with a [`ProvenanceMap`] keyed
//! by canonical RFC 6901 pointer strings. Each entry says either "this node
//! corresponds to a pointer in the source, here is its [`ValueSpan`]" or
//! "this node was synthesized by a transformation". Phase 1 of the
//! `add-ir-foundation` change introduces only the types and lookup helpers;
//! Phase 2+ wires them into the lint pipeline so diagnostics can resolve
//! exact line/col through `Ir::span_for(pointer)` instead of jq-side
//! workarounds.
//!
//! # Design notes
//!
//! - `Ir<'a>` is `Copy` — three borrowed fields, all of which are themselves
//!   `Copy` (`&Value`, `&ProvenanceMap`, [`FormatTag`]). Callers can pass it
//!   into nested helpers without explicit `&` re-borrows.
//! - [`OwnedIr`] mirrors `Ir<'a>` for cases where ownership of the triple is
//!   required (e.g. the result of a transformation that allocated a new
//!   `Value`). It exposes [`OwnedIr::to_borrowed`] for the common case where
//!   the immediate next step needs a borrowed view.
//! - [`Provenance`] separates the source-of-truth pointer (kept inside
//!   `Original { pointer, .. }` as a structured [`Pointer`]) from the map
//!   key (a canonical string). The redundancy is deliberate: callers
//!   inspecting [`ProvenanceMap`] entries get a typed `Pointer` back without
//!   having to re-parse the key.
//! - Provenance is a *side-channel*, not a field of every `Value` node. A
//!   transformation that does not touch `Value` — e.g. `select(.foo)` — does
//!   not have to re-allocate the value tree just to update inner per-node
//!   pointers. This mirrors how [`crate::document::SpanMap`] is laid out.
//!
//! # Divergence from the spec text
//!
//! The `data-query-ir` spec spells the provenance map type as
//! `HashMap<Pointer, Provenance>`. The actual implementation uses
//! `IndexMap<String, Provenance>` keyed by the same canonical RFC 6901 form
//! produced by [`Pointer::as_canonical`]. Three reasons:
//!
//! 1. [`crate::document::SpanMap`] already uses
//!    `IndexMap<String, ValueSpan>`. Mirroring its shape lets us derive a
//!    `ProvenanceMap` from a `SpanMap` with a single ordered iteration —
//!    no rehashing, no key-type plumbing.
//! 2. `IndexMap` keeps insertion order stable, which makes debug output and
//!    snapshot-test diffs deterministic.
//! 3. Lookup keys are always recomputed from a `Pointer` via
//!    `pointer.as_canonical()` — exactly the same path
//!    [`crate::Document::span_at`] takes against `SpanMap`. The map's key
//!    type does not leak into the public API; callers always pass `&Pointer`.
//!
//! [`Pointer::as_canonical`]: crate::pointer::Pointer::as_canonical

use indexmap::IndexMap;

use crate::document::spans::ValueSpan;
use crate::document::{FormatTag, Value};
use crate::pointer::Pointer;

/// Map from canonical RFC 6901 pointer strings to [`Provenance`] entries.
///
/// Keyed by the same canonical form as
/// [`crate::document::SpanMap`] — see the module-level rationale on the
/// divergence from the `HashMap<Pointer, Provenance>` shape spelled in the
/// `data-query-ir` spec text.
///
/// `IndexMap` (rather than `HashMap`) keeps insertion order stable so that
/// debug output and snapshot tests are deterministic; lookup is still
/// `O(1)` average for the hash-based fast path used by
/// [`Ir::provenance_for`] and [`Ir::span_for`].
pub type ProvenanceMap = IndexMap<String, Provenance>;

/// The reason a node was synthesized by a transformation rather than copied
/// from the source.
///
/// The set is intentionally small and closed: it is the **complete** list of
/// `Synthetic` reasons recognised across the IR pipeline. A transformation
/// that cannot pin itself to one of these reasons should default to
/// [`SyntheticReason::Computed`] — that is the explicit "I do not know"
/// signal callers use to suppress span lookup for the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticReason {
    /// Node literal-constructed by an expression (e.g. `{a: 1}` in jq, a
    /// hard-coded value in a fix script). No correspondence to any input
    /// pointer.
    Constructed,
    /// Node produced by an aggregation over multiple input nodes (e.g.
    /// `length`, `add`, `group_by`). Span lookup is meaningless because the
    /// result is not co-located with any single source byte range.
    Aggregated,
    /// Node produced by an arithmetic or other computation whose
    /// input-output pointer correspondence is not statically determinable
    /// (e.g. `.x + .y`, `.foo | tostring`).
    Computed,
}

/// Provenance of a single value node.
///
/// `Original` carries the source pointer (always present so callers can
/// route back into the document) plus an optional [`ValueSpan`]. The span
/// is `None` for read-only formats whose parser does not record byte
/// ranges (jsonl, hcl, ini, dotenv, csv, tsv, dockerfile, ignore-list,
/// markdown body) — the pointer is still meaningful for navigation, only
/// position-aware lookup is unavailable.
///
/// `Synthetic` is emitted by a transformation that produced the node from
/// thin air or from an aggregation; see [`SyntheticReason`] for the closed
/// set of reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// Node corresponds to a pointer in the source document.
    Original {
        /// Source pointer, identical to the canonical key under which this
        /// entry is stored. Held as a structured [`Pointer`] so callers do
        /// not have to re-parse the map key.
        pointer: Pointer,
        /// Source byte range, when the parser populated a [`ValueSpan`].
        /// `None` for read-only formats whose parser carries no span map.
        span: Option<ValueSpan>,
    },
    /// Node produced by a transformation; no source pointer applies.
    Synthetic {
        /// Why the transformation could not preserve provenance for this
        /// node.
        reason: SyntheticReason,
    },
}

/// Borrowed view of a parsed document plus its provenance side-channel.
///
/// `Ir<'a>` is the read-only IR shape: four borrowed fields, no owned
/// data. The type is `Copy` so callers can pass it into helpers without
/// re-borrowing — useful in chained access patterns where every step takes
/// `&Ir<'_>` as input.
///
/// Construct one via [`crate::Document::as_ir`]. Direct construction from a
/// raw triple is intentionally not exposed — the IR's invariant is "the
/// provenance map keys agree with the canonical form of pointers reachable
/// in `value`", and only the document constructors guarantee that today.
///
/// # Source bytes
///
/// Phase 2 of `add-ir-foundation` added a `bytes` field — a borrowed slice
/// of the parser's `original_bytes`. It powers [`Ir::line_col_for`], the
/// helper that turns a `ValueSpan`'s byte range into a 1-indexed
/// `(line, col)` for diagnostic emission. Synthesised IRs (the
/// `Phase 2 jq adapter`-produced [`OwnedIr`], for example) have no source
/// bytes — they construct an [`Ir`] via [`Ir::new`], which leaves the
/// `bytes` field empty, and [`Ir::line_col_for`] correctly returns `None`.
#[derive(Debug, Clone, Copy)]
pub struct Ir<'a> {
    value: &'a Value,
    provenance: &'a ProvenanceMap,
    format: FormatTag,
    /// Borrowed source bytes — the parser's `original_bytes` for write-aware
    /// documents, an empty slice otherwise. See [`Ir::line_col_for`].
    bytes: &'a [u8],
}

impl<'a> Ir<'a> {
    /// Construct an `Ir<'a>` from already-aligned parts (no source bytes).
    ///
    /// Used internally by transformations that produce a fresh
    /// `(value, provenance, format)` triple but have no parser-provided
    /// `original_bytes` to attach (Phase 2 jq adapter, future fix-op
    /// application). The resulting [`Ir`]'s `bytes` slice is empty, so
    /// [`Ir::line_col_for`] always returns `None`.
    ///
    /// For read paths against a parsed [`crate::Document`] the IR is
    /// produced by [`crate::Document::as_ir`], which routes through
    /// [`Ir::with_bytes`] so [`Ir::line_col_for`] can resolve to a real
    /// source position.
    #[must_use]
    pub fn new(value: &'a Value, provenance: &'a ProvenanceMap, format: FormatTag) -> Self {
        Self::with_bytes(value, provenance, format, &[])
    }

    /// Construct an `Ir<'a>` carrying a borrowed slice of the parser's
    /// source bytes alongside the value/provenance/format triple.
    ///
    /// The `bytes` slice is the byte buffer the parser was handed (or an
    /// empty slice when none is available). [`Ir::line_col_for`] reads
    /// `bytes` to derive a 1-indexed `(line, col)` from a `ValueSpan`'s
    /// byte range.
    #[must_use]
    pub fn with_bytes(
        value: &'a Value,
        provenance: &'a ProvenanceMap,
        format: FormatTag,
        bytes: &'a [u8],
    ) -> Self {
        Self {
            value,
            provenance,
            format,
            bytes,
        }
    }

    /// Borrow the underlying [`Value`] tree.
    #[must_use]
    pub fn value(&self) -> &'a Value {
        self.value
    }

    /// Borrow the provenance map.
    #[must_use]
    pub fn provenance(&self) -> &'a ProvenanceMap {
        self.provenance
    }

    /// Format tag carried alongside the IR. Read-only formats whose
    /// parsers emit empty provenance still propagate the tag here so
    /// callers can distinguish "no provenance available" from "this is not
    /// the format you think it is".
    #[must_use]
    pub fn format(&self) -> FormatTag {
        self.format
    }

    /// Borrowed source bytes, when the IR was constructed via
    /// [`Ir::with_bytes`] (read paths through [`crate::Document::as_ir`]).
    ///
    /// Returns an empty slice for IRs produced by [`Ir::new`] — the
    /// "synthesised, no source" path. Callers wanting a real
    /// `(line, col)` should prefer [`Ir::line_col_for`], which already
    /// handles the empty case by returning `None`.
    #[must_use]
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Look up the [`Provenance`] entry for `pointer`'s canonical RFC 6901
    /// form.
    ///
    /// Returns `None` for unmapped pointers. `O(1)` average — backed by a
    /// hash lookup against [`ProvenanceMap`].
    #[must_use]
    pub fn provenance_for(&self, pointer: &Pointer) -> Option<&'a Provenance> {
        self.provenance.get(&pointer.as_canonical())
    }

    /// Look up the [`ValueSpan`] for `pointer`'s canonical RFC 6901 form.
    ///
    /// Returns `None` in three cases:
    ///
    /// - The pointer has no entry in the provenance map (unknown pointer).
    /// - The entry is [`Provenance::Synthetic`] — a synthesized node has
    ///   no source span.
    /// - The entry is [`Provenance::Original`] but its `span` field is
    ///   `None` (read-only formats whose parsers do not produce a span
    ///   map).
    ///
    /// `O(1)` average — same lookup path as [`Ir::provenance_for`].
    #[must_use]
    pub fn span_for(&self, pointer: &Pointer) -> Option<&'a ValueSpan> {
        match self.provenance_for(pointer)? {
            Provenance::Original { span, .. } => span.as_ref(),
            Provenance::Synthetic { .. } => None,
        }
    }

    /// Resolve `pointer` to a 1-indexed `(line, col)` position in the
    /// source bytes.
    ///
    /// Returns `None` when:
    ///
    /// - The pointer has no [`ValueSpan`] (same conditions as
    ///   [`Ir::span_for`]).
    /// - The IR was constructed without source bytes (synthesised IRs
    ///   produced via [`Ir::new`], including
    ///   [`OwnedIr::to_borrowed`]) — there is no buffer to count newlines
    ///   in.
    ///
    /// On success, returns the position of `span.value_range.start` —
    /// `line` is `1 + count of '\n' bytes before the offset`, `col` is
    /// `1 + bytes since the previous newline`. Behaviour matches
    /// `crates/dq-core/src/parsers/json.rs::derive_line_col` byte-for-byte.
    #[must_use]
    pub fn line_col_for(&self, pointer: &Pointer) -> Option<(u32, u32)> {
        let span = self.span_for(pointer)?;
        if self.bytes.is_empty() {
            return None;
        }
        Some(derive_line_col(self.bytes, span.value_range.start))
    }
}

/// Derive a 1-indexed `(line, col)` for byte offset `idx` in `bytes`.
///
/// Behaviour mirrors `crates/dq-core/src/parsers/json.rs::derive_line_col`
/// byte-for-byte; the helper is duplicated here rather than exposed from
/// the JSON parser because it is the load-bearing primitive for the IR
/// layer's diagnostic-position chain (Phase 2 of `add-ir-foundation`),
/// independent of any one parser.
#[must_use]
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

/// Owned IR: a `(Value, ProvenanceMap, FormatTag)` triple that holds the
/// data outright.
///
/// Used by transformations that produce a fresh `Value` tree (Phase 2+
/// jq adapter, future fix-op application). For the read-only "borrow from
/// a parsed `Document`" case use [`crate::Document::as_ir`] instead.
///
/// `Eq` is intentionally not implemented: [`Value`] holds `f64` for
/// the `Float` variant, which is `PartialEq` only (NaN is not reflexive).
/// [`Document`] makes the same trade-off.
///
/// [`Document`]: crate::Document
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedIr {
    value: Value,
    provenance: ProvenanceMap,
    format: FormatTag,
}

impl OwnedIr {
    /// Construct an `OwnedIr` from its three parts.
    #[must_use]
    pub fn new(value: Value, provenance: ProvenanceMap, format: FormatTag) -> Self {
        Self {
            value,
            provenance,
            format,
        }
    }

    /// Borrow this `OwnedIr` as an [`Ir<'_>`].
    ///
    /// Cheap — no allocations, no clones. The returned `Ir<'_>` borrows
    /// from `self`, so its lifetime is tied to this `OwnedIr`.
    #[must_use]
    pub fn to_borrowed(&self) -> Ir<'_> {
        Ir::new(&self.value, &self.provenance, self.format)
    }

    /// Decompose into the constituent triple.
    ///
    /// The resulting tuple round-trips through
    /// [`OwnedIr::new`] / `From<OwnedIr>` — `OwnedIr::new(v, p, f) ==
    /// OwnedIr::new` of the values returned here.
    #[must_use]
    pub fn into_parts(self) -> (Value, ProvenanceMap, FormatTag) {
        (self.value, self.provenance, self.format)
    }
}

impl From<OwnedIr> for (Value, ProvenanceMap, FormatTag) {
    fn from(owned: OwnedIr) -> Self {
        owned.into_parts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::spans::SpanContext;

    /// Compile-time check that `Ir<'_>` is `Copy`. This is part of the
    /// `data-query-ir` capability contract — the Phase 1 spec scenarios
    /// pin it explicitly.
    fn assert_copy<T: Copy>(_: T) {}

    fn sample_value() -> Value {
        Value::Bool(true)
    }

    fn sample_span() -> ValueSpan {
        ValueSpan {
            value_range: 0..4,
            line_range: 0..5,
            indent: 0,
            context: SpanContext::BlockMapValue,
        }
    }

    #[test]
    fn ir_is_copy() {
        let value = sample_value();
        let provenance = ProvenanceMap::new();
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        // Move semantics on a `Copy` type compile to a bitwise copy; passing
        // through a `T: Copy` bound proves the trait is implemented.
        assert_copy(ir);
        // The original binding is still usable post-"move" — because `Copy`.
        let _again = ir;
        let _and_again = ir;
    }

    #[test]
    fn provenance_for_returns_entry() {
        let value = sample_value();
        let mut provenance = ProvenanceMap::new();
        let pointer = Pointer::parse("/name").expect("/name parses");
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Original {
                pointer: pointer.clone(),
                span: Some(sample_span()),
            },
        );
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        match ir.provenance_for(&pointer) {
            Some(Provenance::Original { pointer: p, span }) => {
                assert_eq!(p, &pointer);
                assert!(span.is_some());
            }
            other => panic!("expected Original with span, got: {other:?}"),
        }
    }

    #[test]
    fn span_for_returns_some_for_original_with_span() {
        let value = sample_value();
        let mut provenance = ProvenanceMap::new();
        let pointer = Pointer::parse("/foo").expect("/foo parses");
        let span = sample_span();
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Original {
                pointer: pointer.clone(),
                span: Some(span.clone()),
            },
        );
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        assert_eq!(ir.span_for(&pointer), Some(&span));
    }

    #[test]
    fn span_for_returns_none_for_synthetic() {
        let value = sample_value();
        let mut provenance = ProvenanceMap::new();
        let pointer = Pointer::parse("/foo").expect("/foo parses");
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Synthetic {
                reason: SyntheticReason::Computed,
            },
        );
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        assert_eq!(ir.span_for(&pointer), None);
    }

    #[test]
    fn span_for_returns_none_for_original_without_span() {
        // Read-only formats (jsonl/hcl/ini/...) emit `Original { pointer,
        // span: None }`. The pointer is meaningful for navigation but
        // span-aware lookup must report `None` so callers fall back to
        // file-level diagnostics.
        let value = sample_value();
        let mut provenance = ProvenanceMap::new();
        let pointer = Pointer::parse("/foo").expect("/foo parses");
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Original {
                pointer: pointer.clone(),
                span: None,
            },
        );
        let ir = Ir::new(&value, &provenance, FormatTag::Hcl);
        assert_eq!(ir.span_for(&pointer), None);
    }

    #[test]
    fn span_for_returns_none_for_unmapped_pointer() {
        let value = sample_value();
        let provenance = ProvenanceMap::new();
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        let pointer = Pointer::parse("/missing").expect("/missing parses");
        assert!(ir.span_for(&pointer).is_none());
        assert!(ir.provenance_for(&pointer).is_none());
    }

    #[test]
    fn owned_ir_into_parts_round_trips() {
        let value = Value::Int(42);
        let mut provenance = ProvenanceMap::new();
        let pointer = Pointer::parse("/answer").expect("/answer parses");
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Original {
                pointer,
                span: Some(sample_span()),
            },
        );
        let format = FormatTag::Json;
        let original = OwnedIr::new(value.clone(), provenance.clone(), format);
        let (v, p, f) = original.into_parts();
        assert_eq!(v, value);
        assert_eq!(p, provenance);
        assert_eq!(f, format);
        // `From<OwnedIr> for (..)` exposes the same conversion.
        let again = OwnedIr::new(v, p, f);
        let triple: (Value, ProvenanceMap, FormatTag) = again.into();
        assert_eq!(triple.0, value);
    }

    #[test]
    fn owned_ir_to_borrowed_yields_same_data() {
        let value = Value::String("hello".into());
        let provenance = ProvenanceMap::new();
        let format = FormatTag::Yaml;
        let owned = OwnedIr::new(value.clone(), provenance, format);
        let borrowed = owned.to_borrowed();
        assert_eq!(borrowed.value(), &value);
        assert_eq!(borrowed.format(), format);
        assert!(borrowed.provenance().is_empty());
    }

    #[test]
    fn synthetic_reason_variants_are_distinct() {
        // Pin the closed-set contract: only these three variants exist.
        // A future PR that adds a fourth must update this test alongside
        // the rustdoc.
        assert_ne!(SyntheticReason::Constructed, SyntheticReason::Aggregated);
        assert_ne!(SyntheticReason::Aggregated, SyntheticReason::Computed);
        assert_ne!(SyntheticReason::Constructed, SyntheticReason::Computed);
    }

    // -- Spec-pinned `provenance_for` lookups -----------------------------
    //
    // The spec ("Provenance lookup helpers") demands `provenance_for`
    // returns the underlying `&Provenance` for both `Original` and
    // `Synthetic`, and `None` for unmapped pointers — three distinct test
    // cases. The pre-existing `provenance_for_returns_entry` covers only
    // `Original`; the two cases below pin the remaining branches so a
    // future regression that, say, accidentally folded `Synthetic` into a
    // `None` result cannot land silently.

    #[test]
    fn provenance_for_returns_synthetic_entry() {
        let value = sample_value();
        let mut provenance = ProvenanceMap::new();
        let pointer = Pointer::parse("/computed").expect("/computed parses");
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Synthetic {
                reason: SyntheticReason::Aggregated,
            },
        );
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        match ir.provenance_for(&pointer) {
            Some(Provenance::Synthetic { reason }) => {
                assert_eq!(*reason, SyntheticReason::Aggregated);
            }
            other => panic!("expected Synthetic{{Aggregated}}, got: {other:?}"),
        }
    }

    #[test]
    fn provenance_for_returns_none_for_unmapped() {
        // Distinct from `span_for_returns_none_for_unmapped_pointer` — that
        // test exercises `span_for` (which wraps `provenance_for`); this one
        // pins the underlying lookup helper independently so callers that
        // use `provenance_for` directly don't accidentally start receiving
        // empty `Synthetic` entries.
        let value = sample_value();
        let provenance = ProvenanceMap::new();
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        let pointer = Pointer::parse("/missing").expect("/missing parses");
        assert!(ir.provenance_for(&pointer).is_none());
    }

    // -- Synthetic reason coverage for `span_for` -------------------------
    //
    // The "Synthetic reasons" implicit contract requires every
    // `SyntheticReason` variant to suppress span lookup. A future
    // refactor that introduces e.g. `Synthetic::Aggregated { recovered_span }`
    // must explicitly update this test.

    #[test]
    fn span_for_returns_none_for_every_synthetic_reason() {
        let value = sample_value();
        let pointer = Pointer::parse("/n").expect("/n parses");
        for reason in [
            SyntheticReason::Constructed,
            SyntheticReason::Aggregated,
            SyntheticReason::Computed,
        ] {
            let mut provenance = ProvenanceMap::new();
            provenance.insert(pointer.as_canonical(), Provenance::Synthetic { reason });
            let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
            assert!(
                ir.span_for(&pointer).is_none(),
                "span_for must return None for Synthetic{{{reason:?}}}; \
                 a span on a synthesized node has no source meaning",
            );
        }
    }

    // -- OwnedIr round-trip with each SyntheticReason ---------------------
    //
    // `OwnedIr::into_parts` round-trip is already covered for
    // `Original`-only maps. Pin the orthogonal axis: ownership round-trip
    // also works when the provenance map carries each `Synthetic` variant.
    // A regression where `OwnedIr` accidentally dropped synthetic entries
    // (because, say, a future refactor only walked `Original` for
    // serialisation) would surface here.

    #[test]
    fn owned_ir_round_trips_with_each_synthetic_reason() {
        for reason in [
            SyntheticReason::Constructed,
            SyntheticReason::Aggregated,
            SyntheticReason::Computed,
        ] {
            let value = Value::Int(0);
            let mut provenance = ProvenanceMap::new();
            let pointer = Pointer::parse("/x").expect("/x parses");
            provenance.insert(pointer.as_canonical(), Provenance::Synthetic { reason });
            let format = FormatTag::Json;
            let original = OwnedIr::new(value.clone(), provenance.clone(), format);
            let (v, p, f) = original.into_parts();
            assert_eq!(v, value, "value must round-trip for reason {reason:?}");
            assert_eq!(
                p, provenance,
                "provenance must round-trip for reason {reason:?}"
            );
            assert_eq!(f, format, "format must round-trip for reason {reason:?}");
            // Borrowed-view lookup via the round-tripped triple agrees on
            // the synthetic shape — `span_for` is `None` for every variant.
            let owned = OwnedIr::new(v, p, f);
            assert!(owned.to_borrowed().span_for(&pointer).is_none());
        }
    }

    // -- Document::as_ir zero-copy and mutation-visibility ----------------
    //
    // The full spec scenario "`Document::as_ir` is zero-copy" is hard to
    // assert without unsafe pointer arithmetic; the pragmatic substitute is
    // to confirm two consecutive calls return `Ir`s that point at the SAME
    // `&Value` / `&ProvenanceMap` (same memory address). A regression that
    // accidentally cloned the value tree on every call would fail this
    // test — the addresses would diverge.
    //
    // The companion scenario "mutation reflects on next as_ir()" is the
    // other half of the contract: `Document::set_at` must invalidate the
    // *previously returned* `Ir`'s view of the value and provenance so the
    // next `as_ir()` reflects the write. The test below exercises an
    // in-span replacement against a write-aware YAML document.

    #[test]
    fn document_as_ir_two_calls_borrow_the_same_value_and_provenance() {
        use crate::Document;
        use crate::document::SpanMap;
        use indexmap::IndexMap;

        let mut spans = SpanMap::new();
        spans.insert(
            "/a".into(),
            ValueSpan {
                value_range: 3..4,
                line_range: 0..5,
                indent: 0,
                context: SpanContext::BlockMapValue,
            },
        );
        let mut map = IndexMap::new();
        map.insert("a".to_owned(), Value::Int(1));
        let doc = Document::with_spans(Value::Map(map), b"a: 1\n".to_vec(), spans, FormatTag::Yaml);
        let first = doc.as_ir();
        let second = doc.as_ir();
        assert!(
            std::ptr::eq(first.value(), second.value()),
            "consecutive as_ir() calls must borrow the same `&Value` — \
             a regression that cloned the tree on every call would diverge here",
        );
        assert!(
            std::ptr::eq(first.provenance(), second.provenance()),
            "consecutive as_ir() calls must borrow the same `&ProvenanceMap` — \
             a regression that materialised a fresh map on every call would diverge here",
        );
        assert_eq!(first.format(), second.format());
    }

    #[test]
    fn document_as_ir_reflects_set_at_mutation() {
        // Spec scenario "mutation reflects on next as_ir()". Build a
        // write-aware YAML doc, call `as_ir()` once to capture the
        // pre-mutation view, mutate via `set_at`, then call `as_ir()`
        // again — the post-mutation view's value tree and span lookup
        // must reflect the write. The `SpanMap`-derived provenance map
        // is rebuilt by `set_at` so `span_for` keeps matching `span_at`
        // through the edit.
        use crate::Document;
        use crate::document::SpanMap;
        use indexmap::IndexMap;

        let mut spans = SpanMap::new();
        spans.insert(
            "/a".into(),
            ValueSpan {
                value_range: 3..4,
                line_range: 0..5,
                indent: 0,
                context: SpanContext::BlockMapValue,
            },
        );
        let mut map = IndexMap::new();
        map.insert("a".to_owned(), Value::Int(1));
        let mut doc =
            Document::with_spans(Value::Map(map), b"a: 1\n".to_vec(), spans, FormatTag::Yaml);
        let pointer = Pointer::parse("/a").expect("/a parses");

        // Pre-mutation: `/a` resolves to Int(1).
        let pre = doc.as_ir();
        match pre.value() {
            Value::Map(m) => assert_eq!(m.get("a"), Some(&Value::Int(1))),
            other => panic!("expected map, got: {other:?}"),
        }
        assert!(
            pre.span_for(&pointer).is_some(),
            "pre-mutation provenance must surface a span for /a",
        );

        // Mutate.
        doc.set_at(&pointer, Value::Int(5))
            .expect("set_at must succeed on write-aware YAML doc");

        // Post-mutation: a fresh `as_ir()` view must reflect the new value
        // AND keep span lookup consistent.
        let post = doc.as_ir();
        match post.value() {
            Value::Map(m) => assert_eq!(
                m.get("a"),
                Some(&Value::Int(5)),
                "post-mutation as_ir().value() must reflect set_at's write",
            ),
            other => panic!("expected map, got: {other:?}"),
        }
        let span = post
            .span_for(&pointer)
            .expect("post-mutation span lookup must still resolve /a");
        // Single-byte → single-byte replacement: range unchanged.
        assert_eq!(span.value_range, 3..4);
        // The provenance map and the span map must agree byte-for-byte
        // after the write — they're the two faces of the same metadata.
        assert_eq!(
            doc.span_at(&pointer),
            post.span_for(&pointer),
            "Document::span_at and Ir::span_for must agree after a mutation",
        );
    }

    // -- Phase 2: line_col_for sanity tests -------------------------------
    //
    // The Phase 2 contract pins two cases per the spec:
    //   - positive: a populated span + non-empty source bytes resolve to a
    //     1-indexed `(line, col)` matching the parser-supplied byte range.
    //   - negative: any of (no span, empty source bytes) yields `None`.
    // The `Ir::new` 3-arg constructor leaves `bytes` empty, so synthesised
    // IRs never accidentally claim a spurious source position.

    #[test]
    fn line_col_for_returns_some_for_span_in_source_bytes() {
        // Build a write-aware YAML doc with a known span at byte offset
        // 3..4 (the `1` in `a: 1\n`). On the first line the column maps
        // directly to byte offset + 1 — i.e. (1, 4).
        use crate::Document;
        use crate::document::SpanMap;
        use indexmap::IndexMap;

        let mut spans = SpanMap::new();
        spans.insert(
            "/a".into(),
            ValueSpan {
                value_range: 3..4,
                line_range: 0..5,
                indent: 0,
                context: SpanContext::BlockMapValue,
            },
        );
        let mut map = IndexMap::new();
        map.insert("a".to_owned(), Value::Int(1));
        let doc = Document::with_spans(Value::Map(map), b"a: 1\n".to_vec(), spans, FormatTag::Yaml);
        let pointer = Pointer::parse("/a").expect("/a parses");
        assert_eq!(
            doc.as_ir().line_col_for(&pointer),
            Some((1, 4)),
            "line_col_for must derive (1, 4) for byte offset 3 in `a: 1\\n`",
        );
    }

    #[test]
    fn line_col_for_returns_none_without_source_bytes() {
        // `Ir::new` (the 3-arg constructor) leaves `bytes` empty even if a
        // span is present in the provenance map. `line_col_for` MUST
        // therefore return `None` — synthesised IRs (jq adapter, fix-op
        // application) cannot claim a source position they don't have.
        let value = sample_value();
        let mut provenance = ProvenanceMap::new();
        let pointer = Pointer::parse("/foo").expect("/foo parses");
        provenance.insert(
            pointer.as_canonical(),
            Provenance::Original {
                pointer: pointer.clone(),
                span: Some(sample_span()),
            },
        );
        let ir = Ir::new(&value, &provenance, FormatTag::Yaml);
        assert!(ir.bytes().is_empty(), "Ir::new must default bytes to &[]");
        assert!(
            ir.line_col_for(&pointer).is_none(),
            "line_col_for must return None when bytes is empty",
        );
    }
}
