# Architecture

dq does two jobs in one binary: it reads, edits and converts structured data
files without damaging them, and it is a platform for writing linters over
those files with nothing but YAML and jq. This page gives the principles
behind it and how the code is layered. Behaviour in detail is in
[reference/](reference/); why things were decided the way they were is in
[decisions.md](decisions.md); the original Russian plan these principles come
from is archived as [archive/dq-plan-2026-05.md](archive/dq-plan-2026-05.md).

## Who it is for

Developers, DevOps engineers, and automated users: AI agents and CI
pipelines. The third group is the one that shapes the design. An agent cannot
read a stack trace or guess at a tool's private syntax, so everything dq
prints, accepts and returns is meant to be predictable and machine-readable.

## Principles

- **Agent-first.** Errors are structured: file, line, column, the offending
  snippet, the deepest path that did resolve, and did-you-mean suggestions.
  Every command does one thing with no hidden state. Exit codes are a stable,
  documented contract. Paths are JSON Pointers, not a dq dialect, so there is
  no escaping to get wrong.
- **Round-trip safety before features.** A read-modify-write cycle keeps
  comments, blank lines, key order, quote style, anchors and aliases, flow vs.
  block layout, line endings and number precision. This was built first
  (milestone M2) because everything after it depends on edits not damaging
  files; a feature that cannot keep the round trip is not shipped as an
  in-place edit.
- **Format-agnostic.** One document model behind one `Format` trait means the
  same commands (`get`, `set`, `del`, `lint`, …) work on every format, and a
  new format reaches every command by being registered.
- **Composable.** Read stdin, write stdout, work in a pipeline. No prompts,
  no spinners, the same bytes in a terminal and through a pipe.
- **Honest scope.** No query language of our own: JSON Pointer for paths,
  RFC 9535 JSONPath for `select`, jq (embedded jaq) when expressions are
  needed. No policy language of our own either: rules are jq or JSON Schema.
- **Standard formats out.** RFC 6901 pointers, RFC 6902 JSON Patch, RFC 7396
  Merge Patch, JSON Schema 2020-12, SARIF, JUnit, TAP — nothing proprietary in
  what dq emits.

## Crates

Dependencies point downwards only:

- `dq-core` — depends on no other dq crate;
- `dq-transform` → `dq-core`;
- `dq-lint` → nothing at runtime (its tests use `dq-exec`);
- `dq-exec` → `dq-core`, `dq-transform`, `dq-lint`;
- `dq-plugin` → `dq-core`, `dq-transform`, `dq-exec`;
- `dq-cli` → all of the above.

| Crate | Role |
|---|---|
| `dq-core` | The data layer. `Document` and `Value`, the `Format` trait and every parser and writer, JSON Pointer, the span map and textual-edit machinery, the IR with provenance, `EditScript`, RFC 6902 / 7396 / diff engines, atomic writes, the template guard. No dependency on any other dq crate. |
| `dq-transform` | The embedded jq engine (jaq) and the value adapters between dq values and jq values, behind the default `embedded-jq` feature. |
| `dq-exec` | The lint runtime: rule schema, rule loading, the evaluator (format/glob/filter match, check, diagnostics), JSON Schema and composite checks, the fixer, the fixture test runner. |
| `dq-lint` | The `@std` rule library as data: rule YAML, fixtures and schema sidecars embedded with `include_str!`. No runtime code of note. |
| `dq-plugin` | The WASM plugin host (wasmtime, Component Model, WIT `dq:plugin@0.1.0`), behind the `plugins` feature; without it, a stub that returns `FeatureDisabled`. See [plugin-abi.md](plugin-abi.md). |
| `dq-cli` | The `dq` binary: clap argument parsing, command handlers, reporters, the bulk driver, exit-code mapping. A thin layer over the crates above. |

Outside the workspace: `examples/plugin-rust/` (reference plugin, its own
workspace) and `spikes/` (throwaway investigations, not built).

## How data flows

1. **Detect and parse.** The CLI picks a `Format` (by `-F`, extension or file
   name) and parses the bytes into a `Document`: a `Value` tree with source
   key order and exact numbers. YAML, JSON and TOML parse in a write-aware
   mode that also records a span map — pointer → byte range — over the
   original bytes; YAML streams keep every document.
2. **Read.** Read commands navigate the `Value` with a pointer, JSONPath or a
   jq program and hand the result to a reporter.
3. **Write — two paths.**
   - *Textual edit* (`set`, `del`, `patch`, `merge`, rule `fix.ops`, plugin
     fixes): splice only the bytes of the changed node using the span map,
     then re-read the result to prove it reads back as exactly the requested
     change. This is how comments survive. YAML, JSON and TOML only.
   - *Re-emit* (`convert`, `fmt`, `set --jq`, rule `fix.jq`): write the whole
     value through the format's writer. Works for every writable format;
     comments are lost.

   Either way the file is replaced atomically (temp file in the same
   directory, then rename).
4. **Lint.** The document's `Ir` view (value + provenance + source bytes) goes
   to the evaluator, which runs every matching rule and maps findings back to
   `line:col` through provenance. Fixes come back as edit scripts or whole
   values and go through the write paths above.

## The binary's conventions

Kept on purpose; see [reference/cli.md](reference/cli.md#how-the-binary-is-built):

- `main.rs` only restores `SIGPIPE`, parses arguments, sets up `tracing`,
  wires dependencies and maps the result to an exit code.
- Handlers return values and write through an injected `Reporter`; they never
  print directly. Logs go through `tracing` to stderr, never `println!`.
- Errors are typed per crate (`thiserror`), handled with `anyhow` at the top,
  and mapped to fixed, documented exit codes through stable `kind_name()`
  strings.
- Paths are `camino::Utf8PathBuf`; colour is decided once and passed down as a
  parameter.
- Release builds use `lto`, one codegen unit and `strip`, for a small single
  binary.

## Extending dq

- **A format**: implement `Format` in `crates/dq-core/src/parsers/<name>.rs`,
  register it in `parsers::registry()` (and in the filename table in
  `crates/dq-core/src/format.rs` if it has no telling extension), add
  round-trip tests under `crates/dq-core/tests/`.
- **A `@std` rule**: add `<rule>.yml` and `<rule>.test.yml` under
  `crates/dq-lint/rules/<namespace>/`; `cargo test -p dq-lint` runs the
  fixtures.
- **A command**: clap args in `crates/dq-cli/src/cli/`, the handler in
  `crates/dq-cli/src/commands/`, exit codes in `crates/dq-cli/src/exit_code.rs`.
- **Checks that jq cannot express**: a WASM plugin
  ([plugin-abi.md](plugin-abi.md)).

Anything user-visible or cross-crate starts with a plan in
[plans/](plans/); when it ships, it gets a dated entry in
[decisions.md](decisions.md) and the reference pages are updated.
