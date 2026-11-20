# Template guard

Helm charts, Argo and Flux manifests and GitHub Actions workflows often hold
Go-template (`{{ .Values.x }}`) or expression (`${{ github.ref }}`) syntax.
Such a file is not valid YAML (or it parses into something unintended, such
as a flow mapping), and editing it in place would damage the templates. The
guard catches this before parsing and makes the user choose.

Implementation: `crates/dq-core/src/template_guard.rs`.

## Where it runs

Every command that parses a file runs the guard, with one rule per kind of
command:

- **Textual-edit writes** — `set`, `del`, `patch`, `merge` — refuse any file
  the detection below matches, before parsing: the edit splices text into
  the file, so the user has to choose how its templates are handled.
- **Everything else that reads a file** — `get`, `exists`, `keys`,
  `values`, `len`, `type`, `paths`, `select`, `query`, `convert` (to stdout
  and `-i`), `validate`, `diff`, `lint`, `check`, `fmt`, `fix`, `set --jq`,
  and the file
  `set --value-from` / `@file` reads — refuses a file only when its
  templates change what the parser reads. When the detection matches, the
  file is parsed twice: as it is, and with every template block swapped for
  a placeholder. If both parse and hold the same data once each placeholder
  is put back as the template text, the templates do not matter to the
  reading and the command goes on with the file as it is. A GitHub Actions
  workflow (`run: echo ${{ github.ref }}`, a plain scalar) and a `{{ … }}`
  inside a quoted string pass this way, in every format. A Helm
  `port: {{ .Values.port }}`, which YAML reads as a flow mapping, or a
  `{{- if … }}` line, on which the parser fails, is refused.

`lint` and `check` treat a refused file as they treat a file that fails to
parse: the run stops with exit 3. There is no lint diagnostic of its own for
templates.

`validate` keeps the guard's exit 3 for a refused file, not its own 4: a
templated file is neither valid nor invalid until the user picks an escape
hatch.

`set --jq`, `fix` and `fmt` refuse both escape hatches (exit 6): they
re-emit the whole file through the format's writer, which cannot restore
template syntax.

## Detection

`detect_templates(bytes)` looks for `${{` or for `{{`, an optional `-` trim
marker, blanks, and then a `.`, a word character or a `$` (`{{ .x }}`,
`{{- if .x }}`, `{{ $v := … }}`). It matches anywhere in the file, quoted
strings included; `{{}}` and `{{ }}` alone do not match. At this level a
GitHub Actions expression cannot be told from a Go template, so `${{` is
detected as well. The marker reports its line and the offending line
(trimmed, at most 80 characters).

## Default: refuse

A detected template makes the command fail with `TemplatedFile` (exit 3). The
message names both escape hatches and what each costs.

## `--raw-template-strings`

Every `{{ … }}` / `${{ … }}` / `{{- … -}}` span is replaced by a placeholder
`__DQ_TPL_<N>__` (N counts from 0, unique in the file, the surrounding YAML /
JSON quoting kept) before parsing, and restored on output. The file round
trips: `dq set values.yaml /image/tag v1.2.3 -i --raw-template-strings`
changes the tag and leaves every template line byte-identical. Placeholders
never reach the disk.

On a reading command each template is read as the string it is written as:
`dq get values.yaml /image --raw-template-strings` on `image: {{ .Values.x }}`
prints `{{ .Values.x }}`, and `convert` writes it as that string. Lines in
`lint` positions match the file (a template never spans a line break);
columns after a template on the same line count the placeholder's length.

## `--allow-templates`

Skips the guard and hands the raw bytes to the parser; on a write command
round trip is not guaranteed, and a reading command sees what the parser
reads (`image: {{ .Values.x }}` is a flow mapping in YAML). It works when the templates happen to be valid
YAML (GitHub Actions `${{ … }}` inside strings); with bare Helm templates the
parser fails with its own parse error (exit 3).

The two flags are mutually exclusive (clap usage error, exit 2).
