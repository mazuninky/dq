//! Integration test: JSONL parsers emit empty provenance.
//!
//! Spec scenario covered: "JSONL parse produces empty provenance"
//! ([`openspec/changes/add-ir-foundation/specs/data-query-ir/spec.md`]).
//!
//! The JSONL parser is a read-only format — it does not record byte spans.
//! `Document::as_ir()` for a JSONL document must therefore surface an empty
//! provenance map but still report the correct `FormatTag::Jsonl` so callers
//! can distinguish "no provenance available for this format" from "this is
//! not the format you think it is" (the empty-vs-missing distinction the
//! spec explicitly calls out).

use dq_core::document::FormatTag;
use dq_core::format::Format;
use dq_core::parsers::Jsonl;
use dq_core::{Pointer, Value};

/// Tiny multi-record JSONL fixture: two records, each a single-key object.
/// The fixture is small on purpose — JSONL emptiness is independent of
/// document size, so a small payload exercises the full contract while
/// keeping the test trivially auditable.
const FIXTURE: &str = "{\"a\":1}\n{\"b\":2}\n";

#[test]
fn jsonl_document_has_empty_provenance_and_correct_format_tag() {
    let doc = Jsonl
        .parse(FIXTURE.as_bytes())
        .expect("JSONL parse must succeed on well-formed input");

    let ir = doc.as_ir();

    // The format tag is preserved end-to-end so callers know they're
    // looking at a JSONL document with empty provenance — not, for
    // example, a YAML document whose span builder happened to bail.
    assert_eq!(
        ir.format(),
        FormatTag::Jsonl,
        "Document::as_ir() must surface FormatTag::Jsonl for JSONL inputs",
    );

    // The provenance map itself is empty.
    assert!(
        ir.provenance().is_empty(),
        "JSONL parser must emit an empty ProvenanceMap; got {} entries",
        ir.provenance().len(),
    );

    // The structural value is what callers expect: a top-level array of
    // two records (single-key objects). This anchors the test against the
    // JSONL parser's own contract — if the parser ever started populating
    // provenance, the test below (`provenance_for` returning None for
    // every plausible leaf) would still want this shape to be right.
    match ir.value() {
        Value::Array(items) => assert_eq!(items.len(), 2, "two records in fixture"),
        other => panic!("expected top-level Value::Array, got: {other:?}"),
    }
}

#[test]
fn jsonl_document_provenance_for_known_pointers_returns_none() {
    // Spec wording: "Document's `as_ir().provenance` reports the empty
    // case — use `Ir::provenance_for` against a few obvious pointers and
    // confirm all return `None`." Exercise both the root pointer and a
    // leaf that *does* exist in the value tree (`/0/a`) — the contract
    // must hold for pointers that *would* be valid if the parser had
    // recorded spans.
    let doc = Jsonl
        .parse(FIXTURE.as_bytes())
        .expect("JSONL parse must succeed");
    let ir = doc.as_ir();

    for pointer_str in ["", "/0", "/0/a", "/1/b"] {
        let pointer = Pointer::parse(pointer_str).expect("test pointer parses");
        assert!(
            ir.provenance_for(&pointer).is_none(),
            "JSONL doc must have no provenance for `{pointer_str}`; \
             read-only parsers carry an empty ProvenanceMap by design",
        );
        assert!(
            ir.span_for(&pointer).is_none(),
            "JSONL doc must have no span lookup for `{pointer_str}` — \
             span_for is the user-facing wrapper around provenance_for",
        );
    }
}
