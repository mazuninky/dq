# data-query-plugin-abi

Capability: стабильный ABI для сторонних lint/fix-плагинов на WASM, на базе Component Model + WIT. Runtime-загрузка через wasmtime, изоляция (no fs/net), feature-gated за `--features plugins`.

## ADDED Requirements

### Requirement: `dq-plugin` crate structure and feature-gating

A new crate `dq-plugin` SHALL exist under `crates/dq-plugin/` with `Cargo.toml` declaring:

```toml
[features]
default = []
plugins = ["dep:wasmtime", "dep:wit-bindgen"]
```

Without the `plugins` feature the crate SHALL compile as a public-API stub: every entry point returns `PluginError::FeatureDisabled { hint: "rebuild with --features plugins" }` so that `dq-cli` can link against `dq-plugin` unconditionally and produce a deterministic error rather than a link-time failure when the feature is off. With the feature on, the crate SHALL link wasmtime and the generated WIT bindings.

#### Scenario: Default build does not include wasmtime
- **WHEN** the workspace is built with default features (`cargo build`)
- **THEN** `wasmtime` does not appear in `cargo metadata --format-version 1` resolved dependencies for the `dq-cli` package

#### Scenario: Stub returns FeatureDisabled
- **WHEN** `PluginRuntime::load(path)` is called in a build without `plugins`
- **THEN** the result is `Err(PluginError::FeatureDisabled { hint: "rebuild with --features plugins" })`

### Requirement: WIT schema package `dq:plugin`

The crate SHALL ship `crates/dq-plugin/wit/dq-plugin.wit` declaring package `dq:plugin@0.1.0` with at minimum these interfaces:

- `interface ir` — host-imported, exposes `get-root: func() -> list<u8>` (returns serialized JSON document), `get-at: func(p: pointer) -> option<list<u8>>`, `iterate: func(p: pointer) -> list<pointer>`, `format-tag: func() -> format-tag` (string).
- `interface jq` — host-imported, exposes `compile: func(expr: string) -> result<u32, string>` and `eval: func(handle: u32, input: pointer) -> result<list<list<u8>>, string>`.
- `world plugin` — combines both imports and exports `lint: func() -> list<diagnostic>` and `fix: func() -> result<list<u8>, string>` (return value is JSON Patch byte array).

Records `diagnostic { rule-id: string, severity: severity, message: string, pointer: option<string> }` and enum `severity { error, warn, info }` SHALL be defined in the same package.

The WIT package version SHALL follow semver: patch for bug-fixes only, minor for additive changes (new optional fields, new host-imported interfaces), major for breaking changes (removing fields, retyping, removing interfaces). The runtime SHALL reject loading a plugin whose declared WIT major version differs from the host's compiled-against major.

#### Scenario: Plugin with mismatched WIT major fails to load
- **WHEN** `PluginRuntime::load(path)` is called against a plugin module compiled for `dq:plugin@2.0.0` while the host links `dq:plugin@0.1.0`
- **THEN** the result is `Err(PluginError::SchemaVersion { plugin_version: "2.0.0", host_version: "0.1.0" })`

### Requirement: Wasmtime runtime configuration — fuel and memory limits

The plugin runtime SHALL configure wasmtime as follows:

- `Config::consume_fuel(true)` enabled. Each plugin invocation receives a budget of `100_000_000` fuel units (≈ ~1 second of CPU on M1). Exceeding the budget surfaces as a wasmtime `Trap::Interrupt`, mapped to `PluginError::Exhausted { rule_id }`.
- `Config::max_wasm_stack(2 * 1024 * 1024)` (2 MiB stack).
- Per-instance memory limit: `64 * 1024 * 1024` bytes (64 MiB), enforced via `Store::limiter`. Exceeding the limit surfaces as `PluginError::Memory { rule_id }`.
- WASI: not added. Plugins cannot import WASI interfaces. Attempting to load a plugin that imports WASI fails at link time with `PluginError::DisallowedImport { interface }`.

#### Scenario: Infinite-loop plugin terminates with Exhausted
- **WHEN** a plugin's `lint` export contains an infinite loop and is invoked
- **THEN** the runtime returns `Err(PluginError::Exhausted { rule_id })` within ~1 second of CPU time

#### Scenario: WASI-importing plugin fails to load
- **WHEN** `PluginRuntime::load(path)` is called against a plugin that imports any function from `wasi_snapshot_preview1`
- **THEN** the result is `Err(PluginError::DisallowedImport { interface: "wasi_snapshot_preview1" })`

### Requirement: Plugin invocation surfaces `Diagnostic` and `EditScript` to host

`PluginRuntime::invoke_lint(&self, plugin_id, ir) -> Result<Vec<Diagnostic>, PluginError>` SHALL run the plugin's `lint` export against the given `Ir`, marshal each WIT `diagnostic` record into a host-side `Diagnostic` (preserving rule-id, severity, message, optional pointer), and return them in invocation order. If the plugin's diagnostic carries `pointer: Some(p)`, the host SHALL look up the span via `Ir::span_for(&p)` to fill `line/col`; if `pointer: None`, line/col default to `1` (same fallback as `loc:`-less rules in `data-query-exec`).

`PluginRuntime::invoke_fix(&self, plugin_id, ir) -> Result<EditScript, PluginError>` SHALL run the plugin's `fix` export, parse the returned bytes as a JSON Patch via `serde_json::from_slice::<EditScript>`, and return the parsed script. Parse failures surface as `PluginError::MalformedFix { rule_id, source }`.

#### Scenario: Lint plugin returning two diagnostics
- **WHEN** a plugin's `lint` export returns two `diagnostic` records with rule-ids `"x.a"` and `"x.b"` against a YAML doc whose `as_ir().span_for("/path")` is `Some(line: 5, col: 1)`, and the diagnostic's `pointer` is `Some("/path")`
- **THEN** the host's `invoke_lint` returns `Ok(vec![Diag { rule_id: "x.a", line: 5, col: 1, ... }, Diag { rule_id: "x.b", ... }])`

#### Scenario: Fix plugin returning malformed JSON
- **WHEN** a plugin's `fix` export returns `b"not json"` bytes
- **THEN** `invoke_fix` returns `Err(PluginError::MalformedFix { rule_id, source })` whose `source` chains a `serde_json::Error`

### Requirement: CLI loads plugins from `--plugins <DIR>`

The `dq-cli` SHALL accept a `--plugins <DIR>` flag (global, optional). When present, the lint/fix pipelines SHALL discover every `*.wasm` file under `<DIR>` (non-recursive), load each via `PluginRuntime::load`, and merge the resulting plugin rules into the rule set alongside `@std/*` and `./.dq/rules/*.yml` rules. Loading order SHALL be lexical by file name within the directory.

When the binary is built without `--features plugins`, the flag SHALL still parse (no clap error), but use of the flag with at least one resolved `*.wasm` file SHALL error with `InvalidInput` carrying the message `"plugins are not enabled in this build; rebuild with --features plugins"` and exit code 6.

#### Scenario: Plugin discovery in directory
- **WHEN** `dq lint --plugins ./plugins ./manifest.yaml` is run with `./plugins/a.wasm` and `./plugins/b.wasm` present
- **THEN** the runtime loads both `a.wasm` and `b.wasm` in lexical order before evaluating

#### Scenario: --plugins without feature-gate errors
- **WHEN** `dq lint --plugins ./plugins ./file.yaml` is run with `./plugins/a.wasm` present, on a binary built without `plugins` feature
- **THEN** the CLI exits with code 6 and stderr contains `"plugins are not enabled in this build"`

### Requirement: `PluginError` exposes `kind_name()` for stable exit-code mapping

`PluginError` SHALL implement `kind_name(&self) -> &'static str` returning one of `"feature_disabled"`, `"schema_version"`, `"exhausted"`, `"memory"`, `"disallowed_import"`, `"malformed_fix"`, `"load"`, `"invoke"`. The CLI exit-code mapper SHALL route `feature_disabled` and `disallowed_import` to `InvalidInput` (6), `schema_version` to `PARSE_ERROR` (3), `exhausted` / `memory` / `invoke` to `RUNTIME_ERROR` (4), `malformed_fix` to `PARSE_ERROR` (3).

#### Scenario: kind_name covers every variant
- **WHEN** `kind_name()` is called on each variant of `PluginError`
- **THEN** the returned string is one of the listed canonical names AND no two variants return the same name
