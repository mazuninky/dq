# cli-shell Specification

## Purpose

Defines the structural and behavioral contract of the `dq-cli` binary shell: how `main.rs` is organized, how diagnostics flow through tracing, how output is rendered through the `Reporter` trait, and how exit codes, color resolution, and error rendering behave. This capability covers the framing of the CLI — not the read/write semantics of individual subcommands.
## Requirements
### Requirement: Thin `main.rs` contract

`crates/dq-cli/src/main.rs` SHALL contain at most: SIGPIPE restoration on Unix, clap parse, tracing init, dependency wiring (Reporter factory, stdout/stderr lock), dispatch into a `commands::*` handler, and exit-code mapping. All command logic SHALL live in modules under `crates/dq-cli/src/commands/`. `main.rs` MUST NOT contain business logic, parsing of file content, or output formatting beyond Reporter selection.

#### Scenario: main.rs size budget
- **WHEN** the M1 implementation is complete
- **THEN** `main.rs` contains fewer than 80 non-blank, non-comment lines and `cargo clippy --all-targets -- -D warnings` passes

### Requirement: SIGPIPE handler on Unix

On Unix targets, `main.rs` SHALL restore the SIGPIPE handler to `SIG_DFL` before any output, so that piping CLI output into `head` or similar tools terminates the process cleanly with the conventional broken-pipe exit instead of panicking with `failed printing to stdout`.

#### Scenario: Output piped to head
- **WHEN** the user runs `dq paths big.yaml | head -n 5` and the document has thousands of paths
- **THEN** the process terminates without panic, stderr is empty, and the only output is the first five lines

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

### Requirement: Global CLI flags

The top-level `Cli` struct SHALL declare these flags as `global = true` so they apply to every subcommand: `-F/--format <fmt>`, `-v/--verbose` (`ArgAction::Count`, `conflicts_with = "quiet"`), `-q/--quiet` (`conflicts_with = "verbose"`), `--no-color`, `--no-pager`, `--doc <idx|all>` (parsing-only in M1; semantics in M2). Subcommand-specific args MUST live on per-subcommand `Args` structs.

#### Scenario: Verbosity is countable
- **WHEN** the user runs `dq -vvv get config.yaml /x`
- **THEN** tracing is initialised at TRACE level

#### Scenario: Quiet conflicts with verbose
- **WHEN** the user runs `dq -v -q get config.yaml /x`
- **THEN** clap rejects the invocation with a structured error and exit code 6 (`INVALID_INPUT`)

### Requirement: Tracing as the only diagnostic channel

`dq-cli` SHALL initialise `tracing_subscriber::fmt().try_init()` (NOT `.init()`) in `main.rs` with an `EnvFilter` honoring `RUST_LOG` and the `-v`/`-q` flags (default WARN, `-v` INFO, `-vv` DEBUG, `-vvv` TRACE, `-q` ERROR). All diagnostic output across every crate SHALL use `tracing::*!` macros. Direct `println!`/`eprintln!` are FORBIDDEN outside of `main.rs` panic paths and the Reporter implementations writing user-facing output.

#### Scenario: Re-init resilience for tests
- **WHEN** integration tests invoke a `run()` helper twice in the same process
- **THEN** the second `try_init()` call returns `Err(_)` silently (NOT panic), and tests proceed normally

### Requirement: Reporter trait with dependency injection

`crates/dq-cli/src/output/mod.rs` SHALL define `trait Reporter` with `fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()>`. Implementations: `ConsoleReporter`, `JsonReporter`, `YamlReporter`, `TomlReporter`, `JsonlReporter`, `ToonReporter` — covering all `OutputFormat` variants. The Reporter factory `reporter_for_format(format, use_color)` SHALL live in `main.rs`. Command handlers SHALL accept `reporter: &dyn Reporter` and `out: &mut dyn Write` as parameters; they MUST NOT construct their own Reporter or call `io::stdout()` directly.

#### Scenario: Handler is testable with Vec<u8>
- **WHEN** a unit test calls `commands::get::run(reporter: &JsonReporter, out: &mut Vec<u8>, ...)`
- **THEN** the test asserts on the bytes captured in `out` without spawning the binary or touching the filesystem

#### Scenario: ConsoleReporter respects use_color
- **WHEN** `ConsoleReporter` is constructed with `use_color: false`
- **THEN** no ANSI escape sequences appear in any output, regardless of TTY or `NO_COLOR` state

### Requirement: Color resolution precedence

The CLI SHALL resolve color usage in this order: `--no-color` flag > `NO_COLOR` env var (presence) > `CLICOLOR_FORCE` env var > `is_stdout_tty()` detection. The resolved boolean SHALL be threaded through the call graph as a parameter; tests MUST NOT mutate `NO_COLOR` via `std::env::set_var`.

#### Scenario: --no-color overrides TTY
- **WHEN** the user runs `dq paths config.yaml --no-color` from an interactive terminal
- **THEN** the output contains no ANSI sequences

### Requirement: Non-interactive contract

`dq-cli` SHALL NOT prompt the user for input, display spinners or progress bars, or alter output based on TTY beyond color/pager decisions. Output SHALL be byte-identical when piped (`| cat`) and when run interactively, except for ANSI color codes.

#### Scenario: Identical output under pipe
- **WHEN** the same `dq paths config.yaml -F json` invocation runs in TTY and piped to `cat`
- **THEN** byte-identical output is produced (no extra control bytes)

### Requirement: Structured errors with line/col/caret

When `dq-core` reports a parse error, the error SHALL carry `line`, `col`, `span` (byte range), source snippet, and (where applicable) `did_you_mean`. Console rendering SHALL include a caret indicator on the offending byte; `-F json` rendering SHALL emit a JSON object with these fields. `--no-color` SHALL disable ANSI in the caret rendering only.

#### Scenario: Parse error console rendering
- **WHEN** the user runs `dq validate broken.json` against `{ "x": 1, }`
- **THEN** stderr contains a multiline message: file path with `:line:col`, the offending source line, a caret beneath the trailing comma, and a one-line hint

#### Scenario: Parse error JSON rendering
- **WHEN** the user runs `dq validate broken.json -F json`
- **THEN** stderr contains a single JSON object with fields `kind="parse"`, `file`, `line`, `col`, `span`, `message`, optionally `hint`

### Requirement: Completions and man pages stubs

`dq-cli` SHALL include `clap_complete` and `clap_mangen` dependencies and expose a hidden `dq generate-docs --output-dir <DIR>` command that emits bash/zsh/fish/powershell completions and man pages. Wiring into `install.sh` and CI is out of scope for M1.

#### Scenario: Generate-docs emits expected files
- **WHEN** the developer runs `dq generate-docs --output-dir /tmp/dq-docs`
- **THEN** the directory contains `completions/{dq.bash,_dq,dq.fish,_dq.ps1}` and `man/dq.1` and the command exits 0

### Requirement: Anti-scope for M1 binary

In M8 the binary SHALL include the M7 baseline plus the new lint commands listed above. The reserved-subcommands list (M1's anti-scope, evolved through M2–M7) SHALL be reduced to the still-deferred set: `fix`, `init`, `config`. They are reserved for M10+. The `--quote-style`, `--flow-style`, `--strip-comments` flags SHALL remain reserved (their implementation requires a comment-preserving emitter).

#### Scenario: Reserved subcommand `fix` still unreachable
- **WHEN** the user runs `dq fix file.yaml`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6)

#### Scenario: M8 subcommand `lint` is reachable
- **WHEN** the user runs `dq lint --help`
- **THEN** clap prints the help for `lint` and exits 0

#### Scenario: M8 subcommand `check` is reachable as linter check
- **WHEN** the user runs `dq check rules/foo.yml file.yaml`
- **THEN** the handler resolves the rule, lints the file, and emits diagnostics

#### Scenario: M8 subcommand `test` is reachable
- **WHEN** the user runs `dq test crates/dq-lint/rules/k8s/`
- **THEN** the test runner discovers fixtures and prints pass/fail per fixture

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

### Requirement: Lint subcommand surface

The `Command` enum SHALL gain five new variants: `Lint(LintArgs)`, `Check(CheckArgs)` (linter check, distinct from the bulk `--check` flag — clap routes the linter `check` as a subcommand and `--check` remains a global flag), `Test(TestArgs)`, `Explain(ExplainArgs)`, and `Rules(RulesArgs)` whose nested `RulesCommand` enum is `List(RulesListArgs)` or `Add(RulesAddArgs)`. Each new args struct lives under `crates/dq-cli/src/cli/args/`. Their handlers live under `crates/dq-cli/src/commands/`. The dispatcher routes each variant to its handler and bubbles the handler's error verbatim for exit-code mapping.

#### Scenario: Lint help is reachable
- **WHEN** the user runs `dq lint --help`
- **THEN** clap prints the help for `lint`, listing `--rules`, `--strict`, the global `-F` choices including `junit` / `tap`, and the positional `<files>...`

#### Scenario: Linter `check` is reachable as a subcommand
- **WHEN** the user runs `dq check --help`
- **THEN** clap prints the help for the linter `check` (single rule against files); the global `--check` flag (write idempotency gate) is unchanged in semantics

#### Scenario: Test subcommand is reachable
- **WHEN** the user runs `dq test --help`
- **THEN** clap prints help for `test` listing the `<rules-dir>` positional and reporter flags

#### Scenario: Explain subcommand is reachable
- **WHEN** the user runs `dq explain k8s.no-latest-tag`
- **THEN** the rule's description, severity, and references render to stdout; exit 0

#### Scenario: Rules subcommand is reachable
- **WHEN** the user runs `dq rules list`
- **THEN** the available rulesets render to stdout (table / JSON / Toon as `-F` selects)

### Requirement: New `--strict` global flag

A new global flag `--strict` SHALL be added to `Cli` with `global = true`. The flag is a boolean (`bool`, default `false`) used by the lint handler to escalate `warn`-severity violations into the exit-4 error family. Read commands silently ignore `--strict`; write commands silently ignore it. `--strict` MUST NOT appear in the read-command rejection list (it is meaningful for `lint` / `check`).

#### Scenario: Strict mode escalates warnings
- **WHEN** the user runs `dq lint --strict file.yaml` and only `warn`-severity violations are produced
- **THEN** the exit code is 1 (`GENERIC`); without `--strict`, the same invocation exits 0

### Requirement: Reporter formats `junit` and `tap`

`OutputFormat` SHALL gain two new variants: `Junit` (JUnit XML) and `Tap` (TAP version 13). Both reporters consume the canonical `{ "diagnostics": [...] }` shape produced by the lint engine. Selecting `-F junit` or `-F tap` for a non-diagnostic-shaped command (e.g. `dq get`) SHALL surface the same `InvalidInput` error path used by SARIF: the reporter validates the input value's shape and rejects unrecognised inputs.

#### Scenario: `-F junit` valid for lint
- **WHEN** the user runs `dq lint -F junit file.yaml`
- **THEN** stdout contains a well-formed JUnit XML document (`<testsuite>` with one or more `<testcase>` rows)

#### Scenario: `-F tap` valid for lint
- **WHEN** the user runs `dq lint -F tap file.yaml`
- **THEN** stdout starts with `TAP version 13` and `1..<N>` (where N matches the diagnostic count)

#### Scenario: `-F junit` invalid for query verbs
- **WHEN** the user runs `dq get config.yaml /foo -F junit`
- **THEN** the command exits 6 (`INVALID_INPUT`) with a structured "JUnit reporter expects diagnostics shape" message

### Requirement: `dq fix` subcommand

`dq fix [GLOBAL-FLAGS] FILE... [--rules <RULE>...]` SHALL apply every applicable rule's `fix.jq` to the given files (or glob patterns). Argument shape mirrors `dq lint`. Behaviour mirrors `dq set` / `dq del` for the write-mode flags (`-i` / `--diff` / `--check` / `--continue-on-error` / `--parallel` / `--backup`); the bulk driver is the same code path. The handler rejects `--allow-templates` and `--raw-template-strings` up front because the re-emit path through `Format::write_with_options` does not preserve template placeholder positions, mirroring the `dq set --jq` rejection.

#### Scenario: `dq fix --check` exits 1 when a fix would apply

- **WHEN** `dq fix --check --rules @std/k8s deploy.yaml` would change at least one file
- **THEN** the handler returns `crate::error::CheckPending` (mapped to exit 1) and writes `would modify: <path>` lines to stdout

#### Scenario: `dq fix -i` writes the post-fix bytes atomically

- **WHEN** `dq fix -i --rules @std/npm package.json` runs against a license-less package.json
- **THEN** the file on disk gains `"license": "UNLICENSED"` via `dq_core::atomic_write::write`

#### Scenario: `dq fix --diff` renders a unified diff without writing

- **WHEN** `dq fix --diff --rules @std/k8s deploy.yaml` runs
- **THEN** stdout receives a unified diff and the file on disk is unchanged

#### Scenario: `dq fix --allow-templates` is rejected up front

- **WHEN** `dq fix --allow-templates -i --rules <rule> <file>` runs
- **THEN** the handler returns `crate::error::InvalidInput` (exit 6) whose message names `--allow-templates`

### Requirement: `Cli::Command::Fix` variant

The `Command` enum SHALL expose a `Fix(FixArgs)` variant. `FixArgs { files: Vec<Utf8PathBuf>, rules: Vec<String> }` mirrors `LintArgs` shape. The dispatcher in `crates/dq-cli/src/lib.rs::dispatch` SHALL route `Command::Fix(args)` to `commands::fix::run(cli, args, input_format, use_color, out)`.

#### Scenario: `dq fix` is parsed as `Command::Fix`

- **WHEN** the CLI is invoked with `dq fix --rules @std/k8s deploy.yaml`
- **THEN** clap parses the input into `Command::Fix(FixArgs { files: [deploy.yaml], rules: ["@std/k8s"] })` and the dispatcher routes it to `commands::fix::run`

