# dq-plugin

WASM plugin runtime for the `dq` CLI. Loads third-party `lint` / `fix`
plugins compiled to the WebAssembly Component Model and described by the
WIT package `dq:plugin@0.1.0` (see `wit/dq-plugin.wit`).

## Status

Experimental — `v0.1.0` of the WIT schema is a preview. The schema may
break before `v1.0.0`. Pin against an exact dq release while the ABI
stabilises.

## Build

The wasmtime runtime is feature-gated. Default builds expose a stub that
returns `PluginError::FeatureDisabled` from every entry point, so callers
(e.g. the CLI's lint / fix pipelines) can link unconditionally:

```sh
# Default — wasmtime is NOT pulled in.
cargo build -p dq-plugin

# With the runtime.
cargo build -p dq-plugin --features plugins
```

The `plugins` feature pulls in:

- `wasmtime` (with `component-model`, `cranelift`, `runtime`, `std`) — the
  Component-Model runtime.
- `wit-bindgen` — the *guest-side* code generator that example plugins
  under `examples/plugin-rust/` consume. The host-side bindings come from
  the `wasmtime::component::bindgen!` macro, which is bundled with
  wasmtime itself; `wit-bindgen` stays in the feature gate per the spec's
  exact feature shape and is wired live via a `use wit_bindgen as _;` in
  the runtime module so `cargo` does not warn about an unused optional
  dep.

## Regenerating bindings

Host-side bindings are generated at *compile time* by the
`wasmtime::component::bindgen!` macro from `wit/dq-plugin.wit`. There is
no separate `wit-bindgen` CLI step — when the WIT file changes, a plain
`cargo build --features plugins` regenerates the bindings.

If the WIT macro fails to compile, run with `--verbose` to see the macro
expansion error, or extract the WIT file into a standalone reproducer
with the same `wasmtime` / `wit-bindgen` versions pinned in
`Cargo.toml`.

## Contract

The full plugin ABI contract — sandbox limits, error variants, exit-code
mapping, semver rules — lives in
[`openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md`](../../openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md).
The WIT schema in `wit/dq-plugin.wit` is the authoritative wire format.

## Versioning

The WIT package version follows semver:

| bump  | when                                                                        |
| ----- | --------------------------------------------------------------------------- |
| patch | bug-fix only; no schema changes                                             |
| minor | additive: new optional fields, new host-imported interfaces, new exports   |
| major | breaking: removed fields, retyped fields, removed interfaces                |

The runtime refuses to load a plugin whose declared WIT major version
differs from the host's compiled-against major (`PluginError::SchemaVersion`).
