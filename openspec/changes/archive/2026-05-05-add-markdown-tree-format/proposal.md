## Why

M8 shipped the lint engine plus standard rule libraries for `@std/k8s`, `@std/dockerfile`, `@std/npm`, and `@std/github-actions`. The plan's M9 envelope per [dq-plan.md:471-483](../../../dq-plan.md) closes the last format gap before composite rules (M11) and autofix (M10): **markdown becomes a first-class queryable format with a full AST**, and `@std/markdown` joins the standard ruleset roster.

This is the proof that the format-agnostic architecture works for **tree-structured data**, not only for key-value documents. The four prior format-extension milestones (M1 JSON/YAML/TOML, M5 HCL/INI/.env/CSV/Dockerfile/ignore-list/frontmatter, M8 used these formats from the lint engine) only exercised recursive map/array shapes — markdown is the first format where the natural data model is a typed tree (heading → paragraph → link / code block / list-item / etc.). If the existing `Document::Value` recursive enum + JSON Pointer + jq stack handles markdown linting cleanly, the architecture has paid off; if it doesn't, M11 composite rules will hit the same wall.

The M5 frontmatter parser (introduced as part of [add-format-extensions](../archive/2026-05-04-add-format-extensions/)) parses the YAML/TOML/JSON header of a `.md` file and stores the body as opaque bytes. That is sufficient for static-site generators and `dq get .md /title` workflows, but it is **not** sufficient to lint the markdown body itself (heading order, broken links, code-block language, etc.). M9 adds a parallel **`Markdown` format** with a full CommonMark+GFM AST via `comrak`, makes it the default extension dispatch for `.md` / `.markdown`, and folds the frontmatter into the AST as a first-class node. Users who specifically want the M5 "header + opaque body" shape can still reach it via `-F frontmatter`.

The risk envelope is moderate. The new code is contained in `dq-core` (one new parser module + one new `FormatTag` variant) and `dq-lint` (the `@std/markdown` rule directory + tests). No `dq-cli` command surface changes — every existing command (`get`, `paths`, `keys`, `len`, `type`, `select`, `query`, `convert`, `validate`, `lint`, `check`, `test`, `explain`, `rules`) inherits markdown support through the registry. The single load-bearing decision — **encode the AST as `Value::Map` with a `type` discriminator field** rather than introducing a separate `Tree` enum into `Document` — keeps the existing JSON Pointer / jq infrastructure load-bearing and unchanged. A separate `Tree` enum was the original [dq-plan.md:474](../../../dq-plan.md) phrasing; in implementation review the discriminator-Map encoding turned out to be functionally equivalent for query/lint workloads while requiring zero changes to `Pointer`, `Reporter`, jq, or the bulk driver. The `tree` enum option is preserved as future work in `## What's NOT in M9 (deferred)` if a future format (e.g. XML) ever needs typed-node performance optimisations the discriminator-Map encoding can't deliver.

The plan calls for "AST-селекторы" research at the start of M9 with CSS-style and bespoke options as candidates. M9 ships **JSON Pointer + jq** as the AST selector — no new selector language. Rationale: jq's filter syntax (`select(.type == "heading" and .level == 1)`) covers every use case the standard ruleset needs, and adding a selector DSL would double the surface area without serving a concrete pain point. CSS-style selectors remain explicit anti-scope for M9 and will be reconsidered if a community ruleset author surfaces a workflow that proves untenable in jq.

## What Changes

### New format: `Markdown` (`crates/dq-core/src/parsers/markdown.rs`)

- **CommonMark + GFM parser via `comrak`.** Parses `.md` / `.markdown` files into `Document::value()` shaped as a typed-node tree. Top-level value is `Value::Map { "type": Value::String("document"), "children": Value::Array<node>, "frontmatter": Value::Map | Null, "position": <span> }`. Each child node is itself a `Value::Map` with a `type` discriminator string and node-specific fields.
- **Node types covered.** Block: `heading`, `paragraph`, `code_block`, `block_quote`, `list`, `list_item`, `thematic_break`, `html_block`, `link_reference_definition`, `table`, `table_row`, `table_cell`, `front_matter`. Inline: `text`, `link`, `image`, `code` (inline code span), `emphasis`, `strong`, `strikethrough`, `html_inline`, `line_break`, `soft_break`. Task-list checkbox state on `list_item.checked: bool|null`. The full per-type field schema lives in [design.md](design.md) §1.
- **Frontmatter folded into the AST.** When the source begins with a recognised YAML / TOML / JSON frontmatter block (delimiters identical to the M5 `Frontmatter` parser), the corresponding header value is parsed via the inner format and attached to the document node as `frontmatter: { "kind": "yaml" | "toml" | "json", "value": <parsed-header> }`. When no frontmatter is present, `frontmatter: null`. The body AST is the rest of the file. This means `dq get post.md /frontmatter/value/title` works for Hugo/Jekyll/Obsidian content out of the box without `-F frontmatter`.
- **Position tracking.** Every block-level node carries `position: { "start": { "line": N, "column": N }, "end": { "line": N, "column": N } }` (1-based line/column, all `Value::Int`). `comrak::Options::render::sourcepos` is enabled for this. Inline nodes do NOT carry `position` in M9 — inline-level positions are an M11 refinement (see design D5). Diagnostic line/column extraction in `dq-exec` does not auto-walk this field today; rules that want a real position set `loc.line: ".position.start.line // 1"` explicitly per the existing M8 contract.
- **Round-trip strategy.** Verbatim re-emission for unchanged documents — the parser stores the original bytes alongside the parsed AST, and `Format::write` emits `original_bytes` when the value tree is structurally equal to the parsed-from-original baseline. Mutated documents fall back to canonical re-emission via `comrak::format_commonmark` with default options; this is **lossy for trailing whitespace, exact backtick-fence-vs-tilde style, and reference-link definition order**, and the docs say so. M9 markdown is primarily a lint-target format (read-heavy); textual-edit splicing is an M11+ refinement.
- **`FormatTag::Markdown` variant.** Added to `Document::FormatTag` enum. `FormatTag::from_name("markdown") -> Some(FormatTag::Markdown)`.
- **Registry registration.** `parsers::registry()` gains `&Markdown` placed **before** `&Frontmatter` so extension lookup for `.md` / `.markdown` resolves to `Markdown` by default. The `Frontmatter` parser remains registered (and still exposes its own `extensions(): ["md", "markdown"]`) so `-F frontmatter` and `FormatTag::from_name("frontmatter")` still resolve correctly.

### New standard ruleset: `@std/markdown` (`crates/dq-lint/rules/markdown/`)

≥18 markdown rules with co-located `*.test.yml` fixtures. Initial roster:

- **Structure**: `heading-order` (no level skips), `single-h1` (exactly one `# Title`), `no-trailing-whitespace`, `no-multiple-blank-lines`, `no-empty-paragraphs`, `final-newline`.
- **Content**: `no-empty-links` (every link has text), `no-empty-headings`, `no-broken-relative-links` (best-effort, file-existence check via the loader's working directory), `no-bare-urls` (require `<>` or `[text](url)`), `code-blocks-have-lang` (every fenced block declares an info string), `link-text-not-here` (warn on "click here" / "this page"), `no-duplicate-headings`.
- **Frontmatter**: `frontmatter-required-fields` (configurable list — default `title`, `date`), `frontmatter-date-format` (ISO-8601 / RFC 3339).
- **Tables**: `table-pipes-aligned` (column count consistent across rows), `table-header-required`.
- **HTML**: `no-inline-html` (warn-level by default — informational, not blocking).

The ruleset directory ships embedded via the same `include_str!` mechanism `dq-lint` uses for `@std/k8s`, `@std/dockerfile`, `@std/npm`, `@std/github-actions`. No build-script or filesystem scan at runtime.

### Capabilities

#### Modified Capabilities

- **`format-support`** — gains the `Markdown` format requirement (parser, AST shape, frontmatter folding, position tracking, round-trip semantics, registry placement) and a `FormatTag::Markdown` requirement. The existing M5 `Markdown frontmatter read and write support` requirement is updated to clarify that the `Frontmatter` format remains reachable via `-F frontmatter` but `.md` extension default dispatch now resolves to `Markdown`.
- **`data-query-read`** — gains a requirement that JSON Pointer addresses through the markdown AST land on typed-node `Value::Map`s and that jq filters can dispatch on the `.type` discriminator.
- **`data-query-rules`** — gains a `@std/markdown` namespace requirement and the new rule inventory.
- **`cli-shell`** — `OutputFormat` gains `Markdown` for `convert -F markdown` write target. No new subcommands.

### Meta

- **`dq-plan.md` M9 section.** Marker `✅ Implemented YYYY-MM-DD` plus cross-link to this archived change folder. The plan's "AST-селекторный язык для markdown: research в M9" line in `## Открытые вопросы` is closed with the resolution: JSON Pointer + jq, no new selector DSL.
- **`README.md`.** Status moves from `M8 alpha — adds the lint engine + standard ruleset library` to `M9 alpha — adds markdown AST + @std/markdown`. Examples block adds one `dq get post.md /frontmatter/value/title` and one `dq lint docs/**/*.md` invocation.

### What's NOT in M9 (deferred)

- **`Tree` enum in `Document`.** The discriminator-Map encoding handles every M9 use case. A typed `Tree` enum is reconsidered if (a) XML support arrives in M11+ and benefits from typed-node performance, or (b) a community plugin requires it.
- **CSS-style AST selectors.** jq's `.type ==` discriminator pattern subsumes the use case; revisit only if community feedback proves otherwise.
- **Inline-level position spans.** M9 ships block-level only. Inline `position` is an M11 refinement — most useful diagnostics need block-level (heading order, code block lang, link target).
- **`set` / `del` / `patch` / `merge` round-trip on markdown.** Mutated markdown re-emits canonically via `comrak::format_commonmark` and is **lossy for trailing whitespace and fence-style preservation**. Textual-edit splice paths for markdown are M11+ work.
- **Composite rules.** "Code blocks with `lang: yaml` must be valid YAML" is M11 territory; M9 makes the underlying machinery (extracting code-block bodies via jq) reachable but doesn't ship the cross-format check.
- **`@std/static-sites`.** Hugo/Jekyll/Obsidian-specific frontmatter rules remain anti-scope until a concrete spec lands; M9 ships only the format-neutral `frontmatter-required-fields` / `frontmatter-date-format`.
- **`comrak` extension toggles via flag.** GFM extensions (tables, strikethrough, autolinks, task list items, footnotes) are enabled unconditionally. Per-document opt-out is M11+.
- **JSON Schema rules over markdown frontmatter.** M11.

## Impact

- **New code (`dq-core`)**:
  - `crates/dq-core/Cargo.toml` — adds `comrak` dependency (workspace dep `comrak = "0.31"`, default features minus the optional `shortcodes` extension we don't need).
  - `crates/dq-core/src/parsers/markdown.rs` — `pub struct Markdown` `Format` impl + AST conversion + frontmatter folding + verbatim re-emission helper.
  - `crates/dq-core/src/parsers/mod.rs` — `pub mod markdown;` + `pub use markdown::Markdown;` + registry update (Markdown before Frontmatter).
  - `crates/dq-core/src/document/mod.rs` — new `FormatTag::Markdown` variant; `from_name("markdown") -> Some(Self::Markdown)`; `name() -> "markdown"`; `extensions()` mapping.
- **`dq-cli` updates**:
  - `crates/dq-cli/src/output/mod.rs` — `OutputFormat` enum gains `Markdown` so `convert -F markdown` is accepted at the clap layer.
- **`dq-lint` updates**:
  - `crates/dq-lint/rules/markdown/{rule}.yml` + matching `{rule}.test.yml` — ≥18 rule + test pairs.
  - `crates/dq-lint/src/embed.rs` (or whatever the M8 embedding module is named) — register the `markdown` namespace.
- **Workspace dependency**:
  - `Cargo.toml` — `comrak = "0.31"` added under `[workspace.dependencies]` with comment explaining M9 scope.
- **Tests (new)**:
  - `crates/dq-core/src/parsers/markdown.rs` `#[cfg(test)] mod tests` — ≥15 unit tests covering each block node type, GFM extensions, frontmatter folding (yaml/toml/json/none), position tracking, round-trip without mutation.
  - `crates/dq-core/tests/parse_markdown.rs` — fixture-driven parser coverage on representative documents (Hugo post, Obsidian note, README, GFM-heavy doc).
  - `crates/dq-cli/tests/cli_markdown.rs` — `dq get`, `dq paths`, `dq query`, `dq lint`, `dq convert` happy paths against markdown.
  - `crates/dq-cli/tests/cli_lint.rs` — extend with `@std/markdown` auto-binding when `.md` files appear in `lint`'s file list.
- **Backward compatibility**: `dq get post.md /title` (which previously routed to `Frontmatter` and looked up the YAML header's top-level key) now routes to `Markdown` and looks up the document AST's top-level key. **This is a breaking change in default behaviour**: the new equivalent is `dq get post.md /frontmatter/value/title`, OR `dq get post.md /title -F frontmatter` to opt into the M5 shape explicitly. Documented in CHANGELOG; release notes for M9 call it out as the one breaking change. Every other `.md`-handling invocation (`paths`, `len`, `keys`, etc.) walks a different but coherent shape; users who scripted against M5 frontmatter need to either thread `-F frontmatter` through or adopt the new pointer.
- **Project meta**: `dq-plan.md` M9 marker; `README.md` status line + markdown examples; `CHANGELOG.md` entry calling out the breaking change for `.md` extension default dispatch and the new `@std/markdown` rule count.
