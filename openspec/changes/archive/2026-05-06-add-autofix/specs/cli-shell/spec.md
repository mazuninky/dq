## ADDED Requirements

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
