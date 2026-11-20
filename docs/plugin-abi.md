# Plugin ABI: `dq:plugin@0.1.0`

dq can load third-party lint and fix plugins compiled to WebAssembly
components. This page and the WIT file
[`crates/dq-plugin/wit/dq-plugin.wit`](../crates/dq-plugin/wit/dq-plugin.wit)
together are the contract: the WIT file is the wire format, this page says
what every function and field means and how the host behaves around them.

> **Experimental.** `0.1.0` is a preview. The WIT schema, the host imports
> and the marshalling of diagnostics and edit scripts may change before
> `1.0.0`. Pin a specific dq release in CI while the ABI settles.

Runtime: the `dq-plugin` crate (`crates/dq-plugin/`), wasmtime 50 with the
Component Model. Reference plugin: [`examples/plugin-rust/`](../examples/plugin-rust/).

## When to write a plugin

YAML rules with jq or JSON Schema cover most checks
([reference/lint-engine.md](reference/lint-engine.md)). A plugin is for the
rest: logic that is awkward in jq, a custom aggregation, a check that wants a
real programming language. Plugins see the parsed document, not the file
bytes; they cannot add new formats (the original plan's `parse` / `format`
plugin exports were not built, see [roadmap.md](roadmap.md)).

## Enabling plugins

The runtime is behind the `plugins` cargo feature, so the default static
binary does not link wasmtime:

```sh
cargo install --locked --features plugins --git https://github.com/mazuninky/dq dq-cli
```

Without the feature, `dq-plugin` compiles as a stub whose every entry point
returns `PluginError::FeatureDisabled { hint: "rebuild with --features
plugins" }`, so the CLI links the same way in both builds. `--plugins <DIR>`
is still accepted; it is a silent no-op when the directory holds no `*.wasm`
file, and fails with exit 6 ("plugins are not enabled in this build; rebuild
with --features plugins") as soon as it finds one.

## Discovery and loading

`--plugins <DIR>` (a global flag, used by `dq lint`, `dq check` and
`dq fix`):

- lists `*.wasm` files directly in `DIR` — not recursive — sorted
  lexically by file name; that order is the load and invocation order;
- loads each once per run into a shared runtime, before any file is
  processed. A plugin that fails to load stops the run with the error's exit
  code (see [Errors](#errors-and-exit-codes)).

Each plugin is identified by its file stem (`my_plugin.wasm` → `my_plugin`);
that name appears in error messages (`plugin rule my_plugin exhausted fuel
budget`). The `rule-id` of each diagnostic is whatever the plugin returns.

Load-time checks, in order:

1. read the file and compile it as a component (failure → `load`);
2. walk the component's imports: any `wasi:*` or `wasi_snapshot_preview1*`
   import → `disallowed_import`;
3. for every `dq:plugin/<interface>@<version>` import, compare the major
   version with the host's (`0`); a different major → `schema_version`.

## The WIT contract

```wit
package dq:plugin@0.1.0;

interface ir {
    type pointer = string;
    type format-tag = string;
    get-root: func() -> list<u8>;
    get-at: func(p: pointer) -> option<list<u8>>;
    iterate: func(p: pointer) -> list<pointer>;
    get-format-tag: func() -> format-tag;
}

interface jq {
    compile: func(expr: string) -> result<u32, string>;
    eval: func(handle: u32, input: string) -> result<list<list<u8>>, string>;
}

interface types {
    enum severity { error, warn, info }
    record diagnostic {
        rule-id: string,
        severity: severity,
        message: string,
        pointer: option<string>,
    }
}

world plugin {
    import ir;
    import jq;
    import types;
    use types.{diagnostic};
    export lint: func() -> list<diagnostic>;
    export fix: func() -> result<list<u8>, string>;
}
```

### The document a plugin sees

Every call happens against one document: a whole file, or one document of a
multi-document YAML stream (see [Invocation model](#invocation-model)). The
host converts that document's value to JSON once per call and serves it
through `ir`. Pointers are RFC 6901 strings in canonical form (the same form
`dq get` accepts and diagnostics print); in a stream document `""` is that
document's root, not the stream's.

### `interface ir` (imported from the host)

| Function | Returns |
|---|---|
| `get-root()` | The whole document as UTF-8 JSON bytes. |
| `get-at(p)` | The JSON bytes of the value at pointer `p`; `none` if `p` does not parse or does not resolve. An array index may be given as a digit key. |
| `iterate(p)` | The canonical pointers of the direct children of the node at `p`: one per key of an object (in document order), one per index of an array. Empty for scalars and for pointers that do not resolve. |
| `get-format-tag()` | The parser name of the document's format: `yaml`, `json`, `toml`, `markdown`, `hcl`, `dockerfile`, `xml`, … (the names in [reference/formats.md](reference/formats.md)). |

(The function is called `get-format-tag` because WIT does not allow a
function and a type of the same name in one interface; the type keeps the
name `format-tag`.)

### `interface jq` (imported from the host)

The host's embedded jq engine (jaq; see [reference/jq.md](reference/jq.md)).

- `compile(expr)` compiles a jq expression and returns a handle, an index
  into a per-call pool (handles are valid only for the current `lint` or
  `fix` call, since every call gets a fresh instance). `err` carries the
  compile error text.
- `eval(handle, input)` runs a compiled expression. **`input` is an RFC 6901
  pointer** into the document, not a JSON value: the expression runs on the
  value at that pointer (`""` for the whole document). It runs on the
  document's own value, not on its JSON encoding, so numbers behave as in
  `dq query` and the rule engine: a NaN or an infinity is a jq number
  (`type == "number"`, `isnan`, `isinfinite` hold) and a big number keeps its
  exact value. The result is every value the expression emits, each as UTF-8
  JSON encoded the way `get-root` encodes the document (see
  [Values across the boundary](#values-across-the-boundary)), in order.
  `err` for an unknown handle, a pointer that does not parse or resolve, or a
  jq runtime error.

### `interface types`

- `severity` — `error`, `warn`, `info`, mapped one to one onto dq's rule
  severities (and so onto exit codes: an `error` diagnostic fails `dq lint`
  with exit 4, `warn` only under `--strict`).
- `diagnostic` — `rule-id` (free text; use `<namespace>.<name>` like the
  `@std` rules), `severity`, `message`, and an optional `pointer`.

### Exports

**`lint() -> list<diagnostic>`** — the findings for the current document,
in the order they should be reported. The host turns each into a dq
diagnostic: the file is the file under lint; if `pointer` parses and the
format has spans (YAML, JSON, TOML), `line` / `col` come from the source
position of that node; otherwise the diagnostic lands at the document's
origin — `1:1` for a file, the first line of the document for a stream
document. Plugin diagnostics carry no references and no fix payload.

**`fix() -> result<list<u8>, string>`** — an edit script for the current
document as JSON bytes: an RFC 6902 JSON Patch array restricted to `add`,
`replace` and `remove`, for example

```json
[{"op": "replace", "path": "/spec/replicas", "value": 3},
 {"op": "remove",  "path": "/metadata/annotations/old"}]
```

`[]` means "nothing to fix". Any other op (`copy`, `move`, `test`), an
unknown field or malformed JSON is a `malformed_fix` error. `err(message)` is
a plugin-side failure and becomes an `invoke` error carrying the message.
How the host applies the script is described below.

## Invocation model

- **lint / check**: for every file, after the YAML rules have run, each
  plugin's `lint` runs and its diagnostics are appended to the file's
  diagnostics, plugins in load order.
- **fix**: for every file, after all rule fixes have been applied, each
  plugin's `fix` runs against the current (already rule-fixed) document, in
  load order, and its script is applied.
- **YAML streams**: a plugin runs once per document of a multi-document
  YAML file (or only for document N under `--doc N`); it sees each document
  as a document of its own, and its fix script is rebased under that
  document's `/N` before being applied. Position-less diagnostics land at
  each document's first line.
- Every call (`lint` or `fix`, per document) gets a **fresh store and a
  fresh instance**: no state survives between calls, and each call gets the
  full fuel and memory budget.
- Calls are synchronous; a plugin call blocks the thread that processes the
  file (`--parallel` runs files on several threads).

### Applying a fix script

The script is applied as a textual edit (the same machinery as `dq set` /
`dq patch`), so comments and layout around the edit survive on YAML, JSON
and TOML. On formats without textual edits the apply fails.

- An empty script is skipped.
- A script that fails to apply (a pointer that no longer exists after the
  rule fixes, an edit the textual writer refuses, a read-only format) is
  logged as a warning and skipped; the run continues.
- `remove ""` on a file that is not a stream is skipped with a warning (it
  would empty the file). In a stream, `remove ""` as the **last** op of the
  script drops that document from the stream; followed by more ops it is
  skipped.
- `replace ""` / `add ""` replace the whole document through the format's
  writer, as `dq set FILE ''` does (behaviour from the unreleased changes on
  this branch; whole-document plugin fixes used to be skipped).
- Unlike rule fixes, the host does not re-run a plugin to check that its fix
  is idempotent. Return `[]` once the document is fixed.
- Plugin *invocation* errors (fuel, memory, trap, malformed script) stop the
  run with their exit code; they are not skipped.

## Values across the boundary

`get-root`, `get-at` and the outputs of `jq.eval` are UTF-8 JSON:

- Map keys keep the document's order.
- A number past 64 bits or past `f64` is a JSON number literal with its exact
  value (`18446744073709551616`, `1e400`); a guest JSON parser without
  arbitrary precision will round it.
- A float is its shortest spelling that reads back as the same `f64`, with an
  exponent outside `[1e-5, 1e16)`: `3.0`, `123.25`, `1e300`, `1e-7` — the
  way every JSON output of dq spells it.
- JSON has no NaN or infinity. A YAML `.nan` / `.inf` / `-.inf` or a TOML
  `nan` / `inf` / `-inf` arrives as the JSON **string** `".nan"`, `".inf"` or
  `"-.inf"` — the same three spellings whatever format the document came
  from. A plugin that must tell such a number from a string with that text
  asks `jq.eval` (`isnan`, `type`), which runs on the document's own value.
- Values the plugin writes back in a fix script are parsed as JSON and
  written with the target format's rules (a NaN cannot be written to JSON at
  all).

## Sandbox and limits

| Limit | Value | What happens |
|---|---|---|
| WASI | not linked | No filesystem, network, clock or process access. Importing any `wasi:*` interface fails at load: `disallowed_import`. |
| Fuel | 100 000 000 units per call (about one second of CPU on Apple Silicon) | `Trap::OutOfFuel` → `exhausted`. |
| Linear memory | 64 MiB, summed over **all** memories of the component, initial sizes included | the growth is refused (`memory.grow` returns -1); the trap that follows is reported as `memory`. |
| Table elements | 100 000, summed over all tables | growth refused. |
| Core instances | 32 per store | instantiation fails. |
| Memories | 16 per store | instantiation fails. |
| Tables | 32 per store | instantiation fails. |
| Wasm stack | 2 MiB | trap → `invoke`. |
| Memory64, custom page sizes | rejected | 32-bit memories and tables with 64 KiB pages only; a component declaring a 64-bit memory does not compile (`load`). |

The memory and table caps apply to the whole store, not per memory: a
component can contain several core modules (adapters, composed guests), each
with its own memory, and they share one budget. Accounting is fail-closed —
nothing is returned to the budget, so the total can only overstate what the
plugin holds. Growth beyond a memory's or table's own declared maximum is
refused without being counted. Instance, memory and table counts bound the
bookkeeping; a plugin built with `wit-bindgen` needs one instance, one memory
and one table.

Async is disabled; plugins run synchronously.

## Versioning

The WIT package follows semver:

| Bump | Allowed changes |
|---|---|
| patch (`0.1.x`) | bug fixes; no schema change |
| minor (`0.x.0`) | additive: new optional fields, new host-imported interfaces, new exports; existing plugins keep loading |
| major (`x.0.0`) | breaking: removed or retyped fields, removed interfaces |

The host refuses a plugin whose `dq:plugin/*` imports carry a different
major version than the host was built against (`schema_version`, exit 3).
Two consequences of how this is checked today:

- The version is read from the plugin's **imports**. A component that
  imports no `dq:plugin` interface is not version-checked at load; a
  mismatch in its exports surfaces when the host instantiates it (`invoke`
  or `load` error).
- While the major is `0`, a plugin built against a newer minor (say `0.2.0`)
  passes the check against a `0.1.0` host; if it uses an import the host
  lacks, instantiation fails.

The host's WIT and the example's copy (`examples/plugin-rust/wit/world.wit`,
a copy rather than a symlink so Windows clones and source archives work)
must be kept in sync when the version changes.

## Errors and exit codes

`PluginError::kind_name()` is stable and drives the exit code:

| `kind_name` | Cause | Exit |
|---|---|---|
| `feature_disabled` | dq built without `--features plugins` and `--plugins` found a `*.wasm` | 6 |
| `disallowed_import` | the component imports WASI | 6 |
| `schema_version` | different `dq:plugin` major | 3 |
| `malformed_fix` | `fix` returned bytes that are not an `add`/`replace`/`remove` JSON Patch | 3 |
| `load` | unreadable file, not a component, unsupported features (memory64, …), linker failure | 4 |
| `exhausted` | fuel budget used up | 4 |
| `memory` | memory cap hit | 4 |
| `invoke` | any other trap, or `fix` returned `err(message)` | 4 |

The original contract named the exit-4 family `RUNTIME_ERROR`; dq has no
such constant and uses `VALIDATE_FAIL` (4), the "something failed a check"
code, rather than add a new one. See
[reference/cli.md](reference/cli.md#exit-codes).

## Building a plugin (Rust)

The reference plugin in [`examples/plugin-rust/`](../examples/plugin-rust/)
emits one `warn` diagnostic (`example.demo-lint`, no pointer, so it lands at
`1:1`) and an empty fix. It ignores the `ir` and `jq` imports; a real plugin
would call them. It is a standalone crate (its own `[workspace]`, excluded
from dq's workspace), `#![no_std]` with `alloc`, `crate-type = ["cdylib"]`,
and uses `wit_bindgen::generate!({ world: "plugin", path: "wit/world.wit" })`
to get the `Guest` trait (`fn lint()`, `fn fix()`).

Build it as a component **without WASI**:

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked cargo-component
cd examples/plugin-rust
cargo component build --release --target wasm32-unknown-unknown
# -> target/wasm32-unknown-unknown/release/dq_plugin_example_noop.wasm
```

`--target wasm32-unknown-unknown` matters: without it `cargo component`
targets `wasm32-wasip1` and wraps the module with the WASI adapter, the
component then imports `wasi:cli/*` and `wasi:filesystem/*`, and dq refuses
it (exit 6).

Alternative without `cargo-component`: build a core module and wrap it.

```sh
cargo install --locked wasm-tools
cargo build --release --target wasm32-unknown-unknown
wasm-tools component new \
  target/wasm32-unknown-unknown/release/dq_plugin_example_noop.wasm \
  -o target/dq_plugin_example_noop.component.wasm
```

Run it:

```sh
mkdir -p plugins
cp target/wasm32-unknown-unknown/release/dq_plugin_example_noop.wasm plugins/
dq lint --plugins ./plugins config.yaml   # one example.demo-lint warning per file
dq fix  --plugins ./plugins config.yaml   # empty script, file unchanged
```

Any language that can produce a WebAssembly component against this WIT
world (and without WASI imports) works the same way. The host was built with
wit-bindgen 0.63 in mind; plugins built with wit-bindgen 0.57 are known to
load.

## Not part of 0.1.0

- Inline offsets (positions inside scalars, used by composite rules) are not
  passed to plugins.
- No `parse` / `format` exports for new formats; no `check(document, config)`
  per-rule hook.
- No `dq plugin add` installer or registry; plugins are files in a directory
  you pass with `--plugins`.

Open items are collected in [roadmap.md](roadmap.md).
