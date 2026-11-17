//! `dq-plugin` — WASM plugin runtime for the `dq` CLI.
//!
//! This crate hosts third-party `lint` / `fix` plugins compiled to the
//! WebAssembly Component Model and described by the WIT package
//! `dq:plugin@0.1.0` (see `wit/dq-plugin.wit` for the full schema and
//! `openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md`
//! for the ABI contract).
//!
//! # Feature gating
//!
//! The wasmtime runtime is only linked when the `plugins` cargo feature
//! is enabled:
//!
//! ```toml
//! [features]
//! default = []
//! plugins = ["dep:wasmtime", "dep:wit-bindgen"]
//! ```
//!
//! Without the feature, every entry point returns
//! [`PluginError::FeatureDisabled`] so the CLI can link `dq-plugin`
//! unconditionally and surface a clean user-facing error instead of
//! failing to link.
//!
//! # Public surface
//!
//! - [`PluginRuntime`] — owns the wasmtime engine and per-invocation
//!   sandbox configuration.
//! - [`PluginHandle`] — handle to a single loaded `*.wasm` plugin.
//! - [`PluginError`] / [`Result`] — `thiserror`-based error enum with a
//!   stable [`PluginError::kind_name`] for exit-code routing.
//!
//! The current scaffolding exposes the API surface only — load / invoke
//! method bodies are filled in by `add-ir-foundation` Phase 5 tasks
//! 5.4–5.8.

pub mod error;
pub mod runtime;

pub use error::{PluginError, Result};
pub use runtime::{PluginHandle, PluginRuntime};
