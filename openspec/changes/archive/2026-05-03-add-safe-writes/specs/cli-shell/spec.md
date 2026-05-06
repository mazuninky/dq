## ADDED Requirements

### Requirement: Write-flag activation

`crates/dq-cli/src/cli/args.rs` SHALL declare `-i/--in-place`, `--diff`, and `--backup` as global flags. The M1 helper `Cli::reject_write_flags()` SHALL be removed. Each command handler SHALL inspect these flags itself and treat them as either operative (`set`, `del`) or rejected with `INVALID_INPUT` (read-only commands: `get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `convert`, `validate` — none of these accept write flags in M2). Combinations forbidden by D9/D10 of the design (`-i` + `--diff`, `-i` + `-F`, `--backup` without `-i`) SHALL be rejected by clap with exit code 6 (`INVALID_INPUT`) at parse time.

#### Scenario: Read command rejects -i
- **WHEN** the user runs `dq get config.yaml /x -i`
- **THEN** the command exits with code 6 and a structured error stating "`-i` not supported for `get`"

#### Scenario: Write command accepts -i
- **WHEN** the user runs `dq set config.yaml /x 1 -i`
- **THEN** the command does NOT exit with code 6; it proceeds with the in-place write

### Requirement: Write-side error mapping to `WRITE_FAILED`

The `exit_code_for_error` function in `crates/dq-cli/src/exit_code.rs` SHALL distinguish read-side `Error::Io` (file open, read) from write-side `Error::Io` (temp file create, content write, rename, backup copy). Write-side IO errors SHALL map to `WRITE_FAILED = 7`. Read-side IO errors SHALL continue to map to `IO_ERROR = 5`. The distinction MAY be implemented via a `WriteIoError` newtype wrapped in `anyhow::Error` at the call site of `atomic_write::write`, or via a `during_write: bool` field on `Error::Io` — implementation detail of `dq-cli`.

#### Scenario: Write-side EACCES maps to 7
- **WHEN** `dq set /etc/locked.yaml /x 1 -i` fails because the parent directory denies write
- **THEN** the process exits with code 7

#### Scenario: Read-side ENOENT maps to 5
- **WHEN** `dq set missing.yaml /x 1 -i` fails because the source file does not exist
- **THEN** the process exits with code 5

#### Scenario: SUCCESS / GENERIC / NOT_FOUND / PARSE_ERROR / VALIDATE_FAIL / IO_ERROR / INVALID_INPUT unchanged
- **WHEN** any of the M1-defined exit-code paths fires after the M2 changes
- **THEN** the exit code matches the M1 contract (0/1/2/3/4/5/6 respectively)

## MODIFIED Requirements

### Requirement: Exit codes as named constants

`crates/dq-cli/src/exit_code.rs` SHALL define `pub const SUCCESS: i32 = 0`, `GENERIC: i32 = 1`, `NOT_FOUND: i32 = 2`, `PARSE_ERROR: i32 = 3`, `VALIDATE_FAIL: i32 = 4`, `IO_ERROR: i32 = 5`, `INVALID_INPUT: i32 = 6`, `WRITE_FAILED: i32 = 7`. The `exit_code_for_error(err: &anyhow::Error) -> i32` function SHALL `downcast_ref` to the domain `Error` enum and return the matching constant; unrecognised errors fall back to `GENERIC`. Adding new exit codes is allowed; reassigning existing numeric values is FORBIDDEN as it would break agent CI scripts.

#### Scenario: Path-not-found maps to NOT_FOUND
- **WHEN** a command produces an `Error::Path { .. }` and `main.rs` invokes `exit_code_for_error`
- **THEN** the returned exit code is 2

#### Scenario: Generic anyhow error
- **WHEN** a handler returns `anyhow::bail!("disk full")` (no domain Error)
- **THEN** the exit code is 1

#### Scenario: Write failure maps to WRITE_FAILED
- **WHEN** an atomic-write helper returns `Err(Error::Io { ... })` from a `set`/`del` command
- **THEN** `exit_code_for_error` returns 7

### Requirement: Anti-scope for M1 binary

In M2 the binary SHALL include `set` and `del` subcommands in addition to the M1 read commands. It SHALL NOT include any of the following commands: `patch`, `merge`, `diff`, `fmt`, `query`, `lint`, `check`, `test`, `fix`, `explain`, `rules`, `self`, `init`, `config`. They are reserved for M3, M4, M7, M8, M10, and beyond. The corresponding `Command` enum variants either MUST be omitted entirely or MUST be marked `hide = true` and emit "unavailable in this build" errors with exit code 1.

#### Scenario: Reserved subcommand still unreachable
- **WHEN** the user runs `dq patch config.yaml @ops.json`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6) OR a hidden subcommand emits the structured "arrives in M3" error and exits 1

#### Scenario: M2 subcommand `set` is reachable
- **WHEN** the user runs `dq set --help`
- **THEN** clap prints the help for `set` and exits 0
