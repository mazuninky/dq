//! Plugin discovery and loading helpers for the `--plugins <DIR>` flag.
//!
//! Owns the bridging logic between the CLI's `--plugins` argument and the
//! `dq-plugin` crate's `PluginRuntime`. Discovery walks `<DIR>` non-recursively
//! for `*.wasm` files in lexical order by file name; each is loaded into a
//! shared [`PluginRuntime`] and exposed to the lint and fix pipelines through
//! a single [`LoadedPlugins`] handle.
//!
//! ## Feature gate behaviour
//!
//! `dq-plugin` is linked unconditionally; its `plugins` feature gates the
//! wasmtime backend. When the feature is off, [`PluginRuntime::new`] returns
//! [`dq_plugin::PluginError::FeatureDisabled`]. To keep the flag usable as a
//! probe ("does this directory contain plugins?") we treat the
//! disabled-runtime / empty-discovery combination as a silent no-op. Only
//! when discovery actually finds at least one `*.wasm` file AND the runtime
//! is feature-disabled do we surface
//! [`crate::error::InvalidInput`] (exit 6) with the spec-mandated substring
//! `"plugins are not enabled in this build"`.

use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use dq_plugin::{PluginError, PluginHandle, PluginRuntime};

use crate::error::InvalidInput;

/// Result of loading every plugin under a directory passed via `--plugins`.
///
/// Holds the shared runtime in an `Arc` so the lint and fix pipelines can
/// share it across rayon workers without cloning the underlying wasmtime
/// engine. The handles are owned by this struct and live for the duration of
/// the CLI invocation.
///
/// The `runtime` is `Option<Arc<...>>` to accommodate the empty-discovery /
/// feature-disabled combination: when the user passes `--plugins <DIR>`
/// against a directory that contains no `*.wasm` files AND the binary was
/// built without the `plugins` feature, we cannot construct a runtime
/// (the stub's `new()` errors with `FeatureDisabled`) but the lint / fix
/// pipelines should proceed silently. In that case `runtime` is `None`
/// and `handles` is empty; callers iterate `handles` so no plugin
/// invocation is attempted.
#[derive(Debug)]
pub(crate) struct LoadedPlugins {
    /// The shared runtime. `Arc` so callers can clone cheaply across worker
    /// threads. `None` only in the no-op feature-disabled / empty-discovery
    /// case described above; non-empty `handles` always pairs with `Some`.
    pub(crate) runtime: Option<Arc<PluginRuntime>>,
    /// One handle per loaded `*.wasm` file, in lexical order by file name.
    pub(crate) handles: Vec<PluginHandle>,
}

/// Walk `dir` non-recursively, return every `*.wasm` file in lexical order.
///
/// # Errors
///
/// Returns [`InvalidInput`] (exit 6) when `dir` does not exist or cannot be
/// read. The flag value is user-supplied, so a bad path is a caller-side
/// input error rather than a runtime failure.
pub(crate) fn discover_plugins(dir: &Utf8Path) -> anyhow::Result<Vec<Utf8PathBuf>> {
    if !dir.as_std_path().exists() {
        return Err(anyhow::Error::new(InvalidInput::new(format!(
            "--plugins directory {dir} does not exist"
        ))));
    }
    let read = std::fs::read_dir(dir.as_std_path()).map_err(|e| {
        anyhow::Error::new(InvalidInput::new(format!(
            "--plugins directory {dir} is not readable: {e}"
        )))
    })?;
    let mut wasm_files: Vec<Utf8PathBuf> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| {
            anyhow::Error::new(InvalidInput::new(format!(
                "--plugins directory {dir} read entry failed: {e}"
            )))
        })?;
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = match Utf8PathBuf::from_path_buf(entry.path()) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if path.extension() == Some("wasm") {
            wasm_files.push(path);
        }
    }
    // Lexical sort by file name to give every CLI invocation the same load
    // order — important so plugin diagnostics appear in a stable sequence.
    wasm_files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(wasm_files)
}

/// Discover and load every `*.wasm` plugin under `dir`.
///
/// Pipeline:
///
/// 1. Discover candidate files via [`discover_plugins`].
/// 2. Build a [`PluginRuntime`]. If the runtime returns
///    [`PluginError::FeatureDisabled`] AND discovery found ≥ 1 file, surface
///    a structured [`InvalidInput`] with the spec-mandated substring
///    `"plugins are not enabled in this build"`. If discovery found 0 files,
///    return an empty [`LoadedPlugins`] with no runtime work attempted —
///    the flag is treated as a no-op probe.
/// 3. Load each candidate into the runtime, in lexical order.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when `dir` does not exist or when the runtime
///   is feature-disabled and at least one `*.wasm` file would be loaded.
/// - Any [`PluginError`] surfaced by [`PluginRuntime::load`] (parse, link,
///   schema-version, disallowed-import) is propagated unchanged so the
///   exit-code mapper can route via [`PluginError::kind_name`].
pub(crate) fn load_all(dir: &Utf8Path) -> anyhow::Result<LoadedPlugins> {
    let candidates = discover_plugins(dir)?;
    let runtime = match PluginRuntime::new() {
        Ok(rt) => rt,
        Err(PluginError::FeatureDisabled { .. }) => {
            if candidates.is_empty() {
                // No plugins to load — `--plugins` is a no-op probe. Return
                // an empty `LoadedPlugins` with no runtime; callers iterate
                // `handles` and skip every plugin invocation.
                return Ok(LoadedPlugins {
                    runtime: None,
                    handles: Vec::new(),
                });
            }
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "plugins are not enabled in this build; rebuild dq-cli with `--features plugins` to load {} plugin(s) from {dir}",
                candidates.len(),
            ))));
        }
        Err(other) => return Err(anyhow::Error::new(other)),
    };
    let mut handles: Vec<PluginHandle> = Vec::with_capacity(candidates.len());
    for path in &candidates {
        let handle = runtime.load(path).map_err(anyhow::Error::new)?;
        handles.push(handle);
    }
    Ok(LoadedPlugins {
        runtime: Some(Arc::new(runtime)),
        handles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use std::fs;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    fn utf8(path: &std::path::Path) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(path.to_path_buf()).expect("UTF-8 tempdir path")
    }

    #[test]
    fn discover_returns_invalid_input_for_missing_dir() {
        let err = discover_plugins(Utf8Path::new("/no/such/plugins/dir"))
            .expect_err("missing dir must error");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "missing --plugins dir must produce InvalidInput, got: {err:?}",
        );
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn discover_returns_empty_for_empty_dir() {
        let dir = tempdir();
        let got = discover_plugins(&utf8(dir.path())).expect("empty dir is fine");
        assert!(got.is_empty(), "expected no plugins, got: {got:?}");
    }

    #[test]
    fn discover_filters_to_wasm_files_only() {
        let dir = tempdir();
        fs::write(dir.path().join("a.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
        fs::write(dir.path().join("b.txt"), b"not a plugin").unwrap();
        fs::write(dir.path().join("c.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
        let got = discover_plugins(&utf8(dir.path())).expect("discovery succeeds");
        let names: Vec<&str> = got.iter().filter_map(|p| p.file_name()).collect();
        assert_eq!(
            names,
            vec!["a.wasm", "c.wasm"],
            "only wasm files in lexical order, got: {names:?}",
        );
    }

    #[test]
    fn discover_orders_lexically_by_file_name() {
        let dir = tempdir();
        fs::write(dir.path().join("c.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
        fs::write(dir.path().join("a.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
        fs::write(dir.path().join("b.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
        let got = discover_plugins(&utf8(dir.path())).expect("discovery succeeds");
        let names: Vec<&str> = got.iter().filter_map(|p| p.file_name()).collect();
        assert_eq!(names, vec!["a.wasm", "b.wasm", "c.wasm"]);
    }

    #[test]
    fn load_all_empty_dir_is_no_op_even_when_feature_disabled() {
        // Empty `--plugins` directory is treated as a probe — succeed
        // silently regardless of whether the `plugins` feature is on. This
        // matches the spec: only a non-empty `*.wasm` discovery against a
        // feature-disabled binary triggers the InvalidInput rejection.
        let dir = tempdir();
        let loaded = load_all(&utf8(dir.path())).expect("empty dir must succeed");
        assert!(loaded.handles.is_empty(), "no handles for empty dir");
    }

    #[test]
    fn load_all_with_wasm_files_errors_invalid_input_when_feature_off() {
        // Probe whether `PluginRuntime::new()` is feature-disabled at
        // runtime — this lets the test stay valid regardless of which crate
        // (`dq-cli` or `dq-plugin`) carries the `plugins` feature flag at
        // build time. When the runtime IS available (feature-on), there is
        // no useful assertion to make against an arbitrary `*.wasm` byte
        // sequence; skip the test in that case. The 5.19 CLI integration
        // test asserts the same contract end-to-end against a binary built
        // explicitly without `--features plugins`.
        let runtime_disabled = matches!(
            PluginRuntime::new(),
            Err(PluginError::FeatureDisabled { .. })
        );
        if !runtime_disabled {
            return;
        }
        // The exact substring the 5.19 contract test asserts on. Don't
        // refactor the message without bumping that test.
        let dir = tempdir();
        fs::write(dir.path().join("plugin.wasm"), b"\0asm\x01\x00\x00\x00").unwrap();
        let err = load_all(&utf8(dir.path()))
            .expect_err("non-empty discovery against disabled runtime must error");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "must carry InvalidInput so exit-code mapper picks 6, got: {err:?}",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("plugins are not enabled in this build"),
            "stderr must contain the spec-mandated substring, got: {msg}",
        );
    }
}
