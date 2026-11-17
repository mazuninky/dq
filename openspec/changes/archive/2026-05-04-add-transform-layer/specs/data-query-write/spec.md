# data-query-write Specification (delta)

## ADDED Requirements

### Requirement: `dq set --jq EXPR` transform mode

The `set` subcommand SHALL accept an optional `--jq <EXPR>` flag that switches the handler into transform mode. When `--jq` is set:

1. The `<POINTER>` and `[VALUE]` positional arguments become **mutually exclusive** with `--jq`. Clap rejects `dq set FILE POINTER --jq EXPR` and `dq set FILE POINTER VALUE --jq EXPR` with `INVALID_INPUT` (exit 6) at the parse layer; the handler also re-checks at runtime to defend against any clap-level miss.
2. The handler reads `<FILE>` (format detected by extension or `-F`).
3. Parses through the format's standard `Format::parse` (NOT the write-aware `parse_yaml_with_spans` / `parse_json_with_spans`) — the textual-edit splice path is bypassed because jq can change the document's structure arbitrarily.
4. Converts the parsed `dq_core::Value` to a `serde_json::Value` via the existing `value_to_serde_json` helper.
5. Compiles the jq expression via `dq_transform::JqEngine::compile`. Compile errors are wrapped in `dq_core::Error::Parse` so the existing exit-code mapper picks `PARSE_ERROR = 3`.
6. Evaluates the filter via `JqEngine::run`. Runtime errors are wrapped in `anyhow::anyhow!(...)` and fall through to `GENERIC = 1`. The output stream MUST contain exactly **one** value; zero outputs and multi-output streams are rejected with `INVALID_INPUT` (exit 6) and a message naming the count.
7. Converts the single output back to a `dq_core::Value`.
8. Re-emits the document via `Format::write_with_options` against the global `WriteOptions` (so `--sort-keys` / `--indent` work).
9. The bulk driver receives the new bytes through `FileOpResult::Modified` and applies `-i` / `--diff` / `--check` exactly as for the existing `set` modes.

A `tracing::debug!` line on the splice-vs-re-emit fork notes that comments will be lost when `--jq` is active. The `set --help` text mentions the comment-loss tradeoff next to the `--jq` flag description.

`--jq` is compatible with every existing global flag: `-i`, `--diff`, `--check`, `--backup`, `--continue-on-error`, `--parallel`, glob expansion via the bulk driver. Filter compilation happens **once** outside the per-file loop; bulk runs share a single compiled engine via `Arc<JqEngine>` across rayon workers.

The handler SHALL reject `--jq` combined with the template-guard flags `--allow-templates` or `--raw-template-strings` (`INVALID_INPUT`, exit 6). The `--jq` path uses `Format::parse` directly without the M2 template-substitution pre-pass, and the re-emit step does not restore template placeholders — so the template-guard flags would be silently ignored if the combination were accepted. Future milestones may integrate template-guard support into the `--jq` path; until then the rejection is the documented contract.

#### Scenario: `--jq` rejected with `--allow-templates`
- **WHEN** the user runs `dq set helm-values.yaml --jq '.foo |= 2' -i --allow-templates`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and `--allow-templates`

#### Scenario: `--jq` rejected with `--raw-template-strings`
- **WHEN** the user runs `dq set helm-values.yaml --jq '.foo |= 2' -i --raw-template-strings`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and `--raw-template-strings`

#### Scenario: `--jq` increments a counter
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 1' -i` against a manifest with `spec.replicas: 3`
- **THEN** the file on disk has `spec.replicas: 4`, stdout is empty, and exit code is 0

#### Scenario: `--jq` adds a new key
- **WHEN** the user runs `dq set deploy.yaml --jq '. + {"newKey": "newValue"}' -i` against an object document
- **THEN** the file on disk contains the new top-level key with the new value, every existing key is preserved, and exit code is 0

#### Scenario: `--jq` removes a key
- **WHEN** the user runs `dq set deploy.yaml --jq 'del(.metadata.annotations.old)' -i`
- **THEN** the `metadata.annotations.old` key is removed, sibling keys preserve order, and exit code is 0

#### Scenario: `--jq` with a positional VALUE is rejected
- **WHEN** the user runs `dq set deploy.yaml /spec/replicas 5 --jq '. + 1'`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and the positional VALUE as conflicting

#### Scenario: `--jq` with a positional POINTER is rejected
- **WHEN** the user runs `dq set deploy.yaml /spec/replicas --jq '. + 1'`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message states that POINTER is not accepted alongside `--jq` (the entire document is the transform target)

#### Scenario: `--jq` multi-output stream is rejected
- **WHEN** the user runs `dq set deploy.yaml --jq '.[]' -i` against an array document
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names the count and suggests wrapping in `[...]` to collect

#### Scenario: `--jq` empty stream is rejected
- **WHEN** the user runs `dq set deploy.yaml --jq 'empty' -i`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message states that the document would become empty

#### Scenario: `--jq` compile error maps to PARSE_ERROR
- **WHEN** the user runs `dq set deploy.yaml --jq '.foo |=' -i`
- **THEN** stderr contains a structured error mentioning the unterminated assignment and exit code is 3

#### Scenario: `--jq` runtime error maps to GENERIC
- **WHEN** the user runs `dq set string-only.yaml --jq '. + 1' -i` against a YAML file whose top-level value is a string
- **THEN** stderr contains the runtime type-error message and exit code is 1

#### Scenario: `--jq` re-emits via the native writer (comment loss)
- **WHEN** the user runs `dq set commented.yaml --jq '.foo |= 2' -i` against a YAML file with leading comments
- **THEN** the file on disk has the new `foo` value AND the comments are dropped (re-emit semantics, documented behaviour)

#### Scenario: `--jq` with `--diff` renders unified diff
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 1' --diff`
- **THEN** stdout contains a unified diff with `-replicas: 3` and `+replicas: 4`, the file on disk is unchanged, and exit code is 0

#### Scenario: `--jq` with `--check` reports pending change
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 1' --check` and the transform would change the file
- **THEN** the command exits with code 1 (`CheckPending` → `GENERIC`) and stderr names the file

#### Scenario: `--jq` is idempotent through `--check`
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 0' --check` (a no-op transform)
- **THEN** the command exits with code 0 (no file would be modified)

#### Scenario: `--jq` works across glob expansion
- **WHEN** the user runs `dq set 'k8s/**/*.yaml' --jq '.spec.template.spec.containers[0].image |= sub(":latest"; ":v1")' -i`
- **THEN** every matching file with a container[0] image ending in `:latest` is updated, the bulk summary lists the modified files, and exit code is 0

#### Scenario: `--jq` shares one compiled engine across rayon workers
- **WHEN** the user runs `dq set 'k8s/**/*.yaml' --jq '.spec.replicas |= . + 1' -i --parallel 4`
- **THEN** the filter is compiled exactly once (verified by a `tracing::debug!` count assertion in the integration test) and the parallel workers share the engine via `Arc`

## MODIFIED Requirements

### Requirement: Anti-scope for M2 write commands

In M7 the binary SHALL include `set`, `del`, `patch`, `merge`, `diff`, `convert -i`, and `dq fmt` with the M3 bulk driver, plus the new `dq query` read subcommand and the `set --jq EXPR` transform mode. It SHALL NOT include the linter family (`lint`/`check`/`test`/`explain`/`rules`/`fix`), markdown body parsing, JSON Schema validation, composite-rules, transactional bulk writes (rolling back successful files when a later file fails), or `dq query --in-place`. They are reserved for M8, M9, M10, and M11. Attempts to use them SHALL produce clap "unknown argument" errors (exit 6).

The previously-deferred YAML-emitter flags `--quote-style <double|single|auto>`, `--flow-style <block|flow|auto>`, and `--strip-comments` remain reserved (their implementation requires a comment-preserving emitter — see [dq-plan.md](../../../dq-plan.md)).

#### Scenario: Linter subcommand is still unreachable
- **WHEN** the user runs `dq lint config.yaml`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6)

#### Scenario: `--quote-style` is still unknown
- **WHEN** the user runs `dq fmt config.yaml --quote-style double`
- **THEN** clap exits with code 6 and "unrecognized argument" error

#### Scenario: `dq query` is reachable in M7
- **WHEN** the user runs `dq query --help`
- **THEN** clap prints the help for `query` and exits 0

#### Scenario: `dq set --jq` is reachable in M7
- **WHEN** the user runs `dq set --help`
- **THEN** the help text lists the `--jq <EXPR>` flag with its description and the conflict notes for POINTER / VALUE
