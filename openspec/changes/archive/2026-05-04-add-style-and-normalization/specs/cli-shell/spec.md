# cli-shell Specification (M4 delta — add style and normalization)

## ADDED Requirements

### Requirement: `--sort-keys` and `--indent <N>` global flags

The top-level `Cli` struct SHALL declare two new global flags:

- `--sort-keys` — boolean. When set, every write path that re-emits through `Format::write_with_options` SHALL produce output with map keys sorted alphabetically at every depth. The textual-edit splice path used by `set`/`del`/`patch`/`merge` accepts the flag without error and SHALL NOT reorder existing keys (intentional — preserving the M2 round-trip contract for comments and key order).
- `--indent <N>` — `u8`. When set, formats that honor indent (`json`, `jsonl`) emit `N`-space indentation; YAML and TOML accept the flag and currently ignore it (deferred for YAML to a future milestone with a comment-preserving emitter; fixed by grammar for TOML).

Read commands accept both flags as no-ops. `Cli::ensure_no_write_flags` SHALL accept the two new flags (they are write-mode-adjacent but read-tolerant — being set on a read command is silently ignored, since the user may have them globally enabled in shell aliases).

#### Scenario: --sort-keys is accepted globally
- **WHEN** the user runs `dq fmt config.yaml --sort-keys`
- **THEN** clap parses the flag and the handler runs (no rejection)

#### Scenario: --indent 4 is accepted globally
- **WHEN** the user runs `dq convert config.yaml -F json --indent 4`
- **THEN** clap parses the flag and the JSON writer emits 4-space indented output

#### Scenario: --sort-keys is read-tolerant
- **WHEN** the user runs `dq get config.yaml /a --sort-keys`
- **THEN** the command behaves identically to the same invocation without the flag (silent no-op, exit 0)

### Requirement: New subcommand `Fmt` registered in `Command` enum

`Command` SHALL gain a new variant `Fmt(FmtArgs)`. The variant SHALL be dispatched in `dispatch()` to `commands::fmt::run`. `fmt` is a write-mode subcommand: it accepts every M3 write flag (`-i`, `--diff`, `--check`, `--backup`, `--continue-on-error`, `--parallel`) plus the new globals (`--sort-keys`, `--indent`) and rejects nothing from the M3 set.

#### Scenario: fmt subcommand appears in --help
- **WHEN** the user runs `dq --help`
- **THEN** `fmt` is listed alongside `set`/`del`/`patch`/`merge`/`diff`/`convert`

#### Scenario: fmt --help shows write-mode flags
- **WHEN** the user runs `dq fmt --help`
- **THEN** clap prints help text for `fmt` and the global flags `-i`, `--diff`, `--check`, `--backup`, `--sort-keys`, `--indent`

## MODIFIED Requirements

### Requirement: Anti-scope for M1 binary

In M4 the binary SHALL include `fmt` in addition to the M3 baseline (`set`, `del`, `patch`, `merge`, `diff`, `convert -i` plus `--check`/`--continue-on-error`/`--parallel`). It SHALL NOT include any of the following commands: `query`, `lint`, `check` (the linter check, distinct from the bulk `--check` flag), `test`, `fix`, `explain`, `rules`, `self`, `init`, `config`. They are reserved for M7, M8, M10, and beyond. The corresponding `Command` enum variants either MUST be omitted entirely or MUST be marked `hide = true` and emit "unavailable in this build" errors with exit code 1.

The flags `--quote-style <double|single|auto>`, `--flow-style <block|flow|auto>`, and `--strip-comments` SHALL also be reserved for a future milestone (their implementation requires a comment-preserving emitter) — clap SHALL produce "unrecognized argument" errors (exit 6) when they are used.

#### Scenario: Reserved subcommand still unreachable
- **WHEN** the user runs `dq lint config.yaml`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6)

#### Scenario: M4 subcommand `fmt` is reachable
- **WHEN** the user runs `dq fmt --help`
- **THEN** clap prints the help for `fmt` and exits 0

#### Scenario: Deferred --quote-style is unknown in M4
- **WHEN** the user runs `dq fmt config.yaml --quote-style double`
- **THEN** clap exits with code 6 and "unrecognized argument" error
