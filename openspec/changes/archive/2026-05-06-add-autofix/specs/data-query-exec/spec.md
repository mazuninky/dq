## ADDED Requirements

### Requirement: `Rule.fix` typed schema

The `Rule.fix` field SHALL be `Option<RuleFix>` where `RuleFix { jq: String }` carries `#[serde(deny_unknown_fields)]`. M8 stored this as opaque YAML; M10 types it.

#### Scenario: Rule with `fix.jq` parses to a typed `RuleFix`

- **WHEN** a rule YAML carries `fix: { jq: "..." }`
- **THEN** `Rule.fix == Some(RuleFix { jq: "..." })`

#### Scenario: Rule with an unknown field under `fix:` fails to parse

- **WHEN** a rule YAML carries `fix: { jq: "...", kind: replace }`
- **THEN** the loader returns `ExecError::Parse` whose hint mentions the offending field

### Requirement: `Fixer` runtime

`crates/dq-exec/src/fixer.rs` SHALL expose `Fixer::new(&Evaluator) -> Self` and `Fixer::apply(path, value, format_name) -> Result<FixOutcome>`. The `apply` method walks every rule in the wrapped evaluator's declaration order and, for each rule with a compiled `fix.jq` engine that matches the file's format / glob / `match.filter` AND whose `check.jq` finds at least one violation, runs `fix.jq` against the current value. The single output replaces the current value if (a) a second application produces the same result (idempotency) and (b) the post-fix value differs from the pre-fix value.

#### Scenario: Idempotent fix is applied and returned in `applied_rules`

- **WHEN** a rule's `fix.jq` is `.fixed = true` and the input is `{ name: "x" }`
- **THEN** `FixOutcome.fixed == true`, `FixOutcome.new_value.fixed == true`, `FixOutcome.applied_rules == ["<rule-id>"]`

#### Scenario: Non-idempotent fix is skipped with a warn log

- **WHEN** a rule's `fix.jq` is `.counter = (.counter // 0) + 1` (not idempotent)
- **THEN** `FixOutcome.fixed == false`, the input value is returned unchanged in `new_value`, and the rule id is recorded in `FixOutcome.skipped_non_idempotent`

#### Scenario: Wrong-arity `fix.jq` surfaces `ExecError::FixApply`

- **WHEN** a rule's `fix.jq` is `.[]` (multi-output) or `empty` (zero-output)
- **THEN** `Fixer::apply` returns `ExecError::FixApply` with the rule id and an arity message

### Requirement: `ExecError::FixApply`

`ExecError::FixApply { rule_id, message }` SHALL exist with `kind_name() == "fix_apply"`. The CLI's exit-code mapper routes this variant to `PARSE_ERROR` (3) — same family as `RuleCompile` because both are rule-author bugs at the jq layer.

#### Scenario: `kind_name` returns `"fix_apply"` for the variant

- **WHEN** `ExecError::FixApply { rule_id: "r".into(), message: "m".into() }.kind_name()` is called
- **THEN** the returned string equals `"fix_apply"` and the CLI exit-code mapper routes the error to `PARSE_ERROR` (exit 3)
