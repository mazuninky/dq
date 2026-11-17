Делегирование: `[orch]` — оркестратор пишет markdown / меняет config / прогоняет smoke; `[writer]` / `[test-writer]` — Rust-правки идут через subagents `rust-cli-writer` / `rust-cli-test-writer` (правило в `.claude/rules/rust-delegation.md`). Каждая задача self-contained, ≤ 2 часов реальной работы. Зависимости явно прописаны: §1 готовит фундамент (Document::frontmatter_body + FormatTag variants + workspace deps); §2–§7 — параллельные format implementations, каждая зависит только от §1; §8 объединяет всё в registry + CLI; §9 — тесты по фактам §2–§8.

## 1. Foundation: Document, FormatTag, workspace deps

- [x] 1.1 [writer] Workspace `Cargo.toml`: add `[workspace.dependencies]` entries:
  ```toml
  hcl-rs = "0.18"
  rust-ini = "0.21"
  dotenvy = "0.15"
  csv = "1.3"
  dockerfile-parser-rs = "0.9"
  ```
  Verify each crate's license is MIT or Apache-2.0 (use `cargo metadata --format-version 1` or `cargo deny check licenses` after this step).

- [x] 1.2 [writer] `crates/dq-core/Cargo.toml`: add the five new deps under `[dependencies]` as `workspace = true`. No feature flags needed for default configurations.

- [x] 1.3 [writer] `crates/dq-core/src/document/mod.rs`: extend `FormatTag` enum:
  ```rust
  pub enum FormatTag {
      Yaml, Json, Toml, Jsonl,
      Hcl, Ini, DotEnv, Csv, Tsv, Dockerfile, IgnoreList, Frontmatter,
  }
  ```
  Update `from_name` to recognise: `"hcl"`, `"ini"`, `"dotenv"`, `"csv"`, `"tsv"`, `"dockerfile"`, `"ignore-list"`, `"frontmatter"`.

- [x] 1.4 [writer] `crates/dq-core/src/document/mod.rs`: add `frontmatter_body: Option<Vec<u8>>` field to `Document`. Initialize to `None` in every constructor (`value_only`, `multi_value_only`, `with_spans`, etc.). Add new constructor:
  ```rust
  pub fn frontmatter(value: Value, body: Vec<u8>, format: FormatTag) -> Self {
      Self {
          value, original_bytes: Vec::new(), spans: SpanMap::new(),
          format, multi_doc: false,
          frontmatter_body: Some(body),
      }
  }
  pub fn frontmatter_body(&self) -> Option<&[u8]> { self.frontmatter_body.as_deref() }
  ```
  Existing `Document::clone`, `PartialEq`, etc. derive the new field automatically.

- [x] 1.5 [test-writer] `crates/dq-core/src/document/mod.rs` `#[cfg(test)] mod tests`: add ≥3 unit tests:
  - `Document::frontmatter(value, body, FormatTag::Frontmatter)` exposes `frontmatter_body() == Some(&body)`.
  - `Document::value_only(...)` has `frontmatter_body() == None`.
  - `FormatTag::from_name("hcl") == Some(FormatTag::Hcl)` and similar for the other six new tags.

## 2. HCL parser

- [x] 2.1 [writer] Create `crates/dq-core/src/parsers/hcl.rs`:
  ```rust
  pub struct Hcl;

  impl Format for Hcl {
      fn name(&self) -> &'static str { "hcl" }
      fn extensions(&self) -> &'static [&'static str] { &["hcl", "tf", "tfvars"] }
      fn parse(&self, bytes: &[u8]) -> Result<Document> { /* hcl::from_slice → Value */ }
      fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> { /* hcl::to_writer */ }
  }
  ```
  Parse approach: `hcl::from_slice::<hcl::Body>(bytes)` then walk into `Value` (objects → `Map`, arrays → `Array`, scalars typed). Errors: `Error::Parse { message: format!("hcl: {e}"), ... }` with line/col extracted from `hcl::Error` if available, otherwise `0/0`.
  
  Write approach: convert `Value` back to `hcl::Body`, then `hcl::to_string`. For values that don't round-trip (e.g. arbitrary mixed-type arrays), document the limitation and produce best-effort output. Comments and original spacing are NOT preserved (documented in the M5 design D1).

- [x] 2.2 [test-writer] Create `crates/dq-core/tests/parse_hcl.rs`: ≥6 tests:
  - parse simple top-level HCL with `region = "us-east-1"` → Map with String value.
  - parse nested block: `backend "s3" { bucket = "x" }` → nested Map.
  - parse list: `subnets = ["a", "b"]` → Array of Strings.
  - parse number: `replicas = 3` → Int.
  - registry detects `.hcl`/`.tf`/`.tfvars` extensions.
  - parse error on invalid syntax produces `Error::Parse` with non-empty message.

## 3. INI parser

- [x] 3.1 [writer] Create `crates/dq-core/src/parsers/ini.rs`:
  ```rust
  pub struct Ini;

  impl Format for Ini {
      fn name(&self) -> &'static str { "ini" }
      fn extensions(&self) -> &'static [&'static str] { &["ini", "properties", "cfg"] }
      fn parse(&self, bytes: &[u8]) -> Result<Document> {
          // ini::Ini::load_from_str(text) → walk into Map<section, Map<key, Value::String>>
          // The implicit anonymous section (keys before any [header]) is keyed under "".
      }
      fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
          // Walk Map<section, Map<key, String>> back into rust_ini and write_to.
      }
  }
  ```
  Use `rust_ini::Ini` with default options (case-sensitive, ordered). Non-string scalars in the value tree are coerced to their `to_string` form on write; non-Map shapes error with `Error::Format`.

- [x] 3.2 [test-writer] Create `crates/dq-core/tests/parse_ini.rs`: ≥6 tests:
  - simple `[section]\nkey = value` → nested Map.
  - anonymous section: `key = value\n[s]\nx = 1` → Map { "" → { "key": ... }, "s": { "x": ... } }.
  - multiple sections preserve source order.
  - colon-separator (`.properties`-style): `key: value` parses identically.
  - round-trip: parse + write produces semantically equivalent output (key/value preserved; section order preserved; whitespace may differ).
  - parse error on malformed input.

## 4. .env parser

- [x] 4.1 [writer] Create `crates/dq-core/src/parsers/dotenv.rs`:
  ```rust
  pub struct DotEnv;

  impl Format for DotEnv {
      fn name(&self) -> &'static str { "dotenv" }
      fn extensions(&self) -> &'static [&'static str] { &["env"] }
      fn parse(&self, bytes: &[u8]) -> Result<Document> {
          // Use dotenvy's iterator API: dotenvy::EnvLoader or read line-by-line
          // and call dotenvy::from_str helpers for value-unquoting. Build
          // Value::Map<String, Value::String>.
      }
      fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
          // Walk Map<String, Value::String>; emit one KEY=VALUE per line.
          // Quote when value contains whitespace, #, =, $, \, ", or non-printable.
          // Inside quotes, escape \ and " (and \n if present).
      }
  }
  ```
  Note: `dotenvy` 0.15's API is loader-focused; if no public iterator, build a tiny line scanner (split on `=`, trim leading `export `, handle `# comment` and blank lines, defer value unquoting to a small helper). Write-emitter is bespoke either way.

- [x] 4.2 [test-writer] Create `crates/dq-core/tests/parse_dotenv.rs`: ≥6 tests:
  - simple `KEY=value` → Map { KEY: "value" }.
  - quoted: `KEY="hello world"` → Map { KEY: "hello world" }.
  - escaped: `KEY="line1\nline2"` → Map { KEY: "line1\nline2" }.
  - export prefix: `export KEY=value` parses identically (no `export` in the value).
  - comments and blanks skipped.
  - write quoting: value `"a b"` re-emits as `KEY="a b"` (with double quotes).

## 5. CSV/TSV parser

- [x] 5.1 [writer] Create `crates/dq-core/src/parsers/csv.rs`:
  ```rust
  pub struct Csv;
  pub struct Tsv;

  impl Format for Csv { /* ... delimiter = b',' ... */ }
  impl Format for Tsv { /* ... delimiter = b'\t' ... */ }
  ```
  Each `parse` constructs `csv::ReaderBuilder::new().has_headers(true).delimiter(d).from_reader(bytes)` and walks rows into `Value::Array(Value::Map { col: String, ... })`. Each cell is `Value::String(...)` (no type inference per design D3).
  
  Each `write` requires the document's value to be `Value::Array<Value::Map>` whose maps share a common key set (validated explicitly with a clear error). The write path uses `csv::WriterBuilder::new().delimiter(d).from_writer(w)` and emits `header` row + one row per element.

- [x] 5.2 [test-writer] Create `crates/dq-core/tests/parse_csv.rs`: ≥6 tests:
  - parse: `name,age\nalice,30\nbob,25\n` → Array of two Maps.
  - parse: TSV delimiter works.
  - write: Array<Map> with consistent keys → CSV with header.
  - write: non-Array top-level errors with `Error::Format`.
  - write: Array<Map> with inconsistent keys errors with clear message.
  - all values are String (no `Int(30)` from `"30"`).

## 6. Dockerfile parser (read-only)

- [x] 6.1 [writer] Create `crates/dq-core/src/parsers/dockerfile.rs`:
  ```rust
  pub struct Dockerfile;

  impl Format for Dockerfile {
      fn name(&self) -> &'static str { "dockerfile" }
      fn extensions(&self) -> &'static [&'static str] { &["dockerfile", "containerfile"] }
      fn parse(&self, bytes: &[u8]) -> Result<Document> {
          // dockerfile_parser::Dockerfile::parse(text) → walk instructions
          // into Array<Map { instruction: String, arguments: <String|Array>, line: Int }>.
      }
      fn write(&self, _doc: &Document, _w: &mut dyn Write) -> Result<()> {
          Err(Error::Format {
              format: "dockerfile",
              message: "Dockerfile is read-only in M5".to_owned(),
          })
      }
  }
  ```
  Map every instruction: `FROM` → `{ instruction: "FROM", arguments: "image:tag", line: N }`, `RUN` → `{ instruction: "RUN", arguments: "shell command", line: N }`, etc. For multi-arg instructions like `COPY src dst`, emit `arguments` as an array of strings.
  
  Note on extension detection: Dockerfiles often have NO extension (literal filename `Dockerfile`). Add a special case in `format::detect`: if the file path's filename (case-insensitive) is `Dockerfile`, `dockerfile`, or has the `.dockerfile`/`.containerfile` extension, return `&Dockerfile`. Implement this as a fallback after the extension lookup.

- [x] 6.2 [test-writer] Create `crates/dq-core/tests/parse_dockerfile.rs`: ≥5 tests:
  - parse simple `FROM alpine:latest\nRUN apk add curl` → Array of 2 Maps.
  - parse `COPY src dst` → arguments is Array of 2 Strings (or single string, document the choice).
  - registry detects `Dockerfile` literal filename (no extension).
  - registry detects `.dockerfile` and `.containerfile` extensions.
  - write returns `Error::Format` with the read-only message.

## 7. Ignore-list parser (read-only)

- [x] 7.1 [writer] Create `crates/dq-core/src/parsers/ignore_list.rs`:
  ```rust
  pub struct IgnoreList;

  impl Format for IgnoreList {
      fn name(&self) -> &'static str { "ignore-list" }
      fn extensions(&self) -> &'static [&'static str] { &["gitignore", "dockerignore"] }
      fn parse(&self, bytes: &[u8]) -> Result<Document> {
          // Read line-by-line; skip lines that are blank or whose first
          // non-whitespace char is '#'. Trim trailing whitespace from each
          // remaining line. Return Document::value_only(Array<String>).
      }
      fn write(&self, _doc: &Document, _w: &mut dyn Write) -> Result<()> {
          Err(Error::Format {
              format: "ignore-list",
              message: "ignore-list is read-only in M5".to_owned(),
          })
      }
  }
  ```
  Filename detection: `.gitignore`, `.dockerignore`, `.npmignore`, `.eslintignore` (case-sensitive on extension match — they all have a leading dot but no traditional extension). Implement matching as a special case alongside the Dockerfile filename trick.

- [x] 7.2 [test-writer] Create `crates/dq-core/tests/parse_ignore_list.rs`: ≥4 tests:
  - parse `node_modules/\n# comment\n*.log\n` → Array of 2 Strings (`node_modules/`, `*.log`).
  - parse blank lines and trailing whitespace → cleaned.
  - registry detects `.gitignore` / `.dockerignore` filenames.
  - write returns `Error::Format` with the read-only message.

## 8. Frontmatter parser (delegates to YAML/TOML/JSON)

- [x] 8.1 [writer] Create `crates/dq-core/src/parsers/frontmatter.rs`:
  ```rust
  pub struct Frontmatter;

  impl Format for Frontmatter {
      fn name(&self) -> &'static str { "frontmatter" }
      fn extensions(&self) -> &'static [&'static str] { &["md", "markdown"] }
      fn parse(&self, bytes: &[u8]) -> Result<Document> {
          // 1. Sniff the first bytes for a delimiter: "---\n" → YAML, "+++\n" → TOML.
          //    For a JSON header, accept "{\n" only when followed by a balanced
          //    object closing within the first ~64 KiB.
          // 2. Find the closing delimiter. If none within the first 64 KiB, fall
          //    back to "no frontmatter" — value is empty Map, body = whole file.
          // 3. Parse the header through the inner Format::parse.
          // 4. Document::frontmatter(parsed_value, body_bytes, FormatTag::Frontmatter).
      }
      fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
          // Re-serialize the value through the inner format (detect from the
          // document's stored sub-format — see below), emit the surrounding
          // delimiters, then write the body verbatim.
      }
  }
  ```
  The wrapper needs to remember which inner format produced the header. Store this as a small `FrontmatterKind` enum (`Yaml`, `Toml`, `Json`) inside `Document::frontmatter_body` — actually no, dedicate a separate `frontmatter_kind: Option<FrontmatterKind>` field on `Document`, or piggy-back inside an existing structure. Cleanest: add a private enum `FrontmatterKind` inside `parsers::frontmatter` and store it as the first byte of `frontmatter_body` (e.g. body bytes are prefixed with a kind tag the parser strips on read and re-prepends on write — UGLY). Cleaner again: extend the `Document` struct's existing `frontmatter_body: Option<Vec<u8>>` to a dedicated wrapper struct `FrontmatterPayload { kind: FrontmatterKind, body: Vec<u8> }`. Decide at implementation time; use whichever is easier to test.
  
  RECOMMENDED: pivot §1.4 to `frontmatter_payload: Option<FrontmatterPayload>` from the start. Coordinate with §1.4 if not already done.

- [x] 8.2 [test-writer] Create `crates/dq-core/tests/parse_frontmatter.rs`: ≥6 tests:
  - YAML frontmatter: `---\ntitle: Hello\n---\n# Body\n` → value is Map { title: "Hello" }, body is `# Body\n`.
  - TOML frontmatter: `+++\ntitle = "Hello"\n+++\n# Body\n` → value Map { title: "Hello" }.
  - JSON frontmatter: `{\n  "title": "Hello"\n}\n\n# Body\n` → Map { title: "Hello" }.
  - No frontmatter: `# Just markdown\n` → empty Map, body equals whole file.
  - Round-trip without mutation: parse + write produces output with header re-canonicalized AND body byte-identical.
  - `dq fmt post.md -i` with YAML frontmatter re-canonicalizes the header but keeps body verbatim (this test goes in §10 cli_smoke; mention here so the design constraint is testable).

## 9. Wiring: registry, OutputFormat, format::detect filename special-case

- [x] 9.1 [writer] `crates/dq-core/src/parsers/mod.rs`:
  ```rust
  pub mod hcl;
  pub mod ini;
  pub mod dotenv;
  pub mod csv;
  pub mod dockerfile;
  pub mod ignore_list;
  pub mod frontmatter;

  pub use hcl::Hcl;
  pub use ini::Ini;
  pub use dotenv::DotEnv;
  pub use csv::{Csv, Tsv};
  pub use dockerfile::Dockerfile;
  pub use ignore_list::IgnoreList;
  pub use frontmatter::Frontmatter;
  ```
  Extend `registry()`:
  ```rust
  static FORMATS: &[&dyn Format] = &[
      &Json, &Yaml, &Toml, &Jsonl,
      &Hcl, &Ini, &DotEnv, &Csv, &Tsv,
      &Dockerfile, &IgnoreList, &Frontmatter,
  ];
  ```

- [x] 9.2 [writer] `crates/dq-core/src/format.rs::detect`: extend the lookup so it handles formats whose extension is the whole filename (Dockerfile, .gitignore, .dockerignore, .env). Algorithm:
  1. Try the existing extension match first.
  2. If no match, check the file's *file name* (no leading dot stripping): if it equals (case-insensitive) `Dockerfile`, return `&Dockerfile`. If it equals `.gitignore` / `.dockerignore` / `.npmignore` / `.eslintignore`, return `&IgnoreList`. If it equals `.env`, return `&DotEnv`.
  3. Otherwise return `None`.
  
  Add `#[cfg(test)] mod tests` cases for each of the four filename lookups.

- [x] 9.3 [writer] `crates/dq-cli/src/output/mod.rs`: extend `OutputFormat` enum:
  ```rust
  pub enum OutputFormat {
      Console, Json, Jsonl, Yaml, Toml, Toon,
      Hcl, Ini, DotEnv, Csv, Tsv, Frontmatter,
  }
  ```
  Do NOT add `Dockerfile` or `IgnoreList` — they are read-only and clap will reject them at the `-F` parse step (per design D9). Update `OutputFormat::name` (or whatever the existing string-mapping is) to round-trip the new variants.

- [x] 9.4 [writer] `crates/dq-cli/src/commands/convert.rs`: when `-F` resolves to one of the new write-target formats, dispatch to the corresponding `Format::write_with_options` (already wired through M4's `write_with_options`). Add a guard: if the user's input file is `Dockerfile` or an ignore-list, `convert` works on read (the registry returns the right parser) but errors on write — verify the existing error path produces a clear message ("dockerfile is read-only in M5; cannot use as write target") and maps to `INVALID_INPUT` (exit 6) via the existing `exit_code_for_error`.

- [x] 9.5 [writer] `crates/dq-core/src/lib.rs`: re-export new format types if any cross-crate consumer (the CLI's reporter module, for instance) needs them. Otherwise keep them module-private. Re-export `FormatTag` already exists; the new variants are reachable through the type.

## 10. CLI integration tests + golden fixtures

- [x] 10.1 [test-writer] Create `crates/dq-cli/tests/fixtures/`: add 8 fixture files:
  - `terraform_main.tf` (HCL with backend + variable + resource).
  - `app.ini` (multi-section INI with anonymous section).
  - `service.env` (KEY=VALUE + comment + quoted value + export prefix).
  - `users.csv` (3 columns × 3 rows).
  - `ops.tsv` (2 columns × 2 rows).
  - `Dockerfile` (literal filename, FROM + RUN + COPY + EXPOSE).
  - `repo.gitignore` (5 patterns + 2 comments + 1 blank line).
  - `hugo_post.md` (YAML frontmatter + 2-paragraph body).
  - `mkdocs_post.md` (TOML frontmatter + 1-paragraph body).
  Reference each in `tests/fixtures/SOURCES.md` with origin (synthetic / public-domain example).

- [x] 10.2 [test-writer] Create `crates/dq-cli/tests/unit_format_extensions.rs`: ≥10 handler-level tests via `dq::run`:
  - `dq get terraform_main.tf /backend/0/region` → "us-east-1".
  - `dq paths app.ini` → array of section/key pointers.
  - `dq get service.env /DATABASE_URL` → expected URL.
  - `dq paths users.csv` → array of cell pointers `/0/name`, `/0/email`, ...
  - `dq validate Dockerfile` → exit 0 on valid, exit 4 on truncated/malformed.
  - `dq paths repo.gitignore` → flat array of patterns.
  - `dq get hugo_post.md /title` → "Hello, world".
  - `dq convert hugo_post.md -F json` produces `{"title": "Hello, world"}` with body dropped (convert is value-projection only, documented).
  - `dq convert app.ini -F json` → JSON object whose top-level keys are section names.
  - `dq convert deploy.yaml -F dockerfile` → exit 6 (clap rejects the value at parse time).

- [x] 10.3 [test-writer] Extend `crates/dq-cli/tests/cli_smoke.rs`: 4 smoke scenarios:
  - HCL convert: `dq convert main.tf -F json` produces well-formed JSON.
  - Frontmatter round-trip: `dq fmt hugo_post.md -i` keeps body byte-identical.
  - Read-only error: `dq set Dockerfile /0/instruction RUN -i` exits 7 (`WRITE_FAILED` via `Error::WriteUnavailable`) with a message naming "dockerfile".
  - INI fmt: `dq fmt app.ini -i --sort-keys` sorts keys within each section.

- [x] 10.4 [test-writer] Extend `crates/dq-cli/tests/golden.rs` if a golden runner exists (or `crates/dq-core/tests/...`): add a "format coverage" group asserting that for each of the seven new formats, `parse → write → parse` produces a structurally-equal `Value` (read-only formats skip the write phase and just round-trip parse).

## 11. Plan delta + meta + verification

- [x] 11.1 [orch] Update `dq-plan.md` M5 section with `✅ Implemented YYYY-MM-DD (см. [openspec/changes/archive/<date>-add-format-extensions/](...))` marker. Add cross-link. Add a note in the "Поддерживаемые форматы" table putting `✓` markers for HCL/INI/.env/CSV/TSV/Dockerfile/.gitignore/.dockerignore/Markdown frontmatter rows.

- [x] 11.2 [orch] Update `README.md` status line: `M5 alpha — adds HCL, INI, .env, CSV/TSV, Dockerfile, ignore-list, Markdown frontmatter`. Add an "Examples" subsection demonstrating one query per new format.

- [x] 11.3 [orch] `cargo build --workspace --all-targets` зелёный.

- [x] 11.4 [orch] `cargo test --workspace --all-features` — все existing M1–M4 тесты + new M5 тесты зелёные. Runtime cold ≤ 30s.

- [x] 11.5 [orch] `cargo clippy --workspace --all-targets --all-features -- -D warnings` зелёный.

- [x] 11.6 [orch] `cargo fmt --all -- --check` зелёный.

- [x] 11.7 [orch] `cargo deny check` зелёный (license + advisory check on the five new deps).

- [x] 11.8 [orch] Manual smoke по DoD M5:
  - `dq get config.hcl /backend/0/region` returns expected value.
  - `dq paths .env` returns array of pointers.
  - `dq validate Dockerfile` exits 0 on a valid file.
  - `dq get post.md /title` returns the frontmatter title.
  - `dq convert app.csv -F json` produces array-of-objects JSON.
  - `dq fmt config.ini -i` round-trips.

- [x] 11.9 [orch] `openspec validate add-format-extensions --strict` — `Change is valid`.

- [x] 11.10 [orch] `openspec archive add-format-extensions` — после merge в main (rename folder to `archive/<date>-add-format-extensions/`).
