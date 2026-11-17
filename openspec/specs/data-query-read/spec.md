# data-query-read Specification

## Purpose

Defines the read-only subcommands of `dq` for M1: `get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `convert`, `validate`. Establishes their I/O contract, exit codes, error classes, and the explicit anti-scope of mutating commands.
## Requirements
### Requirement: Get value at pointer

The CLI SHALL provide a `dq get <file> <pointer>` command that reads the value at the given JSON Pointer location and emits it in the requested output format.

#### Scenario: Existing leaf value
- **WHEN** the user runs `dq get config.yaml /server/port` against a YAML file where `server.port = 8080`
- **THEN** the command writes `8080` to stdout in console format and exits with code 0

#### Scenario: Pointer to a nested object
- **WHEN** the user runs `dq get config.yaml /server -F json`
- **THEN** the command writes the full subtree as a JSON object to stdout and exits with code 0

#### Scenario: Missing pointer
- **WHEN** the user runs `dq get config.yaml /missing/path`
- **THEN** the command writes a structured error (with `kind=path`, the original pointer, the longest matching prefix, and a `did_you_mean` suggestion when sibling keys are similar) to stderr and exits with code 2 (`exit_code::NOT_FOUND`)

### Requirement: Existence check via exit code

The CLI SHALL provide `dq exists <file> <pointer>` that reports presence purely via exit code, with no stdout output.

#### Scenario: Pointer exists
- **WHEN** the user runs `dq exists config.yaml /server/port` and the value exists
- **THEN** stdout is empty and exit code is 0

#### Scenario: Pointer missing
- **WHEN** the user runs `dq exists config.yaml /missing` and the value does not exist
- **THEN** stdout is empty, stderr is empty (no error rendering), and exit code is 1

### Requirement: Object key enumeration

The CLI SHALL provide `dq keys <file> <pointer>` that lists keys of the object at the pointer location.

#### Scenario: Object pointer
- **WHEN** the user runs `dq keys config.yaml /server` and that pointer addresses an object
- **THEN** the command writes one key per line in console format (or as a JSON string array under `-F json`), preserving source order, and exits 0

#### Scenario: Non-object pointer
- **WHEN** the user runs `dq keys config.yaml /server/port` and that pointer addresses a scalar
- **THEN** the command writes a structured error with `kind=type` and exits with code 1

### Requirement: Object value enumeration

The CLI SHALL provide `dq values <file> <pointer>` that lists values of the object at the pointer location, preserving source order.

#### Scenario: Object pointer
- **WHEN** the user runs `dq values config.yaml /server -F json`
- **THEN** the command writes a JSON array containing all values from the object (in source order) and exits 0

### Requirement: Length of array, string, or object

The CLI SHALL provide `dq len <file> <pointer>` that returns the number of elements (array), characters (string, counted as Unicode scalar values via `str::chars().count()`; grapheme-cluster counting is an M2+ enhancement that requires `unicode-segmentation`), or keys (object).

#### Scenario: Array length
- **WHEN** the user runs `dq len config.yaml /servers` and that pointer addresses an array of three items
- **THEN** stdout is `3` and exit code is 0

#### Scenario: Length of scalar bool/number/null
- **WHEN** the user runs `dq len config.yaml /enabled` and that pointer addresses a boolean
- **THEN** the command writes a structured error with `kind=type` and exits with code 1

### Requirement: Value type discovery

The CLI SHALL provide `dq type <file> <pointer>` returning one of `null`, `bool`, `int`, `float`, `string`, `array`, `object`.

#### Scenario: Scalar type
- **WHEN** the user runs `dq type config.yaml /server/port` for an integer
- **THEN** stdout is `int` and exit code is 0

### Requirement: Path enumeration

The CLI SHALL provide `dq paths <file>` that emits every reachable JSON Pointer in the document.

#### Scenario: Default tree output
- **WHEN** the user runs `dq paths config.yaml -F json`
- **THEN** the command writes a JSON object whose keys are pointers (RFC 6901 strings) and whose values are leaf type names, covering every leaf and intermediate object/array node, and exits 0

#### Scenario: Console output
- **WHEN** the user runs `dq paths config.yaml` without `-F`
- **THEN** the command writes one pointer per line, preserving source order, and exits 0

### Requirement: JSONPath select

The CLI SHALL provide `dq select <file> <jsonpath>` that runs an RFC 9535 JSONPath query against the document and emits matching values as a JSON array on stdout.

#### Scenario: Single-match query
- **WHEN** the user runs `dq select deployment.yaml '$.spec.replicas'` for a manifest with `spec.replicas = 3`
- **THEN** stdout is `[3]` and exit code is 0

#### Scenario: Multi-match query
- **WHEN** the user runs `dq select deployment.yaml '$.spec.containers[*].image'` for a manifest with three containers
- **THEN** stdout is a JSON array containing the three image strings in document order and exit code is 0

#### Scenario: No matches
- **WHEN** the user runs `dq select config.yaml '$.does.not.exist'`
- **THEN** stdout is `[]` and exit code is 0 (empty match is not an error in `select`)

### Requirement: Format conversion

The CLI SHALL provide `dq convert <file>` that re-emits the input document in the format selected by `-F` (yaml/json/toml/toon/jsonl). In M1 the command writes to stdout only — no `-i/--in-place`.

#### Scenario: YAML to JSON
- **WHEN** the user runs `dq convert deployment.yaml -F json`
- **THEN** stdout contains the document serialized as canonical JSON and exit code is 0

#### Scenario: JSON to TOON
- **WHEN** the user runs `dq convert package.json -F toon`
- **THEN** stdout contains a TOON-encoded representation produced by the `toon-format` crate and exit code is 0

#### Scenario: Lossy conversion warning
- **WHEN** the user runs `dq convert config.yaml -F json` and the source contains comments or anchors
- **THEN** the command emits a `tracing::warn!` line stating which formatting metadata is dropped (anchors, comments, key order is preserved) and still exits 0

### Requirement: Validate document structure

The CLI SHALL provide `dq validate <file>` that exits 0 when the file parses successfully in its detected/declared format and exits with code 4 (`exit_code::VALIDATE_FAIL`) when it does not, emitting a structured parse error.

#### Scenario: Well-formed document
- **WHEN** the user runs `dq validate config.yaml` against a syntactically valid YAML document
- **THEN** stdout is empty and exit code is 0

#### Scenario: Malformed document
- **WHEN** the user runs `dq validate broken.json` against a JSON file with a stray comma
- **THEN** stderr contains a structured error (line, column, span, caret indicator, optional suggestion) and exit code is 4

### Requirement: Read-only command isolation in M1

In M1 the CLI SHALL NOT expose any command that mutates a file on disk. The flags `-i/--in-place`, `--diff`, `--backup` MUST be parsed (so M2 wires them in without breaking compatibility) but MUST emit an "unsupported in this build" structured error and exit 1 for any command in M1.

#### Scenario: User attempts in-place edit
- **WHEN** the user runs `dq get config.yaml /x -i`
- **THEN** the command exits 1 with a structured error stating that `--in-place` requires write commands which arrive in M2, and pointing to `dq-plan.md` M2 anchor

### Requirement: Read-side dispatch covers all M5 formats

Every read-side subcommand defined in this capability (`get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `validate`) SHALL accept files in the seven new M5 formats (HCL, INI, `.env`, CSV, TSV, Dockerfile, ignore-list, Markdown frontmatter) when the format is selected by extension or by an explicit `-F` flag. No subcommand source code change is required — the formats plug in through the registry — but each subcommand's behaviour MUST be identical to the existing four formats with respect to output shape and exit codes.

#### Scenario: `dq get` on an HCL file
- **WHEN** the user runs `dq get terraform_main.tf /backend/0/region` on an HCL file whose `backend` block has a single labeled `s3` block with a `region` field
- **THEN** the command writes the region as the only stdout line and exits with code 0

#### Scenario: `dq paths` on a `.env` file
- **WHEN** the user runs `dq paths service.env` on a `.env` file with three KEY=VALUE entries
- **THEN** the command writes three pointers (`/<KEY1>`, `/<KEY2>`, `/<KEY3>`) and exits with code 0

#### Scenario: `dq paths` on `.gitignore`
- **WHEN** the user runs `dq paths .gitignore` on a file with five non-comment patterns
- **THEN** the command writes five integer pointers (`/0` through `/4`) and exits with code 0

#### Scenario: `dq validate` on a Dockerfile
- **WHEN** the user runs `dq validate Dockerfile` on a syntactically valid Dockerfile
- **THEN** stdout is empty, stderr is empty, and exit code is 0

#### Scenario: `dq validate` on a malformed Dockerfile
- **WHEN** the user runs `dq validate Dockerfile` on a file whose first instruction is not a valid Dockerfile keyword
- **THEN** the command writes a structured parse error and exits with code 4 (`VALIDATE_FAIL`)

#### Scenario: `dq get` on a Markdown frontmatter file
- **WHEN** the user runs `dq get hugo_post.md /title` on a file with `---\ntitle: Hello\n---\n# body\n`
- **THEN** the command writes `Hello` as the only stdout line and exits with code 0; the body of the markdown file is NOT inspected

### Requirement: Read-only formats produce a clear error on write commands

For Dockerfile and ignore-list inputs, any subcommand that requires a write target (`set`, `del`, `patch`, `merge` with `-i`; `convert` with the same format target via `-F dockerfile` / `-F ignore-list`) SHALL produce an unambiguous error that names the read-only format. The write commands continue to use the existing `Error::WriteUnavailable` (which maps to exit 7 / `WRITE_FAILED`); the `convert` command rejects the read-only target at the clap layer (exit 6 / `INVALID_INPUT`).

#### Scenario: `dq set Dockerfile ... -i` errors with read-only message
- **WHEN** the user runs `dq set Dockerfile /0/instruction RUN -i`
- **THEN** the command writes a structured error mentioning "dockerfile" and the lack of write support, and exits with code 7

#### Scenario: `dq convert deploy.yaml -F ignore-list` rejected by clap
- **WHEN** the user runs `dq convert deploy.yaml -F ignore-list`
- **THEN** the command exits with code 6 and the error message names "ignore-list" as an invalid value for `-F`

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

