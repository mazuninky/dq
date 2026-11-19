# Read commands

`get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`,
`query`, `convert` (to stdout) and `validate` never write to disk. They
refuse write-mode flags (`-i`, `--diff`, `--backup`, `--check`,
`--continue-on-error`, `--parallel`) with exit 6; `validate` accepts
`--check` as a no-op. Output goes through the reporter chosen by `-F` — and
remember that on most of these commands `-F` also picks the input parser
(see [cli.md](cli.md#the-two-roles-of--f)).

Every format in [formats.md](formats.md) works with every read command; the
format plugs in through the registry, the commands do not know about it.

## `dq get <file> <pointer>`

Prints the value at the pointer. `''` prints the whole document. A missing
pointer is a path error with matched prefix and `did_you_mean` (exit 2).

## `dq exists <file> <pointer>`

Answers purely through the exit code: 0 if the pointer resolves, 1 if not.
Nothing on stdout, nothing on stderr.

## `dq keys`, `dq values <file> <pointer>`

On a mapping: its keys or values, in source order (a list). On an array: its
indices (`keys`) or items (`values`).

## `dq len <file> <pointer>`

Number of items of an array, keys of a mapping, or characters of a string
(Unicode scalar values, not grapheme clusters).

## `dq type <file> <pointer>`

One of `null`, `bool`, `int`, `float`, `string`, `array`, `object`. Big
numbers report `int` / `float` like ordinary ones.

### Wrong node type

`keys` / `values` on a scalar, and `len` on a number, a boolean or a null,
exit 1 with a message naming the type: `cannot list the keys of '/n': it is
int, not an object or an array`. A pointer that does not resolve is still a
path error, exit 2.

### NaN, infinity and big numbers in output

`get`, `values`, `select` and `query` write a NaN or an infinity one way per
output format, whatever format it came from: `.nan` / `.inf` / `-.inf` on the
console, in YAML and in TOON; `nan` / `inf` in TOML; JSON and JSONL cannot
hold them, so those reporters fail with exit 1. A number too big for 64 bits
keeps its exact literal.

## `dq paths <file>`

Every addressable pointer with its type, the root first, in source order:

```
: object
/server: object
/server/port: int
/l: array
/l/0: int
```

Under a structured reporter the same data is a map of pointer → type name.

## `dq select <file> <jsonpath>`

RFC 9535 JSONPath (see [paths.md](paths.md#jsonpath)). Matches in document
order; an empty match is an empty list, exit 0.

## `dq query <expression> <file>`

Runs a jq expression (jaq dialect, see [jq.md](jq.md)) over the document.
The argument order mirrors `jq EXPR FILE`. `-` reads stdin and then needs
`-F` for the input format; for a named file the input format always comes
from the file name and `-F` only picks the output.

- The result is a stream. Console output prints one value per line; the
  structured reporters (`json`, `yaml`, `toml`, `jsonl`, `toon`) print the
  stream as one array (`.a` under `-F json` prints `[1]`).
- An update expression (`.spec.replicas |= . + 1`) prints the transformed
  document; `query` never writes the file.
- A compile error is exit 3 with the position in the expression; a runtime
  error (`. + 1` on a string) is exit 1.
- `sarif`, `junit`, `tap` are refused (exit 6).
- `--arg`, `--argjson`, `--slurpfile` and friends are not supported.

## `dq convert <file> -F <format>`

Parses the file (format from its name; `-F` is the output) and re-emits it in
the target format through that format's writer, honouring `--sort-keys` and
`--indent`. A conversion that loses formatting metadata (comments, anchors,
quote style) logs a warning and still exits 0. A value the target cannot hold
(NaN in JSON, a big number in HCL/TOON, a non-tabular value in CSV, a
non-table root in TOML) fails with exit 1 or 6. `-F markdown` only accepts
Markdown input. `convert -i` is a write command, see
[write-commands.md](write-commands.md#convert--i).

## `dq validate <file>`

Parses the file. Exit 0 and no output when it parses; exit 4 when it does
not, with the parse error rendered as a diagnostic — on **stderr**, in
whatever reporter `-F` selects (console, `json` → `{"diagnostics": [...]}`,
`sarif`, …). Note that `-F json` also makes the file be parsed as JSON; use
`-F sarif` / `junit` / `tap` for machine-readable output of non-JSON files.

## Multi-document YAML and `--doc`

A YAML file with several `---`-separated documents is a stream. A file that
is not a stream is document 0.

- Without `--doc`, read commands see document 0.
- `--doc N` selects document N (counting from 0). An index past the last
  document is exit 2: `--doc 1 is out of range: f.yaml holds 1 document(s)
  (--doc 0)` — this applies to single-document files too (it used to be
  ignored).
- `--doc all` presents the stream as an array of documents, so `/1/...`
  addresses the second one. On a single-document file it wraps that document
  in an array of one, so `/0/...` works the same everywhere
  (`--doc all query 'length'` prints `1`).
- `convert` writes only the chosen document (`--doc all`: every document).
- Over several files (glob, list), a file without document N is skipped with
  a note on stderr and counted as Skipped; if no file holds it the run fails
  with exit 2 and nothing is written.
- Commands that work on the whole file refuse any `--doc` with a usage error
  (exit 2): `fmt`, `validate`, `diff`, `patch`, `merge`, `convert -i`; `set`
  and `del` refuse `--doc all`.

How lint, check and fix treat streams is in
[lint-engine.md](lint-engine.md#yaml-streams); how set/del/patch edit them is
in [write-commands.md](write-commands.md#yaml-streams).
