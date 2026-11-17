//! Integration test: YAML provenance via `Document::as_ir()`.
//!
//! Spec scenario covered: "YAML parse produces provenance for every leaf"
//! ([`openspec/changes/add-ir-foundation/specs/data-query-ir/spec.md`]).
//!
//! Pinned against the public, write-aware YAML entrypoint
//! `parse_yaml_with_spans`: a small YAML fixture with comments and nested
//! mappings is parsed, then for every leaf pointer we assert that:
//!
//! 1. `doc.as_ir().span_for(&pointer)` returns `Some(span)`.
//! 2. `doc.spans().get(&pointer.as_canonical())` returns the *same* span
//!    (by `PartialEq` — `ValueSpan` derives it for exactly this kind of
//!    cross-channel cross-check).
//!
//! The fixture lives inline in this file, not under `tests/fixtures/`, so the
//! test stays self-contained — there is no risk of a stray edit to a shared
//! fixture changing what this test asserts.
//!
//! Comments inside YAML are deliberately included: the M2 span builder must
//! ignore them when computing leaf byte ranges, and a regression that
//! accidentally folded a comment into the `value_range` would make span
//! lookup disagree with the parser's own `SpanMap` — exactly the divergence
//! this test rules out.

use dq_core::document::FormatTag;
use dq_core::parsers::yaml_spans::parse_yaml_with_spans;
use dq_core::{InlineBaseline, Pointer, Provenance};

/// Inline YAML fixture with a top comment, a same-line trailing comment, and
/// a nested mapping. Three leaves: `/name`, `/spec/replicas`, `/spec/image`.
const FIXTURE: &str = "\
# top comment
name: foo  # inline
spec:
  replicas: 3
  image: nginx:1.0
";

#[test]
fn every_leaf_pointer_resolves_to_the_same_span_via_ir_and_spans() {
    let doc =
        parse_yaml_with_spans(FIXTURE.as_bytes()).expect("write-aware YAML parse must succeed");
    assert_eq!(
        doc.as_ir().format(),
        FormatTag::Yaml,
        "format tag must surface the YAML source through the IR view",
    );

    for pointer_str in ["/name", "/spec/replicas", "/spec/image"] {
        let pointer = Pointer::parse(pointer_str).expect("fixture pointer parses");

        // Path 1: the IR's lookup helper.
        let ir_span = doc
            .as_ir()
            .span_for(&pointer)
            .unwrap_or_else(|| panic!("Ir::span_for must return Some for leaf `{pointer_str}`"));

        // Path 2: the SpanMap directly. Both must be byte-identical because
        // the IR's `ProvenanceMap` is derived from this same `SpanMap`.
        let canonical = pointer.as_canonical();
        let direct_span = doc.spans().get(&canonical).unwrap_or_else(|| {
            panic!("SpanMap must contain a span for the same canonical key `{canonical}`")
        });

        assert_eq!(
            ir_span, direct_span,
            "Ir::span_for and Document::spans().get must agree for `{pointer_str}` — \
             a regression where the provenance map drifted from the span map would \
             surface here as a structural diff",
        );
    }
}

#[test]
fn provenance_for_every_leaf_is_original_with_some_span() {
    // The lookup wrapper `span_for` flattens a Synthetic into None; this
    // companion test pins the structural shape of the underlying entry —
    // every leaf produced by a write-aware YAML parse must be
    // `Original { span: Some(_), .. }`. A bug that emitted `Synthetic` for
    // a real source pointer would corrupt downstream lint diagnostics
    // (Phase 2+ of `add-ir-foundation`), so it is worth asserting on the
    // structural shape, not just the span result.
    let doc =
        parse_yaml_with_spans(FIXTURE.as_bytes()).expect("write-aware YAML parse must succeed");

    for pointer_str in ["/name", "/spec/replicas", "/spec/image"] {
        let pointer = Pointer::parse(pointer_str).expect("fixture pointer parses");
        match doc.as_ir().provenance_for(&pointer) {
            Some(Provenance::Original {
                pointer: p,
                span,
                inline_offset,
            }) => {
                assert_eq!(
                    &p.as_canonical(),
                    &pointer.as_canonical(),
                    "the structured pointer inside Original must round-trip to the \
                     same canonical key under which the entry is stored",
                );
                assert!(
                    span.is_some(),
                    "every leaf in a write-aware YAML doc must carry Some(span); \
                     `{pointer_str}` had None",
                );
                assert!(
                    inline_offset.is_none(),
                    "plain / quoted YAML scalars MUST have inline_offset = None — \
                     only block scalars (`|`, `>`, `|-`, `>-`) opt in to the \
                     inline-offset baseline; `{pointer_str}` is a plain scalar \
                     and must not carry one",
                );
            }
            other => panic!("expected Original{{Some(span)}} for `{pointer_str}`, got: {other:?}"),
        }
    }
}

/// Phase 2 spec scenario ("YAML block scalar carries inline-offset"): a YAML
/// document containing a block scalar at `/script` with body
/// `"echo 1\necho 2\n"` MUST surface
/// `inline_offset = Some(InlineBaseline { byte_start: 0, line: 1, col: 1 })`
/// on its `Provenance::Original` entry. The contract is asserted through
/// both `provenance_for` (pattern-match on the underlying field) and
/// `Ir::inline_offset_for` (the public lookup helper) so a regression that
/// drifted between the two surfaces here.
#[test]
fn block_scalar_at_script_carries_inline_baseline_via_both_paths() {
    let bytes = b"script: |\n  echo 1\n  echo 2\n";
    let doc = parse_yaml_with_spans(bytes).expect("write-aware YAML parse must succeed");
    let pointer = Pointer::parse("/script").expect("/script parses");

    // Path 1: pattern-match the underlying provenance entry.
    match doc.as_ir().provenance_for(&pointer) {
        Some(Provenance::Original {
            inline_offset,
            span,
            ..
        }) => {
            assert!(
                span.is_some(),
                "block scalar must still carry a ValueSpan — write-aware path produces both",
            );
            assert_eq!(
                *inline_offset,
                Some(InlineBaseline {
                    byte_start: 0,
                    line: 1,
                    col: 1,
                }),
                "Provenance::Original.inline_offset MUST be Some(0,1,1) for a YAML \
                 block scalar; composite-rule projection (Phase 4) reads this baseline",
            );
        }
        other => panic!("expected Original for /script, got: {other:?}"),
    }

    // Path 2: the public lookup helper must agree.
    let helper = doc.as_ir().inline_offset_for(&pointer);
    let expected = InlineBaseline {
        byte_start: 0,
        line: 1,
        col: 1,
    };
    assert_eq!(
        helper,
        Some(&expected),
        "Ir::inline_offset_for must agree with the underlying \
         Provenance::Original.inline_offset field",
    );
}

#[test]
fn unknown_pointer_returns_none_from_both_lookups() {
    // Mirror of the positive case: a pointer that does NOT exist in the
    // source must be `None` in both lookup paths. Anchoring this prevents a
    // future regression where, say, `provenance_for` started seeding a
    // synthetic entry for every uncached lookup.
    let doc =
        parse_yaml_with_spans(FIXTURE.as_bytes()).expect("write-aware YAML parse must succeed");
    let pointer = Pointer::parse("/spec/replicaCountThatDoesntExist").expect("pointer parses");
    assert!(
        doc.as_ir().span_for(&pointer).is_none(),
        "Ir::span_for must report None for an unknown pointer",
    );
    assert!(
        doc.spans().get(&pointer.as_canonical()).is_none(),
        "SpanMap must report None for the same unknown pointer",
    );
}
