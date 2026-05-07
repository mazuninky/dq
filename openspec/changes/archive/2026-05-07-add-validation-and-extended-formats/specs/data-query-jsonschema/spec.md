## ADDED Requirements

### Requirement: `Rule.check` accepts `schema:` inline JSON Schema 2020-12

The `dq-exec` rule schema SHALL accept an alternative `check.schema:` field whose value is a JSON Schema 2020-12 document expressed as inline YAML. When present, the rule SHALL validate every input file's `Document::value()` against the schema using the `jsonschema` crate (≥ 0.34, draft 2020-12), and emit one `Diagnostic` per validation error. The `instancePath` from each error SHALL be parsed as an RFC 6901 `Pointer` and used as the diagnostic's `pointer` field; `keywordLocation` SHALL be included in the diagnostic's `message` field for traceability.

The `check.schema:` field is mutually exclusive with `check.jq:`, `check.schema_file:`, and `check.extract:` — exactly one of the four MUST be present in any rule.

#### Scenario: Inline schema validates document
- **GIVEN** a rule with `check.schema: { type: object, required: [name] }`
- **WHEN** the evaluator runs against a YAML file containing `{}`
- **THEN** one `Diagnostic` is emitted with `pointer = ""`, `severity = error`, and `message` containing the keyword `required`

#### Scenario: Schema violation gets pointer from `instancePath`
- **GIVEN** a rule with `check.schema: { properties: { age: { type: integer } } }`
- **WHEN** the evaluator runs against `{age: "twelve"}` parsed as JSON
- **THEN** the emitted `Diagnostic` has `pointer = "/age"` and the `line`/`col` are looked up via `Ir::span_for("/age")` (or fall back to `1, 1` when the input format has no spans)

#### Scenario: Mutual-exclusion error at rule-load time
- **GIVEN** a rule YAML with both `check.jq: '.foo'` and `check.schema: {type: object}`
- **WHEN** `RuleSet::compile` is called
- **THEN** the load fails with `ExecError::CheckMutuallyExclusive { rule_id, present_fields: ["jq", "schema"] }`

### Requirement: `Rule.check` accepts `schema_file:` resolving relative to rule directory

The `dq-exec` rule schema SHALL also accept `check.schema_file:` whose value is a path resolved relative to the directory containing the rule YAML file. Absolute paths SHALL be rejected with `ExecError::SchemaFileAbsolutePath { rule_id, path }`. Paths that resolve outside the rule directory tree (via `..` segments) SHALL be rejected with `ExecError::SchemaFileEscapesRuleDir { rule_id, path }`. The schema content SHALL be read once at `RuleSet::compile` time and compiled into a `jsonschema::Validator` cached on the `CompiledRule`.

#### Scenario: Relative schema file resolves
- **GIVEN** a rule at `crates/dq-lint/rules/jsonschema/k8s-crd.yml` with `check.schema_file: ./k8s-crd.schema.json`
- **WHEN** `RuleSet::compile` is called
- **THEN** the schema is read from `crates/dq-lint/rules/jsonschema/k8s-crd.schema.json` and compiled successfully

#### Scenario: Absolute path rejected
- **GIVEN** a rule with `check.schema_file: /etc/passwd`
- **WHEN** `RuleSet::compile` is called
- **THEN** the load fails with `ExecError::SchemaFileAbsolutePath { rule_id, path }`

#### Scenario: Path escape rejected
- **GIVEN** a rule at `rules/foo/bar.yml` with `check.schema_file: ../../etc/passwd.json`
- **WHEN** `RuleSet::compile` is called
- **THEN** the load fails with `ExecError::SchemaFileEscapesRuleDir { rule_id, path }`

### Requirement: Schema validator is compiled once per ruleset

`RuleSet::compile` SHALL produce a `jsonschema::Validator` for every schema-bearing rule exactly once, store it on the corresponding `CompiledRule`, and reuse it across all subsequent `evaluate_file` calls. Per-file recompilation is forbidden — repeated `evaluate_file` calls against the same `Evaluator` MUST NOT trigger schema-compile work.

#### Scenario: Compile-once contract
- **GIVEN** an `Evaluator` built from a ruleset containing one schema-rule
- **WHEN** `evaluate_file` is called 100 times with different inputs
- **THEN** `jsonschema::Validator` is constructed exactly once (verifiable via the existing `Rule compilation happens once per evaluator construction` contract in `data-query-exec`)

### Requirement: Reference `@std/jsonschema` ruleset

`dq-lint` SHALL ship the `@std/jsonschema` namespace containing at least three reference rules: `kubernetes-crd-shape` (validates basic kind/apiVersion/metadata shape), `helm-values-against-schema` (a parameterised template rule users override per-chart with their `values.schema.json`), and `openapi-3.1-shape` (validates OpenAPI 3.1.0 root structure). Each rule SHALL have a co-located `*.test.yml` fixture with at least one positive and one negative case.

#### Scenario: Namespace registered
- **WHEN** `list_std_rulesets()` is called
- **THEN** the slice contains `"jsonschema"`

#### Scenario: All three reference rules pass `dq test`
- **WHEN** `dq test crates/dq-lint/rules/jsonschema/` is run
- **THEN** the exit code is 0 and the summary reports zero failures

### Requirement: JSON Schema `$ref` is restricted to internal references

Schema rules SHALL resolve `$ref` only to references within the schema itself (resolvable via the schema's own `$id` graph). HTTP-loaded `$ref` (`http://`, `https://` prefixes) and file-loaded `$ref` (`file://` or relative paths) MUST NOT trigger network or filesystem reads. The `jsonschema` validator SHALL be configured with the default registry (no resolver hooks installed). When a rule's schema declares an unresolvable `$ref`, `RuleSet::compile` SHALL fail with `ExecError::SchemaCompile { rule_id, source }`.

#### Scenario: HTTP $ref rejected at compile-time
- **GIVEN** a rule with `check.schema: { $ref: "https://json-schema.org/draft/2020-12/schema" }`
- **WHEN** `RuleSet::compile` is called
- **THEN** the load fails with `ExecError::SchemaCompile { rule_id, source }` and no network call is made
