## ADDED Requirements

### Requirement: `@std/markdown` standard ruleset

`dq-lint` SHALL ship a `@std/markdown` namespace with at least 18 rules covering CommonMark + GFM markdown. Each rule is a YAML file under `crates/dq-lint/rules/markdown/{rule-name}.yml` with a colocated `{rule-name}.test.yml` fixture. The namespace is embedded into the binary via the same `include_str!` mechanism used by `@std/k8s`, `@std/dockerfile`, `@std/npm`, and `@std/github-actions`.

The auto-bind behaviour of `dq lint <files>` (no `--rules` flag) SHALL include `@std/markdown` whenever the file list contains at least one `.md` / `.markdown` file or a file detected as `markdown` via `-F`.

The initial rule inventory:

| Rule ID | Severity | Concern |
|---|---|---|
| `markdown.heading-order` | error | Heading levels can't increase by more than 1 |
| `markdown.single-h1` | error | Exactly one top-level `#` per document |
| `markdown.no-trailing-whitespace` | warn | No trailing whitespace in paragraph text |
| `markdown.no-multiple-blank-lines` | warn | At most one blank line between blocks |
| `markdown.no-empty-paragraphs` | warn | No paragraphs with zero text content |
| `markdown.final-newline` | warn | File ends with `\n` |
| `markdown.no-empty-links` | error | Every link node has non-empty inline text |
| `markdown.no-empty-headings` | error | Every heading node has non-empty inline text |
| `markdown.no-broken-relative-links` | warn | Relative link URLs (best-effort) point at existing files |
| `markdown.no-bare-urls` | warn | Bare URLs in plain text are flagged; autolink syntax is fine |
| `markdown.code-blocks-have-lang` | warn | Every fenced code block declares an info string |
| `markdown.link-text-not-here` | info | Link text matching `/^(click here\|this page\|this link\|here)$/i` |
| `markdown.no-duplicate-headings` | warn | Headings with identical text within the same H2 section |
| `markdown.frontmatter-required-fields` | error | Configurable list (default `title`, `date`) must be present |
| `markdown.frontmatter-date-format` | warn | `frontmatter.value.date` must match RFC 3339 / ISO 8601 |
| `markdown.table-pipes-aligned` | warn | Every `table_row` has the same cell count |
| `markdown.table-header-required` | warn | Every `table` has a header row |
| `markdown.no-inline-html` | info | Flags `html_block` and `html_inline` occurrences |

#### Scenario: `dq lint post.md` auto-binds `@std/markdown`

- **WHEN** the user runs `dq lint post.md` with no `--rules` flag against a markdown file
- **THEN** the loader resolves `@std/markdown` automatically and runs every rule in that ruleset whose `match.format` includes `markdown`

#### Scenario: `dq test @std/markdown` is green

- **WHEN** the user runs `dq test @std/markdown`
- **THEN** every `*.test.yml` fixture under `crates/dq-lint/rules/markdown/` passes; exit code is 0

#### Scenario: `dq explain markdown.heading-order` shows the rule

- **WHEN** the user runs `dq explain markdown.heading-order`
- **THEN** stdout includes the rule's description, severity, and references list

#### Scenario: SARIF output for a markdown lint run

- **WHEN** the user runs `dq lint --rules @std/markdown post.md -F sarif` against a markdown file with at least one `error`-severity violation
- **THEN** stdout is valid SARIF v2.1.0 with `runs[0].results` containing one entry per violation; exit code is 4

#### Scenario: Frontmatter-required-fields fires on missing field

- **WHEN** the user runs `dq check markdown.frontmatter-required-fields post.md` against a file whose YAML frontmatter is missing `title`
- **THEN** the command exits with code 4 and the diagnostic message names the missing field

#### Scenario: Frontmatter-required-fields passes when the file has no frontmatter at all

- **WHEN** the user runs `dq check markdown.frontmatter-required-fields plain.md` against a file with no frontmatter
- **THEN** the rule does NOT fire (rule applicability requires `frontmatter != null`); exit code is 0

#### Scenario: Heading-order detects a level skip

- **WHEN** the user runs `dq check markdown.heading-order doc.md` against a file with `# H1` followed by `### H3` (skipping H2)
- **THEN** the command exits with code 4 and the diagnostic names the skipped level
