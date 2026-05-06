## Why

M8 shipped the lint engine plus standard rule libraries; M9 added markdown as a first-class queryable format. M10 closes the last "DevOps quality gate" loop in the dq-plan.md M10 envelope: **rules can now ship a `fix:` block, and `dq fix` applies those fixes atomically with the same `-i` / `--diff` / `--check` discipline used by `dq set` / `dq del`**. Without M10, `dq lint --strict` in CI is a one-way gate — every violation has to be hand-fixed by the developer who landed on it. M10 lets the same rule library that produces the diagnostic also tell `dq` how to make the file pass.

The risk envelope is moderate. The new code is contained in `dq-exec` (one new module: `fixer.rs`, plus a typed `RuleFix` schema and one new error variant) and `dq-cli` (one new command: `commands/fix.rs`, plus the matching args struct, dispatcher wiring, and exit-code routing). No new dependencies. The bulk driver (`dq-cli/src/bulk.rs`) is reused verbatim — `dq fix` builds a `FixFileOp` and hands it to `bulk::run_per_file`, which already implements `-i` (atomic write) / `--diff` (stdout) / `--check` (compare-only, exit 1) / `--continue-on-error` / `--parallel`. M10 inherits all those guarantees for free.

The single load-bearing decision is **scope the fix payload to a whole-document jq transform** (`fix: { jq: "..." }`). Per-violation fixes and an explicit ops vocabulary (`replace` / `insert` / `delete`) were on the table per the dq-plan.md phrasing "трансформация (jq-выражение или явный набор ops)". In design review, the ops vocabulary turned out to triple the implementation surface (per-op validation, idempotency proof per op, fixture format extension to assert ops applied) for use cases that whole-document jq already handles cleanly. The trade-off is the same as `dq set --jq`: comments are lost on re-emit. M10 documents this in the `dq fix` handler module-doc and in the README, mirroring the M7 disclosure for `dq set --jq`.

## What Changes

### New typed schema: `RuleFix` (`crates/dq-exec/src/rule.rs`)

The `Rule.fix: Option<serde_yml::Value>` opaque-YAML field becomes `Rule.fix: Option<RuleFix>` where `RuleFix` is `{ jq: String }` with `#[serde(deny_unknown_fields)]`. Forward-incompatible ops vocabularies (e.g. a future `kind: replace`) now fail at rule-load time instead of being silently ignored. The `Diagnostic.fix` field gains the same typed shape — no reporter currently consumes it (M8 audit confirmed only the SARIF reporter looks at diagnostics, and it doesn't touch the `fix` payload), so this is a non-breaking change for the existing reporters.

### New runtime: `Fixer` (`crates/dq-exec/src/fixer.rs`)

`Fixer::new(&Evaluator) -> Self` borrows the evaluator's pre-compiled rules through `Arc` clones. `Fixer::apply(path, value, format_name) -> Result<FixOutcome>` walks every rule in declaration order and, for each rule that:

1. has a compiled `fix.jq` engine,
2. matches the file's format / glob / `match.filter`,
3. has at least one violation reported by `check.jq` against the current value,

runs `fix.jq` against the current value. The output replaces the current value if (a) the fix is idempotent (a second application produces the same result) and (b) the post-fix value differs from the pre-fix value. **Non-idempotent fixes are skipped at runtime with a `tracing::warn!` log line** — they are a rule-author bug, not a hard failure of the run. The output `FixOutcome` carries `applied_rules: Vec<String>` and `skipped_non_idempotent: Vec<String>` so callers can audit the run.

### New CLI command: `dq fix` (`crates/dq-cli/src/commands/fix.rs`)

```
dq fix [GLOBAL-FLAGS] FILE...
       --rules <RULE>... (repeatable; same syntax as dq lint --rules)
```

Honours every write-mode flag through the shared `bulk::run_per_file` driver: `-i` / `--diff` / `--check` / `--continue-on-error` / `--parallel` / `--backup`. Rejects `--allow-templates` and `--raw-template-strings` up front (the re-emit path can't preserve template placeholder positions, same trade-off as `dq set --jq`). Auto-binds the same `@std/<ns>` rule namespaces as `dq lint` when `--rules` is empty.

### New error variant: `ExecError::FixApply`

Surfaces a `fix.jq` runtime failure or wrong-arity output (zero / multi). The `kind_name()` selector returns `"fix_apply"`; the CLI's exit-code mapper routes it to `PARSE_ERROR` (3) — same family as `RuleCompile` because both are "the rule author shipped buggy jq". `FixApply` is **not** used for non-idempotent fixes; those are skipped at runtime with a warn log and surfaced in `FixOutcome.skipped_non_idempotent`.

### Two `@std` rules gain `fix:` blocks (proof)

- `crates/dq-lint/rules/k8s/image-pull-policy-always.yml` — when a pinned-tag container has `imagePullPolicy: Always`, swap to `IfNotPresent`. Idempotent: after the swap the predicate `imagePullPolicy == "Always"` no longer holds.
- `crates/dq-lint/rules/npm/has-license.yml` — when the license field is missing or empty on a non-private package, set it to `"UNLICENSED"` (npm's documented placeholder). Idempotent: the predicate `(.license // "") == ""` no longer holds.

### Capabilities

#### Modified Capabilities

- **`data-query-exec`** — gains a `RuleFix` schema requirement and a `Fixer` runtime requirement. The existing `Rule.fix` requirement is updated from "opaque YAML, M10 will execute it" to the typed-jq form.
- **`cli-shell`** — gains a `dq fix` subcommand requirement. No change to existing `dq lint` / `dq check` semantics.

### Meta

- **`dq-plan.md` M10 section** marked `✅ Implemented 2026-05-05 (см. [openspec/changes/add-autofix/](openspec/changes/add-autofix/))`.
- **`README.md`** status line moves from `M9 alpha — adds markdown AST + @std/markdown` to `M10 alpha — adds dq fix autofix engine`. Examples block adds four `dq fix` invocations covering `--check` / `--diff` / `-i` / `@std/<ns>`.

### What's NOT in M10 (deferred)

- **Per-violation fixes.** A single `fix.jq` runs once per matching file; fine-grained "fix only the third element" is not in scope. Use the violation discriminator inside the jq expression instead.
- **Explicit ops vocabulary.** No `kind: replace` / `kind: insert` / `kind: delete` syntax — whole-document jq subsumes the use case for M10's standard ruleset.
- **Comment preservation through `dq fix`.** None — same trade-off as `dq set --jq`.
- **Compile-time idempotency proof.** Idempotency is enforced at runtime by `Fixer::apply` (the second-application check). Static analysis of the jq expression to prove idempotency in advance is out of scope.
- **Migration of every existing `@std` rule to ship a fix.** M10 ships two proofs (`k8s.image-pull-policy-always`, `npm.has-license`). Migrating the remaining 44 rules is incremental community / maintainer work post-M10.

## Impact

- **New code (`dq-exec`)**:
  - `crates/dq-exec/src/fixer.rs` — `Fixer`, `FixOutcome`, the per-rule pipeline, idempotency check, eight unit tests.
  - `crates/dq-exec/src/rule.rs` — typed `RuleFix` struct + tests for the new schema.
  - `crates/dq-exec/src/error.rs` — `ExecError::FixApply` variant + `kind_name` test.
  - `crates/dq-exec/src/evaluator.rs` — `CompiledRule.fix_engine` field, `compile_rule` extension, `pub(crate) compiled_rules()` accessor for `Fixer`.
  - `crates/dq-exec/src/lib.rs` — re-exports `Fixer`, `FixOutcome`, `RuleFix`.
- **`dq-cli` updates**:
  - `crates/dq-cli/src/commands/fix.rs` — handler + 7 unit tests.
  - `crates/dq-cli/src/cli/args/fix.rs` — `FixArgs` struct.
  - `crates/dq-cli/src/cli/args.rs` — `Command::Fix(FixArgs)` variant.
  - `crates/dq-cli/src/cli/mod.rs` — re-export `FixArgs`.
  - `crates/dq-cli/src/lib.rs` — dispatcher entry for `Command::Fix`.
  - `crates/dq-cli/src/commands/mod.rs` — `pub mod fix`.
  - `crates/dq-cli/src/commands/lint_core.rs` — `expand_lint_inputs` becomes `pub(crate)` so `commands::fix` can re-use it.
  - `crates/dq-cli/src/exit_code.rs` — route `ExecError::FixApply` to `PARSE_ERROR`.
- **`dq-lint` updates**:
  - `crates/dq-lint/rules/k8s/image-pull-policy-always.yml` — adds `fix:` block.
  - `crates/dq-lint/rules/npm/has-license.yml` — adds `fix:` block.
- **Backward compatibility**: `RuleFix` is a typed schema replacement for the previous opaque YAML field. Pre-M10 rules that shipped a `fix:` block with arbitrary content (none in `@std`) will now fail to load with an `unknown_field` error. No reporters consumed `Diagnostic.fix` in the wild, so the type change is invisible at the reporter layer.
- **Project meta**: `dq-plan.md` M10 marker; `README.md` status line + four new `dq fix` examples.
