## Why

M2 closed the read+write contract for a single file at a time. M3 adds the multi-file dimension: **one command — many files** (`dq set 'k8s/**/*.yaml' /spec/replicas 3 -i`) and **one file — many ops** (`patch` / `merge` / `diff`). Without these, every CI/CD use case still has to shell-out to `find … -exec` or hand-roll a loop, which is exactly the friction the project exists to remove ([dq-plan.md:371-383](../../../dq-plan.md)).

M3 is also where the project earns the "agent-friendly" badge: the `--check` mode (exit 1 on changes pending) makes `dq` a clean pre-commit gate; structural `diff` between two files is what an agent uses to summarise drift without parsing unified output; `patch` lets an agent ship a list of operations as data instead of a script. RFC 6902 + RFC 7396 are the two interchange formats the agent ecosystem already speaks (jsonpatch, kubernetes JSON Patch admission, AWS CloudFormation transforms, etc.), so we don't invent a fourth dialect.

The risk is contained. Round-trip safety is M2's job (already done). Parallel write is the only new operational risk and it is bounded: every write goes through the existing `atomic_write` helper, so per-file atomicity carries over unchanged. Cross-file consistency is intentionally **not** a goal — a partial bulk failure leaves successful files persisted and the report names which ones failed. That matches the operational expectation users have around `xargs`/`parallel` and avoids the multi-file two-phase-commit rabbit hole.

## What Changes

- **New command `dq patch <FILE> <OPS>`** — apply an [RFC 6902](https://www.rfc-editor.org/rfc/rfc6902) JSON Patch document. `<OPS>` accepts the same shapes as `set`'s value source: inline JSON literal, `@<path>`, `--ops-from <path>`, `-` for stdin. A simplified line-format (`<op> <pointer> [value]` per line) is also accepted via `--line-format` for hand-written patches. Operations supported: `add`, `remove`, `replace`, `move`, `copy`, `test`. A failed `test` op aborts the whole patch and returns the original document unchanged (RFC 6902 §5).
- **New command `dq merge <FILE> <PATCH>`** — apply an [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396) JSON Merge Patch. Same value-source shape as `patch`. `null` in the patch removes the key; objects merge recursively; everything else replaces.
- **New command `dq diff <A> <B>`** — emit a structural diff between two documents as a JSON Patch document by default (`-F json`). With `--unified` flag, emits a textual unified diff over the rendered representations (uses the existing `similar` integration). With `--format json|yaml|toml`, the JSON Patch ops are rendered through the matching reporter for consistency with the rest of the CLI. The intent is "as data" by default; "as text" only when explicitly asked.
- **Multi-file glob driver for write commands** — `dq set`, `dq del`, `dq patch`, `dq merge`, and (per §8 below) `dq convert` all accept a glob pattern as `<FILE>`. When the argument matches more than one file via `globset`, the operation runs across every match and a summary line is printed at the end (`Modified: N, Skipped: M, Failed: K`). When the argument resolves to a single literal file (or matches exactly one file), behaviour is byte-identical to M2 — no summary, no glob expansion side effects.
- **Bulk-mode flags** — `--continue-on-error` (don't abort on first failure; collect into the summary), `--parallel <N>` (run up to N writes in parallel via `rayon`; default `1`, `0` means CPU count), `--check` (exit 1 if any file would be modified; do NOT write — the idempotency check used in pre-commit / CI hooks). `--check` is the third output mode alongside `-i` / `--diff` and is mutually exclusive with both.
- **`convert -i` (in-place format conversion)** — the M2 deferral comes due. `dq convert deploy.yaml -i -F json` reads `deploy.yaml`, writes `deploy.json` (file extension swapped), and removes the original. Combined with the glob driver: `dq convert 'manifests/*.yaml' -i -F json` converts a directory in one command. `--keep-source` preserves the original alongside the new file.
- **Exit code mapping for bulk mode** — partial success returns exit 7 (`WRITE_FAILED`) with the summary on stderr. Total success on every matched file returns 0. `--check` returns 1 when changes are pending and 0 when every file is up-to-date — distinct from `WRITE_FAILED` because no write was even attempted.
- **dq-core extensions** — `PatchOp` enum + `apply_patch(&mut Document, &[PatchOp])` (RFC 6902 semantics on `Document::set_at` / `del_at` primitives), `apply_merge(&mut Document, &Value)` (RFC 7396 semantics on the same primitives), `diff(&Value, &Value) -> Vec<PatchOp>` (structural recursion producing minimal `replace`/`add`/`remove` ops). All three live in a new `dq-transform` module that re-exports through `dq-core` for ergonomics — but behind a `dq_transform::*` namespace so the M7 jaq adapter can land alongside without a re-org.

**What's NOT in M3** ([dq-plan.md:381](../../../dq-plan.md)): `--sort-keys` / `--quote-style` / `--indent N` / `--flow-style` (formatting flags — M4); `dq fmt` (M4); jq-driven transforms — `dq query` / `set --jq` (M7); linters (M8); markdown (M9); `--strict` order-sensitive comparison (out of scope per `dq-plan.md`); two-phase-commit transactional bulk write (no roadmap entry, intentional anti-scope).

## Capabilities

### New Capabilities

- `data-query-bulk`: bulk-mode glob expansion contract, `--parallel` semantics, `--continue-on-error`, `--check`, summary reporter, partial-success exit-code mapping. Covers every write command (`set`, `del`, `patch`, `merge`, `convert`) uniformly so the contract is documented once.

### Modified Capabilities

- `data-query-write`: lifts the M2 anti-scope on `convert -i` (now in scope), on `patch`/`merge`/`diff`-between-files (now in scope), and on glob expansion (now expanded). The single-file semantics from M2 remain the contract when `<FILE>` resolves to a single match.
- `cli-shell`: adds `--check`, `--continue-on-error`, `--parallel <N>`, and the bulk summary reporter to the global flag surface. `Cli::ensure_write_flags_consistent` learns the new mutual-exclusion rules (`--check` ⊥ `-i`, `--check` ⊥ `--diff`). `--diff` becomes valid for bulk mode (each file's diff is preceded by a `=== <file> ===` marker).
- `format-support`: no behavioural change to existing parsers/writers — but `dq-transform` adds a new public surface (`PatchOp`, `apply_patch`, `apply_merge`, `diff`) that consumers will reach through `dq_core::transform::*`.

(Capabilities `data-query-read`, `path-syntax`, and `template-guard` are not modified — read commands still don't accept globs, JSON Pointer is unchanged, and the template guard already covers the new write commands via the same `Document::set_at` / `del_at` plumbing.)

## Impact

- **Code (new)**:
  - `crates/dq-core/src/transform/mod.rs` — public re-exports (`PatchOp`, `apply_patch`, `apply_merge`, `diff`).
  - `crates/dq-core/src/transform/patch.rs` — RFC 6902 `PatchOp` enum + apply engine over `Document::set_at` / `del_at`.
  - `crates/dq-core/src/transform/merge.rs` — RFC 7396 merge engine over the same primitives.
  - `crates/dq-core/src/transform/diff.rs` — structural diff producing `Vec<PatchOp>` (minimal `replace`/`add`/`remove`).
  - `crates/dq-cli/src/cli/args/patch.rs`, `merge.rs`, `diff.rs` — new `*Args` structs.
  - `crates/dq-cli/src/commands/patch.rs`, `merge.rs`, `diff.rs`, `convert.rs` (extend existing) — handler entry points.
  - `crates/dq-cli/src/bulk.rs` — glob expansion, parallel driver (rayon), summary reporter, `--check` aggregation. Visible to handlers as `bulk::run_per_file(...)` so each command stays a thin shell.
- **Code (changes)**:
  - `crates/dq-cli/src/cli/args.rs` — `--check`, `--continue-on-error`, `--parallel <N>` as global flags. `ensure_write_flags_consistent` learns the new rules.
  - `crates/dq-cli/src/lib.rs` — `dispatch` learns three new variants (`Patch`, `Merge`, `Diff`).
  - `crates/dq-cli/src/exit_code.rs` — no new constants; documentation updates only (the bulk partial-success path reuses `WRITE_FAILED = 7`).
  - `crates/dq-cli/src/commands/set.rs` / `del.rs` — wrap their existing single-file logic in the `bulk::run_per_file` driver.
  - `crates/dq-cli/src/commands/convert.rs` — accept `-i`, glob expansion, `--keep-source`.
- **Dependencies (new)**:
  - `globset` (workspace) — pattern expansion. Preferred over `glob` because `globset::GlobSet` is allocation-efficient for the multi-pattern case (`'k8s/**/*.yaml' 'helm/**/*.yaml'`) and we want stable behaviour on the always-on `**` semantics.
  - `rayon` (workspace) — `--parallel` driver. Already listed in dq-plan.md tech stack but not yet pulled in. Used only in `crates/dq-cli/src/bulk.rs`.
- **Tests**:
  - Golden runner for bulk: `crates/dq-cli/tests/cli_bulk.rs` — happy path (10 files, 1 failing → `Modified: 9, Failed: 1`, exit 7 with `--continue-on-error`), `--check` (exit 1 when changes pending), `--parallel 4` runs (smoke).
  - Snapshot suite for `patch` / `merge` / `diff`: `crates/dq-cli/tests/cli_patch_merge_diff.rs` with insta. Round-trip property: `apply_patch(diff(A, B), A) == B` (proptest in `dq-core`).
  - Integration tests for `convert -i` (cross-format swap, glob).
  - Existing M2 golden suite re-runs unchanged — bulk mode is a NOP when the glob matches exactly one file.
- **Backward compatibility**: M2 single-file behaviour is preserved bit-for-bit when `<FILE>` resolves to one match. Globs containing `*`/`?`/`[`/`{` that previously fell through to `IO_ERROR=5` (no such file) now expand — this is an *intentional* break documented in the M2 anti-scope scenario `Glob pattern is not expanded`. Users who pass literal special characters in filenames must quote them through their shell as before. Commands' exit-code semantics on the single-file path are unchanged.
- **Project meta**:
  - `dq-plan.md` M3 section gets a "✅ Implemented YYYY-MM-DD" marker after archive.
  - `README.md` status line moves from `M2 alpha — read + write` to `M3 alpha — read + write + bulk + CI`.
  - `deny.toml` — no new ignores; rayon and globset are clean.
