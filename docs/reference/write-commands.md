# Write commands

`set`, `del`, `patch`, `merge`, `convert -i`, `fmt` and `fix` change files.
They share the output modes, the atomic write and the bulk driver described
here and in [bulk.md](bulk.md). `fmt` has its own page ([fmt.md](fmt.md)),
`fix` is part of the lint engine ([lint-engine.md](lint-engine.md)).

## Two ways to write a file

dq writes a file in one of two ways, and which one a command uses decides
whether comments survive:

| | Textual edit | Re-emit |
|---|---|---|
| Used by | `set` / `del` with a pointer, `patch`, `merge`, rule `fix.ops`, plugin fixes — on YAML, JSON, TOML | `set --jq`, `fmt`, `convert`, rule `fix.jq`; the point edits on HCL, XML, INI, dotenv, CSV/TSV, JSON Lines |
| Formats | YAML, JSON, TOML | every writable format |
| How | splices only the bytes of the changed node, using the span map built at parse time | parses to a value, transforms it, writes the whole document with the format's writer |
| Keeps | comments, blank lines, key order, quote style, anchors/aliases, flow vs. block style, indentation, line endings (LF, CRLF, lone CR), BOM | key order only |
| `--sort-keys` / `--indent` | no-op | applied by `set --jq`, `fmt`, `convert`; no-op on the point edits |

The trade-off is deliberate: textual edits are the round-trip guarantee,
re-emit is for transforms that may change the structure arbitrarily.

HCL, XML, INI, dotenv, CSV/TSV and JSON Lines have no textual edits, so a point edit
(`set` / `del` with a pointer, `patch`, `merge`, `fix.ops`) on them is
made to the value tree and the whole file is re-emitted, exactly as
`set --jq` would: `dq set app.ini /server/port 8080` writes
`[server]\nhost=a\nport=8080\n` for `; c\n[server]\nhost = a\nport = 80\n`.
The ops of a `patch`, a `merge` or a `fix.ops` script are all made to the
tree and the file is written once, after the last, so each op sees the tree
the op before it left, as RFC 6902 says: removing `/ingress/1` and then
`/ingress/0` of two repeated HCL blocks removes both, though one block left
alone would read back as its body rather than a list of one; a failing op
leaves the file untouched. The comments go (XML keeps its own, which are in its tree), an HCL file keeps
its blocks as blocks and every expression the edit does not change as its
source (`ami = var.ami` stays; see [formats.md](formats.md#hcl)), and the
value is read back as the file holds it (an
INI, dotenv or CSV value is a string). A tree the writer cannot hold — a
nested map or list in INI, dotenv or CSV, a scalar where XML wants an
element, a number HCL cannot write — is refused with the writer's error
(`ini format error: …`, exit 1) and the file is untouched; in a bulk run it
counts as `Failed`. An edit that leaves the data as it was — a value set
to what is there, or to one that reads back as it (`80` in INI, whose
`80` is a string) — leaves the file's bytes alone, comments included:
`--check` exits 0, and a bulk run counts the file as `Skipped`. Textual
edits behave the same. `--sort-keys` and `--indent` are accepted and do
nothing on these point edits, as on the textual ones, so a point edit
behaves alike in every format (`dq fmt` applies them; see
[fmt.md](fmt.md)). A JSON Lines record
is addressed by its index, as `get` addresses it: `dq set log.jsonl
/0/level warn` rewrites the file one compact record per line. Point edits
on Markdown fail with exit 7 ("has no textual edits").

A textual edit is checked before it is written: dq re-reads the edited bytes
and requires that they read back as exactly the requested change. If the
splice would change anything else (an edit dq cannot express in place), it is
refused as `edit_rejected`, exit 7, and the file is untouched.

A rewrite is checked the same way: the bytes the writer would write must read
back as the edited tree, as the format reads any tree back — a number, a
boolean or a null as its text in INI, dotenv and CSV (and XML), a cell a CSV
row lacks as an empty one, an HCL list of one block as the block's body, an XML element given as
a scalar as an element holding it, the XML declaration (`/#xml`) as the
version, encoding and standalone it writes (a missing version as `1.0`).
Anything else is refused as `edit_rejected`, exit 7, the file untouched: `dq
set a.ini /s/k ' v '` ("cannot set '/s/k': the ini dq would write reads back
as another value at '/s/k'": INI trims the value), a key `k=x` that would
read back as the key `k`, or an `#xml` below the root (`dq set g.xml
/r/0/#xml/v 1`), where an XML file has no declaration to hold it. A JSON Lines file is a list of records, so a point edit that would leave
it a map or a scalar (`dq set l.jsonl '' '{"x":1}'`, `dq merge l.jsonl
'{"x":1}'`) is refused up front, exit 1 ("a JSON Lines file is a list of
records, one per line, so its root is an array, not a map").

## Output modes

Exactly one of:

- **default** — the changed document goes to stdout; the file is untouched.
- `-i` / `--in-place` — the file is replaced atomically, its permissions kept;
  stdout stays empty.
- `--diff` — a unified diff (original vs. changed) on stdout; the file is
  untouched. The headers are `--- a/<path>` and `+++ b/<path>`, the path as
  given, as `git diff` writes them; a shown line that ends the file without a
  line break is followed by `\ No newline at end of file`, as in GNU diff
  and git.
- `--check` — nothing is written or printed except `would modify: <path>`
  lines and a count; exit 1 if any file would change, 0 otherwise. With
  `--continue-on-error`, a file that fails makes it exit 7
  ([bulk.md](bulk.md)).

A file holding Go-template or GitHub Actions markers is edited when its
templates do not change what the parser reads (a workflow's
`run: echo ${{ github.sha }}`), and the edit must keep them: a textual edit
leaves every template it does not target byte for byte, a rewrite writes
each so it reads back as the same string — otherwise exit 3, file
untouched. A Helm values file is refused (exit 3) unless
`--raw-template-strings` or `--allow-templates` says how to treat it; see
[template-guard.md](template-guard.md).

`--backup` (only with `-i`) copies the original to `<file>.bak` first,
overwriting an existing `.bak` without asking. Invalid combinations are listed
in [cli.md](cli.md#global-flags). A write command keeps the file in its own
format — the one it was read in, from its name or `--input-format` — in all
three modes: a `-F` naming another format is a usage error (exit 2), the same
format is accepted, and only `convert -i` takes `-F` as a target. Stdin (`-`,
with `--input-format`) works in plain stdout mode and with `--diff`; `-i` and
`--check` on it are usage errors (exit 2).

## Atomic writes

Every `-i` write goes through `dq_core::atomic_write` (`read` notes the file that
was read, `rewrite` replaces that one): the content is written to a `tempfile::NamedTempFile` created **in the target's directory**
and then renamed over the target. Same-directory placement keeps the rename
atomic (no cross-device `EXDEV`). If writing or renaming fails, the original
is byte-identical to before and the error exits 7. Read-side failures (the
source cannot be opened) exit 5. The file is not synced to disk before the
rename.

- **Permissions.** The file keeps its mode, and its owner and group where the
  process may set them. Where the group cannot be kept, the bits it and others
  had are narrowed, never widened; a lost owner drops setuid, a lost group
  setgid. Both log a WARN, neither fails the write. At no moment is the new
  content readable by more users than the old. A new file gets `0666` less the
  umask. A read-only file is edited (the directory must be writable) and stays
  read-only.
- **Symbolic links.** A file edited through a link, or a chain of them, is the
  one the link leads to, in its own directory; the link stays.
- **`--backup`** copies the real file to `<real file>.bak` beside it, with its
  mode. Anything but a regular file at that name (a link, a directory) is
  refused, exit 7, and nothing is written.
- **ACLs.** POSIX ACLs on Linux and ACLs on macOS are kept, on the file, its
  `.bak` and the file `convert -i` makes; one the directory would give the new
  file is not. Where an ACL cannot be read or kept, the group and others get
  nothing and a WARN says so: access never widens. Kept through the rename,
  the ACL's group entry refers to the old group, so a file whose group could not
  be kept keeps no ACL and is private to its owner.
- **Changed while dq was running.** The file written is the one that was read:
  if, between the two, the path leads to another file (it was replaced, or
  turned into a link), the write is refused, exit 7.
- **Not preserved:** other ACLs (NFSv4, other systems), extended attributes
  (the SELinux label too), file flags, timestamps, and the other names of a
  hard-linked file, which keep the old content.
- A target that is no regular file (a FIFO) is refused, exit 7.

## `dq set <file> <pointer> [value]`

Writes a value at a pointer.

- **Value source**: the inline argument; `-` for stdin; `@path` to read a
  file (its format from its name); `--value-from <path>`, the same. A value
  read from stdin (`-`, `@-`, `--value-from -`) has no name to take a format
  from, so it is parsed as an inline value is (below), `--value-string`
  included; `--input-format` names the format of the file, not of the value.
- **Inline parsing**: an inline value is parsed as a JSON literal when it
  starts with `{`, `[`, a digit or `-`, or is `true` / `false` / `null`, and
  is valid JSON; anything else is a string (`abc`, `08`, `"q"` with its
  quotes). `--value-string` forces a string (`8080` → `"8080"`).
- **Missing parents** are created as mappings (`set /b/c 2` on `a: 1` gives
  `b:\n  c: 2`; JSON gets `"b": {"c": 2}`; TOML a dotted key or a `[b]`
  header). Arrays are never created: a missing index along the way is exit 2.
  `--no-create` refuses any missing parent with exit 2.
- **Arrays**: `/-` appends; an existing index is replaced.
- **Big numbers** round-trip: `set data.json /id 4722366482869645213696`
  followed by `get` prints the same literal; a long float literal is written
  in full.
- **Root**: `set f.yaml '' '{"b": 2}'` replaces the whole document (YAML,
  JSON, TOML) through the writer and keeps the comment lines it starts with.
  A TOML root must stay a table (exit 7 otherwise).
- **Empty files**: an empty YAML file, or one holding only comments or `---`,
  takes the new key (`# c` → `# c\na: 1`).
- Values set into a YAML flow collection are written in flow style
  (`{b: 2}`, quoting only when needed).

## `dq del <file> <pointer>`

Removes the node at the pointer; later array items shift down. A missing
pointer is not silent: path error, exit 2. `del f ''` is refused ("cannot
delete the root ''", exit 2). Deleting the only entry of a flow collection
keeps an adjacent comment (`[1 # c\n]` → `[ # c\n]`).

## `dq set <file> --jq <expr>`

Transform mode: the jq expression gets the whole document and must produce
**exactly one** value, which replaces the document.

- The pointer must be absent (or `/`); a value or `--value-from` with `--jq`
  is a clap conflict (exit 2); the handler re-checks and refuses with 6.
- Zero outputs (`empty`) or several (`.[]`) are refused with exit 6, naming
  the count (wrap in `[...]` to collect).
- Compile error → exit 3; runtime error → exit 1.
- The result is re-emitted through the format's writer: **comments are
  dropped** (`set --help` says so). Use point edits when comments matter.
- Works with every write mode and with globs; the expression is compiled once
  and shared across `--parallel` workers.
- Refuses `--allow-templates` / `--raw-template-strings` (exit 6): the jq
  path parses without the template pre-pass and could not restore
  placeholders.
- On a YAML stream without `--doc`, the filter sees an array of documents and
  must return a non-empty array; the result is written as a stream.

A path assignment creates what it walks through, as jq does:
`.metadata.labels.team = "core"` on a manifest with no `labels` (or no
`metadata`, or on an empty document) writes the missing mappings, and
`del` of a missing path changes nothing. See
[jq.md](jq.md#path-assignment).

## `dq patch <file> <ops>`

Applies an RFC 6902 JSON Patch through textual edits (YAML, JSON, TOML), or
through the writer on HCL, XML, INI, dotenv, CSV/TSV and JSON Lines (see
[Two ways to write a file](#two-ways-to-write-a-file)).

- Ops from: inline JSON array, `-` (stdin), `@path`, or `--ops-from <path>`;
  `@-` and `--ops-from -` read stdin too.
- `--line-format` accepts one op per line, `<op> <pointer> [json-value]`
  (`replace /a 5`, `remove /l/2`).
- `add`, `remove`, `replace`, `move`, `copy`, `test`. The patch is atomic: if
  any op fails (including a `test` mismatch), nothing is written. A failed
  `test` exits 1 and names the pointer, expected and actual values.
- Strict RFC 6902 semantics: `add` into an array **inserts** (`add /0 9` on
  `[1,2]` gives `[9,1,2]`; `/-` or `/2` appends), and so do `copy` / `move`
  into an index; parents are **never** created (`add /b/c` on `a: 1` is exit
  2). `--no-create` is accepted for symmetry with `set` and changes nothing.
  A `move` into the value's own child is refused (exit 2, naming both paths).
- Inserts keep comments in YAML, JSON and TOML (including arrays of tables),
  work before the first item of a compact YAML sequence, and through an
  anchored YAML array that an alias shares (the alias sees the new item);
  inserting *under* the alias is refused as with `set`.
- A `-F` naming another format than the file's is refused (exit 2).

## `dq merge <file> <patch>`

Applies an RFC 7396 Merge Patch through textual edits (through the writer on
HCL, XML, INI, dotenv, CSV/TSV and JSON Lines). Patch from inline
JSON, `-`, `@path` or `--patch-from <path>`; `@-` and `--patch-from -` read
stdin too.

- `null` removes a key; objects merge recursively; arrays and scalars
  replace.
- A mapping patch on a root that is not a mapping replaces the root with the
  patch minus its null members (RFC 7396 §2); nested nulls under a key whose
  old value was not a mapping are dropped.
- A YAML stream is refused (exit 6): a merge patch applies to one document.

## `dq diff <a> <b>`

Read-only; compares two files structurally (each format detected from its
name, so YAML can be compared with JSON). By default the result is a minimal
RFC 6902 patch from `a` to `b` — equal subtrees produce nothing, a replaced
parent is not followed by child ops — rendered through the reporter. `--unified`
prints a textual unified diff of the two documents rendered as JSON, each
text ending with a line break (so never a `\ No newline at end of file`
marker: that would describe dq's rendering, not the files), under the two
paths as given on the command line, as `diff -u` labels them (`--- a.yaml`,
`+++ b.json`, no timestamps). Equal documents give an empty list. Feeding the output of `diff -F json` to
`patch` turns `a` into `b`. Big numbers print as numbers, and `1e400` equals
`1e+400`.

`--unified` colours its output (red removals, green additions, cyan hunk
headers) by the rule every command follows — `--no-color`, then `NO_COLOR`,
then `CLICOLOR_FORCE`, then whether stdout is a terminal (see
[cli.md](cli.md#colour)) — so piped output is plain text.

## `convert -i`

`dq convert deploy.yaml -F json -i` writes `deploy.json` atomically and, only
after that succeeded, removes `deploy.yaml`; `--keep-source` keeps it. A
conversion to the file's own format is refused (exit 6), and so is a target
that is the source under another spelling (`f.YAML` to `f.yaml` on a
case-insensitive file system). `--doc` is refused with `-i`, because deleting
the source would delete the other documents with it.

The new file takes the mode, owner and group of the source. An existing file at
the target is replaced without a question and keeps its own mode; `--backup`
keeps a copy of it. A target that is a link is refused (exit 7). When the source is a
link, the new file goes beside the link, the link is removed and the file it
leads to stays. Before anything is written the whole run is planned: two sources
that map to the same target (`a.yaml` and `a.yml` to `a.json`), a target that is
a source, a target that is two names of one file (a hard link), a `<target>.bak`
that is a source, are refused (exit 6), and a link at a target exits 7. Nothing
is converted then.

## YAML streams

Writes address the documents of a stream as `/N` (or relative to `--doc N`):

- `set FILE /N VALUE` (or `--doc N set FILE '' VALUE`) writes document N anew
  through the YAML writer: its `---` line, the comment lines after its content
  and every other document stay byte for byte; comments inside its content go.
  A replaced document keeps the comment lines between `---` and its content.
- `del FILE /N` removes document N with its `---` line and the comment lines
  just above it.
- `set FILE '' …` and `del FILE ''` on a whole stream are usage errors
  (exit 2); `--doc all` is refused by `set` and `del`.
- `patch` uses the same `/N` addressing: `remove /N`, `replace /N`, `/N/...`
  inside one document, `add /N` inserts a new document there (with its `---`;
  before a document carrying `%YAML` / `%TAG` directives the new document goes
  before them and ends with `...`), `add /-` appends one.
- `merge` refuses streams (exit 6).
- `set --jq` without `--doc` sees an array of documents (see above).

## Design notes

- Pointers, not a DSL: every write command takes RFC 6901 pointers or RFC
  6902 / 7396 documents, so agents never learn dq-specific syntax.
- Round-trip first: the textual-edit path was built before any other write
  feature, because everything after it relies on edits not damaging files.
  See [../decisions.md](../decisions.md) for how the approach was chosen
  (a span map over saphyr-parser events and `toml_edit`, instead of a custom
  emitter).
- Transactional bulk writes (rolling back files already written when a later
  file fails) are not offered; each file is atomic on its own.
- `query` never writes, whatever the expression.
