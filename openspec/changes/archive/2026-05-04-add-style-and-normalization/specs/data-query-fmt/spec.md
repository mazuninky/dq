# data-query-fmt Specification

## ADDED Requirements

### Requirement: `dq fmt` command — re-emit through native writer

The CLI SHALL provide a `fmt` subcommand `dq fmt <FILE>` that parses `<FILE>`, then re-emits the parsed document through the format's native writer to produce a canonicalized rendering. The source format SHALL be preserved (YAML→YAML, JSON→JSON, TOML→TOML, JSONL→JSONL); cross-format conversion remains the responsibility of `convert -i -F`. Comments, anchor names, and quote-style hints from the source are dropped on output (intentional — `fmt` is the explicit canonicalizer; comment preservation is the contract of `set`/`del`/`patch`/`merge`).

#### Scenario: Default fmt to stdout
- **WHEN** the user runs `dq fmt deploy.yaml` (no `-i`, no `--diff`)
- **THEN** the rendered YAML is written to stdout, the file on disk is unchanged, and exit code is 0

#### Scenario: fmt -i writes back atomically
- **WHEN** the user runs `dq fmt deploy.yaml -i`
- **THEN** the file on disk is replaced with the canonicalized rendering via `tempfile` + `persist`, stdout is empty, and exit code is 0

#### Scenario: fmt --diff shows unified diff
- **WHEN** the user runs `dq fmt deploy.yaml --diff`
- **THEN** stdout contains a unified diff between the source bytes and the re-emitted bytes, the file is unchanged, and exit code is 0

#### Scenario: fmt preserves source format
- **WHEN** the user runs `dq fmt config.toml`
- **THEN** stdout is TOML (the renderer is `toml_edit`, not the JSON writer)

### Requirement: `dq fmt --check` idempotency gate

`dq fmt --check <FILE>` SHALL exit 0 when the source bytes are byte-identical to the re-emitted bytes (file is canonical), and exit 1 when they differ (file would be modified). The `--check` mode SHALL NOT write to disk and SHALL NOT print the rendered output. In bulk mode, the per-file decisions are aggregated: exit 0 if all are canonical, exit 1 if any one would change. The list of would-change files SHALL be printed to stdout one path per line so pre-commit hooks can use the output.

#### Scenario: fmt --check passes on canonical file
- **WHEN** the user runs `dq fmt deploy.yaml --check` on a file that is already canonically formatted
- **THEN** stdout is empty, the file is unchanged, and exit code is 0

#### Scenario: fmt --check fails on non-canonical file
- **WHEN** the user runs `dq fmt deploy.yaml --check` on a file whose re-emitted bytes differ from the source
- **THEN** stdout contains the file path, the file is unchanged, and exit code is 1

#### Scenario: fmt --check on bulk glob
- **WHEN** the user runs `dq fmt 'k8s/**/*.yaml' --check` on a tree of 5 files where 2 are non-canonical
- **THEN** stdout names the 2 non-canonical files (one per line), the files are unchanged, and exit code is 1

### Requirement: `dq fmt` is glob-aware via the M3 bulk driver

`dq fmt` SHALL accept a glob pattern as `<FILE>` and run through `crates/dq-cli/src/bulk.rs::run_per_file` for expansion, parallelism, summary, and error aggregation. The contract from `data-query-bulk` (M3 capability) is inherited unchanged: `--continue-on-error`, `--parallel <N>`, summary line `Modified: N, Skipped: M, Failed: K`, partial-failure exit 7.

#### Scenario: fmt on a glob with mix of canonical and non-canonical files
- **WHEN** the user runs `dq fmt 'k8s/**/*.yaml' -i` on 5 YAML files where 3 are non-canonical
- **THEN** the 3 non-canonical files are rewritten on disk, the 2 canonical files are unchanged, stdout shows `Modified: 3, Skipped: 2, Failed: 0`, and exit code is 0

#### Scenario: fmt --parallel 4 smoke
- **WHEN** the user runs `dq fmt 'k8s/**/*.yaml' -i --parallel 4` on 10 files
- **THEN** all files are rewritten correctly via the rayon thread pool and exit code is 0

### Requirement: `--sort-keys` global flag

`crates/dq-cli/src/cli/args.rs` SHALL declare `--sort-keys` as a global boolean flag. When set, every write path that re-emits through `Format::write_with_options` SHALL receive `WriteOptions { sort_keys: true, .. }` and produce output with map keys sorted alphabetically at every depth. This affects `dq fmt`, `dq convert -i`, and any future write commands that go through `write_with_options`. The textual-edit splice path used by `set`/`del`/`patch`/`merge` SHALL accept the flag without error and SHALL NOT reorder existing keys (textual-edit preserves byte order; reordering would defeat the M2 round-trip contract). Read commands accept the flag as a no-op.

#### Scenario: fmt with --sort-keys reorders nested maps
- **WHEN** the user runs `dq fmt config.yaml --sort-keys -i` on a file with `{ z: 1, a: { y: 2, b: 3 } }`
- **THEN** the file on disk reads `{ a: { b: 3, y: 2 }, z: 1 }` (deep alphabetical order, IndexMap-stable beneath)

#### Scenario: set with --sort-keys does NOT reorder existing keys
- **WHEN** the user runs `dq set config.yaml /spec/replicas 5 --sort-keys -i` on a file whose existing key order is non-alphabetic
- **THEN** only the targeted pointer is updated; sibling key order is preserved byte-for-byte; exit code is 0

#### Scenario: read command with --sort-keys is a no-op
- **WHEN** the user runs `dq get config.yaml /a --sort-keys`
- **THEN** the command behaves identically to `dq get config.yaml /a` (reads do not emit; the flag is silently ignored) and exit code is 0

### Requirement: `--indent N` global flag

`crates/dq-cli/src/cli/args.rs` SHALL declare `--indent <N>` as a global option taking a `u8`. When set, every write path that re-emits through `Format::write_with_options` SHALL receive `WriteOptions { indent: Some(N), .. }`. JSON SHALL honor it via `serde_json::ser::PrettyFormatter::with_indent`. JSONL SHALL honor it per line. YAML SHALL ignore the flag in M4 (the `serde_yml` writer does not expose an indent setting; the saphyr-emitter rewrite, deferred to a future milestone, will lift this). TOML SHALL ignore the flag (grammar-fixed indent). Setting `--indent 0` SHALL produce compact output without indentation. The textual-edit splice path accepts the flag without error (no-op for splice).

#### Scenario: --indent 4 produces 4-space JSON
- **WHEN** the user runs `dq convert deploy.yaml -F json --indent 4`
- **THEN** stdout is JSON with 4-space indentation at every level

#### Scenario: --indent 0 produces compact JSON
- **WHEN** the user runs `dq convert deploy.yaml -F json --indent 0`
- **THEN** stdout is one-line compact JSON without indentation

#### Scenario: --indent on YAML is a no-op (M4 deferred)
- **WHEN** the user runs `dq fmt deploy.yaml --indent 4 -i`
- **THEN** the file on disk has the writer's default YAML indent (2 spaces); a `tracing::warn!` line on stderr names the deferral

### Requirement: `validate --check` accepts the flag as a no-op

`dq validate <FILE> --check` SHALL be accepted (not rejected with `INVALID_INPUT`). The semantics match `dq validate <FILE>` exactly: parse the file, exit 0 on success, exit 4 (`VALIDATE_FAIL`) on parse error. The flag exists for symmetry with `dq fmt --check $FILES` so pre-commit hook authors can use a uniform flag across the chain.

#### Scenario: validate --check on valid file
- **WHEN** the user runs `dq validate deploy.yaml --check`
- **THEN** stdout is empty, the file is unchanged, and exit code is 0

#### Scenario: validate --check on invalid file
- **WHEN** the user runs `dq validate broken.json --check` on a file with a syntax error
- **THEN** stderr contains a structured parse error, the file is unchanged, and exit code is 4

### Requirement: `dq fmt` with `--backup` writes `.bak` only on actual change

When `dq fmt -i --backup` is used and the file is already canonical (re-emitted bytes equal source bytes), `<path>.bak` SHALL NOT be created (no write happens). When the file would be changed, `<path>.bak` SHALL be created via the same atomic_write helper used by `set`/`del`. This matches the M2 backup contract.

#### Scenario: backup not written for canonical file
- **WHEN** the user runs `dq fmt deploy.yaml -i --backup` on a file that is already canonical
- **THEN** `deploy.yaml.bak` does not exist, the file is unchanged, and exit code is 0

#### Scenario: backup written for non-canonical file
- **WHEN** the user runs `dq fmt deploy.yaml -i --backup` on a non-canonical file
- **THEN** `deploy.yaml.bak` contains the original bytes, `deploy.yaml` contains the re-emitted bytes, and exit code is 0

### Requirement: Anti-scope for M4 fmt command

In M4 the `fmt` command SHALL NOT support the following flags or behaviours: `--quote-style <double|single|auto>`, `--flow-style <block|flow|auto>`, `--strip-comments`, schema-aware canonicalization, watch mode (`--watch`), and any per-format flags (`--yaml-flow-arrays`, `--toml-pad-keys`). These are deferred to later milestones (M5+ for the three flags requiring comment-preserving emitters). Attempts to use them SHALL produce clap "unrecognized argument" errors (exit 6).

#### Scenario: --quote-style is unknown in M4
- **WHEN** the user runs `dq fmt config.yaml --quote-style double`
- **THEN** clap exits with code 6 and "unrecognized argument" error

#### Scenario: --flow-style is unknown in M4
- **WHEN** the user runs `dq fmt config.yaml --flow-style block`
- **THEN** clap exits with code 6 and "unrecognized argument" error

#### Scenario: --strip-comments is unknown in M4
- **WHEN** the user runs `dq fmt config.yaml --strip-comments`
- **THEN** clap exits with code 6 and "unrecognized argument" error
