# data-query-read Specification (delta)

## ADDED Requirements

### Requirement: jq query subcommand

The CLI SHALL provide `dq query <expression> <file>` that evaluates the jq expression against the parsed document and emits the resulting value stream through the configured `Reporter`.

The handler:

1. Rejects every write-mode flag via `Cli::ensure_no_write_flags` (exit 6 / `INVALID_INPUT` if any are set).
2. Loads the document via the standard format-detection path (`-F` override > extension), reading from stdin when `<file>` is `-` and `-F` is supplied.
3. Resolves `--doc` exactly as `dq get` does: single-doc files ignore `--doc`; multi-doc YAML defaults to index 0, accepts `--doc N` for a specific index, and `--doc all` for the entire stream as a JSON array.
4. Converts the selected `dq_core::Value` to a `serde_json::Value`.
5. Compiles the jq expression via `dq_transform::JqEngine::compile`. Compile errors are converted to `dq_core::Error::Parse` so the existing exit-code mapper picks `PARSE_ERROR = 3` and the existing console renderer prints the standard caret diagnostic.
6. Evaluates the filter via `JqEngine::run`. Runtime errors are wrapped in `anyhow::anyhow!(...)` and fall through to `GENERIC = 1`.
7. Renders the resulting value stream:
   - **JSON / JSONL / YAML / TOML / TOON reporters**: stream materialised as a JSON array (`[v1, v2, …]`), then handed to the reporter exactly once.
   - **Console reporter**: each value rendered on its own line.
   - **SARIF reporter**: rejected via the existing `BannedReporter` pattern with a structured "wrong reporter for this verb" error (exit 6).

#### Scenario: Single-output filter prints one value
- **WHEN** the user runs `dq query '.spec.replicas' deployment.yaml` against a manifest with `spec.replicas: 3`
- **THEN** stdout contains `3` (one line) and exit code is 0

#### Scenario: Multi-output filter prints array
- **WHEN** the user runs `dq query '.spec.containers[].image' deployment.yaml -F json` against a manifest with three containers
- **THEN** stdout contains a JSON array of three image strings in document order and exit code is 0

#### Scenario: Empty result is not an error
- **WHEN** the user runs `dq query '.does.not.exist' config.yaml -F json`
- **THEN** stdout is `[null]` (jq's standard "missing key returns null" semantics), exit code is 0

#### Scenario: Update assignment is read-only at the query level
- **WHEN** the user runs `dq query '.spec.replicas |= . + 1' deployment.yaml -F json`
- **THEN** stdout contains the *transformed* document as JSON; the file on disk is NOT modified (the `query` verb never writes to disk regardless of the expression)

#### Scenario: Compile error maps to PARSE_ERROR
- **WHEN** the user runs `dq query '.foo |=' config.yaml`
- **THEN** stderr contains a structured error mentioning the unterminated assignment and the byte offset, and exit code is 3 (`PARSE_ERROR`)

#### Scenario: Runtime error maps to GENERIC
- **WHEN** the user runs `dq query '. + 1' "string-only.yaml"` against a YAML file whose top-level value is a string
- **THEN** stderr contains the runtime error message from jaq and exit code is 1 (`GENERIC`)

#### Scenario: Read flag rejection
- **WHEN** the user runs `dq query '.x' file.yaml -i`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names `--in-place` as not supported by `query`

#### Scenario: Stdin read with explicit format
- **WHEN** the user runs `cat config.yaml | dq query '.foo' - -F yaml`
- **THEN** the command reads stdin as YAML, evaluates the filter, and writes the result to stdout (exit 0)

#### Scenario: Stdin read without format errors
- **WHEN** the user runs `cat config.yaml | dq query '.foo' -`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error names "stdin requires -F"

#### Scenario: Multi-doc YAML uses --doc
- **WHEN** the user runs `dq query '.kind' multi.yaml --doc 1` against a stream with two documents (`Service` then `Deployment`)
- **THEN** stdout is `Deployment` (the second document) and exit code is 0

#### Scenario: --doc all queries the entire stream
- **WHEN** the user runs `dq query '. | length' multi.yaml --doc all` against a stream with three documents
- **THEN** stdout is `3` (jq's `length` on an array of three documents) and exit code is 0

#### Scenario: SARIF reporter rejected for query
- **WHEN** the user runs `dq query '.x' file.yaml -F sarif`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error names "sarif" as an unsupported reporter for query results
