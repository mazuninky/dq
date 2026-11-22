//! Transform-engine benchmarks: [`diff`], [`apply_patch`], [`apply_merge`].
//!
//! Five groups, each targeting one suspected hot path:
//!
//! - **`transform/diff`** — replace-ratio sweep on a 1k-leaf JSON-like
//!   `Value`. Variants `b = a` (no diff), and `b = a` mutated at 1% / 10%
//!   / 50% of leaves. Targets `Step::emit` in
//!   `crates/dq-core/src/transform/diff.rs`, where `PatchOp::Replace`
//!   clones whole subtrees on type mismatch. `ratio_0` is the up-front
//!   `a == b` early return of `diff`.
//! - **`transform/apply_patch`** — runs synth patches of 100 / 1_000 /
//!   10_000 `Replace` ops. Each op pays one `set_at` traversal so this
//!   scales as the *product* of op count and document depth.
//! - **`transform/apply_merge_empty`** — calls
//!   [`apply_merge`] with an empty `{}` patch. Quantifies the entry-clone
//!   in `apply_merge` (`crates/dq-core/src/transform/merge.rs`: the
//!   function clones the full document for zero useful work).
//! - **`transform/diff_deep`** — diffs two 256-leaf trees whose leaves all
//!   sit at depth 2 / 4 / 8 / 16 (same leaf count at every depth, so only
//!   depth varies). The walk is linear in the number of nodes: containers
//!   of the same kind are recursed into without a deep `a == b` first, and
//!   the pointer is one `Pointer` pushed and popped in place. Each emitted
//!   `Replace` clones that pointer, O(depth), so the group grows about 2×
//!   per doubling of depth (1.8× measured).
//! - **`transform/apply_merge_deep`** — calls [`apply_merge`] with a nested
//!   sparse merge patch that reaches a seeded ~10% of the leaves of a
//!   256-leaf tree at depth 2 / 4 / 8 / 16. End-to-end cost, not the walk
//!   alone: every patched leaf is a textual edit that parses the file again,
//!   and the file grows with depth. The "is the target a map" check of
//!   `merge_into` borrows (`is_map_at`): it still walks from the root at
//!   every level, O(depth) each, but clones nothing.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use dq_core::{Document, PatchOp, Pointer, Value, apply_merge, apply_patch, diff};
use indexmap::IndexMap;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use std::hint::black_box;

/// Build a flat map of `n` `{key_i: i}` entries — the unit workload for
/// every transform bench below. Flat shape keeps the per-op cost
/// deterministic and isolates the engine cost from tree traversal cost.
fn synth_flat_map(n: usize) -> Value {
    let mut map: IndexMap<String, Value> = IndexMap::with_capacity(n);
    for i in 0..n {
        map.insert(format!("key_{i}"), Value::Int(i as i64));
    }
    Value::Map(map)
}

/// Parse `bytes` through the JSON format registration so the resulting
/// [`Document`] carries spans + original bytes. `apply_patch` and
/// `apply_merge` both require a write-aware document — see the
/// `WriteUnavailable` early-return in [`Document::set_at`].
fn parse_json_doc(bytes: &[u8]) -> Document {
    let fmt = dq_core::format::by_name("json").expect("json registered");
    fmt.parse(bytes).expect("parse json")
}

/// Mutate `ratio_per_thousand`/1_000 of the entries in a 1_000-entry
/// flat map to `"changed"`. A flat 1k-leaf shape isolates the diff
/// engine's per-leaf branching from any tree-walk cost.
fn synth_diff_pair(ratio_per_thousand: u32) -> (Value, Value) {
    let a = synth_flat_map(1_000);
    let mut b_map = if let Value::Map(m) = a.clone() {
        m
    } else {
        unreachable!("synth_flat_map returns Map")
    };
    let mut rng = StdRng::seed_from_u64(42);
    let count = ratio_per_thousand as usize;
    for _ in 0..count {
        let idx = rng.random_range(0..1_000);
        let key = format!("key_{idx}");
        b_map.insert(key, Value::String("changed".to_owned()));
    }
    (a, Value::Map(b_map))
}

/// Number of keys per level in the two-level base of [`synth_deep_tree`].
const DEEP_FANOUT: usize = 16;

/// Build a tree with up to `DEEP_FANOUT * DEEP_FANOUT` (256) leaves, every
/// leaf at exactly `depth` (>= 2). The first two levels are 16-key maps; each
/// of the 256 paths is then extended with `depth - 2` single-key maps
/// (`"n"`), so the leaf count stays fixed and only the depth varies.
/// `leaf(i)` gives the scalar of the i-th leaf in walk order, or `None` to
/// leave that leaf (and any map left empty by it) out — used for sparse
/// merge patches.
fn synth_deep_tree(depth: usize, leaf: impl Fn(usize) -> Option<Value>) -> Value {
    assert!(depth >= 2, "depth counts the two fan-out levels");
    let mut root: IndexMap<String, Value> = IndexMap::with_capacity(DEEP_FANOUT);
    for i in 0..DEEP_FANOUT {
        let mut mid: IndexMap<String, Value> = IndexMap::with_capacity(DEEP_FANOUT);
        for j in 0..DEEP_FANOUT {
            let Some(mut node) = leaf(i * DEEP_FANOUT + j) else {
                continue;
            };
            for _ in 2..depth {
                let mut single: IndexMap<String, Value> = IndexMap::with_capacity(1);
                single.insert("n".to_owned(), node);
                node = Value::Map(single);
            }
            mid.insert(format!("k{j}"), node);
        }
        if !mid.is_empty() {
            root.insert(format!("k{i}"), Value::Map(mid));
        }
    }
    Value::Map(root)
}

/// Deterministic subset of the 256 leaves that the merge bench changes:
/// about 10%, chosen with a seeded RNG so every depth changes the same ones.
fn synth_changed_leaves() -> Vec<bool> {
    let mut rng = StdRng::seed_from_u64(42);
    (0..DEEP_FANOUT * DEEP_FANOUT)
        .map(|_| rng.random_range(0..10) == 0)
        .collect()
}

fn bench_diff_deep(c: &mut Criterion) {
    let mut group = c.benchmark_group("transform/diff_deep");
    for &depth in &[2usize, 4, 8, 16] {
        let a = synth_deep_tree(depth, |i| Some(Value::Int(i as i64)));
        // Every leaf changes type, so the walk reaches all 256 leaves and
        // emits one `Replace` per leaf.
        let b = synth_deep_tree(depth, |_| Some(Value::String("changed".to_owned())));
        group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &(&a, &b),
            |bencher, &(a, b)| {
                bencher.iter(|| {
                    let ops = diff(black_box(a), black_box(b));
                    black_box(ops)
                });
            },
        );
    }
    group.finish();
}

fn bench_apply_merge_deep(c: &mut Criterion) {
    let mut group = c.benchmark_group("transform/apply_merge_deep");
    let changed = synth_changed_leaves();
    for &depth in &[2usize, 4, 8, 16] {
        let doc_bytes = serde_json::to_vec(&synth_deep_tree(depth, |i| Some(Value::Int(i as i64))))
            .expect("serialize doc");
        let proto_doc = parse_json_doc(&doc_bytes);
        // Sparse patch: only the paths down to the changed leaves, as a
        // real merge patch would be. Every merge-patch leaf is one `set_at`,
        // a textual edit that parses the whole file again; patching all 256
        // leaves takes about 40 ms at depth 2, too slow to sample well.
        let patch = synth_deep_tree(depth, |i| {
            changed[i].then(|| Value::String("changed".to_owned()))
        });
        group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &(&proto_doc, &patch),
            |bencher, &(proto_doc, patch)| {
                bencher.iter_batched(
                    || proto_doc.clone(),
                    |mut doc| {
                        apply_merge(&mut doc, black_box(patch)).expect("apply merge");
                        doc
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_diff(c: &mut Criterion) {
    let mut group = c.benchmark_group("transform/diff");
    // 0 = identity (best case), 10 = 1%, 100 = 10%, 500 = 50%.
    let ratios = [0u32, 10, 100, 500];
    for &r in &ratios {
        let (a, b) = synth_diff_pair(r);
        let label = format!("ratio_{r}_per_1000");
        group.bench_with_input(
            BenchmarkId::from_parameter(&label),
            &(&a, &b),
            |bencher, &(a, b)| {
                bencher.iter(|| {
                    let ops = diff(black_box(a), black_box(b));
                    black_box(ops)
                });
            },
        );
    }
    group.finish();
}

fn bench_apply_patch(c: &mut Criterion) {
    let mut group = c.benchmark_group("transform/apply_patch");
    // Doc-size ladder is bracketed at 1_000 entries — every `set_at`
    // call walks the span map to splice into `original_bytes`, so the
    // cost is *proportional to (op_count × doc_size)*, not just
    // op_count. A 10k-op variant against a 10k-entry doc landed at
    // 22s/iter on the spike, which criterion (rightly) rejects as too
    // slow to sample. We keep the ladder at 10 / 100 / 1_000 and use
    // `iter_batched` to clone the pre-parsed `Document` per iteration
    // rather than re-parsing — the parse cost is already measured by
    // `benches/parse.rs::parse/json` and we don't want it bleeding into
    // these numbers.
    let doc_bytes = serde_json::to_vec(&synth_flat_map(1_000)).expect("serialize doc");
    let proto_doc = parse_json_doc(&doc_bytes);
    for &op_count in &[10usize, 100, 1_000] {
        let ops: Vec<PatchOp> = (0..op_count)
            .map(|i| PatchOp::Replace {
                path: Pointer::parse(&format!("/key_{i}")).expect("parse pointer"),
                value: Value::String("patched".to_owned()),
            })
            .collect();
        group.bench_with_input(
            BenchmarkId::from_parameter(op_count),
            &(&proto_doc, &ops),
            |bencher, &(proto_doc, ops)| {
                bencher.iter_batched(
                    || proto_doc.clone(),
                    |mut doc| {
                        apply_patch(&mut doc, black_box(ops)).expect("apply patch");
                        doc
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_apply_merge_empty(c: &mut Criterion) {
    // 1k-entry doc, empty `{}` patch — measures the entry-clone at
    // `apply_merge` because the recursive merge has zero leaves to
    // visit. Pure setup overhead. Use `iter_batched` so the
    // pre-parsed prototype is cloned per iteration (the parse cost is
    // already measured by `benches/parse.rs::parse/json`).
    let doc_bytes = serde_json::to_vec(&synth_flat_map(1_000)).expect("serialize doc");
    let proto_doc = parse_json_doc(&doc_bytes);
    let empty_patch = Value::Map(IndexMap::new());
    c.bench_function("transform/apply_merge_empty", |bencher| {
        bencher.iter_batched(
            || proto_doc.clone(),
            |mut doc| {
                apply_merge(&mut doc, black_box(&empty_patch)).expect("apply merge");
                doc
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    benches,
    bench_diff,
    bench_apply_patch,
    bench_apply_merge_empty,
    bench_diff_deep,
    bench_apply_merge_deep,
);
criterion_main!(benches);
