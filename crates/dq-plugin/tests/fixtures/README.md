# `dq-plugin` integration-test fixtures

This directory holds prebuilt WASM Component-Model artifacts that the
integration tests under `crates/dq-plugin/tests/` load to exercise the
`PluginRuntime` against real guest plugins.

The fixtures are NOT committed by default — building them requires
`cargo-component` (or `wasm-tools` plus `wasm32-unknown-unknown`), neither
of which is installed in CI / contributor sandboxes by default. The tests
that depend on a fixture detect its absence and skip with a clear log
line; the build recipes below populate the directory once the host has
the tooling.

| Fixture filename     | Purpose                                                                            | Consumed by                                  |
| -------------------- | ---------------------------------------------------------------------------------- | -------------------------------------------- |
| `example_noop.wasm`  | Reference plugin: `lint()` returns one warn diag, `fix()` returns `b"[]"` (no-op). | 5.15 (lint), 5.16 (fix happy path)           |
| `infinite_loop.wasm` | `lint()` runs `loop {}` — exercises the fuel-budget interrupt.                     | 5.17 (`PluginError::Exhausted`)              |
| `wasi_plugin.wasm`   | Imports `wasi:io/streams` (or `wasi_snapshot_preview1`) — must be rejected.        | 5.18 (`PluginError::DisallowedImport`)       |
| `wrong_version.wasm` | Compiled against `dq:plugin@2.0.0` — host links `0.1.0`.                           | 5.20 (`PluginError::SchemaVersion` or Load)  |
| `malformed_fix.wasm` | `fix()` returns non-JSON bytes — exercises `MalformedFix` parsing path.            | 5.16 (second test; `#[ignore]` until built)  |

## Why are the tests "skipped" rather than `#[ignore]`d?

`#[ignore]` requires `cargo test -- --ignored` to opt in. We want the
tests to run automatically when a contributor populates the fixtures
without any extra flag. Each test does an `if !path.exists() { eprintln!
("SKIPPING ..."); return Ok(()); }` guard so:

* Default sandbox / CI without tooling → green test that prints a SKIP
  notice in the captured stdout.
* Contributor with `cargo-component` builds the fixture → next
  `cargo test` exercises the full WASM round-trip.

## Build recipes

All recipes assume you have a working Rust toolchain with the relevant
WASM target installed.

### `example_noop.wasm` — reference plugin (Tasks 5.15, 5.16)

Source lives at `examples/plugin-rust/`. See its README for the full
walk-through. Short form (Path A — `cargo-component`):

```sh
rustup target add wasm32-wasip2
cargo install --locked cargo-component

cd examples/plugin-rust
cargo component build --release

cp target/wasm32-wasip2/release/dq_plugin_example_noop.wasm \
   ../../crates/dq-plugin/tests/fixtures/example_noop.wasm
```

### `infinite_loop.wasm` — fuel-exhaustion guest (Task 5.17)

A separate guest whose `lint()` body is `loop {}`. Recipe (sketch):

```sh
# Copy the example as a starting point.
cp -R examples/plugin-rust /tmp/plugin-loop
cd /tmp/plugin-loop

# Replace the lint impl in src/lib.rs with `loop {}`.
sed -i.bak 's|vec!\[Diagnostic .*\]|{ loop {} }|' src/lib.rs

cargo component build --release
cp target/wasm32-wasip2/release/*.wasm \
   /path/to/dq/crates/dq-plugin/tests/fixtures/infinite_loop.wasm
```

### `wasi_plugin.wasm` — disallowed-import guest (Task 5.18)

A guest that pulls a `wasi:io/streams` import (or any `wasi:*` /
`wasi_snapshot_preview1` symbol). Sketch — add a WASI dependency to the
guest's WIT world and re-export a function that uses it:

```sh
# In a copy of examples/plugin-rust/wit/world.wit, add:
#   import wasi:io/streams@0.2.0;
# Then in src/lib.rs reference any wasi:io::streams item so wasm-tools
# does not optimize the import away.
cargo component build --release
cp target/wasm32-wasip2/release/*.wasm \
   /path/to/dq/crates/dq-plugin/tests/fixtures/wasi_plugin.wasm
```

### `wrong_version.wasm` — WIT major mismatch (Task 5.20)

Same source as `example_noop`, but with the WIT package declaration
edited to `dq:plugin@2.0.0`:

```sh
cp -R examples/plugin-rust /tmp/plugin-v2
cd /tmp/plugin-v2

# Bump the WIT package version in BOTH the package decl and any `@0.1.0`
# version specifiers on imports/exports inside wit/world.wit.
sed -i.bak 's/dq:plugin@0\.1\.0/dq:plugin@2.0.0/g' wit/world.wit
sed -i.bak 's/@0\.1\.0/@2.0.0/g' wit/world.wit

cargo component build --release
cp target/wasm32-wasip2/release/*.wasm \
   /path/to/dq/crates/dq-plugin/tests/fixtures/wrong_version.wasm
```

The host detects the mismatch by walking import names for any
`dq:plugin/<iface>@<major>.<minor>.<patch>` whose major != 0; either the
load path returns `PluginError::SchemaVersion` (if the version detection
triggers at parse time) OR `PluginError::Load` if the link fails first.
The test accepts either variant.

### `malformed_fix.wasm` — bad fix payload (Task 5.16)

Same source as `example_noop`, but with `Component::fix` returning a
non-JSON byte literal:

```sh
cp -R examples/plugin-rust /tmp/plugin-malformed
cd /tmp/plugin-malformed

# Replace `Ok(b"[]".to_vec())` with `Ok(b"not json".to_vec())` in src/lib.rs.
sed -i.bak 's|Ok(b"\[\]".to_vec())|Ok(b"not json".to_vec())|' src/lib.rs

cargo component build --release
cp target/wasm32-wasip2/release/*.wasm \
   /path/to/dq/crates/dq-plugin/tests/fixtures/malformed_fix.wasm
```
