//! CLI integration test for Task 5.19 — `--plugins` without the `plugins`
//! cargo feature must fail with exit code 6 (`InvalidInput`).
//!
//! Pins the spec scenario "--plugins without feature-gate errors" from
//! `data-query-plugin-abi/spec.md` Requirement
//! "CLI loads plugins from `--plugins <DIR>`": when the binary is built
//! without `--features plugins` and the user passes `--plugins ./dir`
//! against a directory that contains at least one `*.wasm` file, the CLI
//! exits with code 6 and stderr contains `"plugins are not enabled in this
//! build"`.
//!
//! This test compiles ONLY in the no-features (default) build so the
//! assertion runs against the feature-disabled exit-code path. With
//! `--features plugins`, the dummy `.wasm` would fail to LOAD with
//! `PluginError::Load` (exit 4) instead — a different code path that lives
//! in the integration tests under `crates/dq-plugin/tests/`. CI runs both
//! configurations; this test runs in the default-features configuration.
//!
//! Spawned through `assert_cmd::Command::cargo_bin("dq")` because the
//! exit-code contract is part of the CLI's external surface and the
//! per-invocation `main`-level mapper (`exit_code_for_error`) is the
//! component under test here. In-process `dq::run` would short-circuit the
//! mapper.

#![cfg(not(feature = "plugins"))]

use assert_cmd::Command;
use predicates::str::contains;
use std::io::Write;
use tempfile::{NamedTempFile, TempDir};

/// Spec scenario "--plugins without feature-gate errors":
/// stderr must include the substring `"plugins are not enabled in this build"`
/// AND the exit code must be 6 (`InvalidInput`).
#[test]
fn lint_with_plugins_dir_errors_when_feature_disabled() {
    // Build a tempdir that contains one *.wasm file. The bytes don't have
    // to be a valid component — discovery only filters by extension, and in
    // the feature-disabled build the runtime never gets a chance to parse
    // the file before the FeatureDisabled short-circuit fires.
    let plugins_dir = TempDir::new().expect("plugins tempdir");
    let plugin_path = plugins_dir.path().join("dummy.wasm");
    std::fs::write(&plugin_path, b"not really a wasm component").expect("write dummy plugin file");

    // A trivial YAML doc to lint against. The lint pipeline never runs —
    // plugin loading errors first — but the positional arg is required by
    // clap so we still need the file to exist.
    let mut yaml_tmp = NamedTempFile::with_suffix(".yaml").expect("yaml tempfile");
    yaml_tmp.write_all(b"a: 1\n").expect("write yaml fixture");
    let yaml_path = yaml_tmp.into_temp_path();

    let plugins_dir_str = plugins_dir.path().to_str().expect("UTF-8 plugins dir path");
    let yaml_path_str = yaml_path.to_str().expect("UTF-8 yaml path");

    let assert = Command::cargo_bin("dq")
        .expect("cargo_bin dq must resolve")
        // Strip developer's environment that could leak into the assertion.
        // `NO_COLOR` is irrelevant here (we don't assert color), but
        // `RUST_LOG` could attach extra log lines to stderr that are fine
        // — we don't assert exact stderr, only that the substring appears.
        .env_remove("NO_COLOR")
        .env_remove("RUST_LOG")
        .arg("lint")
        .arg("--plugins")
        .arg(plugins_dir_str)
        .arg(yaml_path_str)
        .assert()
        .failure();

    let output = assert.get_output();
    let exit_code = output.status.code().unwrap_or_else(|| {
        panic!(
            "expected an exit code (6 == InvalidInput); got signal termination. \
             stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    });
    assert_eq!(
        exit_code,
        6,
        "spec mandates exit code 6 (INVALID_INPUT) for --plugins on a \
         feature-disabled build; got {exit_code}. \
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // The full assertion shape: stderr must mention the canonical phrase
    // from the spec so users know what to do (rebuild with the feature on).
    assert.stderr(contains("plugins are not enabled in this build"));
}
