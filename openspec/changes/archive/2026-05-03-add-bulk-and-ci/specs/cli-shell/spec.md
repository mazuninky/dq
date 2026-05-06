## ADDED Requirements

### Requirement: Bulk-mode global flags

The top-level `Cli` struct SHALL declare these flags as `global = true`:

- `--check` — third output mode alongside `-i` and `--diff`. Exits 0 if every matched file is byte-identical to its prospective output, exits 1 if any file would be modified. Mutually exclusive with `-i`, `--diff`, and `--backup`.
- `--continue-on-error` — for bulk runs, do not abort on the first failure; collect per-file results into the summary, return exit 7 if any failed.
- `--parallel <N>` — run up to N file operations concurrently via `rayon`. `--parallel 0` uses `current_num_threads()`. Default is `1` (sequential).

`Cli::ensure_write_flags_consistent` SHALL learn the new mutual-exclusion rules and the `--check` validation.

#### Scenario: --check conflicts with -i
- **WHEN** the user runs `dq set f.yaml /x 1 -i --check`
- **THEN** clap or the validator returns `INVALID_INPUT` (exit 6) with a message naming the conflicting flags

#### Scenario: --parallel 0 fans out to CPU count
- **WHEN** the user runs `dq set 'k8s/*.yaml' /x 1 -i --parallel 0` on a 8-core machine
- **THEN** the underlying rayon thread pool uses 8 threads (or whatever `current_num_threads()` returns)

#### Scenario: --continue-on-error without bulk is a no-op
- **WHEN** the user runs `dq set deploy.yaml /x 1 -i --continue-on-error` (single file)
- **THEN** the command behaves identically to the same invocation without the flag (no error, no warning)

### Requirement: New subcommands registered in `Command` enum

`Command` SHALL gain three variants: `Patch(PatchArgs)`, `Merge(MergeArgs)`, `Diff(DiffArgs)`. Each variant SHALL be dispatched in `dispatch()` to a `commands::*::run` handler. The hidden-by-default behaviour from M2 (`#[command(hide = true)]` for unimplemented commands) SHALL be removed for these three.

#### Scenario: New subcommands appear in --help
- **WHEN** the user runs `dq --help`
- **THEN** the `patch`, `merge`, and `diff` subcommands are listed without the `(hidden)` marker

#### Scenario: Each new subcommand has its own --help
- **WHEN** the user runs `dq patch --help`
- **THEN** clap prints help text for `patch` including its args (`<FILE>`, `<OPS>`, `--ops-from`, `--line-format`, `--no-create`) and the relevant globals

## MODIFIED Requirements

### Requirement: Anti-scope for M1 binary

In M3 the binary SHALL include `patch`, `merge`, `diff`, `convert -i`, and bulk-mode glob expansion in addition to the M2 baseline. It SHALL NOT include any of the following commands: `fmt`, `query`, `lint`, `check` (the linter check, distinct from the bulk `--check` flag), `test`, `fix`, `explain`, `rules`, `self`, `init`, `config`. They are reserved for M4, M7, M8, M10, and beyond. The corresponding `Command` enum variants either MUST be omitted entirely or MUST be marked `hide = true` and emit "unavailable in this build" errors with exit code 1.

#### Scenario: Reserved subcommand still unreachable
- **WHEN** the user runs `dq fmt config.yaml`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6)

#### Scenario: M3 subcommand `patch` is reachable
- **WHEN** the user runs `dq patch --help`
- **THEN** clap prints the help for `patch` and exits 0

### Requirement: Write-flag activation

`crates/dq-cli/src/cli/args.rs` SHALL declare `-i/--in-place`, `--diff`, `--backup`, `--check`, `--continue-on-error`, and `--parallel <N>` as global flags. Each command handler SHALL inspect these flags itself and treat them as either operative (write subcommands: `set`, `del`, `patch`, `merge`, `convert -i`) or rejected with `INVALID_INPUT` (exit 6).

Read-only subcommands (`get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `validate`, and `diff` — `diff` reads two files but does not write) SHALL reject every write flag (`-i`, `--diff` write-mode meaning, `--backup`, `--check`, `--continue-on-error`, `--parallel`). The existing `Cli::ensure_no_write_flags` SHALL be extended to cover the new flags.

`convert` is special: it accepts `-i` (when paired with `-F`) but rejects the other write modes.

The following flag combinations SHALL be rejected as `INVALID_INPUT` (exit 6):

- `-i` together with `--diff` (mutually exclusive output modes)
- `-i` together with `--check` (mutually exclusive output modes)
- `--diff` together with `--check` (mutually exclusive output modes)
- `--backup` without `-i` (no in-place rename to back up)
- `--backup` together with `--check` (no write to back up)
- `-i` together with `-F <format>` for `set`/`del` (deferred — only `convert` uses `-i + -F`)
- `--parallel <N>` with `N > 1` on a non-glob single-file invocation (the parallelism has no work to do; reject explicitly so users notice the typo)

#### Scenario: Read command rejects --check
- **WHEN** the user runs `dq get config.yaml /x --check`
- **THEN** the command exits with code 6 and a structured error stating "`--check` not supported for `get`"

#### Scenario: --diff and --check are mutually exclusive
- **WHEN** the user runs `dq set f.yaml /x 1 --diff --check`
- **THEN** the command exits with code 6 and the error names both flags

### Requirement: Write-side error mapping to `WRITE_FAILED`

The `exit_code_for_error` function in `crates/dq-cli/src/exit_code.rs` SHALL distinguish read-side `Error::Io` (file open, read) from write-side `Error::Io` (temp file create, content write, rename, backup copy). Write-side IO errors SHALL map to `WRITE_FAILED = 7`. Read-side IO errors SHALL continue to map to `IO_ERROR = 5`. **Bulk partial-failure** (one or more files in a `--continue-on-error` run failed) SHALL also map to `WRITE_FAILED = 7` regardless of the underlying per-file failure cause — this aggregates "some writes did not complete successfully" under a single CI-checkable code.

#### Scenario: Bulk partial failure maps to 7
- **WHEN** a `--continue-on-error` bulk run completes with at least one file failed (any cause: parse, path, IO)
- **THEN** the process exits with code 7

#### Scenario: New `PatchTestFailed` error maps to GENERIC
- **WHEN** `dq patch` raises `Error::PatchTestFailed` because a `test` op did not match
- **THEN** the process exits with code 1 (`GENERIC`) and stderr names the failing pointer, expected, and actual values
