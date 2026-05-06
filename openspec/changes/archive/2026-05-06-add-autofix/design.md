## Context

The dq-plan.md M10 envelope phrases the fix payload as "трансформация (jq-выражение или явный набор ops)". Two designs were on the table:

1. **Whole-document jq transform.** `fix: { jq: "..." }`. Single output, single application per matching file.
2. **Explicit ops vocabulary.** `fix: { kind: replace, path: ".spec.containers[].image", with: "..." }` and friends.

## Decision

Ship the whole-document jq transform. Defer the ops vocabulary indefinitely.

## Rationale

The ops vocabulary triples the implementation surface — every op needs validation, idempotency proof, fixture format extension, and exit-code mapping — for use cases the jq form already handles cleanly. Both proof-of-concept rules in M10 (`k8s.image-pull-policy-always`, `npm.has-license`) compress to a single line of jq with the `walk(...)` / `if ... then ... else . end` patterns; an ops vocabulary would not have improved the rule-author experience for either.

The trade-off is comment loss on re-emit: `dq fix` routes through `Format::write_with_options`, same as `dq set --jq`, and drops comments / preserved formatting. M10 documents this in the `dq fix` handler module-doc and in the README so users opt in with eyes open. Comment-preserving fixes are an M11+ refinement (would require a comment-preserving emitter, which today exists only for the textual-edit splice path used by `dq set FILE POINTER VALUE`).

## Consequences

- **Idempotency is enforced at runtime, not statically.** `Fixer::apply` runs `fix.jq` twice and compares the outputs; a non-idempotent rule is skipped with a `tracing::warn!` log line and surfaced in `FixOutcome.skipped_non_idempotent`. This is cheap (one extra jq evaluation per fixable file) and catches the bug class. Static analysis of the jq expression to prove idempotency in advance is out of scope.
- **No new CLI exit code.** `dq fix --check` reuses `crate::error::CheckPending` (already mapped to exit 1), same as `dq set --check` / `dq fmt --check`. `ExecError::FixApply` shares `PARSE_ERROR` (3) with `RuleCompile` because both are "the rule author shipped buggy jq".
- **Comment-preserving autofix is an M11+ refinement.** When the M11 textual-edit splice path lands for re-emit, `dq fix` can be retrofitted to use it for value-preserving fixes; the M10 contract is "best-effort within `Format::write_with_options`" per the dq-plan.md M10 phrasing "форматирование вокруг исправления остаётся прежним (best-effort)".
