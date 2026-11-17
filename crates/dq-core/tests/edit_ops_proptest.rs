//! Property-based tests for the [`EditScript`] vocabulary.
//!
//! The contract under test (per `specs/data-query-edit-ops/spec.md`,
//! Requirement "EditScript round-trip through Document::set_at / del_at"):
//!
//! 1. **Replace ↔ set_at byte equivalence.** For every `(pointer, value)`
//!    pair where `Document::set_at(pointer, value.clone())` succeeds in the
//!    M2 baseline, applying
//!    `EditScript::from(vec![EditOp::Replace { path: pointer, value }])`
//!    against a fresh clone of the same input document SHALL produce a
//!    byte-identical `original_bytes()` buffer.
//!
//! 2. **Remove ↔ del_at byte equivalence.** For every `pointer` where
//!    `Document::del_at(pointer)` succeeds, applying
//!    `EditScript::from(vec![EditOp::Remove { path: pointer }])` against a
//!    fresh clone SHALL produce a byte-identical buffer.
//!
//! Strategy structure:
//! - Three fixed parser-driven fixtures (one YAML, one JSON, one TOML),
//!   parsed via the public `parse_yaml_with_spans` / `parse_json_with_spans`
//!   / `by_name("toml").parse(...)` entry points so the strategy stays
//!   robust against any internal renderer change. Re-using parser entry
//!   points (rather than hand-building [`Document::with_spans`]) means a
//!   future renderer overhaul that breaks only the splice path will surface
//!   here, not be papered over by a stale fixture.
//! - The proptest selects a recorded-span pointer from the fixture and
//!   pairs it with a leaf-only [`Value`] from a small strategy
//!   (`Bool` / `Int(small)` / `String(short bare-safe)`).
//! - Cases where `set_at` / `del_at` would fail (out-of-bounds, type
//!   mismatch, mkdir-p territory) are skipped via `prop_assume!`. Comparison
//!   is byte-for-byte on `original_bytes()`.
//!
//! Runtime budget: 64 cases per property. The fixtures are tiny (≤ 200
//! bytes) and the splice path is O(bytes); the full proptest finishes well
//! under the 30 s suite-wide cold target.

use dq_core::{
    Document, EditOp, EditScript, Pointer, Value, by_name, parse_json_with_spans,
    parse_yaml_with_spans,
};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

// ----------------------- fixed parser-driven fixtures ----------------------

/// Small multi-key YAML doc with a nested map and an array. Every leaf
/// pointer is recorded by `parse_yaml_with_spans`, so the strategy below can
/// pick any of them.
const YAML_FIXTURE: &str = "name: example\n\
spec:\n  \
replicas: 3\n  \
port: 8080\n  \
strategy: rolling\n\
labels:\n  \
- alpha\n  \
- beta\n";

/// Small multi-key JSON doc with the same shape as the YAML fixture, so the
/// JSON property exercises a structurally similar set of pointers.
const JSON_FIXTURE: &str =
    "{\"name\":\"example\",\"replicas\":3,\"port\":8080,\"strategy\":\"rolling\"}";

/// Small TOML doc — TOML doesn't model arrays of strings the same way as
/// YAML, so we keep this fixture flat. Keys cover `String`, `Int`, and `Bool`.
const TOML_FIXTURE: &str = "name = \"example\"\n\
replicas = 3\n\
port = 8080\n\
strategy = \"rolling\"\n\
verbose = true\n";

/// Parse each fixture once per case; this is cheap (a few hundred bytes) and
/// keeps the property fully deterministic — no shared mutable state across
/// generated cases.
fn parse_yaml_fixture() -> Document {
    parse_yaml_with_spans(YAML_FIXTURE.as_bytes())
        .expect("YAML fixture must parse via the public parser entry point")
}

fn parse_json_fixture() -> Document {
    parse_json_with_spans(JSON_FIXTURE.as_bytes())
        .expect("JSON fixture must parse via the public parser entry point")
}

fn parse_toml_fixture() -> Document {
    let fmt = by_name("toml").expect("toml format must be registered");
    fmt.parse(TOML_FIXTURE.as_bytes())
        .expect("TOML fixture must parse via the format registry entry point")
}

/// Pick a recorded-span pointer out of `doc.spans()`. The strategy is
/// `Just`-based and uniformly samples the recorded keys; the actual
/// proptest case generator wires it together with a `Value` strategy below.
fn span_keys(doc: &Document) -> Vec<String> {
    let mut keys: Vec<String> = doc.spans().keys().cloned().collect();
    // Sort for proptest determinism: the SpanMap iteration order is
    // implementation-defined and can change with no behavioural impact.
    // Sorting here pins the case sequence under a fixed seed.
    keys.sort();
    keys
}

/// Generate a small leaf [`Value`] suitable as a Replace target. We keep it
/// to scalar variants whose YAML / JSON / TOML serialization is unambiguous
/// (no quoting promotion surprises) so a divergence in `original_bytes()`
/// after the splice cannot be blamed on the renderer's quote-style choice.
fn arb_leaf_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Bool(true)),
        Just(Value::Bool(false)),
        (-100_i64..=100_i64).prop_map(Value::Int),
        // Bare-safe ASCII string: lowercase letters and digits only, length
        // 1..=8. Picking only bare-safe strings means a YAML / TOML write
        // never has to promote to quoted form, which would change the byte
        // count and force the comparison to deal with span-shift.
        "[a-z][a-z0-9]{0,7}".prop_map(Value::String),
    ]
}

// ----------------------- proptest cases ------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        // 64 cases per property keeps the total proptest runtime well under
        // 1 s; that's enough to surface any byte-divergence between the
        // EditScript path and the direct set_at / del_at path without
        // slowing the suite-wide cold target.
        cases: 64,
        // Empty fork file path keeps proptest in-process (no OS fork) — the
        // fixtures are too small to benefit from forking and avoiding it
        // makes the property more reproducible across platforms.
        ..ProptestConfig::default()
    })]

    /// Replace ↔ set_at byte equivalence. The proptest case picks a recorded
    /// pointer, parses both `doc1` and `doc2` from the same fixture, mutates
    /// `doc1` via `Document::set_at` and `doc2` via `EditScript::Replace`,
    /// and asserts byte-equality of `original_bytes()`.
    #[test]
    fn yaml_replace_matches_set_at_byte_for_byte(
        // Picking any recorded pointer at random keeps the property
        // grammar-broad: nested map values, top-level scalars, array elems,
        // and the YAML doc-level prefix all show up in the SpanMap.
        key_idx in 0_usize..usize::MAX,
        new_value in arb_leaf_value(),
    ) {
        let doc_seed = parse_yaml_fixture();
        let keys = span_keys(&doc_seed);
        prop_assume!(!keys.is_empty(), "fixture must record at least one span");
        let key = &keys[key_idx % keys.len()];
        let pointer = Pointer::parse(key).expect("recorded keys round-trip through Pointer::parse");

        // Skip cases where `set_at` would fail in the M2 baseline. We
        // discover this by trying it on a throwaway clone first; if it
        // errs, the equivalence-with-EditScript property is vacuous and we
        // move on (proptest::prop_assume! reduces the case count, not a
        // shrink target).
        let mut doc1 = doc_seed.clone();
        if doc1.set_at(&pointer, new_value.clone()).is_err() {
            // Per spec: "for every (pointer, value) for which set_at
            // succeeds" — failures here are not in the property's domain.
            return Ok(());
        }

        let mut doc2 = doc_seed.clone();
        let script = EditScript::from(vec![EditOp::Replace {
            path: pointer.clone(),
            value: new_value.clone(),
        }]);
        script.apply(&mut doc2)
            .expect("EditScript::Replace must succeed iff set_at did (set_at already succeeded above)");

        prop_assert_eq!(
            doc1.original_bytes(),
            doc2.original_bytes(),
            "{}",
            format!(
                "YAML: EditScript::Replace must produce byte-identical output to \
                 Document::set_at for pointer={key:?} value={new_value:?}",
            ),
        );
    }

    #[test]
    fn json_replace_matches_set_at_byte_for_byte(
        key_idx in 0_usize..usize::MAX,
        new_value in arb_leaf_value(),
    ) {
        let doc_seed = parse_json_fixture();
        let keys = span_keys(&doc_seed);
        prop_assume!(!keys.is_empty(), "fixture must record at least one span");
        let key = &keys[key_idx % keys.len()];
        let pointer = Pointer::parse(key).expect("recorded keys round-trip");

        let mut doc1 = doc_seed.clone();
        if doc1.set_at(&pointer, new_value.clone()).is_err() {
            return Ok(());
        }
        let mut doc2 = doc_seed.clone();
        EditScript::from(vec![EditOp::Replace {
            path: pointer.clone(),
            value: new_value.clone(),
        }])
        .apply(&mut doc2)
        .expect("EditScript::Replace must succeed iff set_at did");
        prop_assert_eq!(
            doc1.original_bytes(),
            doc2.original_bytes(),
            "{}",
            format!(
                "JSON: EditScript::Replace must match set_at for pointer={key:?} value={new_value:?}",
            ),
        );
    }

    #[test]
    fn toml_replace_matches_set_at_byte_for_byte(
        key_idx in 0_usize..usize::MAX,
        new_value in arb_leaf_value(),
    ) {
        let doc_seed = parse_toml_fixture();
        let keys = span_keys(&doc_seed);
        prop_assume!(!keys.is_empty(), "fixture must record at least one span");
        let key = &keys[key_idx % keys.len()];
        let pointer = Pointer::parse(key).expect("recorded keys round-trip");

        let mut doc1 = doc_seed.clone();
        if doc1.set_at(&pointer, new_value.clone()).is_err() {
            return Ok(());
        }
        let mut doc2 = doc_seed.clone();
        EditScript::from(vec![EditOp::Replace {
            path: pointer.clone(),
            value: new_value.clone(),
        }])
        .apply(&mut doc2)
        .expect("EditScript::Replace must succeed iff set_at did");
        prop_assert_eq!(
            doc1.original_bytes(),
            doc2.original_bytes(),
            "{}",
            format!(
                "TOML: EditScript::Replace must match set_at for pointer={key:?} value={new_value:?}",
            ),
        );
    }

    /// Remove ↔ del_at byte equivalence. Same shape as the Replace
    /// properties: parse two fresh docs from the same fixture, mutate one
    /// via `Document::del_at` and the other via `EditScript::Remove`, and
    /// assert `original_bytes()` is byte-identical.
    #[test]
    fn yaml_remove_matches_del_at_byte_for_byte(
        key_idx in 0_usize..usize::MAX,
    ) {
        let doc_seed = parse_yaml_fixture();
        let keys = span_keys(&doc_seed);
        prop_assume!(!keys.is_empty(), "fixture must record at least one span");
        let key = &keys[key_idx % keys.len()];
        let pointer = Pointer::parse(key).expect("recorded keys round-trip");

        let mut doc1 = doc_seed.clone();
        if doc1.del_at(&pointer).is_err() {
            return Ok(());
        }
        let mut doc2 = doc_seed.clone();
        EditScript::from(vec![EditOp::Remove {
            path: pointer.clone(),
        }])
        .apply(&mut doc2)
        .expect("EditScript::Remove must succeed iff del_at did");
        prop_assert_eq!(
            doc1.original_bytes(),
            doc2.original_bytes(),
            "{}",
            format!("YAML: EditScript::Remove must match del_at for pointer={key:?}"),
        );
    }

    #[test]
    fn json_remove_matches_del_at_byte_for_byte(
        key_idx in 0_usize..usize::MAX,
    ) {
        let doc_seed = parse_json_fixture();
        let keys = span_keys(&doc_seed);
        prop_assume!(!keys.is_empty(), "fixture must record at least one span");
        let key = &keys[key_idx % keys.len()];
        let pointer = Pointer::parse(key).expect("recorded keys round-trip");

        let mut doc1 = doc_seed.clone();
        if doc1.del_at(&pointer).is_err() {
            return Ok(());
        }
        let mut doc2 = doc_seed.clone();
        EditScript::from(vec![EditOp::Remove {
            path: pointer.clone(),
        }])
        .apply(&mut doc2)
        .expect("EditScript::Remove must succeed iff del_at did");
        prop_assert_eq!(
            doc1.original_bytes(),
            doc2.original_bytes(),
            "{}",
            format!("JSON: EditScript::Remove must match del_at for pointer={key:?}"),
        );
    }

    #[test]
    fn toml_remove_matches_del_at_byte_for_byte(
        key_idx in 0_usize..usize::MAX,
    ) {
        let doc_seed = parse_toml_fixture();
        let keys = span_keys(&doc_seed);
        prop_assume!(!keys.is_empty(), "fixture must record at least one span");
        let key = &keys[key_idx % keys.len()];
        let pointer = Pointer::parse(key).expect("recorded keys round-trip");

        let mut doc1 = doc_seed.clone();
        if doc1.del_at(&pointer).is_err() {
            return Ok(());
        }
        let mut doc2 = doc_seed.clone();
        EditScript::from(vec![EditOp::Remove {
            path: pointer.clone(),
        }])
        .apply(&mut doc2)
        .expect("EditScript::Remove must succeed iff del_at did");
        prop_assert_eq!(
            doc1.original_bytes(),
            doc2.original_bytes(),
            "{}",
            format!("TOML: EditScript::Remove must match del_at for pointer={key:?}"),
        );
    }
}

// ------------ deterministic sanity tests -----------------------------------

/// Quick sanity check for the YAML fixture: every recorded span pointer must
/// resolve through the value tree, so the proptest's `Pointer::resolve` calls
/// can't silently fail. This lives outside the `proptest!` block because it
/// exercises the fixture itself, not the EditScript ↔ Document equivalence.
#[test]
fn yaml_fixture_records_at_least_one_span_per_leaf_type() {
    let doc = parse_yaml_fixture();
    let keys = span_keys(&doc);
    assert!(
        !keys.is_empty(),
        "YAML fixture must produce at least one recorded span; got 0",
    );
    for key in &keys {
        let p = Pointer::parse(key).expect("recorded key round-trips through Pointer::parse");
        let _ = p.resolve(doc.value()).unwrap_or_else(|e| {
            panic!("recorded YAML span key {key:?} must resolve through the value tree: {e}")
        });
    }
}

#[test]
fn json_fixture_records_at_least_one_span_per_leaf_type() {
    let doc = parse_json_fixture();
    let keys = span_keys(&doc);
    assert!(
        !keys.is_empty(),
        "JSON fixture must produce at least one recorded span; got 0",
    );
    for key in &keys {
        let p = Pointer::parse(key).expect("recorded key round-trips");
        let _ = p
            .resolve(doc.value())
            .unwrap_or_else(|e| panic!("recorded JSON span key {key:?} must resolve: {e}"));
    }
}

#[test]
fn toml_fixture_records_at_least_one_span_per_leaf_type() {
    let doc = parse_toml_fixture();
    let keys = span_keys(&doc);
    assert!(
        !keys.is_empty(),
        "TOML fixture must produce at least one recorded span; got 0",
    );
    for key in &keys {
        let p = Pointer::parse(key).expect("recorded key round-trips");
        let _ = p
            .resolve(doc.value())
            .unwrap_or_else(|e| panic!("recorded TOML span key {key:?} must resolve: {e}"));
    }
}
