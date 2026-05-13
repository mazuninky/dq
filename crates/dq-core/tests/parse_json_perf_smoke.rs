//! Performance regression smoke tests for the JSON parser.
//!
//! Pins the perf-json-parse-linear OpenSpec contract: parsing a 10 000-element
//! flat JSON array (the pathological "compact single-line" shape) completes in
//! linear time, not the quadratic time the pre-fix `compute_line_range` /
//! `compute_indent` helpers exhibited (~14 s on M1 Pro release).
//!
//! Design target after the fix is ~250 ms; the assertion threshold is 1.0 s
//! (~3× headroom) so the test stays reliable on slow CI runners while still
//! firing immediately on an O(n²) regression. The pretty-printed variant
//! never exhibited the quadratic behaviour — it serves as the "easy case"
//! sanity check and should pass both before and after the fix.
//!
//! Run via:
//!
//! ```sh
//! cargo test -p dq-core --test parse_json_perf_smoke
//! ```
//!
//! These tests run under the default `dev` profile, which is intentional —
//! the bench harness ([`crates/dq-core/benches/parse.rs`]) is the right tool
//! for tight wall-time numbers; this file's job is regression detection,
//! not benchmarking.

use std::time::{Duration, Instant};

use dq_core::format::by_name;

/// Run `body` and assert it completes within `budget`. Panics with a clear
/// message naming the actual elapsed wall-time, intended to point a future
/// maintainer at the likely root cause (the JSON span-map builder going
/// quadratic again).
#[track_caller]
fn assert_under<F>(label: &str, budget: Duration, body: F)
where
    F: FnOnce(),
{
    let started = Instant::now();
    body();
    let elapsed = started.elapsed();
    assert!(
        elapsed < budget,
        "JSON parse of {label} took {elapsed:?}, expected < {budget:?} \
         — likely O(n²) regression in parsers/json.rs span builder",
    );
}

/// Compact single-line JSON: `[0,1,2,...,9999]`. This is the pathological
/// shape that exposed the quadratic backward-scan in the pre-fix helpers
/// (~14 s on a 10k-element array). Post-fix the design target is ~250 ms;
/// 1.0 s is a deliberately generous regression threshold to avoid CI
/// flakes — anything beyond that is squarely back in O(n²) territory.
#[test]
fn parses_10k_element_flat_array_under_1s() {
    let values: Vec<u32> = (0..10_000).collect();
    let bytes = serde_json::to_vec(&values).expect("serde_json::to_vec of Vec<u32> is infallible");
    let json = by_name("json").expect("json format must be registered");
    assert_under(
        "10k-element flat array (compact)",
        Duration::from_secs(1),
        || {
            let _doc = json.parse(&bytes).expect("compact JSON array must parse");
        },
    );
}

/// Multi-line pretty-printed equivalent. Each scalar lives on its own line,
/// so the pre-fix backward-scan never went more than a few bytes — this case
/// was already fast. Kept as a co-located guard against unrelated regressions
/// in the parser's hot path that would degrade both variants.
#[test]
fn parses_10k_element_pretty_array_under_1s() {
    let values: Vec<u32> = (0..10_000).collect();
    let bytes = serde_json::to_vec_pretty(&values)
        .expect("serde_json::to_vec_pretty of Vec<u32> is infallible");
    let json = by_name("json").expect("json format must be registered");
    assert_under(
        "10k-element flat array (pretty)",
        Duration::from_secs(1),
        || {
            let _doc = json.parse(&bytes).expect("pretty JSON array must parse");
        },
    );
}
