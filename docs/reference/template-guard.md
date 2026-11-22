# Template guard

Helm charts, Argo and Flux manifests and GitHub Actions workflows often hold
Go-template (`{{ .Values.x }}`) or expression (`${{ github.ref }}`) syntax.
Such a file is not valid YAML (or it parses into something unintended, such
as a flow mapping), and editing it in place would damage the templates. The
guard catches this before parsing and makes the user choose.

Implementation: `crates/dq-core/src/template_guard.rs`.

## Where it runs

Every command that parses a file runs the guard — `get`, `exists`, `keys`,
`values`, `len`, `type`, `paths`, `select`, `query`, `convert` (to stdout and
`-i`), `validate`, `diff`, `lint`, `check`, `fmt`, `fix`, `set`, `del`,
`patch`, `merge`, `set --jq`, and the file `set --value-from` / `@file`
reads — on the format `--input-format` or the file name picks, stdin
included. It refuses a file only when its templates change what the parser
reads. When the detection below matches, the file is parsed twice: as it
is, and with every template block swapped for a placeholder (letters only,
so that no format reads it as markup). If both parse and hold the same data,
each placeholder standing where the string read as it is holds one template
block, the templates do not matter to the reading and the command goes on
with the file as it is. The block's text is not compared: a quoted string
reads its escapes out of the block (`'${{ a == ''b'' }}'` in YAML,
`"${{ f(\"y\") }}"` in YAML, JSON or TOML), so the block as read is not the
block as written. The `position` of every Markdown node is left out of the
comparison: a placeholder longer or shorter than its block moves it. A GitHub
Actions workflow (`run: echo ${{ github.ref }}`, a plain scalar), a
`{{ … }}` inside a quoted string, with escapes or without, and a template in
the text, a code span or a fenced block of a Markdown file pass this way, in
every format. A template that changes what Markdown frontmatter reads is
refused as in a YAML file. A Helm
`port: {{ .Values.port }}`, which YAML reads as a flow mapping, or a
`{{- if … }}` line, on which the parser fails, is refused (`TemplatedFile`,
exit 3).

The refusal names the line of a template that changes the reading: of the
templates whose placeholder stands where the two readings first differ (a
value, a key, or a collection of another length), the first. A GitHub
Actions `${{ … }}` above a Helm `port: {{ .p }}` is not the one cited. When
the file does not parse as it is (or reads as another number of documents),
nothing tells which template is to blame, and the first template of the
file is cited.

### Edits keep the templates

An edit of a file the guard let through (`set`, `del`, `patch`, `merge`,
`set --jq`, `fix`, `fmt`) must keep its templates; otherwise it is refused with
`TemplatedFile` (exit 3) and the file is left alone, rather than written
with a template changed:

- The written text must still read the same with its templates as
  placeholders: no edit may move a template to where it changes the reading.
- Every string (value or key) holding a template must read back from the
  written text as the same string — through the textual edit, or through
  the writer when the file is rewritten whole (`set --jq`, a `fix.jq`, `fmt`, and
  the formats edited by rewriting: XML, HCL, INI, dotenv, CSV/TSV, JSON Lines). Strings
  the edit targets are exempt: `set`, `del` and `patch` may replace or
  remove the node a template is in, `merge` the members its patch names, and
  a value set may bring a template of its own.
- A textual edit (YAML, JSON, TOML) must leave every template block it did
  not target byte for byte as it was. A block is told apart by where it
  stands in the file, not by its text, so the same block on two lines is two
  blocks. The blocks of a targeted node may go, and so may those in the
  comments on its lines (`del /k` on `k: 1 # ${{ c }}`), and those in the
  comments of its key line when the edit removes that line.
- `patch` applies its ops in sequence, as RFC 6902 says: an op's path names
  the document the ops before it left. The guard carries each path back
  through the index shifts of those ops, so after `add /steps/0`,
  `replace /steps/3/name` targets the original `/steps/2`; an `add` (or the
  `path` of a `copy` or `move`) into an array inserts and targets no original
  node.

The message names the line of the template not kept — the one the edit
would lose, not the first block with the same text — and says the edit
would not keep it.

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

A template that changes the reading makes the command fail with
`TemplatedFile` (exit 3). The message names both escape hatches and what
each costs.

## `--raw-template-strings`

Every `{{ … }}` / `${{ … }}` / `{{- … -}}` span is replaced by a placeholder
`DQtplxq<N>qx` (N counts from 0, unique in the file, the surrounding YAML /
JSON quoting kept) before parsing, and restored on output. A file that holds
the text `DQtplxq` already gets placeholders with one more `x` (`DQtplxxq0qx`,
and so on until the file holds none of them), so text in the file is never
read, nor written back, as a template; the guard's second reading uses the
same placeholders. Text that spells a placeholder only once its escapes are
read (YAML `"DQtpl\x78q0qx"`, JSON `"DQtpl\u0078q0qx"`) is caught too: the
file is read a second time with placeholders of the same length spelled with
`y` (`DQtplyq0qx`), and where the two readings differ apart from the
placeholders, both get one more letter, up to eight. A file that spells the
placeholder text at every one of those lengths is refused (exit 3,
`file contains Go template syntax`, with a hint naming the placeholder text),
never read wrongly. A file with no `\`, no `&` and no `<` spells nothing with
escapes or with markup between two runs of text (XML reads
`DQtpl<!-- c -->xq0qx` as one text node), and is read once. The placeholder is
letters and digits only, so every format reads it as plain text: Markdown
took the former `__DQ_TPL_<N>__` for strong emphasis, and a template alone in
a paragraph came back as a `strong` node holding `DQ_TPL_0`. HCL reads a bare
template (`a = {{ .x }}`) as an expression; it reads back as the template,
`{{ .x }}`. The file round
trips: `dq set values.yaml /image/tag v1.2.3 -i --raw-template-strings`
changes the tag and leaves every template line byte-identical. Placeholders
never reach the disk.

On a reading command each template is read as the string it is written as:
`dq get values.yaml /image --raw-template-strings` on `image: {{ .Values.x }}`
prints `{{ .Values.x }}`, and `convert` writes it as that string. Lines in
`lint` positions match the file (a template never spans a line break);
columns after a template on the same line count the placeholder's length.
A parse error names its place in the file, not in the substituted text.

## `--allow-templates`

Skips the guard and hands the raw bytes to the parser; on a write command
round trip is not guaranteed, and a reading command sees what the parser
reads (`image: {{ .Values.x }}` is a flow mapping in YAML). It works when the templates happen to be valid
YAML (GitHub Actions `${{ … }}` inside strings); with bare Helm templates the
parser fails with its own parse error (exit 3).

The two flags are mutually exclusive (clap usage error, exit 2).
