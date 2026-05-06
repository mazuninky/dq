## Context

M8 finalised the lint engine: `dq-exec` reads rule YAML, evaluates `match.filter` and `check.jq` over a parsed `Document::value()`, and emits `Diagnostic`s with line/col extracted from the violation node when `loc` is not specified. Four namespaces (`@std/k8s`, `@std/dockerfile`, `@std/npm`, `@std/github-actions`) ship 26 rules across 28 test fixtures. The remaining gap from `dq-plan.md` Roadmap is M9 — markdown linting — which the plan flagged as the **first tree-format**, and explicitly the validator that the format-agnostic architecture works for non-key-value data.

The M5 [add-format-extensions](../archive/2026-05-04-add-format-extensions/) change introduced `Frontmatter` as the `.md` parser: it splits the YAML/TOML/JSON header from the opaque body. That covers the static-site use case (read/write the header) but leaves the body unqueryable. M9 adds a parallel `Markdown` parser that produces a full CommonMark+GFM AST and folds the frontmatter into it as a first-class node, then promotes `Markdown` to be the default extension dispatch for `.md` and `.markdown`.

The lint engine itself needs zero changes. M9 is entirely **a new format** plus **a new standard ruleset**; the rest of the work is wiring.

## Goals / Non-Goals

### Goals

- `dq lint docs/**/*.md` with no `--rules` flag finds typical structure / content / frontmatter violations across a representative documentation tree (this repo's `docs/`, the dq-plan, README, and a tested set of Hugo/Obsidian-style fixtures).
- `dq query post.md '.children[] | select(.type == "code_block") | .lang'` enumerates every code block's declared language, including inline `null` for unfenced blocks.
- `dq get post.md /frontmatter/value/title` returns the YAML frontmatter title without any `-F` flag.
- `dq lint --rules @std/markdown docs/spec.md -F sarif` produces SARIF v2.1.0 that GitHub PR annotations render correctly.
- `dq test @std/markdown` is green for ≥18 rules; total project test count after M9 is ≥120.
- `cargo test --workspace --all-features` runs cold in ≤30 seconds (the M1+ runtime budget per `references/cli-testing.md`).

### Non-Goals

- **`Tree` enum.** The discriminator-Map encoding subsumes every M9 query. A typed-node enum is deferred to a future milestone if XML or a community plugin needs it.
- **CSS-style selectors.** jq covers the use cases.
- **Inline-level position spans.** Block-level only in M9.
- **Round-trip on mutation.** Verbatim only when `Document::value()` is structurally equal to the parsed-from-original baseline; otherwise the writer emits canonical commonmark via `comrak::format_commonmark`. Textual-edit splicing for markdown is M11+ work.
- **GFM extension toggles via CLI flag.** Tables, strikethrough, autolinks, task list items, and footnotes are unconditionally enabled.
- **Composite rules** (markdown → embedded YAML / JSON validation): M11.

## Decisions

### D1: AST encoding — discriminator-Map, not separate `Tree` enum

The plan's [dq-plan.md:474](../../../dq-plan.md) phrasing was "Tree модель в `dq-core`: `Tree` со типизированными узлами (`Heading`, `Paragraph`, `Link`, `CodeBlock`, и т.д.)". The original framing assumed a parallel data model alongside `Value` (key-value formats) and a new `Document` variant carrying the typed tree. In implementation review the discriminator-Map encoding turned out to be functionally equivalent for every M9 use case (query, lint, diagnostic location) at zero refactor cost across `Pointer`, `Reporter`, jq, and the bulk driver.

**Decision**: every markdown AST node is a `Value::Map` with a `"type"` discriminator string field (e.g., `"heading"`, `"paragraph"`, `"code_block"`) and node-specific fields. Children are stored as `Value::Array<Value::Map>` under the `"children"` key. Position is `Value::Map { "start": …, "end": … }` under the `"position"` key.

**Why this is not a downgrade vs. a typed enum**:
- jq's `.type == "heading"` selector is identical in length and clarity to a hypothetical `.[] is Heading` Rust-side discriminator.
- JSON Pointer addresses (`/children/0/level`) work with no special-casing.
- Reporters (`console`, `json`, `sarif`, `junit`, `tap`) all serialise `Value::Map` natively; a typed-tree variant would have required parallel serialisation logic.
- The bulk driver's parallel write path is unchanged.
- `comrak::nodes::Ast` → `Value::Map` conversion is one ~150-line function. Mapping into a typed enum and back would have doubled the file.

**Trade-off accepted**: rule authors looking at a markdown AST in `dq query post.md '.'` see slightly more verbose JSON than a typed enum's serde representation would have produced (every node has `"type": "..."` + `"position": {...}`). This is acceptable for an inspection / debugging tool; for human-readable output users use `convert -F markdown` to round-trip back to source text.

**Future-proofing**: if M11 XML or a community plugin needs typed-node performance characteristics the discriminator-Map can't deliver, `dq-core` can introduce a `Tree` variant in `Document` then; existing markdown rules continue working because rule authors interact with the AST through jq / Pointer, both of which stay constant. Concretely: a `Tree` migration would update `Markdown::parse` to return `Document::tree(...)` instead of `Document::with_value(...)`, but the JSON shape rule authors see (the `value()` projection, dispatched on the same field names) stays the same.

### D2: Frontmatter folding into the AST

The M5 `Frontmatter` parser stores the header as `Document::value() = parsed-header-map` and the body as opaque bytes. The M9 `Markdown` parser folds frontmatter into the AST as a top-level node:

```
{
  "type": "document",
  "frontmatter": { "kind": "yaml", "value": { "title": "Hello" } } | null,
  "children": [...],
  "position": { "start": {...}, "end": {...} }
}
```

When no recognised frontmatter delimiter (`---\n`, `+++\n`, `{` + closing `}` + blank line) is found within the first 64 KiB, `frontmatter` is `Value::Null` and `children` starts at byte 0. This matches the M5 fallback behaviour but exposes it in the AST shape.

**Why fold instead of separate**: rule authors writing markdown checks frequently want to dispatch on whether a doc has a frontmatter (`select(.frontmatter)`) or read frontmatter values for cross-checks (e.g., a rule that the `<h1>` text matches `frontmatter.value.title`). Folding makes both natural in jq. Users who specifically want the M5 split shape (`-F frontmatter`) can still get it.

**Why `kind` instead of three sibling keys** (`yaml`/`toml`/`json` directly): `select(.frontmatter.kind == "yaml")` is more idiomatic than `select(.frontmatter.yaml)`, and the discriminator pattern is consistent with how every other AST node identifies its type.

### D3: Default extension dispatch for `.md` flips from `Frontmatter` to `Markdown`

This is the one breaking change in M9. After the change:

| Invocation | Pre-M9 | Post-M9 |
|---|---|---|
| `dq get post.md /title` | reads YAML header `title` | reads top-level AST field `title` (== `Value::String("document")` since the document node has no `title`) |
| `dq get post.md /title -F frontmatter` | reads YAML header `title` | reads YAML header `title` (M5 path explicit) |
| `dq get post.md /frontmatter/value/title` | path not found | reads YAML header `title` (canonical M9 path) |
| `dq paths post.md` | shows header keys | shows AST node tree |
| `dq paths post.md -F frontmatter` | shows header keys | shows header keys (M5 path explicit) |
| `dq lint post.md` (no rules) | no rules match `.md`+frontmatter | `@std/markdown` auto-binds and runs |

**Why we accept this break**: `.md` is the canonical markdown extension; resolving it to a header-only parser was a stop-gap because M5 didn't ship a body parser. M9 closes that. Users who relied on the M5 path retain a one-flag escape hatch (`-F frontmatter`); the new pointer (`/frontmatter/value/...`) is a documented path.

**What we do for users**: CHANGELOG entry calls out the break with the migration recipe (replace `/title` with `/frontmatter/value/title` OR add `-F frontmatter`); README example for the pointer shape.

### D4: comrak crate version + features

`comrak = "0.31"` (latest as of 2026-04). Default features minus the optional `shortcodes` extension (we don't need shortcode rendering). Pinned in `[workspace.dependencies]`. The crate is MIT-licensed and dual-published (matches our `cargo deny` policy). It exposes:
- `parse_document(arena, source, options) -> &AstNode` — the entry point.
- `Options::extension::{ table, strikethrough, autolink, tasklist, footnotes, header_ids, front_matter_delimiter }` — GFM toggles enabled by default for M9.
- `Options::render::{ sourcepos: true }` — required for position tracking.
- `format_commonmark(node, options, &mut String)` — the canonical re-emission path used when the value tree has been mutated.

The `front_matter_delimiter` option is set so comrak emits a `NodeValue::FrontMatter(text)` node at the top of the AST when the document starts with `---\n…\n---\n`. The post-comrak conversion pass parses that text via the existing `Yaml` / `Toml` / `Json` parsers and folds it into the document node's `frontmatter` field per D2. Comrak does not natively support TOML or JSON frontmatter — those are detected by our pre-comrak scanner and stripped from the source before comrak parsing, with the resulting `frontmatter` value attached after.

### D5: Position tracking — block-level only

`comrak` exposes `Sourcepos { start_line, start_column, end_line, end_column }` on every block-level node when `render.sourcepos = true`. Inline nodes inherit only the containing block's position. The AST encoder emits `position: { "start": { "line": Int, "column": Int }, "end": { "line": Int, "column": Int } }` for every block node. `Diagnostic::line` extraction in `dq-exec` does NOT auto-walk this field today — the M8 evaluator strips byte spans from the value adapter and defaults `line=1` per `evaluator.rs:23-28`. Rules that want a real position MUST set `loc.line: ".position.start.line // 1"` (or equivalent) per the existing M8 contract; this is the same pattern that future M9+ position-aware rules in non-markdown formats will use.

Inline-level positions are an M11 refinement. Most useful M9 rules (heading order, code-block lang, frontmatter required fields) operate at block level; the few inline rules (`no-empty-links`, `link-text-not-here`) report at the containing-paragraph level which is acceptable.

### D6: Round-trip strategy — verbatim or canonical

Two cases:

**Unchanged document**: `Format::write` emits `Document::original_bytes()` verbatim. Determined by comparing `Document::value()` to the parsed-from-original baseline (a structural equality check). Identical to how every other `dq-core` parser handles unchanged round-trip.

**Mutated document**: `Format::write` strips the `position` field (canonical re-emission ignores positions) and renders via `comrak::format_commonmark`. The output is **lossy** for:
- Trailing whitespace.
- Fence character (` ``` ` vs. `~~~`).
- Reference-link definition order and placement.
- Indent style (tab vs. space).
- Non-canonical link / image whitespace inside `[…](…)`.

This is documented in the M9 `set`/`del` help text and in the changelog. M9 markdown is **primarily a lint-target format** (read-heavy); textual-edit splicing for markdown is M11+ work.

### D7: Standard ruleset shape — `match.format: [markdown]`

Rules in `crates/dq-lint/rules/markdown/` set `match.format: [markdown]` and write `check.jq` against the AST shape. Examples (pseudo):

- `heading-order.yml` — emit a violation whenever a heading's `level` is more than one greater than the previous heading's level. `check.jq` walks `.children[] | select(.type == "heading")` with a `reduce` accumulator.
- `code-blocks-have-lang.yml` — `check.jq: .children[] | select(.type == "code_block" and (.fenced and (.lang == null or .lang == "")))`.
- `frontmatter-required-fields.yml` — `check.jq: select(.frontmatter == null) // (["title", "date"] - (.frontmatter.value | keys))[] | { field: . }` (rule fires either when frontmatter is absent or when a required field is missing).

The exact list of ≥18 rules with tested fixtures lives in `tasks.md` §3.

## Risks / Trade-offs

### R1: comrak's GFM extension defaults differ from raw CommonMark

We enable `tables`, `strikethrough`, `autolinks`, `tasklist`, `footnotes`, and `header_ids` unconditionally. A user who feeds a strict-CommonMark file in (no GFM features) gets the same parse result as a GFM file — that's expected. A user who relies on `*foo~bar~baz*` parsing as plain text (because GFM strikethrough isn't standard CommonMark) gets `~bar~` parsed as strikethrough. **Mitigation**: documented in `convert -F markdown` and `lint` help text. M11+ may add a `--no-gfm` flag if community feedback warrants.

### R2: Mutated round-trip losing trailing whitespace breaks pre-commit users

Trailing-whitespace-sensitive markdown is rare (it has semantic meaning only for hard-line-break syntax — two trailing spaces on a line). When `dq fmt` (M4) lands on markdown in M10 it will be subject to the canonical re-emission, which strips trailing whitespace silently. **Mitigation**: M9 doesn't surface `dq fmt -F markdown` as a write target unless explicitly opted into via `-F markdown`. The default `dq fmt post.md` parses via `Markdown` for the lint side but writes via... actually `dq fmt` is M4 and re-emits the value unchanged through the parser's writer; for markdown that means `format_commonmark`, which is canonical and lossy. **Decision: M9 ships `Markdown` as a fmt-target with the documented losses**; the alternative (deferring `fmt -F markdown` to M11) is a worse user experience because `dq fmt -i README.md` is a load-bearing pre-commit hook for many projects.

### R3: comrak parser allocations — performance regression risk

`comrak::parse_document` builds an arena-allocated AST tree. Our `Value::Map` conversion clones every node into the IndexMap structure. For a typical 5 KiB markdown file this is sub-millisecond; for an unusually large doc (e.g. 100 KiB tutorial), it's ~5 ms. The bulk driver runs file-level rayon, so per-file latency multiplies linearly. **Mitigation**: no premature optimisation in M9. The performance benchmark in M6 runs a single representative file and tracks regression in CI; if an M9 baseline shows >10× regression vs. yaml on a same-size file, we add a lazy-children path. Otherwise this stays below the noise floor.

### R4: Frontmatter detection ambiguity with documents that legitimately start with `---`

A markdown doc whose first line is a horizontal rule `---` followed by content (not closed by `---` within 64 KiB) falls into the "no frontmatter" branch per the M5 spec — `frontmatter: null`, `children` starts with the `thematic_break` node. Edge case: a doc with `---\n# Title\n---\n…` where the author intended both `---`s as horizontal rules but our parser treats the segment as YAML frontmatter (and `# Title` parses as the YAML scalar `'# Title'`). **Mitigation**: the YAML parser fails on `# Title` as a non-scalar non-mapping non-sequence form, and the frontmatter detector falls through to "no frontmatter" because the inner parser failed. So the AST emits `frontmatter: null` + `children: [thematic_break, heading, thematic_break]`. Acceptable.

### R5: Hidden HTML in AST escapes content sanitization

`html_block` and `html_inline` nodes carry raw HTML in their `value` field. When emitted to JSON or SARIF reporters this is opaque text — no escaping required (JSON encoder handles it). When rendered to console (the default reporter), the raw HTML is printed as text — **never executed**. There is no XSS surface in dq itself; if a downstream tool consumes our SARIF output and renders the raw HTML field as HTML, that's the downstream tool's problem. **Decision: no sanitization in dq.** Documented in design notes.

## Migration Plan

The single behaviour break is the `.md` extension default dispatch (D3). Users who relied on the M5 path get one of two migration recipes:

1. **Add the explicit `-F frontmatter` flag** to existing invocations. Lowest-effort change.
2. **Update the pointer** from `/foo` to `/frontmatter/value/foo`. Cleaner long-term — the user's tooling now operates on the M9 AST shape and gains markdown body queries for free.

CHANGELOG entry calls this out in the M9 alpha release notes with both recipes and a one-paragraph rationale ("M9 promotes markdown to a first-class queryable format; the M5 path is preserved via `-F frontmatter`").

There is no shim, no deprecation period, no feature flag. The break is a known quantity and the escape hatch is already documented.

## Open Questions

- **Should `comrak::Options::render::sourcepos` cost be paid only when needed?** Position tracking is required for `dq-exec` diagnostic emission; it's optional for read-only commands like `dq paths` or `dq query`. Disabling sourcepos saves ~5% parse time. **Decision deferred to post-M9 perf pass** — if benchmarks show it's worth it, `Markdown::parse` can branch on a thread-local "needs-position" flag set by the exec engine. M9 ships sourcepos always-on for simplicity.
- **Should we emit `text` nodes' `value` as a single concatenated string or preserve sibling text segmentation?** comrak's AST splits text by inline-formatting boundaries (`"hello "`, `Strong("world")`, `"!"`). Concatenating loses inline structure; preserving it makes simple text-extraction queries verbose. **Decision: preserve segmentation**. Rule authors who want concatenated text use `[ .children[] | recurse(.children?[]) | select(.type == "text") | .value ] | join("")` — slightly verbose but composable. M11 may add a `text(.)` jq builtin if community asks.
- **Reference-link definitions with no inline use.** A doc that defines `[link]: https://example.com` but never uses `[link]` anywhere has a `link_reference_definition` node but no `link` node. Should `no-broken-relative-links` check the definition or only the use? **Decision: check both**. The M9 implementation visits `link_reference_definition` nodes alongside `link` nodes when validating URLs.
