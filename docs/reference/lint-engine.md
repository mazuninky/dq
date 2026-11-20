# Lint engine

dq's second job, next to editing data, is to be a platform for linters over
any format it reads: rules are YAML files with jq (or JSON Schema) checks,
optional autofixes, and fixture tests, with no Rust needed. The runtime is
the `dq-exec` crate (`crates/dq-exec/src/`); the standard library of rules is
`dq-lint` ([std-rules.md](std-rules.md)); the check kinds are detailed in
[rule-checks.md](rule-checks.md).

## Commands

| Command | Purpose |
|---|---|
| `dq lint <files/globs>… [--rules SRC]…` | Run rule sets over files. |
| `dq check (--rule ID-OR-PATH \| --inline YAML) <files>…` | Run one rule. No auto-binding. |
| `dq fix <files/globs>… [--rules SRC]…` | Apply rule fixes; write modes as in [write-commands.md](write-commands.md). |
| `dq test <dir \| @std/NS \| @std>` | Run every `*.test.yml` fixture under a directory, or the embedded fixtures of a standard namespace (`@std`: of every namespace). |
| `dq explain <rule-id>` | Description, severity, references of a rule (`console` or `json`). |
| `dq rules list [--namespace NS]` | List `@std` rules (id, namespace, severity, source). |
| `dq rules add <@std/NS \| path> [--force] [--symlink]` | Copy (or symlink, Unix) a rule set into `./.dq/rules/<ns>/` for local override or vendoring. |

`dq check` takes the rule through `--rule` (a rule file path or an id such as
`k8s.no-latest-tag`), not as a positional argument; with neither flag it
exits 6.

Exit status of `lint` / `check`: 4 if any diagnostic has severity `error`;
with `--strict`, 1 if there are `warn` findings and no errors; otherwise 0.
`info` never fails a run. Rule-loading problems exit 3 (bad rule) or 6
(unknown rule / namespace).

## Rule files

One rule per `*.yml` file (a file may also hold a list of rules). Fields are
checked strictly (`deny_unknown_fields` on the rule and on `match`, `check`,
`loc`, `fix`): a typo is a parse error naming the field, and the loader
reports every broken rule of a set at once rather than stopping at the first.

```yaml
id: org.no-debug-images          # required; <namespace>.<kebab-name> by convention
description: |                   # required; shown by `dq explain`
  Debug and nightly images must not reach the cluster.
severity: error                  # required; error | warn | info
match:                           # required
  format: yaml                   # required; a format name or a list: [yaml, json]
  filter: '.kind == "Pod"'       # optional jq; the rule applies when the first output is truthy
  glob: 'k8s/**/*.yaml'          # optional; matched against the file path
check:                           # required; exactly one kind, see rule-checks.md
  jq: '.spec.containers[]? | select(.image | test("debug|nightly"))'
  message: "container '{{ .name }}' uses {{ .image }}"
loc:                             # optional; where to point the diagnostic
  pointer: '"/spec/containers/" + (.idx | tostring)'
fix:                             # optional; see "Fixes"
  ops: '[...]'
references:                      # optional; URLs shown by `dq explain`
  - https://example.org/policy
```

`match.format` uses the parser names from [formats.md](formats.md) (`yaml`,
`json`, `toml`, `markdown`, `dockerfile`, `hcl`, `xml`, …). All jq strings
(`filter`, `check.jq`, `extract`, `loc.*`, `fix.*`) and `glob` are compiled
once when the evaluator is built; a compile error names the rule (exit 3).

## Evaluation

`Evaluator::evaluate_file(path, &Ir, format_name) -> Vec<Diagnostic>` runs
every rule in declaration order:

1. **Format**: skip unless the file's format is in `match.format`.
2. **Glob**: skip unless `match.glob` (if set) matches the path.
3. **Filter**: run `match.filter` (if set); skip when there is no output or
   the first output is `false` or `null`.
4. **Check**: dispatch on the check kind. For `jq`, **every value the
   expression emits is one violation** — including `true`, `false` and
   `null`. Emit only failures (`select(...)`, `empty` when fine); an
   expression that returns a boolean fires on every file.
5. **Diagnostic**: render the message, resolve the position, attach
   severity, references and fix payload.

The result is deterministic: rule declaration order, then the order of the
violation stream (for composite rules: extract item order, then the nested
rule's own order).

A rule whose jq raises a runtime error on a particular input (a filter or
check that assumed a field's type) is skipped for that file, so one bad rule
cannot sink a whole run; a `loc.file` or `loc.line` that raises falls back to
the file under check or the document start. The evaluator returns each of
these as a rule error beside the diagnostics (`Evaluator::evaluate`). `dq lint`
prints one warning per rule error on stderr
(`WARN check.jq raised a runtime error; skipping rule for this file
rule_id=… error=jq runtime error: …`) and does not change its exit code;
`dq test` fails the case (see below). The flip side, noted in
[../roadmap.md](../roadmap.md): `@std` rules that raise on unexpected shapes
(e.g. `kind: Pod` with `spec: x`) report nothing for that file beyond the
warning.

### Diagnostics

```rust
pub struct Diagnostic { rule_id, severity, message, file: Option<Utf8PathBuf>,
                        line: u32, col: u32, span: Option<Range<usize>>,
                        references: Vec<String>, fix: Option<RuleFix> }
```

Serialised for reporters as `{path, line, col, message, severity, rule_id,
references}` inside `{"diagnostics": [...]}` — the shape the SARIF, JUnit
and TAP reporters consume ([cli.md](cli.md#output-formats--f)).

### Message templates

`message` supports a minimal mustache subset over the violation value:
`{{ . }}` (the whole value as compact JSON), `{{ .field }}`, `{{ .a.b }}`,
`{{ .arr.0 }}`. Whitespace inside the braces is ignored. A path that does
not resolve renders as `<missing>`. No conditionals, loops or helpers.

### Positions and `loc`

Without `loc`, a diagnostic lands at the start of the document (`1:1`, or the
first line of its document in a YAML stream). `loc` overrides it with jq
expressions evaluated against the violation value:

1. `loc.pointer` (preferred) must produce a JSON Pointer string; it is looked
   up in the input IR's provenance and gives an exact `line:col` when the
   format has spans (YAML, JSON, TOML, Dockerfile).
2. `loc.line` (deprecated, kept for older rules) produces a positive integer
   line; used when `loc.pointer` is unset or does not resolve.
3. Otherwise the document's origin, as above.

`loc.file` independently overrides the reported file (for rules that check a
generated file but want to point at its source); an empty or non-string
result keeps the file under check. Schema and composite checks compute their
positions themselves ([rule-checks.md](rule-checks.md)).

Formats without spans (HCL, INI, …) report line 1 even with `loc.pointer`.

## Where rules come from

`RuleLoader::resolve` takes the `--rules` values, the working directory and
the set of formats present in the inputs:

- With `--rules`, each value is `@std/<namespace>`, a rule file, or a
  directory (every `*.yml` in it except `*.test.yml`). Only those are used.
- Without `--rules` (`lint`, `fix`), dq auto-binds every `@std` namespace that
  has a rule for one of the input formats, plus every rule under
  `./.dq/rules/` if that directory exists. Rules there need no namespace
  prefix.
- Duplicate sources are loaded once.
- An unknown `@std/<name>` is exit 6 with up to three Levenshtein-2
  suggestions.

`dq explain` resolves `@std/<ns>.<rule>`, `<ns>.<rule>` for a known
namespace, or an exact id among loaded rules; an unknown id exits 6 with
suggestions.

## Fixture tests (`dq test`)

Next to every rule `foo.yml` sits `foo.test.yml`:

```yaml
tests:
  - name: latest tag fires
    input: |
      kind: Pod
      spec: { containers: [ { name: app, image: app:latest } ] }
    # format: yaml        # optional; defaults to the rule's first match.format
    expected:
      violations:
        - rule: k8s.no-latest-tag
          message_contains: "uses :latest"   # optional
          # message_equals: "…"              # optional
          # line: 3                          # optional
  - name: pinned tag is fine
    input: "kind: Pod\nspec: { containers: [ { image: app:1.2 } ] }\n"
    expected:
      violations: []
```

`dq test <dir>` finds every `*.test.yml` recursively, loads the sibling rule,
parses `input`, evaluates it under the fixture file's own path (so a
`match.glob` is matched against that path, and a rule it excludes reports
nothing), and compares order-insensitively: a case passes
when every expected violation matches an actual diagnostic (same rule id,
and `message_contains` / `message_equals` / `line` when given) and no actual
diagnostic is left unmatched and no rule raised a jq runtime error on the
input. `input` is read by the format's reader as a file would be, so a
`line` resolved through `loc.pointer` is the one `dq lint` reports where
that reader records positions (Dockerfile); the YAML and JSON readers it
uses do not, and such a line stays 1 in a fixture even though `dq lint`
reports the real one. Failures print the missing and the extra diagnostics
and each rule error as `raised: rule=<id> <field>: <jq error>` (a `raised`
list in `-F json` and `-F tap`), so a fixture case catches a rule that
raises even when it expects no violation. The summary line reads `summary: N passed, F failed, E errored,
T total`. `dq test @std/<ns>` runs the fixtures embedded in the binary for one
standard namespace against the embedded rules (labelled
`@std/<ns>/<rule>.test.yml`), and `dq test @std` those of every namespace; an
unknown namespace is `unknown rule @std/<ns>` (exit 6). In a checkout,
`dq test crates/dq-lint/rules/` runs the same fixtures from the source tree;
the `cargo test -p dq-lint` suite runs both.

## Fixes

A rule may carry `fix:` with `ops`, `jq`, or both (at least one; `fix: {}` is
a parse error). `dq fix` applies, per file and in rule order, the fix of every
rule that matches and currently reports at least one violation.

### `fix.ops` (preferred)

A jq expression over the document that must produce **one** value: a JSON
Patch array limited to `add`, `replace`, `remove` (an `EditScript`, see
[ir-and-edit-ops.md](ir-and-edit-ops.md)).
It is applied as a textual edit, so comments and layout survive. On HCL,
XML, INI, dotenv, CSV/TSV and JSON Lines, which have no textual edits, the script changes
the value tree and the file is re-emitted through its writer, as `fix.jq`
does: comments go (XML keeps its own), and an op whose result the writer
cannot hold fails the rule with the writer's error (exit 3).

1. Zero or several outputs, a non-array, or an op outside the subset →
   `FixApply` error (exit 3).
2. An empty script → the rule is already satisfied; skipped.
3. The script is applied to a clone of the document.
4. Idempotency: the ops expression is evaluated again on the result; if it
   yields a non-empty script the rule is not idempotent — the clone is
   discarded and the rule is reported as skipped (non-idempotent).
5. If the bytes did not change, the rule is silently skipped; otherwise the
   result is kept and the rule counted as applied.

An edit the textual writer cannot make in place (`edit_rejected`) or one on a
document that a previous `fix.jq` turned read-only is logged and skipped,
leaving the finding in place.

### `fix.jq` (legacy)

A whole-document jq transform producing exactly one value, which replaces the
document. It is applied only if applying it twice gives the same result
(otherwise skipped with a warning). It re-emits through the format's writer,
so **comments in the re-emitted content are dropped**. On a YAML file the
directives, the `---` line and the comment lines around the content are kept.
After a `fix.jq` the document is value-only, so later `fix.ops` rules on the
same file are skipped (except in a YAML stream, see below). When a rule has
both, `ops` wins and a warning notes that `jq` was shadowed.

`dq fix` refuses `--allow-templates` and `--raw-template-strings` (exit 6)
and otherwise follows the write modes and the bulk driver of the other write
commands: `--check` (exit 1 if a fix would apply), `--diff`, `-i`,
`--backup`, `--parallel`, `--continue-on-error`.

## YAML streams

`lint`, `check` and `fix` treat every document of a multi-document YAML file
as a file of its own: rules match, check, locate and fix one document at a
time, and diagnostics carry the line in the file. A diagnostic with no
position lands at its document's first line; an empty document (`---` and
nothing) reports at its own `---` line. `--doc N` limits the run to one
document.

When fixing a stream:

- `fix.ops` scripts are rebased under `/N` and applied in place; every other
  document and every `---` keep their bytes.
- `fix.jq` re-emits only its own document's content (in the file's line
  ending, CRLF kept); the document's `---` line (with any `!tag` / `&anchor`
  on it, while they still fit), the comment and blank lines before the next
  `---`, and all other documents keep their bytes. The stream stays editable,
  so later `fix.ops` rules still apply.
- Whole-document ops (path `""`): `replace ""` / `add ""` rewrite the
  document through the YAML writer, keeping its `---` line and the comment
  lines around it, and apply like any other op. `remove ""` drops the
  document with its `---` line. It is a `FixApply` error (exit 3) in a
  single-document file, on the last document left in a stream, or when
  another op follows it in the same script.

Plugins run once per document of a stream as well; see
[../plugin-abi.md](../plugin-abi.md).

## Plugins

`--plugins <DIR>` adds sandboxed WASM lint/fix plugins next to the YAML
rules (experimental, behind the `plugins` cargo feature). The contract is in
[../plugin-abi.md](../plugin-abi.md).
