//! Integration test for Task 5.16 — `PluginRuntime::invoke_fix` end-to-end.
//!
//! Pins the spec scenario "Plugin invocation surfaces `Diagnostic` and
//! `EditScript` to host" (fix path) from
//! `data-query-plugin-abi/spec.md`. The reference noop plugin returns
//! `b"[]"` from its `fix()` export — an empty JSON Patch array — and the
//! host parses these bytes via `serde_json::from_slice::<EditScript>` into
//! an empty (no-op) `EditScript`.
//!
//! A second test exercises the malformed-payload path:
//! `data-query-plugin-abi/spec.md` scenario "Fix plugin returning malformed
//! JSON" — when the plugin's `fix()` returns non-JSON bytes, the runtime
//! surfaces `PluginError::MalformedFix` whose `source` chains the
//! `serde_json::Error`. That test depends on a `malformed_fix.wasm`
//! fixture that is not built by default; it skips with a SKIP notice when
//! absent (NOT `#[ignore]`d, so populating the fixture exercises it
//! automatically without `--ignored`).
//!
//! Compiles only with `--features plugins`; without the feature there is
//! no wasmtime runtime to drive.

#![cfg(feature = "plugins")]

use dq_core::parse_yaml_with_spans;
use dq_plugin::{PluginError, PluginRuntime};

const NOOP_FIXTURE: &str = "example_noop.wasm";
const MALFORMED_FIXTURE: &str = "malformed_fix.wasm";

/// Resolve the absolute path to a fixture under `tests/fixtures/`. Returns
/// `None` (with a SKIP notice already printed) when the fixture is absent.
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

/// Spec scenario coverage: the host runs the plugin's `fix` export, parses
/// the returned bytes as a JSON Patch via `serde_json::from_slice::
/// <EditScript>`, and returns the parsed script. The reference plugin
/// returns `b"[]"` so the resulting `EditScript` is the empty (no-op)
/// script.
#[test]
fn invoke_fix_returns_empty_edit_script_for_reference_plugin()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = fixture_path(NOOP_FIXTURE) else {
        return Ok(());
    };

    let runtime = PluginRuntime::new()?;
    let handle = runtime.load(&fixture)?;

    let doc = parse_yaml_with_spans(b"a: 1\n")?;
    let ir = doc.as_ir();

    let script = runtime.invoke_fix(&handle, &ir)?;

    assert!(
        script.is_noop(),
        "reference noop plugin returns `b\"[]\"`, which parses to a no-op \
         EditScript; got non-empty script with {} op(s)",
        script.len(),
    );
    Ok(())
}

/// Spec scenario coverage: "Fix plugin returning malformed JSON" — when the
/// plugin's `fix` export returns `b"not json"` bytes, `invoke_fix` returns
/// `Err(PluginError::MalformedFix { rule_id, source })` whose `source`
/// chains a `serde_json::Error`.
///
/// Skipped (with SKIP notice, not `#[ignore]`) when the fixture is absent —
/// see `tests/fixtures/README.md` for the build recipe.
#[test]
fn invoke_fix_surfaces_malformed_fix_error_for_non_json_payload()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = fixture_path(MALFORMED_FIXTURE) else {
        return Ok(());
    };

    let runtime = PluginRuntime::new()?;
    let handle = runtime.load(&fixture)?;

    let doc = parse_yaml_with_spans(b"a: 1\n")?;
    let ir = doc.as_ir();

    let err = runtime
        .invoke_fix(&handle, &ir)
        .expect_err("malformed fix payload must surface as Err(MalformedFix), not Ok");

    assert!(
        matches!(err, PluginError::MalformedFix { .. }),
        "expected PluginError::MalformedFix, got {err:?} (kind_name = {:?})",
        err.kind_name(),
    );
    // Spec contract: the underlying `serde_json::Error` is chained via
    // `#[source]` so reporters that walk `error::source()` see it.
    let source: &dyn std::error::Error = &err;
    assert!(
        source.source().is_some(),
        "MalformedFix must chain the serde_json::Error via #[source]; \
         no source on {err:?}",
    );
    Ok(())
}
