# format-support Specification (M4 delta — add WriteOptions)

## ADDED Requirements

### Requirement: `WriteOptions` public struct in `dq-core`

`crates/dq-core/src/write_options.rs` SHALL define a public struct:

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WriteOptions {
    pub sort_keys: bool,
    pub indent: Option<u8>,
}
```

The struct SHALL be re-exported from `dq_core` crate root as `pub use write_options::WriteOptions;`. It SHALL be `#[non_exhaustive]` so M5+ can add fields (`quote_style`, `flow_style`, `strip_comments`) without breaking consumers — callers MUST construct via `WriteOptions { sort_keys: true, ..Default::default() }` rather than positionally. `Default` SHALL produce `WriteOptions { sort_keys: false, indent: None }` — the no-op identity that produces byte-identical output to today's writers.

#### Scenario: Default is the no-op identity
- **WHEN** a writer is invoked via `format.write_with_options(&doc, &mut buf, &WriteOptions::default())`
- **THEN** the output bytes are identical to `format.write(&doc, &mut buf)`

#### Scenario: WriteOptions is non_exhaustive
- **WHEN** application code constructs `WriteOptions { sort_keys: true, indent: Some(4) }` positionally without `..Default::default()`
- **THEN** the compiler rejects it with the `non-exhaustive` lint and the user is forced to use struct-update syntax

### Requirement: `Format::write_with_options` trait method

The `Format` trait in `crates/dq-core/src/format.rs` SHALL gain a new method:

```rust
fn write_with_options(
    &self,
    doc: &Document,
    w: &mut dyn Write,
    opts: &WriteOptions,
) -> Result<()> {
    let _ = opts;
    self.write(doc, w)
}
```

The default implementation forwards to `write` so existing format implementations keep working unchanged. JSON, JSONL, YAML, and TOML SHALL override the default to honor the relevant `WriteOptions` fields per the table below:

| Format | `sort_keys` | `indent` |
|---|---|---|
| JSON | yes | yes |
| JSONL | yes | yes (per-line) |
| YAML | yes | no (deferred — `serde_yml` does not expose indent) |
| TOML | yes | no (grammar-fixed) |

#### Scenario: JSON honors sort_keys + indent
- **WHEN** a caller invokes `Json.write_with_options(&doc, &mut buf, &WriteOptions { sort_keys: true, indent: Some(4) })`
- **THEN** `buf` contains 4-space-indented JSON with map keys in alphabetical order

#### Scenario: TOML honors sort_keys but ignores indent
- **WHEN** a caller invokes `Toml.write_with_options(&doc, &mut buf, &WriteOptions { sort_keys: true, indent: Some(4) })`
- **THEN** `buf` contains TOML with table keys sorted alphabetically; the `indent` field has no effect

#### Scenario: YAML default behaviour preserved with WriteOptions::default()
- **WHEN** a caller invokes `Yaml.write_with_options(&doc, &mut buf, &WriteOptions::default())`
- **THEN** `buf` is byte-identical to `Yaml.write(&doc, &mut buf)`

### Requirement: `dq_core::canonicalize_keys` helper

`crates/dq-core/src/write_options.rs` SHALL define a public free function:

```rust
pub fn canonicalize_keys(value: &Value) -> Value;
```

The function SHALL return a deep clone of `value` with `Value::Map(IndexMap)` keys sorted alphabetically (case-sensitive byte order, ASCII < Unicode). Arrays SHALL be walked recursively (their elements canonicalized). Scalar variants (`Null`, `Bool`, `Int`, `BigInt`, `Float`, `BigFloat`, `String`) SHALL be returned unchanged. The function SHALL be deterministic and idempotent: `canonicalize_keys(canonicalize_keys(v)) == canonicalize_keys(v)`.

#### Scenario: Map keys sort alphabetically
- **WHEN** the caller invokes `canonicalize_keys` on `{ z: 1, a: 2, m: 3 }`
- **THEN** the returned `Value::Map` has keys in order `["a", "m", "z"]`

#### Scenario: Nested maps inside arrays are canonicalized
- **WHEN** the caller invokes `canonicalize_keys` on `[{z: 1, a: 2}, {y: 3, b: 4}]`
- **THEN** the returned array contains `[{a: 2, z: 1}, {b: 4, y: 3}]`

#### Scenario: Idempotence
- **WHEN** the caller invokes `canonicalize_keys(canonicalize_keys(v))` for any value `v`
- **THEN** the result is `Value`-equal to `canonicalize_keys(v)` and the order is identical

## MODIFIED Requirements

### Requirement: M2 dependency boundary updates

`crates/dq-core/Cargo.toml` SHALL retain the M3 dependency set: `serde_yml`, `serde_json` (with `preserve_order` + `arbitrary_precision`), `toml_edit` (with `preserve-order` + `parse`), `saphyr-parser`, `regex`, `tempfile`, `similar`. M4 SHALL NOT introduce any new runtime dependencies — `WriteOptions` and `canonicalize_keys` are pure-Rust stdlib code. The `serde_json` `PrettyFormatter::with_indent` API used by `--indent` is already vendored.

#### Scenario: No new dependencies in M4
- **WHEN** `cargo deny check` runs after the M4 change is applied
- **THEN** zero new entries appear in `Cargo.lock` compared to the M3 baseline (excluding patch-level updates)
