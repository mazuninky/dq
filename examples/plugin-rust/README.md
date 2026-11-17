# dq-plugin-example-noop

Minimal Rust reference plugin for the `dq` v0.1.0 plugin ABI. Demonstrates
the smallest possible implementation of the WIT contract documented in
[`openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md`](../../openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md).

## What it does

- **`lint()`** returns a single warn-severity diagnostic with
  `rule_id = "example.demo-lint"` and a constant message. The diagnostic
  carries no pointer, so the host marshals it at `(line, col) = (1, 1)`.
- **`fix()`** returns `b"[]"` — an empty JSON Patch array. The host parses
  this as an empty `EditScript` and applies a no-op (the document stays
  byte-identical).

The plugin ignores its input — it does not call any of the imported `ir`
or `jq` host interfaces. Real plugins consume those imports to produce
data-driven output; this one is a smoke-test fixture / reference shape.

## Project layout

```
examples/plugin-rust/
├── Cargo.toml          standalone (NOT a member of the dq workspace)
├── README.md           this file
├── src/
│   └── lib.rs          the Guest impl + `export!(Component);`
└── wit/
    └── world.wit       mirror of crates/dq-plugin/wit/dq-plugin.wit
```

`Cargo.toml` carries an empty `[workspace]` section that marks this
manifest as its own workspace root, isolated from the top-level dq
workspace (which lists `examples/*` under `exclude = [...]`).

`wit/world.wit` is a copy of the host's WIT, not a symlink — symlinks
break on Windows clones and in `cargo package` / `git archive` snapshots.
Keep both files in sync when bumping the WIT major.

## Building as a Component-Model wasm artifact

The host loads plugins as **WASM Components** (Component Model) — not raw
core-wasm modules. There are two equivalent build paths; pick whichever
matches the tooling on your machine.

### Path A — `cargo-component` (recommended)

```sh
# Install once.
rustup target add wasm32-wasip2
cargo install --locked cargo-component

# From this directory.
cargo component build --release

# Artifact lands at:
#   target/wasm32-wasip2/release/dq_plugin_example_noop.wasm
```

`cargo-component` understands the `wit-bindgen::generate!` macro emitted
in `src/lib.rs` and produces a fully-wrapped Component-Model artifact
straight out of `cargo build`.

### Path B — `wasm-tools component new` (manual wrap)

```sh
# Install once.
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-tools

# From this directory: build a core-wasm module, then wrap it.
cargo build --release --target wasm32-unknown-unknown
wasm-tools component new \
    target/wasm32-unknown-unknown/release/dq_plugin_example_noop.wasm \
    -o target/dq_plugin_example_noop.component.wasm
```

The wrapped artifact at
`target/dq_plugin_example_noop.component.wasm` is what the dq host loads.

> **Compatibility note.** As of `wasmtime 44.x` the host accepts both the
> `wasm32-wasip2` (Component-Model) and the manually-wrapped
> `wasm32-unknown-unknown` artifact shapes. If `cargo component build`
> fails because `cargo-component` is not yet on a release that matches
> wit-bindgen 0.57, fall back to Path B.

## Using the plugin with `dq`

Plugins are gated behind the `plugins` cargo feature. Install or rebuild
`dq` with the feature enabled:

```sh
cargo install --locked --features plugins dq-cli
```

Then drop the built `*.wasm` into any directory and pass it via the
`--plugins` flag:

```sh
mkdir -p ./plugins
cp target/wasm32-wasip2/release/dq_plugin_example_noop.wasm ./plugins/

# Lint — every file produces one `example.demo-lint` warn diagnostic.
dq lint --plugins ./plugins config.yaml

# Fix — applies an empty EditScript (no-op), confirming the round-trip
# works end-to-end.
dq fix --plugins ./plugins config.yaml
```

`dq` discovers `*.wasm` files non-recursively under the directory passed
to `--plugins`, sorted lexically. Without the `plugins` feature enabled,
`dq` still parses the flag but exits `6` (`InvalidInput`) the moment it
encounters a `*.wasm` file under the directory.

## Modifying the plugin

The single source of truth for the export surface is `wit/world.wit`. To
change what the plugin emits:

1. Edit `src/lib.rs::Component::lint` / `Component::fix`.
2. Run `cargo component build --release` (or the Path B equivalent).
3. Drop the new artifact into your `--plugins` directory and re-invoke
   `dq`.

To upgrade to a newer `dq:plugin` major:

1. Replace `wit/world.wit` with the host's updated
   `crates/dq-plugin/wit/dq-plugin.wit`.
2. Update the generated bindings (rebuild — the `wit_bindgen::generate!`
   macro re-runs at compile time).
3. Adapt `src/lib.rs` to whatever shape the new `Guest` trait exposes.

## Status

`v0.1.0` is **experimental**. Breaking changes to the WIT schema, the
host imports, and the `Diagnostic` / `EditScript` marshalling shapes are
allowed before `v1.0.0`. Pin to a specific dq version in CI until the
ABI stabilizes.
