# cli-shell Specification (delta)

## ADDED Requirements

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

## MODIFIED Requirements

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
