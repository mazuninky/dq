//! Minimal noop dq plugin — emits one warn-severity demo diagnostic per
//! invocation and an empty (no-op) fix script.
//!
//! This is a documentation-grade reference plugin: it shows the smallest
//! possible implementation of the v0.1.0 plugin ABI. Real plugins consume
//! the imported `ir` / `jq` host interfaces to inspect the document and
//! produce data-driven diagnostics; this one ignores its input and emits a
//! constant payload so it can be used as a smoke-test fixture.
//!
//! # WIT contract
//!
//! See `wit/world.wit` (a mirror of `crates/dq-plugin/wit/dq-plugin.wit`).
//! Authoritative spec: `openspec/changes/add-ir-foundation/specs/
//! data-query-plugin-abi/spec.md`.
//!
//! # Build recipe
//!
//! See `README.md` in this directory. The short form:
//!
//! ```sh
//! rustup target add wasm32-wasip2
//! cargo install cargo-component
//! cargo component build --release
//! ```
//!
//! The artifact lands at
//! `target/wasm32-wasip2/release/dq_plugin_example_noop.wasm` and can be
//! dropped into any directory passed to `dq lint --plugins <DIR>` or
//! `dq fix --plugins <DIR>`.

#![no_std]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

// Pull in the bindings generated from the WIT world. `world: "plugin"`
// matches the `world plugin { ... }` declaration in `wit/world.wit`. The
// `path:` is resolved relative to `CARGO_MANIFEST_DIR`.
//
// The macro emits, among other things:
//   * `Guest` trait — the export surface (`fn lint()` + `fn fix()`).
//   * `exports::*` types for the WIT records (`Diagnostic`, `Severity`).
//   * `export!` macro that wires a user struct's `Guest` impl into the
//     wasm component's exported functions.
wit_bindgen::generate!({
    world: "plugin",
    path: "wit/world.wit",
});

// `Diagnostic` is re-exported at the world's top level because the world
// declares `use types.{diagnostic}`, so the macro lifts it into the same
// namespace as the `Guest` trait — no extra `use` is needed for it.
//
// `Severity` is NOT re-exported by the world (only `diagnostic` is `use`d
// from the `types` interface), so we pull it in via its full interface
// path. The guest bindings emit imported-interface types under
// `dq::plugin::types::*` (matching the WIT package + interface name).
use dq::plugin::types::Severity;

/// Marker struct that owns the plugin's `Guest` trait implementation. Has
/// no state — every invocation produces the same constant output.
struct Component;

impl Guest for Component {
    /// Emit a single warn-severity diagnostic on every invocation. The
    /// `pointer: None` causes the host's diagnostic marshalling to default
    /// `(line, col)` to `(1, 1)`, matching the spec's contract for
    /// pointer-less plugin diagnostics.
    fn lint() -> Vec<Diagnostic> {
        vec![Diagnostic {
            rule_id: "example.demo-lint".to_string(),
            severity: Severity::Warn,
            message: "demo plugin emits this diagnostic on every file".to_string(),
            pointer: None,
        }]
    }

    /// Return an empty JSON Patch array — a no-op fix that always succeeds
    /// idempotently. The host parses these bytes via
    /// `serde_json::from_slice::<EditScript>` and applies the resulting
    /// (empty) script against the document, leaving it byte-identical.
    fn fix() -> Result<Vec<u8>, alloc::string::String> {
        Ok(b"[]".to_vec())
    }
}

// Wire `Component`'s `Guest` impl into the component's exported `lint` /
// `fix` functions. Without this macro invocation the build produces a
// component whose exports are unbound and `wasmtime` rejects it at load
// time.
export!(Component);
