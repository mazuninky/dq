## 1. dq-exec — typed `RuleFix` schema

- [x] 1.1 `crates/dq-exec/src/rule.rs`: replace `Rule.fix: Option<serde_yml::Value>` with `Rule.fix: Option<RuleFix>` where `RuleFix { jq: String }` carries `#[serde(deny_unknown_fields)]`. Replaced the `parses_fix_field_as_opaque_yaml` test with `parses_fix_jq_field`, `rejects_unknown_field_in_fix`, and `rejects_fix_missing_jq`.
- [x] 1.2 `crates/dq-exec/src/diagnostic.rs`: change `Diagnostic.fix` type from `Option<serde_yml::Value>` to `Option<RuleFix>`. Existing reporters do not consume this field, so no reporter changes are required.

## 2. dq-exec — Fixer runtime

- [x] 2.1 `crates/dq-exec/src/evaluator.rs`: make `CompiledRule` `pub(crate)` with `pub(crate)` fields, add `fix_engine: Option<JqEngine>`, compile alongside the other engines in `compile_rule`. Add `pub(crate) Evaluator::compiled_rules()` accessor for the fixer.
- [x] 2.2 `crates/dq-exec/src/fixer.rs` (new): `Fixer::new(&Evaluator)`, `Fixer::apply(path, value, format_name) -> Result<FixOutcome>`, `FixOutcome { fixed, new_value, applied_rules, skipped_non_idempotent }`. The `apply` pipeline gates on format/glob/filter, requires `check.jq` to find at least one violation, runs `fix.jq`, enforces single-output arity, runs the idempotency check (apply twice, compare), and either adopts or skips. Eight unit tests covering happy path, non-idempotent skip, no-fix rule, no-violation skip, identity fix, declaration-order, multi-output rejection, and format mismatch.
- [x] 2.3 `crates/dq-exec/src/error.rs`: add `ExecError::FixApply { rule_id, message }`, extend `kind_name()` to return `"fix_apply"`, add a `kind_name_covers_fix_apply_variant` test.
- [x] 2.4 `crates/dq-exec/src/lib.rs`: register `mod fixer` and re-export `Fixer`, `FixOutcome`, `RuleFix`.

## 3. dq-cli — `dq fix` command

- [x] 3.1 `crates/dq-cli/src/cli/args/fix.rs` (new): `FixArgs { files, rules }` mirroring `LintArgs` shape.
- [x] 3.2 `crates/dq-cli/src/cli/args.rs`: declare `mod fix`, re-export `FixArgs`, add `Command::Fix(FixArgs)` variant. `crates/dq-cli/src/cli/mod.rs`: extend the public re-export list with `FixArgs`.
- [x] 3.3 `crates/dq-cli/src/commands/fix.rs` (new): handler. Calls `cli.ensure_write_flags_consistent()`, rejects `--allow-templates` / `--raw-template-strings`, expands input globs via the read-mode helper from `lint_core`, builds `Evaluator` + `Fixer`, dispatches a `FixFileOp` through `bulk::run_per_file`. Per-file `apply`: parse, jq-evaluate via `Fixer`, re-emit through `Format::write_with_options`, compute optional unified diff. Seven unit tests covering `--diff`, `--check` (`CheckPending`), `-i`, both template-guard rejections, no-matching-rules → `Unchanged`, default stdout path.
- [x] 3.4 `crates/dq-cli/src/commands/mod.rs`: `pub mod fix`.
- [x] 3.5 `crates/dq-cli/src/lib.rs::dispatch`: route `Command::Fix(args)` to `commands::fix::run(cli, args, input_format, use_color, out)`.
- [x] 3.6 `crates/dq-cli/src/commands/lint_core.rs`: change `expand_lint_inputs` from private to `pub(crate)` so the fix handler can reuse the same read-mode glob expansion semantics.
- [x] 3.7 `crates/dq-cli/src/exit_code.rs`: route `ExecError::FixApply` to `PARSE_ERROR` (3) and add a regression test.

## 4. dq-lint — proof rules

- [x] 4.1 `crates/dq-lint/rules/k8s/image-pull-policy-always.yml`: add `fix.jq` walking the AST and rewriting `imagePullPolicy: Always` to `IfNotPresent` on container objects with non-`:latest` images. Idempotent: after the swap the predicate no longer holds.
- [x] 4.2 `crates/dq-lint/rules/npm/has-license.yml`: add `fix.jq` setting `.license = "UNLICENSED"` when missing or empty on a non-private package. Idempotent: the predicate no longer holds.

## 5. Docs

- [x] 5.1 `dq-plan.md` M10 section header: append `✅ Implemented 2026-05-05 (см. [openspec/changes/add-autofix/](openspec/changes/add-autofix/))`.
- [x] 5.2 `README.md` status line: update from `M9 alpha — adds markdown AST + @std/markdown` to `M10 alpha — adds dq fix autofix engine`. Add four `dq fix` examples (`--check`, `--diff`, `-i --rules @std/k8s`, `-i --rules @std/npm`) to the examples block.

## 6. Validation

- [x] 6.1 `cargo fmt --all -- --check` is clean.
- [x] 6.2 `cargo clippy --workspace --all-features -- -D warnings` is clean.
- [x] 6.3 `cargo test --workspace --all-features` is green: 966 passed, 0 failed (was 700+ before M10; M10 added the fixer and CLI tests on top).
- [x] 6.4 Manual smoke (release): `dq fix --check` returns exit 1 when fixes are pending; `dq fix --diff` renders a unified diff without writing; `dq fix -i` writes atomically; second `--check` returns exit 0 (idempotency).
- [x] 6.5 Manual smoke: `dq fix --rules @std/k8s deploy.yaml` and `dq fix --rules @std/npm package.json` apply the two new fix blocks correctly.
- [x] 6.6 Existing `@std` rule fixtures still pass (`dq test crates/dq-lint/rules/`): 115 / 115.
