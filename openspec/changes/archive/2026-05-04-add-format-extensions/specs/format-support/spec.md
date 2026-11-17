# format-support Specification (delta)

## ADDED Requirements

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

If no opening delimiter is recognised at the start of the file (i.e. the first 4 bytes are not `---\n`, `+++\n`, or the JSON `{\n` prefix) OR no matching closing delimiter is found within the first 64 KiB of the file, the parser returns `Document::frontmatter(empty_map, whole_file_bytes, FormatTag::Frontmatter)` — the value is an empty map, the body is the entire file contents.

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

`Document::FormatTag` SHALL gain eight new variants: `Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Dockerfile`, `IgnoreList`, `Frontmatter`. `FormatTag::from_name` SHALL recognise the corresponding lowercase names (`"hcl"`, `"ini"`, `"dotenv"`, `"csv"`, `"tsv"`, `"dockerfile"`, `"ignore-list"`, `"frontmatter"`).

#### Scenario: from_name maps the new tags
- **WHEN** the caller invokes `FormatTag::from_name("frontmatter")`
- **THEN** the result is `Some(FormatTag::Frontmatter)`

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

`crates/dq-cli/src/output/mod.rs::OutputFormat` SHALL gain six new variants for `convert -F` write targets: `Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Frontmatter`. `OutputFormat::Dockerfile` and `OutputFormat::IgnoreList` SHALL NOT exist — clap rejects `-F dockerfile` / `-F ignore-list` at the parse step (exit 6).

#### Scenario: convert -F hcl is accepted
- **WHEN** the user runs `dq convert app.json -F hcl`
- **THEN** clap parses the value successfully and the convert handler dispatches to the HCL writer

#### Scenario: convert -F dockerfile is rejected
- **WHEN** the user runs `dq convert app.json -F dockerfile`
- **THEN** clap exits with code 6 and the error message names "dockerfile" as an invalid value for `-F`

## MODIFIED Requirements

### Requirement: M1 anti-scope for formats

The crate SHALL NOT include parsers for **the M9 markdown body parser** (full markdown AST), **XML write**, or the conftest-only formats (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL) in M5; those are covered by M9 and M11 capabilities or remain anti-scope.

The earlier wording "no parsers for HCL, INI, .env, CSV/TSV, Dockerfile, .gitignore, XML, Markdown, or Markdown frontmatter in M1" is updated: HCL, INI, .env, CSV/TSV, Dockerfile, .gitignore/.dockerignore, and Markdown frontmatter are added by this M5 change. XML and the markdown body parser remain deferred.

#### Scenario: Unsupported format error
- **WHEN** the user runs `dq get script.sh /x` (no registered format for `.sh`)
- **THEN** the command writes a structured error suggesting `-F <fmt>` and exits with code 1
