//! Structural diff — produce a minimal RFC 6902 patch that transforms `a` into `b`.
//!
//! `diff(&a, &b)` walks both trees in parallel and emits the smallest set of
//! `add` / `remove` / `replace` ops that, when applied via [`apply_patch`] to a
//! [`Document`] carrying `a`, produce a structurally-equal `b`. The engine
//! intentionally does NOT search for `move` / `copy` opportunities — that
//! search is NP-hard in the general case (longest common subsequence + key
//! similarity scoring) and production diff/merge tools (k8s strategic merge
//! patch, jq diff) likewise skip it. See M3 design decision **D4** for the
//! full rationale.
//!
//! # Minimality rules
//!
//! - Equal subtrees produce no ops.
//! - Type mismatch at any depth (e.g. `Map` vs `Array`, `Null` vs scalar)
//!   produces a single `Replace { path, value: b.clone() }` and the recursion
//!   stops — emitting child ops on top of a parent replace would be redundant.
//! - Map walks: keys present only in `a` produce `Remove`; keys present only
//!   in `b` produce `Add`; common keys recurse with the pointer extended by
//!   the key.
//! - Array walks: index-aligned recursion over `0..min(a.len(), b.len())`,
//!   then a tail. A shrinking array emits `Remove` ops for indices
//!   `b.len()..a.len()` in **reverse order** so the indices remain valid as
//!   the ops apply sequentially. A growing array emits `Add` ops with
//!   concrete numeric indices (NOT the RFC 6902 `/-` append marker — that is
//!   reserved for hand-written patches).
//!
//! # Iteration order
//!
//! Map ops are emitted in this order, deterministic across runs:
//!
//! 1. `Remove` ops for keys missing from `b`, in `a`'s [`IndexMap`] order.
//! 2. `Add` ops for keys missing from `a`, in `b`'s [`IndexMap`] order.
//! 3. Recursion into common keys, in `b`'s order — `b` is the target, so its
//!    order is what wins.
//!
//! [`apply_patch`]: super::apply_patch
//! [`Document`]: crate::Document
//! [`IndexMap`]: indexmap::IndexMap

use super::PatchOp;
use crate::Value;
use crate::pointer::{Pointer, Segment};

/// Produce a minimal RFC 6902 patch transforming `a` into `b`.
///
/// Returns an empty `Vec` (no allocations) when `a == b`. Otherwise walks the
/// two trees recursively and emits `Add` / `Remove` / `Replace` ops; never
/// `Move` / `Copy` / `Test` (see module docs for rationale).
///
/// Applying the returned ops to a [`Document`] carrying `a` via
/// [`apply_patch`] yields a document whose value is structurally equal to
/// `b`. This is the round-trip property pinned by
/// `tests/transform_diff_property.rs`.
///
/// [`Document`]: crate::Document
/// [`apply_patch`]: super::apply_patch
#[must_use]
pub fn diff(a: &Value, b: &Value) -> Vec<PatchOp> {
    let mut ops = Vec::new();
    // The recursive worker walks via `&mut Pointer` (push/pop) instead of
    // building owned child pointers for each step — that keeps the per-walk
    // cost O(N × depth) rather than O(N × depth²). See the
    // `perf-pointer-recursive-walks` change for the bench numbers behind
    // this choice.
    let mut path = Pointer::default();
    diff_into(&mut path, a, b, &mut ops);
    ops
}

/// Recursive worker. `path` is the pointer of the current subtree, mutated
/// in place as the recursion descends and unwinds; `ops` accumulates emitted
/// operations across the whole walk.
///
/// Invariant: every caller of `diff_into` observes `path` in the same state
/// it was in on entry. Internal helpers preserve this by pairing each
/// `push_segment` with a matching `pop_segment`.
fn diff_into(path: &mut Pointer, a: &Value, b: &Value, ops: &mut Vec<PatchOp>) {
    if a == b {
        return;
    }
    match (a, b) {
        (Value::Map(am), Value::Map(bm)) => diff_maps(path, am, bm, ops),
        (Value::Array(av), Value::Array(bv)) => diff_arrays(path, av, bv, ops),
        // Type mismatch (including Null vs anything-non-Null) and scalar
        // inequality: replace the whole subtree at `path`. Child ops would
        // be redundant — D4 minimality rule.
        _ => ops.push(PatchOp::Replace {
            path: path.clone(),
            value: b.clone(),
        }),
    }
}

fn diff_maps(
    path: &mut Pointer,
    am: &indexmap::IndexMap<String, Value>,
    bm: &indexmap::IndexMap<String, Value>,
    ops: &mut Vec<PatchOp>,
) {
    // 1) Removes — keys in `a` but not in `b`. Iteration order: `a`'s
    //    insertion order. Documented at module level.
    for k in am.keys() {
        if !bm.contains_key(k) {
            // Push, clone the fully-extended pointer into the emitted op,
            // then pop. The clone produces the same owned `Pointer` shape
            // the prior `with_segment(...)`-based code produced, so the
            // emitted op is byte-identical.
            path.push_segment(Segment::Key(k.clone()));
            ops.push(PatchOp::Remove { path: path.clone() });
            path.pop_segment();
        }
    }
    // 2) Adds — keys in `b` but not in `a`. Iteration order: `b`'s order
    //    (the target wins).
    for (k, v) in bm {
        if !am.contains_key(k) {
            path.push_segment(Segment::Key(k.clone()));
            ops.push(PatchOp::Add {
                path: path.clone(),
                value: v.clone(),
            });
            path.pop_segment();
        }
    }
    // 3) Common keys — recurse, walking `b` in order so the resulting patch
    //    reflects the target document's key order.
    for (k, bv) in bm {
        if let Some(av) = am.get(k) {
            path.push_segment(Segment::Key(k.clone()));
            diff_into(path, av, bv, ops);
            path.pop_segment();
        }
    }
}

fn diff_arrays(path: &mut Pointer, av: &[Value], bv: &[Value], ops: &mut Vec<PatchOp>) {
    let common = av.len().min(bv.len());
    // Index-aligned recursion over the shared prefix.
    for i in 0..common {
        path.push_segment(Segment::Index(i));
        diff_into(path, &av[i], &bv[i], ops);
        path.pop_segment();
    }
    if av.len() > bv.len() {
        // Shrink: remove tail indices in REVERSE order so each remove keeps
        // the indices of the not-yet-removed elements valid as the patch
        // applies sequentially. E.g. shrink [0,1,2,3] → [0,1] emits
        // remove /3 then remove /2 — both indices stay valid mid-apply.
        for i in (bv.len()..av.len()).rev() {
            path.push_segment(Segment::Index(i));
            ops.push(PatchOp::Remove { path: path.clone() });
            path.pop_segment();
        }
    } else if bv.len() > av.len() {
        // Grow: add tail indices in forward order. Concrete numeric indices,
        // never `/-` — `/-` is RFC 6902 syntactic sugar for hand-written
        // patches; diff produces explicit positions for determinism.
        for (i, value) in bv.iter().enumerate().skip(av.len()) {
            path.push_segment(Segment::Index(i));
            ops.push(PatchOp::Add {
                path: path.clone(),
                value: value.clone(),
            });
            path.pop_segment();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

    fn map_of<I>(pairs: I) -> Value
    where
        I: IntoIterator<Item = (&'static str, Value)>,
    {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_owned(), v);
        }
        Value::Map(m)
    }

    #[test]
    fn equal_documents_produce_empty_patch() {
        let a = map_of([("x", Value::Int(1)), ("y", Value::String("z".into()))]);
        let b = a.clone();
        assert_eq!(diff(&a, &b), Vec::<PatchOp>::new());
    }

    #[test]
    fn nested_scalar_change_emits_one_replace_at_deep_path() {
        // Mirror the design's motivating `/spec/replicas` example.
        let a = map_of([(
            "spec",
            map_of([("replicas", Value::Int(1)), ("port", Value::Int(8080))]),
        )]);
        let b = map_of([(
            "spec",
            map_of([("replicas", Value::Int(3)), ("port", Value::Int(8080))]),
        )]);
        let ops = diff(&a, &b);
        assert_eq!(
            ops,
            vec![PatchOp::Replace {
                path: Pointer::parse("/spec/replicas").unwrap(),
                value: Value::Int(3),
            }]
        );
    }

    #[test]
    fn type_change_at_root_emits_one_root_replace_with_no_child_ops() {
        // Map → Array at root. The minimality rule forbids emitting child
        // ops on top of a root replace.
        let a = map_of([("x", Value::Int(1))]);
        let b = Value::Array(vec![Value::Int(1)]);
        let ops = diff(&a, &b);
        assert_eq!(
            ops,
            vec![PatchOp::Replace {
                path: Pointer::default(),
                value: b.clone(),
            }]
        );
        assert_eq!(ops.len(), 1, "type change must NOT recurse into children");
    }

    #[test]
    fn map_key_removed_and_added_emits_remove_then_add() {
        // Determinism: removes precede adds in our chosen ordering. The test
        // pins the choice so a refactor that re-orders these doesn't quietly
        // change the wire output.
        let a = map_of([("removed", Value::Int(1))]);
        let b = map_of([("added", Value::Int(2))]);
        let ops = diff(&a, &b);
        assert_eq!(ops.len(), 2);
        assert_eq!(
            ops[0],
            PatchOp::Remove {
                path: Pointer::parse("/removed").unwrap(),
            }
        );
        assert_eq!(
            ops[1],
            PatchOp::Add {
                path: Pointer::parse("/added").unwrap(),
                value: Value::Int(2),
            }
        );
    }

    #[test]
    fn array_grow_emits_add_ops_with_concrete_numeric_indices() {
        // No `/-` in diff output — the produced path must be the concrete
        // index, e.g. `/1`, `/2`. The `as_canonical` form is the contract
        // the wire format pins.
        let a = Value::Array(vec![Value::Int(1)]);
        let b = Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let ops = diff(&a, &b);
        assert_eq!(ops.len(), 2);
        assert_eq!(
            ops[0],
            PatchOp::Add {
                path: Pointer::new(vec![Segment::Index(1)]),
                value: Value::Int(2),
            }
        );
        assert_eq!(
            ops[1],
            PatchOp::Add {
                path: Pointer::new(vec![Segment::Index(2)]),
                value: Value::Int(3),
            }
        );
        // Sanity: paths render without `/-`.
        for op in &ops {
            if let PatchOp::Add { path, .. } = op {
                let canonical = path.as_canonical();
                assert!(
                    !canonical.ends_with("/-"),
                    "diff must not emit /- (RFC 6902 append) — got {canonical}",
                );
            }
        }
    }

    #[test]
    fn array_shrink_emits_remove_ops_in_reverse_index_order() {
        // Reverse-order removes keep the surviving indices valid as the patch
        // applies sequentially. Forward order would invalidate indices after
        // the first removal.
        let a = Value::Array(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]);
        let b = Value::Array(vec![Value::Int(0), Value::Int(1)]);
        let ops = diff(&a, &b);
        assert_eq!(ops.len(), 2);
        // Highest index first.
        assert_eq!(
            ops[0],
            PatchOp::Remove {
                path: Pointer::new(vec![Segment::Index(3)]),
            }
        );
        assert_eq!(
            ops[1],
            PatchOp::Remove {
                path: Pointer::new(vec![Segment::Index(2)]),
            }
        );
    }
}
