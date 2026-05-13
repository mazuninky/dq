//! Transform-engine benchmarks: [`diff`], [`apply_patch`], [`apply_merge`].
//!
//! Three groups, each targeting one of the suspected hot paths surfaced by
//! the plan:
//!
//! - **`transform/diff`** — replace-ratio sweep on a 1k-leaf JSON-like
//!   `Value`. Variants `b = a` (no diff), and `b = a` mutated at 1% / 10%
//!   / 50% of leaves. Targets `crates/dq-core/src/transform/diff.rs:78`,
//!   where `PatchOp::Replace` clones whole subtrees on type mismatch.
//! - **`transform/apply_patch`** — runs synth patches of 100 / 1_000 /
//!   10_000 `Replace` ops. Each op pays one `set_at` traversal so this
//!   scales as the *product* of op count and document depth.
//! - **`transform/apply_merge_empty`** — calls
//!   [`apply_merge`] with an empty `{}` patch. Quantifies the entry-clone
//!   at `crates/dq-core/src/transform/merge.rs:38` (the function clones
//!   the full document for zero useful work).

use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use dq_core::{Document, PatchOp, Pointer, Value, apply_merge, apply_patch, diff};
use indexmap::IndexMap;
use rand::{Rng, SeedableRng, rngs::StdRng};

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
        let idx = rng.gen_range(0..1_000);
        let key = format!("key_{idx}");
        b_map.insert(key, Value::String("changed".to_owned()));
    }
    (a, Value::Map(b_map))
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
);
criterion_main!(benches);
