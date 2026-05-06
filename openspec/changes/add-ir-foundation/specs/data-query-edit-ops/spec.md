# data-query-edit-ops

Capability: словарь edit-операций (`EditOp`/`EditScript`) — JSON Patch (RFC 6902) subset с детерминированным applier через существующие renderer-факторы. Используется fix-engine (per-violation fix), плагинами (WASM-fix), и под капотом — `dq set`/`dq del`.

## ADDED Requirements

### Requirement: `EditOp` and `EditScript` types in `dq-core`

`dq-core` SHALL expose a public `edit_ops` module with at minimum:

```rust
pub enum EditOp {
    Add { path: Pointer, value: Value },
    Replace { path: Pointer, value: Value },
    Remove { path: Pointer },
}

pub struct EditScript(Vec<EditOp>);

impl EditScript {
    pub fn new() -> Self;
    pub fn push(&mut self, op: EditOp);
    pub fn ops(&self) -> &[EditOp];
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;
}
```

`EditOp` SHALL `derive(Debug, Clone, PartialEq)`. `EditScript` SHALL `derive(Debug, Clone, PartialEq, Default)` and implement `IntoIterator<Item = EditOp>` and `FromIterator<EditOp>`.

#### Scenario: `EditScript::new` is empty
- **WHEN** `EditScript::new()` is called
- **THEN** the resulting script has `is_empty() == true` AND `len() == 0`

#### Scenario: Round-trip via `IntoIterator` / `FromIterator`
- **WHEN** an `EditScript` of three ops is collected via `script.into_iter().collect::<EditScript>()`
- **THEN** the collected script has the same `ops()` slice as the original

### Requirement: `EditScript` JSON Patch (de)serialization

`EditScript` SHALL serialize to and deserialize from RFC 6902 JSON Patch via `serde`. Each `EditOp` variant maps to one JSON object with required field `op` (`"add"` | `"replace"` | `"remove"`) and `path` (JSON Pointer string). The `Add` and `Replace` variants additionally carry `value`.

Deserialization SHALL reject unknown ops (`copy`, `move`, `test`) with a structured error (`Error::Format` carrying the unsupported op name) so future support is opt-in. Deserialization SHALL reject unknown fields under `serde(deny_unknown_fields)`.

#### Scenario: Serialize replace op as JSON Patch
- **WHEN** `EditOp::Replace { path: Pointer::parse("/a/b").unwrap(), value: Value::Int(1) }` is serialized via `serde_json::to_value`
- **THEN** the result equals `{"op": "replace", "path": "/a/b", "value": 1}`

#### Scenario: Deserialize JSON Patch array as EditScript
- **WHEN** `serde_json::from_str::<EditScript>(r#"[{"op":"add","path":"/x","value":42},{"op":"remove","path":"/y"}]"#)` is called
- **THEN** the result is `Ok(EditScript)` with two ops in order: `Add` then `Remove`

#### Scenario: Unsupported op fails with structured error
- **WHEN** `serde_json::from_str::<EditScript>(r#"[{"op":"copy","from":"/a","path":"/b"}]"#)` is called
- **THEN** the result is `Err(_)` whose error message identifies `copy` as the unsupported op

### Requirement: `EditScript::apply` mutates a Document via existing renderer-factories

`EditScript::apply(&mut Document) -> Result<(), Error>` SHALL apply each op in declaration order via `Document::set_at` (for `Add`/`Replace`) and `Document::del_at` (for `Remove`). Each individual op SHALL go through the registered `ScalarRenderer` / `InsertionRenderer` for the document's `FormatTag`, preserving comments and surrounding whitespace exactly as `Document::set_at` already does for the M2 baseline.

If any op fails (path not found, type mismatch, write-unavailable for the format), `apply` SHALL return the `Error` from that op, and the document SHALL remain in a partially-applied state — atomicity for multi-op scripts is the responsibility of the caller (see `Fixer::apply_script` in `data-query-exec`, which clones the Document before apply).

#### Scenario: Single Replace updates value and bytes
- **WHEN** `EditScript::from(vec![EditOp::Replace { path: Pointer::parse("/name").unwrap(), value: Value::String("foo".into()) }]).apply(&mut doc)` is called for a YAML document `name: bar\n`
- **THEN** `doc.value()` reflects `name = "foo"` AND `doc.original_bytes()` contains the bytes for `name: foo\n` AND comments outside `/name` are preserved

#### Scenario: Multi-op script applies in order
- **WHEN** an EditScript contains `[Add /x 1, Replace /y 2]` and is applied to a document with `{y: 0}`
- **THEN** the resulting document has `{x: 1, y: 2}` (key order preserves insertion: `x` first because Add ran first)

#### Scenario: Failed op leaves document partially applied
- **WHEN** an EditScript contains `[Replace /existing 1, Replace /missing 2]` and `/missing` does not exist
- **THEN** `apply` returns `Err(Error::Path { ... })` AND `/existing` was successfully replaced before the failure

### Requirement: `EditScript` round-trip through Document::set_at / del_at

For any `Pointer` and `Value` for which `Document::set_at(p, v)` succeeds in the M2 baseline, applying `EditScript::from(vec![EditOp::Replace { path: p, value: v }])` SHALL produce a byte-identical result. The same holds for `Document::del_at(p)` and `EditOp::Remove { path: p }`. This requirement makes `EditScript` a strict superset (vocabulary) of the existing point-mutation primitives.

#### Scenario: set_at and EditScript Replace produce identical bytes
- **WHEN** the same document is mutated via `doc1.set_at(&p, v.clone())` and `doc2` is mutated via `EditScript::from(vec![EditOp::Replace { path: p, value: v }]).apply(&mut doc2)`
- **THEN** `doc1.original_bytes() == doc2.original_bytes()`

### Requirement: `EditScript::is_noop` checks emptiness without applying

`EditScript::is_noop(&self) -> bool` SHALL return `true` iff `self.ops().is_empty()`. The method exists as a separately named helper to make idempotency-checks self-documenting at call sites in `Fixer`. It does not look at semantics (a `Replace` with the same value is not a no-op for this method — runtime byte-equality check is the caller's job).

#### Scenario: Empty script is noop
- **WHEN** `EditScript::new().is_noop()` is called
- **THEN** the result is `true`

#### Scenario: Single-op script is not noop by this definition
- **WHEN** `EditScript::from(vec![EditOp::Remove { path: p }]).is_noop()` is called
- **THEN** the result is `false` regardless of whether `/p` exists in any specific document
