//! Integration test for Task 5.18 — WASI import rejection at load time.
//!
//! Pins the spec scenario "WASI-importing plugin fails to load" from
//! `data-query-plugin-abi/spec.md` Requirement
//! "Wasmtime runtime configuration — fuel and memory limits": when
//! `PluginRuntime::load(path)` is called against a plugin that imports any
//! function from `wasi_snapshot_preview1` (or any `wasi:*` Component-Model
//! interface), the result is `Err(PluginError::DisallowedImport { interface })`
//! where `interface` carries the offending import name.
//!
//! The runtime detects the import name pre-instantiation by walking
//! `Component::component_type().imports()` for any name starting with
//! `wasi:` or `wasi_snapshot_preview1`; the test pins both that detection
//! path AND the surface form of the resulting error variant.
//!
//! # Fixture gating
//!
//! Loads `tests/fixtures/wasi_plugin.wasm`. When the fixture is absent
//! (default for sandboxed builds without `cargo-component`), the test
//! prints a SKIP notice and returns `Ok(())` so the suite stays green.
//! See `tests/fixtures/README.md` for the build recipe.

#![cfg(feature = "plugins")]

use dq_plugin::{PluginError, PluginRuntime};

const FIXTURE_NAME: &str = "wasi_plugin.wasm";

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
fn load_rejects_wasi_importing_plugin() -> Result<(), Box<dyn std::error::Error>> {
    let Some(fixture) = fixture_path(FIXTURE_NAME) else {
        return Ok(());
    };

    let runtime = PluginRuntime::new()?;
    let err = runtime
        .load(&fixture)
        .expect_err("WASI-importing plugin must be rejected at load time");

    assert!(
        matches!(err, PluginError::DisallowedImport { .. }),
        "expected PluginError::DisallowedImport, got {err:?} (kind_name = {:?})",
        err.kind_name(),
    );
    // The interface name is part of the error contract — surface it back to
    // the caller so reporters can mention which import was rejected. The
    // detection regex matches both `wasi:<pkg>/<iface>@<ver>` and the
    // legacy `wasi_snapshot_preview1*` shapes; either should satisfy.
    if let PluginError::DisallowedImport { interface } = &err {
        let lc = interface.to_lowercase();
        assert!(
            lc.contains("wasi:") || lc.contains("wasi_snapshot_preview1"),
            "DisallowedImport.interface should mention `wasi:` or \
             `wasi_snapshot_preview1`; got {interface:?}",
        );
    }
    Ok(())
}
