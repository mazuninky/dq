# Formats

Every format plugs into one `Format` trait in `dq-core`
(`crates/dq-core/src/format.rs`): `parse(bytes) -> Document`,
`write(doc, w)`, `write_with_options(doc, w, &WriteOptions)`,
`extensions()` and `name()`. Commands only see the parsed `Document` and its
`Value` tree, so a new format becomes available to every command by being
registered in `crates/dq-core/src/parsers/mod.rs` (`registry()`); nothing
else changes. The parser implementations live in
`crates/dq-core/src/parsers/`.

## The value model

A parsed document is a tree of `dq_core::Value`:

`Null`, `Bool`, `Int(i64)`, `BigInt(String)`, `Float(f64)`,
`BigFloat(String)`, `String`, `Array`, `Map` (an `IndexMap`, so key order is
the source order).

Numbers keep their exact value:

- An integer that fits `i64` is `Int`; one that does not (YAML or JSON, any
  size) is `BigInt` holding the literal as written, so
  `4722366482869645213696` reads, prints and writes back character for
  character.
- A float is `Float` when the nearest `f64` has the same decimal value
  (ordinary floats, including 17-digit `%.17g` data such as
  `0.10000000000000001`). A float literal no `f64` holds — more significant
  digits than it has (`0.1000000000000000000000000001`) or past its range
  (`1e400`) — is `BigFloat` with its literal.
- Two spellings of the same big number (`1e400`, `1e+400`) compare equal in
  `patch` `test` ops and in `diff`, but stay two distinct map keys.
- NaN and the infinities (YAML `.nan`, `.inf`, `-.inf`; TOML `nan`, `inf`)
  are `Float`s, and numbers everywhere dq evaluates them: in jq (`query`,
  `set --jq`, rule checks, `loc`, composite `extract`, plugin `jq.eval`) and
  in JSON Schema checks. Read output spells them one way per output format,
  whatever the source: `.nan` / `.inf` / `-.inf` on the console, in YAML and
  TOON, `nan` / `inf` in TOML. JSON cannot hold them, so writing one as JSON
  or JSONL fails with exit 1 (`convert`, `get` / `query -F json`) or exit 7
  (`set` on a JSON file). Plugins reading the document as JSON get the
  strings `".nan"`, `".inf"`, `"-.inf"` ([../plugin-abi.md](../plugin-abi.md)).
- A format that cannot hold a number refuses it rather than mangling it:
  `convert -F hcl` / `-F toon` of an arbitrary-precision number exits 1
  naming it; `dq set` on TOML of such a number exits 7 ("TOML cannot
  represent arbitrary-precision number …").

## Detection

The input format is chosen by, in order:

1. `-F <format>` on commands where `-F` doubles as the input override (see
   [cli.md](cli.md#the-two-roles-of--f));
2. the file extension, case-insensitively;
3. the file name, case-insensitively, for files without a telling extension:
   `Dockerfile`, `Containerfile` → dockerfile; `.gitignore`,
   `.dockerignore`, `.npmignore`, `.eslintignore` → ignore-list; `.env` →
   dotenv.

An unknown extension is an error ("unsupported format 'sh'", exit 6), never a
guess. stdin (`-`) has no name, so it needs `-F`.

| Format (`name`) | Extensions / names | Read | Point edits (`set`, `del`, `patch`, `merge`) | Re-emit (`convert`, `fmt`, `set --jq`, `fix.jq`) |
|---|---|---|---|---|
| YAML (`yaml`) | `.yaml`, `.yml` | yes, incl. multi-document streams | yes, textual, comments kept | yes, comments dropped |
| JSON (`json`) | `.json` | yes | yes, textual | yes |
| JSON Lines (`jsonl`) | `.jsonl`, `.ndjson` | yes, as an array of records | no (exit 7) | yes, one record per line |
| TOML (`toml`) | `.toml` | yes | yes, textual, comments kept | yes, comments dropped |
| HCL (`hcl`) | `.hcl`, `.tf`, `.tfvars` | yes | no (exit 7) | yes, best effort |
| INI / properties (`ini`) | `.ini`, `.properties`, `.cfg` | yes | no (exit 7) | yes, comments dropped |
| dotenv (`dotenv`, `-F dot-env`) | `.env` (name), `*.env` | yes | no (exit 7) | yes, comments dropped |
| CSV / TSV (`csv`, `tsv`) | `.csv`, `.tsv` | yes, array of records | no (exit 7) | yes, tabular values only |
| Markdown (`markdown`) | `.md`, `.markdown` | yes, CommonMark + GFM tree | no (exit 7) | unmodified documents only |
| Markdown frontmatter (`frontmatter`) | same extensions, only via `-F frontmatter` | yes, header only | no (exit 7) | yes, body kept byte for byte |
| XML (`xml`) | `.xml` | yes | no (exit 7) | yes, partial round trip |
| Dockerfile (`dockerfile`) | `Dockerfile`, `Containerfile` (names), `*.dockerfile`, `*.containerfile` | yes | no | no (read-only) |
| ignore-list (`ignore-list`) | `.gitignore`, `.dockerignore`, `.npmignore`, `.eslintignore` | yes | no | no (read-only) |
| TOON (`toon`) | — | no | — | output only |

Notes on the table:

- `.pom` and `.xsd` are not detected as XML (README lists them; the parser
  claims only `.xml`); pass `-F xml`. `.env.local` and other `.env.*` names
  are not detected either.
- "Point edits" are the textual edits described in
  [write-commands.md](write-commands.md); on any other format they fail with
  `write operation unavailable: <format> has no textual edits` (exit 7). Use
  `set --jq` to change such a file through its writer.
- `-F dockerfile` and `-F ignore-list` do not exist as output formats; clap
  rejects them (exit 2).
- Markdown with frontmatter is parsed by the Markdown tree parser by default
  (it precedes the frontmatter parser in the registry); `-F frontmatter`
  selects the header-only parser.

## Per-format behaviour

### YAML

YAML 1.2, single documents and `---`-separated streams. Reading goes through
`serde_norway` for the value tree plus `saphyr-parser` (the low-level event
API) for a `Pointer → byte range` span map that powers textual edits and
diagnostic positions. Mapping keys that collapse to the same key (`1` and
`"1"`, `true` and `"true"`, `~` and `""`, a big integer and the same digits
quoted, equal floats, or two collection keys YAML calls equal) are a parse
error that names the first key and points at the repeat; dq never silently
keeps one. A key that is itself a sequence or mapping is named by its compact
flow text (`[1, 2]`, `{k: v}`), tags dropped.

Streams: every document is addressable; see
[read-commands.md](read-commands.md#multi-document-yaml-and---doc) for
`--doc` and [write-commands.md](write-commands.md) for whole-document edits.

The YAML writer (used by `convert -F yaml`, `set --jq` / `fix.jq` on YAML,
`fmt`, and YAML output of `get` / `query`):

- quotes strings that a YAML 1.1 reader (PyYAML; go-yaml as used by
  Kubernetes and Helm) would type differently — `'on'`, `'y'`,
  `'1:20'`, `'2001-12-14'`, `'1_000'`, `'0b1'` — as values and as keys
  (a GitHub Actions `on:` key rewritten by `set --jq` becomes `'on':`);
  a `<<` merge key stays plain;
- writes integers of any size plain and floats in exponent form with a dot
  and a signed exponent (`1.0e+16`, `1.0e+400`), which YAML 1.1 readers
  also read as numbers;
- writes collections set into a flow context in YAML flow style (`{b: 2}`),
  quoting strings only when needed;
- ignores `--indent` (two spaces always).

Known limit shared by every YAML reader (saphyr, libyaml, PyYAML): `{n:
!!str}` with no space before `}` is rejected.

### JSON

RFC 8259. Object key order is kept. Comments are refused: a file with `//`
or `/* */` fails with "comments are not valid JSON; JSONC is not supported"
(exit 3). A leading UTF-8 byte order mark is accepted; edits and `-i` keep
it, `convert` output has none. Textual edits detect and reuse the file's
indent (2 spaces, 4 spaces, tab) and its trailing-newline state. Numbers are
handled with `serde_json`'s `arbitrary_precision`, so big literals pass
through untouched. `--indent N` applies on re-emit (`--indent 0` gives
compact output).

Known issues (see [../roadmap.md](../roadmap.md)): the re-emit writer leaves
no trailing newline on some paths (`set --jq`, `fmt --indent 2 -i`), and
`convert -F json` of a JSON file can print an extra blank line.

### JSON Lines

One JSON value per line, read as an array (`/0`, `/1`, …). Writing emits one
compact line per top-level array element.

### TOML

TOML 1.0 through `toml_edit`, which keeps comments, blank lines, inline vs.
table style, dotted keys and datetime literals (`1979-05-27T07:32:00Z` stays
as written). A TOML document is a table, so a result whose root is not a
table is refused with "a TOML document is a table, so its root is one, not an
integer" (exit 1; exit 7 on `set ''`). `set` creating a nested key writes a
dotted key (`b.c = 2`) or a `[b]` header if the file already uses headers.
CR-only line ends are invalid per the TOML spec and are not supported.

### HCL

HCL 2 through `hcl-rs`. Blocks map to nested maps with labels as keys:
`terraform { backend "s3" { region = "x" } }` becomes
`{terraform: {backend: {s3: {region: x}}}}`; each extra label nests one
level. Object keys `a` and `"a"` are a duplicate (exit 3). Writing goes
through `hcl::to_string`: comments, spacing and layout are not kept.

Limits: HCL documents carry no spans, so every Terraform diagnostic reports
line 1; repeated blocks with the same type and labels (`x { a = 1 }` twice)
keep only the last one, silently. Both are open items in
[../roadmap.md](../roadmap.md).

### INI / properties

`rust-ini`. Shape: `{section: {key: string}}`, sections in source order; keys
before the first header go under the section `""`. `=` and `:` both separate
keys from values. All values are strings. Comments and quoting style are not
kept on write.

### dotenv

A flat `{KEY: string}` map. Handles `KEY=value`, `export KEY=value`,
double-quoted values with `\\ \" \n \t` escapes, single-quoted literal values,
comments and blank lines. No `${VAR}` interpolation: values are stored as
written. The writer double-quotes values containing whitespace, `#`, `=`,
`$`, `\`, `"` or non-printable characters.

### CSV / TSV

The `csv` crate, header row required. The document is an array of maps keyed
by the header; every cell is a string (no type inference). TSV is the same
with a tab delimiter. Writing needs an array of maps; the header is the union
of keys in first-seen order. Any other shape is an error naming the offending
type.

### Markdown

CommonMark + GFM (tables, strikethrough, autolinks, task lists, footnotes)
through `comrak`, as a typed tree:

```json
{ "type": "document",
  "frontmatter": { "kind": "yaml", "value": { "title": "Hi" } },
  "children": [ { "type": "heading", "level": 1,
                  "children": [ { "type": "text", "value": "T" } ],
                  "position": { "start": {"line": 1, "column": 1},
                                "end":   {"line": 1, "column": 3} } } ] }
```

Every node is a map with a `type` discriminator and node-specific fields
(`heading.level`, `code_block.lang`, `link.url`, …); children live under
`children`. Block nodes carry `position`; inline nodes do not. Frontmatter
(`---` YAML, `+++` TOML, or `{…}` JSON followed by a blank line) is parsed
before comrak and folded into `frontmatter: {kind, value}`; without one,
`frontmatter` is absent/null. Positions are counted in the body after the
frontmatter block, not in the file (observed: a heading on file line 4 after
a three-line frontmatter reports line 1), so Markdown lint findings in a file
with frontmatter are off by the frontmatter's length — a known bug, see
[../roadmap.md](../roadmap.md).

There is no selector language for the tree: JSON Pointer and jq address it
like any other document (`dq get post.md /frontmatter/value/title`,
`dq query '.children[] | select(.type == "heading") | .level' post.md`).

Writing: an unmodified document is written back byte for byte; a modified
one is refused, and `convert -F markdown` only accepts Markdown input.
Fenced code blocks carry an inline offset so composite rules can place
findings inside them (see [rule-checks.md](rule-checks.md)).

### Markdown frontmatter (`-F frontmatter`)

The older header-only parser: the value is the frontmatter header (YAML,
TOML or JSON, detected by delimiter) and the body is kept as opaque bytes.
No frontmatter within the first 64 KiB gives an empty map and the whole file
as body. Re-emitting serialises the header through its own format and
appends the body verbatim.

### XML

`quick-xml`, mapped onto ordinary values with conventional keys:

| XML | Value |
|---|---|
| element `<tag>` | `tag: [ {…} ]` on the parent — always an array, even for one occurrence, so pointers stay stable |
| attributes | `@attrs: {name: value}` |
| text | `#text` |
| comments | `#comments: [..]` on the parent |
| CDATA | `#cdata: [..]` |
| processing instructions | `#pi: [..]` on the parent |
| XML declaration | top-level `#xml: {version, encoding, standalone}` |
| namespace prefixes, `xmlns:*` | kept verbatim in names and `@attrs` |

So `<project><version>1</version></project>` is read as
`/project/0/version/0/#text`. Round trip is partial: structure, attributes,
comments, CDATA, PIs, prefixes and the declaration survive; whitespace
between elements is normalised; mixed content (`<p>Hello <b>w</b>!</p>`) is
folded into `#text` with a warning. Only UTF-8-compatible input is accepted
(no UTF-16 or legacy code pages).

### Dockerfile

dq parses Dockerfiles itself, following the Dockerfile reference and the
BuildKit frontend (it no longer uses `dockerfile-parser-rs`). The value is an
array of `{instruction, arguments, line}` records in source order:

- `instruction` is the upper-cased keyword; `line` is the 1-based line where
  the instruction starts.
- `arguments` is the text as written (flags included, continuation lines
  joined) for shell form; an array of strings for JSON/exec form
  (`["sh", "-c", "…"]`, detected the BuildKit way); a map for `ENV`, `LABEL`
  (quotes and escapes removed, no expansion) and `ARG` (no default → null).
- Parser directives (`# syntax=`, `# escape=`, `# check=`) are honoured,
  including the backtick escape. Heredocs on `RUN`, `COPY`, `ADD` are
  appended to `arguments` verbatim.
- Unknown keywords, missing arguments, unterminated heredocs and malformed
  directives are parse errors carrying the instruction's line.

Read-only: writing returns a format error. Dockerfile diagnostics currently
report line 1 because the format has no spans yet, even though every record
knows its `line` (see [../roadmap.md](../roadmap.md)).

### ignore-list

`.gitignore`-style files as a flat array of patterns; blank lines and `#`
comments are dropped, trailing whitespace trimmed. Read-only.

### TOON

Output only, through the `toon-format` crate (dq ships no TOON encoder of its
own). Meant for compact LLM context.

## Write options

Re-emit paths take a `dq_core::WriteOptions` (`#[non_exhaustive]`, built
from the global flags):

| Field | Flag | Honoured by |
|---|---|---|
| `sort_keys` | `--sort-keys` | JSON, JSONL, YAML, TOML re-emit (deep, byte-order sort via `dq_core::canonicalize_keys`, idempotent) |
| `indent` | `--indent N` | JSON, JSONL; YAML and TOML ignore it |

`WriteOptions::default()` produces exactly what `write` produces. Textual
edits never re-emit, so both flags are no-ops there (see [fmt.md](fmt.md)).
`--quote-style`, `--flow-style` and `--strip-comments` do not exist; they need
a comment-preserving emitter (see [../roadmap.md](../roadmap.md)).

## Deliberately unsupported

Binary formats (MessagePack, CBOR, Bencode), Lua tables, Protobuf text, Avro,
GraphQL and SQL are out of scope. The conftest-only formats (CUE, EDN,
Jsonnet, HOCON, nginx, SPDX, TextProto, VCL) are added only for a concrete use
case. Schema languages other than JSON Schema 2020-12 (XSD, RelaxNG,
Schematron) are out of scope.
