# data-query-exec — delta for add-ir-foundation

## MODIFIED Requirements

### Requirement: Rule evaluation pipeline

`Evaluator::evaluate_file(&self, path: &Utf8Path, ir: &Ir<'_>, format_name: &str) -> Vec<Diagnostic>` SHALL apply each rule in declaration order with the following pipeline:

1. **Format match.** If `format_name` is not in the rule's `match.format` list, skip.
2. **Glob match.** If `match.glob` is set and does not match the file's relative path (using `globset`), skip.
3. **Filter match.** If `match.filter` is set, evaluate it against the document. If the result stream is empty, or the first result is `false` / `null`, skip.
4. **Check eval.** Evaluate `check.jq` against the document. Each output value is one violation.
5. **Diagnostic build.** For each violation, render `check.message` via the templater against the violation value, derive location from `loc.pointer` / `loc.file` / `loc.line` (jq expressions evaluated against the violation; see `Location override via loc:` requirement for precedence), and emit a `Diagnostic`.

The signature SHALL change from `(&self, path, value: &serde_json::Value, format_name: &str)` to `(&self, path, ir: &Ir<'_>, format_name: &str)` — **BREAKING** at the Rust API level. Internally the evaluator MAY still call `ir_to_val` to feed jaq; the `&Ir<'_>` is required so the evaluator has access to `Ir::span_for` for the new `loc.pointer` resolution path.

The evaluator SHALL be deterministic: the same `(rulesets, file, ir)` triple produces the same diagnostics in the same order across runs. The order is `(rule declaration order, violation stream order)`.

#### Scenario: Format mismatch skips rule
- **WHEN** a rule declares `match.format: yaml` and the evaluator runs against a JSON document
- **THEN** the rule is skipped with no jq evaluation

#### Scenario: Filter false skips check
- **WHEN** a rule declares `match.filter: '.kind == "Deployment"'` and the document has `kind: Service`
- **THEN** `check.jq` is not evaluated and no diagnostics are emitted

#### Scenario: Each check stream value is one violation
- **WHEN** `check.jq` is `.spec.containers[]` and the document has three containers
- **THEN** three diagnostics are emitted

#### Scenario: Evaluator accepts borrowed `Ir`
- **WHEN** `evaluator.evaluate_file(&path, &doc.as_ir(), "yaml")` is called
- **THEN** the call type-checks AND no clone of the document's value is made by the call site

### Requirement: Location override via `loc:`

The `loc:` block SHALL be optional. When present, `loc.pointer`, `loc.file`, and `loc.line` are jq expressions evaluated against the violation value. Resolution precedence for `line/col` is:

1. **`loc.pointer`** (preferred): coerced to a JSON Pointer string. The evaluator looks up the pointer in the input `Ir`'s provenance map via `Ir::span_for(&p)`. If the lookup returns `Some(span)`, the diagnostic's `line/col` come from the span. If `loc.pointer` is set but coerces to non-string or empty, the diagnostic falls through to the next step.
2. **`loc.line`** (fallback, M8 behaviour): coerced to a positive integer; non-integer or `<= 0` results fall back to the violation's intrinsic line (or 1 if none). Used when `loc.pointer` is unset or fails to resolve to a span.
3. **Intrinsic / default**: the violation's intrinsic line if the parser provided one (M1 saphyr / toml_edit / serde_json all do for the formats they support); otherwise line 1, col 1.

`loc.file` is coerced to a string; empty / non-string results fall back to the file under check. `loc.file` is independent of the `loc.pointer` / `loc.line` precedence chain.

When `loc:` is absent, the diagnostic uses the file under check, the violation's intrinsic line if available, otherwise line 1.

`loc.line` is **deprecated** in favour of `loc.pointer`. Its support is preserved for backwards compatibility with M8-era rules; deprecation is documented in `CHANGELOG.md`. Removal is deferred to a future change.

#### Scenario: `loc.pointer` resolves to span line
- **WHEN** the rule declares `loc.pointer: '"/spec/containers/" + (.idx|tostring)'`, the violation is `{"idx": 0}`, and the input `Ir`'s `span_for("/spec/containers/0")` is `Some(ValueSpan { line: 12, col: 5, ... })`
- **THEN** the diagnostic's `line` is `12` AND `col` is `5`

#### Scenario: `loc.pointer` falls through to `loc.line` when span is missing
- **WHEN** the rule declares both `loc.pointer: '"/missing"'` and `loc.line: '.line'`, the violation is `{"line": 7}`, and the input `Ir` has no span for `/missing`
- **THEN** the diagnostic's `line` is `7` (taken from `loc.line` fallback)

#### Scenario: `loc.line` jq override (legacy path)
- **WHEN** the rule declares only `loc.line: '.position.line'` (no `loc.pointer`) and the violation is `{"position": {"line": 42}}`
- **THEN** the diagnostic's `line` is `42`

#### Scenario: `loc.file` jq override for generated files
- **WHEN** the rule declares `loc.file: '.source_file'` and the violation is `{"source_file": "src/original.tf"}`
- **THEN** the diagnostic's `file` is `src/original.tf` (not the file under check)

#### Scenario: `loc:` absent uses intrinsic position
- **WHEN** `loc:` is unset and the violation node has line 17 in its parser metadata
- **THEN** the diagnostic's `line` is `17`

### Requirement: `Rule.fix` typed schema

The `Rule.fix` field SHALL be `Option<RuleFix>` where:

```rust
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuleFix {
    pub jq: Option<String>,
    pub ops: Option<String>,
}
```

At least one of `jq` or `ops` SHALL be set; both unset is a parse error (`ExecError::Parse` with hint mentioning the rule id). When both are set, `ops` takes precedence (`fix.jq` is ignored for that rule, with a `tracing::warn!` log line). The `jq` field carries a whole-document jq expression (M10 semantics, deprecated). The `ops` field carries a jq expression that returns a JSON-Patch-shaped array (subset: `add`/`replace`/`remove` ops only — see `data-query-edit-ops`).

#### Scenario: Rule with `fix.jq` only parses to a typed `RuleFix`
- **WHEN** a rule YAML carries `fix: { jq: "..." }`
- **THEN** `Rule.fix == Some(RuleFix { jq: Some("..."), ops: None })`

#### Scenario: Rule with `fix.ops` only parses to a typed `RuleFix`
- **WHEN** a rule YAML carries `fix: { ops: "[{op:\"replace\",path:\"/x\",value:1}]" }`
- **THEN** `Rule.fix == Some(RuleFix { jq: None, ops: Some("...") })`

#### Scenario: Rule with both `jq` and `ops` parses; `ops` wins at runtime
- **WHEN** a rule YAML carries `fix: { jq: "A", ops: "B" }`
- **THEN** `Rule.fix == Some(RuleFix { jq: Some("A"), ops: Some("B") })` AND the `Fixer` runtime applies `ops` (logging a warn that `jq` is shadowed)

#### Scenario: Rule with empty `fix:` block fails to parse
- **WHEN** a rule YAML carries `fix: {}`
- **THEN** the loader returns `ExecError::Parse` whose hint mentions that at least one of `jq`/`ops` must be set

#### Scenario: Rule with an unknown field under `fix:` fails to parse
- **WHEN** a rule YAML carries `fix: { jq: "...", kind: replace }`
- **THEN** the loader returns `ExecError::Parse` whose hint mentions the offending field

### Requirement: `Fixer` runtime

`crates/dq-exec/src/fixer.rs` SHALL expose `Fixer::new(&Evaluator) -> Self` and `Fixer::apply(path, doc: &mut Document, format_name) -> Result<FixOutcome>`. The `apply` method walks every rule in the wrapped evaluator's declaration order. For each rule that matches the file's format / glob / `match.filter` AND whose `check.jq` finds at least one violation, the `Fixer` SHALL:

- If the rule has `fix.ops` set: evaluate the ops jq-expression against the current `Ir`, parse the result as `EditScript`, and apply it via `EditScript::apply(&mut doc)`. On parse failure surface `ExecError::FixApply` with the rule id and a parse-error message.
- Else if the rule has `fix.jq` set: evaluate the whole-document jq-expression. The single output replaces the current value if the second application produces the same byte-output (idempotency check by re-parse + re-emit, M10 semantics).
- The rule's fix application is recorded in `FixOutcome.applied_rules` only if the document's bytes changed. If the result is byte-identical to pre-fix, the rule is silently skipped (it was already conformant).

**Idempotency check for `fix.ops`**: after first `EditScript::apply`, the `Fixer` SHALL re-evaluate the rule's `ops` expression against the post-fix `Ir`. If the second `EditScript` is `is_noop()` AND applying it again would not change `original_bytes`, idempotency holds. If the second script is non-empty, the rule is skipped, the input is restored (via the pre-apply Document clone), and the rule id is recorded in `FixOutcome.skipped_non_idempotent`.

**Idempotency check for `fix.jq`** (legacy): the M10 byte-equality check after two applications is preserved unchanged.

#### Scenario: Idempotent `fix.ops` is applied and returned in `applied_rules`
- **WHEN** a rule's `fix.ops` is `[{"op":"replace","path":"/fixed","value":true}]` (jq literal expression) and the input document is `{ name: "x" }`
- **THEN** `FixOutcome.fixed == true`, the document's value contains `fixed: true`, AND `FixOutcome.applied_rules == ["<rule-id>"]`

#### Scenario: Non-idempotent `fix.ops` is skipped and document restored
- **WHEN** a rule's `fix.ops` is `[{"op":"add","path":"/-","value":(.counter // 0) + 1}]` against an array (not idempotent — every apply increments)
- **THEN** `FixOutcome.fixed == false`, the document is restored to its pre-apply state, AND the rule id is recorded in `FixOutcome.skipped_non_idempotent`

#### Scenario: Malformed `fix.ops` output surfaces `ExecError::FixApply`
- **WHEN** a rule's `fix.ops` evaluates to a non-array value or an array containing an unsupported op (e.g. `copy`)
- **THEN** `Fixer::apply` returns `Err(ExecError::FixApply { rule_id, message })` with a parse-error message identifying the offending shape

#### Scenario: Idempotent `fix.jq` (legacy) still applied
- **WHEN** a rule's `fix.jq` is `.fixed = true` (no `fix.ops` set) and the input is `{ name: "x" }`
- **THEN** `FixOutcome.fixed == true`, `FixOutcome.applied_rules == ["<rule-id>"]`, AND a `tracing::debug!` log line records use of the legacy `fix.jq` path

#### Scenario: Both `fix.jq` and `fix.ops` set — `ops` wins
- **WHEN** a rule has `fix.jq: ".x = 1"` AND `fix.ops: [{"op":"replace","path":"/x","value":2}]` (so the two paths would produce different documents)
- **THEN** the resulting document has `x: 2` (from `ops`) AND a `tracing::warn!` log line records that `fix.jq` was shadowed
