# format-support Specification

## Purpose

Defines the formats `dq` reads and writes in M1, the trait abstraction that lets new formats plug in without changes to consumers, and how format selection (extension detection vs. explicit `-F` override) works. Also pins external crate boundaries (e.g. TOON via `toon-format`) and number-precision preservation.
## Requirements
### Requirement: Format trait abstraction

The `dq-core` crate SHALL define a `trait Format` exposing at minimum: `parse(bytes: &[u8]) -> Result<Document>`, `write(doc: &Document, w: &mut dyn Write) -> Result<()>`, `extensions() -> &'static [&'static str]`, and `name() -> &'static str`. Adding a new format MUST NOT require changes to consumers other than registering the implementation in the format dispatcher.

#### Scenario: Plug-in registration
- **WHEN** a new struct that implements `Format` is added to the registry under a new file extension
- **THEN** every existing CLI command (`get`, `paths`, `convert`, `validate`, etc.) accepts files with that extension without source changes outside the registry module

### Requirement: YAML 1.2 read support

`dq-core` SHALL parse YAML 1.2 documents (single-document and multi-document via `---` separators) into `Document`, preserving key order from source. The write path additionally builds a `Pointer → ByteRange` span map alongside the original bytes, so `Document::set_at` / `Document::del_at` can splice spans without re-emitting unchanged surroundings (D1, D4). Read-only commands (`get`, `paths`, `keys`, etc.) SHALL continue to operate against the bare `Value` projection (`Document::value()`) and remain behaviourally equivalent to M1 for non-round-trip use cases.

#### Scenario: Single document with mixed types
- **WHEN** the parser is fed a YAML document containing strings, integers, floats, booleans, nulls, sequences, and mappings
- **THEN** every value is correctly typed in the resulting `Document` and the order of mapping keys matches source order

#### Scenario: Multi-document YAML
- **WHEN** the parser is fed a YAML stream containing two documents separated by `---`
- **THEN** parsing returns a multi-document `Document` (`Document::is_multi()` is true and `doc.values()` yields the document sequence), and the global flag `--doc <idx|all>` selects the document; default is document 0

#### Scenario: M1 read-command behavior is preserved
- **WHEN** any M1 read command (`get`, `paths`, `keys`, `values`, `len`, `type`, `select`, `validate`) is run on the same fixture file before and after the M2 yaml_spans introduction
- **THEN** stdout is byte-identical and exit code is identical

### Requirement: JSON read support

`dq-core` SHALL parse RFC 8259 JSON documents into `Document`, preserving object key order from source.

#### Scenario: Object key order preservation
- **WHEN** a JSON object is parsed and re-emitted via `dq convert -F json`
- **THEN** keys appear in stdout in the same order as they appear in the input bytes

#### Scenario: Number precision
- **WHEN** a JSON document contains an integer larger than `i64::MAX` (e.g. `4722366482869645213696`)
- **THEN** the value is stored without precision loss (as `Value::BigInt` carrying the original textual representation) and re-emits byte-for-byte under `dq convert -F json`

### Requirement: TOML read support

`dq-core` SHALL parse TOML 1.0 documents into `Document` via `toml_edit`, preserving table and key order AND retaining per-node metadata for round-trip. Read-only commands SHALL behave identically to the M1 baseline.

#### Scenario: Nested tables
- **WHEN** a TOML file with `[server]` and `[server.tls]` sections is parsed
- **THEN** the resulting `Document` represents `server.tls` as a nested object under `server`

#### Scenario: M1 TOML test fixtures still pass
- **WHEN** the existing `crates/dq-core/tests/parse_toml.rs` test suite is executed against the `toml_edit`-based parser
- **THEN** all five tests pass without modification of the test code

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

When reading numeric values, `dq-core` SHALL preserve the original textual representation for any integer that does not fit in `i64`/`u64` and for any float whose serialization would lose precision; these are stored in dedicated `Value::BigInt(String)` / `Value::BigFloat(String)` variants of the `Value` enum carried by a `Document`. Standard-precision values use `Value::Int(i64)` and `Value::Float(f64)`. The textual representation MUST round-trip byte-for-byte through `set` mutation as well as `convert`.

#### Scenario: Round-trip large integer through convert
- **WHEN** a JSON file contains `{"id": 4722366482869645213696}` and the user runs `dq convert big.json -F json`
- **THEN** stdout contains the same integer literal, character-for-character

#### Scenario: Round-trip large integer through set
- **WHEN** the user runs `dq set big.json /id 4722366482869645213696 -i` followed by `dq get big.json /id`
- **THEN** the get output is exactly `4722366482869645213696`

### Requirement: M1 anti-scope for formats

The crate SHALL NOT include parsers for the conftest-only formats (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL); those remain anti-scope per [dq-plan.md:600-612](../../../dq-plan.md:600). XSD / RelaxNG / Schematron schema-validators are also anti-scope (covered separately if/when a use case appears).

The earlier wording deferring **XML write** to M11 is now superseded: M11 added full XML read+write through the `XmlFormat` requirement below. The earlier wording deferring the M9 markdown body parser is also obsolete (already shipped in M9).

#### Scenario: Unsupported format error
- **WHEN** the user runs `dq get script.sh /x` (no registered format for `.sh`)
- **THEN** the command writes a structured error suggesting `-F <fmt>` and exits with code 1

### Requirement: YAML round-trip preservation

`dq-core` SHALL parse YAML 1.2 documents into a `Document` that retains the original byte sequence and a `Pointer → ByteRange` span map sufficient for round-trip preservation via the textual-edit approach (D1, D4 of the change's design.md). Reading a file and writing it back without mutation SHALL produce byte-identical output. Mutating exactly one scalar via `Document::set_at` and writing back SHALL change exactly the lines necessary to express the new value, leaving every other byte (comments, blank lines, anchor declarations, alias references, indent, quote style, flow-vs-block choice, document separators) unchanged.

The write-path span builder SHALL use `saphyr-parser` (low-level event API) for structural discovery only. The read path continues to use `serde_yml` for parse-to-`Value`, and `serde_yml` SHALL remain a `dq-core` dependency through M2 (D13). Removal of `serde_yml` is deferred to a post-M2 refactor change.

#### Scenario: Round-trip without mutation is byte-identical
- **WHEN** a Helm chart `values.yaml` containing comments, anchors, and merge keys is parsed and immediately written back via `Format::write`
- **THEN** the output bytes are equal to the input bytes (`==`)

#### Scenario: Single scalar mutation changes only one line
- **WHEN** a Kubernetes Deployment with `spec.replicas: 3` (with leading and trailing comments on adjacent lines) is mutated by `Document::set_at(/spec/replicas, Value::Int(5))` and written back
- **THEN** the diff between source and output is a single hunk: `-  replicas: 3` / `+  replicas: 5`; all comments, blank lines, indent, and key order are unchanged

#### Scenario: Anchor and alias preservation
- **WHEN** a YAML document contains `defaults: &base { x: 1 }` and `service: { <<: *base, y: 2 }`, is parsed, and is written back without mutation
- **THEN** the output retains the `&base` anchor declaration, the `<<: *base` merge key, and identical formatting of both mappings

### Requirement: TOML round-trip preservation

`dq-core` SHALL parse TOML 1.0 documents using `toml_edit` (replacing the standard `toml` crate). The same byte-identical round-trip and single-line-mutation guarantees as YAML SHALL apply: comments, blank lines, inline-vs-table style, dotted keys, datetime literals, and key order MUST be preserved.

#### Scenario: Round-trip without mutation is byte-identical for TOML
- **WHEN** a `Cargo.toml` containing comments, dotted keys, and inline tables is parsed and written back via `Format::write`
- **THEN** the output bytes are equal to the input bytes

#### Scenario: Datetime literal preservation
- **WHEN** a TOML file contains `created = 1979-05-27T07:32:00Z` and is mutated only at an unrelated key
- **THEN** the datetime literal is written back exactly as `1979-05-27T07:32:00Z` (not converted to `Z`-stripped or sub-second-padded form)

### Requirement: JSON round-trip preservation

`dq-core` SHALL extend the JSON parser to detect and preserve indent style (2-space, 4-space, tab) and trailing-newline-at-EOF on parsing, and re-emit them on writing. JSON does not formally support comments — if the parser encounters `//` or `/* */`, it SHALL produce a `Parse` error stating "comments are not valid JSON; JSONC is not supported". `IndexMap` key order preservation and `BigInt(literal_text)` precision (already implemented in M1) remain in force.

#### Scenario: 4-space indent preserved
- **WHEN** a `.json` file uses 4-space indentation and is read then written without mutation
- **THEN** the output uses 4-space indentation

#### Scenario: Tab indent preserved
- **WHEN** a `.json` file uses tab indentation and is read then written without mutation
- **THEN** the output uses tab indentation

#### Scenario: JSONC produces structured error
- **WHEN** the user runs `dq get config.jsonc /x` on a file containing `{ /* comment */ "x": 1 }`
- **THEN** the command exits with code 3 and the structured error mentions "JSONC is not supported"

### Requirement: Atomic write helper in `dq-core`

`dq-core` SHALL expose `pub fn atomic_write::write(path: &Utf8Path, content: &[u8], backup: bool) -> Result<()>` which uses `tempfile::NamedTempFile::new_in(path.parent())` followed by `persist(path)`. The helper SHALL handle the `--backup` semantics inline (copy original to `<path>.bak` before persist). Same-directory placement of the temp file is REQUIRED so that the rename is atomic on every supported filesystem (no `EXDEV` errors).

#### Scenario: Temp file lives in target directory
- **WHEN** `atomic_write::write("/var/data/config.yaml", b"...", false)` is invoked
- **THEN** during the write the temporary file is created under `/var/data/` (not `/tmp/`); the temp filename is unpredictable but begins with `.tmp`

#### Scenario: Failure leaves original intact
- **WHEN** the underlying `persist` returns an `Io` error (e.g., EACCES on rename)
- **THEN** the original file at `path` is byte-identical to its pre-call state, the function returns `Err(Error::Io { ... })`, and the temp file may be left behind (caller responsibility — typically cleaned up by `tempfile`'s Drop)

#### Scenario: Backup is created before persist
- **WHEN** `atomic_write::write("/etc/config.yaml", b"new", true)` is invoked on an existing file
- **THEN** after the call, both `/etc/config.yaml` (with new content) and `/etc/config.yaml.bak` (with old content) exist

### Requirement: M2 dependency boundary updates

`crates/dq-core/Cargo.toml` SHALL retain the M3 dependency set: `serde_yml`, `serde_json` (with `preserve_order` + `arbitrary_precision`), `toml_edit` (with `preserve-order` + `parse`), `saphyr-parser`, `regex`, `tempfile`, `similar`. M4 SHALL NOT introduce any new runtime dependencies — `WriteOptions` and `canonicalize_keys` are pure-Rust stdlib code. The `serde_json` `PrettyFormatter::with_indent` API used by `--indent` is already vendored.

#### Scenario: No new dependencies in M4
- **WHEN** `cargo deny check` runs after the M4 change is applied
- **THEN** zero new entries appear in `Cargo.lock` compared to the M3 baseline (excluding patch-level updates)

### Requirement: `dq-core::transform` public surface for ops-as-data primitives

`dq-core` SHALL expose a `transform` module re-exporting three engines:

- `pub fn apply_patch(doc: &mut Document, ops: &[PatchOp]) -> Result<()>` — applies an RFC 6902 patch atomically (clone-on-apply: `doc` is left untouched if any op fails).
- `pub fn apply_merge(doc: &mut Document, patch: &Value) -> Result<()>` — applies an RFC 7396 merge patch (recursive, `null` removes, scalars replace).
- `pub fn diff(a: &Value, b: &Value) -> Vec<PatchOp>` — emits a minimal RFC 6902 patch transforming `a` into `b`.

The `PatchOp` enum SHALL have variants `Add`, `Remove`, `Replace`, `Move`, `Copy`, `Test`, each carrying the RFC 6902 path (and value/from where applicable). All three engines SHALL preserve textual round-trip semantics by going through `Document::set_at` / `Document::del_at` for any byte-level mutation — the engines do NOT bypass the textual-edit pipeline.

#### Scenario: apply_patch is atomic on test failure
- **WHEN** an `apply_patch` call applies an op-list whose third op is a failing `test`
- **THEN** the function returns `Err(Error::PatchTestFailed { ... })` and `doc.original_bytes()` is byte-identical to its pre-call value

#### Scenario: diff round-trips
- **WHEN** for any two `Value`s `a` and `b`, the caller computes `ops = diff(&a, &b)` and applies them to a Document carrying `a`
- **THEN** the resulting Document's value is structurally equal to `b`

#### Scenario: apply_merge null removes
- **WHEN** `apply_merge` is called with patch `{"a": null}` against a Document whose top-level map contains `"a"`
- **THEN** the resulting Document has `"a"` removed and every other key preserved

### Requirement: `WriteOptions` public struct in `dq-core`

`crates/dq-core/src/write_options.rs` SHALL define a public struct:

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WriteOptions {
    pub sort_keys: bool,
    pub indent: Option<u8>,
}
```

The struct SHALL be re-exported from `dq_core` crate root as `pub use write_options::WriteOptions;`. It SHALL be `#[non_exhaustive]` so M5+ can add fields (`quote_style`, `flow_style`, `strip_comments`) without breaking consumers — callers MUST construct via `WriteOptions { sort_keys: true, ..Default::default() }` rather than positionally. `Default` SHALL produce `WriteOptions { sort_keys: false, indent: None }` — the no-op identity that produces byte-identical output to today's writers.

#### Scenario: Default is the no-op identity
- **WHEN** a writer is invoked via `format.write_with_options(&doc, &mut buf, &WriteOptions::default())`
- **THEN** the output bytes are identical to `format.write(&doc, &mut buf)`

#### Scenario: WriteOptions is non_exhaustive
- **WHEN** application code constructs `WriteOptions { sort_keys: true, indent: Some(4) }` positionally without `..Default::default()`
- **THEN** the compiler rejects it with the `non-exhaustive` lint and the user is forced to use struct-update syntax

### Requirement: `Format::write_with_options` trait method

The `Format` trait in `crates/dq-core/src/format.rs` SHALL gain a new method:

```rust
fn write_with_options(
    &self,
    doc: &Document,
    w: &mut dyn Write,
    opts: &WriteOptions,
) -> Result<()> {
    let _ = opts;
    self.write(doc, w)
}
```

The default implementation forwards to `write` so existing format implementations keep working unchanged. JSON, JSONL, YAML, and TOML SHALL override the default to honor the relevant `WriteOptions` fields per the table below:

| Format | `sort_keys` | `indent` |
|---|---|---|
| JSON | yes | yes |
| JSONL | yes | yes (per-line) |
| YAML | yes | no (deferred — `serde_yml` does not expose indent) |
| TOML | yes | no (grammar-fixed) |

#### Scenario: JSON honors sort_keys + indent
- **WHEN** a caller invokes `Json.write_with_options(&doc, &mut buf, &WriteOptions { sort_keys: true, indent: Some(4) })`
- **THEN** `buf` contains 4-space-indented JSON with map keys in alphabetical order

#### Scenario: TOML honors sort_keys but ignores indent
- **WHEN** a caller invokes `Toml.write_with_options(&doc, &mut buf, &WriteOptions { sort_keys: true, indent: Some(4) })`
- **THEN** `buf` contains TOML with table keys sorted alphabetically; the `indent` field has no effect

#### Scenario: YAML default behaviour preserved with WriteOptions::default()
- **WHEN** a caller invokes `Yaml.write_with_options(&doc, &mut buf, &WriteOptions::default())`
- **THEN** `buf` is byte-identical to `Yaml.write(&doc, &mut buf)`

### Requirement: `dq_core::canonicalize_keys` helper

`crates/dq-core/src/write_options.rs` SHALL define a public free function:

```rust
pub fn canonicalize_keys(value: &Value) -> Value;
```

The function SHALL return a deep clone of `value` with `Value::Map(IndexMap)` keys sorted alphabetically (case-sensitive byte order, ASCII < Unicode). Arrays SHALL be walked recursively (their elements canonicalized). Scalar variants (`Null`, `Bool`, `Int`, `BigInt`, `Float`, `BigFloat`, `String`) SHALL be returned unchanged. The function SHALL be deterministic and idempotent: `canonicalize_keys(canonicalize_keys(v)) == canonicalize_keys(v)`.

#### Scenario: Map keys sort alphabetically
- **WHEN** the caller invokes `canonicalize_keys` on `{ z: 1, a: 2, m: 3 }`
- **THEN** the returned `Value::Map` has keys in order `["a", "m", "z"]`

#### Scenario: Nested maps inside arrays are canonicalized
- **WHEN** the caller invokes `canonicalize_keys` on `[{z: 1, a: 2}, {y: 3, b: 4}]`
- **THEN** the returned array contains `[{a: 2, z: 1}, {b: 4, y: 3}]`

#### Scenario: Idempotence
- **WHEN** the caller invokes `canonicalize_keys(canonicalize_keys(v))` for any value `v`
- **THEN** the result is `Value`-equal to `canonicalize_keys(v)` and the order is identical

### Requirement: HCL read and best-effort write support

`dq-core` SHALL parse HCL 2 documents (`.hcl`, `.tf`, `.tfvars` extensions) into `Document` via `hcl-rs`. The parser maps HCL objects to `Value::Map`, arrays to `Value::Array`, and scalars to their typed variants (`Int`, `Float`, `Bool`, `String`, `Null`). Heredocs and multi-line strings are preserved as `Value::String`. Block syntax (`backend "s3" { ... }`) is mapped to `Value::Map` with a labels-as-keys convention: a single-labeled block `block_type "label" { ... }` becomes `block_type: { "label": { ... } }`; multi-labeled blocks nest one level per label.

The write path emits HCL via `hcl::to_string`. Comments, operator spacing, and trailing newlines from the source are NOT preserved (M5 v1 limitation, documented). The format is registered in `parsers::registry()` and addressable via `-F hcl`.

#### Scenario: HCL backend block parses to nested map
- **WHEN** the parser is fed `terraform { backend "s3" { region = "us-east-1" } }`
- **THEN** the resulting `Document` value contains `Map { "terraform": Map { "backend": Map { "s3": Map { "region": String("us-east-1") } } } }`

#### Scenario: HCL list of strings
- **WHEN** the parser is fed `subnets = ["a", "b", "c"]`
- **THEN** the resulting `Document` value contains `Map { "subnets": Array [String("a"), String("b"), String("c")] }`

#### Scenario: HCL detected by extension
- **WHEN** `dq get terraform_main.tf /backend` is run without `-F`
- **THEN** the HCL parser is selected (the registry returns `&Hcl` for `.tf`)

### Requirement: INI / `.properties` read and write support

`dq-core` SHALL parse INI files (`.ini`, `.properties`, `.cfg` extensions) into `Document` via `rust-ini`. The top-level shape is `Value::Map<section-name, Value::Map<key, Value::String>>`. Sections preserve source order. Keys before the first `[header]` are stored under the empty-string section name `""`. The `:` separator (used by Java `.properties`) is accepted alongside `=`.

The write path emits each section header (skipping the anonymous section header line if present) followed by its keys. Quote-style preservation is NOT a contract in M5 — `rust-ini`'s default emission is used. Comments are not preserved through round-trip (acknowledged in `dq fmt --help` output).

#### Scenario: Multi-section INI parses to nested map
- **WHEN** the parser is fed `[server]\nhost = localhost\nport = 8080\n[client]\nretries = 3`
- **THEN** the resulting `Document` value contains `Map { "server": Map { "host": String, "port": String }, "client": Map { "retries": String } }`

#### Scenario: Anonymous section
- **WHEN** the parser is fed `default-key = value\n[server]\nhost = localhost`
- **THEN** the resulting `Document` value contains `Map { "": Map { "default-key": String("value") }, "server": Map { "host": String("localhost") } }`

#### Scenario: Round-trip preserves section order
- **WHEN** an INI file with sections `[c]`, `[a]`, `[b]` (in that source order) is parsed and written back via `Format::write`
- **THEN** the output emits the sections in the same order: `[c]` first, `[a]` second, `[b]` third

### Requirement: `.env` read and write support

`dq-core` SHALL parse `.env` files into `Document` as a flat `Value::Map<String, Value::String>`. The parser MUST handle:

- `KEY=value` lines.
- `export KEY=value` lines (the `export` prefix is stripped; only the assignment is stored).
- Double-quoted values with backslash escapes (`\\`, `\"`, `\n`, `\t`).
- Single-quoted values (literal — no escape processing, no interpolation).
- Comment lines (`# ...`) and blank lines (skipped).

Variable interpolation (`${OTHER_KEY}`) is NOT performed; the raw string is stored verbatim. Values containing whitespace, `#`, `=`, `$`, `\`, `"`, or non-printable characters are quoted on emission with double quotes; otherwise emitted unquoted. Comments and the original quote style are NOT preserved through round-trip.

#### Scenario: Simple key-value
- **WHEN** the parser is fed `KEY=value\nOTHER=42\n`
- **THEN** the resulting `Document` value is `Map { "KEY": String("value"), "OTHER": String("42") }`

#### Scenario: Quoted value with whitespace
- **WHEN** the parser is fed `MESSAGE="hello world"\n`
- **THEN** the resulting `Document` value is `Map { "MESSAGE": String("hello world") }`

#### Scenario: Export prefix
- **WHEN** the parser is fed `export PATH=/usr/bin\n`
- **THEN** the resulting `Document` value is `Map { "PATH": String("/usr/bin") }` (no `export` in key or value)

#### Scenario: Re-quoting on write
- **WHEN** the value `Map { "MESSAGE": String("hello world") }` is written through `DotEnv::write`
- **THEN** the output is `MESSAGE="hello world"\n` (double-quoted because the value contains whitespace)

### Requirement: CSV and TSV read and write support

`dq-core` SHALL parse CSV (`.csv`) and TSV (`.tsv`) files into `Document` via the `csv` crate. The parser assumes a header row; each data row becomes a `Value::Map` keyed by the header columns; the document value is `Value::Array<Value::Map>`. Every cell is `Value::String` — no numeric / boolean / null type inference. TSV uses the same parser with `b'\t'` as the delimiter.

The write path requires the document's value to be `Value::Array<Value::Map>` whose maps share a common key set. Any other shape produces `Error::Format` with a message naming the offending top-level type. The header is taken from the union of keys (sorted by first occurrence in source order) and one row is emitted per array element.

#### Scenario: Header + rows produces array of maps
- **WHEN** the parser is fed `name,age\nalice,30\nbob,25\n`
- **THEN** the resulting `Document` value is `Array [Map { "name": String("alice"), "age": String("30") }, Map { "name": String("bob"), "age": String("25") }]`

#### Scenario: All cells are strings
- **WHEN** the parser is fed a CSV with `count,42` data
- **THEN** the corresponding cell is `Value::String("42")`, NOT `Value::Int(42)`

#### Scenario: Non-tabular write rejected
- **WHEN** the user runs `dq convert post.md -F csv` against a frontmatter document whose value is a top-level Map (not Array)
- **THEN** the command exits with code 6 and the structured error message names the offending top-level type

#### Scenario: TSV is identical to CSV with tab delimiter
- **WHEN** the parser is fed a `.tsv` file with tab-separated values
- **THEN** the resulting `Document` value is structurally identical to the equivalent CSV with `,` delimiters

### Requirement: Dockerfile read-only support

`dq-core` SHALL parse Dockerfiles via `dockerfile-parser-rs`. The parser is selected by:
- file extension `.dockerfile` or `.containerfile`, OR
- file basename equal to `Dockerfile` (case-sensitive) regardless of extension.

The parser walks each instruction into `Value::Map { "instruction": Value::String, "arguments": Value::String OR Value::Array<Value::String>, "line": Value::Int }`. Multi-arg instructions like `COPY src dst` may be represented either as a single `Value::String` ("src dst") or as `Value::Array<Value::String>` (`["src", "dst"]`); the spec does not require a specific choice but the test suite SHALL pin whichever choice is taken.

The document value is `Value::Array<Value::Map>` indexed by instruction order. The write path returns `Error::Format { format: "dockerfile", message: "Dockerfile is read-only in M5" }`. `convert -F dockerfile` is rejected at the clap layer (no `OutputFormat::Dockerfile` variant exists).

#### Scenario: Dockerfile filename detected without extension
- **WHEN** `dq get Dockerfile /0/instruction` is run on a file literally named `Dockerfile`
- **THEN** the Dockerfile parser is selected and the result is the first instruction's name (e.g. `"FROM"`)

#### Scenario: Write path returns format error
- **WHEN** `Dockerfile::write` is invoked on any document
- **THEN** the call returns `Err(Error::Format { format: "dockerfile", message: contains "read-only" })`

#### Scenario: convert -F dockerfile rejected by clap
- **WHEN** the user runs `dq convert deploy.yaml -F dockerfile`
- **THEN** clap exits with code 6 (`INVALID_INPUT`) and the error message names "dockerfile" as an invalid value for `-F`

### Requirement: `.gitignore` / `.dockerignore` (ignore-list) read-only support

`dq-core` SHALL parse ignore-list files into `Document` as a flat `Value::Array<Value::String>`. The parser is selected by file basename: `.gitignore`, `.dockerignore`, `.npmignore`, or `.eslintignore` (case-sensitive). One pattern per non-blank, non-`#`-prefixed line; trailing whitespace is trimmed; blank lines and comments are dropped (NOT preserved in the value tree).

The write path returns `Error::Format { format: "ignore-list", message: "ignore-list is read-only in M5" }`.

#### Scenario: Patterns parsed into flat array
- **WHEN** the parser is fed `node_modules/\n# build artefacts\n*.log\n\ntarget/\n`
- **THEN** the resulting `Document` value is `Array [String("node_modules/"), String("*.log"), String("target/")]` — comments and blank lines are dropped

#### Scenario: `.gitignore` filename detected
- **WHEN** `dq paths .gitignore` is run on a file literally named `.gitignore`
- **THEN** the ignore-list parser is selected and the output is the array of pattern pointers (`/0`, `/1`, …)

### Requirement: Markdown frontmatter read and write support

`dq-core` SHALL parse the frontmatter block of a Markdown file (`.md`, `.markdown` extensions) into `Document` and store the body of the file as opaque bytes. The parser detects three frontmatter delimiters at the start of the file:

- `---\n` … `---\n` → header is YAML; parsed via the YAML format.
- `+++\n` … `+++\n` → header is TOML; parsed via the TOML format.
- `{\n` … `}\n` followed by a blank line → header is JSON; parsed via the JSON format.

If no opening delimiter is recognised within the first byte OR no matching closing delimiter is found within the first 64 KiB of the file, the parser returns `Document::frontmatter(empty_map, whole_file_bytes, FormatTag::Frontmatter)` — the value is an empty map, the body is the entire file contents.

The body is stored alongside the parsed value (the implementation may use a new `Document::frontmatter_body` field, a wrapper `FrontmatterPayload` struct, or any equivalent representation that round-trips through `Format::write`). The write path:
1. Re-serializes the value through the inner format (YAML/TOML/JSON, depending on which delimiter was used at parse time).
2. Emits the opening delimiter, the serialized header, the closing delimiter.
3. Concatenates the stored body bytes verbatim (no re-canonicalization of the body).

#### Scenario: YAML frontmatter parses header into map and stores body
- **WHEN** the parser is fed `---\ntitle: Hello\n---\n# Body\n`
- **THEN** the resulting `Document` value is `Map { "title": String("Hello") }` and `Document::frontmatter_body()` returns `Some(b"# Body\n")`

#### Scenario: TOML frontmatter
- **WHEN** the parser is fed `+++\ntitle = "Hello"\n+++\n# Body\n`
- **THEN** the resulting `Document` value is `Map { "title": String("Hello") }` and the body is `# Body\n`

#### Scenario: No frontmatter falls back to empty map + whole file body
- **WHEN** the parser is fed `# Just a markdown\n\nNo frontmatter here.\n`
- **THEN** the resulting `Document` value is an empty Map and the body equals the entire input bytes

#### Scenario: Round-trip preserves body byte-identical
- **WHEN** a Markdown file with YAML frontmatter and a 3-paragraph body is parsed and written back via `Format::write`
- **THEN** the body portion of the output (everything after the closing `---\n`) is byte-identical to the body portion of the input

### Requirement: New `FormatTag` variants

`Document::FormatTag` SHALL gain the M5 variants `Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Dockerfile`, `IgnoreList`, `Frontmatter` and the M11 variant `Xml`. `FormatTag::from_name` SHALL recognise the corresponding lowercase names (`"hcl"`, `"ini"`, `"dotenv"`, `"csv"`, `"tsv"`, `"dockerfile"`, `"ignore-list"`, `"frontmatter"`, `"xml"`).

#### Scenario: from_name maps the new tags
- **WHEN** the caller invokes `FormatTag::from_name("frontmatter")`
- **THEN** the result is `Some(FormatTag::Frontmatter)`

#### Scenario: from_name maps `xml`
- **WHEN** the caller invokes `FormatTag::from_name("xml")`
- **THEN** the result is `Some(FormatTag::Xml)`

### Requirement: Filename-based detection for extensionless or dot-prefix formats

`dq_core::format::detect` SHALL fall back from the standard extension lookup to a filename-based lookup for formats whose canonical filename has no traditional extension: a Dockerfile literally named `Dockerfile` (or `Containerfile`); a dotfile literally named `.gitignore`, `.dockerignore`, `.npmignore`, `.eslintignore`, or `.env`. The fallback is deterministic and case-sensitive on the filename.

#### Scenario: `Dockerfile` (no extension) detected
- **WHEN** `format::detect(Utf8Path::new("project/Dockerfile"))` is called
- **THEN** the result is `Some(&Dockerfile)`

#### Scenario: `.gitignore` detected
- **WHEN** `format::detect(Utf8Path::new(".gitignore"))` is called
- **THEN** the result is `Some(&IgnoreList)`

#### Scenario: `.env` detected
- **WHEN** `format::detect(Utf8Path::new(".env"))` is called
- **THEN** the result is `Some(&DotEnv)`

### Requirement: New `OutputFormat` write-target variants

`crates/dq-cli/src/output/mod.rs::OutputFormat` SHALL gain the M5 variants for `convert -F` write targets (`Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Frontmatter`) plus the M11 `Xml` variant. `OutputFormat::Dockerfile` and `OutputFormat::IgnoreList` SHALL NOT exist — clap rejects `-F dockerfile` / `-F ignore-list` at the parse step (exit 6). `dq convert <input> -F xml` SHALL be accepted and route through `XmlFormat::write_with_options`.

#### Scenario: convert -F hcl is accepted
- **WHEN** the user runs `dq convert app.json -F hcl`
- **THEN** clap parses the value successfully and the convert handler dispatches to the HCL writer

#### Scenario: `convert -F xml` is accepted
- **WHEN** the user runs `dq convert app.json -F xml`
- **THEN** the command exits 0 and stdout contains a well-formed XML document built from the JSON via the conventional-key mapping (see XML support requirement)

#### Scenario: convert -F dockerfile is rejected
- **WHEN** the user runs `dq convert app.json -F dockerfile`
- **THEN** clap exits with code 6 and the error message names "dockerfile" as an invalid value for `-F`

### Requirement: XML read and write support via `quick-xml`

`dq-core` SHALL parse and write XML 1.0 documents through a new `XmlFormat` implementation that depends on `quick-xml = "0.36"` (with the `serialize` feature). XML documents map onto the existing `Document::Value` enum using **conventional keys** rather than introducing a new `Value` variant:

| XML construct                         | `Value` mapping                                                  |
|---------------------------------------|------------------------------------------------------------------|
| `<tag>` element                        | `Map { tag => Array<Map { ... }> }` on the parent                |
| Attributes                             | `Map { "@attrs" => Map { name => string, ... } }` on the element |
| Text content                           | `String` under key `"#text"` on the element                      |
| `<!-- comment -->`                     | `Array<String>` under key `"#comments"` on the parent element    |
| `<![CDATA[...]]>` block                 | `Array<String>` under key `"#cdata"` on the element              |
| `<?xml-stylesheet ...?>` PI            | `Array<String>` under key `"#pi"` on the parent element          |
| `<?xml version="1.0" encoding="..."?>` | `Map { "version", "encoding", "standalone" }` under top-level key `"#xml"` |
| Namespace prefix on tag (`foo:tag`)    | retained in the tag name string verbatim (`"foo:tag"`)           |
| `xmlns:foo` attribute                  | retained in `@attrs` verbatim                                    |

Multi-element children with the same tag are stored as a single `Array` to preserve order; even single occurrences are wrapped in a one-element array so `Pointer` indexing is stable across `<a><b/></a>` and `<a><b/><b/></a>`.

The `XmlFormat::write` round-trip is **partial**: element structure, attributes, comments, CDATA, processing instructions, namespace prefixes, and the XML declaration are preserved, but **mixed content** (text interleaved with child elements within the same parent — e.g. `<p>Hello <b>world</b>!</p>`) is **opaque**: the entire body is folded into the `"#text"` value and inner element positions are not tracked. Whitespace-only pretty-printing between elements is not preserved on round-trip; the writer emits a normalised compact-with-newlines layout. Both behaviours are documented as known limitations; mixed-content XML emits a `tracing::warn!` on parse so users are aware their file is partially round-trippable.

#### Scenario: Format trait registration
- **WHEN** a new struct `XmlFormat` is registered in `dq-core::format` and `format::detect(Utf8Path::new("pom.xml"))` is called
- **THEN** the result is `Some(&XmlFormat)`

#### Scenario: Element with attribute and text round-trips
- **GIVEN** an XML document `<user id="42"><name>Alice</name></user>`
- **WHEN** `XmlFormat::parse` is called and then `XmlFormat::write` is called on the result
- **THEN** the resulting bytes are functionally equivalent (semantically identical XML; whitespace between elements may differ)

#### Scenario: Multi-child same-tag preserves order
- **GIVEN** an XML document `<list><item>A</item><item>B</item><item>C</item></list>`
- **WHEN** `XmlFormat::parse` is called
- **THEN** the resulting `Value` has `/list/item` as an `Array` of three elements `["A", "B", "C"]` in that order, addressable as `/list/item/0`, `/list/item/1`, `/list/item/2`

#### Scenario: Comments preserved on round-trip
- **GIVEN** an XML document with a `<!-- top note -->` comment inside `<root>`
- **WHEN** `XmlFormat::parse` then `XmlFormat::write` runs
- **THEN** the output XML contains the same comment text in the same position relative to its sibling elements

#### Scenario: CDATA preserved on round-trip
- **GIVEN** an XML document with `<script><![CDATA[if (a < b) {}]]></script>`
- **WHEN** `XmlFormat::parse` then `XmlFormat::write` runs
- **THEN** the output XML contains the CDATA block with byte-identical content

#### Scenario: Mixed content emits warning
- **GIVEN** an XML document with `<p>Hello <b>world</b>!</p>`
- **WHEN** `XmlFormat::parse` is called
- **THEN** a `tracing::warn!` log is emitted noting that mixed content was encountered AND parsing succeeds with the body folded into `"#text"`

#### Scenario: XML declaration preserved
- **GIVEN** an XML document beginning with `<?xml version="1.0" encoding="UTF-8"?>`
- **WHEN** `XmlFormat::parse` then `XmlFormat::write` runs
- **THEN** the output XML begins with an equivalent declaration

#### Scenario: Auto-detection by extension
- **WHEN** the user runs `dq get pom.xml /project/version`
- **THEN** XML format is detected from the `.xml` extension and the value is returned

#### Scenario: `-F xml` override accepted on read
- **WHEN** the user runs `dq get config.txt -F xml /root/setting`
- **THEN** the file is parsed as XML regardless of the `.txt` extension

