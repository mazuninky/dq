## ADDED Requirements

### Requirement: `dq-core::transform` public surface for ops-as-data primitives

`dq-core` SHALL expose a `transform` module re-exporting three engines:

- `pub fn apply_patch(doc: &mut Document, ops: &[PatchOp]) -> Result<()>` — applies an RFC 6902 patch atomically (clone-on-apply: `doc` is left untouched if any op fails).
- `pub fn apply_merge(doc: &mut Document, patch: &Value) -> Result<()>` — applies an RFC 7396 merge patch (recursive, `null` removes, scalars replace).
- `pub fn diff(a: &Value, b: &Value) -> Vec<PatchOp>` — emits a minimal RFC 6902 patch transforming `a` into `b`.

The `PatchOp` enum SHALL have variants `Add`, `Remove`, `Replace`, `Move`, `Copy`, `Test`, each carrying the RFC 6902 path (and value/from where applicable). All three engines SHALL preserve textual round-trip semantics by going through `Document::set_at` / `Document::del_at` for any byte-level mutation — the engines do NOT bypass the textual-edit pipeline.

#### Scenario: apply_patch is atomic on test failure
- **WHEN** an `apply_patch` call applies an op-list whose third op is a failing `test`
- **THEN** the function returns `Err(Error::PatchTestFailed { ... })` and `doc.original_bytes()` is byte-identical to its pre-call value

#### Scenario: diff round-trips
- **WHEN** for any two `Value`s `a` and `b`, the caller computes `ops = diff(&a, &b)` and applies them to a Document carrying `a`
- **THEN** the resulting Document's value is structurally equal to `b`

#### Scenario: apply_merge null removes
- **WHEN** `apply_merge` is called with patch `{"a": null}` against a Document whose top-level map contains `"a"`
- **THEN** the resulting Document has `"a"` removed and every other key preserved
