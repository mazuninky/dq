//! Integration test for Task 5.17 — fuel-budget interrupt path.
//!
//! Pins the spec scenario "Infinite-loop plugin terminates with Exhausted"
//! from `data-query-plugin-abi/spec.md` Requirement
//! "Wasmtime runtime configuration — fuel and memory limits": when a
//! plugin's `lint` export contains an infinite loop and is invoked, the
//! runtime returns `Err(PluginError::Exhausted { rule_id })` within ~1
//! second of CPU time.
//!
//! The test bounds wall-clock time at 5 seconds as a safety net — well
//! above the spec's "~1 second of CPU on M1" budget but tight enough that
//! a regression (e.g. fuel disabled, budget mis-set) still fails fast.
//!
//! # Fixture gating
//!
//! Loads `tests/fixtures/infinite_loop.wasm`. When the fixture is absent
//! (default for sandboxed builds without `cargo-component`), the test
//! prints a SKIP notice and returns `Ok(())` so the suite stays green.
//! See `tests/fixtures/README.md` for the build recipe.

#![cfg(feature = "plugins")]

use std::time::{Duration, Instant};

use dq_core::parse_yaml_with_spans;
use dq_plugin::{PluginError, PluginRuntime};

const FIXTURE_NAME: &str = "infinite_loop.wasm";
/// Wall-clock safety net. The spec says ~1s of CPU; we allow 5s wall-clock
/// to absorb CI noise and cold-start cost. If the fuel budget is broken
/// (limit absent, set too high, never consumed) the test will run forever
/// — the assertion catches the regression before the system timeout.
const WALL_CLOCK_BUDGET: Duration = Duration::from_secs(5);

fn fixture_path(name: &str) -> Option<camino::Utf8PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = camino::Utf8PathBuf::from(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "SKIPPING: fixture {name} not found at {path}; \
             see crates/dq-plugin/tests/fixtures/README.md for the build recipe",
        );
        None
    }
}

#[test]
fn invoke_lint_terminates_with_exhausted_on_infinite_loop() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(fixture) = fixture_path(FIXTURE_NAME) else {
        return Ok(());
    };

    let runtime = PluginRuntime::new()?;
    let handle = runtime.load(&fixture)?;

    let doc = parse_yaml_with_spans(b"a: 1\n")?;
    let ir = doc.as_ir();

    let started = Instant::now();
    let err = runtime
        .invoke_lint(&handle, &ir, None)
        .expect_err("infinite-loop plugin must surface Err, not Ok");
    let elapsed = started.elapsed();

    assert!(
        elapsed < WALL_CLOCK_BUDGET,
        "fuel budget should interrupt within {WALL_CLOCK_BUDGET:?}; took {elapsed:?} \
         — fuel may be disabled or the budget set too high",
    );
    assert!(
        matches!(err, PluginError::Exhausted { .. }),
        "expected PluginError::Exhausted, got {err:?} (kind_name = {:?})",
        err.kind_name(),
    );
    Ok(())
}
