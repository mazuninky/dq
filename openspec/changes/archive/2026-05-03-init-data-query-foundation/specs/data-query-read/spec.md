## ADDED Requirements

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
