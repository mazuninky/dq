## Why

M1–M4 covered the four formats users edit by hand most often (YAML, JSON, TOML, JSONL) with safe writes, bulk edits, and canonicalisation. What `dq` cannot do yet — and what blocks the M8 lint engine — is read every other format DevOps engineers have to reason about: HCL, INI/`.properties`, `.env`, CSV/TSV, Dockerfile, `.gitignore`/`.dockerignore`, and Markdown frontmatter. Until those land, the lint engine has nothing useful to lint outside Kubernetes manifests.

The M5 envelope per [dq-plan.md:401-411](../../../dq-plan.md) is "format coverage needed for linters in M8" — read-only is acceptable for formats whose primary value is being scanned (Dockerfile, ignore-lists), and write support without comment-preserving round-trip is acceptable for formats where round-trip is not the user's expectation (HCL v1, CSV, `.env`). Markdown frontmatter is the single tricky case: the body of the document is opaque, but the header block must round-trip through one of the existing parsers (YAML/TOML/JSON).

The risk envelope is small. None of these formats touches the M2 textual-edit pipeline — they all use the M1 `Document::value_only` shape (no spans, no `set_at` / `del_at`). New `Format` impls plug into the existing registry; `dq get`, `dq paths`, `dq convert`, `dq validate` pick them up by extension automatically. Where round-trip matters (frontmatter), the wrapper delegates to the inner parser and concatenates its output with the opaque body — no new parser internals.

## What Changes

- **HCL (read+write, no formatting preservation in v1).** New `dq_core::parsers::hcl::Hcl` over `hcl-rs`. Read parses any HCL file (`.hcl`, `.tf`, `.tfvars`) into `Document` (objects → `Map`, arrays → `Array`, scalars typed). Write re-emits via `hcl::to_string` — comments and operator spacing are NOT preserved (documented). Registered in the format registry; `dq get`, `dq paths`, `dq convert`, `dq validate` accept HCL inputs by extension.
- **INI / `.properties` (read+write, preserve quotes via flag).** New `dq_core::parsers::ini::Ini` over `rust-ini`. Read parses a file with `[section]` headers (and the implicit anonymous section for keys before the first header) into a two-level `Map<section, Map<key, String>>`. Write re-emits sections in source order. Quote-style preservation: a top-level boolean flag inside `Document::ini_meta` carrying which keys were quoted in the source — re-emit honours it.
- **`.env` (read+write).** New `dq_core::parsers::dotenv::DotEnv`. Read uses a hand-rolled line scanner (the `dotenvy` crate's loader API mutates `std::env` and is unsuitable as a pure parser; see design D4) to parse `KEY=VALUE` and `export KEY=VALUE` lines into a flat `Map<String, String>`. Write emits one `KEY=VALUE` per line in source order; quoting is recovered when the value contains whitespace, `#`, or `$`.
- **CSV / TSV (top-level array-of-objects only).** New `dq_core::parsers::csv::Csv` (over the `csv` crate). Read assumes a header row; each data row becomes a `Map` keyed by header columns; the document is `Array<Map>` at the top level. Write requires the document to be `Array<Map>` whose maps share the same key set; any other shape errors with `Error::Format`. TSV is the same parser with `b'\t'` as the delimiter (`.tsv` extension).
- **Dockerfile (read-only).** New `dq_core::parsers::dockerfile::Dockerfile` over `dockerfile-parser-rs`. Parses each instruction into a `Map { instruction: <name>, arguments: <string-or-list> }` and the file-level shape is `Array<Map>` indexed by line. `Format::write` returns `Error::Format` ("dockerfile is read-only").
- **`.gitignore` / `.dockerignore` (read-only).** New `dq_core::parsers::ignore_list::IgnoreList`. Parses one pattern per non-blank, non-`#`-prefixed line into a flat `Array<String>`. Comments and blank lines are dropped from the value tree (they are byte-only artifacts the lint engine does not need to reason about). `Format::write` returns `Error::Format` ("ignore-list is read-only").
- **Markdown frontmatter (read+write).** New `dq_core::parsers::frontmatter::Frontmatter`. Detects a `---`/`---` block (YAML), `+++`/`+++` block (TOML), or `{` -prefixed block (JSON) at the start of the file, parses it through the corresponding inner parser, and stores the body of the document as an opaque `Document` field. The exposed value is the parsed header — so `dq get post.md /title` returns the title. Write re-serializes the header through the inner parser and concatenates the unchanged body bytes. Files without a frontmatter block produce an empty `Map` value and a body equal to the entire file.
- **`Document::value_only` / `Document::single` constructors and `FormatTag` extended.** `FormatTag` gains `Hcl`, `Ini`, `DotEnv`, `Csv`, `Dockerfile`, `IgnoreList`, `Frontmatter` variants so the rendering factory and `convert -F <fmt>` flag accept them.
- **`dq convert` accepts every new format as a write target where applicable.** Concretely: `-F hcl|ini|dotenv|csv|tsv|frontmatter` are accepted; `-F dockerfile|ignore-list` are rejected by clap at parse time with `INVALID_INPUT` (exit 6) — the `OutputFormat` enum has no `Dockerfile` / `IgnoreList` variants, so the values never reach a handler. Independently, write commands (`set`, `del`, `patch`, `merge`) targeting a Dockerfile or ignore-list input file return `Error::WriteUnavailable` (exit 7 / `WRITE_FAILED`) because the document is loaded read-only. `convert` between any pair of formats uses the in-memory `Value` projection — round-trip semantics depend on the value space being expressible in both formats (e.g. `dq convert deploy.yaml -F csv` errors when the YAML is not a flat array-of-records).
- **Anti-scope reaffirmed.** Markdown body parsing (the AST), XML, and the conftest-only formats (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL) remain out of scope for M5 per [dq-plan.md:409](../../../dq-plan.md). They are added in M9 / M11 / on-demand only.

**What's NOT in M5** (per [dq-plan.md:409](../../../dq-plan.md)):
- Full markdown AST (M9). Frontmatter only.
- XML write (M11+).
- HCL formatting preservation (a v2 question — `hcl-rs` does not surface span info today).
- Span-aware `set_at`/`del_at` for any of the new formats. They all use `value_only` shape — `set` / `del` / `patch` / `merge` will reject them with `Error::WriteUnavailable` and a message naming the format. (`fmt` works through the `write_with_options` re-emit path, so `dq fmt config.ini` is supported.)
- The three deferred YAML flags (`--quote-style`, `--flow-style`, `--strip-comments`).

## Capabilities

### New Capabilities

None. Every change is an extension to existing capabilities.

### Modified Capabilities

- `format-support`: adds seven new `Format` impls and corresponding `FormatTag` variants. Documents which formats round-trip cleanly (frontmatter via inner parser; INI via `rust-ini` source order; `.env` via custom emitter), which round-trip without formatting (HCL v1; CSV/TSV — n/a tabular), and which are read-only (Dockerfile, ignore-list). The "M1 anti-scope for formats" requirement gets renamed to "M1–M4 anti-scope for formats" and explicitly notes which entries M5 lifts.
- `data-query-read`: extends the format-detection contract — `dq get`, `dq paths`, `dq keys`, `dq values`, `dq len`, `dq type`, `dq exists`, `dq select`, `dq validate` all accept the new extensions automatically through the registry. The unsupported-format error message now lists the seven new formats (so users running `dq get config.toml` on a TOML file with a non-standard extension can reach for `-F`). For Dockerfile and ignore-list, write-targeting subcommands error with `Error::Format` immediately and the message names the read-only status.

(`data-query-write`, `data-query-bulk`, `data-query-fmt`, `cli-shell`, `path-syntax`, `template-guard` are NOT modified — none of the new formats lifts any restriction in those capabilities. Write commands continue to require span-aware shapes; the bulk driver and `fmt` driver pick up new extensions for free; the JSON Pointer / glob / template-guard contracts are untouched.)

## Impact

- **Code (new modules under `crates/dq-core/src/parsers/`)**:
  - `hcl.rs` — `Hcl` struct + `Format` impl wrapping `hcl-rs`.
  - `ini.rs` — `Ini` struct + `Format` impl wrapping `rust-ini`, plus a small wrapper for source-quote preservation.
  - `dotenv.rs` — `DotEnv` struct + custom parser/writer (entirely hand-rolled; the `dotenvy` workspace dep is retained for future expansion but not used at parse time).
  - `csv.rs` — `Csv` struct + `Format` impl wrapping the `csv` crate (TSV is the same impl with a delimiter override registered as a separate static).
  - `dockerfile.rs` — `Dockerfile` struct + `Format` impl wrapping `dockerfile-parser-rs`. Read-only.
  - `ignore_list.rs` — `IgnoreList` struct + `Format` impl with own line scanner. Read-only.
  - `frontmatter.rs` — `Frontmatter` struct + `Format` impl that delegates to YAML/TOML/JSON for the header block and stores the body as opaque bytes.
- **Code (`dq-core` boundary updates)**:
  - `crates/dq-core/src/document/mod.rs` — extend `FormatTag` with seven new variants; update `from_name` to recognise their `name()` strings.
  - `crates/dq-core/src/parsers/mod.rs` — `pub mod` for each new parser; extend `registry()` array.
  - `crates/dq-core/src/lib.rs` — re-export new format types if any cross-crate consumer needs them (otherwise keep them module-private).
  - `crates/dq-core/Cargo.toml` — add dependencies: `hcl-rs = "0.19"`, `rust-ini = "0.21"`, `dotenvy = "0.15"`, `csv = "1.3"`, `dockerfile-parser-rs = "3.3"`. Workspace `Cargo.toml` mirrors them under `[workspace.dependencies]` so per-crate stanzas use `workspace = true`. The `hcl-rs` and `dockerfile-parser-rs` versions are bumped from the original M5-planning notes (`0.18` and `0.9` respectively) to track the latest stable releases on crates.io at implementation time — `dockerfile-parser-rs` `0.9` belongs to a different, deprecated crate.
- **Code (`dq-cli` updates)**:
  - `crates/dq-cli/src/output/mod.rs` — extend `OutputFormat` enum with new write-target variants (`Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Frontmatter`); reject the read-only ones (`Dockerfile`, `IgnoreList`) at the clap layer.
  - `crates/dq-cli/src/commands/convert.rs` — same changes the `OutputFormat` extension implies; ensure `Error::Format` from a read-only format target is mapped to `INVALID_INPUT` (exit 6), not `PARSE_ERROR` (exit 3).
  - `crates/dq-cli/src/commands/{set,del,patch,merge}.rs` — when targeted at a `value_only` document (no spans), surface a clear `Error::WriteUnavailable` naming the format. The infrastructure already exists; this change is just verifying every new format produces the right error message.
- **Tests (new)**:
  - `crates/dq-core/tests/parse_hcl.rs`, `parse_ini.rs`, `parse_dotenv.rs`, `parse_csv.rs`, `parse_dockerfile.rs`, `parse_ignore_list.rs`, `parse_frontmatter.rs` — unit-style integration tests per parser: round-trip on a representative fixture, error shapes for malformed inputs, registry detection by extension.
  - `crates/dq-cli/tests/unit_format_extensions.rs` — handler-level: `dq get config.hcl /backend/0/region`, `dq paths .env`, `dq convert app.csv -F json`, `dq validate Dockerfile`, `dq fmt config.ini -i` round-trip.
  - `crates/dq-cli/tests/cli_smoke.rs` — three smoke scenarios: HCL convert to JSON, frontmatter `dq get` on a Hugo post, ignore-list `dq paths`.
  - `crates/dq-cli/tests/golden.rs` — golden fixtures grow from the M4 set: add 8 fixture files (one per new format + a frontmatter-with-yaml-header, frontmatter-with-toml-header).
- **Dependencies (new)**: `hcl-rs`, `rust-ini`, `dotenvy`, `csv`, `dockerfile-parser-rs`. Each is well-maintained (>1k stars or active maintenance in the last 6 months as of M5 planning). License audit via `cargo deny check`: all are MIT/Apache-2.0 dual; `cargo deny` config does not need updates.
- **Backward compatibility**: every M1–M4 invocation produces byte-identical output. New formats only activate when the user runs `dq` against a file with a new extension (`.hcl` / `.tf` / `.ini` / `.env` / `.csv` / `.tsv` / `Dockerfile` / `.gitignore` / `.dockerignore` / `.md`) or passes `-F <new-fmt>`. The format dispatcher's behaviour for unrecognised extensions is unchanged (clear `Error::Format` with a `did_you_mean` suggestion).
- **Project meta**:
  - `dq-plan.md` M5 section gains a `✅ Implemented YYYY-MM-DD` marker at archive time.
  - `README.md` status moves from `M4 alpha — adds dq fmt + --sort-keys + --indent` to `M5 alpha — adds HCL, INI, .env, CSV/TSV, Dockerfile, ignore-list, Markdown frontmatter`.
  - The `Поддерживаемые форматы` table in `dq-plan.md` gets `✓` markers for the seven new formats.
