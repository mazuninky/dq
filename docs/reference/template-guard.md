# Template guard

Helm charts, Argo and Flux manifests and GitHub Actions workflows often hold
Go-template (`{{ .Values.x }}`) or expression (`${{ github.ref }}`) syntax.
Such a file is not valid YAML (or it parses into something unintended, such
as a flow mapping), and editing it in place would damage the templates. The
guard catches this before parsing and makes the user choose.

Implementation: `crates/dq-core/src/template_guard.rs`.

## Where it runs

The guard runs in the textual-edit write commands: `set`, `del`, `patch`,
`merge`. Read commands (`get`, `paths`, `convert`, `validate`, …) parse the
file directly, without the guard and without the placeholder substitution, so
`dq get values.yaml /image` on `image: {{ .Values.x }}` reads the value YAML
sees (a flow mapping), exit 0. The original design had every reading command
run the guard; that was not carried through (see
[../roadmap.md](../roadmap.md)).

`set --jq` and `fix` refuse both escape hatches (exit 6), because their
re-emit path cannot restore template placeholders.

## Detection

`detect_templates(bytes)` looks for `{{` (optionally followed by `-` and
blanks) and then a word character or `.`, in a structural position: at the
start of a line's content or after `:`, `,`, `-` or whitespace where a
value starts. `{{` inside a quoted string literal does not trigger. `${{` is
detected as well: at this level a GitHub Actions expression cannot be told
from a Go template, so both need an explicit choice. The marker reports its
line and the offending line (trimmed, at most 80 characters).

## Default: refuse

A detected template makes the command fail with `TemplatedFile` (exit 3). The
message names both escape hatches and what each costs.

## `--raw-template-strings`

Every `{{ … }}` / `{{- … -}}` span is replaced by a placeholder
`__DQ_TPL_<N>__` (N counts from 0, unique in the file, the surrounding YAML /
JSON quoting kept) before parsing, and restored on output. The file round
trips: `dq set values.yaml /image/tag v1.2.3 -i --raw-template-strings`
changes the tag and leaves every template line byte-identical. Placeholders
never reach the disk.

## `--allow-templates`

Skips the guard and hands the raw bytes to the parser, with a warning that
round trip is not guaranteed. It works when the templates happen to be valid
YAML (GitHub Actions `${{ … }}` inside strings); with bare Helm templates the
parser fails with its own parse error (exit 3).

The two flags are mutually exclusive (clap usage error, exit 2).
