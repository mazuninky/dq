# data-query-transform — delta for add-ir-foundation

## MODIFIED Requirements

### Requirement: Value adapter between `serde_json::Value` and `jaq_json::Val`

`dq-transform` SHALL expose `pub fn serde_to_val(input: &serde_json::Value) -> Result<jaq_json::Val, JqError>` and `pub fn val_to_serde(val: &jaq_json::Val) -> Result<serde_json::Value, JqError>`. Both functions are total over the JSON-representable subset of their input types and return `JqError::Conversion { message }` for inputs outside that subset (e.g. `Val::BStr` whose bytes are not valid UTF-8 cannot become a `serde_json::Value::String`; `serde_json::Number` values that don't round-trip through `Num` cannot become `Val::Num`).

The conversion SHALL preserve:

- `Null`, `Bool`, integers (including `i64::MAX`-edge values), finite `f64` floats, strings (valid UTF-8).
- Array order.
- Object key insertion order.

Numeric precision: integers and finite floats SHALL round-trip without loss via the `serde_json::Number` representation (the workspace-level `arbitrary_precision` feature on `serde_json` is required for full BigInt round-trip support, which is documented in the conversion functions' rustdoc).

In addition, `dq-transform` SHALL expose `pub fn ir_to_val(input: &Ir<'_>) -> Result<jaq_json::Val, JqError>` and `pub fn val_to_owned_ir(val: &jaq_json::Val, format: FormatTag) -> Result<OwnedIr, JqError>`. The IR-aware variants SHALL behave identically to the `serde_json::Value` variants for value contents, AND additionally:

- `ir_to_val` accepts the input `Ir`'s `ProvenanceMap` and (in v0.1 of this capability) discards it, since jaq's `Val` cannot carry provenance directly. This is a documented limitation; provenance is recovered by callers via separate `Ir::span_for`/`provenance_for` lookups against the **input** `Ir` keyed by RFC 6901 pointer strings emitted by the jq expression itself (see `data-query-exec`'s `loc.pointer` requirement).
- `val_to_owned_ir` produces an `OwnedIr` whose `ProvenanceMap` marks every value node as `Provenance::Synthetic { reason: SyntheticReason::Computed }` by default. Callers that need pointer-based attribution opt in by emitting pointer-tagged shapes (`[pointer, value]` pairs) from their jq expressions and reconstructing provenance host-side.

The existing `serde_to_val` / `val_to_serde` SHALL remain unchanged in signature and behaviour for backwards compatibility with `dq query`, `dq set --jq`, and other callers that don't need provenance.

#### Scenario: Round-trip null/bool/int/string
- **WHEN** the caller round-trips `serde_json::json!({"a": null, "b": true, "c": 42, "d": "hello"})` through `serde_to_val` then `val_to_serde`
- **THEN** the result is structurally equal to the input

#### Scenario: Round-trip nested array of objects
- **WHEN** the caller round-trips `serde_json::json!([{"x": 1}, {"x": 2}])`
- **THEN** the result is structurally equal to the input

#### Scenario: Object key order preserved
- **WHEN** the caller round-trips an object whose keys were inserted in `["z", "a", "m"]` order
- **THEN** the resulting `serde_json::Map` keys iterate in `["z", "a", "m"]` order

#### Scenario: `ir_to_val` discards provenance but preserves value
- **WHEN** an `Ir<'_>` carrying `value = {"a": 1}` and a non-empty `ProvenanceMap` is converted via `ir_to_val` and back via `val_to_owned_ir`
- **THEN** the resulting `OwnedIr.value` is structurally equal to the input value AND every entry in `OwnedIr.provenance` is `Provenance::Synthetic { reason: SyntheticReason::Computed }`

#### Scenario: `val_to_owned_ir` carries the format tag through
- **WHEN** `val_to_owned_ir(&val, FormatTag::Yaml)` is called against any input
- **THEN** the resulting `OwnedIr.format == FormatTag::Yaml`
