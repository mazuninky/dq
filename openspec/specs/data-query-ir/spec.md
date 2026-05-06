# data-query-ir Specification

## Purpose
TBD - created by archiving change add-ir-foundation. Update Purpose after archive.
## Requirements
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
    Original { pointer: Pointer, span: Option<ValueSpan> },
    Synthetic { reason: SyntheticReason },
}

pub enum SyntheticReason {
    Constructed,
    Aggregated,
    Computed,
}
```

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

### Requirement: Parsers populate `ProvenanceMap` for span-bearing formats

Every parser in `dq-core::parsers::*` whose underlying format produces a `SpanMap` (currently YAML, JSON, TOML — see [`crates/dq-core/src/textual_edit/mod.rs`](../../../../crates/dq-core/src/textual_edit/mod.rs)) SHALL also populate a `ProvenanceMap` keyed by the same canonical RFC 6901 pointer strings, with each entry recording `Provenance::Original { pointer, span: Some(value_span) }`. Read-only formats without spans (JSONL, HCL, INI, DotEnv, CSV, TSV, Dockerfile, IgnoreList, Frontmatter body, Markdown body) SHALL emit an empty `ProvenanceMap`; `Document::as_ir` for such documents returns an `Ir` whose provenance is empty but whose `format` is set, so callers can distinguish "no provenance available" from "provenance applies elsewhere".

#### Scenario: YAML parse produces provenance for every leaf
- **WHEN** a write-aware YAML parser produces a `Document` for `name: foo\n`
- **THEN** the document's `as_ir().provenance` contains an entry for `/name` whose `Provenance::Original { pointer, span }` matches the document's existing `spans.get("/name")`

#### Scenario: JSONL parse produces empty provenance
- **WHEN** a JSONL parser produces a multi-document `Document`
- **THEN** the document's `as_ir().provenance` is empty AND `as_ir().format == FormatTag::Jsonl`

### Requirement: Provenance propagation contract for read-only transformations

A read-only transformation SHALL preserve `Provenance::Original` for any output node that is **byte-identical** to the input node addressed by some pointer in the input. Output nodes constructed by the transformation (literals in the expression, results of arithmetic, aggregations like `length`/`add`) SHALL carry `Provenance::Synthetic { reason }`. The contract is one-way: a transformation MAY upgrade `Original` to `Synthetic` if the path becomes ambiguous, but it MUST NOT downgrade `Synthetic` to `Original`.

A transformation that cannot statically determine provenance MAY mark every output node as `Synthetic { reason: SyntheticReason::Computed }`; callers consuming the output use this as a signal that span lookup is unavailable for that node.

#### Scenario: Identity transformation preserves all provenance
- **WHEN** a no-op transformation runs against an input `Ir` with N `Original` entries
- **THEN** the output `OwnedIr` contains the same N `Original` entries (same pointers, same spans)

#### Scenario: Constructor produces synthetic
- **WHEN** a transformation produces a literal value not present in the input
- **THEN** the output node's provenance is `Synthetic { reason: SyntheticReason::Constructed }`

#### Scenario: Aggregation produces synthetic
- **WHEN** a transformation computes `length` over an input array
- **THEN** the resulting integer node's provenance is `Synthetic { reason: SyntheticReason::Aggregated }`

### Requirement: Provenance lookup helpers

`Ir<'_>` SHALL expose `pub fn provenance_for(&self, pointer: &Pointer) -> Option<&Provenance>` and `pub fn span_for(&self, pointer: &Pointer) -> Option<&ValueSpan>`. The latter returns `None` for `Synthetic` provenance, for unmapped pointers, and for `Original { span: None }`. Both lookups are O(1) on the underlying `HashMap`.

#### Scenario: Lookup returns span for original node
- **WHEN** `Ir::span_for("/foo")` is called against an `Ir` whose provenance map maps `/foo` to `Original { span: Some(s), ... }`
- **THEN** the result is `Some(&s)`

#### Scenario: Lookup returns None for synthetic node
- **WHEN** `Ir::span_for("/foo")` is called against an `Ir` whose provenance map maps `/foo` to `Synthetic { ... }`
- **THEN** the result is `None`

#### Scenario: Lookup returns None for unmapped pointer
- **WHEN** `Ir::span_for("/missing")` is called against an `Ir` whose provenance map has no entry for `/missing`
- **THEN** the result is `None`

