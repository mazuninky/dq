# Rules as gates: `dq lint` fails closed, rules with `match.glob` can be tested

**Status: in progress (2026-11-22).** From feedback on replacing hand-written CI checks with dq rules. Phase 1 is the blocker.

## Problem

A check that guards a repository must not be able to die without a line of output. Reproduced on the released `dq` 2026.47.3:

| what is wrong | what dq does today |
|---|---|
| a rule's `check.jq` or `match.filter` raises at run time (a typo, an unexpected shape) | `WARN … skipping rule for this file`, `diagnostics: []`, exit 0; with `-q` not even the warning |
| `match.format: [jsn]`, a format dq does not have | the rule never applies; no output, exit 0 |
| `--rules DIR` with no rule file in it | `diagnostics: []`, exit 0 |
| a rule with `match.glob` in `dq test` | the fixture is evaluated under the path of its own `*.test.yml`, the glob excludes it, the rule reports nothing |
| a rule file that holds a list of rules | `rule #1 failed to parse: invalid type: sequence`, though [reference/lint-engine.md](../reference/lint-engine.md) says a file may hold a list |
| `.npmrc` | `unsupported format '.npmrc'`; read as INI it is wrong: `@scope:registry=https://…` becomes the key `@scope`, because INI also splits at `:` |
| `IN` in jq | `undefined filter`; jaq does not have it |

The first three are the same class: the run says "no findings" when it means "nothing was checked". A jq *compile* error already exits 3, and a file that does not parse is already a `lint.parse` result and exit 3. A rule that raises is the odd one out.

Skipping a rule that raises was deliberate: the reference said "one bad rule cannot sink a whole run". That reason holds for the *other* rules and files; it does not justify exit 0.

## Design

### 1. A rule that raises fails the run

Same shape as a file that does not parse:

- The other rules and files are still evaluated and the report is written.
- Each rule error is a result under `lint.rule_error`, severity `error`, on the file it was raised for, in every structured format, so SARIF, JSON and JUnit carry it. Message: `rule 'ID' did not run: check.jq: jq runtime error: …`. One result per rule, file and field, however many documents of a stream raised.
- The run exits 3, as for a rule that does not compile. It outranks findings (4), as `lint.parse` does.
- The console prints the findings and then the error on stderr, also under `-q`.

What counts: `match.filter` and `check.jq`, where the rule is skipped and a finding can be lost. `loc.file` and `loc.line` that raise only move a finding; they stay a warning.

**Default on, with `--allow-rule-errors` to get the old behaviour** (a warning, exit code unaffected). A flag that turns the check on would be one more thing whose removal breaks nothing, which is the problem being solved. `@std` rules do not raise on unexpected shapes (a test sweeps them), so a run with only `@std` rules does not change.

This applies to `lint`, `check` and `fix`: a rule whose fix is skipped because it raised makes `fix --check` pass for the wrong reason. `fix` still applies the other rules' fixes and exits 3 at the end.

### 2. Rules that can never apply

- An unknown name in `match.format` is a rule parse error (exit 3) naming the rule, the bad name and close matches.
- An empty `match.format` likewise.
- An explicit `--rules DIR` without a single rule file is exit 6, as an unknown rule is. `./.dq/rules/` that is found automatically may be empty.

A rule whose formats or glob match none of the files is not an error: one rule set over several runs is normal.

### 3. Fixtures with a path

A test case takes an optional `path`:

```yaml
tests:
  - name: install script is refused
    path: packages/a/package.json
    input: '{"scripts": {"postinstall": "node x.js"}}'
    expected:
      violations: [{ rule: org.no-install-scripts }]
```

The case is evaluated under that path: `match.glob` is matched against it and diagnostics carry it. Nothing is read from disk. Without `path` it stays the fixture's own path. Without `format`, the format is the one the path implies, else the rule's first `match.format`, as today.

### 4. Rule files with a list

A document of a rule file may be a list of rules, as the reference says. A stream of documents keeps working, and both can be mixed. An error names the rule by its position.

### 5. `.npmrc`

A format of its own, `npmrc`, detected by file name. npm reads `key = value` lines split at the first `=` only, `;` and `#` comments, `key[] = value` arrays, and `${VAR}` left as written. Read-only at first (`get`, `query`, `lint`, …); writing is refused as for other read-only formats. INI itself does not change.

### 6. `IN` and `INDEX`

The two SQL-style operators of jq 1.5 and later, added to dq's jq prelude with jq's own definitions. The other jq builtins jaq lacks are listed in [reference/jq.md](../reference/jq.md).

## Phases

1. **Fail closed** (1 and 2): **done.** `dq-exec`, `dq-cli`, reference docs, CHANGELOG "Changed". Also: rule directories follow links (rules added with `--symlink` were never loaded) and take `.yml` / `.yaml` in any case; `main` no longer holds the stderr lock, which hung `fix --parallel`.
2. **Rule authoring** (3 and 4): `dq-exec`, reference docs. Also: `match.glob` ignores a leading `./`; a `dq test` case whose `format` the rule does not match is an error.
3. **`.npmrc`, `IN`, `INDEX`** (5 and 6): `dq-core`, `dq-transform`, reference docs.

## Compatibility

- A run that passed with a `WARN … raised a runtime error` now exits 3. The usual fix is to guard the expression (`type == "object" and .kind == "Pod"`, or `.kind? == "Pod"`).
- A rule with an unknown format name used to load; it is now an error.
- The `WARN` line of a rule error is replaced by the result and the error, except under `--allow-rule-errors`.
- Exit codes keep their meaning: 3 already covers invalid rules.

## Alternatives rejected

- **A flag that makes rule errors fatal.** See above.
- **Exit 4.** It means "the document failed a check". Here no check was made.
- **Stopping at the first rule error.** The other findings are still worth having in the report.
- **Failing a run in which no rule applied to any file.** Shared rule sets make that a normal run.
