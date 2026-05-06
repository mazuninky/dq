## ADDED Requirements

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

`crates/dq-cli/src/exit_code.rs` SHALL define `pub const SUCCESS: i32 = 0`, `GENERIC: i32 = 1`, `NOT_FOUND: i32 = 2`, `PARSE_ERROR: i32 = 3`, `VALIDATE_FAIL: i32 = 4`, `IO_ERROR: i32 = 5`, `INVALID_INPUT: i32 = 6`. The `exit_code_for_error(err: &anyhow::Error) -> i32` function SHALL `downcast_ref` to the domain `Error` enum and return the matching constant; unrecognised errors fall back to `GENERIC`.

#### Scenario: Path-not-found maps to NOT_FOUND
- **WHEN** a command produces an `Error::Path { .. }` and `main.rs` invokes `exit_code_for_error`
- **THEN** the returned exit code is 2

#### Scenario: Generic anyhow error
- **WHEN** a handler returns `anyhow::bail!("disk full")` (no domain Error)
- **THEN** the exit code is 1

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

In M1 the binary SHALL NOT include any of the following commands: `set`, `del`, `patch`, `merge`, `diff`, `fmt`, `query`, `lint`, `check`, `test`, `fix`, `explain`, `rules`, `self`, `init`, `config`. They are reserved for later milestones. The corresponding `Command` enum variants either MUST be omitted entirely or MUST be marked `hide = true` and emit "unavailable in this build" errors.

#### Scenario: Reserved subcommand is unreachable
- **WHEN** the user runs `dq set config.yaml /x 1`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6) OR a hidden subcommand emits the structured "arrives in M2" error and exits 1
