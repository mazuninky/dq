# data-query-transform Specification

## Purpose
TBD - created by archiving change add-transform-layer. Update Purpose after archive.
## Requirements
### Requirement: Embedded jq engine via `dq-transform` crate

The `dq-transform` crate SHALL embed a jq evaluator built on `jaq-core 3.0`, `jaq-std 3.0`, and `jaq-json 2.0` (with the `sync` and `serde` features). The crate exposes `pub struct JqEngine` whose public API is:

- `JqEngine::compile(expression: &str) -> Result<Self, JqError>` — parses + compiles the expression once, panic-free for any input.
- `JqEngine::run(&self, input: &serde_json::Value) -> Result<Vec<serde_json::Value>, JqError>` — evaluates the compiled filter against one input value and materialises the entire output stream.
- `JqEngine` SHALL be `Send + Sync + Clone` so a single compiled engine can be shared across rayon workers in the M3 bulk driver.

Compilation includes the `jaq-core` builtin definitions plus `jaq-std` (covering `map`, `select`, `length`, `sort_by`, `group_by`, `keys`, `values`, `del`, `to_entries`, `from_entries`, regex, format, math, time) plus `jaq-json` (covering `tojson`, `fromjson`).

#### Scenario: Compile and evaluate identity filter
- **WHEN** the caller invokes `JqEngine::compile(".")?.run(&serde_json::json!({"a": 1}))?`
- **THEN** the result is `vec![serde_json::json!({"a": 1})]`

#### Scenario: Compile and evaluate path filter
- **WHEN** the caller invokes `JqEngine::compile(".foo")?.run(&serde_json::json!({"foo": 42}))?`
- **THEN** the result is `vec![serde_json::json!(42)]`

#### Scenario: Compile and evaluate update assignment
- **WHEN** the caller invokes `JqEngine::compile(".count |= . + 1")?.run(&serde_json::json!({"count": 1}))?`
- **THEN** the result is `vec![serde_json::json!({"count": 2})]`

#### Scenario: Multi-output stream materialises in order
- **WHEN** the caller invokes `JqEngine::compile(".[]")?.run(&serde_json::json!([1, 2, 3]))?`
- **THEN** the result is `vec![serde_json::json!(1), serde_json::json!(2), serde_json::json!(3)]`

#### Scenario: Engine is Send + Sync
- **WHEN** the caller wraps `JqEngine` in `Arc` and clones it across threads
- **THEN** the type-check passes (the trait bounds are statically verified by a `fn assert_send_sync<T: Send + Sync>(_: &T) {}` helper in the test suite)

### Requirement: Compile-time errors carry position and snippet

`JqError::Compile { snippet: String, position: usize, message: String }` SHALL be returned for any expression that fails to parse or type-check. The `position` is the byte offset within the original expression where the error was detected (0-based). The `snippet` is a short excerpt of the expression around the offending position (≤ 60 characters, with `...` ellipsis if truncated). The `message` is the diagnostic from `jaq-core`'s loader/compiler.

Errors SHALL be safe to convert to `dq_core::Error::Parse` for routing through the existing CLI exit-code mapper (`PARSE_ERROR = 3`).

#### Scenario: Unterminated update assignment
- **WHEN** the caller invokes `JqEngine::compile(".foo |=")`
- **THEN** the result is `Err(JqError::Compile { … })` with a non-empty `message` and a `snippet` containing the `|=` operator

#### Scenario: Unknown function
- **WHEN** the caller invokes `JqEngine::compile("nonexistent_fn")`
- **THEN** the result is `Err(JqError::Compile { … })` with a `message` mentioning the unknown identifier

### Requirement: Runtime errors carry message but not position

`JqError::Runtime { message: String }` SHALL be returned when a successfully-compiled filter fails during evaluation (e.g. arithmetic on incompatible types, division by zero, function called on the wrong shape). The `message` is the diagnostic from `jaq-core`'s exception type. Position information is NOT included — runtime errors don't have a meaningful "position" in the source expression that maps to a single character offset.

#### Scenario: Type mismatch in arithmetic
- **WHEN** the caller invokes `JqEngine::compile(". + 1")?.run(&serde_json::json!("string"))?`
- **THEN** the result is `Err(JqError::Runtime { message })` with a `message` describing the type error

#### Scenario: Division by zero on null input
- **WHEN** the caller invokes `JqEngine::compile("null / 0")?.run(&serde_json::json!(null))?`
- **THEN** the result is `Err(JqError::Runtime { message })` describing the type error
- **NOTE** The literal `1 / 0` in jaq 3.0 evaluates to IEEE-754 `Infinity`, which the value adapter then rejects with `Err(JqError::Conversion { … })` because JSON cannot carry non-finite numbers — that is a different error path covered implicitly by the value-adapter requirement above. This scenario uses `null / 0` to exercise the runtime-error path unambiguously.

### Requirement: Value adapter between `serde_json::Value` and `jaq_json::Val`

`dq-transform` SHALL expose `pub fn serde_to_val(input: &serde_json::Value) -> Result<jaq_json::Val, JqError>` and `pub fn val_to_serde(val: &jaq_json::Val) -> Result<serde_json::Value, JqError>`. Both functions are total over the JSON-representable subset of their input types and return `JqError::Conversion { message }` for inputs outside that subset (e.g. `Val::BStr` whose bytes are not valid UTF-8 cannot become a `serde_json::Value::String`; `serde_json::Number` values that don't round-trip through `Num` cannot become `Val::Num`).

The conversion SHALL preserve:

- `Null`, `Bool`, integers (including `i64::MAX`-edge values), finite `f64` floats, strings (valid UTF-8).
- Array order.
- Object key insertion order.

Numeric precision: integers and finite floats SHALL round-trip without loss via the `serde_json::Number` representation (the workspace-level `arbitrary_precision` feature on `serde_json` is required for full BigInt round-trip support, which is documented in the conversion functions' rustdoc).

#### Scenario: Round-trip null/bool/int/string
- **WHEN** the caller round-trips `serde_json::json!({"a": null, "b": true, "c": 42, "d": "hello"})` through `serde_to_val` then `val_to_serde`
- **THEN** the result is structurally equal to the input

#### Scenario: Round-trip nested array of objects
- **WHEN** the caller round-trips `serde_json::json!([{"x": 1}, {"x": 2}])`
- **THEN** the result is structurally equal to the input

#### Scenario: Object key order preserved
- **WHEN** the caller round-trips an object whose keys were inserted in `["z", "a", "m"]` order
- **THEN** the resulting `serde_json::Map` keys iterate in `["z", "a", "m"]` order

### Requirement: `embedded-jq` cargo feature gates the implementation

`dq-transform/Cargo.toml` SHALL declare:

```toml
[features]
default = ["embedded-jq"]
embedded-jq = ["dep:jaq-core", "dep:jaq-std", "dep:jaq-json"]
```

The `JqEngine` struct, the `JqError` enum (including a `JqError::FeatureDisabled { hint: &'static str }` variant), and the `serde_to_val` / `val_to_serde` signatures SHALL be available regardless of the feature state. With the feature **enabled**, the methods do real work. With the feature **disabled**, every method returns `JqError::FeatureDisabled { hint: "rebuild with --features embedded-jq" }`.

The crate SHALL build cleanly under both `cargo build -p dq-transform` and `cargo build -p dq-transform --no-default-features`.

#### Scenario: Feature enabled — full functionality
- **WHEN** the crate is compiled with `--features embedded-jq` (the default) and `JqEngine::compile(".")?.run(&serde_json::Value::Null)?` is invoked
- **THEN** the result is `Ok(vec![serde_json::Value::Null])`

#### Scenario: Feature disabled — deterministic error
- **WHEN** the crate is compiled with `--no-default-features` and `JqEngine::compile(".")` is invoked
- **THEN** the result is `Err(JqError::FeatureDisabled { hint: "rebuild with --features embedded-jq" })`

### Requirement: `JqError` exposes `kind_name()` for stable exit-code mapping

`JqError` SHALL expose `pub fn kind_name(&self) -> &'static str` returning one of `"compile"`, `"runtime"`, `"conversion"`, `"feature_disabled"`. This mirrors the `dq_core::Error::kind_name()` contract and is used by callers that want a stable string for error categorisation (e.g. JSON output formats, log-aggregation rules).

#### Scenario: Compile error reports kind "compile"
- **WHEN** the caller checks `JqError::Compile { … }.kind_name()`
- **THEN** the value is `"compile"`

#### Scenario: Feature-disabled error reports kind "feature_disabled"
- **WHEN** the caller checks `JqError::FeatureDisabled { … }.kind_name()`
- **THEN** the value is `"feature_disabled"`

