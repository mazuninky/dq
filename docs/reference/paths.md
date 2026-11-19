# Paths: JSON Pointer, JSONPath, jq

dq has no path language of its own. Three standard ones cover everything:

| Language | Where | Why |
|---|---|---|
| JSON Pointer (RFC 6901) | `get`, `exists`, `keys`, `values`, `len`, `type`, `set`, `del`, `patch` ops, rule `loc.pointer`, diagnostics | Minimal and unambiguous; an agent can hardly get the escaping wrong. |
| JSONPath (RFC 9535) | `select` only | Read-only filtering across a document. |
| jq (jaq dialect) | `query`, `set --jq`, rule `check.jq` / `match.filter` / `fix`, composite `extract` | Conditions, transforms, arithmetic. See [jq.md](jq.md). |

## JSON Pointer

`dq_core::Pointer` (`crates/dq-core/src/pointer.rs`) parses RFC 6901:

- `''` (empty) is the whole document; `/` is the key `""` at the root.
- `/foo/bar` walks keys, `/0` indexes arrays.
- `~1` stands for `/` and `~0` for `~` inside a segment:
  `/metadata/labels/app.kubernetes.io~1name`.
- A final `-` segment is RFC 6902's "past the end of the array": `set` and
  `patch add` append there. Deleting `/-` is refused (it is not deletable),
  as a path error (exit 2).

A pointer that does not resolve is a path error (exit 2) carrying the
pointer, the deepest prefix that does exist, and — when sibling keys are
within Levenshtein distance 2 of the missing segment — up to three
`did_you_mean` candidates. Pointers in errors and suggestions are printed in
canonical form, re-escaped (`app.kubernetes.io~1name`), so they can be pasted
back into a command.

Passing a JSONPath expression to `get` (`dq get f.yaml '$.spec'`) is caught
and answered with the pointer to use instead (`dq get f.yaml /spec`), exit
1.

In a multi-document YAML stream, `/N/...` addresses document N when no
`--doc` is given; with `--doc N` the pointer is relative to that document.
See [read-commands.md](read-commands.md#multi-document-yaml-and---doc).

Navigation goes through the `Value` tree for every format, so the same
pointer syntax works on XML (`/project/0/version/0/#text`), Markdown
(`/children/0/level`), Dockerfiles (`/0/instruction`) and the rest; see
[formats.md](formats.md) for each format's shape.

## JSONPath

`dq select <file> <jsonpath>` evaluates RFC 9535 through `jsonpath-rust`
1.x, a strict evaluator: `$..*`, `?@` filters, functions and negative indices
work; non-standard forms such as `.length()`, `.['x']` and `=~` are rejected.
Blanks inside a member name are part of the name (`$.a b` looks up the key
`"a b"`). Matches are returned as a list in document order; no match is an
empty list and exit 0.

## jq

See [jq.md](jq.md) for the engine and [read-commands.md](read-commands.md)
for `dq query`. The original plan's idea of a separate selector language for
Markdown trees was dropped: JSON Pointer and jq address the Markdown tree
like any other document.
