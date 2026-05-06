## Context

M4 archived `dq fmt` and `WriteOptions`. The remaining item before linters (M8) is **format breadth**: the standard rule library covers Kubernetes manifests in YAML, but for `@std/dockerfile`, `@std/npm` (which sometimes lives in `.npmrc` INI files), `@std/github-actions` (which has frontmatter-style metadata in workflow templates), and `@std/static-sites`, the parser registry has to recognise the formats first.

The technical risk for M5 is small because every new format goes through the M1 read-pat shape:

- **No new core types.** Each parser produces a `Document::value_only(value, format_tag)` — the same shape `Json` / `Yaml` / `Toml` / `Jsonl` produce for read commands. No span maps, no textual-edit support, no round-trip-with-comments contract.
- **No new CLI verbs.** Every existing handler dispatches by `Format::name()`; adding a format means adding an entry in the registry, not new handlers.
- **No bulk driver changes.** `bulk::run_per_file` does not know about formats — it iterates files and hands them to `Format::parse` / `Format::write_with_options`.
- **No `Cargo.toml` dependency for `dq-cli`.** Every new format crate is a `dq-core` dependency only.

The interesting work is at the boundary — getting each external crate's value model into our `Value` enum without leaking abstractions, and deciding what "round-trip" means for a format the user does not expect to round-trip (CSV, `.env`).

**Current state:** M4 archived (`2026-05-04-add-style-and-normalization`). Active changes: `add-format-extensions` (this document).

**Constraints:**

- Conventions from `/rust-cli` skill are unchanged: thin `main.rs`, Reporter with DI, exit codes as named constants, no `println!` outside `main.rs` / Reporter implementations.
- Rust code edits are delegated to `rust-cli-writer` / `rust-cli-test-writer` per `.claude/rules/rust-delegation.md`.
- M1–M4 single-file behaviour and golden snapshots stay byte-identical — no new format alters how the existing four parsers behave.
- Dependencies must be MIT/Apache-2.0 to pass `cargo deny check`. Each chosen crate is verified.
- M5 ships read+write for HCL / INI / `.env` / CSV / TSV / Frontmatter, read-only for Dockerfile / ignore-list. The split is per [dq-plan.md:401-411](../../../dq-plan.md).

**Stakeholders:**

- Lint engine (M8): every standard ruleset for a format depends on the format being readable here.
- Pre-commit hook authors: `dq fmt --check '*.ini'` should work; `dq validate Dockerfile` should work.
- AI agents: machine-readable error from `dq convert app.csv -F json` when a row is malformed.
- Future milestones: M9 markdown body parsing extends the frontmatter wrapper; M11 OpenAPI / Terraform rulesets reuse HCL.

## Goals / Non-Goals

**Goals:**
- `dq get config.hcl /backend/0/region` reads HCL into the canonical `Value` shape.
- `dq paths app.csv` returns one pointer per cell (`/0/name`, `/0/age`, ...).
- `dq validate Dockerfile` exits 0 on a syntactically valid Dockerfile, exit 4 otherwise.
- `dq get post.md /title` returns the title from a Hugo/Jekyll-style YAML frontmatter block, leaving the body untouched.
- `dq convert app.env -F json` produces a JSON object of the env vars.
- `dq fmt config.ini -i` re-emits the INI file through `rust-ini`.
- `dq paths .gitignore` returns one pointer per pattern (`/0`, `/1`, ...).

**Non-Goals:**
- HCL with comment preservation. `hcl-rs` does not surface span info; we accept the v1 limitation.
- CSV with embedded JSON columns or string-typed numerics inferred. Each cell is a string; numeric inference would surprise users importing CSVs and is out of scope.
- `.env` shell-expansion semantics (`${OTHER_KEY}` interpolation). The parser stores raw strings; expansion is the runtime's job.
- Markdown body parsing — the M9 work. The body is opaque bytes carried alongside the parsed header.
- Dockerfile linting rules. Those land in M8 once Dockerfile is parseable.
- New CLI flags. M5 surfaces no new global or per-subcommand flags. (`-F` already exists; the only addition is more values for it.)
- TOON read support. TOON is a write-only format per M1's `format-support` spec. M5 does not change that.

## Decisions

### D1. Each new format produces a `Document::value_only` — no spans, no `set_at`/`del_at`

**Decision:** every new parser builds `Document::value_only(value, FormatTag::<New>)` with empty `original_bytes` and empty `SpanMap`. Mutating commands (`set`/`del`/`patch`/`merge`) targeting a new format will get the existing `Error::WriteUnavailable` from the textual-edit pipeline, the same way `dq set logs.jsonl /0 'foo' -i` errors today.

**Alternatives:**
- Build span maps for HCL via `hcl-rs` token spans: the crate exposes some position info, but the API is not stable enough to commit to a span map; deferred to a v2 milestone.
- Build span maps for INI via custom byte scanning: feasible but doubles the implementation cost. Deferred.
- Make `set`/`del` re-emit through `Format::write` for new formats (lose spans, lose comments): violates the M2 contract for the formats that DO have round-trip preservation. Rejected.

**Trade-offs:** users running `dq set config.ini /database/host new-host -i` get an error saying "INI does not support in-place edits in M5; use `dq fmt config.ini -i` to canonicalize, or read+modify+write via `dq convert -F json`". This is documented in the format's `--help` text. Acceptable because every M5 format is a "lint scope" priority, not an "edit-in-place" priority.

### D2. Frontmatter is a wrapper format that delegates to YAML/TOML/JSON for the header

**Decision:** `Frontmatter::parse` looks at the first bytes of the file:
- `---\n...---\n` → header is YAML, parsed via `Yaml::parse`.
- `+++\n...+++\n` → header is TOML, parsed via `Toml::parse`.
- `{\n...}` (followed by blank line then body) → header is JSON, parsed via `Json::parse`.
- No recognized prefix → empty `Map` value, body equals the entire file.

The `Document` carries the header's parsed `Value` as the visible value (`doc.value()` returns the YAML/TOML/JSON-parsed map). The body is stored separately in a new `Document::frontmatter_body: Option<Vec<u8>>` field, populated only when the format is `FormatTag::Frontmatter`. `Format::write` for `Frontmatter` re-serializes the header through the inner format and concatenates the body.

**Alternatives:**
- Make frontmatter a CLI feature (a `--frontmatter` flag on `get`/`set`): leaks the abstraction, every command has to know about frontmatter. Rejected.
- Treat the body as part of the value (e.g. `value.body = "..."`): pollutes the value model with an md-specific field. Rejected.
- Support TOML frontmatter only (Hugo's default): too narrow — Jekyll uses YAML, MkDocs uses YAML, Obsidian uses YAML, and JSON frontmatter shows up in custom static-site generators. Three is the right number.

**Trade-offs:** Storing body bytes in `Document` adds 0–N KB per parsed file, which is a non-issue for in-memory reasoning. The new `frontmatter_body` field is `None` for every non-frontmatter format. Test coverage focuses on round-trip through `dq fmt` (header re-canonicalized, body byte-identical).

### D3. CSV requires top-level array-of-records; any other shape errors

**Decision:** `Csv::parse` requires a header row and produces `Value::Array(Value::Map { col: <string>, ... })` — every cell is a string, no type inference. `Csv::write` requires the input to be exactly that shape and errors with `Error::Format { format: "csv", message: ... }` otherwise. The error message names the offending shape ("expected array of objects, got `<top-level-type>`") and points at `dq paths` to inspect the document.

This matches `csv` crate's `Reader::deserialize` / `Writer::serialize` model and avoids the ambiguity of "what does CSV with nested objects mean."

**Alternatives:**
- Allow any tabular projection (e.g. column-of-records reading a CSV with one column): too clever, surprises users.
- Numeric type inference (`"42" → Int`): bites users when columns mix `"42"` and `"42.0"`. Strings only.
- Support multi-line cells via the `csv` crate's quote-handling: this works automatically because we use the crate's `Reader` defaults. Documented but not advertised.

**Trade-offs:** CSV-as-input commands are useful for queries (`dq get servers.csv /0/region`) and for the lint engine (data quality rules). CSV-as-output commands are limited to YAML/JSON of array-of-records. Acceptable.

### D4. `.env` parses with a hand-rolled scanner, writes with a custom emitter

**Decision:** `DotEnv::parse` is hand-rolled: a small line scanner that handles `KEY=VALUE`, `export KEY=VALUE`, double-quoted values with backslash escapes (`\\`, `\"`, `\n`, `\t`, `\r`), single-quoted values (literal — no escape processing), inline comments after unquoted values, and full-line `# comment` / blank-line skipping. The result is `Value::Map<String, Value::String>`. The `dotenvy` crate is intentionally NOT used — its public API (`dotenvy::from_read_iter`, `dotenvy::from_path_iter`) is a process-env loader and mutates `std::env`; it has no opt-in pure-parser surface that builds a `Map` without side effects. A 50-line custom scanner avoids the side-effect hazard.

`DotEnv::write` walks the map and emits one line per entry. Quoting is mechanical: if the value contains whitespace, `#`, `=`, `$`, `\`, `"`, or non-printable characters, double-quote it and escape (`\\`, `\"`). Otherwise emit unquoted. Source comments are NOT preserved — `.env` is a leaf format and round-trip-with-comments is not the user's expectation.

**Alternatives:**
- Delegate to `dotenvy::from_read_iter`: rejected because it mutates `std::env` as a side effect; tests would race and process-wide env pollution would surface in unrelated handlers.
- Use `dotenv-vault` (third-party): adds an unmaintained dependency.
- Match `dotenvy`'s behaviour exactly across every edge case: its parser handles 800+ lines of historical shell-quoting quirks. The custom scanner covers the common cases the M5 lint scope cares about; if a real-world `.env` file produces a divergent parse, file an issue and extend the scanner.

**Trade-offs:** Comments dropped on `dq fmt config.env -i`. Users who care about comments should not be using `dq fmt` on `.env` files; `dq get` and `dq set --jq` (in M7) still work without touching the file structure. The `dotenvy` workspace dependency is retained for future expansion (e.g. interpolation support) and for design-D11 parity, but is not pulled into the parse path today.

### D5. INI uses `rust-ini` with source-order preservation; `.properties` is the same parser with a different file extension

**Decision:** `Ini::parse` builds a `Map<section-name, Map<key, String>>`. Sections preserve source order (`rust-ini` does this with `OrderedHashMap`). The implicit anonymous section (keys before the first `[header]`) is stored under the empty-string key `""`. `Ini::write` emits each section header followed by its keys.

`.properties` (Java config files) is registered as a second extension on the same parser, since the syntax is a superset (Java `.properties` may use `:` as separator instead of `=`, but `rust-ini` accepts both).

**Alternatives:**
- Two separate parsers for INI and `.properties`: adds maintenance overhead with no semantic difference. Rejected.
- Skip `.properties`: too narrow — Spring Boot configs in 1Orbit infrastructure use `.properties`. Inclusion is cheap.

**Trade-offs:** quote preservation is opt-in via the `Document::ini_meta` shadow map (added in this change). For the M5 "lint scope" use case, quote preservation is a nice-to-have, not a contract. The `dq fmt config.ini -i` round-trip aims for "byte-identical for typical INI" but does not guarantee it.

### D6. Dockerfile is read-only; the value shape is `Array<Map>` indexed by line

**Decision:** `Dockerfile::parse` walks `dockerfile-parser-rs` instructions and builds `Array<Map { instruction: <FROM|RUN|COPY|...>, arguments: <string-or-list>, line: <u32> }>`. `Dockerfile::write` returns `Error::Format { format: "dockerfile", message: "Dockerfile is read-only in M5" }` so `dq convert deploy.yaml -F dockerfile` produces a clear error. The `Format::write_with_options` default impl forwards to `write` and inherits the same error.

**Alternatives:**
- Round-trip via re-emission: doable but useless until autofix (M10) needs it. Deferred.
- Skip Dockerfile entirely: defeats the purpose — the M8 `@std/dockerfile` ruleset is one of the four headlining ruleset families. Inclusion is the whole point of M5.

**Trade-offs:** users running `dq set Dockerfile /0/arguments 'busybox' -i` get the same `Error::WriteUnavailable` they get for any value-only document, with a more specific message. Documented.

### D7. `.gitignore` / `.dockerignore` is a flat array of strings

**Decision:** `IgnoreList::parse` reads non-blank, non-`#`-prefixed lines into a flat `Array<String>`. The original byte buffer is dropped — comments and blank lines are not preserved (the lint engine never needs them, and re-emitting is not in scope). `IgnoreList::write` returns `Error::Format { format: "ignore-list", ... }`.

**Alternatives:**
- Preserve comments as `Value::String("# ...")` interleaved with patterns: pollutes the value tree with markup. Rejected.
- Recognise the `!`-negation prefix and split into a `{patterns: [...], excluded: [...]}` shape: too clever; users running `dq paths .gitignore` expect a flat array. Rejected.

**Trade-offs:** comment-aware tooling has to bring its own parser. Acceptable for M5.

### D8. `Document::frontmatter_body` is the only `Document` shape change

**Decision:** add `frontmatter_body: Option<Vec<u8>>` field to `Document`. `Document::value_only` and `Document::single` initialise it to `None`. A new constructor `Document::frontmatter(value, body, format)` populates it. `Format::write` for `Frontmatter` reads it back to concatenate after the header.

This is the minimal extension that supports the wrapper-format pattern without leaking it elsewhere. Every other format ignores the field.

**Alternatives:**
- New `DocumentKind::Frontmatter` enum variant: requires touching every match arm. Rejected.
- Store the body inside `Value` (e.g. `Value::Frontmatter { header, body }`): pollutes the value space — the body is markup, not data, and `dq paths post.md` should not return `/body` as a pointer. Rejected.

**Trade-offs:** non-frontmatter `Document`s carry an extra `Option<Vec<u8>>` (`None`) — a `usize`-tagged null pointer per document. Negligible memory cost.

### D9. New `OutputFormat` enum variants for `convert -F`

**Decision:** `OutputFormat` enum gains `Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Frontmatter` (write-target). `Dockerfile` and `IgnoreList` are NOT added — they have no write semantics, so `-F dockerfile` would have no meaning. `dq convert deploy.yaml -F dockerfile` is rejected by clap with "invalid value 'dockerfile' for '--format <FORMAT>'".

The split between "all formats are inputs" and "write-targets are a subset" is documented on `dq convert --help`. The clap `ValueEnum` derive automatically rejects `dockerfile` / `ignore-list` for `-F`.

**Alternatives:**
- Single enum, runtime check: confusing — clap would accept `-F dockerfile` then fail at runtime with `Error::Format`. Better to fail at the parse step.
- Add a separate `InputFormat` enum: doubles the surface for one-off cases. Rejected.

**Trade-offs:** the `OutputFormat::Dockerfile` / `OutputFormat::IgnoreList` constants do not exist; format detection on the input side uses `dq_core::detect`/`by_name` which still recognises both. The asymmetry is intentional and documented.

### D10. No new CLI flags, no global flag changes

**Decision:** M5 ships zero new CLI flags. The seven new formats are recognised through the existing `-F` flag and through file-extension auto-detection. `--allow-templates` / `--raw-template-strings` (M2) and `--sort-keys` / `--indent` (M4) work for new formats whose grammar makes sense (HCL templates exist; INI does not have templates; CSV has neither).

`--sort-keys` works for HCL (re-emitting via `hcl-rs`), INI (sorts keys within each section), `.env` (sorts top-level keys), Frontmatter (sorts inside the header). It is a no-op for CSV (column order is meaningful), TSV, Dockerfile, ignore-list. `--indent` works for HCL (which has nested blocks); is a no-op for INI / `.env` / CSV / Dockerfile / ignore-list / Frontmatter.

**Alternatives:**
- Add `--csv-delimiter` to choose `,` vs `;`: yes, CSVs from the EU are often `;`-delimited, but the user runs `-F tsv` (or a future `-F scsv`) — register the format twice with different delimiters instead of bolting on a flag. Deferred until concrete use case appears.
- Add `--frontmatter-format yaml|toml|json` to force a specific header format: the parser auto-detects it, and converting between header formats is `dq convert post.md --rewrite-frontmatter -F toml` (a future flag, not in M5).

**Trade-offs:** users with non-standard delimiters use `-F` to a custom format, or pre-process. Acceptable for M5.

### D11. Dependencies are added to the workspace and re-used per-crate

**Decision:** all five new crate dependencies are added to `[workspace.dependencies]` in the workspace `Cargo.toml`. `crates/dq-core/Cargo.toml` references them as `workspace = true`. This keeps version pinning centralised and lets future format expansions reuse the same crates without bumping per-crate.

Versions chosen at M5 planning:
- `hcl-rs = "0.19"` — current stable, MIT, ~700 stars, last release 2025-Q4. (Bumped from the original `0.18` planning note to track the latest stable release.)
- `rust-ini = "0.21"` — MIT, ~150 stars, very stable.
- `dotenvy = "0.15"` — MIT, the canonical Rust dotenv loader. Carried as a workspace dep but not used at parse time (see D4).
- `csv = "1.3"` — BurntSushi MIT, the standard.
- `dockerfile-parser-rs = "3.3"` — Apache-2.0, 1Password-maintained, 1k+ stars. (Bumped from the original `0.9` planning note — the `0.9` release belongs to a different, deprecated crate.)

**Alternatives:**
- Vendor each parser: massive maintenance cost. Rejected.
- Use `nom`-based hand-written parsers for INI / `.env`: doable in 200 LoC each, but doubles the test surface for marginal benefit. Rejected.

**Trade-offs:** five new transitive dep trees in `Cargo.lock`. `cargo deny check` is run as part of acceptance; any license / advisory issue blocks the change.

## Risks / Trade-offs

- **`hcl-rs` has no comment-preserving emitter.** Mitigation: documented; users who need round-trip with comments use `terraform fmt` before `dq`. Acceptable for M5.
- **CSV/TSV write rejects non-tabular shapes.** Mitigation: clear error message naming the offending shape; `dq paths` and `dq type` help users diagnose. Acceptable.
- **Frontmatter parsing for ambiguous headers** (a markdown file that starts with `---` not as frontmatter — e.g. a horizontal rule): the parser SHALL only treat `---` as frontmatter when followed by a newline AND a closing `---` within the first ~64 KB of the file. If no closing marker is found, fall back to "no frontmatter, body = whole file." Documented.
- **Adding seven formats touches the registry`.const`-array** in one place. Risk of test regression for the existing four formats is near-zero (tests are byte-identity and the new entries land at the end of the array); golden suite re-runs unchanged.
- **License compliance.** `cargo deny check` runs in CI as part of M3 baseline; we re-run after dependency additions. All five are MIT or Apache-2.0.

## Migration Plan

No migration required for users — every M1–M4 invocation produces byte-identical output. Users who were workaround-converting `.env` / `.ini` / `.csv` with shell pipelines can drop those scripts.

For developers extending `dq` after M5: when adding a new format in M6+, follow the same pattern — implement `Format`, register, add tests. The wrapper-format pattern (Frontmatter) is reusable for any format that has a typed header and an opaque body (e.g. `.editorconfig` which has a global header + per-section rules).

## Open Questions

- Should `.toml.tpl` / `.yaml.tpl` (template-style files in 1Orbit infrastructure) be auto-recognised as their non-`.tpl` counterpart? Defer; users can pass `-F` for the M5 baseline. M6 ships `--allow-templates` integration if demand surfaces.
- Should `dq` recognize `.envrc` (direnv) the same as `.env`? The grammar is a superset — direnv allows arbitrary shell commands. Defer; `.envrc` is shell, not data. Documented.
- Should HCL2's `for_each` and `dynamic` blocks be flattened on read? `hcl-rs` represents them as opaque expression nodes — flattening is a Terraform-specific operation. Defer to M11 `@std/terraform` ruleset.
