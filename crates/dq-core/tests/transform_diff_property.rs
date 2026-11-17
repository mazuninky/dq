//! Round-trip property test for [`dq_core::diff`].
//!
//! Property: for any two `Value`s `a` and `b`, applying `diff(&a, &b)` to a
//! tree carrying `a` produces a tree structurally equal to `b`. This is the
//! contract pinned by the `add-bulk-and-ci` change's
//! `format-support/spec.md` — "diff round-trips".
//!
//! # Why a private value-tree applier?
//!
//! The "obvious" applier is [`dq_core::apply_patch`] against a [`Document`]
//! built via [`Document::value_only`]. That short-circuits today: M2's
//! textual-edit pipeline gates writes on the presence of spans, and
//! `value_only` documents have empty span maps, so every `set_at` /
//! `del_at` call surfaces `Error::WriteUnavailable`. Building a full
//! write-aware document via [`Document::with_spans`] in a property test is
//! impractical — the strategy below generates random tree shapes that don't
//! correspond to any concrete textual source. The full textual-edit round
//! trip is M2's territory and is already covered by
//! `tests/round_trip_property.rs`.
//!
//! What we want to validate here is the **structural correctness of `diff`**:
//! does the engine emit a sequence of ops whose tree-level effect transforms
//! `a` into `b`? That question is independent of byte-level splice machinery,
//! so we apply the ops directly to a `Value` tree via [`apply_ops_to_value`]
//! below. The textual-edit round trip is M2's contract and is exercised by
//! `tests/round_trip_property.rs`.
//!
//! [`Document`]: dq_core::Document
//! [`Document::value_only`]: dq_core::Document::value_only
//! [`Document::with_spans`]: dq_core::Document::with_spans

use dq_core::pointer::Segment;
use dq_core::{PatchOp, Pointer, Value, diff};
use indexmap::IndexMap;
use proptest::prelude::*;

/// Apply a sequence of `Add` / `Remove` / `Replace` ops to a [`Value`] tree.
///
/// `diff` only ever emits these three variants (see `diff.rs` module docs);
/// `Move` / `Copy` / `Test` are intentionally not supported by this helper.
/// If the engine ever starts emitting them this function will panic, which is
/// a desirable signal — extending the helper is cheaper than letting an
/// untested op kind silently slip past the property.
fn apply_ops_to_value(mut value: Value, ops: &[PatchOp]) -> Value {
    for op in ops {
        match op {
            PatchOp::Add { path, value: v } => {
                set_at(&mut value, path.segments(), v.clone());
            }
            PatchOp::Remove { path } => {
                remove_at(&mut value, path.segments());
            }
            PatchOp::Replace { path, value: v } => {
                set_at(&mut value, path.segments(), v.clone());
            }
            other => panic!(
                "apply_ops_to_value only supports Add/Remove/Replace; \
                 diff is not expected to emit: {other:?}",
            ),
        }
    }
    value
}

/// Walk `value` along `segments` and replace the addressed node with `new`.
/// Empty `segments` replaces the root.
fn set_at(value: &mut Value, segments: &[Segment], new: Value) {
    let Some((first, rest)) = segments.split_first() else {
        *value = new;
        return;
    };
    match (value, first) {
        (Value::Map(map), Segment::Key(k)) => {
            if rest.is_empty() {
                // Insert preserves IndexMap order if key exists, appends if new.
                map.insert(k.clone(), new);
            } else {
                let entry = map
                    .get_mut(k)
                    .expect("set_at: key must exist for descent (diff invariant)");
                set_at(entry, rest, new);
            }
        }
        (Value::Array(items), Segment::Index(i)) => {
            if rest.is_empty() {
                if *i == items.len() {
                    items.push(new);
                } else {
                    items[*i] = new;
                }
            } else {
                set_at(&mut items[*i], rest, new);
            }
        }
        (Value::Array(items), Segment::Key(k)) => {
            // diff emits `Segment::Index` for arrays, but be defensive in case
            // a future PatchOp normalization round-trips through string form.
            let idx = k
                .parse::<usize>()
                .expect("Segment::Key on Array must parse as usize");
            if rest.is_empty() {
                if idx == items.len() {
                    items.push(new);
                } else {
                    items[idx] = new;
                }
            } else {
                set_at(&mut items[idx], rest, new);
            }
        }
        (other, seg) => panic!(
            "set_at: cannot descend into {other:?} via segment {seg:?}; \
             diff produced an op the structural applier can't replay",
        ),
    }
}

/// Walk `value` along `segments` and remove the addressed node.
fn remove_at(value: &mut Value, segments: &[Segment]) {
    let Some((first, rest)) = segments.split_first() else {
        // Removing the root is not something `diff` should emit, but if it
        // ever does we collapse to Null as a sensible "empty" value.
        *value = Value::Null;
        return;
    };
    match (value, first) {
        (Value::Map(map), Segment::Key(k)) => {
            if rest.is_empty() {
                // shift_remove preserves IndexMap insertion order for siblings.
                map.shift_remove(k);
            } else {
                let entry = map
                    .get_mut(k)
                    .expect("remove_at: key must exist for descent (diff invariant)");
                remove_at(entry, rest);
            }
        }
        (Value::Array(items), Segment::Index(i)) => {
            if rest.is_empty() {
                items.remove(*i);
            } else {
                remove_at(&mut items[*i], rest);
            }
        }
        (Value::Array(items), Segment::Key(k)) => {
            let idx = k
                .parse::<usize>()
                .expect("Segment::Key on Array must parse as usize");
            if rest.is_empty() {
                items.remove(idx);
            } else {
                remove_at(&mut items[idx], rest);
            }
        }
        (other, seg) => panic!(
            "remove_at: cannot descend into {other:?} via segment {seg:?}; \
             diff produced an op the structural applier can't replay",
        ),
    }
}

// ----------------------------- strategies -----------------------------

/// Tiny scalar strategy: bool, small i64, short ASCII string. Big-int /
/// big-float / float variants are excluded so the round-trip property focuses
/// on structural correctness — the textual-precision round trip is the
/// concern of M2's `tests/round_trip_property.rs`.
fn arb_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-100_i64..=100).prop_map(Value::Int),
        "[a-z]{0,5}".prop_map(Value::String),
    ]
}

/// Recursive strategy bounded by depth ≤ 3 and small fan-out (≤ 4 elements
/// per container). Keeps the suite under a second on the workstation the
/// 30 s cold target was set against.
fn arb_value() -> impl Strategy<Value = Value> {
    arb_scalar().prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            // Arrays of up to 4 children.
            prop::collection::vec(inner.clone(), 0..=4).prop_map(Value::Array),
            // Maps with up to 4 entries; keys are short bare-safe identifiers
            // so the IndexMap can't accidentally end up with duplicate keys
            // collapsing to a single entry.
            prop::collection::vec(("[a-z]{1,3}", inner), 0..=4).prop_map(|pairs| {
                let mut m = IndexMap::new();
                for (k, v) in pairs {
                    m.insert(k, v);
                }
                Value::Map(m)
            }),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// `apply_ops_to_value(a, &diff(a, b)) == b` for every `(a, b)` pair the
    /// recursive strategy generates.
    #[test]
    fn diff_round_trips_via_value_tree_applier(a in arb_value(), b in arb_value()) {
        let ops = diff(&a, &b);
        let result = apply_ops_to_value(a.clone(), &ops);
        prop_assert_eq!(
            &result,
            &b,
            "diff(a, b) applied to a must equal b; ops = {:?}",
            ops,
        );
    }

    /// Self-diff is empty for every generated value. Sharper than the
    /// generic property because it pins the empty-Vec contract directly.
    #[test]
    fn diff_self_is_empty(v in arb_value()) {
        let ops = diff(&v, &v);
        prop_assert!(
            ops.is_empty(),
            "diff(v, v) must be empty; got {} ops",
            ops.len(),
        );
    }
}

// --------------------------- hand-picked sanity ---------------------------

/// A small set of hand-picked pairs run alongside the proptest. These pin
/// concrete shapes the generator might not hit often (cross-type swap,
/// nested type change inside a map, deep array reshape) so a regression
/// in those branches surfaces with a deterministic failure rather than
/// only after a few hundred randomized cases.
#[test]
fn diff_round_trips_on_hand_picked_pairs() {
    fn check(a: Value, b: Value) {
        let ops = diff(&a, &b);
        let result = apply_ops_to_value(a.clone(), &ops);
        assert_eq!(result, b, "ops = {ops:?}; from a = {a:?}");
    }

    // Map shape preserved, scalar swap.
    check(
        {
            let mut m = IndexMap::new();
            m.insert("k".into(), Value::Int(1));
            Value::Map(m)
        },
        {
            let mut m = IndexMap::new();
            m.insert("k".into(), Value::Int(2));
            Value::Map(m)
        },
    );

    // Type change — Map → Array.
    check(
        {
            let mut m = IndexMap::new();
            m.insert("a".into(), Value::Int(1));
            Value::Map(m)
        },
        Value::Array(vec![Value::Int(1)]),
    );

    // Array grow + element type change.
    check(
        Value::Array(vec![Value::Int(1)]),
        Value::Array(vec![Value::Bool(true), Value::String("z".into())]),
    );

    // Deep nested replace.
    check(
        {
            let mut inner = IndexMap::new();
            inner.insert("x".into(), Value::Int(1));
            let mut outer = IndexMap::new();
            outer.insert("spec".into(), Value::Map(inner));
            Value::Map(outer)
        },
        {
            let mut inner = IndexMap::new();
            inner.insert("x".into(), Value::Int(2));
            let mut outer = IndexMap::new();
            outer.insert("spec".into(), Value::Map(inner));
            Value::Map(outer)
        },
    );

    // Null ↔ value (treated as type mismatch, single replace at root).
    check(Value::Null, Value::Int(42));
    check(Value::Int(42), Value::Null);

    // Array shrink.
    check(
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
        Value::Array(vec![Value::Int(1)]),
    );
}

/// Smoke test for the pointer rendering of generated ops. The diff is allowed
/// to use `Segment::Index` for array positions — `as_canonical()` should
/// render them as `/0`, `/1`, etc. (no `/-`).
#[test]
fn diff_array_paths_render_as_concrete_indices() {
    let a = Value::Array(vec![Value::Int(1)]);
    let b = Value::Array(vec![Value::Int(1), Value::Int(2)]);
    let ops = diff(&a, &b);
    assert_eq!(ops.len(), 1);
    let PatchOp::Add { path, .. } = &ops[0] else {
        panic!("expected Add, got {:?}", ops[0]);
    };
    let canonical = path.as_canonical();
    assert_eq!(canonical, "/1");
    // Round-trip through Pointer::parse to confirm the rendered form is
    // accepted by the same parser apply_patch uses.
    let reparsed = Pointer::parse(&canonical).unwrap();
    assert_eq!(reparsed.as_canonical(), canonical);
}
