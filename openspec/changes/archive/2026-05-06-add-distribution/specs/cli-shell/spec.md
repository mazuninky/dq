# cli-shell Specification (delta)

## ADDED Requirements

### Requirement: User-facing `dq completions <shell>` subcommand

`dq-cli` SHALL expose a top-level `completions` subcommand that writes a shell completion script to stdout. The required positional argument is the shell name parsed via `clap_complete::Shell` (`bash`, `zsh`, `fish`, `powershell`, `elvish`). The handler invokes `clap_complete::generate(shell, &mut Cli::command(), "dq", out)` and returns successfully on every supported shell.

This subcommand is distinct from the existing hidden `dq generate-docs --output-dir DIR` (which writes a directory tree of files for packaging scripts). `dq completions` is the documented end-user entry point: `dq completions zsh > ~/.zsh/completions/_dq`.

#### Scenario: Bash completion script printed to stdout
- **WHEN** the user runs `dq completions bash`
- **THEN** stdout contains a `complete -F` declaration referencing `_dq` and exits 0

#### Scenario: Unsupported shell rejected by clap
- **WHEN** the user runs `dq completions tcsh`
- **THEN** clap exits with code 6 (`INVALID_INPUT`) and an "invalid value" error

### Requirement: User-facing `dq man [PAGE]` subcommand

`dq-cli` SHALL expose a top-level `man` subcommand that writes a troff-formatted man page to stdout. With no positional argument, the top-level `dq.1` page is rendered via `clap_mangen::Man::new(Cli::command()).render(out)`. With a positional argument matching a registered subcommand name (`get`, `set`, etc.), the corresponding `dq-<name>.1` page is rendered. An unknown name returns an `InvalidInput` error (exit 6) whose message names the missing subcommand.

#### Scenario: Top-level man page printed to stdout
- **WHEN** the user runs `dq man`
- **THEN** stdout contains a `.TH "dq"` header and exits 0

#### Scenario: Per-subcommand man page printed to stdout
- **WHEN** the user runs `dq man get`
- **THEN** stdout contains a `.TH "dq-get"` header and exits 0

#### Scenario: Unknown subcommand surfaces InvalidInput
- **WHEN** the user runs `dq man bogus`
- **THEN** the handler exits 6 with a message naming `bogus`

### Requirement: User-facing `dq self check` and `dq self update` subcommands

`dq-cli` SHALL expose a top-level `self` subcommand with two children, `check` and `update`. The `check` child queries `https://api.github.com/repos/mazuninky/dq/releases/latest` via `ureq` and prints one of three messages comparing `env!("CARGO_PKG_VERSION")` to the remote `tag_name`: "up to date", "newer version available", or "running pre-release version". Exit 0 on every comparison outcome; exit 5 (`IO_ERROR`) on a network failure. When GitHub returns 403 with `X-RateLimit-Remaining: 0`, the handler MUST print a hint suggesting `GITHUB_TOKEN` before exiting 5.

The `update` child takes an optional `--to <VER>` flag. It downloads the appropriate prebuilt binary for the running platform/arch from GitHub Releases via the `self_update` crate, verifies the SHA256 against the published `dq-checksums.txt`, and atomically replaces the running binary. Exit 0 on success, 5 on network failure, 7 on atomic-replace failure. The handler MUST refuse to operate when the running binary lives under a system path the current user cannot write (e.g. `/usr/local/bin/dq` for non-root) and MUST emit a `sudo dq self update` hint, exiting 6.

#### Scenario: Self-check up to date
- **WHEN** the user runs `dq self check` with the local version equal to the latest GitHub tag
- **THEN** stdout contains "up to date" and exits 0

#### Scenario: Self-check newer available
- **WHEN** the user runs `dq self check` with the local version older than the latest GitHub tag
- **THEN** stdout contains "newer version available" and the suggestion `dq self update`, and exits 0

#### Scenario: Self-update --to pins to specific version
- **WHEN** the user runs `dq self update --to v0.5.0`
- **THEN** the handler downloads the v0.5.0 artifact, verifies the SHA256, and replaces the binary; exits 0

#### Scenario: Self-update fails on read-only install path
- **WHEN** the user (non-root) runs `dq self update` against a binary in `/usr/local/bin`
- **THEN** the handler refuses to operate, prints a `sudo` hint, and exits 6 (`INVALID_INPUT`)

### Requirement: SARIF output format for diagnostic-shaped values

`dq-cli` SHALL accept `-F sarif` as an output format selector. The `OutputFormat::Sarif` variant maps to a `SarifReporter` that expects the input `serde_json::Value` to be an object containing a `diagnostics` array. Each diagnostic entry is rendered as one SARIF 2.1.0 `result` with `level` mapped (`error → error`, `warn → warning`, `info → note`), `message.text` set from the entry's `message`, and `physicalLocation` populated from `path` / `line` / `col`. Output is a single SARIF document (one `runs` entry naming `dq` as the tool driver) emitted via `serde_json::to_writer_pretty`.

For value shapes that do not match (e.g. a query handler accidentally selects `-F sarif` for a `dq get` invocation), the reporter MUST return an `InvalidInput` error so the exit-code mapper produces 6 (`INVALID_INPUT`).

`as_input_format_name()` SHALL return `None` for `OutputFormat::Sarif` — SARIF is output-only.

#### Scenario: Validate emits SARIF on parse failure
- **WHEN** the user runs `dq validate -F sarif broken.yaml` against a YAML file with a parse error
- **THEN** stdout contains a SARIF document with one `result` whose `physicalLocation.region.startLine` matches the parse error line, and the handler exits 4 (`VALIDATE_FAIL`)

#### Scenario: Query command rejects SARIF
- **WHEN** the user runs `dq get config.yaml /name -F sarif`
- **THEN** the handler returns `InvalidInput` with a message naming SARIF as the unsupported reporter format and exits 6

## MODIFIED Requirements

### Requirement: Anti-scope for M1 binary

In M6 the binary SHALL include `completions`, `man`, and `self` (with `check` and `update` children) in addition to the M5 baseline (`get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `convert`, `fmt`, `validate`, `diff`, `set`, `del`, `patch`, `merge`). It SHALL NOT include any of the following commands: `query`, `lint`, `check` (the linter check, distinct from the bulk `--check` flag), `test`, `fix`, `explain`, `rules`, `init`, `config`. They are reserved for M7, M8, M10, and beyond. The corresponding `Command` enum variants either MUST be omitted entirely or MUST be marked `hide = true` and emit "unavailable in this build" errors with exit code 1.

The flags `--quote-style <double|single|auto>`, `--flow-style <block|flow|auto>`, and `--strip-comments` SHALL also be reserved for a future milestone (their implementation requires a comment-preserving emitter) — clap SHALL produce "unrecognized argument" errors (exit 6) when they are used.

#### Scenario: Reserved subcommand still unreachable
- **WHEN** the user runs `dq lint config.yaml`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6)

#### Scenario: M6 subcommand `self` is reachable
- **WHEN** the user runs `dq self --help`
- **THEN** clap prints the help for `self` (with `check` and `update` children listed) and exits 0

#### Scenario: M6 subcommand `completions` is reachable
- **WHEN** the user runs `dq completions --help`
- **THEN** clap prints the help for `completions` (listing the supported shells) and exits 0

#### Scenario: Deferred --quote-style is unknown in M6
- **WHEN** the user runs `dq fmt config.yaml --quote-style double`
- **THEN** clap exits with code 6 and "unrecognized argument" error
