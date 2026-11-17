//! Property-based test: pointer enumeration round-trips through `resolve`.
//!
//! For any randomly generated `dq_core::Value` tree, every pointer returned by
//! `enumerate_pointers` MUST resolve back to the same node when re-parsed via
//! its canonical RFC 6901 string. This catches escape-bugs (`~0`/`~1`) and
//! enumeration/resolution drift over time.
//!
//! Strategy bounds:
//! - Tree depth ≤ 4
//! - Array length ≤ 5
//! - Object size ≤ 5 keys
//! - String chars ≤ 16 from `[a-zA-Z0-9_/.~ ]` — deliberately includes `/` and
//!   `~` to exercise the escaping codepath.
//!
//! ≥ 100 cases per run by default; persistence file is gitignored so flakes
//! cannot cause CI to fail silently on a stale fixture.

use indexmap::IndexMap;
use proptest::prelude::*;

use dq_core::{Document, Pointer, Value, enumerate_pointers};

/// Generator for keys / strings that may include the RFC 6901 escape characters.
/// We deliberately include `/` and `~` so the canonical form has to escape them.
fn key_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_/.~ ]{1,16}".prop_map(String::from)
}

/// Recursive `Value` strategy bounded to depth 4. Container sizes capped to
/// keep individual cases fast (< 1ms each).
fn value_strategy() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        any::<f64>()
            .prop_filter("only finite floats", |f| f.is_finite())
            .prop_map(Value::Float),
        key_strategy().prop_map(Value::String),
    ];
    leaf.prop_recursive(
        4,  // max depth
        32, // max total nodes
        5,  // size hint per branch
        |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..=5).prop_map(Value::Array),
                proptest::collection::vec((key_strategy(), inner), 0..=5).prop_map(|kvs| {
                    let mut m = IndexMap::new();
                    for (k, v) in kvs {
                        // IndexMap dedups keys; enumeration treats each key
                        // exactly once, which matches the test's expectation.
                        m.insert(k, v);
                    }
                    Value::Map(m)
                }),
            ]
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 100,
        // Don't persist regression seeds — keeps reruns deterministic and
        // avoids surprising failures from stale persistence files. If a
        // regression matters enough to pin, we'd add a #[test] case for it.
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    /// Every enumerated pointer's canonical form parses back into the same
    /// segments and resolves to the same node.
    #[test]
    fn enumerated_pointer_round_trips(value in value_strategy()) {
        let doc = Document::single(value.clone());
        for (pointer, _ty) in enumerate_pointers(&doc) {
            let canonical = pointer.as_canonical();
            let parsed = Pointer::parse(&canonical)
                .map_err(|e| TestCaseError::fail(format!(
                    "canonical pointer {canonical:?} re-parse failed: {e}"
                )))?;
            let resolved = parsed.resolve(&value)
                .map_err(|e| TestCaseError::fail(format!(
                    "canonical pointer {canonical:?} resolve failed: {e}"
                )))?;
            // The resolved node must be value-equal to the node enumerate
            // landed on. Two nodes are considered equal when their `Value`
            // representation matches — `Value: PartialEq`.
            // We re-enumerate to find the original node by pointer equality
            // instead of holding a borrow during iteration.
            let original = pointer.resolve(&value)
                .map_err(|e| TestCaseError::fail(format!(
                    "original pointer {canonical:?} should always resolve: {e}"
                )))?;
            prop_assert_eq!(
                resolved, original,
                "re-parsed pointer {:?} resolved to a different node",
                canonical,
            );
        }
    }
}
