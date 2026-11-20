# IR, provenance and edit scripts

This page is for contributors working inside `dq-core`, `dq-exec` and
`dq-plugin`. It describes the read-side view the lint engine works on (the
IR, with provenance that maps every node back to source bytes) and the
write-side vocabulary fixes are expressed in (`EditScript`).

## Why an IR

Diagnostics need positions, and composite rules need positions *inside* a
scalar (YAML embedded in a Markdown code block). Fixes need to be applied
without destroying comments. Both are served by keeping, next to the value
tree, a map from every pointer to where its bytes came from — and by
expressing fixes as pointer-addressed edits that go through the same
textual-edit machinery as `dq set`.

## Types (`crates/dq-core/src/ir/mod.rs`)

```rust
pub struct Ir<'a>  { value: &'a Value, provenance: &'a ProvenanceMap,
                     format: FormatTag, bytes: &'a [u8], origin: Option<usize> }
pub struct OwnedIr { value: Value, provenance: ProvenanceMap, format: FormatTag }

pub type ProvenanceMap = IndexMap<String, Provenance>; // canonical RFC 6901 pointer → provenance

pub enum Provenance {
    Original { pointer: Pointer, span: Option<ValueSpan>,
               inline_offset: Option<InlineBaseline> },
    Synthetic { reason: SyntheticReason },
}
pub enum SyntheticReason { Constructed, Aggregated, Computed }

pub struct InlineBaseline { pub byte_start: usize, pub line: u32, pub col: u32 }
```

- `Ir<'_>` is a cheap `Copy` view; `Document::as_ir()` builds it from a
  parsed document without cloning the value.
- The provenance map is keyed by the same canonical pointer strings as the
  textual-edit span map (an `IndexMap` rather than the `HashMap<Pointer, _>`
  first sketched, for deterministic ordering and to share keys with the span
  map).
- `Provenance::original(pointer, span)` defaults `inline_offset` to `None`.
- `OwnedIr` has `to_borrowed()` and `into_parts()`. It is `PartialEq` but not
  `Eq` (floats).

Lookups, all O(1):

- `provenance_for(&ptr)`, `span_for(&ptr)` (None for synthetic or unmapped
  nodes, or an `Original` without span), `inline_offset_for(&ptr)`.
- `line_col_for(&ptr)` resolves a span to a 1-based `(line, col)` in the
  source bytes (`col` counts bytes since the last newline). None when the IR
  has no source bytes.
- `origin_line_col()` is where a diagnostic without a position lands: `(1,
  1)` for a whole file, or, for one document of a YAML stream, the first
  line of that document that holds a spanned value (its `---` line for an
  empty document).

### Which formats carry provenance

YAML, JSON and TOML (the formats with textual edits) populate an `Original`
entry with a span for every node. Markdown block nodes carry positions in the
tree itself (`position`), and the Markdown parser adds a provenance entry
for the `value` leaf of every fenced code block: a span from the first
content line (the line after the opening fence, at the fence's column) to the
end of the block, and an inline offset, so a composite rule anchored at that
leaf projects into it. The Dockerfile parser, read-only, populates
an `Original` entry with a span for every node of every instruction: the
record, `instruction` and `line` at the keyword on the line the instruction
starts on, `arguments` and everything inside it at the first argument on that
line; the root has none. Every other format (JSONL, HCL, INI, dotenv,
CSV/TSV, ignore-list, XML, frontmatter body) has an empty provenance map:
diagnostics on them fall back to line 1. That is why Terraform findings
report line 1 today.

### Inline offsets

For content nested inside a scalar, `inline_offset` records where the
scalar's content starts so composite rules can project inner positions:

- YAML block scalars (`|`, `>`, `|-`, `>-`, with or without an indentation
  indicator): always `Some({byte_start: 0, line: 1, col: 1})`.
- Markdown fenced code blocks: `Some({byte_start: 0, line: 1, col})` with
  `col` the file column the content lines start at — the fence's column, 1
  for a top-level block, more inside a list item or a block quote (comrak
  takes that indentation off every content line).
- JSON strings with `\n` escapes: best effort, may be `None`.
- Everything else: `None`; composite rules then fall back to the anchor's
  span.

`col` counts Unicode scalar values, not bytes, so multi-byte input projects
correctly.

### Streams

`Document::stream_documents()` yields one `StreamDocument` per document of a
YAML stream. Its provenance is rebased so the document's root is `""`: a
rule's `loc.pointer`, a composite `anchor` or a plugin pointer addresses one
document the way it would address a single-document file, while resolved
positions are still positions in the whole file.

### Provenance through transformations

A transformation keeps `Original` for an output node that is byte-identical
to an input node at some pointer, and marks constructed nodes
`Synthetic(Constructed)`, aggregates `Synthetic(Aggregated)`, and anything it
cannot account for `Synthetic(Computed)`. It may turn `Original` into
`Synthetic` when a path becomes ambiguous, never the other way. The jq adapter
marks everything `Computed` (see [jq.md](jq.md)).

## Edit scripts (`crates/dq-core/src/edit_ops/mod.rs`)

```rust
pub enum EditOp {
    Add     { path: Pointer, value: Value },
    Replace { path: Pointer, value: Value },
    Remove  { path: Pointer },
}
pub struct EditScript(Vec<EditOp>); // new, push, ops, len, is_empty, is_noop, under, apply
```

- (De)serialises as an RFC 6902 JSON Patch array restricted to `add`,
  `replace`, `remove`. `copy`, `move` and `test` are rejected by name, and
  unknown fields are rejected, so extending the vocabulary stays a deliberate
  decision. (`dq patch` has its own full `PatchOp` engine in
  `crates/dq-core/src/transform/`.)
- `apply(&mut Document)` runs each op in order through `Document::set_at` /
  `del_at` — the textual-edit path on YAML, JSON and TOML, the rewrite through
  the writer on HCL, XML, INI, dotenv, CSV/TSV and JSON Lines (`Document::edits_by_rewrite`)
  — so comments and layout fare exactly as with `dq set`. A `Replace` built
  from the same pointer and value as a `set_at` produces identical bytes.
- Each op is atomic; the script is not. On failure the document is left
  partially applied; callers that need all-or-nothing clone the document
  first (the fixer does).
- `is_noop()` is true only for an empty script; it does not look at values.
- `under(&parent)` rebases every path below `parent`, turning a script written
  against one stream document (root `""`) into one for the whole stream
  (`/N/...`).

Rule `fix.ops` and plugin fixes produce edit scripts; see
[lint-engine.md](lint-engine.md#fixes) and [../plugin-abi.md](../plugin-abi.md).
