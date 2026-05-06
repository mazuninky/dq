# data-query-exec Specification

## Purpose
TBD - created by archiving change add-exec-engine. Update Purpose after archive.
## Requirements
### Requirement: `dq-exec` crate public API

The `dq-exec` crate SHALL expose the following public surface, all `Send + Sync` where applicable:

- `pub struct Diagnostic { rule_id: String, severity: Severity, message: String, file: Option<Utf8PathBuf>, line: u32, col: u32, span: Option<Range<usize>>, references: Vec<String>, fix: Option<serde_yml::Value> }`
- `pub enum Severity { Error, Warn, Info }` with `as_str() -> &'static str` returning `"error"` / `"warn"` / `"info"`.
- `pub struct Rule` parsed via `serde` from the YAML schema (`id`, `description`, `severity`, `match`, `check`, optional `fix`, `references`, `loc`).
- `pub struct RuleSet { source: RuleSource, rules: Vec<Rule> }` and `pub enum RuleSource { Std(&'static str), Local(Utf8PathBuf), Inline }`.
- `RuleSet::from_str(yaml: &str) -> Result<Self, ExecError>` — parses an inline rule or array of rules.
- `RuleSet::from_path(path: &Utf8Path) -> Result<Self, ExecError>` — parses a single rule file or every `*.yml` under a directory (excluding `*.test.yml`).
- `RuleSet::from_std(name: &str) -> Result<Self, ExecError>` — resolves `@std/<namespace>` against `dq_lint::std_ruleset(...)`.
- `pub struct Evaluator` constructed via `Evaluator::new(rulesets: Vec<RuleSet>) -> Result<Self, ExecError>` which compiles every rule's `match.filter` and `check.jq` once. Exposes `evaluate_file(&self, path: &Utf8Path, value: &serde_json::Value, format_name: &str) -> Vec<Diagnostic>`.
- `pub struct RuleLoader` with `RuleLoader::resolve(args: &LoaderArgs) -> Result<Vec<RuleSet>, ExecError>`.
- `pub enum ExecError` (`thiserror`-based): `Parse { source: serde_yml::Error, hint: String }`, `RuleCompile { rule_id: String, source: dq_transform::JqError }`, `UnknownRule { id: String, did_you_mean: Vec<String> }`, `Io { path: Utf8PathBuf, source: std::io::Error }`. `kind_name()` returns `"parse"` / `"rule_compile"` / `"unknown_rule"` / `"io"`.

#### Scenario: Crate compiles in isolation
- **WHEN** the contributor runs `cargo test -p dq-exec`
- **THEN** the crate's unit tests pass without depending on `dq-cli`

#### Scenario: Diagnostic JSON shape matches SARIF reporter
- **WHEN** `Diagnostic::to_serde_json` is called for a sample diagnostic
- **THEN** the produced object contains the keys `path`, `line`, `col`, `message`, `severity`, `rule_id`, `references` — the same shape the M6 SARIF reporter consumes

### Requirement: Rule schema parsing with `serde(deny_unknown_fields)`

`Rule` SHALL be parsed via `serde_yml` with `#[serde(deny_unknown_fields)]` on the top-level rule struct and on the `match` / `check` / `loc` substructures. Typos in field names produce a structured `ExecError::Parse` error pointing at the offending field; the loader collects every parse error in a ruleset and emits them collectively rather than aborting on the first.

The schema:

```yaml
id: <namespace.rule-name>          # required, unique within a RuleSet
description: <multiline-string>    # required
severity: error | warn | info      # required
match:                              # required
  format: <string-or-array>        # required, format-name match (yaml | json | ...)
  filter: <jq-expr>                # optional
  glob: <shell-glob>               # optional
check:                              # required
  jq: <jq-expr>                    # required
  message: <handlebars-lite>       # required
fix: <free-form>                   # optional, parsed but unused in M8 (M10)
references:                         # optional
  - <url>
loc:                                # optional
  file: <jq-expr>                  # optional
  line: <jq-expr>                  # optional
```

Format match accepts a single string (`format: yaml`) or an array (`format: [yaml, json]`); both forms produce the same `Vec<String>` after parsing. The `filter` and `check.jq` strings are validated at compile time by `JqEngine::compile`, with errors surfaced as `ExecError::RuleCompile` carrying the rule id.

#### Scenario: Unknown field rejected
- **WHEN** a rule YAML contains `actoin: error` (typo for `action:`)
- **THEN** `RuleSet::from_str` returns `ExecError::Parse` with a message naming `actoin`

#### Scenario: Format match accepts string or array
- **WHEN** rule A has `format: yaml` and rule B has `format: [yaml, json]`
- **THEN** both parse successfully and the evaluator considers `yaml` a match for both

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

### Requirement: Message templating

The `template::render` function SHALL implement minimal mustache-style substitution: `{{ . }}` (whole value as compact JSON), `{{ .field }}` (object field), `{{ .a.b }}` (nested), `{{ .arr.0 }}` (array index by integer literal). Whitespace inside `{{ }}` is trimmed. Unknown paths render as the literal string `<missing>` so users can see which path failed. No conditionals, loops, helpers, or inline expressions.

#### Scenario: Simple field substitution
- **WHEN** the template is `"container '{{ .name }}' uses :latest"` and the value is `{"name": "app", "image": "app:latest"}`
- **THEN** the rendered string is `"container 'app' uses :latest"`

#### Scenario: Nested path
- **WHEN** the template is `"image: {{ .spec.image }}"` and the value is `{"spec": {"image": "nginx:1.0"}}`
- **THEN** the rendered string is `"image: nginx:1.0"`

#### Scenario: Missing path renders as `<missing>`
- **WHEN** the template is `"x: {{ .nope }}"` and the value is `{}`
- **THEN** the rendered string is `"x: <missing>"`

#### Scenario: Whole value as JSON
- **WHEN** the template is `"violation: {{ . }}"` and the value is `{"a": 1}`
- **THEN** the rendered string is `"violation: {\"a\":1}"`

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

### Requirement: Test runner contract

`RuleTester::run_dir(p) -> Vec<TestOutcome>` SHALL discover every `*.test.yml` under `p` (recursive) and parse each fixture into a `RuleTestFile { tests: Vec<RuleTestCase> }`. The companion rule path is derived from the fixture file's path (e.g. `foo.test.yml` → `foo.yml`), not stored on `RuleTestFile`. For each fixture:

1. Load the parent rule from the matching `<rule>.yml` file (same basename minus `.test`).
2. Parse the fixture's `input` text using the format named by the fixture's `format:` field, defaulting to the parent rule's first declared `match.format`.
3. Evaluate the rule against the parsed value.
4. Compare the actual diagnostics against `expected.violations` order-insensitively.

A test case passes when **every** expected violation matches **at least one** actual diagnostic AND **no** actual diagnostic is unmatched. Match criteria:

- Rule id MUST match exactly.
- If `message_contains` is present, the actual message MUST contain the substring.
- If `message_equals` is present, the actual message MUST equal the string exactly.
- If `line` is present, the actual line MUST equal it.

`TestOutcome` SHALL carry the file, fixture name, pass/fail status, and (on failure) a delta listing missing-expected and extra-actual diagnostics with full context for a clear test failure message.

#### Scenario: Fixture with no expected violations passes when rule is silent
- **WHEN** the rule's `check.jq` returns nothing for a fixture's input
- **THEN** the fixture's `TestOutcome` is `Pass`

#### Scenario: Over-firing rule fails the test
- **WHEN** the rule produces 2 diagnostics and the fixture's `expected.violations` lists 1
- **THEN** the `TestOutcome` is `Fail` with a delta listing 1 unmatched-actual diagnostic

#### Scenario: Missing expected diagnostic fails the test
- **WHEN** the rule produces 0 diagnostics and the fixture's `expected.violations` lists 1
- **THEN** the `TestOutcome` is `Fail` with a delta listing 1 missing-expected diagnostic

### Requirement: Loader resolves explicit and implicit rulesets

`RuleLoader::resolve` SHALL accept a `LoaderArgs` carrying:

- `rules: Vec<String>` — the `--rules` flag values.
- `cwd: Utf8PathBuf` — the directory to search for `./.dq/rules/`.
- `discovered_formats: HashSet<String>` — the formats present in the input files.

When `args.rules` is non-empty, the loader resolves each value as either `@std/<name>`, a path to a file, or a path to a directory. When `args.rules` is empty, the loader includes every `@std/<name>` whose declared `match.format` overlaps `discovered_formats`, plus every `*.yml` under `args.cwd.join(".dq/rules/")` if the directory exists. Duplicate rulesets are de-duplicated by source.

#### Scenario: Explicit `--rules` wins
- **WHEN** the user passes `--rules @std/k8s --rules ./extra.yml`
- **THEN** the loader returns exactly two rulesets, regardless of `./.dq/rules/` contents

#### Scenario: Implicit auto-binding by format
- **WHEN** no `--rules` is given and the input files contain only `*.yaml` Kubernetes manifests
- **THEN** the loader returns `@std/k8s` (which declares `match.format: yaml`); it does NOT include `@std/dockerfile` (declares `match.format: dockerfile`)

#### Scenario: `./.dq/rules/` auto-loaded
- **WHEN** no `--rules` is given and `./.dq/rules/custom.yml` exists
- **THEN** the loader includes `custom.yml` alongside any auto-bound `@std/*`

#### Scenario: Unknown `@std/...` produces structured error
- **WHEN** the user passes `--rules @std/nope`
- **THEN** `RuleLoader::resolve` returns `ExecError::UnknownRule { id: "@std/nope", did_you_mean: ["@std/npm", ...] }` with at most three Levenshtein-2 suggestions

### Requirement: Rule compilation happens once per evaluator construction

`Evaluator::new` SHALL compile every rule's `match.filter` (when present) and `check.jq` exactly once via `JqEngine::compile`, store the resulting engines in the evaluator, and reuse them for every `evaluate_file` call. Compile failures propagate as `ExecError::RuleCompile`. The evaluator MUST be `Send + Sync + Clone` so `Arc<Evaluator>` can be shared across rayon workers in the bulk lint path.

#### Scenario: Compile error names the offending rule
- **WHEN** rule `k8s.bad` has `check.jq: '.foo |='` (missing RHS)
- **THEN** `Evaluator::new` returns `ExecError::RuleCompile { rule_id: "k8s.bad", source: JqError::Compile { ... } }`

#### Scenario: Engine is Send + Sync
- **WHEN** `fn assert_send_sync<T: Send + Sync>(_: &T) {}` is called with `&Evaluator`
- **THEN** the type-check passes

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

### Requirement: `ExecError::FixApply`

`ExecError::FixApply { rule_id, message }` SHALL exist with `kind_name() == "fix_apply"`. The CLI's exit-code mapper routes this variant to `PARSE_ERROR` (3) — same family as `RuleCompile` because both are rule-author bugs at the jq layer.

#### Scenario: `kind_name` returns `"fix_apply"` for the variant

- **WHEN** `ExecError::FixApply { rule_id: "r".into(), message: "m".into() }.kind_name()` is called
- **THEN** the returned string equals `"fix_apply"` and the CLI exit-code mapper routes the error to `PARSE_ERROR` (exit 3)

