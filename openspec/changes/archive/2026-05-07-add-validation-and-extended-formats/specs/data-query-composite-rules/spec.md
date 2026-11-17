## ADDED Requirements

### Requirement: `Rule.check` accepts `extract:` + `nested:` for cross-format composition

The `dq-exec` rule schema SHALL accept a composite `check` shape with two fields: `extract:` (a jq expression returning an array) and `nested:` (a recursively-typed `Rule`). When present, the evaluator SHALL run `extract` against the input `Ir`, parse each extracted item according to its declared format, run `nested` against each parsed sub-document, and re-project resulting diagnostics back to source-file coordinates.

The `extract:` + `nested:` pair is mutually exclusive with `check.jq:`, `check.schema:`, and `check.schema_file:` — exactly one of the four MUST be present in any rule. Specifying `extract:` without `nested:` (or vice versa) at rule-load time SHALL fail with `ExecError::CompositeIncomplete { rule_id, missing_field }`.

#### Scenario: Composite shape parses
- **GIVEN** a rule YAML with `check.extract: '...'` and `check.nested: { id: ..., match: {...}, check: { jq: '...' } }`
- **WHEN** `RuleSet::compile` is called
- **THEN** the load succeeds and the rule is registered as a composite rule

#### Scenario: Mutual-exclusion enforced
- **GIVEN** a rule with both `check.jq` and `check.extract`
- **WHEN** `RuleSet::compile` is called
- **THEN** the load fails with `ExecError::CheckMutuallyExclusive { rule_id, present_fields: ["jq", "extract"] }`

#### Scenario: Half-composite rejected
- **GIVEN** a rule with only `check.extract` (no `check.nested`)
- **WHEN** `RuleSet::compile` is called
- **THEN** the load fails with `ExecError::CompositeIncomplete { rule_id, missing_field: "nested" }`

### Requirement: `extract` jq expression contract

The `extract` jq expression SHALL evaluate to a JSON array of objects, where every object has at minimum the three string fields `value`, `format`, and `anchor`. The `value` is the raw bytes (as a string) to be re-parsed; `format` is a name from the `FormatTag` enum (`yaml`, `json`, `toml`, `markdown`, `xml`, etc.); `anchor` is an RFC 6901 pointer addressing the parent scalar in the outer document.

If `extract` returns a non-array value, the evaluator SHALL fail with `ExecError::CompositeExtractNotArray { rule_id }`. If any extracted object is missing one of the three required fields, the evaluator SHALL fail with `ExecError::CompositeExtractMalformed { rule_id, missing_field }`. If `format` is not a known `FormatTag`, the evaluator SHALL fail with `ExecError::CompositeExtractUnknownFormat { rule_id, format }`.

#### Scenario: Well-formed extract array
- **GIVEN** a rule with `extract: '[{value: "key: 1", format: "yaml", anchor: "/code/0"}]'`
- **WHEN** the evaluator runs against any input
- **THEN** the parser is invoked once with `b"key: 1"` as YAML and the resulting nested diagnostics are anchored at `/code/0`

#### Scenario: Non-array extract rejected
- **GIVEN** a rule with `extract: '"hello"'` (a string, not array)
- **WHEN** the evaluator runs
- **THEN** evaluation fails with `ExecError::CompositeExtractNotArray { rule_id }`

#### Scenario: Missing field rejected
- **GIVEN** a rule with `extract: '[{value: "x", format: "yaml"}]'` (no `anchor`)
- **WHEN** the evaluator runs
- **THEN** evaluation fails with `ExecError::CompositeExtractMalformed { rule_id, missing_field: "anchor" }`

#### Scenario: Unknown format rejected
- **GIVEN** a rule with `extract: '[{value: "x", format: "fortran", anchor: ""}]'`
- **WHEN** the evaluator runs
- **THEN** evaluation fails with `ExecError::CompositeExtractUnknownFormat { rule_id, format: "fortran" }`

### Requirement: Inner-format parse failure becomes outer-rule diagnostic

When `Format::parse` fails on an extracted value, the evaluator SHALL emit a `Diagnostic` attributed to the outer rule with `rule_id = "<outer-id>.parse-failed"`, `severity = error`, `message = "<format> parse failed: <parse-error>"`, and `pointer` set to the `anchor`. The inner `nested` rule SHALL NOT be evaluated for that extracted item. Subsequent extracted items SHALL still be processed (parse failure is per-item, not per-rule).

#### Scenario: Invalid YAML in markdown code block
- **GIVEN** an outer rule extracting yaml-tagged code blocks from markdown
- **WHEN** the evaluator encounters a code block with `key: : invalid` content
- **THEN** one `Diagnostic` is emitted with `rule_id = "<outer>.parse-failed"`, `severity = error`, message containing `yaml parse failed`, and `pointer` pointing at the code-block anchor

#### Scenario: Multiple extracted items, partial failure
- **GIVEN** extract returns three items, the second of which has invalid YAML
- **WHEN** the evaluator runs
- **THEN** items 1 and 3 are parsed and have `nested` evaluated; item 2 emits one parse-failed diagnostic; total emitted diagnostics include all three results without aborting

### Requirement: Nested diagnostic coordinates project to source via anchor + inline-offset

Diagnostics produced by `nested` SHALL be re-projected onto outer-file coordinates using the formula:

```
final_line = anchor_span.line + inner_diagnostic.line - 1
final_col  = if inner_diagnostic.line == 1 {
                anchor_inline.col + inner_diagnostic.col - 1
             } else {
                inner_diagnostic.col
             }
```

Here `anchor_span` is `Ir::span_for(&anchor_pointer)` and `anchor_inline` is `Ir::provenance_for(&anchor_pointer).inline_offset` (see `data-query-ir`). When `anchor_span` is `None`, the projected diagnostic SHALL retain `inner_diagnostic.line` / `inner_diagnostic.col` and a `tracing::warn!` SHALL log the missing anchor span. When `anchor_inline` is `None`, the column projection SHALL fall back to using `anchor_span.col` only (no inline-precision).

#### Scenario: YAML-in-markdown line projection
- **GIVEN** a markdown file where a yaml code block starts at source line 10, column 5
- **AND** the inner YAML evaluator produces a diagnostic at inner line 3, column 7
- **WHEN** the composite evaluator projects the diagnostic
- **THEN** the projected `Diagnostic.line` is `12` (10 + 3 - 1) and `Diagnostic.col` is `7`

#### Scenario: First-line column projection uses anchor inline-offset
- **GIVEN** an inner diagnostic at inner line 1, column 4
- **AND** anchor span at source line 10, column 5 with inline-offset col 1
- **WHEN** the composite evaluator projects the diagnostic
- **THEN** the projected `Diagnostic.col` is `4` (1 + 4 - 1)

#### Scenario: Missing anchor span warns and degrades
- **GIVEN** an anchor pointer with no entry in `Ir::span_for`
- **WHEN** the composite evaluator projects diagnostics
- **THEN** projected coordinates equal inner coordinates, a `tracing::warn!` log is emitted with the rule id and anchor pointer, and processing continues for remaining items

### Requirement: Recursion depth bound for composite rules

Composite-rule evaluation SHALL be bounded by `MAX_EXTRACT_DEPTH = 4` recursive levels (counting the outermost rule as depth 0). Exceeding the bound SHALL fail with `ExecError::CompositeDepthExceeded { rule_id, depth, max }`. The `Evaluator` SHALL expose `with_max_extract_depth(usize)` as a builder method intended for unit tests; the constant is otherwise non-configurable through rule YAML.

#### Scenario: Bound enforced
- **GIVEN** a self-similar rule whose `extract` re-extracts the entire input as the same format
- **WHEN** the evaluator runs against any input
- **THEN** at depth 4 the evaluator emits `ExecError::CompositeDepthExceeded { rule_id, depth: 4, max: 4 }`

#### Scenario: Test override allowed
- **GIVEN** an `Evaluator::new(...).with_max_extract_depth(2)`
- **WHEN** the same self-similar rule runs
- **THEN** the bound trips at depth 2 instead of 4
