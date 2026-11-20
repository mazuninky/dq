# `dq fmt`, `--sort-keys`, `--indent`

`dq fmt <file-or-glob>` parses a file and writes it back through its own
format's writer: YAML stays YAML, TOML stays TOML (cross-format conversion
is `convert -i`). It is the explicit canonicaliser, so it **drops comments,
anchor names and quote style** on purpose; preserving them is the contract of
the textual-edit commands (`set`, `del`, `patch`, `merge`), not of `fmt`.

- Output modes as for every write command: stdout by default, `-i`,
  `--diff`, `--check`, `--backup` (see
  [write-commands.md](write-commands.md#output-modes)).
- Globs, `--parallel`, `--continue-on-error` and the summary come from the
  bulk driver ([bulk.md](bulk.md)).
- `--check` exits 0 when the file is already canonical (re-emitted bytes
  equal the source), 1 otherwise, listing the files that would change. This
  is what the `dq-fmt-check` pre-commit hook runs.
- `-i --backup` writes `<file>.bak` only when the file actually changes.
- `--doc` is refused (exit 2): `fmt` re-emits the whole file.
- Without `--sort-keys` or `--indent`, only structural normalisation happens.

## `--sort-keys`

Sorts map keys at every depth, by byte order, through
`dq_core::canonicalize_keys` (deterministic and idempotent; arrays keep their
order, maps inside them are sorted). It applies to every re-emit path —
`fmt`, `convert`, `set --jq` — for JSON, JSONL, YAML and TOML. On point
edits (`set`/`del` with a pointer, `patch`, `merge`, `fix.ops`) it is accepted
and does nothing, in every format: on YAML, JSON and TOML, which take textual
edits, sorting would rewrite the file and break the comment-preserving
contract, and XML, HCL, INI, dotenv, CSV/TSV and JSON Lines, which a point
edit rewrites through the writer, follow the same rule, so a point edit
behaves alike in every format. Run `dq fmt --sort-keys` after such edits if
you want sorted keys. Read commands ignore it.

## `--indent <N>`

Indent width for JSON (and JSONL per line) on re-emit paths; `--indent 0`
gives compact JSON. YAML ignores it (the writer always uses two spaces; a
YAML indent option needs a different emitter) and TOML ignores it (its layout
is fixed by the grammar). Point edits ignore it in every format: textual
edits keep the file's own indentation, and a point edit rewritten through
the writer (XML, HCL, INI, dotenv, CSV/TSV, JSON Lines) uses the writer's.

## Not offered

`--quote-style`, `--flow-style`, `--strip-comments`, schema-aware
canonicalisation, a watch mode and per-format knobs do not exist. The first
three need an emitter that keeps comments; neither the YAML read path nor
`toml_edit` nor the JSON writer exposes such control, and saphyr's scanner
drops comment tokens before the event stream. See
[../roadmap.md](../roadmap.md#decided-not-to-do).
