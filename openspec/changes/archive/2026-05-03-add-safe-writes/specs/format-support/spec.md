## ADDED Requirements

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

The `dq-core` crate SHALL add `saphyr-parser` (low-level event API) for write-path span discovery, alongside the existing `serde_yml` which continues to back the read path through M2 (D13). It SHALL replace `toml` with `toml_edit`. The `tempfile` crate SHALL move from `[dev-dependencies]` to `[dependencies]`. The `similar` crate SHALL be added as a `[dependencies]` of `dq-cli` for unified-diff rendering. The advisory ignore for `serde_yml` in `deny.toml` SHALL remain in place through M2 and is removed in a post-M2 refactor change when (and if) read-path is unified onto `saphyr-parser`.

#### Scenario: `saphyr-parser` is in the dep tree
- **WHEN** the developer runs `cargo tree --workspace` after the M2 implementation
- **THEN** `saphyr-parser` appears in the output and `toml` (the standard crate) does not; `serde_yml` and `toml_edit` are both present

#### Scenario: deny.toml advisory ignores are accurate
- **WHEN** `cargo deny check` runs after M2
- **THEN** there are no warnings about unused advisory ignores; the existing `serde_yml` ignore is still actively suppressing its advisory because the crate is still in the dep tree

## MODIFIED Requirements

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

### Requirement: TOML read support

`dq-core` SHALL parse TOML 1.0 documents into `Document` via `toml_edit`, preserving table and key order AND retaining per-node metadata for round-trip. Read-only commands SHALL behave identically to the M1 baseline.

#### Scenario: Nested tables
- **WHEN** a TOML file with `[server]` and `[server.tls]` sections is parsed
- **THEN** the resulting `Document` represents `server.tls` as a nested object under `server`

#### Scenario: M1 TOML test fixtures still pass
- **WHEN** the existing `crates/dq-core/tests/parse_toml.rs` test suite is executed against the `toml_edit`-based parser
- **THEN** all five tests pass without modification of the test code

### Requirement: Number representation preservation

When reading numeric values, `dq-core` SHALL preserve the original textual representation for any integer that does not fit in `i64`/`u64` and for any float whose serialization would lose precision; these are stored in dedicated `Value::BigInt(String)` / `Value::BigFloat(String)` variants of the `Value` enum carried by a `Document`. Standard-precision values use `Value::Int(i64)` and `Value::Float(f64)`. The textual representation MUST round-trip byte-for-byte through `set` mutation as well as `convert`.

#### Scenario: Round-trip large integer through convert
- **WHEN** a JSON file contains `{"id": 4722366482869645213696}` and the user runs `dq convert big.json -F json`
- **THEN** stdout contains the same integer literal, character-for-character

#### Scenario: Round-trip large integer through set
- **WHEN** the user runs `dq set big.json /id 4722366482869645213696 -i` followed by `dq get big.json /id`
- **THEN** the get output is exactly `4722366482869645213696`
