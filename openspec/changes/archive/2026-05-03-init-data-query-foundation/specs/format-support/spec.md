## ADDED Requirements

### Requirement: Format trait abstraction

The `dq-core` crate SHALL define a `trait Format` exposing at minimum: `parse(bytes: &[u8]) -> Result<Document>`, `write(doc: &Document, w: &mut dyn Write) -> Result<()>`, `extensions() -> &'static [&'static str]`, and `name() -> &'static str`. Adding a new format MUST NOT require changes to consumers other than registering the implementation in the format dispatcher.

#### Scenario: Plug-in registration
- **WHEN** a new struct that implements `Format` is added to the registry under a new file extension
- **THEN** every existing CLI command (`get`, `paths`, `convert`, `validate`, etc.) accepts files with that extension without source changes outside the registry module

### Requirement: YAML 1.2 read support

`dq-core` SHALL parse YAML 1.2 documents (single-document and multi-document via `---` separators) into `Document`, preserving key order from source. In M1 the parser does NOT need to preserve comments, anchor names, or quote style — those become required in M2 round-trip.

#### Scenario: Single document with mixed types
- **WHEN** the parser is fed a YAML document containing strings, integers, floats, booleans, nulls, sequences, and mappings
- **THEN** every value is correctly typed in the resulting `Document` and the order of mapping keys matches source order

#### Scenario: Multi-document YAML
- **WHEN** the parser is fed a YAML stream containing two documents separated by `---`
- **THEN** parsing returns a `MultiDocument` value and the global flag `--doc <idx|all>` (parsing only — full semantics in M2) selects the document; default is document 0

### Requirement: JSON read support

`dq-core` SHALL parse RFC 8259 JSON documents into `Document`, preserving object key order from source.

#### Scenario: Object key order preservation
- **WHEN** a JSON object is parsed and re-emitted via `dq convert -F json`
- **THEN** keys appear in stdout in the same order as they appear in the input bytes

#### Scenario: Number precision
- **WHEN** a JSON document contains an integer larger than `i64::MAX` (e.g. `4722366482869645213696`)
- **THEN** the value is stored without precision loss (as `Document::BigInt` carrying the original textual representation) and re-emits byte-for-byte under `dq convert -F json`

### Requirement: TOML read support

`dq-core` SHALL parse TOML 1.0 documents into `Document`, preserving table and key order.

#### Scenario: Nested tables
- **WHEN** a TOML file with `[server]` and `[server.tls]` sections is parsed
- **THEN** the resulting `Document` represents `server.tls` as a nested object under `server`

### Requirement: Newline-delimited JSON (JSONL) read and write

`dq-core` SHALL handle JSONL / NDJSON input as an array-of-records stream — one JSON value per line — and SHALL write JSONL by emitting one line per top-level array element.

#### Scenario: JSONL read collapses to array
- **WHEN** `dq paths logs.jsonl` is run on a JSONL file with three records
- **THEN** the document is treated as an array of three values, addressable via `/0`, `/1`, `/2`

#### Scenario: JSONL write
- **WHEN** the user runs `dq convert data.json -F jsonl` on a top-level JSON array of three objects
- **THEN** stdout contains exactly three lines, each a compact JSON object, terminated by `\n`

### Requirement: TOON write support via `toon-format` crate

`dq-cli` SHALL emit TOON output by depending on the `toon-format = "0.4"` crate. The project MUST NOT ship its own TOON encoder.

#### Scenario: TOON output is delegated to `toon-format`
- **WHEN** the user runs `dq convert package.json -F toon`
- **THEN** the command serializes the `Document` via `toon_format::encode` (or the crate's documented public API) without any in-tree TOON encoder

### Requirement: Format auto-detection and override

`dq-cli` SHALL detect format by file extension (`.yaml`/`.yml` → yaml, `.json` → json, `.toml` → toml, `.jsonl`/`.ndjson` → jsonl) and accept `-F/--format` to override detection or to set output format for `convert`.

#### Scenario: Extension-based detection
- **WHEN** the user runs `dq get config.yaml /x` without `-F`
- **THEN** the YAML parser is selected

#### Scenario: stdin requires explicit format
- **WHEN** the user runs `dq get - /x` reading from stdin without `-F`
- **THEN** the command writes a structured error stating that input from stdin requires `-F <fmt>` and exits with code 1

#### Scenario: Format override
- **WHEN** the user runs `dq get unknown.txt /x -F json` against a JSON-formatted file with a non-standard extension
- **THEN** the JSON parser is used regardless of extension

### Requirement: Number representation preservation

When reading numeric values, `dq-core` SHALL preserve the original textual representation for any integer that does not fit in `i64`/`u64` and for any float whose serialization would lose precision; these are stored in dedicated `Document::BigInt(String)` / `Document::BigFloat(String)` variants. Standard-precision values use `Document::Int(i64)` and `Document::Float(f64)`.

#### Scenario: Round-trip large integer through convert
- **WHEN** a JSON file contains `{"id": 4722366482869645213696}` and the user runs `dq convert big.json -F json`
- **THEN** stdout contains the same integer literal, character-for-character

### Requirement: M1 anti-scope for formats

The crate SHALL NOT include parsers for HCL, INI, .env, CSV/TSV, Dockerfile, .gitignore, XML, Markdown, or Markdown frontmatter in M1; those are covered by M5/M9 capabilities.

#### Scenario: Unsupported format error
- **WHEN** the user runs `dq get script.sh /x` (no registered format for `.sh`)
- **THEN** the command writes a structured error suggesting `-F <fmt>` and exits with code 1
