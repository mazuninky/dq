//! Oracle property test for [`dq_core::diff`].
//!
//! Property: `diff(a, b)` emits exactly the ops the pre-change recursive
//! walk emitted — the same ops, element for element, in the same order —
//! for every pair of values. The walk stopped comparing containers of the
//! same kind with a deep `==` before descending into them; this pins that
//! the change is invisible in the output.
//!
//! The reference is the old algorithm copied verbatim into [`oracle`]. The
//! generator makes `b` mostly a few edits away from `a` (so the pairs share
//! structure and differ in a few places, at any depth), sometimes an
//! independent tree. Its scalars cover every [`Value`] variant, with the
//! values where `==` and [`Value::same_big_number`] disagree with a naive
//! comparison: `NaN`, `0.0` / `-0.0`, big-number literals that spell the
//! same number differently, and big literals equal to a `Float`.
//!
//! Ops are compared by [`same_ops`], which is stricter than `PatchOp`'s
//! derived `PartialEq` in two ways that matter here: a float compares by its
//! bits (a `NaN` in a `Replace` matches the same `NaN`; `0.0` and `-0.0`
//! differ) and a map compares in key order.

use dq_core::pointer::Segment;
use dq_core::{PatchOp, Pointer, Value, diff};
use indexmap::IndexMap;
use proptest::prelude::*;

/// The diff algorithm as it was before the walk stopped deep-comparing
/// containers, kept verbatim as the oracle for `dq_core::diff`. Do not
/// "improve" it: its only job is to be the old behaviour.
mod oracle {
    use dq_core::pointer::{Pointer, Segment};
    use dq_core::{PatchOp, Value};

    pub fn reference_diff(a: &Value, b: &Value) -> Vec<PatchOp> {
        let mut ops = Vec::new();
        // The recursive worker walks via `&mut Pointer` (push/pop) instead of
        // building owned child pointers for each step — that keeps the per-walk
        // cost O(N × depth) rather than O(N × depth²). See the
        // `docs/archive/perf-pointer-recursive-walks.md` for the bench numbers
        // behind this choice.
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
        if a == b || a.same_big_number(b) {
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
}

// ----------------------------- comparison -----------------------------

/// Structural identity: floats by bits, maps in key order, everything else
/// by `==`. Both algorithms clone their op values out of `b`, so identical
/// output is identical to this strictness.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_value(x, y))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((kx, vx), (ky, vy))| kx == ky && same_value(vx, vy))
        }
        _ => a == b,
    }
}

fn same_op(a: &PatchOp, b: &PatchOp) -> bool {
    match (a, b) {
        (
            PatchOp::Add {
                path: pa,
                value: va,
            },
            PatchOp::Add {
                path: pb,
                value: vb,
            },
        )
        | (
            PatchOp::Replace {
                path: pa,
                value: va,
            },
            PatchOp::Replace {
                path: pb,
                value: vb,
            },
        ) => pa == pb && same_value(va, vb),
        (PatchOp::Remove { path: pa }, PatchOp::Remove { path: pb }) => pa == pb,
        _ => false,
    }
}

fn same_ops(a: &[PatchOp], b: &[PatchOp]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_op(a, b))
}

/// `diff(a, b)` against the oracle; returns the ops for further checks.
fn assert_matches_oracle(a: &Value, b: &Value) -> Vec<PatchOp> {
    let got = diff(a, b);
    let want = oracle::reference_diff(a, b);
    assert!(
        same_ops(&got, &want),
        "diff differs from the pre-change algorithm\n a = {a:?}\n b = {b:?}\n diff   = {got:?}\n oracle = {want:?}",
    );
    got
}

// ----------------------------- generators -----------------------------

const BIG_INTS: &[&str] = &[
    "18446744073709551616",
    "+18446744073709551616",
    "18446744073709551617",
    "-9223372036854775809",
];

const BIG_FLOATS: &[&str] = &[
    "1e400",
    "1e+400",
    "1.0e+400",
    "10E399",
    "1e401",
    "1e-400",
    "0.10",
    "0.1000000000000000000000000001",
    ".nan",
    ".inf",
    "-.inf",
];

fn arb_float() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => prop::sample::select(vec![
            0.0,
            -0.0,
            f64::NAN,
            -f64::NAN,
            0.1,
            1.0,
            1.5,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ]),
        1 => any::<f64>(),
    ]
}

/// Every scalar variant, drawn from small domains so independent draws
/// collide often enough to produce equal leaves.
fn arb_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-3_i64..=3).prop_map(Value::Int),
        arb_float().prop_map(Value::Float),
        prop::sample::select(BIG_INTS).prop_map(|s| Value::BigInt(s.to_owned())),
        prop::sample::select(BIG_FLOATS).prop_map(|s| Value::BigFloat(s.to_owned())),
        "[a-c]{0,2}".prop_map(Value::String),
    ]
}

/// Trees up to `depth` levels of containers, at most 4 children each,
/// empty containers included. Map keys come from a tiny alphabet so two
/// trees share keys.
fn arb_tree(depth: u32, size: u32) -> impl Strategy<Value = Value> {
    arb_scalar().prop_recursive(depth, size, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..=4).prop_map(Value::Array),
            prop::collection::vec(("[a-e]", inner), 0..=4)
                .prop_map(|pairs| Value::Map(pairs.into_iter().collect())),
        ]
    })
}

fn arb_value() -> impl Strategy<Value = Value> {
    arb_tree(6, 96)
}

/// The same number spelled another way where one exists: equal under `==`
/// (`0.0` / `-0.0`) or under [`Value::same_big_number`] (big literals). `Int`
/// becomes a `Float` of the same magnitude, which is a change. Anything else
/// is returned as it is.
fn twin(v: &Value) -> Value {
    let big_float = |s: &str| Value::BigFloat(s.to_owned());
    match v {
        Value::Float(f) if f.is_nan() => big_float(".nan"),
        Value::Float(f) if *f == f64::INFINITY => big_float(".inf"),
        Value::Float(f) if *f == f64::NEG_INFINITY => big_float("-.inf"),
        Value::Float(f) if f.to_bits() == 0.1_f64.to_bits() => big_float("0.10"),
        Value::Float(f) if *f == 0.0 => Value::Float(-*f),
        Value::Int(n) => Value::Float(*n as f64),
        Value::BigFloat(s) => match s.as_str() {
            "1e400" => big_float("1e+400"),
            "1e+400" => big_float("1.0e+400"),
            "1.0e+400" => big_float("10E399"),
            "10E399" => big_float("1e400"),
            "0.10" => Value::Float(0.1),
            ".nan" => Value::Float(f64::NAN),
            ".inf" => Value::Float(f64::INFINITY),
            "-.inf" => Value::Float(f64::NEG_INFINITY),
            _ => v.clone(),
        },
        Value::BigInt(s) => match s.strip_prefix('+') {
            Some(rest) => Value::BigInt(rest.to_owned()),
            None if !s.starts_with('-') => Value::BigInt(format!("+{s}")),
            None => v.clone(),
        },
        _ => v.clone(),
    }
}

fn twin_all(v: &mut Value) {
    match v {
        Value::Map(m) => m.values_mut().for_each(twin_all),
        Value::Array(a) => a.iter_mut().for_each(twin_all),
        scalar => *scalar = twin(scalar),
    }
}

/// One edit to the node a [`Mutation`]'s path selects.
#[derive(Debug, Clone)]
enum Edit {
    /// Replace the node with a fresh tree (often a type change).
    Replace(Value),
    /// Respell every scalar of the subtree ([`twin`]).
    Twin,
    /// Reverse the key order of a map, the items of an array.
    Reverse,
    /// Move the first key of a map / item of an array to the end.
    Rotate,
    /// Remove child `n % len`.
    Remove(usize),
    /// Map: insert (or overwrite) a key; array: insert at `n % (len + 1)`;
    /// scalar: replace.
    Insert(usize, String, Value),
    /// Keep the first `n % (len + 1)` children.
    Truncate(usize),
    /// Wrap the node in a one-item array (`true`) or a one-key map.
    Wrap(bool),
    /// Replace a container with its first child (`Null` when empty).
    Unwrap,
}

/// An edit at the node reached by descending into child `c % len` for
/// each `c` of `path`, stopping early at a scalar or an empty container.
#[derive(Debug, Clone)]
struct Mutation {
    path: Vec<usize>,
    edit: Edit,
}

fn arb_edit() -> impl Strategy<Value = Edit> {
    let fresh = || arb_tree(2, 8);
    prop_oneof![
        3 => fresh().prop_map(Edit::Replace),
        3 => Just(Edit::Twin),
        1 => Just(Edit::Reverse),
        1 => Just(Edit::Rotate),
        2 => any::<usize>().prop_map(Edit::Remove),
        2 => (any::<usize>(), "[a-g]", fresh()).prop_map(|(n, k, v)| Edit::Insert(n, k, v)),
        1 => any::<usize>().prop_map(Edit::Truncate),
        1 => any::<bool>().prop_map(Edit::Wrap),
        1 => Just(Edit::Unwrap),
    ]
}

fn arb_mutation() -> impl Strategy<Value = Mutation> {
    (prop::collection::vec(any::<usize>(), 0..=6), arb_edit())
        .prop_map(|(path, edit)| Mutation { path, edit })
}

fn node_at<'a>(v: &'a mut Value, path: &[usize]) -> &'a mut Value {
    let Some((&c, rest)) = path.split_first() else {
        return v;
    };
    let len = match v {
        Value::Map(m) => m.len(),
        Value::Array(a) => a.len(),
        _ => 0,
    };
    if len == 0 {
        return v;
    }
    match v {
        Value::Map(m) => node_at(m.get_index_mut(c % len).expect("in range").1, rest),
        Value::Array(a) => node_at(&mut a[c % len], rest),
        _ => unreachable!("len > 0 only for containers"),
    }
}

fn apply(root: &mut Value, mutation: &Mutation) {
    let node = node_at(root, &mutation.path);
    match (&mutation.edit, &mut *node) {
        (Edit::Replace(v), _) => *node = v.clone(),
        (Edit::Twin, _) => twin_all(node),
        (Edit::Reverse, Value::Map(m)) => m.reverse(),
        (Edit::Reverse, Value::Array(a)) => a.reverse(),
        (Edit::Rotate, Value::Map(m)) if !m.is_empty() => m.move_index(0, m.len() - 1),
        (Edit::Rotate, Value::Array(a)) if !a.is_empty() => a.rotate_left(1),
        (Edit::Remove(n), Value::Map(m)) if !m.is_empty() => {
            m.shift_remove_index(n % m.len());
        }
        (Edit::Remove(n), Value::Array(a)) if !a.is_empty() => {
            a.remove(n % a.len());
        }
        (Edit::Insert(_, k, v), Value::Map(m)) => {
            m.insert(k.clone(), v.clone());
        }
        (Edit::Insert(n, _, v), Value::Array(a)) => a.insert(n % (a.len() + 1), v.clone()),
        (Edit::Insert(_, _, v), _) => *node = v.clone(),
        (Edit::Truncate(n), Value::Map(m)) => m.truncate(n % (m.len() + 1)),
        (Edit::Truncate(n), Value::Array(a)) => a.truncate(n % (a.len() + 1)),
        (Edit::Wrap(as_array), _) => {
            let inner = std::mem::replace(node, Value::Null);
            *node = if *as_array {
                Value::Array(vec![inner])
            } else {
                Value::Map(IndexMap::from([("w".to_owned(), inner)]))
            };
        }
        (Edit::Unwrap, Value::Map(m)) => {
            *node = m.shift_remove_index(0).map_or(Value::Null, |(_, v)| v);
        }
        (Edit::Unwrap, Value::Array(a)) => {
            *node = if a.is_empty() {
                Value::Null
            } else {
                a.swap_remove(0)
            };
        }
        _ => {}
    }
}

/// Mostly `b` = `a` after a few edits; sometimes two independent trees.
fn arb_pair() -> impl Strategy<Value = (Value, Value)> {
    prop_oneof![
        4 => (arb_value(), prop::collection::vec(arb_mutation(), 0..=4)).prop_map(
            |(a, mutations)| {
                let mut b = a.clone();
                for m in &mutations {
                    apply(&mut b, m);
                }
                (a, b)
            }
        ),
        1 => (arb_value(), arb_value()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8192))]

    /// `diff` emits exactly the oracle's ops, in both directions.
    #[test]
    fn diff_matches_pre_change_algorithm((a, b) in arb_pair()) {
        for (x, y) in [(&a, &b), (&b, &a)] {
            let got = diff(x, y);
            let want = oracle::reference_diff(x, y);
            prop_assert!(
                same_ops(&got, &want),
                "diff differs from the pre-change algorithm\n a = {:?}\n b = {:?}\n diff   = {:?}\n oracle = {:?}",
                x, y, got, want,
            );
        }
    }
}

// --------------------------- named edge cases ---------------------------

fn map(pairs: &[(&str, Value)]) -> Value {
    Value::Map(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn arr(items: &[Value]) -> Value {
    Value::Array(items.to_vec())
}

fn big_float(s: &str) -> Value {
    Value::BigFloat(s.to_owned())
}

fn big_int(s: &str) -> Value {
    Value::BigInt(s.to_owned())
}

fn key(k: &str) -> Segment {
    Segment::Key(k.to_owned())
}

fn path(segments: &[Segment]) -> Pointer {
    Pointer::new(segments.to_vec())
}

fn replace(segments: &[Segment], value: Value) -> PatchOp {
    PatchOp::Replace {
        path: path(segments),
        value,
    }
}

fn assert_ops(got: &[PatchOp], want: &[PatchOp]) {
    assert!(same_ops(got, want), "got {got:?}\nwant {want:?}");
}

#[test]
fn reordered_map_keys_emit_nothing_and_changes_follow_b_order() {
    let a = map(&[
        ("a", Value::Int(1)),
        ("b", map(&[("c", Value::Int(2)), ("d", Value::Int(3))])),
    ]);
    let b = map(&[
        ("b", map(&[("d", Value::Int(3)), ("c", Value::Int(2))])),
        ("a", Value::Int(1)),
    ]);
    assert_ops(&assert_matches_oracle(&a, &b), &[]);

    let a = map(&[
        ("a", Value::Int(1)),
        ("b", Value::Int(2)),
        ("c", Value::Int(3)),
    ]);
    let b = map(&[
        ("c", Value::Int(4)),
        ("b", Value::Int(2)),
        ("a", Value::Int(0)),
    ]);
    assert_ops(
        &assert_matches_oracle(&a, &b),
        &[
            replace(&[key("c")], Value::Int(4)),
            replace(&[key("a")], Value::Int(0)),
        ],
    );
}

#[test]
fn arrays_of_different_lengths_emit_tail_ops_after_common_prefix() {
    let a = map(&[(
        "l",
        arr(&[Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)]),
    )]);
    let b = map(&[("l", arr(&[Value::Int(1), Value::Int(9)]))]);
    let l = || key("l");
    assert_ops(
        &assert_matches_oracle(&a, &b),
        &[
            replace(&[l(), Segment::Index(1)], Value::Int(9)),
            PatchOp::Remove {
                path: path(&[l(), Segment::Index(3)]),
            },
            PatchOp::Remove {
                path: path(&[l(), Segment::Index(2)]),
            },
        ],
    );
    assert_ops(
        &assert_matches_oracle(&b, &a),
        &[
            replace(&[l(), Segment::Index(1)], Value::Int(2)),
            PatchOp::Add {
                path: path(&[l(), Segment::Index(2)]),
                value: Value::Int(3),
            },
            PatchOp::Add {
                path: path(&[l(), Segment::Index(3)]),
                value: Value::Int(4),
            },
        ],
    );
}

#[test]
fn empty_containers() {
    let empty_map = || map(&[]);
    let empty_arr = || arr(&[]);
    assert_ops(&assert_matches_oracle(&empty_map(), &empty_map()), &[]);
    assert_ops(&assert_matches_oracle(&empty_arr(), &empty_arr()), &[]);
    assert_ops(
        &assert_matches_oracle(&empty_map(), &empty_arr()),
        &[replace(&[], empty_arr())],
    );
    assert_ops(
        &assert_matches_oracle(&map(&[("a", empty_map())]), &map(&[("a", empty_arr())])),
        &[replace(&[key("a")], empty_arr())],
    );
    assert_ops(
        &assert_matches_oracle(&arr(&[empty_arr()]), &arr(&[arr(&[Value::Null])])),
        &[PatchOp::Add {
            path: path(&[Segment::Index(0), Segment::Index(0)]),
            value: Value::Null,
        }],
    );
    assert_ops(
        &assert_matches_oracle(
            &map(&[("a", map(&[("b", Value::Null)]))]),
            &map(&[("a", empty_map())]),
        ),
        &[PatchOp::Remove {
            path: path(&[key("a"), key("b")]),
        }],
    );
}

#[test]
fn null_against_containers_and_scalars_is_a_replace() {
    assert_ops(
        &assert_matches_oracle(&Value::Null, &map(&[])),
        &[replace(&[], map(&[]))],
    );
    let inner = map(&[("b", Value::Int(1))]);
    assert_ops(
        &assert_matches_oracle(&map(&[("a", Value::Null)]), &map(&[("a", inner.clone())])),
        &[replace(&[key("a")], inner)],
    );
    assert_ops(
        &assert_matches_oracle(&arr(&[arr(&[])]), &arr(&[Value::Null])),
        &[replace(&[Segment::Index(0)], Value::Null)],
    );
    assert_ops(
        &assert_matches_oracle(&arr(&[Value::Null]), &arr(&[Value::Null])),
        &[],
    );
}

#[test]
fn nan_is_never_equal_to_nan() {
    // `==` says NaN != NaN and `same_big_number` wants a big literal, so a
    // NaN leaf is replaced even by itself, at the root and nested.
    let nan = || Value::Float(f64::NAN);
    assert_ops(
        &assert_matches_oracle(&nan(), &nan()),
        &[replace(&[], nan())],
    );
    assert_ops(
        &assert_matches_oracle(&map(&[("x", arr(&[nan()]))]), &map(&[("x", arr(&[nan()]))])),
        &[replace(&[key("x"), Segment::Index(0)], nan())],
    );
}

#[test]
fn nan_literal_equals_float_nan_through_same_big_number() {
    let a = map(&[("x", big_float(".nan")), ("y", big_float("-.inf"))]);
    let b = map(&[
        ("x", Value::Float(f64::NAN)),
        ("y", Value::Float(f64::NEG_INFINITY)),
    ]);
    assert_ops(&assert_matches_oracle(&a, &b), &[]);
    assert_ops(&assert_matches_oracle(&b, &a), &[]);
}

#[test]
fn signed_zeros_are_equal() {
    // `==` on `f64` says `0.0 == -0.0`: no op, at the root and nested.
    assert_ops(
        &assert_matches_oracle(&Value::Float(0.0), &Value::Float(-0.0)),
        &[],
    );
    assert_ops(
        &assert_matches_oracle(
            &map(&[("z", arr(&[Value::Float(-0.0)]))]),
            &map(&[("z", arr(&[Value::Float(0.0)]))]),
        ),
        &[],
    );
}

#[test]
fn big_floats_spelled_differently_are_equal() {
    let a = map(&[("a", arr(&[big_float("1e400"), big_float("10E399")]))]);
    let b = map(&[("a", arr(&[big_float("1e+400"), big_float("1.0e+400")]))]);
    assert_ops(&assert_matches_oracle(&a, &b), &[]);
    assert_ops(
        &assert_matches_oracle(&big_float("1e400"), &big_float("1e+400")),
        &[],
    );
    assert_ops(
        &assert_matches_oracle(
            &map(&[("a", big_float("1e400"))]),
            &map(&[("a", big_float("1e401"))]),
        ),
        &[replace(&[key("a")], big_float("1e401"))],
    );
}

#[test]
fn big_float_equal_to_a_float_is_equal() {
    assert_ops(
        &assert_matches_oracle(
            &map(&[("x", big_float("0.10"))]),
            &map(&[("x", Value::Float(0.1))]),
        ),
        &[],
    );
    let precise = big_float("0.1000000000000000000000000001");
    assert_ops(
        &assert_matches_oracle(
            &map(&[("x", Value::Float(0.1))]),
            &map(&[("x", precise.clone())]),
        ),
        &[replace(&[key("x")], precise)],
    );
}

#[test]
fn big_ints_spelled_differently_are_equal() {
    let a = arr(&[map(&[("n", big_int("18446744073709551616"))])]);
    let b = arr(&[map(&[("n", big_int("+18446744073709551616"))])]);
    assert_ops(&assert_matches_oracle(&a, &b), &[]);
    let c = arr(&[map(&[("n", big_int("18446744073709551617"))])]);
    assert_ops(
        &assert_matches_oracle(&a, &c),
        &[replace(
            &[Segment::Index(0), key("n")],
            big_int("18446744073709551617"),
        )],
    );
}

#[test]
fn int_and_float_of_the_same_magnitude_differ() {
    assert_ops(
        &assert_matches_oracle(&arr(&[Value::Int(1)]), &arr(&[Value::Float(1.0)])),
        &[replace(&[Segment::Index(0)], Value::Float(1.0))],
    );
}

#[test]
fn type_changes_replace_the_whole_subtree() {
    let m = || map(&[("k", arr(&[Value::Int(1)]))]);
    let l = || arr(&[map(&[("k", Value::Int(1))])]);
    let s = || Value::String("k".to_owned());
    // Root: map ↔ array ↔ scalar.
    for (a, b) in [
        (m(), l()),
        (l(), m()),
        (m(), s()),
        (s(), m()),
        (l(), s()),
        (s(), l()),
    ] {
        assert_ops(&assert_matches_oracle(&a, &b), &[replace(&[], b.clone())]);
    }
    // Nested, under a map and under an array, below a shared prefix.
    for (a, b) in [(m(), l()), (l(), s()), (s(), m())] {
        let wa = map(&[
            ("same", Value::Int(0)),
            ("x", arr(&[Value::Null, a.clone()])),
        ]);
        let wb = map(&[
            ("same", Value::Int(0)),
            ("x", arr(&[Value::Null, b.clone()])),
        ]);
        assert_ops(
            &assert_matches_oracle(&wa, &wb),
            &[replace(&[key("x"), Segment::Index(1)], b.clone())],
        );
    }
}
