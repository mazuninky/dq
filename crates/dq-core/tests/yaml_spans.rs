//! Integration tests for the M2 YAML write-pat span builder and renderers.
//!
//! These tests pin the **public contract** of `parse_with_spans` /
//! `parse_yaml_with_spans` and the two textual-edit renderers
//! (`YamlScalarRenderer`, `YamlInsertionRenderer`) against representative
//! shapes the production code ships against. Unit tests inside the module
//! cover the same primitives at finer granularity; the value of these
//! integration tests is that they:
//!
//! - exercise the full `Document::with_spans` shape via the public
//!   re-export `parse_yaml_with_spans` so accidental signature changes
//!   surface at the integration boundary;
//! - lock in span byte positions on a real-world fixture (a Kubernetes
//!   `Deployment`) so a refactor that drifts span semantics can't pass
//!   silently;
//! - assert how the parser behaves on YAML 1.2 features the M2 baseline
//!   does *not* support (anchors / aliases) — preventing a future
//!   "look, set_at on `*base` works!" regression that would actually be
//!   modifying the wrong span.
//!
//! # Documented baseline (M2)
//!
//! - `Event::Alias` is skipped; **no span is recorded** for the alias
//!   reference. `Document::set_at` on the alias path therefore surfaces
//!   `Error::Path { kind: MissingKey }` instead of mutating the anchored
//!   value. This is intentional — see `parsers/yaml_spans.rs` module docs.

use dq_core::document::FormatTag;
use dq_core::document::spans::SpanContext;
use dq_core::parsers::yaml_spans::{
    YamlInsertionRenderer, YamlScalarRenderer, parse_with_spans, parse_yaml_with_spans,
};
use dq_core::textual_edit::{InsertionRenderer, ScalarRenderer};
use dq_core::{Pointer, Value};
use indexmap::IndexMap;
use pretty_assertions::assert_eq;

// -----------------------------------------------------------------------
// 1. parse_with_spans on a Kubernetes-Deployment-shaped fixture
// -----------------------------------------------------------------------

/// Real-world fixture: `tests/fixtures/yaml/k8s_deployment.yaml` contains a
/// UTF-8 em-dash (`—`, 3 bytes) inside the second comment line. This
/// previously broke the span builder because `saphyr-parser` 0.0.6 reports
/// `Marker::index()` as a **character** count and the splice path needed
/// **byte** offsets — every span after the em-dash was off by 2 bytes. The
/// builder now translates char-index → byte-offset; this test pins that
/// behaviour against the actual fixture so a regression cannot land
/// silently.
const K8S_DEPLOYMENT_FIXTURE: &str = include_str!("fixtures/yaml/k8s_deployment.yaml");

#[test]
fn k8s_deployment_replicas_span_covers_only_value_byte() {
    // `/spec/replicas` is the canonical sample edit target for a YAML
    // write path: humans set it from CI, agents from automation. The span
    // must cover EXACTLY the digit `3` — not the surrounding whitespace,
    // not the colon, not the trailing newline. Any drift here means
    // `Document::set_at` would corrupt indentation when bumping the value.
    let bytes = K8S_DEPLOYMENT_FIXTURE.as_bytes();
    let (_, spans) = parse_with_spans(bytes).expect("parse_with_spans must succeed");

    let span = spans
        .get("/spec/replicas")
        .expect("/spec/replicas span must be recorded");
    let value_slice = &bytes[span.value_range.clone()];
    assert_eq!(
        value_slice,
        b"3",
        "value_range must cover exactly the digit `3` (got bytes: {:?})",
        std::str::from_utf8(value_slice).unwrap_or("<non-utf8>"),
    );

    // line_range covers the entire physical line including trailing newline,
    // so del_at would remove the line cleanly.
    let line_slice = &bytes[span.line_range.clone()];
    assert_eq!(
        line_slice, b"  replicas: 3\n",
        "line_range must cover the whole `  replicas: 3\\n` line",
    );
    assert_eq!(
        span.context,
        SpanContext::BlockMapValue,
        "/spec/replicas is a block-mapping value",
    );
}

// -----------------------------------------------------------------------
// 1a. Regression: multi-byte UTF-8 before the value must not shift its span
// -----------------------------------------------------------------------

#[test]
fn parse_with_spans_handles_multibyte_utf8_in_preamble() {
    // Em-dash before the value triggers the saphyr-parser char-vs-byte
    // bug when the marker index isn't translated. After the fix,
    // value_range must point at the actual bytes of the value, not at a
    // position offset by the bytes used for the em-dash representation.
    let yaml = "# header — note\nname: alice\nport: 8080\n";
    let bytes = yaml.as_bytes();
    let doc = parse_yaml_with_spans(bytes).expect("parse_yaml_with_spans");
    let port_span = doc
        .span_at(&Pointer::parse("/port").expect("pointer parses"))
        .expect("/port span must exist");
    let value_bytes = &bytes[port_span.value_range.clone()];
    assert_eq!(
        value_bytes,
        b"8080",
        "value_range after a multi-byte preamble must point at exactly `8080` \
         (got bytes: {:?})",
        std::str::from_utf8(value_bytes).unwrap_or("<non-utf8>"),
    );

    // Also verify a span before the em-dash and a span after share the
    // same correctness — pre-em-dash content has char-index == byte-index,
    // post-em-dash content has char-index < byte-index.
    let name_span = doc
        .span_at(&Pointer::parse("/name").expect("pointer parses"))
        .expect("/name span must exist");
    assert_eq!(&bytes[name_span.value_range.clone()], b"alice");
}

// -----------------------------------------------------------------------
// 2. flow mapping → SpanContext::FlowMapValue
// -----------------------------------------------------------------------

#[test]
fn flow_mapping_values_are_tagged_as_flow_map() {
    // `data: {a: 1, b: 2}` — both `/a` and `/b` (after the `data:` parent)
    // must report the flow-mapping context so the renderer knows to quote
    // structural characters (`,`, `]`, `}`) when replacing.
    let bytes = b"{a: 1, b: 2}\n";
    let (_, spans) = parse_with_spans(bytes).expect("parse must succeed");
    let a = spans.get("/a").expect("/a span");
    let b = spans.get("/b").expect("/b span");
    assert_eq!(
        a.context,
        SpanContext::FlowMapValue,
        "flow-mapping value must be tagged FlowMapValue",
    );
    assert_eq!(
        b.context,
        SpanContext::FlowMapValue,
        "every flow-mapping value carries the same context tag",
    );
    // Sanity-check that the bytes reported are the actual scalars.
    assert_eq!(&bytes[a.value_range.clone()], b"1");
    assert_eq!(&bytes[b.value_range.clone()], b"2");
}

// -----------------------------------------------------------------------
// 3. block sequence → SpanContext::BlockSeqItem
// -----------------------------------------------------------------------

#[test]
fn block_sequence_items_are_tagged_as_block_seq() {
    // `- a\n- b\n` — two block-sequence items at the document root.
    // `del_at` consults the context to know it can splice out the whole
    // line; flow-sequence items would need a different splice strategy.
    let bytes = b"- a\n- b\n";
    let (_, spans) = parse_with_spans(bytes).expect("parse must succeed");
    let zero = spans.get("/0").expect("/0 span");
    let one = spans.get("/1").expect("/1 span");
    assert_eq!(
        zero.context,
        SpanContext::BlockSeqItem,
        "block-sequence item must be tagged BlockSeqItem",
    );
    assert_eq!(
        one.context,
        SpanContext::BlockSeqItem,
        "every block-sequence item carries the same context tag",
    );
    assert_eq!(&bytes[zero.value_range.clone()], b"a");
    assert_eq!(&bytes[one.value_range.clone()], b"b");
}

// -----------------------------------------------------------------------
// 4. anchor + alias — alias has NO span recorded (M2 baseline)
// -----------------------------------------------------------------------

#[test]
fn alias_event_is_skipped_no_span_recorded() {
    // YAML anchors (`&`) and aliases (`*`) are a known M2 limitation: the
    // span builder skips `Event::Alias`. We pin the baseline behaviour so
    // a future change that flips this (e.g. M3 anchor support) MUST also
    // update this test rather than silently changing the contract.
    //
    // `&base ...` defines an anchor; `*base` would normally substitute the
    // anchored value at parse time. The span builder only records spans
    // for resolved scalar values it observes — never for the alias event
    // itself.
    let bytes = b"defaults: &base 100\nactual: *base\n";
    let (_, spans) = parse_with_spans(bytes).expect("parse must succeed");

    // The anchored scalar `100` has a span on the line where it's defined.
    let defaults = spans.get("/defaults").expect("/defaults span");
    assert_eq!(&bytes[defaults.value_range.clone()], b"100");

    // The alias reference at `/actual` does NOT get a span — the
    // implementation logs at debug level and skips the event. Pin this
    // explicitly so the absence is observable, not silent.
    assert!(
        spans.get("/actual").is_none(),
        "M2 baseline: alias references must NOT have a span recorded; got: {:?}",
        spans.get("/actual"),
    );
}

// -----------------------------------------------------------------------
// 5. multi-doc YAML — pointers prefixed with /<doc_idx>
// -----------------------------------------------------------------------

#[test]
fn multi_doc_yaml_prefixes_pointers_with_doc_index() {
    // Three documents separated by `---`. Each document's spans are
    // namespaced under `/0/...`, `/1/...`, `/2/...`. The renderer factory
    // depends on this convention; getting it wrong would make `set_at`
    // edit the wrong document.
    let bytes = b"---\nname: a\n---\nname: b\n---\nname: c\n";
    let (_, spans) = parse_with_spans(bytes).expect("parse must succeed");
    for canonical in ["/0/name", "/1/name", "/2/name"] {
        assert!(
            spans.contains_key(canonical),
            "missing {canonical}; have keys: {:?}",
            spans.keys().collect::<Vec<_>>(),
        );
    }
    // And the bytes the spans point at are the right per-document scalars.
    assert_eq!(
        &bytes[spans.get("/0/name").unwrap().value_range.clone()],
        b"a"
    );
    assert_eq!(
        &bytes[spans.get("/1/name").unwrap().value_range.clone()],
        b"b"
    );
    assert_eq!(
        &bytes[spans.get("/2/name").unwrap().value_range.clone()],
        b"c"
    );
}

// -----------------------------------------------------------------------
// 6. YamlScalarRenderer preserves DoubleQuoted style
// -----------------------------------------------------------------------

#[test]
fn scalar_renderer_preserves_double_quoted_style() {
    // The renderer detects the original style by inspecting the first
    // non-whitespace byte: leading `"` → DoubleQuoted, leading `'` →
    // SingleQuoted, etc. Replacing a DoubleQuoted value with a String
    // must keep the surrounding quotes — otherwise edits to a YAML
    // file with consistent style suddenly produce mixed-style output.
    let renderer = YamlScalarRenderer;
    let out = renderer.render_replacement(
        &Value::String("new".into()),
        SpanContext::BlockMapValue,
        b"\"old\"",
    );
    assert_eq!(
        out, b"\"new\"",
        "replacement must keep the surrounding double quotes",
    );
}

// -----------------------------------------------------------------------
// 7. Bare → DoubleQuoted upgrade when value contains unsafe characters
// -----------------------------------------------------------------------

#[test]
fn scalar_renderer_upgrades_bare_to_quoted_for_unsafe_value() {
    // A bare YAML scalar containing `: ` would re-parse as a key-value
    // pair, breaking the file. The renderer must upgrade to double-quoted
    // when the new value would need quoting.
    let renderer = YamlScalarRenderer;
    let out = renderer.render_replacement(
        &Value::String("a: b".into()),
        SpanContext::BlockMapValue,
        b"old",
    );
    assert_eq!(
        out, b"\"a: b\"",
        "value containing `: ` must be promoted from bare to double-quoted",
    );
}

// -----------------------------------------------------------------------
// 8. YamlInsertionRenderer for nested map produces parsable YAML
// -----------------------------------------------------------------------

#[test]
fn insertion_renderer_nested_map_output_is_parsable() {
    // The renderer's primary contract: every byte sequence it produces
    // MUST parse cleanly via the read-path YAML parser. If the renderer
    // ever emits invalid indentation or stray characters, this test
    // surfaces it without depending on the exact output bytes (which the
    // unit tests already pin).
    let renderer = YamlInsertionRenderer;
    let mut inner = IndexMap::new();
    inner.insert("type".into(), Value::String("RollingUpdate".into()));
    let fragment = renderer.render_insertion(
        "strategy",
        &Value::Map(inner),
        1, // top-level → indent column 1 (1-indexed)
        SpanContext::BlockMapValue,
    );
    // Skip the leading newline the renderer emits — the splice path
    // appends it after an existing line, so the parser sees the full
    // fragment minus the leading `\n`.
    let parsable = &fragment[1..];
    let parsed: serde_yml::Value = serde_yml::from_slice(parsable).unwrap_or_else(|e| {
        panic!(
            "renderer output must be valid YAML; got error: {e}\nbytes: {:?}",
            String::from_utf8_lossy(parsable)
        )
    });
    // Sanity: `strategy.type` round-trips through the read parser.
    let strategy = parsed
        .get("strategy")
        .expect("parsed YAML must contain `strategy` key");
    let kind = strategy
        .get("type")
        .expect("`strategy.type` must round-trip through parse");
    assert_eq!(
        kind.as_str(),
        Some("RollingUpdate"),
        "nested map value must round-trip exactly as the renderer encoded it",
    );
}

// -----------------------------------------------------------------------
// 9. parse error surfaces as Error::Parse with line >= 1 and a message
// -----------------------------------------------------------------------

#[test]
fn malformed_yaml_returns_parse_error_with_position() {
    // `key: : :` is syntactically invalid — saphyr-parser raises a
    // ScanError. The error must surface as `Error::Parse` with a non-empty
    // `message` and a `line >= 1` so the CLI can render a diagnostic.
    let bytes = b"key: : :\n";
    let err = parse_with_spans(bytes).expect_err("malformed YAML must error");
    match err {
        dq_core::Error::Parse { line, message, .. } => {
            assert!(
                !message.is_empty(),
                "Parse error must carry a non-empty message; got message=`{message}`",
            );
            assert!(
                line >= 1,
                "Parse error line must be 1-indexed and non-zero; got line={line}",
            );
        }
        other => panic!("expected Error::Parse, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// 10. Round-trip via Document::set_at preserves surrounding bytes
// -----------------------------------------------------------------------

#[test]
fn document_set_at_round_trip_on_replicas_preserves_other_bytes() {
    // The end-to-end smoke test: parse a fixture-shaped YAML with
    // `replicas: 3`, set_at(/spec/replicas, Int(5)), and assert that
    // (a) the replicas line is updated to `replicas: 5`, and
    // (b) every byte outside the modified span is byte-identical.
    let bytes = b"# header\nspec:\n  replicas: 3\n  port: 8080\n";
    let mut doc = parse_yaml_with_spans(bytes).expect("parse_yaml_with_spans");
    assert_eq!(doc.format(), FormatTag::Yaml);

    // Capture the span before edit so we can assert on byte-equality
    // outside the modified range.
    let pointer = Pointer::parse("/spec/replicas").expect("pointer parses");
    let span_before = doc
        .span_at(&pointer)
        .expect("/spec/replicas span must exist")
        .clone();

    doc.set_at(&pointer, Value::Int(5))
        .expect("set_at must succeed for known pointer");

    let after = doc.original_bytes();
    // The new buffer contains the new line.
    assert!(
        std::str::from_utf8(after)
            .expect("utf-8")
            .contains("replicas: 5"),
        "post-edit buffer must contain `replicas: 5`; got: {:?}",
        std::str::from_utf8(after).unwrap_or("<non-utf8>"),
    );
    assert!(
        !std::str::from_utf8(after)
            .expect("utf-8")
            .contains("replicas: 3"),
        "post-edit buffer must NOT still contain `replicas: 3`; got: {:?}",
        std::str::from_utf8(after).unwrap_or("<non-utf8>"),
    );

    // Bytes left of the edit must be byte-identical to the original.
    let left_end = span_before.value_range.start;
    assert_eq!(
        &after[..left_end],
        &bytes[..left_end],
        "bytes left of the splice must be byte-equal to the original",
    );
    // Bytes right of the edit must equal the original right-half (offset
    // by the same delta — replacing 1 byte with 1 byte means delta == 0).
    let original_right_start = span_before.value_range.end;
    let new_right_start = span_before.value_range.start + 1; // "5" is one byte
    assert_eq!(
        &after[new_right_start..],
        &bytes[original_right_start..],
        "bytes right of the splice must be byte-equal to the original tail",
    );
}
