## Why

M3 closed the bulk + transform contract: a single command can edit dozens of files, RFC 6902 / RFC 7396 ops are first-class, and `--check` makes `dq` a pre-commit gate. What it can NOT yet do is **canonicalize** — produce a deterministic, normalized rendering of a file. That is the M4 job per [dq-plan.md:385-397](../../../dq-plan.md): "всё, что связано с «как должен выглядеть результат»."

Concretely, three CI/agent workflows are still painful:

1. **Pre-commit normalization.** Today there is no way to say "format every YAML in this repo to a canonical shape" short of writing a script that loops over `dq convert -i`. The natural verb is `dq fmt` and it is the one M4 must ship.
2. **Stable diffs across machines.** Without deterministic key ordering, two engineers committing the same logical change produce diffs that include unrelated key reorders. The fix is `--sort-keys`, applied at write time so it composes with `set`/`del`/`patch`/`merge`/`fmt` uniformly.
3. **Indent-style consistency.** Different teams want 2-space vs 4-space JSON, indented vs compact YAML lists. `--indent N` covers the common case without re-litigating "tabs vs spaces" (we always emit spaces).

The risk envelope is small. Re-emitting through the format writers does NOT regress M2 textual-edit guarantees — `fmt` is the *one place* where dropping comments and re-canonicalizing whitespace is the explicit user intent. M2's textual-edit pipeline is reserved for `set`/`del`/`patch`/`merge` where comment preservation is the contract.

## What Changes

- **New command `dq fmt <FILE>`** — re-emit the document through its native writer. Produces a canonicalized rendering: deterministic key ordering when `--sort-keys` is set, consistent indentation when `--indent N` is set. Source format is preserved (YAML→YAML, JSON→JSON, TOML→TOML, JSONL→JSONL); cross-format conversion remains the responsibility of `convert -i -F`. Supports `-i` (atomic in-place), `--diff` (per-file unified diff), `--check` (idempotency gate, exit 1 if any file would change), `--backup`, glob expansion, and `--continue-on-error` / `--parallel <N>` from the M3 bulk driver.
- **New global flag `--sort-keys`** — when set, every write path (`set`, `del`, `patch`, `merge`, `fmt`, `convert`) sorts map keys alphabetically before serialization. Threaded through `dq_core::WriteOptions` and honored by every `Format::write_with_options` implementation. Source files where `--sort-keys` was not used remain untouched (the flag affects emission only).
- **New global flag `--indent <N>`** — controls indentation width for indented formats. JSON honors `--indent 2|4`. YAML honors `--indent 2|4` for nested mapping indent. TOML and JSONL ignore the flag (TOML has fixed grammar, JSONL is one-line-per-doc). Default is the writer's existing default (JSON: 2, YAML: 2).
- **`dq-core` extensions** — public `WriteOptions { sort_keys: bool, indent: Option<u8> }` struct in `dq-core` (re-exported from crate root). New trait method `Format::write_with_options(&self, doc, w, opts) -> Result<()>` with a default impl that ignores `opts` and forwards to `Format::write` (so unconverted formats keep working). JSON and YAML writers override the default with options-aware emission. A free helper `dq_core::canonicalize_keys(value)` produces a deep-sorted-keys clone of a `Value` tree without touching the original — used by every writer that wants stable `--sort-keys` output without per-format glue.
- **`validate --check` polish** — the `validate` command already returns 0 (parseable) / 4 (parse error). M4 documents `--check` as an accepted no-op alias for clarity in pre-commit hook entries (the `--check` flag is already on the global surface for `set`/`del`/`fmt`; allowing it to flow through `validate` without rejection makes hook entries uniform: `dq validate --check $FILES`).
- **`.pre-commit-hooks.yaml`** — repo-level config with two ready-to-use entries: `dq-fmt-check` (runs `dq fmt --check $FILES`) and `dq-validate` (runs `dq validate $FILES`). Lets users add `dq` to their `pre-commit` chain in three lines of `.pre-commit-config.yaml`.
- **Anti-scope deferred to later milestones (NOT shipped in M4):** `--quote-style double|single|auto`, `--flow-style block|flow|auto`, `--strip-comments`. These three flags are listed in [dq-plan.md:391](../../../dq-plan.md) for M4, but each one requires a comment-preserving emitter that today's `serde_yml` / `toml_edit::DocumentMut` writers do not surface. The textual-edit pipeline (M2) DOES preserve comments, but it splices spans, not re-emits — so it cannot retroactively choose quote/flow style. Implementing these three needs the saphyr-emitter rewrite explicitly listed as out-of-scope in [openspec/changes/archive/2026-05-03-add-safe-writes/spec.md](../../changes/archive/2026-05-03-add-safe-writes/spec.md) (saphyr-parser scanner discards comment tokens before the event stream, [issue #103](https://github.com/saphyr-rs/saphyr/issues/103)). M4 ships the two flags whose value is unambiguous (`--sort-keys`, `--indent`) and explicitly defers the others; the deferral is documented in `dq-plan.md` and on the `dq fmt --help` text.

**What's NOT in M4** (per [dq-plan.md:395](../../../dq-plan.md)): linter autofix (M10); jq-driven transforms (M7); markdown / tree formats (M9); the three deferred flags above.

## Capabilities

### New Capabilities

- `data-query-fmt`: `dq fmt` command contract — re-emit through native writer, glob-aware via the M3 bulk driver, supports `-i`/`--diff`/`--check`/`--backup`/`--continue-on-error`/`--parallel`, source-format preserved, comments dropped (intentional), `--sort-keys` and `--indent` honored.

### Modified Capabilities

- `cli-shell`: adds `--sort-keys` and `--indent <N>` as global flags. `Cli::ensure_no_write_flags` accepts both (read commands tolerate them, since they are emission-only and a no-op on read). Anti-scope updates: M4 adds `fmt`; reserves `query`, `lint`, `check` (linter), `test`, `fix`, `explain`, `rules`, `self`, `init`, `config` for later.
- `format-support`: introduces `Format::write_with_options(doc, w, opts)` with default-forward-to-`write` impl. JSON and YAML implementations override it to honor `WriteOptions::sort_keys` and `WriteOptions::indent`. TOML and JSONL keep the default (and the M4 spec documents that `--indent` / `--sort-keys` are no-ops for them).
- `data-query-write`: `set`, `del`, `patch`, `merge` write paths thread `WriteOptions` through to the renderer. The textual-edit pipeline ignores `--sort-keys` (it splices into existing bytes; reordering would defeat the M2 round-trip contract); `--sort-keys` only affects re-emit paths (`fmt`, `convert -i`). The CLI documents this in `--help` so users are not surprised.

(Capabilities `data-query-read`, `path-syntax`, `template-guard`, and `data-query-bulk` are not modified — read commands ignore the new flags, JSON Pointer is unchanged, the template guard already covers `fmt`'s parse step the same way it covers `set`'s, and the M3 bulk driver gains no new behaviour.)

## Impact

- **Code (new)**:
  - `crates/dq-core/src/write_options.rs` — `WriteOptions` struct + `canonicalize_keys` helper.
  - `crates/dq-cli/src/cli/args/fmt.rs` — `FmtArgs` struct.
  - `crates/dq-cli/src/commands/fmt.rs` — handler entry point.
- **Code (changes)**:
  - `crates/dq-core/src/lib.rs` — re-export `WriteOptions`, `canonicalize_keys`.
  - `crates/dq-core/src/format.rs` — add `Format::write_with_options` with default forwarding to `write`.
  - `crates/dq-core/src/parsers/json.rs` — override `write_with_options` to honor `sort_keys` + `indent`.
  - `crates/dq-core/src/parsers/yaml.rs` — override `write_with_options` to honor `sort_keys` (indent for YAML stays at the writer's default in M4 because `serde_yml` does not expose indent configuration; documented).
  - `crates/dq-core/src/parsers/toml.rs` — override `write_with_options` to honor `sort_keys` (`toml_edit::DocumentMut::sort_values_by` exists for it). `indent` ignored (TOML grammar has fixed indent for nested tables).
  - `crates/dq-core/src/parsers/jsonl.rs` — override to honor `sort_keys` + `indent` per-line.
  - `crates/dq-cli/src/cli/args.rs` — add `sort_keys: bool` and `indent: Option<u8>` global flags. Update `ensure_no_write_flags` to accept both (read tolerance).
  - `crates/dq-cli/src/lib.rs` — dispatch `Command::Fmt(args)`. Build `WriteOptions` from `Cli` once and thread to handlers.
  - `crates/dq-cli/src/commands/convert.rs` — accept `WriteOptions` and pass to `write_with_options`.
  - `crates/dq-cli/src/commands/set.rs`, `del.rs`, `patch.rs`, `merge.rs` — accept `WriteOptions` and document the `--sort-keys` no-op for textual-edit splice paths.
  - `crates/dq-cli/src/commands/validate.rs` — relax `ensure_no_write_flags` to accept `--check` as no-op (validate is already a check-only command).
- **Dependencies**: none new. `WriteOptions` is a plain struct, sort/indent options use already-vendored facilities of `serde_json` / `serde_yml` / `toml_edit`.
- **Tests**:
  - `crates/dq-core/tests/write_options.rs` — unit tests: `canonicalize_keys` is deep, stable, and idempotent; JSON `write_with_options(sort_keys=true, indent=4)` produces 4-space indented sorted JSON; YAML/TOML sort_keys round-trip.
  - `crates/dq-cli/tests/unit_fmt.rs` — handler tests: stdout mode, `-i` writes back, `--check` exits 1 on non-canonical files, glob with mixed canonical/non-canonical, source format preserved (YAML stays YAML).
  - `crates/dq-cli/tests/cli_smoke.rs` — additional smoke for `dq fmt --sort-keys --check 'k8s/**/*.yaml'`.
  - `crates/dq-cli/tests/cli_snapshots.rs` — snapshot for `fmt --diff` per-file marker, `fmt --check` summary.
  - Existing M3 golden suite re-runs unchanged — `WriteOptions::default()` is the no-op identity, so untouched code paths produce byte-identical output.
- **Backward compatibility**: every existing M3 invocation produces identical bytes (the new `WriteOptions::default()` matches today's writer behaviour exactly; threading through `_with_options` is a no-op when the user passes no flags). The new globals (`--sort-keys`, `--indent`) are additive — code paths that did not name them previously could not parse them, and `dq` previously rejected them with `unrecognized argument` (exit 6). M3 anti-scope (`Glob pattern`, `--sort-keys is unknown in M3`) lifts the latter; the M4 anti-scope tightens around the four still-deferred subcommands.
- **Project meta**:
  - `dq-plan.md` M4 section gains a `✅ Implemented YYYY-MM-DD` marker at archive time.
  - `README.md` status moves from `M3 alpha — read + write + bulk + CI` to `M4 alpha — adds dq fmt + --sort-keys + --indent`.
  - `.pre-commit-hooks.yaml` lands at repo root with two entries.
