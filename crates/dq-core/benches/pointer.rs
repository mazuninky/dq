//! Pointer micro-benchmarks.
//!
//! Three concerns, each parameterised by pointer depth `[1, 4, 16, 64]`:
//!
//! - **`pointer/parse`** — string → [`dq_core::Pointer`].
//! - **`pointer/resolve`** — walk a synthesised nested map.
//! - **`pointer/with_segment`** — repeated calls to
//!   [`dq_core::Pointer::with_segment`], the suspected hot loop at
//!   `crates/dq-core/src/pointer.rs:62` (clones `Vec<Segment>` on every
//!   step).
//!
//! A separate sanity group, **`pointer/canonical_roundtrip`**, asserts
//! that `Pointer::parse(s).as_canonical() == s` for each depth — this is
//! cheap to bench and doubles as a regression sentinel for the canonical
//! escaping path.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use dq_core::{Pointer, Segment, Value};
use indexmap::IndexMap;

const DEPTHS: &[usize] = &[1, 4, 16, 64];

/// Build a pointer string of `depth` segments, each a fixed key
/// (`/x/x/x/…`). Fixed-name keys make the workload identical across
/// depths — only the segment count varies.
fn synth_pointer_string(depth: usize) -> String {
    let mut s = String::with_capacity(depth * 2);
    for _ in 0..depth {
        s.push_str("/x");
    }
    s
}

/// Build a nested map of `depth` `{x: {x: {x: …: 1}}}` levels — the
/// resolve target for the pointer string above.
fn synth_nested_value(depth: usize) -> Value {
    let mut current = Value::Int(1);
    for _ in 0..depth {
        let mut map: IndexMap<String, Value> = IndexMap::new();
        map.insert("x".to_owned(), current);
        current = Value::Map(map);
    }
    current
}

fn bench_pointer_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("pointer/parse");
    for &d in DEPTHS {
        let s = synth_pointer_string(d);
        group.bench_with_input(BenchmarkId::from_parameter(d), &s, |b, s| {
            b.iter(|| {
                let p = Pointer::parse(black_box(s.as_str())).expect("parse pointer");
                black_box(p)
            });
        });
    }
    group.finish();
}

fn bench_pointer_resolve(c: &mut Criterion) {
    let mut group = c.benchmark_group("pointer/resolve");
    for &d in DEPTHS {
        let ptr = Pointer::parse(&synth_pointer_string(d)).expect("parse pointer");
        let value = synth_nested_value(d);
        // Use a tuple input so criterion's setup-phase doesn't repeat
        // the synthesis on every iteration — both inputs are immutable
        // and we want the timed region to be `resolve` only.
        group.bench_with_input(
            BenchmarkId::from_parameter(d),
            &(&ptr, &value),
            |b, &(ptr, value)| {
                b.iter(|| {
                    let v = ptr.resolve(black_box(value)).expect("resolve");
                    black_box(v)
                });
            },
        );
    }
    group.finish();
}

fn bench_pointer_with_segment(c: &mut Criterion) {
    let mut group = c.benchmark_group("pointer/with_segment");
    for &d in DEPTHS {
        // Time the full `0..d` recursion chain — measures the cost of
        // building a `d`-deep pointer by repeated `with_segment` calls,
        // which is what `transform::merge::merge_into` does in the wild.
        group.bench_function(BenchmarkId::from_parameter(d), |b| {
            b.iter(|| {
                let mut p = Pointer::default();
                for _ in 0..d {
                    p = p.with_segment(black_box(Segment::Key("x".to_owned())));
                }
                black_box(p)
            });
        });
    }
    group.finish();
}

fn bench_pointer_recursive_walk(c: &mut Criterion) {
    // Models the push / recurse / emit / pop pattern used by
    // `transform::diff` and `transform::merge_into` — the hot paths that
    // motivated the `push_segment` / `pop_segment` mutation API.
    //
    // Before push/pop, those call sites built each child via `with_segment`
    // (an O(depth) clone-and-push). Summing across a `depth`-deep walk gives
    // an O(depth²) curve, which this bench surfaces as a regression sentinel.
    //
    // After-target: linear in `depth`. If `depth = 64` runs more than 5× the
    // cost of `depth = 16`, treat it as a regression.
    let mut group = c.benchmark_group("pointer/recursive_walk");
    for &d in DEPTHS {
        group.bench_with_input(BenchmarkId::from_parameter(d), &d, |b, &d| {
            b.iter(|| {
                let mut p = Pointer::default();
                // Descent — simulates diff/merge's recursive push at each level.
                for i in 0..d {
                    p.push_segment(Segment::Index(i));
                }
                let canon = black_box(p.as_canonical());
                // Unwind — pairs with push to model the post-recursion pop.
                for _ in 0..d {
                    p.pop_segment();
                }
                canon
            });
        });
    }
    group.finish();
}

fn bench_pointer_canonical_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("pointer/canonical_roundtrip");
    for &d in DEPTHS {
        let s = synth_pointer_string(d);
        group.bench_with_input(BenchmarkId::from_parameter(d), &s, |b, s| {
            b.iter(|| {
                let parsed = Pointer::parse(black_box(s.as_str())).expect("parse");
                let canon = parsed.as_canonical();
                // Sanity check inside the bench loop is cheap and
                // doubles as a regression sentinel — if the canonical
                // renderer ever diverges from the parser, the bench
                // fails loudly rather than silently producing wrong
                // numbers.
                assert_eq!(canon, *s);
                black_box(canon)
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_pointer_parse,
    bench_pointer_resolve,
    bench_pointer_with_segment,
    bench_pointer_recursive_walk,
    bench_pointer_canonical_roundtrip,
);
criterion_main!(benches);
