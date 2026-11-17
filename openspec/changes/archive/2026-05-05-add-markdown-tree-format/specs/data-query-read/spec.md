## ADDED Requirements

### Requirement: JSON Pointer addresses through markdown AST

`dq get`, `dq paths`, `dq keys`, `dq values`, `dq len`, `dq type`, and `dq select` SHALL accept JSON Pointer addresses (RFC 6901) that walk the markdown AST node tree produced by the M9 `Markdown` format. Each AST node is a `Value::Map` with a `"type"` discriminator and node-specific fields; navigation uses standard JSON Pointer semantics (slash-separated tokens, `~1` for `/` in keys, `~0` for `~`).

#### Scenario: Pointer to a heading's level

- **WHEN** the user runs `dq get post.md /children/0/level` against a markdown file whose first child is a level-2 heading
- **THEN** stdout is `2` and the exit code is 0

#### Scenario: Pointer to a frontmatter field

- **WHEN** the user runs `dq get post.md /frontmatter/value/title` against a markdown file with YAML frontmatter `title: Hello`
- **THEN** stdout is `Hello` and the exit code is 0

#### Scenario: Pointer to a code block's language

- **WHEN** the user runs `dq get post.md /children/2/lang` against a markdown file whose third top-level child is a fenced code block with info string `yaml`
- **THEN** stdout is `yaml`

#### Scenario: Pointer that walks into inline children

- **WHEN** the user runs `dq get post.md /children/0/children/0/value` against a markdown file whose first heading's first inline child is a text node
- **THEN** stdout is the text content of that text node

#### Scenario: Pointer that doesn't match returns NOT_FOUND

- **WHEN** the user runs `dq get post.md /children/99/level` and the document has fewer than 100 children
- **THEN** the command exits with code 2 (NOT_FOUND) and writes a structured error naming the matched prefix

### Requirement: jq filters dispatch on the AST type discriminator

`dq query` SHALL accept jq expressions that filter on the `.type` field to walk the markdown AST. Recursive walks via `..` and `recurse(.children?[])` SHALL function exactly as for any other `Value::Map` / `Value::Array` shape.

#### Scenario: List every heading's text

- **WHEN** the user runs `dq query post.md '.children[] | select(.type == "heading") | .children[] | select(.type == "text") | .value'`
- **THEN** stdout contains one line per heading text

#### Scenario: Find code blocks without a declared language

- **WHEN** the user runs `dq query post.md '.children[] | select(.type == "code_block" and (.lang == null or .lang == "")) | .position.start.line'`
- **THEN** stdout contains the starting line number of every code block missing an info string

#### Scenario: Cross-reference frontmatter and body

- **WHEN** the user runs `dq query post.md 'select(.frontmatter != null) | .frontmatter.value.title'`
- **THEN** stdout is the frontmatter title for documents that have one, and empty for documents that don't
