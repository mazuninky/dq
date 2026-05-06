# cli-shell Specification (delta)

## ADDED Requirements

### Requirement: New subcommand `Query` registered in `Command` enum

`Command` SHALL gain a new variant `Query(QueryArgs)`. The variant SHALL be dispatched in `dispatch()` to `commands::query::run`. `Query` is a read-mode subcommand: it accepts every read-tolerant global flag (including `--sort-keys` / `--indent` as inert no-ops) and rejects every write flag (`-i`, `--diff`, `--backup`, `--check`, `--continue-on-error`, `--parallel`) via `Cli::ensure_no_write_flags`.

`QueryArgs` SHALL declare:

- A positional `expression: String` argument — the jq filter source.
- A positional `file: Utf8PathBuf` argument — the file to query (or `-` for stdin, requiring `-F`).

The argument order mirrors `jq`'s muscle-memory shape (`jq EXPR FILE`).

#### Scenario: `query` appears in --help
- **WHEN** the user runs `dq --help`
- **THEN** `query` is listed alongside `get`/`select`/`paths` (without the `(hidden)` marker)

#### Scenario: `query --help` lists positional args
- **WHEN** the user runs `dq query --help`
- **THEN** clap prints help text for `query` showing the `<EXPRESSION>` and `<FILE>` positional arguments and the relevant globals

### Requirement: `--jq` flag on `set` subcommand

`SetArgs` SHALL gain an optional `jq: Option<String>` field, exposed as `--jq <EXPR>` on the `set` subcommand. The flag SHALL be declared with clap-level `conflicts_with = "value"` and `conflicts_with = "value_from"` so the most common misuse (passing both a transform and a literal value source) is caught at parse time with `INVALID_INPUT` (exit 6).

The handler SHALL re-validate at runtime that:

- When `--jq` is set, the positional `pointer` argument is either absent or exactly `/` (the document root).
- When `--jq` is set, the positional `value` argument is absent (clap should already have rejected, but a runtime defence guards against future arg-shape changes).

Both validations produce `InvalidInput` errors mapped to exit code 6.

#### Scenario: `--jq` is parsed as an Option<String>
- **WHEN** the user runs `dq set f.yaml --jq '.foo |= 1' -i`
- **THEN** clap parses successfully and `args.jq` is `Some(".foo |= 1".to_string())`

#### Scenario: `--jq` conflicts with positional VALUE at parse time
- **WHEN** the user runs `dq set f.yaml /x 5 --jq '.foo'`
- **THEN** clap exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and the value argument

#### Scenario: `--jq` conflicts with `--value-from` at parse time
- **WHEN** the user runs `dq set f.yaml /x --value-from new.json --jq '.foo'`
- **THEN** clap exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and `--value-from`

## MODIFIED Requirements

### Requirement: Anti-scope for M1 binary

In M7 the binary SHALL include the M6 baseline (`get`/`exists`/`keys`/`values`/`len`/`type`/`paths`/`select`/`convert`/`fmt`/`validate`/`diff`/`set`/`del`/`patch`/`merge`/`completions`/`man`/`self check`/`self update`) **plus** the new `query` read subcommand and the `--jq EXPR` flag on `set`. It SHALL NOT include any of the following commands: `lint`, `check` (the linter check, distinct from the bulk `--check` flag), `test`, `fix`, `explain`, `rules`, `init`, `config`. They are reserved for M8, M10, and beyond. The corresponding `Command` enum variants either MUST be omitted entirely or MUST be marked `hide = true` and emit "unavailable in this build" errors with exit code 1.

The flags `--quote-style <double|single|auto>`, `--flow-style <block|flow|auto>`, and `--strip-comments` SHALL also be reserved for a future milestone (their implementation requires a comment-preserving emitter) — clap SHALL produce "unrecognized argument" errors (exit 6) when they are used.

#### Scenario: Reserved subcommand still unreachable
- **WHEN** the user runs `dq lint config.yaml`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6)

#### Scenario: M7 subcommand `query` is reachable
- **WHEN** the user runs `dq query --help`
- **THEN** clap prints the help for `query` and exits 0

#### Scenario: M7 flag `--jq` is reachable on `set`
- **WHEN** the user runs `dq set --help`
- **THEN** clap prints the help for `set` including the `--jq <EXPR>` flag

#### Scenario: Deferred --quote-style is unknown in M7
- **WHEN** the user runs `dq fmt config.yaml --quote-style double`
- **THEN** clap exits with code 6 and "unrecognized argument" error
