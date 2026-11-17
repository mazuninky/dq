## MODIFIED Requirements

### Requirement: `dq-core::ir` module exposes `Ir` / `OwnedIr` / `Provenance` types

`dq-core` SHALL expose a public `ir` module with at minimum:

```rust
pub struct Ir<'a> {
    value: &'a Value,
    provenance: &'a ProvenanceMap,
    format: FormatTag,
}

pub struct OwnedIr {
    value: Value,
    provenance: ProvenanceMap,
    format: FormatTag,
}

pub type ProvenanceMap = HashMap<Pointer, Provenance>;

pub enum Provenance {
    Original {
        pointer: Pointer,
        span: Option<ValueSpan>,
        inline_offset: Option<InlineBaseline>,
    },
    Synthetic { reason: SyntheticReason },
}

pub struct InlineBaseline {
    /// Byte offset within the parent scalar's content where this node's content begins (0-based).
    pub byte_start: usize,
    /// Line number within the parent scalar's content (1-based).
    pub line: u32,
    /// Column number within the parent scalar's content (1-based).
    pub col: u32,
}

pub enum SyntheticReason {
    Constructed,
    Aggregated,
    Computed,
}
```

`Provenance::Original` SHALL include the `inline_offset` field for nodes whose content lies inside a multiline parent scalar (see the dedicated requirement on inline-offset population). The field is `Option<InlineBaseline>`; `None` means inline-offset information is unavailable for the node and callers SHALL fall back to span-only positions. A constructor `Provenance::original(pointer, span)` SHALL be provided that defaults `inline_offset` to `None` so existing callsites migrate via find/replace without specifying the new field.

`Ir<'a>` SHALL be `Copy` and trivially constructible from a borrowed `(Value, ProvenanceMap, FormatTag)` triple. `OwnedIr` SHALL provide `to_borrowed(&self) -> Ir<'_>` and `into_parts(self) -> (Value, ProvenanceMap, FormatTag)`. The existing `Document` type SHALL gain a method `Document::as_ir(&self) -> Ir<'_>` that builds the borrowed view from its existing `value`, `spans`, and `format` fields without allocating.

#### Scenario: `Document::as_ir` is zero-copy
- **WHEN** a parsed `Document` exposes `as_ir()`
- **THEN** the returned `Ir<'_>` borrows `&Document.value` and a `ProvenanceMap` derived from the document's existing `SpanMap` without cloning either

#### Scenario: `OwnedIr::into_parts` round-trips
- **WHEN** an `OwnedIr` is constructed from `(Value, ProvenanceMap, FormatTag)` and `into_parts()` is called
- **THEN** the returned triple is structurally equal to the construction inputs (value PartialEq, provenance map equality, format tag equality)

#### Scenario: `Ir<'_>` is `Copy`
- **WHEN** `fn assert_copy<T: Copy>(_: T) {}` is called with `Ir<'_>`
- **THEN** the type-check passes

#### Scenario: `Provenance::original` defaults `inline_offset` to `None`
- **WHEN** the helper `Provenance::original(pointer, span)` is invoked
- **THEN** the returned variant is `Original { pointer, span, inline_offset: None }`

## ADDED Requirements

### Requirement: Parsers populate `inline_offset` for multiline parent scalars

Parsers whose input format encodes nested content inside multiline scalars SHALL populate `inline_offset` on the parent scalar's `Provenance::Original` entry so that composite-rule evaluation (see `data-query-composite-rules`) can map inner-document line/column back to source-file coordinates with sub-line precision.

The contract per format:

- **YAML block scalars** (`|`, `>`, `|-`, `>-`, including with explicit indentation indicator): the parser SHALL emit `inline_offset = Some(InlineBaseline { byte_start: 0, line: 1, col: 1 })` for every block-scalar leaf, signalling that the scalar's body starts at line 1 col 1 of its content. **Mandatory.**
- **Markdown fenced code blocks**: the parser SHALL emit `inline_offset = Some(InlineBaseline { byte_start: 0, line: 1, col: 1 })` for every fenced code block leaf. **Mandatory.**
- **JSON strings containing `\n` escapes**: best-effort. When the string is consumed as the `value` of a composite-rule `extract`, the parser SHOULD provide a `Some(InlineBaseline { ... })` describing the unescaped content baseline, but it MAY emit `None` and rely on the anchor-only fallback documented in `data-query-composite-rules`.
- **All other parsers** (TOML, JSONL, HCL, INI, .env, CSV/TSV, Dockerfile, IgnoreList): `inline_offset = None` for every node. Composite-rules still work; the projection simply lacks sub-line precision.

#### Scenario: YAML block scalar carries inline-offset
- **GIVEN** a YAML document containing a block scalar at `/script` with body `"echo 1\necho 2\n"`
- **WHEN** the parser produces the `Document` and `Document::as_ir()` is called
- **THEN** `provenance_for("/script")` returns `Original { ..., inline_offset: Some(InlineBaseline { byte_start: 0, line: 1, col: 1 }) }`

#### Scenario: Markdown fenced code block carries inline-offset
- **GIVEN** a markdown document with a fenced code block at `/0/code/2` containing `"x: 1\ny: 2\n"`
- **WHEN** `Document::as_ir().provenance_for("/0/code/2")` is called
- **THEN** the result includes `inline_offset = Some(InlineBaseline { byte_start: 0, line: 1, col: 1 })`

#### Scenario: TOML scalar carries no inline-offset
- **WHEN** a TOML document is parsed and `Document::as_ir()` is called
- **THEN** every `Provenance::Original` entry's `inline_offset` is `None`

#### Scenario: Existing read-only commands ignore `inline_offset`
- **WHEN** any read command (`get`, `paths`, `keys`, `values`, `len`, `type`, `select`, `validate`) runs on a YAML document containing block scalars before and after this change
- **THEN** stdout is byte-identical and exit code is identical

### Requirement: Inline-offset lookup helper

`Ir<'_>` SHALL expose `pub fn inline_offset_for(&self, pointer: &Pointer) -> Option<&InlineBaseline>` returning the `InlineBaseline` of the node's `Provenance::Original` entry when present, or `None` for `Synthetic` provenance, unmapped pointers, and `Original { inline_offset: None }`. The lookup is O(1).

#### Scenario: Lookup returns inline-offset
- **GIVEN** an `Ir` whose provenance map maps `/script` to `Original { ..., inline_offset: Some(b) }`
- **WHEN** `Ir::inline_offset_for("/script")` is called
- **THEN** the result is `Some(&b)`

#### Scenario: Lookup returns None when offset absent
- **GIVEN** an `Ir` whose provenance map maps `/x` to `Original { ..., inline_offset: None }`
- **WHEN** `Ir::inline_offset_for("/x")` is called
- **THEN** the result is `None`
