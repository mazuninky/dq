# dq documentation

Contributor documentation for dq. The user guide is the repository
[README](../README.md); release notes are in [CHANGELOG.md](../CHANGELOG.md).

## Start here

- [architecture.md](architecture.md) — principles, crate layering, how data
  flows through parse, read, write and lint.
- [roadmap.md](roadmap.md) — what's next: active plans, open bugs, gaps,
  deferred work, and what the project decided not to do.
- [decisions.md](decisions.md) — dated log of the shipped changes: what each
  did, the decisions behind it, what it left out.
- [plugin-abi.md](plugin-abi.md) — the `dq:plugin@0.1.0` WASM plugin contract
  (with [the WIT file](../crates/dq-plugin/wit/dq-plugin.wit)).

## Plans

Work in progress, with design and remaining steps.

- [plans/add-std-skills-rules.md](plans/add-std-skills-rules.md) — `@std/skills`
  rules for `SKILL.md` frontmatter and `evals.json`.
- [plans/perf-jq-compile-cache.md](plans/perf-jq-compile-cache.md) — compile
  each jq expression once per evaluator.
- [plans/perf-json-parse-linear.md](plans/perf-json-parse-linear.md) —
  linear-time JSON span map.
- [plans/perf-pointer-recursive-walks.md](plans/perf-pointer-recursive-walks.md)
  — linear-cost pointers in `diff` / `merge` walks.

## Reference

How dq behaves today.

- [reference/cli.md](reference/cli.md) — commands, global flags, `-F`, output
  formats, errors, exit codes.
- [reference/formats.md](reference/formats.md) — value model, number
  precision, detection, per-format behaviour, write options.
- [reference/paths.md](reference/paths.md) — JSON Pointer, JSONPath, jq: which
  path language where.
- [reference/read-commands.md](reference/read-commands.md) — `get`, `exists`,
  `keys`, `values`, `len`, `type`, `paths`, `select`, `query`, `convert`,
  `validate`, multi-document YAML and `--doc`.
- [reference/write-commands.md](reference/write-commands.md) — textual edits vs.
  re-emit, output modes, atomic writes, `set`, `del`, `set --jq`, `patch`,
  `merge`, `diff`, `convert -i`, YAML streams.
- [reference/bulk.md](reference/bulk.md) — globs, `--check`,
  `--continue-on-error`, `--parallel`, the summary line.
- [reference/template-guard.md](reference/template-guard.md) — Helm / Go-template
  detection and the two escape hatches.
- [reference/fmt.md](reference/fmt.md) — `dq fmt`, `--sort-keys`, `--indent`.
- [reference/jq.md](reference/jq.md) — the embedded jq engine (jaq), its API,
  errors and number handling.
- [reference/ir-and-edit-ops.md](reference/ir-and-edit-ops.md) — IR, provenance,
  inline offsets, `EditScript`.
- [reference/lint-engine.md](reference/lint-engine.md) — rule files, evaluation,
  messages, `loc`, rule loading, fixtures, fixes, YAML streams.
- [reference/rule-checks.md](reference/rule-checks.md) — the check kinds: jq,
  JSON Schema, composite rules.
- [reference/std-rules.md](reference/std-rules.md) — the `@std` rule library.
- [reference/distribution.md](reference/distribution.md) — versioning, release
  and Docker workflows, installers, self-update, CI.

## Archive

Historical documents in [archive/](archive/), kept for context:

- [archive/dq-plan-2026-05.md](archive/dq-plan-2026-05.md) — the original
  design document and milestone plan (M1–M12), in Russian. Its principles are
  in [architecture.md](architecture.md), its open items in
  [roadmap.md](roadmap.md).
- [archive/plan-validation-2026-05-03.md](archive/plan-validation-2026-05-03.md)
  — a review of the original plan's engineering conventions (May 2026).
