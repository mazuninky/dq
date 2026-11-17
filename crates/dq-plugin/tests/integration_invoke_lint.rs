//! Integration test for Task 5.15 — `PluginRuntime::invoke_lint` end-to-end
//! against the reference no-op plugin.
//!
//! Pins the spec scenario "Lint plugin returning two diagnostics" from
//! `data-query-plugin-abi/spec.md` Requirement
//! "Plugin invocation surfaces `Diagnostic` and `EditScript` to host". The
//! reference fixture under `examples/plugin-rust/` returns ONE diagnostic
//! per invocation (not two — that variation lives in unit tests for the
//! marshaller); this test pins the marshalling contract end-to-end:
//!
//! - the host returns the diagnostic in invocation order,
//! - `rule_id`, `severity`, and `message` come straight from the WIT record,
//! - `pointer: None` falls back to `(line, col) = (1, 1)` per spec.
//!
//! # Fixture gating
//!
//! The test loads `tests/fixtures/example_noop.wasm`. When the fixture is
//! absent (default for sandboxed builds without `cargo-component`), the
//! test prints a SKIP notice and returns `Ok(())` so the suite stays green.
//! See `tests/fixtures/README.md` for the build recipe.
//!
//! Compiles only with `--features plugins`; without the feature there is no
//! wasmtime runtime to drive.

#![cfg(feature = "plugins")]

use camino::Utf8Path;
use dq_core::parse_yaml_with_spans;
use dq_exec::Severity;
use dq_plugin::PluginRuntime;
use pretty_assertions::assert_eq;

/// Path to the prebuilt component-model artifact for the noop reference
/// plugin. Resolved relative to `CARGO_MANIFEST_DIR` so the test does not
/// depend on the developer's cwd.
const FIXTURE_NAME: &str = "example_noop.wasm";

/// Resolve the absolute path to a fixture under `tests/fixtures/`. Returns
/// `None` (with a SKIP notice already printed) when the fixture is absent.
/// Tests call this and `return Ok(())` on `None`.
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

/// Spec scenario coverage:
/// `data-query-plugin-abi/spec.md` Requirement "Plugin invocation surfaces
/// `Diagnostic` and `EditScript` to host" — the host marshals each WIT
/// `diagnostic` record into a host-side `Diagnostic` preserving `rule-id`,
/// `severity`, `message`, and (when `pointer: None`) defaulting line/col to
/// `(1, 1)`.
#[test]
fn invoke_lint_returns_reference_plugin_diagnostic() -> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = fixture_path(FIXTURE_NAME) else {
        return Ok(());
    };

    let runtime = PluginRuntime::new()?;
    let handle = runtime.load(&fixture)?;

    // A minimal YAML doc so `as_ir()` exposes a real (non-empty) IR. The
    // reference plugin ignores its input — we just need a borrow target.
    let doc = parse_yaml_with_spans(b"a: 1\n")?;
    let ir = doc.as_ir();

    let file = Utf8Path::new("test-input.yaml");
    let diagnostics = runtime.invoke_lint(&handle, &ir, Some(file))?;

    assert_eq!(
        diagnostics.len(),
        1,
        "reference noop plugin emits exactly one diagnostic per invocation; got {} \
         (full payload: {diagnostics:?})",
        diagnostics.len(),
    );

    let diag = &diagnostics[0];
    assert_eq!(
        diag.rule_id, "example.demo-lint",
        "rule_id must round-trip from the WIT record verbatim; got {:?}",
        diag.rule_id,
    );
    assert_eq!(
        diag.severity,
        Severity::Warn,
        "severity Warn from WIT must marshal to host Severity::Warn; got {:?}",
        diag.severity,
    );
    assert_eq!(
        diag.message, "demo plugin emits this diagnostic on every file",
        "message must round-trip verbatim; got {:?}",
        diag.message,
    );
    assert_eq!(
        diag.line, 1,
        "pointer: None must fall back to line 1; got {}",
        diag.line,
    );
    assert_eq!(
        diag.col, 1,
        "pointer: None must fall back to col 1; got {}",
        diag.col,
    );
    // The host wires `file` through unconditionally — used by reporters that
    // need the source-file path.
    assert_eq!(
        diag.file.as_deref(),
        Some(file),
        "file path passed to invoke_lint must propagate to the diagnostic; got {:?}",
        diag.file,
    );

    Ok(())
}
