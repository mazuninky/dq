# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `loc.pointer` rule field (jq expression returning a JSON Pointer string).
  When set, the lint evaluator looks up the pointer in the input document's
  provenance map and resolves the diagnostic's `(line, col)` from the
  parser-recorded byte span — no more `loc.line` int-coercion to recover
  positions for span-aware formats (YAML, JSON, TOML).
- `dq_core::Ir::line_col_for(&Pointer) -> Option<(u32, u32)>` for callers
  that need the `(line, col)` of a pointer's value through the IR.
- `dq_transform::ir_to_val` / `dq_transform::val_to_owned_ir` IR-aware
  variants of the value adapter (provenance is dropped on the way into jaq
  and reconstructed as `Provenance::Synthetic { Computed }` on the way out
  — see `data-query-transform` spec for the contract).
- `dq_core::Value::to_serde_json` and `dq_core::Value::from_serde_json`
  promoted from private duplicates in `dq-cli` and `dq-core::transform` to
  a single public API.
- `fix.ops` rule field (jq expression returning a JSON Patch array). When
  set, the autofix engine applies the patch via `EditScript::apply`
  against the parsed document, preserving comments and surrounding bytes.
  `add`, `replace`, and `remove` ops are supported (RFC 6902 subset;
  `move` / `copy` / `test` rejected).
- `dq_core::{EditOp, EditScript}` re-exported at the crate root — the
  per-rule edit vocabulary used by `Fixer` and (later) WASM plugins.
- `@std/npm/has-license` migrated to `fix.ops` as the reference rule with
  comment-preserving autofix. The migration covers the empty-string-value
  case (`/license: ""`); the missing-key case is deferred until mkdir-p
  insertion lands in `Document::set_at`.
- Plugin ABI v0.1.0 (experimental). New `dq-plugin` crate exposes
  `PluginRuntime` over WIT + Component-Model wasmtime. The WIT package is
  `dq:plugin@0.1.0` with host-imported `ir` (read-only document access)
  and `jq` (compile/eval against the document) interfaces, plus a
  `world plugin` that exports `lint() -> list<diagnostic>` and
  `fix() -> result<list<u8>, string>`. Plugins run sandboxed: no WASI,
  no filesystem / network / process control, ~1s of CPU per invocation
  (fuel budget), 64 MiB linear-memory cap. New global `--plugins <DIR>`
  flag on `dq lint` / `dq fix` discovers `*.wasm` files non-recursively
  under `<DIR>` (lexical sort) and loads them through the runtime.
  Feature-gated behind `--features plugins`; without the feature the
  flag still parses, but encountering any `*.wasm` errors with exit `6`
  (`InvalidInput`). Breaking changes to the WIT schema and marshalling
  shapes are possible before `v1.0.0`. See `examples/plugin-rust/` for a
  working Rust reference plugin and the full build recipe.

### Changed

- `dq_exec::Evaluator::evaluate_file` now takes `&dq_core::Ir<'_>` instead
  of `&serde_json::Value`. Internally the evaluator still feeds `serde`
  values into jaq; the `Ir` is required so the new `loc.pointer` chain has
  access to the provenance map.
- `dq lint` (and `dq check`) now route YAML and JSON inputs through the
  span-aware parsers, so `loc.pointer`-using rules emit accurate
  `(line, col)` instead of falling through to `(1, 1)`.
- `@std/k8s/image-pull-policy-always` migrated to `loc.pointer`; its
  `check.jq` now emits a pointer per violation that anchors at the
  offending container's `name:` scalar.
- `dq_exec::Fixer::apply` now takes `&mut Document` instead of
  `&serde_json::Value`. The CLI's `dq fix` handler routes the post-fix
  document's bytes directly to disk when only `fix.ops` rules ran,
  preserving comments byte-for-byte. Legacy `fix.jq` rules continue to
  re-emit through the format writer (same comment-loss trade-off as
  `dq set --jq`).
- `dq_exec::FixOutcome` lost its `new_value: serde_json::Value` field;
  the document itself is now the source of truth. A new
  `legacy_jq_applied: bool` field tells the CLI which output path to
  take.
- `RuleFix` schema now accepts both `jq` (legacy) and `ops` (new) fields,
  each `Option<String>`. At least one must be set; both is allowed and
  `ops` wins at runtime (with a `tracing::warn!` shadowing log).

### Deprecated

- `loc.line` jq override. Backwards-compatible fallback when `loc.pointer`
  is unset or fails to resolve to a span. Removal is deferred to a future
  change once the `@std/*` rule library has fully migrated.
- `fix.jq` whole-document jq fixes. Backwards-compatible; new rules
  should prefer `fix.ops` for comment preservation. Removal is deferred
  to a future change once the `@std/*` rule library has fully migrated.
