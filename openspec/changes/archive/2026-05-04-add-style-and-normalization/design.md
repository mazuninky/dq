## Context

M3 closed the multi-file write story for CI/agent workflows: bulk write across globs, RFC 6902 / RFC 7396 ops, structural diff, and a `--check` idempotency gate. The remaining gap is **canonicalization** — given two engineers commiting the same logical change to the same YAML, the diff today can include unrelated key reorders or inconsistent indents. The user-visible verb for "make this look right" is `dq fmt`.

The technical risk is low because the heavy lifting is already done:

- **Re-emission infrastructure exists.** `Format::write` is the read-mode pipeline used by `dq convert`. Adding `write_with_options(opts)` is a thin overload.
- **Bulk driver exists.** `bulk::run_per_file` already handles glob expansion, `-i`, `--diff`, `--check`, `--continue-on-error`, `--parallel`. `dq fmt` is a new `FileOp` that plugs into it.
- **Atomic write exists.** `dq_core::atomic_write::write` is unchanged.

**Current state:** M3 archived (`2026-05-03-add-bulk-and-ci`). Active changes: `add-style-and-normalization` (this document).

**Constraints:**

- M4 plan-text lists five flags: `--sort-keys`, `--indent N`, `--flow-style`, `--quote-style`, `--strip-comments`. Three of them (`--flow-style`, `--quote-style`, `--strip-comments`) require comment-preserving emitters that the saphyr-parser scanner cannot produce ([issue #103](https://github.com/saphyr-rs/saphyr/issues/103)) — these are deferred to a future milestone with a custom emitter or a different YAML library. M4 ships only `--sort-keys` and `--indent N`. The deferral is documented in `dq-plan.md`'s M4 update and on `dq fmt --help`.
- Conventions from `/rust-cli` skill are unchanged: thin `main.rs`, Reporter with DI, exit codes as named constants, no `println!` outside `main.rs`/Reporter implementations.
- Rust code edits are delegated to `rust-cli-writer` / `rust-cli-test-writer` per `.claude/rules/rust-delegation.md`.
- M3 single-file behaviour and golden snapshots stay bit-identical — `WriteOptions::default()` produces byte-identical output to the M3 writers.

**Stakeholders:**

- Pre-commit hook authors: need `dq fmt --check $FILES` and `dq validate $FILES` entries that exit 0/1 on the right semantics.
- AI agents in CI: need stable diffs (sort-keys) and predictable JSON indent.
- Future milestones: M5 format expansion will likely add more `WriteOptions` fields (per-format quote/flow controls); M10 autofix will use `WriteOptions` to render fix output; M7 jq-driven transforms benefit from canonicalization too.

## Goals / Non-Goals

**Goals:**
- `dq fmt deploy.yaml --sort-keys -i` rewrites the file with alphabetically sorted map keys at every depth, byte-identical except for the reorder and any indentation changes.
- `dq fmt --check 'k8s/**/*.yaml'` exits 0 when every file is already canonical, exit 1 with a per-file summary otherwise. Same exit-code semantics as M3 `set --check`.
- `dq fmt deploy.yaml --diff` writes a unified diff to stdout without modifying the file.
- `dq set deploy.yaml /spec/replicas 5 --sort-keys -i` accepts the flag without error and DOES NOT reorder existing keys (textual-edit splice keeps original byte order; the `--sort-keys` flag only affects re-emit paths). The `dq fmt` follow-up is the documented way to canonicalize after `set`.
- `dq convert deploy.yaml -F json --indent 4` renders 4-space JSON.
- `.pre-commit-hooks.yaml` provides ready entries.

**Non-Goals:**
- `--quote-style` / `--flow-style` / `--strip-comments` (deferred — see Constraints).
- Per-format formatting flags (`--yaml-flow-arrays`, `--toml-pad-keys`). One global surface across all formats; per-format finesse is a later refinement.
- Rewriting the textual-edit pipeline to support sort-keys. M2's M2 splice-into-bytes contract remains unchanged; `dq fmt` is the canonical re-emit path.
- Watch mode (`dq fmt --watch`). Out of scope; users compose with `entr`/`watchexec`.

## Decisions

### D1. `dq fmt` re-emits through `Format::write_with_options` — does NOT use textual-edit

**Decision:** `dq fmt` parses the file, then calls `format.write_with_options(&doc, &mut buf, &opts)` to produce output bytes. It does NOT touch `Document::original_bytes` or `set_at`/`del_at`. This is the explicit user contract for `fmt`: comments, anchor names, and quote-style hints are not preserved (they are re-emitted by `serde_yml` / `serde_json` / `toml_edit` defaults). This matches the canonical formatter contract: comments are not preserved.

**Alternatives:**
- Try to preserve comments by walking the textual-edit span tree and rewriting nodes in place: needs a comment-preserving emitter we do not have. Out of scope.
- Make `fmt` a no-op when `--sort-keys`/`--indent` are not set: feels surprising; users expect `fmt` to canonicalize whitespace and indentation regardless.

**Trade-offs:** comments are lost on `fmt`. Documented prominently in `--help` and the M4 README diff. For users who want sort-key without losing comments, the answer is "wait for the comment-preserving emitter rewrite (M5+) and use `dq fmt` then" — not "stack splice on top of textual-edit."

### D2. `WriteOptions` lives in `dq-core`, not `dq-cli`

**Decision:** `pub struct WriteOptions { sort_keys: bool, indent: Option<u8> }` ships from `dq-core`. The `Format` trait gains `fn write_with_options(&self, doc, w, opts) -> Result<()>` with a default impl that ignores `opts` and forwards to `write`. Per-format implementations override the default.

This puts options in the library, not the CLI, so M5+ format expansions and M7 jaq adapter can reuse them. CLI just builds `WriteOptions` from `Cli` flags and threads it to handlers.

**Alternatives:**
- Per-handler ad-hoc `sort_keys: bool` parameter in every command handler: leaks the abstraction, multiplies signature noise.
- Builder pattern (`WriteOptions::builder().sort_keys(true).build()`): unnecessary ceremony for two fields. Plain struct + `Default` is enough; can switch to builder when fields exceed five.

**Trade-offs:** struct is `non_exhaustive` so adding fields in M5+ does not break consumers — they must use `..Default::default()` to construct.

### D3. `canonicalize_keys` is a free helper in `dq-core`, not an `impl Value` method

**Decision:** `dq_core::canonicalize_keys(value: &Value) -> Value` returns a deep-sorted-keys clone. Each writer that wants `--sort-keys` calls it once before serialization (and arrays are walked recursively).

The reason it is not `Value::canonicalize_keys(&mut self)` is that we never want to mutate the in-memory `Value` tree the user got from `parse` — that tree is the source of truth for the textual-edit pipeline (M2). Sort-keys is a write-time projection.

**Alternatives:**
- Push sort-keys into each `serde_yml` / `serde_json` / `toml_edit` emit call individually: `serde_json` does not expose key ordering at write-time when going through `serde_json::Value` (which is `IndexMap`-backed in our build via the `preserve_order` feature). The cleanest place to apply sort-keys is BEFORE handing the value to the writer.
- Implement `Value::sort_keys_recursive(&mut self)` and call it on a clone: same outcome, just less ergonomic. Free function it is.

**Trade-offs:** allocation (deep clone) on every sort-keys write. Acceptable for human-scale documents (<1MB); benchmarks deferred until a regression report appears.

### D4. `--sort-keys` and `--indent` are GLOBAL flags

**Decision:** both flags live on `Cli` with `global = true`, the same level as `-i`/`--diff`/`--check`. They affect every write path uniformly: `set`, `del`, `patch`, `merge`, `convert -i`, `fmt`. Read commands (`get`, `paths`, `keys`, ...) accept them as a no-op (they do not write).

The reason for global-not-per-subcommand is that `dq` already has `-F`, `-v`, `-q`, `--no-color` on the global surface, and adding two more keeps the surface consistent. Users do not have to remember which subcommand accepts which flag.

**Alternatives:**
- Per-subcommand `--sort-keys`: violates the M3 pattern (`-i`, `--diff`, `--check` are global). Rejected.
- `--sort-keys` only on `fmt` and `convert`: confusing, since `dq set --sort-keys` is reasonable when re-emitting via the patch path (although M4 path-A is textual-edit-only — see D5). Rejected.

**Trade-offs:** read commands silently accept the flag and ignore it. Documented in `--help`.

### D5. `--sort-keys` is a no-op for textual-edit `set`/`del`/`patch`/`merge` (only `fmt`/`convert -i` re-emit)

**Decision:** when `set`/`del`/`patch`/`merge` are run with `--sort-keys`, the flag is accepted but does NOT cause the file to be re-canonicalized. The textual-edit pipeline splices into existing bytes; reordering would defeat the M2 round-trip contract for comments and existing key order.

The user-visible behaviour:
- `dq set f.yaml /a 1 --sort-keys` → only `/a` is written, existing keys keep their original order, exit 0.
- `dq fmt f.yaml --sort-keys -i` → entire file is re-canonicalized, keys sorted.
- `dq convert f.yaml -i -F json --sort-keys` → output JSON has sorted keys.

This is documented at the top of `dq fmt --help` and in the README. Failing the user's expectation of "sort everything" via `set` is rare in practice (most users who want sort-keys are running `fmt` anyway), and the alternative — implicitly running fmt after set — would surprise users who set a value in a Helm template and would now have their file silently re-written.

**Alternatives:**
- `--sort-keys` rejects the textual-edit subcommands with `InvalidInput`: too aggressive — users running `dq set` with `--sort-keys` in a global config or env var should not break the build.
- Implicitly run fmt after set: silent file rewrites violate the principle of least surprise. Rejected.

### D6. `--indent N` is honored by JSON, ignored by TOML/JSONL, partial for YAML

**Decision:**
- **JSON**: full support. `serde_json::ser::PrettyFormatter::with_indent(b" " * N)`.
- **JSONL**: per-line full support (each line is its own JSON object).
- **YAML**: `serde_yml` does not expose an indent setting in its public API. M4 documents that YAML `--indent` is a no-op and falls back to the writer's default (2 spaces). When the saphyr-emitter rewrite lands (M5+), this flag becomes meaningful.
- **TOML**: TOML's grammar fixes indent (zero-indent for top-level keys, one-tab for nested arrays-of-tables in pretty mode). `--indent` is a no-op. `toml_edit::DocumentMut::set_implicit` controls table headers; that is a different concept and not covered by `--indent`.

This asymmetry is documented in `--help` and the M4 spec scenario list, so users do not file bug reports.

**Alternatives:**
- Reject `--indent` for TOML/JSONL/YAML with `InvalidInput`: too aggressive in a global flag.
- Apply `--indent` to YAML by post-processing the rendered string: fragile (need to know YAML's existing indent and rewrite it), out of scope.

### D7. `--check` for `validate` is a tolerated no-op alias

**Decision:** `dq validate --check $FILES` is accepted (does not error). The semantics are "parse all files; exit 0 if all parse, exit 4 if any does not." This matches `dq validate` without `--check` exactly. The point of accepting the flag is symmetry with `dq fmt --check $FILES` for pre-commit hook authors who want a uniform `--check` flag across their hook chain.

**Alternatives:**
- Reject `--check` for `validate` with `InvalidInput`: forces hook authors to special-case validate. Friction without benefit.
- Repurpose `--check` for validate to mean "validate against schema" (M11): conflates orthogonal concepts. Rejected.

### D8. `.pre-commit-hooks.yaml` ships in repo root

**Decision:** the repo root contains a `.pre-commit-hooks.yaml` with two entries:

```yaml
- id: dq-fmt-check
  name: dq fmt --check
  entry: dq fmt --check
  language: system
  files: '\.(yaml|yml|json|toml|jsonl)$'
- id: dq-validate
  name: dq validate
  entry: dq validate
  language: system
  files: '\.(yaml|yml|json|toml|jsonl)$'
```

This lets users add the standard `pre-commit` integration:

```yaml
# .pre-commit-config.yaml
repos:
  - repo: https://github.com/mazuninky/dq
    rev: vYYYY.WW.PATCH
    hooks:
      - id: dq-fmt-check
      - id: dq-validate
```

`language: system` means `pre-commit` does not try to install `dq` itself; users are expected to have `dq` on PATH (per the M6 distribution story). M6 will swap `language: system` to `language: rust` once the `cargo install dq` story is in place.

**Alternatives:**
- Include `language: rust` in M4: the `pre-commit` rust-language adapter assumes the binary is published and `cargo install`-able, which is M6's job. Not yet.

### D9. `WriteOptions` is `non_exhaustive`

**Decision:** mark the struct `#[non_exhaustive]` so callers must use `..Default::default()` when constructing. This lets M5+ add `quote_style: QuoteStyle`, `flow_style: FlowStyle`, `strip_comments: bool` without breaking the wire.

**Trade-offs:** slightly more typing for callers (`WriteOptions { sort_keys: true, ..Default::default() }`). Acceptable.

### D10. Plan delta — what `dq-plan.md` updates after archive

After archive M4:
- `dq-plan.md:385-397` (M4 section) gets `✅ Implemented YYYY-MM-DD` marker AND a paragraph naming the deferred flags (`--quote-style`, `--flow-style`, `--strip-comments`) and the technical reason.
- README status line: `M4 alpha`.
- The deferred-flag note is also present on `dq fmt --help` text so users hit it before reading docs.

## Risks / Trade-offs

- **R1 (low): Comments dropped on `fmt`.** Documented as user contract — `fmt` is the explicit canonicalizer. Users who want comments preserved keep using `set`/`del` (textual-edit) until M5+.
- **R2 (low): YAML `--indent` is a no-op until M5+.** Surface in `--help` and on stderr if a non-default `--indent` is passed and the source is YAML — but only at `tracing::warn!` level, not as an error.
- **R3 (low): `canonicalize_keys` deep-clones the value tree.** Allocation is bounded by file size; for human-scale (<1MB) files imperceptible. Re-evaluate if a real-world report shows regression.
- **R4 (medium): User confusion about `--sort-keys` not affecting `set` in-place.** Mitigation: prominent note in `set --help`, `fmt --help`, and the M4 README. The follow-up "run `dq fmt` after a series of `set`s" is the documented workflow.

## Migration Plan

M4 is additive. Three observable behaviour changes:

1. **`dq fmt <FILE>` is now a valid subcommand** — was `unknown subcommand` (exit 6) in M1–M3.
2. **`--sort-keys` is now a valid global flag** — was `unrecognized argument` (exit 6) in M1–M3. Documented as anti-scope-lifted in M3 archive scenario `--sort-keys is unknown in M3`.
3. **`--indent <N>` is now a valid global flag** — same as above.

No breaking changes. Existing M3 invocations produce byte-identical output (`WriteOptions::default()` matches M3 writer behaviour exactly).

Test strategy:
- Existing M3 golden suite re-runs unchanged (no `--sort-keys`/`--indent` in fixtures).
- New `tests/fixtures/fmt/` directory holds canonical and non-canonical fixture pairs.
- `bulk::run_per_file` is reused — no new bulk integration tests beyond `fmt`-specific golden assertions.

## Open Questions

- **Q1.** Should `--sort-keys` accept a stable-but-non-alphabetic ordering (e.g., RFC 8785 JCS / "JSON Canonicalization Scheme")? Decision: NO for M4. Alphabetic sort is the canonical interpretation in YAML/JSON tooling. JCS support can be added later as `--canonical=jcs` if the demand appears.
- **Q2.** Should `dq fmt` accept `--read-only` (a "would-it-format" probe distinct from `--check`)? Decision: NO. `--diff` already shows what would change without writing; adding `--read-only` is redundant.
- **Q3.** Should `convert -F` reject conversion to a format whose writer cannot honor a passed `--indent` (e.g., `convert deploy.yaml -F toml --indent 4` since TOML ignores indent)? Decision: NO — silent ignore matches the M4 contract that `--indent` is "applied where meaningful, ignored where not." Re-evaluate if user reports surprise.
