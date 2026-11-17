## MODIFIED Requirements

### Requirement: Rule schema parsing with `serde(deny_unknown_fields)`

`Rule` SHALL be parsed via `serde_yml` with `#[serde(deny_unknown_fields)]` on the top-level rule struct and on the `match` / `check` / `loc` substructures. Typos in field names produce a structured `ExecError::Parse` error pointing at the offending field; the loader collects every parse error in a ruleset and emits them collectively rather than aborting on the first.

The schema:

```yaml
id: <namespace.rule-name>          # required, unique within a RuleSet
description: <multiline-string>    # required
severity: error | warn | info      # required
match:                              # required
  format: <string-or-array>        # required, format-name match (yaml | json | xml | ...)
  filter: <jq-expr>                # optional
  glob: <shell-glob>               # optional
check:                              # required, exactly one of {jq | schema | schema_file | extract+nested}
  jq: <jq-expr>                    # variant 1: jq-driven check
  message: <handlebars-lite>       # required when jq is used
  schema: <inline-json-schema>     # variant 2: JSON Schema 2020-12 inline
  schema_file: <relative-path>     # variant 3: JSON Schema 2020-12 in sibling file
  extract: <jq-expr>               # variant 4: composite (paired with `nested`)
  nested: <Rule>                   # variant 4: composite (paired with `extract`)
fix: <free-form>                   # optional, see Rule.fix typed schema
references:                         # optional
  - <url>
loc:                                # optional
  file: <jq-expr>                  # optional
  line: <jq-expr>                  # optional
```

Format match accepts a single string (`format: yaml`) or an array (`format: [yaml, json]`); both forms produce the same `Vec<String>` after parsing. The `filter` and `check.jq` strings are validated at compile time by `JqEngine::compile`, with errors surfaced as `ExecError::RuleCompile` carrying the rule id.

The `check` block SHALL be parsed as a tagged-by-shape enum: exactly one of `jq`, `schema`, `schema_file`, or `(extract + nested)` MUST be present. Combinations or omissions SHALL fail with `ExecError::CheckMutuallyExclusive { rule_id, present_fields }` (when more than one variant is set) or `ExecError::CheckMissing { rule_id }` (when none of the four is set). When the `extract` field is set without `nested` (or vice versa), the loader SHALL emit `ExecError::CompositeIncomplete { rule_id, missing_field }`.

The `message` field is required for `check.jq` and `check.extract`+`nested` variants; for `check.schema` and `check.schema_file` variants the message is auto-generated from the validator's `keywordLocation` and the requirement's own description, and the `message` field, if present, is used as a prefix.

#### Scenario: Unknown field rejected
- **WHEN** a rule YAML contains `actoin: error` (typo for `action:`)
- **THEN** `RuleSet::from_str` returns `ExecError::Parse` with a message naming `actoin`

#### Scenario: Format match accepts string or array
- **WHEN** rule A has `format: yaml` and rule B has `format: [yaml, json]`
- **THEN** both parse successfully and the evaluator considers `yaml` a match for both

#### Scenario: Existing `check.jq`-only rules parse unchanged
- **WHEN** a rule YAML carries `check: { jq: ".foo", message: "..." }`
- **THEN** the rule loads as `Check::Jq { jq: ".foo", message: "..." }` with no other variants set

#### Scenario: Schema variant parses
- **WHEN** a rule YAML carries `check: { schema: { type: object, required: [name] } }`
- **THEN** the rule loads as `Check::Schema { schema: ... }`

#### Scenario: Schema-file variant parses
- **WHEN** a rule YAML carries `check: { schema_file: "./shape.schema.json" }`
- **THEN** the rule loads as `Check::SchemaFile { schema_file: "./shape.schema.json" }`

#### Scenario: Composite variant parses
- **WHEN** a rule YAML carries `check: { extract: "...", nested: { id: ..., match: {...}, check: { jq: "true", message: "..." } } }`
- **THEN** the rule loads as `Check::Composite { extract: "...", nested: <Rule> }`

#### Scenario: Empty `check` rejected
- **WHEN** a rule YAML carries `check: {}`
- **THEN** loading fails with `ExecError::CheckMissing { rule_id }`

#### Scenario: Two variants rejected
- **WHEN** a rule YAML carries `check: { jq: ".x", schema: {type: object} }`
- **THEN** loading fails with `ExecError::CheckMutuallyExclusive { rule_id, present_fields: ["jq", "schema"] }`

### Requirement: Rule evaluation pipeline

`Evaluator::evaluate_file(&self, path: &Utf8Path, ir: &Ir<'_>, format_name: &str) -> Vec<Diagnostic>` SHALL apply each rule in declaration order with the following pipeline:

1. **Format match.** If `format_name` is not in the rule's `match.format` list, skip.
2. **Glob match.** If `match.glob` is set and does not match the file's relative path (using `globset`), skip.
3. **Filter match.** If `match.filter` is set, evaluate it against the document. If the result stream is empty, or the first result is `false` / `null`, skip.
4. **Check eval.** Dispatch on the `Check` variant:
   - `Jq { jq, message }`: evaluate `jq` against the document. Each output value is one violation; render `message` against the violation value.
   - `Schema { schema }` or `SchemaFile { schema_file }`: invoke the cached `jsonschema::Validator` against the document's JSON projection. Each validation error is one violation; the diagnostic's `pointer` comes from the error's `instancePath`, the message contains `keywordLocation` and the validator's human message.
   - `Composite { extract, nested }`: evaluate `extract` against the document, parse each item according to its declared format, run `nested` recursively (subject to `MAX_EXTRACT_DEPTH` — see `data-query-composite-rules`), and re-project resulting diagnostics back to outer-file coordinates using anchor + inline-offset.
5. **Diagnostic build.** For each violation, derive location from the variant-specific source (jq path: `loc.pointer`/`loc.line`/intrinsic; schema path: `instancePath`; composite path: anchor + inline-offset projection), and emit a `Diagnostic`.

The signature accepts a borrowed `Ir<'_>` (unchanged from the IR-foundation milestone). The evaluator SHALL be deterministic: the same `(rulesets, file, ir)` triple produces the same diagnostics in the same order across runs. The order is `(rule declaration order, violation stream order)`. For composite rules the order within a rule is `(extract item index, nested rule's own order)`.

#### Scenario: Format mismatch skips rule
- **WHEN** a rule declares `match.format: yaml` and the evaluator runs against a JSON document
- **THEN** the rule is skipped with no jq evaluation

#### Scenario: Filter false skips check
- **WHEN** a rule declares `match.filter: '.kind == "Deployment"'` and the document has `kind: Service`
- **THEN** `check.jq` is not evaluated and no diagnostics are emitted

#### Scenario: Each check stream value is one violation
- **WHEN** `check.jq` is `.spec.containers[]` and the document has three containers
- **THEN** three diagnostics are emitted

#### Scenario: Schema check produces one diagnostic per validation error
- **WHEN** a `check.schema` validator yields three errors against a document
- **THEN** three diagnostics are emitted, one per error, with distinct `pointer`s derived from each error's `instancePath`

#### Scenario: Composite check produces one diagnostic per nested violation
- **WHEN** `check.extract` returns three items and `nested` produces one violation per item
- **THEN** three diagnostics are emitted with line/col projected to outer-file coordinates

#### Scenario: Evaluator accepts borrowed `Ir`
- **WHEN** `evaluator.evaluate_file(&path, &doc.as_ir(), "yaml")` is called
- **THEN** the call type-checks AND no clone of the document's value is made by the call site

### Requirement: Rule compilation happens once per evaluator construction

`Evaluator::new` SHALL compile every rule's `match.filter` (when present), every `Check::Jq`'s `jq`, every `Check::Composite`'s `extract` (and recursively the `nested` rule's compiled artefacts), AND every `Check::Schema` / `Check::SchemaFile`'s schema (into a `jsonschema::Validator`) exactly once, store the resulting compiled artefacts on the corresponding `CompiledRule`, and reuse them for every `evaluate_file` call. Compile failures propagate as `ExecError::RuleCompile { rule_id, source }` for jq compile errors and `ExecError::SchemaCompile { rule_id, source }` for schema compile errors. The evaluator MUST be `Send + Sync + Clone` so `Arc<Evaluator>` can be shared across rayon workers in the bulk lint path.

#### Scenario: jq compile error names the offending rule
- **WHEN** rule `k8s.bad` has `check.jq: '.foo |='` (missing RHS)
- **THEN** `Evaluator::new` returns `ExecError::RuleCompile { rule_id: "k8s.bad", source: JqError::Compile { ... } }`

#### Scenario: Schema compile error names the offending rule
- **WHEN** rule `js.bad` has `check.schema: { $ref: "https://invalid.example/" }`
- **THEN** `Evaluator::new` returns `ExecError::SchemaCompile { rule_id: "js.bad", source: ... }`

#### Scenario: Engine is Send + Sync
- **WHEN** `fn assert_send_sync<T: Send + Sync>(_: &T) {}` is called with `&Evaluator`
- **THEN** the type-check passes

## ADDED Requirements

### Requirement: New `ExecError` variants for schema and composite checks

`ExecError` SHALL gain the following variants, each with a `kind_name()` returning the snake-case name and a CLI exit-code mapping:

```rust
pub enum ExecError {
    // ... existing ...
    CheckMutuallyExclusive { rule_id: String, present_fields: Vec<String> },  // kind: check_mutually_exclusive, exit 3
    CheckMissing { rule_id: String },                                          // kind: check_missing, exit 3
    CompositeIncomplete { rule_id: String, missing_field: String },            // kind: composite_incomplete, exit 3
    CompositeExtractNotArray { rule_id: String },                              // kind: composite_extract_not_array, exit 1
    CompositeExtractMalformed { rule_id: String, missing_field: String },      // kind: composite_extract_malformed, exit 1
    CompositeExtractUnknownFormat { rule_id: String, format: String },         // kind: composite_extract_unknown_format, exit 1
    CompositeDepthExceeded { rule_id: String, depth: usize, max: usize },      // kind: composite_depth_exceeded, exit 1
    SchemaCompile { rule_id: String, source: jsonschema::Error },              // kind: schema_compile, exit 3
    SchemaFileAbsolutePath { rule_id: String, path: Utf8PathBuf },             // kind: schema_file_absolute_path, exit 3
    SchemaFileEscapesRuleDir { rule_id: String, path: Utf8PathBuf },           // kind: schema_file_escapes_rule_dir, exit 3
}
```

`CompositeExtract*` and `CompositeDepthExceeded` are runtime errors mapped to `GENERIC` (1) because they signal user-input semantics (the rule is structurally valid but the input + extract combination produced an evaluation failure). The other new variants are rule-load-time errors mapped to `PARSE_ERROR` (3).

#### Scenario: `kind_name` is stable for every new variant
- **WHEN** each new variant is constructed and `kind_name()` is called
- **THEN** the returned strings are exactly `"check_mutually_exclusive"`, `"check_missing"`, `"composite_incomplete"`, `"composite_extract_not_array"`, `"composite_extract_malformed"`, `"composite_extract_unknown_format"`, `"composite_depth_exceeded"`, `"schema_compile"`, `"schema_file_absolute_path"`, `"schema_file_escapes_rule_dir"`

#### Scenario: Exit-code mapping is correct
- **WHEN** the CLI exit-code mapper sees each new variant
- **THEN** `CompositeExtract*` and `CompositeDepthExceeded` route to `GENERIC` (1) and the remainder route to `PARSE_ERROR` (3)
