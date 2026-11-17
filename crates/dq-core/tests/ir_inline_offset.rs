//! Cross-format integration test: `inline_offset` population on the
//! `data-query-ir` provenance side-channel.
//!
//! Spec scenarios covered (`add-validation-and-extended-formats`,
//! `data-query-ir`):
//!
//! - "YAML block scalar carries inline-offset"
//! - "Markdown fenced code block carries inline-offset"
//! - "TOML scalar carries no inline-offset"
//! - "Lookup returns inline-offset"
//! - "Lookup returns None when offset absent"
//!
//! Plus one negative-shape test for JSONL — the spec is explicit that every
//! non-YAML / non-markdown parser keeps `inline_offset = None`, so a single
//! representative sample (TOML + JSONL) is enough to pin the contract.
//!
//! YAML coverage in `tests/ir_yaml_provenance.rs` already pins the
//! cross-channel agreement between `Ir::span_for` and `Document::span_at`;
//! this file adds the inline-offset axis.

use dq_core::format::Format;
use dq_core::ir::{InlineBaseline, Provenance};
use dq_core::parsers::yaml_spans::parse_yaml_with_spans;
use dq_core::parsers::{Jsonl, Markdown, Toml};
use dq_core::pointer::Pointer;

/// Phase 2 contract: a YAML block scalar at `/script` with body
/// `"echo 1\necho 2\n"` MUST surface
/// `inline_offset = Some(InlineBaseline { 0, 1, 1 })` on its
/// `Provenance::Original` entry, addressable via
/// `provenance_for(&Pointer::parse("/script"))` and
/// `Ir::inline_offset_for(...)`.
#[test]
fn yaml_block_scalar_at_script_carries_baseline_via_both_lookups() {
    let bytes = b"script: |\n  echo 1\n  echo 2\n";
    let doc = parse_yaml_with_spans(bytes).expect("write-aware YAML parse must succeed");
    let pointer = Pointer::parse("/script").expect("/script parses");

    // Path 1: pattern-match the underlying provenance entry directly.
    let direct = doc.as_ir().provenance_for(&pointer);
    match direct {
        Some(Provenance::Original { inline_offset, .. }) => assert_eq!(
            *inline_offset,
            Some(InlineBaseline {
                byte_start: 0,
                line: 1,
                col: 1,
            }),
            "Provenance::Original.inline_offset MUST be Some(0,1,1) for a YAML \
             block scalar — composite-rule projection depends on this baseline",
        ),
        other => panic!("expected Original for /script, got: {other:?}"),
    }

    // Path 2: the public lookup helper must agree with the underlying field.
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

/// Markdown fenced code block at `/children/0/value` carries
/// `inline_offset = Some(InlineBaseline { 0, 1, 1 })`. The pointer reflects
/// the M9 typed-discriminator-Map shape: top-level `Document` is
/// `Map { "children": Array<...> }`, each child is a typed-node Map, and a
/// fenced code block's body lives in its `value` field.
#[test]
fn markdown_fenced_code_block_carries_baseline() {
    let src = "```yaml\nfoo: bar\n```\n";
    let doc = Markdown.parse(src.as_bytes()).expect("markdown parse");
    let pointer = Pointer::parse("/children/0/value").expect("pointer parses");

    let helper = doc.as_ir().inline_offset_for(&pointer);
    let expected = InlineBaseline {
        byte_start: 0,
        line: 1,
        col: 1,
    };
    assert_eq!(
        helper,
        Some(&expected),
        "fenced code block's `value` MUST carry inline_offset = Some(0,1,1)",
    );
}

/// Spec scenario "TOML scalar carries no inline-offset": every
/// `Provenance::Original` entry produced by the TOML parser MUST have
/// `inline_offset = None`. The TOML parser uses `Document::with_spans` which
/// routes through `provenance_from_spans` — defaulting every entry to
/// `inline_offset: None` — so the contract is structural, not per-leaf.
#[test]
fn toml_scalar_carries_no_inline_offset() {
    let bytes = b"name = \"foo\"\nport = 8080\n";
    let doc = Toml.parse(bytes).expect("toml parse");
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
        "TOML parser MUST NOT populate inline_offset on any entry — \
         a regression that started doing so would change composite-rule \
         coordinate projection for every TOML config",
    );
}

/// JSONL parser does not record any provenance metadata (read-only format,
/// no `SpanMap`). The contract is that `inline_offset_for` returns `None`
/// for every pointer reachable in the value tree. We pin this with a
/// shape-level assertion: the provenance map is empty, so no entry can
/// carry inline-offset.
#[test]
fn jsonl_carries_no_inline_offset_anywhere() {
    let bytes = b"{\"a\":1}\n{\"a\":2}\n";
    let doc = Jsonl.parse(bytes).expect("jsonl parse");
    assert!(
        doc.as_ir().provenance().is_empty(),
        "JSONL parser MUST emit an empty provenance map (read-only format); \
         a regression that started populating entries would silently change \
         the inline-offset contract for JSONL composite extracts",
    );
}

/// Lookup helper returns `None` when the entry is `Original` but
/// `inline_offset` is `None` — verified through a write-aware YAML doc
/// whose leaves are all plain scalars.
#[test]
fn inline_offset_for_returns_none_for_plain_yaml_scalars() {
    let bytes = b"name: foo\nport: 8080\n";
    let doc = parse_yaml_with_spans(bytes).expect("parse");
    for pointer_str in ["/name", "/port"] {
        let pointer = Pointer::parse(pointer_str).expect("pointer parses");
        assert!(
            doc.as_ir().inline_offset_for(&pointer).is_none(),
            "plain scalar `{pointer_str}` MUST surface None via inline_offset_for",
        );
    }
}

/// Lookup helper returns `None` for an unknown pointer — same fall-through
/// as `span_for`. Pinning this rules out a regression where the helper
/// accidentally seeded an empty entry on miss.
#[test]
fn inline_offset_for_returns_none_for_unknown_pointer() {
    let bytes = b"script: |\n  echo 1\n";
    let doc = parse_yaml_with_spans(bytes).expect("parse");
    let unknown = Pointer::parse("/does/not/exist").expect("pointer parses");
    assert!(doc.as_ir().inline_offset_for(&unknown).is_none());
}
