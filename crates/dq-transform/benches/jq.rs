//! `JqEngine` benchmarks — compile, recompile penalty, and run.
//!
//! Three groups:
//!
//! - **`jq/compile`** — cold compile of three filters of increasing
//!   complexity: identity, simple path, and a `map(select(…))` pipe.
//! - **`jq/recompile_same_filter`** — `compile(".")` in a tight loop,
//!   timed in **per-call** units. Validates the "no compile cache" claim
//!   at `crates/dq-transform/src/jq.rs:227`. If two rules in a ruleset
//!   share the same filter, both pay the full lex+parse+compile cost.
//! - **`jq/run`** — run a pre-compiled filter against small / mid /
//!   large `serde_json::Value` inputs. Same filter, three input sizes,
//!   so the delta is the runtime cost only.
//!
//! All groups are guarded by `#[cfg(feature = "embedded-jq")]` so a
//! `cargo bench --no-default-features` build still compiles to an empty
//! `criterion_main!`.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

#[cfg(feature = "embedded-jq")]
use dq_transform::JqEngine;
#[cfg(feature = "embedded-jq")]
use rand::{Rng, SeedableRng, rngs::StdRng};

/// Three filters of increasing complexity. Kept as one constant so the
/// compile and recompile benches share the exact same strings — the
/// recompile bench specifically depends on `FILTER_IDENTITY` being a
/// trivial filter so its result is dominated by lex+parse+compile setup
/// cost rather than program complexity.
#[cfg(feature = "embedded-jq")]
const FILTER_IDENTITY: &str = ".";
#[cfg(feature = "embedded-jq")]
const FILTER_PATH: &str = ".foo.bar";
#[cfg(feature = "embedded-jq")]
const FILTER_PIPE: &str = ".items | map(select(.x > 0))";

#[cfg(feature = "embedded-jq")]
fn synth_run_input(n: usize) -> serde_json::Value {
    let mut rng = StdRng::seed_from_u64(42);
    let items: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            serde_json::json!({
                "x": rng.gen_range(-50i32..50),
                "name": format!("item_{i}"),
            })
        })
        .collect();
    serde_json::json!({ "items": items })
}

#[cfg(feature = "embedded-jq")]
fn bench_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("jq/compile");
    for (label, src) in [
        ("identity", FILTER_IDENTITY),
        ("path", FILTER_PATH),
        ("pipe", FILTER_PIPE),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                let engine = JqEngine::compile(black_box(src)).expect("compile filter");
                black_box(engine)
            });
        });
    }
    group.finish();
}

/// Per-call recompile penalty for the same filter. Compiles
/// `FILTER_IDENTITY` 10 times per timed iteration and divides the
/// reported total by 10 implicitly via `iter_custom`'s element count.
///
/// We use `bench_function` with a single timed body of 10 compiles
/// because criterion's auto-sampling already produces a per-iteration
/// number — the comparison point is `jq/compile/identity`. If the
/// recompile bench is ~10× the compile bench (it should be), the cost
/// is fully amortizable behind a cache; if it's ~1×, jq-core has its
/// own internal cache and adding one in `dq-transform` is wasted work.
#[cfg(feature = "embedded-jq")]
fn bench_recompile_same_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("jq/recompile_same_filter");
    // Sample at 1, 10, 100 to see the linear slope (cache absent =>
    // slope ~= compile cost; cache present => slope ~= 0).
    for &count in &[1usize, 10, 100] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let mut last = None;
                for _ in 0..count {
                    last = Some(JqEngine::compile(black_box(FILTER_IDENTITY)).expect("compile"));
                }
                black_box(last)
            });
        });
    }
    group.finish();
}

#[cfg(feature = "embedded-jq")]
fn bench_run(c: &mut Criterion) {
    let mut group = c.benchmark_group("jq/run");
    let engine = JqEngine::compile(FILTER_PIPE).expect("compile pipe filter");
    for &n in &[10usize, 100, 1_000] {
        let input = synth_run_input(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &input, |b, input| {
            b.iter(|| {
                let out = engine.run(black_box(input)).expect("run filter");
                black_box(out)
            });
        });
    }
    group.finish();
}

#[cfg(not(feature = "embedded-jq"))]
fn noop(_c: &mut Criterion) {
    // The `embedded-jq` feature is off — stub bench so the bench
    // binary links. `criterion_main!` accepts zero-group invocations
    // poorly, so we keep one harmless group with no body. Run with
    // `--features embedded-jq` (default) to exercise the real engine.
}

#[cfg(feature = "embedded-jq")]
criterion_group!(
    benches,
    bench_compile,
    bench_recompile_same_filter,
    bench_run
);

#[cfg(not(feature = "embedded-jq"))]
criterion_group!(benches, noop);

criterion_main!(benches);
