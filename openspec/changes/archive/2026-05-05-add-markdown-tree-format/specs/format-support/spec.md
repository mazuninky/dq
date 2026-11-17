## ADDED Requirements

### Requirement: Markdown read and write support

`dq-core` SHALL parse CommonMark + GFM markdown documents (`.md`, `.markdown` extensions) into `Document` via the `comrak` crate. The parser produces a typed-node tree shaped as `Value::Map` with a `"type"` discriminator string field per node. The top-level value is `Value::Map { "type": "document", "frontmatter": Value::Map | Null, "children": Value::Array<node>, "position": <span> }`. Frontmatter, when present, is folded into the `frontmatter` field as `{ "kind": "yaml" | "toml" | "json", "value": <parsed-header> }`.

The parser SHALL enable the following GFM extensions unconditionally: tables, strikethrough, autolinks, task list items, footnotes, header IDs. Front-matter detection covers `---\n…\n---\n` (YAML), `+++\n…\n+++\n` (TOML), and `{\n…\n}\n` followed by a blank line (JSON). When no recognised opening delimiter is present at the start of the file, OR no matching closing delimiter is found within the first 64 KiB, the `frontmatter` field is `Value::Null` and `children` represents the entire input.

Block-level position tracking is enabled (`comrak::Options::render::sourcepos = true`). Every block node carries `position: { "start": { "line": Int, "column": Int }, "end": { "line": Int, "column": Int } }` with 1-based line and column. Inline-level positions are NOT a contract in M9 (deferred to a future milestone).

The write path:
1. If `Document::value()` is structurally equal to the parsed-from-original baseline, emits `Document::original_bytes()` verbatim (byte-identical round-trip).
2. Otherwise renders the AST via `comrak::format_commonmark` with default options. Canonical re-emission is **lossy** for: trailing whitespace; fence character (` ``` ` vs. `~~~`); reference-link definition order and placement; indent style; whitespace inside `[…](…)` constructs.

The format is registered in `parsers::registry()` BEFORE `Frontmatter` so extension-based dispatch for `.md` / `.markdown` resolves to `Markdown`. The `Frontmatter` format remains in the registry and remains reachable via `-F frontmatter`.

#### Scenario: Simple heading + paragraph parses to AST

- **WHEN** the parser is fed `# Hello\n\nA paragraph.\n`
- **THEN** the resulting `Document` value is `Map { "type": "document", "frontmatter": Null, "children": [Map { "type": "heading", "level": 1, "children": [Map { "type": "text", "value": "Hello" }], "position": ... }, Map { "type": "paragraph", "children": [Map { "type": "text", "value": "A paragraph." }], "position": ... }], "position": ... }`

#### Scenario: YAML frontmatter folds into AST

- **WHEN** the parser is fed `---\ntitle: Hello\n---\n# Body\n`
- **THEN** the resulting `Document` value's `frontmatter` field equals `Map { "kind": "yaml", "value": Map { "title": String("Hello") } }`, and the first child is a `heading` node — NOT a separate frontmatter child node

#### Scenario: TOML frontmatter folds into AST

- **WHEN** the parser is fed `+++\ntitle = "Hello"\n+++\n# Body\n`
- **THEN** the resulting `Document` value's `frontmatter` field equals `Map { "kind": "toml", "value": Map { "title": String("Hello") } }`

#### Scenario: No frontmatter produces null

- **WHEN** the parser is fed `# Just a heading\n\nText.\n`
- **THEN** the resulting `Document` value's `frontmatter` field is `Value::Null`

#### Scenario: Fenced code block exposes lang and value

- **WHEN** the parser is fed ```` ```yaml\nfoo: bar\n``` ````
- **THEN** the resulting AST contains a child `Map { "type": "code_block", "fenced": true, "lang": String("yaml"), "value": String("foo: bar\n"), "info": String("yaml"), "position": ... }`

#### Scenario: Unfenced indented code block has null lang

- **WHEN** the parser is fed `    foo bar\n` (four-space-indented)
- **THEN** the resulting AST contains a child `Map { "type": "code_block", "fenced": false, "lang": Null, "value": String("foo bar\n"), "info": String(""), "position": ... }`

#### Scenario: GFM table parses to typed node tree

- **WHEN** the parser is fed `| a | b |\n|---|---|\n| 1 | 2 |\n`
- **THEN** the resulting AST contains a `table` node with `children` of `table_row` nodes, each containing `table_cell` children; the first row carries `header: true`

#### Scenario: GFM task list item exposes checked state

- **WHEN** the parser is fed `- [x] done\n- [ ] todo\n`
- **THEN** the AST contains a `list` node whose two `list_item` children carry `checked: true` and `checked: false` respectively

#### Scenario: Verbatim re-emission for unmutated document

- **WHEN** a markdown file with mixed headings, code blocks, lists, and tables is parsed and immediately written via `Format::write` without mutation
- **THEN** the output bytes equal the input bytes (byte-identical)

#### Scenario: Canonical re-emission for mutated document

- **WHEN** a markdown file is parsed, the value tree is mutated (e.g., a heading's level is changed via `Document::set_at`), and written via `Format::write`
- **THEN** the output is valid CommonMark / GFM markdown produced by `comrak::format_commonmark` with default options; it MAY differ from the original in trailing whitespace, fence character, or other documented lossy aspects, but MUST round-trip back to the same `Document::value()` when re-parsed

### Requirement: `FormatTag::Markdown` variant

`Document::FormatTag` SHALL gain a `Markdown` variant. `FormatTag::from_name` SHALL recognise `"markdown"` and return `Some(FormatTag::Markdown)`. `FormatTag::name(&self)` for the `Markdown` variant SHALL return `"markdown"`.

#### Scenario: from_name maps the new tag

- **WHEN** the caller invokes `FormatTag::from_name("markdown")`
- **THEN** the result is `Some(FormatTag::Markdown)`

#### Scenario: name returns the canonical lowercase string

- **WHEN** the caller invokes `FormatTag::Markdown.name()`
- **THEN** the result is `"markdown"`

### Requirement: `OutputFormat::Markdown` write target

`crates/dq-cli/src/output/mod.rs::OutputFormat` SHALL gain a `Markdown` variant. `dq convert -F markdown` is accepted at the clap layer and dispatches to the `Markdown` writer.

#### Scenario: convert -F markdown is accepted

- **WHEN** the user runs `dq convert post.md -F markdown`
- **THEN** clap parses the value successfully and the convert handler dispatches to `Markdown::write`

## MODIFIED Requirements

### Requirement: Markdown frontmatter read and write support

`dq-core` SHALL retain the `Frontmatter` format introduced in M5 as an alternative reachable via `-F frontmatter` and via `FormatTag::from_name("frontmatter")`. Default extension-based dispatch for `.md` / `.markdown` SHALL resolve to the `Markdown` format introduced in M9, NOT to `Frontmatter`. The `Frontmatter` format's `extensions()` method SHALL continue to return `["md", "markdown"]` for explicit-name reachability, but registry order (Markdown before Frontmatter) ensures the M9 default.

The M5 frontmatter parser's behaviour is otherwise unchanged: parses the YAML / TOML / JSON header into `Document::value()`, stores the body as opaque bytes, and round-trips the body verbatim.

#### Scenario: Default `.md` dispatch goes to Markdown

- **WHEN** `format::detect(Utf8Path::new("post.md"))` is called
- **THEN** the result is `Some(&Markdown)` (NOT `Some(&Frontmatter)`)

#### Scenario: `-F frontmatter` opt-in still routes to M5 parser

- **WHEN** `parsers::by_name("frontmatter")` is called
- **THEN** the result is `Some(&Frontmatter)` and `Frontmatter::parse` produces the M5-shaped Document (header value + opaque body bytes)

#### Scenario: M5 frontmatter test fixtures pass under explicit `-F frontmatter`

- **WHEN** the existing M5 frontmatter parser test suite is run with the `Frontmatter` format selected by name (not by extension)
- **THEN** every test passes without modification of the test code

### Requirement: M1 anti-scope for formats

The crate SHALL NOT include parsers for **XML write** or the conftest-only formats (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL); those remain anti-scope. The earlier wording referencing the markdown body parser as anti-scope is updated: the markdown body parser is added by this M9 change.

#### Scenario: Unsupported format error

- **WHEN** the user runs `dq get script.sh /x` (no registered format for `.sh`)
- **THEN** the command writes a structured error suggesting `-F <fmt>` and exits with code 1
