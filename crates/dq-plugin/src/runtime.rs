//! WASM plugin runtime entry points.
//!
//! This module compiles in two flavors selected by the `plugins` cargo
//! feature:
//!
//! * **Feature off** — every entry point returns
//!   [`crate::PluginError::FeatureDisabled`] so the CLI can link
//!   `dq-plugin` unconditionally and produce a deterministic, user-facing
//!   error rather than a link-time failure when the feature is disabled.
//!   No `wasmtime` symbols are linked in this configuration.
//! * **Feature on** — host-side bindings are generated from
//!   `wit/dq-plugin.wit` via `wasmtime::component::bindgen!`, and the
//!   public API exposes a real [`PluginRuntime`] backed by wasmtime.
//!
//! Both flavors expose the same public types ([`PluginRuntime`],
//! [`PluginHandle`]) and method signatures so callers (the CLI's lint /
//! fix pipelines) can be feature-agnostic.

#[cfg(feature = "plugins")]
mod plugins_enabled {
    //! Real wasmtime-backed runtime.
    //!
    //! # Wasmtime configuration rationale
    //!
    //! - `consume_fuel(true)` — every plugin invocation receives a fixed
    //!   fuel budget (`FUEL_BUDGET = 100_000_000`, ~1s of CPU on M1).
    //!   Exhaustion surfaces as `Trap::OutOfFuel`, mapped to
    //!   [`PluginError::Exhausted`].
    //! - `max_wasm_stack(2 * 1024 * 1024)` — 2 MiB stack matches the spec.
    //! - `wasm_component_model(true)` — required for the
    //!   `wasmtime::component::bindgen!`-generated bindings.
    //! - **No WASI**: the runtime intentionally does not link
    //!   `wasmtime_wasi`. Plugins importing `wasi:*` interfaces fail to
    //!   load with [`PluginError::DisallowedImport`]. Plugins are
    //!   sandboxed: no filesystem, no network, no process control.
    //! - Async disabled — the spec is sync-only; plugin invocation
    //!   blocks the calling thread.
    //! - `wasm_custom_page_sizes(false)`, `wasm_memory64(false)` — pinned
    //!   off (they are off by default) because the store-wide memory
    //!   accounting below relies on them; see [`crate::limits`].
    //!
    //! # Per-invocation memory limit
    //!
    //! Each `Store<HostState>` installs a [`PluginLimiter`] through
    //! `Store::limiter` (via [`HostState::limits_mut`]). The 64 MiB cap
    //! (`MEMORY_LIMIT = 64 * 1024 * 1024`) applies to the sum of all linear
    //! memories in the store — every core module of the component counts,
    //! including the initial size of each memory at instantiation. Table
    //! elements are capped store-wide the same way, and the number of core
    //! instances, memories and tables per store is bounded; the values and
    //! their rationale live in [`crate::limits`].
    //!
    //! When a memory request would exceed the cap, the limiter refuses it
    //! (`memory.grow` returns -1 to the guest) and records the refusal. If
    //! the call or the instantiation then fails, the failure is reported as
    //! [`PluginError::Memory`]; see [`classify_trap_error`].

    use camino::{Utf8Path, Utf8PathBuf};
    use dq_core::pointer::Segment;
    use dq_core::{EditScript, Ir, Pointer};
    use dq_exec::{Diagnostic, Severity as ExecSeverity};
    use wasmtime::component::{Component, HasSelf, Linker};
    use wasmtime::{Config, Engine, Store};

    use crate::error::{PluginError, Result};
    use crate::limits::{MEMORY_LIMIT, PluginLimiter, TABLE_ELEMENTS_LIMIT};

    // Generate host-side bindings for the `dq:plugin` world from the WIT
    // schema. The macro expands at compile time; if the WIT file changes,
    // a plain `cargo build` regenerates the bindings.
    //
    // `path:` is resolved relative to `CARGO_MANIFEST_DIR`.
    #[allow(clippy::all)]
    mod bindings {
        wasmtime::component::bindgen!({
            path: "wit/dq-plugin.wit",
            world: "plugin",
        });
    }

    // Keep `wit-bindgen` linked in the feature-on configuration so the
    // optional dep does not warn as unused. The crate is the *guest-side*
    // code generator that example plugins under `examples/plugin-rust/`
    // (Task 5.12) consume; the host bindings come from the
    // `wasmtime::component::bindgen!` macro above.
    #[allow(unused_imports)]
    use wit_bindgen as _;

    use bindings::Plugin as PluginBindings;
    use bindings::dq::plugin::types::{Diagnostic as WitDiagnostic, Severity as WitSeverity};

    /// Per-invocation fuel budget — ~1s of CPU on Apple Silicon. See the
    /// spec scenario "Infinite-loop plugin terminates with Exhausted".
    const FUEL_BUDGET: u64 = 100_000_000;

    /// Maximum native stack the WASM module may consume (2 MiB).
    const STACK_LIMIT: usize = 2 * 1024 * 1024;

    /// Host crate's compiled-against WIT major version. The runtime refuses
    /// to load plugins whose declared WIT major differs from this value.
    const HOST_WIT_MAJOR: u32 = 0;

    /// Host WIT version string surfaced in
    /// [`PluginError::SchemaVersion::host_version`].
    const HOST_WIT_VERSION: &str = "0.1.0";

    /// Per-invocation host state.
    ///
    /// Owned by the wasmtime [`Store`] and exposed to the linker via
    /// [`HasSelf<HostState>`]. Holds the data the `interface ir` and
    /// `interface jq` host imports project from, plus the store-wide
    /// [`PluginLimiter`].
    pub(crate) struct HostState {
        /// Pre-serialized JSON of the IR root. `interface ir`'s `get-root`
        /// returns a clone of these bytes; `get-at` walks the underlying
        /// `serde_json::Value` (parsed back from these bytes is wasteful, so
        /// we keep the structured value alongside).
        ir_root_bytes: Vec<u8>,
        /// Structured form of the IR root, used by `get-at` / `iterate` /
        /// `jq::eval` for pointer-based projection.
        ir_root_value: serde_json::Value,
        /// Format tag returned by `interface ir`'s `get-format-tag`.
        format_tag: String,
        /// Pool of compiled jq engines. `jq::compile` pushes a new engine
        /// and returns its index as a `u32` handle; `jq::eval` looks the
        /// engine up by handle.
        jq_engines: Vec<dq_transform::JqEngine>,
        /// Store-wide resource limiter attached via `Store::limiter`.
        limits: PluginLimiter,
    }

    impl HostState {
        fn new(ir: &Ir<'_>) -> Self {
            let ir_root_value = ir.value().to_serde_json();
            // Serializing through `to_vec` is infallible for any
            // `serde_json::Value`; if it ever fails we'd rather surface an
            // empty payload than panic mid-invocation. Plugins receive an
            // empty document in that case.
            let ir_root_bytes = serde_json::to_vec(&ir_root_value).unwrap_or_default();
            let limits = PluginLimiter::new(MEMORY_LIMIT, TABLE_ELEMENTS_LIMIT);
            Self {
                ir_root_bytes,
                ir_root_value,
                format_tag: ir.format().name().to_owned(),
                jq_engines: Vec::new(),
                limits,
            }
        }

        fn limits_mut(&mut self) -> &mut dyn wasmtime::ResourceLimiter {
            &mut self.limits
        }

        /// Whether the limiter refused a memory request for exceeding the
        /// store-wide budget during this invocation.
        fn memory_limit_hit(&self) -> bool {
            self.limits.memory_limit_hit()
        }
    }

    impl bindings::dq::plugin::ir::Host for HostState {
        fn get_root(&mut self) -> Vec<u8> {
            self.ir_root_bytes.clone()
        }

        fn get_at(&mut self, p: String) -> Option<Vec<u8>> {
            let pointer = Pointer::parse(&p).ok()?;
            let sub = resolve_serde(&self.ir_root_value, &pointer)?;
            serde_json::to_vec(sub).ok()
        }

        fn iterate(&mut self, p: String) -> Vec<String> {
            let Ok(pointer) = Pointer::parse(&p) else {
                return Vec::new();
            };
            let Some(sub) = resolve_serde(&self.ir_root_value, &pointer) else {
                return Vec::new();
            };
            match sub {
                serde_json::Value::Object(map) => map
                    .keys()
                    .map(|k| pointer.with_segment(Segment::Key(k.clone())).as_canonical())
                    .collect(),
                serde_json::Value::Array(items) => (0..items.len())
                    .map(|i| pointer.with_segment(Segment::Index(i)).as_canonical())
                    .collect(),
                _ => Vec::new(),
            }
        }

        fn get_format_tag(&mut self) -> String {
            self.format_tag.clone()
        }
    }

    impl bindings::dq::plugin::jq::Host for HostState {
        fn compile(&mut self, expr: String) -> std::result::Result<u32, String> {
            match dq_transform::JqEngine::compile(&expr) {
                Ok(engine) => {
                    let handle = u32::try_from(self.jq_engines.len()).map_err(|_| {
                        "jq engine pool exhausted (more than u32::MAX engines)".to_owned()
                    })?;
                    self.jq_engines.push(engine);
                    Ok(handle)
                }
                Err(e) => Err(format!("{e}")),
            }
        }

        fn eval(
            &mut self,
            handle: u32,
            input: String,
        ) -> std::result::Result<Vec<Vec<u8>>, String> {
            let pointer = Pointer::parse(&input).map_err(|e| format!("invalid pointer: {e}"))?;
            let sub = resolve_serde(&self.ir_root_value, &pointer)
                .ok_or_else(|| format!("pointer {input} did not resolve"))?
                .clone();
            let idx = usize::try_from(handle).map_err(|_| "invalid jq engine handle".to_owned())?;
            let engine = self
                .jq_engines
                .get(idx)
                .ok_or_else(|| "invalid jq engine handle".to_owned())?;
            let outputs = engine.run(&sub).map_err(|e| format!("{e}"))?;
            outputs
                .iter()
                .map(|v| serde_json::to_vec(v).map_err(|e| format!("{e}")))
                .collect()
        }
    }

    impl bindings::dq::plugin::types::Host for HostState {}

    /// Wasmtime-backed plugin runtime.
    ///
    /// One instance is reused across plugin loads and invocations within a
    /// single CLI run. Internally it owns the [`Engine`] (compiled once,
    /// shared across stores) and the pre-built [`Linker`] with all `ir` /
    /// `jq` / `types` host imports registered.
    ///
    /// `PluginRuntime` is `Send + Sync` because both `Engine` and
    /// `Linker<HostState>` are. Callers that want to share one runtime
    /// across rayon workers should wrap it in `Arc<PluginRuntime>` —
    /// neither field is `Clone`-friendly individually but the runtime as a
    /// whole is cheap to share by reference.
    pub struct PluginRuntime {
        engine: Engine,
        linker: Linker<HostState>,
    }

    impl std::fmt::Debug for PluginRuntime {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PluginRuntime").finish_non_exhaustive()
        }
    }

    /// Handle to a single loaded plugin module.
    ///
    /// One plugin file (`*.wasm`) corresponds to one [`PluginHandle`].
    /// Holds the compiled [`Component`] plus the rule-id derived from the
    /// plugin path's file stem (used for diagnostic attribution in
    /// [`PluginError::Exhausted`] / [`PluginError::Memory`] /
    /// [`PluginError::Invoke`]).
    pub struct PluginHandle {
        component: Component,
        rule_id: String,
    }

    impl std::fmt::Debug for PluginHandle {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PluginHandle")
                .field("rule_id", &self.rule_id)
                .finish_non_exhaustive()
        }
    }

    impl PluginHandle {
        /// Returns the rule-id used to attribute diagnostics from this
        /// plugin's invocations. Derived from the loaded path's file stem
        /// at [`PluginRuntime::load`] time.
        #[must_use]
        pub fn rule_id(&self) -> &str {
            &self.rule_id
        }
    }

    impl PluginRuntime {
        /// Build a new runtime with the spec-mandated sandbox configuration.
        ///
        /// # Errors
        ///
        /// Returns [`PluginError::Load`] (with an empty path) when wasmtime
        /// rejects the engine or linker configuration. In practice this
        /// never fires — the parameters are constants — but the result type
        /// matches the spec's `Result<Self, PluginError>` signature so a
        /// future config change can surface failures cleanly.
        pub fn new() -> Result<Self> {
            let mut config = Config::new();
            config
                .consume_fuel(true)
                .max_wasm_stack(STACK_LIMIT)
                .wasm_component_model(true)
                // Both are off by default; pinned because the store-wide
                // accounting in `PluginLimiter` relies on them being off.
                .wasm_custom_page_sizes(false)
                .wasm_memory64(false);
            let engine = Engine::new(&config).map_err(|e| PluginError::Load {
                path: Utf8PathBuf::new(),
                message: format!("engine init: {e}"),
            })?;
            let mut linker: Linker<HostState> = Linker::new(&engine);
            // `Plugin::add_to_linker` wires up the `ir`, `jq`, and `types`
            // host imports in one go. `HasSelf<HostState>` makes the host
            // trait implementations live directly on `HostState`.
            PluginBindings::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |s| s)
                .map_err(|e| PluginError::Load {
                    path: Utf8PathBuf::new(),
                    message: format!("linker setup: {e}"),
                })?;
            Ok(Self { engine, linker })
        }

        /// Load a plugin module from `path`.
        ///
        /// Performs four pre-instantiation checks:
        ///
        /// 1. Reads `*.wasm` bytes (IO error → [`PluginError::Load`]).
        /// 2. Compiles to a [`Component`] (compile error →
        ///    [`PluginError::Load`]).
        /// 3. Walks the component's imports for any `wasi:*` /
        ///    `wasi_snapshot_preview1*` name → [`PluginError::DisallowedImport`].
        /// 4. Walks the imports for any `dq:plugin/*@N.M.K` name and
        ///    compares `N` to [`HOST_WIT_MAJOR`] →
        ///    [`PluginError::SchemaVersion`].
        ///
        /// # Errors
        ///
        /// Surfaces the first failure encountered in the order above.
        pub fn load(&self, path: &Utf8Path) -> Result<PluginHandle> {
            let bytes = std::fs::read(path).map_err(|e| PluginError::Load {
                path: path.to_owned(),
                message: format!("read: {e}"),
            })?;
            let component =
                Component::new(&self.engine, &bytes).map_err(|e| PluginError::Load {
                    path: path.to_owned(),
                    message: format!("compile: {e}"),
                })?;
            // Walk imports once; the first WASI hit aborts. If no WASI
            // imports, look for a major-version mismatch on the
            // `dq:plugin/*` interfaces. We bail out of the loop on the
            // first match and propagate the error.
            for (name, _item) in component.component_type().imports(&self.engine) {
                if is_wasi_import(name) {
                    return Err(PluginError::DisallowedImport {
                        interface: name.to_owned(),
                    });
                }
                if let Some((plugin_major, plugin_version)) = parse_dq_plugin_version(name)
                    && plugin_major != HOST_WIT_MAJOR
                {
                    return Err(PluginError::SchemaVersion {
                        plugin_version,
                        host_version: HOST_WIT_VERSION.to_owned(),
                    });
                }
            }
            let rule_id = path
                .file_stem()
                .map(str::to_owned)
                .unwrap_or_else(|| "plugin".to_owned());
            Ok(PluginHandle { component, rule_id })
        }

        /// Run the plugin's `lint` export against `ir` and marshal each
        /// returned WIT `diagnostic` record into a host-side
        /// [`Diagnostic`].
        ///
        /// # Errors
        ///
        /// - [`PluginError::Exhausted`] when the plugin exceeds
        ///   [`FUEL_BUDGET`].
        /// - [`PluginError::Memory`] when the plugin tries to grow past
        ///   [`MEMORY_LIMIT`] (summed over the store).
        /// - [`PluginError::Invoke`] for any other wasmtime trap.
        pub fn invoke_lint(
            &self,
            handle: &PluginHandle,
            ir: &Ir<'_>,
            file: Option<&Utf8Path>,
        ) -> Result<Vec<Diagnostic>> {
            let (mut store, plugin) = self.instantiate(handle, ir)?;
            let raw = plugin.call_lint(&mut store).map_err(|e| {
                classify_trap_error(&e, &handle.rule_id, store.data().memory_limit_hit())
            })?;
            let diagnostics = raw
                .into_iter()
                .map(|d| marshal_diagnostic(d, ir, file))
                .collect();
            Ok(diagnostics)
        }

        /// Run the plugin's `fix` export against `ir`, parse the returned
        /// JSON Patch bytes, and return the resulting [`EditScript`].
        ///
        /// # Errors
        ///
        /// - [`PluginError::MalformedFix`] when the plugin's bytes do not
        ///   parse as a JSON-Patch [`EditScript`].
        /// - [`PluginError::Invoke`] when the plugin's `fix` export
        ///   returns `Err(message)` (a guest-side application failure).
        /// - [`PluginError::Exhausted`] / [`PluginError::Memory`] /
        ///   [`PluginError::Invoke`] for fuel / memory / generic traps,
        ///   classified the same way as [`Self::invoke_lint`].
        pub fn invoke_fix(&self, handle: &PluginHandle, ir: &Ir<'_>) -> Result<EditScript> {
            let (mut store, plugin) = self.instantiate(handle, ir)?;
            let raw = plugin.call_fix(&mut store).map_err(|e| {
                classify_trap_error(&e, &handle.rule_id, store.data().memory_limit_hit())
            })?;
            let bytes = raw.map_err(|message| PluginError::Invoke {
                rule_id: handle.rule_id.clone(),
                message,
            })?;
            serde_json::from_slice::<EditScript>(&bytes).map_err(|source| {
                PluginError::MalformedFix {
                    rule_id: handle.rule_id.clone(),
                    source,
                }
            })
        }

        /// Build a fresh `Store<HostState>` with the spec-mandated fuel and
        /// memory limits, then instantiate the component into the
        /// pre-built linker. Returns the store and a typed
        /// [`PluginBindings`] for issuing the actual export call.
        fn instantiate(
            &self,
            handle: &PluginHandle,
            ir: &Ir<'_>,
        ) -> Result<(Store<HostState>, PluginBindings)> {
            let host_state = HostState::new(ir);
            let mut store = Store::new(&self.engine, host_state);
            store.set_fuel(FUEL_BUDGET).map_err(|e| PluginError::Load {
                path: Utf8PathBuf::new(),
                message: format!("set_fuel: {e}"),
            })?;
            // Route every memory / table allocation of the store through
            // `HostState::limits` so the caps apply to the store as a whole.
            store.limiter(HostState::limits_mut);
            let instantiated =
                PluginBindings::instantiate(&mut store, &handle.component, &self.linker);
            let plugin = instantiated.map_err(|e| {
                let memory_limit_hit = store.data().memory_limit_hit();
                classify_instantiation_error(&e, &handle.rule_id, memory_limit_hit)
            })?;
            Ok((store, plugin))
        }
    }

    /// Walk `value` along `pointer`, returning the addressed sub-value.
    ///
    /// Mirrors [`Pointer::resolve`] but operates on a `serde_json::Value`
    /// (the pre-converted IR root we keep in [`HostState`]). Returns
    /// `None` for unmapped pointers, type mismatches, or out-of-bounds
    /// indices.
    fn resolve_serde<'a>(
        value: &'a serde_json::Value,
        pointer: &Pointer,
    ) -> Option<&'a serde_json::Value> {
        let mut current = value;
        for seg in pointer.segments() {
            current = match (current, seg) {
                (serde_json::Value::Object(map), Segment::Key(k)) => map.get(k)?,
                (serde_json::Value::Array(items), Segment::Index(i)) => items.get(*i)?,
                (serde_json::Value::Array(items), Segment::Key(k)) => {
                    let idx: usize = k.parse().ok()?;
                    items.get(idx)?
                }
                _ => return None,
            };
        }
        Some(current)
    }

    /// Returns `true` when `name` identifies a WASI host import the
    /// runtime refuses to satisfy.
    ///
    /// The check matches the two import-name shapes wasmtime surfaces:
    ///
    /// - Component-Model WASI shape `wasi:<package>/<interface>@<version>`.
    /// - Core-WASM WASI shape `wasi_snapshot_preview1` (rare for component
    ///   plugins but cheap to detect).
    fn is_wasi_import(name: &str) -> bool {
        name.starts_with("wasi:") || name.starts_with("wasi_snapshot_preview1")
    }

    /// Parse a `dq:plugin/<interface>@<major>.<minor>.<patch>` import name
    /// and return `(major, "<major>.<minor>.<patch>")`.
    ///
    /// Returns `None` for any non-`dq:plugin` import or when the version
    /// suffix is absent/unparseable. Used at load time to reject plugins
    /// whose declared WIT major differs from the host's.
    fn parse_dq_plugin_version(name: &str) -> Option<(u32, String)> {
        // Imports look like `dq:plugin/ir@0.1.0`, `dq:plugin/jq@0.1.0`, or
        // `dq:plugin/types@0.1.0`. The package id is the substring before
        // the first `/`, and the version follows the last `@`.
        let after_pkg = name.strip_prefix("dq:plugin/")?;
        let (_, version) = after_pkg.split_once('@')?;
        let major_str = version.split('.').next()?;
        let major: u32 = major_str.parse().ok()?;
        Some((major, version.to_owned()))
    }

    /// Marshal a WIT `diagnostic` record into a host-side [`Diagnostic`].
    ///
    /// Severity maps 1:1 from [`WitSeverity`] to [`ExecSeverity`]. Pointer
    /// resolution mirrors the spec's "if `pointer: None`, line/col default
    /// to `1`" rule: when the pointer parses and resolves through
    /// [`Ir::line_col_for`], use those values; otherwise fall back to
    /// `(1, 1)`.
    fn marshal_diagnostic(wit: WitDiagnostic, ir: &Ir<'_>, file: Option<&Utf8Path>) -> Diagnostic {
        let (line, col) = wit
            .pointer
            .as_deref()
            .and_then(|p| Pointer::parse(p).ok())
            .and_then(|pointer| ir.line_col_for(&pointer))
            .unwrap_or((1, 1));
        Diagnostic {
            rule_id: wit.rule_id,
            severity: severity_from_wit(wit.severity),
            message: wit.message,
            file: file.map(Utf8Path::to_path_buf),
            line,
            col,
            span: None,
            references: Vec::new(),
            fix: None,
        }
    }

    fn severity_from_wit(s: WitSeverity) -> ExecSeverity {
        match s {
            WitSeverity::Error => ExecSeverity::Error,
            WitSeverity::Warn => ExecSeverity::Warn,
            WitSeverity::Info => ExecSeverity::Info,
        }
    }

    /// Classify a wasmtime invocation error into the corresponding
    /// [`PluginError`] variant.
    ///
    /// - Fuel exhaustion (`Trap::OutOfFuel`) → [`PluginError::Exhausted`].
    /// - Memory cap denial → [`PluginError::Memory`]. A refused
    ///   `memory.grow` returns -1 to the guest rather than trapping; the
    ///   guest's allocator then aborts with a generic trap (typically
    ///   `unreachable`). `memory_limit_hit` is the limiter's record of the
    ///   refusal and attributes that trap to the cap.
    /// - Anything else → [`PluginError::Invoke`].
    fn classify_trap_error(
        err: &wasmtime::Error,
        rule_id: &str,
        memory_limit_hit: bool,
    ) -> PluginError {
        if let Some(trap) = err.downcast_ref::<wasmtime::Trap>()
            && *trap == wasmtime::Trap::OutOfFuel
        {
            return PluginError::Exhausted {
                rule_id: rule_id.to_owned(),
            };
        }
        if memory_limit_hit {
            return PluginError::Memory {
                rule_id: rule_id.to_owned(),
            };
        }
        let message = format!("{err}");
        // Fallback for memory failures the limiter did not record (an
        // error raised by wasmtime itself while growing memory): its text
        // mentions the growth or the limit. Detected via substring because
        // wasmtime does not expose a typed error for it.
        if message.contains("memory") && (message.contains("grow") || message.contains("limit")) {
            return PluginError::Memory {
                rule_id: rule_id.to_owned(),
            };
        }
        PluginError::Invoke {
            rule_id: rule_id.to_owned(),
            message,
        }
    }

    /// Classify an instantiation-time error.
    ///
    /// Linker-level mismatches (missing imports, type conflicts) come from
    /// `Component::instantiate`. The most common case for a versioned WIT
    /// mismatch that slipped past `parse_dq_plugin_version` is the
    /// component imports `dq:plugin/ir@2.0.0` while the host's linker
    /// only registers `@0.1.0` — wasmtime renders this as an "unknown
    /// import" error. We surface it as `PluginError::Load` so the caller
    /// sees a clean message.
    ///
    /// A memory whose initial size does not fit the store-wide cap fails
    /// instantiation; `memory_limit_hit` reports that as
    /// [`PluginError::Memory`].
    fn classify_instantiation_error(
        err: &wasmtime::Error,
        rule_id: &str,
        memory_limit_hit: bool,
    ) -> PluginError {
        // Trap-shaped failures (rare during instantiation but possible if
        // a `start` function runs) route through the same classifier as
        // call-site errors.
        if let Some(trap) = err.downcast_ref::<wasmtime::Trap>()
            && *trap == wasmtime::Trap::OutOfFuel
        {
            return PluginError::Exhausted {
                rule_id: rule_id.to_owned(),
            };
        }
        if memory_limit_hit {
            return PluginError::Memory {
                rule_id: rule_id.to_owned(),
            };
        }
        PluginError::Invoke {
            rule_id: rule_id.to_owned(),
            message: format!("{err}"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        // The `Host` trait imports below bring the `get_root` / `get_at` /
        // `iterate` / `get_format_tag` methods into scope so the tests can
        // call them directly on `HostState` without going through the
        // wasmtime linker.
        use bindings::dq::plugin::ir::Host as IrHost;
        use dq_core::Value;

        #[test]
        fn new_returns_runtime() {
            // Sanity check — the engine config is all constants, so
            // construction should always succeed.
            let _ = PluginRuntime::new().expect("PluginRuntime::new must succeed");
        }

        /// Compile-time check that `PluginRuntime` is `Send + Sync`. The
        /// CLI wraps it in `Arc<PluginRuntime>` and shares it across rayon
        /// workers; if the trait bounds regress we want it to fail here
        /// rather than at the call site.
        #[test]
        fn plugin_runtime_is_send_and_sync() {
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<PluginRuntime>();
            assert_send_sync::<PluginHandle>();
        }

        #[test]
        fn is_wasi_import_matches_component_and_core_shapes() {
            assert!(is_wasi_import("wasi:io/streams@0.2.0"));
            assert!(is_wasi_import("wasi_snapshot_preview1"));
            assert!(!is_wasi_import("dq:plugin/ir@0.1.0"));
            assert!(!is_wasi_import("custom-host"));
        }

        #[test]
        fn parse_dq_plugin_version_extracts_major() {
            assert_eq!(
                parse_dq_plugin_version("dq:plugin/ir@0.1.0"),
                Some((0, "0.1.0".to_owned()))
            );
            assert_eq!(
                parse_dq_plugin_version("dq:plugin/jq@2.0.0"),
                Some((2, "2.0.0".to_owned()))
            );
            assert_eq!(parse_dq_plugin_version("dq:plugin/ir"), None);
            assert_eq!(parse_dq_plugin_version("other:pkg/iface@1.0.0"), None);
        }

        #[test]
        fn severity_round_trip() {
            assert_eq!(severity_from_wit(WitSeverity::Error), ExecSeverity::Error);
            assert_eq!(severity_from_wit(WitSeverity::Warn), ExecSeverity::Warn);
            assert_eq!(severity_from_wit(WitSeverity::Info), ExecSeverity::Info);
        }

        #[test]
        fn resolve_serde_walks_objects_and_arrays() {
            let v: serde_json::Value = serde_json::json!({
                "a": [1, 2, {"b": "c"}],
            });
            let p = Pointer::parse("/a/2/b").expect("/a/2/b parses");
            assert_eq!(
                resolve_serde(&v, &p),
                Some(&serde_json::Value::String("c".to_owned()))
            );
            // Out-of-bounds index returns None (does not panic).
            let p_oob = Pointer::parse("/a/9").expect("/a/9 parses");
            assert!(resolve_serde(&v, &p_oob).is_none());
            // Type mismatch (string keyed into a non-object) returns None.
            let p_bad = Pointer::parse("/a/0/x").expect("/a/0/x parses");
            assert!(resolve_serde(&v, &p_bad).is_none());
        }

        #[test]
        fn host_state_iterate_yields_object_keys() {
            use dq_core::FormatTag;
            use dq_core::ProvenanceMap;
            let value = Value::Map(
                [
                    ("a".to_owned(), Value::Int(1)),
                    ("b".to_owned(), Value::Int(2)),
                ]
                .into_iter()
                .collect(),
            );
            let prov = ProvenanceMap::new();
            let ir = Ir::new(&value, &prov, FormatTag::Json);
            let mut state = HostState::new(&ir);
            let mut got = state.iterate(String::new());
            got.sort();
            assert_eq!(got, vec!["/a".to_owned(), "/b".to_owned()]);
        }

        #[test]
        fn host_state_iterate_yields_array_indices() {
            use dq_core::FormatTag;
            use dq_core::ProvenanceMap;
            let value = Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)]);
            let prov = ProvenanceMap::new();
            let ir = Ir::new(&value, &prov, FormatTag::Json);
            let mut state = HostState::new(&ir);
            let got = state.iterate(String::new());
            assert_eq!(got, vec!["/0".to_owned(), "/1".to_owned(), "/2".to_owned()]);
        }

        #[test]
        fn host_state_iterate_returns_empty_for_scalar() {
            use dq_core::FormatTag;
            use dq_core::ProvenanceMap;
            let value = Value::Int(42);
            let prov = ProvenanceMap::new();
            let ir = Ir::new(&value, &prov, FormatTag::Json);
            let mut state = HostState::new(&ir);
            assert!(state.iterate(String::new()).is_empty());
        }

        #[test]
        fn host_state_get_at_returns_subtree_bytes() {
            use dq_core::FormatTag;
            use dq_core::ProvenanceMap;
            let mut inner = indexmap::IndexMap::new();
            inner.insert("name".to_owned(), Value::String("dq".to_owned()));
            let value = Value::Map(
                [("config".to_owned(), Value::Map(inner))]
                    .into_iter()
                    .collect(),
            );
            let prov = ProvenanceMap::new();
            let ir = Ir::new(&value, &prov, FormatTag::Yaml);
            let mut state = HostState::new(&ir);
            let bytes = state
                .get_at("/config/name".to_owned())
                .expect("get_at must resolve /config/name");
            let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
            assert_eq!(v, serde_json::Value::String("dq".to_owned()));
        }

        #[test]
        fn host_state_get_at_returns_none_for_unknown() {
            use dq_core::FormatTag;
            use dq_core::ProvenanceMap;
            let value = Value::Map(indexmap::IndexMap::new());
            let prov = ProvenanceMap::new();
            let ir = Ir::new(&value, &prov, FormatTag::Yaml);
            let mut state = HostState::new(&ir);
            assert!(state.get_at("/missing".to_owned()).is_none());
            // Malformed pointer (no leading slash) — also None, never panic.
            assert!(state.get_at("not-a-pointer".to_owned()).is_none());
        }

        #[test]
        fn host_state_format_tag_round_trips() {
            use dq_core::FormatTag;
            use dq_core::ProvenanceMap;
            let value = Value::Null;
            let prov = ProvenanceMap::new();
            let ir = Ir::new(&value, &prov, FormatTag::Toml);
            let mut state = HostState::new(&ir);
            assert_eq!(state.get_format_tag(), "toml");
        }

        // ---- Store-wide limits, driven through wasmtime ----------------
        //
        // The fixtures below are core modules encoded by hand (wasmtime is
        // built without its `wat` feature). They run on the runtime's own
        // engine, so the engine configuration is the production one, with
        // a `PluginLimiter` carrying the production budgets as store data.

        /// One wasm page, in bytes.
        const PAGE: u32 = 64 * 1024;

        fn leb_u32(mut value: u32) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let byte = u8::try_from(value & 0x7f).expect("seven bits fit in u8");
                value >>= 7;
                if value == 0 {
                    out.push(byte);
                    return out;
                }
                out.push(byte | 0x80);
            }
        }

        fn section(id: u8, body: &[u8]) -> Vec<u8> {
            let len = u32::try_from(body.len()).expect("test section fits in u32");
            let mut out = vec![id];
            out.extend(leb_u32(len));
            out.extend_from_slice(body);
            out
        }

        fn module(sections: &[Vec<u8>]) -> Vec<u8> {
            let mut out = b"\0asm\x01\0\0\0".to_vec();
            for s in sections {
                out.extend_from_slice(s);
            }
            out
        }

        /// Memory section with one memory per entry of `min_pages`, no
        /// declared maximum.
        fn memory_section(min_pages: &[u32]) -> Vec<u8> {
            let count = u32::try_from(min_pages.len()).expect("count fits in u32");
            let mut body = leb_u32(count);
            for &min in min_pages {
                body.push(0x00); // limits: min only
                body.extend(leb_u32(min));
            }
            section(5, &body)
        }

        /// Two empty memories plus exports `g0(pages) -> i32` and
        /// `g1(pages) -> i32` (`memory.grow` on memory 0 / 1) and
        /// `trap()` (`unreachable`).
        fn two_memories_module() -> Vec<u8> {
            let types = section(1, &[0x02, 0x60, 0x01, 0x7f, 0x01, 0x7f, 0x60, 0x00, 0x00]);
            let funcs = section(3, &[0x03, 0x00, 0x00, 0x01]);
            let memories = memory_section(&[0, 0]);
            let exports = section(
                7,
                &[
                    0x03, // three exports
                    0x02, b'g', b'0', 0x00, 0x00, // func 0
                    0x02, b'g', b'1', 0x00, 0x01, // func 1
                    0x04, b't', b'r', b'a', b'p', 0x00, 0x02, // func 2
                ],
            );
            let code = section(
                10,
                &[
                    0x03, // three bodies
                    0x06, 0x00, 0x20, 0x00, 0x40, 0x00, 0x0b, // memory.grow 0
                    0x06, 0x00, 0x20, 0x00, 0x40, 0x01, 0x0b, // memory.grow 1
                    0x03, 0x00, 0x00, 0x0b, // unreachable
                ],
            );
            module(&[types, funcs, memories, exports, code])
        }

        fn limited_store(engine: &Engine) -> Store<PluginLimiter> {
            let mut store = Store::new(
                engine,
                PluginLimiter::new(MEMORY_LIMIT, TABLE_ELEMENTS_LIMIT),
            );
            store.set_fuel(FUEL_BUDGET).expect("fuel is enabled");
            store.limiter(|limiter| limiter as &mut dyn wasmtime::ResourceLimiter);
            store
        }

        fn instantiate_core(
            store: &mut Store<PluginLimiter>,
            bytes: &[u8],
        ) -> wasmtime::Result<wasmtime::Instance> {
            let module = wasmtime::Module::new(store.engine(), bytes)?;
            wasmtime::Instance::new(store, &module, &[])
        }

        fn memory_limit_pages() -> i32 {
            i32::try_from(MEMORY_LIMIT / PAGE as usize).expect("page count fits in i32")
        }

        #[test]
        fn memory_limit_applies_to_the_sum_of_all_memories() {
            let runtime = PluginRuntime::new().expect("runtime");
            let mut store = limited_store(&runtime.engine);
            let instance =
                instantiate_core(&mut store, &two_memories_module()).expect("instantiate");
            let g0 = instance
                .get_typed_func::<i32, i32>(&mut store, "g0")
                .expect("g0");
            let g1 = instance
                .get_typed_func::<i32, i32>(&mut store, "g1")
                .expect("g1");
            let half = memory_limit_pages() / 2;

            // Each memory takes half of the budget: both fit exactly.
            assert_eq!(g0.call(&mut store, half).expect("grow 0"), 0);
            assert_eq!(g1.call(&mut store, half).expect("grow 1"), 0);
            assert!(!store.data().memory_limit_hit());

            // One more page in either memory goes over the store budget,
            // although each memory alone is only at half of it.
            assert_eq!(g1.call(&mut store, 1).expect("grow 1 again"), -1);
            assert_eq!(g0.call(&mut store, 1).expect("grow 0 again"), -1);
            assert!(store.data().memory_limit_hit());
        }

        #[test]
        fn trap_after_refused_growth_classifies_as_memory() {
            let runtime = PluginRuntime::new().expect("runtime");
            let mut store = limited_store(&runtime.engine);
            let instance =
                instantiate_core(&mut store, &two_memories_module()).expect("instantiate");
            let g0 = instance
                .get_typed_func::<i32, i32>(&mut store, "g0")
                .expect("g0");
            let trap = instance
                .get_typed_func::<(), ()>(&mut store, "trap")
                .expect("trap");

            // Before any refusal, a trap is an ordinary invocation failure.
            let err = trap.call(&mut store, ()).expect_err("unreachable traps");
            let classified = classify_trap_error(&err, "p", store.data().memory_limit_hit());
            assert!(
                matches!(classified, PluginError::Invoke { .. }),
                "{classified:?}"
            );

            // A refused growth followed by the allocator's abort is the
            // memory cap.
            assert_eq!(
                g0.call(&mut store, memory_limit_pages() + 1)
                    .expect("grow returns"),
                -1
            );
            let err = trap.call(&mut store, ()).expect_err("unreachable traps");
            let classified = classify_trap_error(&err, "p", store.data().memory_limit_hit());
            assert!(
                matches!(classified, PluginError::Memory { ref rule_id } if rule_id == "p"),
                "{classified:?}"
            );
        }

        #[test]
        fn initial_memories_over_the_store_budget_fail_instantiation_as_memory() {
            let runtime = PluginRuntime::new().expect("runtime");
            let mut store = limited_store(&runtime.engine);
            // Two memories of 40 MiB each: each fits on its own, the sum
            // does not.
            let pages = 40 * 1024 * 1024 / PAGE;
            let bytes = module(&[memory_section(&[pages, pages])]);
            let err = instantiate_core(&mut store, &bytes).expect_err("over budget");
            let classified =
                classify_instantiation_error(&err, "p", store.data().memory_limit_hit());
            assert!(
                matches!(classified, PluginError::Memory { .. }),
                "{classified:?}"
            );
        }

        #[test]
        fn too_many_memories_fail_instantiation() {
            let runtime = PluginRuntime::new().expect("runtime");
            let mut store = limited_store(&runtime.engine);
            let count = crate::limits::MEMORIES_LIMIT + 1;
            let bytes = module(&[memory_section(&vec![0; count])]);
            let err = instantiate_core(&mut store, &bytes).expect_err("too many memories");
            assert!(format!("{err}").contains("memory count"), "{err}");
        }

        #[test]
        fn table_over_the_element_budget_fails_instantiation() {
            let runtime = PluginRuntime::new().expect("runtime");
            let mut store = limited_store(&runtime.engine);
            let over = u32::try_from(TABLE_ELEMENTS_LIMIT + 1).expect("fits");
            let mut body = vec![0x01, 0x70, 0x00]; // one funcref table, min only
            body.extend(leb_u32(over));
            let bytes = module(&[section(4, &body)]);
            let err = instantiate_core(&mut store, &bytes).expect_err("table over budget");
            assert!(format!("{err}").contains("table minimum size"), "{err}");
            assert!(!store.data().memory_limit_hit());
        }
    }
}

#[cfg(not(feature = "plugins"))]
mod plugins_disabled {
    //! Feature-disabled stub. Every entry point returns
    //! [`PluginError::FeatureDisabled`] so the CLI can produce a clean,
    //! deterministic error rather than failing to link.

    use camino::Utf8Path;
    use dq_core::{EditScript, Ir};
    use dq_exec::Diagnostic;

    use crate::error::{PluginError, Result};

    /// Marker `PluginRuntime` for feature-disabled builds.
    ///
    /// Holds no state; every method returns
    /// [`PluginError::FeatureDisabled`]. Callers check the variant via
    /// [`PluginError::kind_name`] and surface the user-facing remediation
    /// hint.
    #[derive(Debug, Default)]
    pub struct PluginRuntime {
        _private: (),
    }

    /// Marker `PluginHandle` for feature-disabled builds. Construction is
    /// impossible because [`PluginRuntime::load`] always errors — the
    /// type still exists so the public API surface matches the
    /// feature-on build.
    #[derive(Debug)]
    pub struct PluginHandle {
        _private: (),
    }

    fn feature_disabled<T>() -> Result<T> {
        Err(PluginError::FeatureDisabled {
            hint: "rebuild with --features plugins".to_owned(),
        })
    }

    impl PluginRuntime {
        /// Construct a `PluginRuntime` stub. Always returns
        /// [`PluginError::FeatureDisabled`] — there is no meaningful
        /// runtime to construct without the `plugins` feature.
        pub fn new() -> Result<Self> {
            feature_disabled()
        }

        /// Always returns [`PluginError::FeatureDisabled`].
        pub fn load(&self, _path: &Utf8Path) -> Result<PluginHandle> {
            feature_disabled()
        }

        /// Always returns [`PluginError::FeatureDisabled`].
        pub fn invoke_lint(
            &self,
            _handle: &PluginHandle,
            _ir: &Ir<'_>,
            _file: Option<&Utf8Path>,
        ) -> Result<Vec<Diagnostic>> {
            feature_disabled()
        }

        /// Always returns [`PluginError::FeatureDisabled`].
        pub fn invoke_fix(&self, _handle: &PluginHandle, _ir: &Ir<'_>) -> Result<EditScript> {
            feature_disabled()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use camino::Utf8PathBuf;

        #[test]
        fn new_returns_feature_disabled() {
            let err = PluginRuntime::new().expect_err("expected FeatureDisabled");
            assert_eq!(err.kind_name(), "feature_disabled");
        }

        #[test]
        fn load_returns_feature_disabled() {
            let runtime = PluginRuntime { _private: () };
            let err = runtime
                .load(&Utf8PathBuf::from("plugin.wasm"))
                .expect_err("expected FeatureDisabled");
            assert_eq!(err.kind_name(), "feature_disabled");
        }
    }
}

#[cfg(feature = "plugins")]
pub use plugins_enabled::{PluginHandle, PluginRuntime};

#[cfg(not(feature = "plugins"))]
pub use plugins_disabled::{PluginHandle, PluginRuntime};
