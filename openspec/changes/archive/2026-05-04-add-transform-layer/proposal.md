## Why

M1–M6 covered ten formats with safe writes, bulk edits, canonicalisation, and a distribution story. What `dq` cannot do yet — and what blocks both the M8 lint engine and a real chunk of the agent-CI use case — is **express transformations more interesting than "set scalar at pointer"**: increment a counter, conditionally rename a key, project across an array of containers, derive one field from another. JSON Pointer + `dq set` cover the simple cases; everything else needs a query language.

The plan's M7 envelope per [dq-plan.md:435-447](../../../dq-plan.md) is "embed jq via jaq, expose `dq query` for read-side jq, and `dq set --jq EXPR` for transform-mode writes". The choice of jq over a homegrown DSL is load-bearing for the M8 linter (rule `check.jq` expressions are the engine's only escape hatch) — committing to jaq now means the linter can ship in M8 without re-litigating the query-language question.

The risk envelope is moderate. The Rust additions are confined to one new crate (`dq-transform`) and two CLI surfaces (`dq query` is brand-new, `dq set` gains one optional flag). The existing M2 textual-edit pipeline is **not** touched: `dq set --jq` deliberately uses the re-emit path (`Format::write_with_options`), the same one `dq fmt` and `dq convert` use, and accepts the same comment-loss cost. This keeps the round-trip contract for `dq set FILE POINTER VALUE -i` byte-identical to M2 — adding `--jq` is purely additive.

The single load-bearing dependency choice — `jaq-core 3.0` + `jaq-std 3.0` + `jaq-json 2.0` — is well-trodden: jaq is the Rust-native jq implementation downloaded ~2M times, MIT-licensed, actively maintained, and already on the workspace's M0 dependency shortlist. The `sync` feature on `jaq-json` makes `Val: Send + Sync`, which is necessary for the rayon-driven bulk path that `dq set --jq 'expr' 'k8s/**/*.yaml' -i --parallel 4` exercises.

## What Changes

### New crate: `dq-transform` (jaq integration)

- **`crates/dq-transform/src/lib.rs`** — public surface:
  - `pub struct JqEngine { filter: jaq_core::Filter<…> }` — owns a compiled jq filter; `Send + Sync + Clone` so a single compiled engine fans out across rayon workers.
  - `JqEngine::compile(expression: &str) -> Result<Self, JqError>` — parses + compiles once, callable many times.
  - `JqEngine::run(&self, input: &serde_json::Value) -> Result<Vec<serde_json::Value>, JqError>` — evaluates the filter against one input and materialises the full output stream.
  - `pub enum JqError` (`thiserror`-based): `Compile { snippet, position, message }`, `Runtime { message }`, `Conversion { message }`, `FeatureDisabled { hint: &'static str }`. `kind_name()` stable for exit-code mapping (returns `"compile"` / `"runtime"` / `"conversion"` / `"feature_disabled"`).
  - `pub fn serde_to_val(&serde_json::Value) -> Result<jaq_json::Val, JqError>` and `pub fn val_to_serde(&jaq_json::Val) -> Result<serde_json::Value, JqError>` — exposed for callers that need finer control than `run()`.
- **`crates/dq-transform/Cargo.toml`** — new dependencies `jaq-core = "3.0"`, `jaq-std = "3.0"`, `jaq-json = "2.0"` (with the `sync` and `serde` features enabled), plus `serde_json`, `thiserror`, `tracing`. The `embedded-jq` cargo feature exists and is **default-on**; with the feature off, `dq-transform` provides a thin `JqEngine` shell whose every method returns `JqError::FeatureDisabled` so downstream `dq query` / `dq set --jq` produce a clear "this build was compiled without `embedded-jq`" error rather than a link-time failure.
- **Workspace plumbing** — three new `[workspace.dependencies]` entries (`jaq-core`, `jaq-std`, `jaq-json`) so per-crate stanzas use `workspace = true`.

### CLI surface

- **`dq query EXPR FILE` (new subcommand).** Reads `FILE` (format detected by extension or `-F`), converts the parsed `Document` to `serde_json::Value`, evaluates the jq expression, renders the resulting stream through the configured `Reporter`. Multi-doc YAML is honoured via `--doc <idx|all>` exactly like the other read commands. `EXPR` is the first positional, `FILE` is the second, mirroring `dq select`'s shape (`select EXPR FILE`). Empty result stream renders as `[]` in JSON and produces no stdout in console mode (mirroring `select`'s "empty match is not an error" rule). Exit codes:
  - 0 — query ran, any number of values produced.
  - 3 — `EXPR` failed to compile (jq parse / type / arity error). Maps to `PARSE_ERROR` so CI scripts can group it with file-parse failures.
  - 1 — runtime error inside the filter (e.g. divide by zero, type-mismatched op). Maps to `GENERIC` because the *file* parsed fine and the *expression* compiled fine — only the evaluation against this specific data failed.
  - 5/6 — standard `IO_ERROR` / `INVALID_INPUT` (read flags rejected, missing file, stdin without `-F`).
- **`dq set --jq EXPR FILE` (new mode of existing command).** When `--jq` is set, `set` becomes a transform: the document is parsed, converted to JSON, the expression is applied (must return exactly one value — multi-output streams are rejected with `INVALID_INPUT`), the result is converted back to a `dq_core::Value`, the document is re-emitted via `Format::write_with_options`. The pointer / value positional args become **optional** when `--jq` is supplied. The handler enforces two distinct rules: (1) clap rejects `--jq` together with a positional VALUE (or `--value-from`) at parse time with `INVALID_INPUT`; (2) at runtime, when `--jq` is present, POINTER must be absent or the single token `/` (the explicit "whole document" pointer, semantically identical to omitting it) — any other pointer value is rejected with `INVALID_INPUT`. `--jq` is also incompatible with the template-guard flags `--allow-templates` / `--raw-template-strings`: the re-emit path does not preserve template placeholders, so the combination is rejected with `INVALID_INPUT` rather than silently dropping the user's flag. All other existing `set` flags (`-i`, `--diff`, `--check`, `--backup`, `--continue-on-error`, `--parallel`, glob expansion) work unchanged through the bulk driver. The comment-preservation contract from M2 textual-edit is **explicitly deferred** for `--jq`: the user gets re-emit semantics (same as `dq fmt`).

### Capabilities

#### New Capabilities

- **`data-query-transform`** — covers the embedded jaq engine, the value adapter (`dq_core::Value` ↔ `jaq_json::Val` ↔ `serde_json::Value`), the `embedded-jq` cargo feature contract, and the `JqEngine` public API. Single home for "how jq plugs into dq" so the M8 linter can depend on this capability rather than reaching directly into a CLI module.

#### Modified Capabilities

- **`cli-shell`** — `Command` enum gains a `Query(QueryArgs)` variant; the M4 anti-scope sentence "the binary SHALL NOT include any of the following commands: `query`, `lint`, …" gets `query` removed from the list (it's now reachable). `--jq` is added to the bulk-driver-aware write-flag matrix as a new mutual-exclusion: rejecting `--jq` together with a positional VALUE (both describe what to write).
- **`data-query-read`** — adds `dq query` as a read-side subcommand with full requirements + scenarios. Inherits the read-flag rejection contract (`-i`, `--diff`, `--backup`, `--check`, `--continue-on-error`, `--parallel` all rejected via `Cli::ensure_no_write_flags`).
- **`data-query-write`** — `dq set` gains an optional `--jq EXPR` flag and the matching positional-args validation. The existing M2 textual-edit splice path stays as the default; `--jq` opts the document into the re-emit path explicitly, with a one-line `tracing::debug!` noting the comment-loss tradeoff so users running `-vv` see why their comments disappeared.

### Meta

- **`dq-plan.md` M7 section.** Marker `✅ Implemented YYYY-MM-DD` plus cross-link to this archived change folder.
- **`README.md`.** Status moves from `M6 alpha — adds installer/completions/man/SARIF/CI/Docker/packaging` to `M7 alpha — adds dq query (jq) + dq set --jq`. New "Examples" subsection demonstrating one `dq query` and one `dq set --jq` invocation.

### What's NOT in M7 (deferred)

- **External `jq` shell-out fallback.** The plan mentions deferring to a system `jq` binary when the `embedded-jq` feature is off; M7 ships the simpler "feature off → clear error" contract instead. Adding shell-out is a follow-up if anyone files an issue asking for it.
- **`dq query --in-place`.** Once `dq set --jq` exists, `query --in-place` is redundant — anything you'd want it for, `set --jq` already does. Reserved as a future ergonomic alias if user feedback demands it.
- **`dq query` over multi-file globs.** Read commands in M1–M6 are single-file; `query` follows the same contract. Bulk-jq is a `dq set --jq` job.
- **jq variables / arguments (`--arg name value`, `--argjson name JSON`, `--slurpfile`).** Useful for advanced users but out of scope for M7's "make jq reachable" goal. Tracked as a follow-up issue once usage settles.
- **Lint engine integration.** M8 will consume `JqEngine` from `dq-transform` directly; M7 only ships the building block, not the consumer.
- **`dq query` SARIF output.** SARIF is for diagnostic-shaped data; query results are arbitrary JSON. The existing `BannedReporter` pattern from M5 covers this (selecting `-F sarif` for `query` produces a structured "wrong reporter for this verb" error).

## Impact

- **Code (`dq-transform` — first non-stub revision)**:
  - `crates/dq-transform/Cargo.toml` — bumps from the M2 placeholder package to a real crate with the jaq dependencies and the `embedded-jq` feature gate.
  - `crates/dq-transform/src/lib.rs` — replaces the M2 `_placeholder()` stub with module declarations and re-exports.
  - `crates/dq-transform/src/jq.rs` — the new `JqEngine` + `JqError` + value adapters. The `embedded-jq` feature gates the heavy implementation; without the feature, the file's contents are a small `cfg`-gated stub.
- **Code (`dq-cli`)**:
  - `crates/dq-cli/Cargo.toml` — new `dq-transform` workspace dep (no feature flags — relies on default `embedded-jq`).
  - `crates/dq-cli/src/cli/args/query.rs` — new `QueryArgs { expression, file }` struct.
  - `crates/dq-cli/src/cli/args/set.rs` — new optional `jq: Option<String>` field on `SetArgs`. Marked `conflicts_with = "value"` and `conflicts_with = "value_from"` at the clap level so most user errors are caught at parse time.
  - `crates/dq-cli/src/cli/args.rs` — re-exports `QueryArgs`; new `Command::Query(QueryArgs)` variant.
  - `crates/dq-cli/src/lib.rs` (`dispatch`) — new arm routing `Command::Query` to `commands::query::run`.
  - `crates/dq-cli/src/commands/mod.rs` — `pub mod query;`.
  - `crates/dq-cli/src/commands/query.rs` — new handler. Reads file → converts to `serde_json::Value` → evaluates via `JqEngine` → renders via reporter. Rejects write flags via `Cli::ensure_no_write_flags`.
  - `crates/dq-cli/src/commands/set.rs` — new branch when `args.jq.is_some()`. The `SetFileOp::apply` path forks: when `jq` is present, parse via the value-only parser, run jq, write via `Format::write_with_options`. The existing splice path remains the default.
  - `crates/dq-cli/src/exit_code.rs` — no new variants needed. jq compile errors map to `dq_core::Error::Parse` (so `PARSE_ERROR = 3`); jq runtime errors stay as `anyhow::anyhow!(...)` and fall through to `GENERIC = 1`.
- **Tests (new)**:
  - `crates/dq-transform/src/jq.rs` `#[cfg(test)] mod tests` — ≥6 unit tests: compile + run identity, compile + run `.foo`, compile error surfaces structured `Compile`, runtime error surfaces `Runtime`, value-adapter round-trip for nulls/bools/ints/floats/strings/arrays/objects/big-int/big-float, multi-output stream returns multiple values.
  - `crates/dq-cli/tests/cli_query.rs` — ≥8 integration tests via `dq::run`: simple `.foo`, multi-output `.[]`, empty match `.nonexistent`, JSONPath-equivalent `.spec.containers[].image`, jq compile error → exit 3, jq runtime error (e.g. `1 / 0`) → exit 1, write flag rejected → exit 6, `-F json` materialises the stream as a JSON array.
  - `crates/dq-cli/tests/cli_set_jq.rs` — ≥6 integration tests: `dq set --jq '.spec.replicas |= . + 1' deploy.yaml -i` increments the field, transform that adds a new key writes the new key, transform that removes a key writes the file without it, `--jq` with a positional VALUE → exit 6, multi-output stream → exit 6, `--diff` mode renders the unified diff.
  - `crates/dq-cli/tests/cli_smoke.rs` — extend with one query smoke + one set --jq smoke.
- **Dependencies (new)**: `jaq-core = "3.0"`, `jaq-std = "3.0"` (default features cover format/log/math/regex/time so `select`, `map`, `length` work), `jaq-json = { version = "2.0", features = ["sync", "serde"] }`. All MIT-licensed; `cargo deny check licenses` passes without amendment.
- **Backward compatibility**: every M1–M6 invocation produces byte-identical output. `dq query` is brand-new; `dq set --jq` is opt-in and disjoint from existing `set` modes. The `Command::Query` variant is the first reachable use of the M4 anti-scope's `query` reservation, so the cli-shell anti-scope wording is updated rather than added to.
- **Project meta**:
  - `dq-plan.md` M7 section gains the implementation marker; the "Tech stack" row for `jaq-core` / `jaq-std` is unchanged but moves from "(M7+)" to dropping the "+" suffix.
  - `README.md` status line and Examples block as above.
