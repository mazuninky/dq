## 1. Workspace dependency

- [ ] 1.1 Add `comrak = "0.31"` to `[workspace.dependencies]` in the root `Cargo.toml` with a comment explaining M9 scope and the disabled `shortcodes` feature flag. Verify `cargo deny check` passes (MIT/Apache-2.0 dual-license, no new policy entries needed). Delegate to `rust-cli-writer`.

## 2. dq-core — new `Markdown` format

- [ ] 2.1 Add `FormatTag::Markdown` variant in `crates/dq-core/src/document/mod.rs`. Update `from_name`, `name`, and any `Display` impls. Update unit tests in the same file to cover the new variant. Delegate to `rust-cli-writer`.
- [ ] 2.2 Add `comrak` dependency to `crates/dq-core/Cargo.toml` with `default-features = false` plus the explicit feature set we need (`tables`, `strikethrough`, `autolinks`, `tasklist`, `footnotes`, `header_ids`, `front_matter_delimiter`). Verify it compiles with `cargo build -p dq-core`. Delegate to `rust-cli-writer`.
- [ ] 2.3 Implement `crates/dq-core/src/parsers/markdown.rs`:
  - `pub struct Markdown;`
  - `impl Format for Markdown` with `name()`, `extensions()`, `parse()`, `write()`, `write_with_options()`.
  - Pre-comrak frontmatter scanner that detects YAML / TOML / JSON delimiters per the M5 `Frontmatter` parser's logic (re-use the helper functions if they're public; if not, vendor them with a comment pointing at `frontmatter.rs`).
  - `comrak::parse_document` invocation with `Options { extension: { tables: true, strikethrough: true, autolinks: true, tasklist: true, footnotes: true, header_ids: Some("".to_string()), front_matter_delimiter: Some("---".to_string()), .. }, render: { sourcepos: true, .. }, .. }`.
  - `convert_ast(node: &AstNode) -> Value` walking the comrak AST and producing `Value::Map` per the spec field schema (D1, D2, D5).
  - `write` path: if `Document::value()` matches the parsed-from-original baseline, emit `original_bytes` verbatim; otherwise re-render via `comrak::format_commonmark` (D6). The structural-equality check uses the existing `Document::is_baseline` helper if one exists, else compares `Document::value()` to a stored canonical clone made at parse time.
  - `#[cfg(test)] mod tests` covering: simple paragraph, all six heading levels, fenced + unfenced code blocks, links + reference-link definitions, lists (ordered / unordered / nested), tables, task list items, frontmatter (yaml / toml / json / none), GFM strikethrough, autolinks, position tracking on a multi-paragraph doc, verbatim re-emission round-trip on five representative fixtures.
  - Total ≥15 unit tests.
  Delegate to `rust-cli-writer`.
- [ ] 2.4 Update `crates/dq-core/src/parsers/mod.rs`: `pub mod markdown;`, `pub use markdown::Markdown;`, registry entry **before** `&Frontmatter`. Verify that `parsers::detect(Utf8Path::new("post.md"))` returns `Some(&Markdown)` and `parsers::by_name("frontmatter")` still returns `Some(&Frontmatter)`. Delegate to `rust-cli-writer`.
- [ ] 2.5 Add component tests in `crates/dq-core/tests/parse_markdown.rs`:
  - Hugo-style post (YAML frontmatter + body with headings, code blocks, links).
  - Obsidian-style note (no frontmatter, lots of inline formatting + tables).
  - GFM-heavy fixture (task lists, footnotes, autolinks, strikethrough).
  - README-style fixture (multi-level headings, fenced code blocks with various langs).
  - Edge: file starting with `---\n` that is NOT frontmatter (closing `---` past 64 KiB → `frontmatter: null`, body starts at byte 0).
  Each fixture asserts the AST shape via insta snapshot AND verifies a `Markdown::write(parsed) == bytes` round-trip.
  Delegate to `rust-cli-test-writer`.

## 3. dq-cli — wire markdown into commands and reporters

- [ ] 3.1 Add `OutputFormat::Markdown` variant in `crates/dq-cli/src/output/mod.rs`. Update the clap `ValueEnum` derivation, the factory in `lib.rs` (or wherever the `OutputFormat → &dyn Reporter` mapping lives), and any docstrings. Delegate to `rust-cli-writer`.
- [ ] 3.2 Verify (no code changes expected) that `dq get`, `dq paths`, `dq keys`, `dq values`, `dq len`, `dq type`, `dq query`, `dq lint`, `dq check`, `dq explain`, `dq rules list`, `dq convert`, `dq validate` all dispatch markdown correctly via the format registry alone — no per-command branching. Delegate to `rust-cli-writer` with instruction to add only smoke-level integration tests where coverage is missing.
- [ ] 3.3 Add CLI integration tests in `crates/dq-cli/tests/cli_markdown.rs`:
  - `dq get post.md /frontmatter/value/title` returns the YAML title.
  - `dq paths post.md` lists AST node pointers (`/`, `/type`, `/frontmatter`, `/children`, `/children/0`, …).
  - `dq query post.md '.children[] | select(.type == "heading") | .level'` enumerates heading levels.
  - `dq query post.md '.children[] | select(.type == "code_block") | .lang'` enumerates code-block languages.
  - `dq convert post.md -F json` emits the AST as JSON.
  - `dq convert post.md -F markdown` round-trips verbatim for unmutated input.
  - `dq lint --rules @std/markdown post.md -F sarif` produces a SARIF v2.1.0 envelope with a non-empty `runs[0].results` for a fixture with violations.
  - `dq lint post.md` (no `--rules`) auto-binds `@std/markdown`.
  - Exit code 4 on `error`-severity violations.
  - Format-detection: `dq get post.md /title -F frontmatter` still works (M5 path explicit).
  Delegate to `rust-cli-test-writer`.

## 4. dq-lint — `@std/markdown` ruleset

- [ ] 4.1 Create `crates/dq-lint/rules/markdown/` directory with the rule files below. Each rule is a YAML file plus a colocated `*.test.yml` fixture. Use the same shape as the existing `@std/k8s` rules (see `crates/dq-lint/rules/k8s/` for reference). Delegate to `rust-cli-writer` (rule authoring) — note: rule files are *.yml, not *.rs, but they're embedded into the binary via `include_str!` so they count as code-bearing changes per the rust-delegation rule.
  - `heading-order.yml` (`error`) — heading levels can't increase by more than 1.
  - `single-h1.yml` (`error`) — exactly one top-level `#` per doc.
  - `no-trailing-whitespace.yml` (`warn`) — no trailing whitespace in paragraph lines.
  - `no-multiple-blank-lines.yml` (`warn`) — at most one blank line between blocks.
  - `no-empty-paragraphs.yml` (`warn`) — no paragraphs with zero text content.
  - `final-newline.yml` (`warn`) — file ends with `\n`.
  - `no-empty-links.yml` (`error`) — every `link` node has non-empty inline text.
  - `no-empty-headings.yml` (`error`) — every `heading` node has non-empty inline text.
  - `no-broken-relative-links.yml` (`warn`) — relative `link.url` that doesn't end in `#fragment` must point at an existing file relative to the document path. Best-effort: file-existence check via the loader's working directory; rule emits a `warn` if the file is missing.
  - `no-bare-urls.yml` (`warn`) — autolink-style `<https://…>` ok; bare `https://…` in plain text not ok.
  - `code-blocks-have-lang.yml` (`warn`) — every fenced code block declares an info string.
  - `link-text-not-here.yml` (`info`) — link text matching `/^(click here|this page|this link|here)$/i` is informational.
  - `no-duplicate-headings.yml` (`warn`) — headings with identical text within the same H2 section are warned.
  - `frontmatter-required-fields.yml` (`error`) — configurable list (default `["title", "date"]`) — every named field must be present in `frontmatter.value` when `frontmatter` is non-null.
  - `frontmatter-date-format.yml` (`warn`) — `frontmatter.value.date` (when present) must match RFC 3339 / ISO 8601.
  - `table-pipes-aligned.yml` (`warn`) — every `table_row` in a `table` has the same cell count.
  - `table-header-required.yml` (`warn`) — every `table` has a header row (first `table_row` with `header: true`).
  - `no-inline-html.yml` (`info`) — informational; flags `html_block` and `html_inline` occurrences.
  Each rule file MUST include `id`, `description`, `severity`, `match.format: [markdown]`, `check.jq`, `check.message`, optional `loc`, `references` array. Each test file MUST cover at least one passing fixture and one failing fixture.
  Delegate to `rust-cli-writer`.
- [ ] 4.2 Update `crates/dq-lint/src/embed.rs` (or whatever file the M8 `include_str!` mechanism lives in) to register the new `markdown` namespace. Verify that `dq_lint::std_ruleset("@std/markdown")` returns the embedded YAML and `dq_lint::list_std_rulesets()` includes `"@std/markdown"` alphabetically. Delegate to `rust-cli-writer`.
- [ ] 4.3 Run `dq test @std/markdown` (or the equivalent in-process test) to verify all ≥18 rules pass their fixtures. Delegate to `rust-cli-test-writer`.

## 5. Docs and meta

- [ ] 5.1 Update `dq-plan.md` M9 section header with `✅ Implemented YYYY-MM-DD (см. [openspec/changes/archive/YYYY-MM-DD-add-markdown-tree-format/](openspec/changes/archive/YYYY-MM-DD-add-markdown-tree-format/))`. Replace the "AST-селекторный язык для markdown: research в M9" line in `## Открытые вопросы` with the resolution: `"AST-селекторный язык для markdown: closed by M9 — JSON Pointer + jq, no new selector DSL (см. add-markdown-tree-format design D1)"`. Direct edit OK (markdown).
- [ ] 5.2 Update `README.md`: status line moves from `M8 alpha` to `M9 alpha — adds markdown AST + @std/markdown`. Add one example each for `dq get post.md /frontmatter/value/title` and `dq lint docs/**/*.md`. Direct edit OK.
- [ ] 5.3 Update `CHANGELOG.md` (or the existing release notes file): M9 entry with the breaking change for `.md` extension default dispatch, both migration recipes, and the new `@std/markdown` rule count. Direct edit OK.

## 6. Validation

- [ ] 6.1 Run `cargo test --workspace --all-features` — must be green. Verify cold runtime ≤30 s.
- [ ] 6.2 Run `cargo clippy --all-features -- -D warnings` — must be clean.
- [ ] 6.3 Run `cargo fmt --check` — must be clean.
- [ ] 6.4 Manual smoke: `cargo run -- lint docs/**/*.md` against this repo's `docs/` directory. Verify `@std/markdown` finds the expected violations on the M5 fixture archive.
- [ ] 6.5 Manual smoke: `cargo run -- get dq-plan.md /frontmatter` returns `null` (the plan has no frontmatter).
- [ ] 6.6 Manual smoke: `cargo run -- query dq-plan.md '.children[] | select(.type == "heading" and .level == 2) | .text'` enumerates all H2 headings in the plan.

## 7. Archive

- [ ] 7.1 Move this change folder from `openspec/changes/add-markdown-tree-format/` to `openspec/changes/archive/YYYY-MM-DD-add-markdown-tree-format/` after CI is green. Direct edit / `mv` OK.
- [ ] 7.2 Apply spec deltas: copy each `specs/<capability>/spec.md` from this change into `openspec/specs/<capability>/spec.md`, merging requirements (additions append, modifications replace, removals delete the requirement section).
