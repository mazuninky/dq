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
  or JSONL fails with exit 1 (`convert`, `get` / `query -F json`, and
  `set --jq` / `fix` on a JSON file). HCL cannot hold them either: `convert -F hcl`,
  `set` and `fix` on an HCL file fail with exit 1 naming the value
  (`HCL cannot represent non-finite number .nan`) and write nothing.
  Plugins reading the document as JSON get the strings `".nan"`, `".inf"`,
  `"-.inf"` ([../plugin-abi.md](../plugin-abi.md)).
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
- writes a string holding NEL, LS or PS (U+0085, U+2028, U+2029) —
  line breaks to a YAML 1.1 reader, content to a YAML 1.2 one —
  double-quoted with `\N` / `\L` / `\P`, never raw nor in a block
  scalar, so both read it alike; textual-edit `set` writes such a value
  the same way;
- writes a string holding a character YAML does not take raw — a C0
  control other than tab and line feed, DEL, a C1 control (U+0080–U+009F),
  U+FEFF, U+FFFE, U+FFFF — double-quoted with escapes (`\x80`, `\uFEFF`),
  as a value, a key or an item; textual-edit `set` writes such a value or
  key the same way (spelled `\x80`, `\ufeff`), whatever scalar it
  replaces;
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
level. Object keys `a` and `"a"` are a duplicate (exit 3). Numbers are read
as their literal spells them: a whole float (`3.0`, `1e16`) stays a float
and is written back as `3.0` / `1e16`, not `3`; an integer past 64 bits or
a decimal with more digits than an `f64` holds keeps its exact value, which
`-F hcl` cannot write back (exit 1, see the value model above). Writing
lays a body out as `hcl::to_string` does: comments, spacing and layout are
not kept. An HCL file is a body of attributes, so only a map can be written
as one: `-F hcl` of an array, a string, a number, a boolean or null exits 1
("an HCL document is a body of attributes, so its root is a map, not an
array").

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
| XML declaration, byte order mark, encoding told from the bytes | top-level `#xml: {version, encoding, standalone, bom, detected}` |
| namespace prefixes, `xmlns:*` | kept verbatim in names and `@attrs` |

So `<project><version>1</version></project>` is read as
`/project/0/version/0/#text`. Round trip is partial: structure, attributes,
comments, CDATA, PIs, prefixes and the declaration survive; whitespace
between elements is normalised; mixed content (`<p>Hello <b>w</b>!</p>`) is
folded into `#text` with a warning. Whitespace is XML's: space, tab, CR
and LF. Text of nothing else is indentation on read, and is written as
character references (`&#32;`) so it reads back as text; U+3000 and other
Unicode spaces are text. NEL and LS (U+0085, U+2028) are always written as
references, as XML 1.1 reads them raw as line ends. In a `version="1.1"` document the
writer also writes XML 1.1's restricted characters — the C0 controls but
tab, LF and CR, and the C1 controls U+007F–U+0084 and U+0086–U+009F — as
references in text and attribute values, and refuses one in a comment, a
PI or CDATA, which hold no references. XML 1.0 has no C0 controls but
tab, LF and CR, so the writer refuses them, and writes C1 controls raw.

A line end reads as a line feed wherever it stands — text, comments, PIs
and CDATA — as XML's end-of-line handling has it: CR LF and a lone CR,
and in a `version="1.1"` document NEL, LS and CR NEL too. An attribute
value reads each as a space. So the writer refuses a CR in a comment, a PI or
CDATA, which hold no references, and in a 1.1 document NEL and LS too;
text and attribute values write them as references.

Encodings. A file is decoded per its byte order mark, else per the
declaration's `encoding`, else as UTF-8. Labels are the WHATWG Encoding
Standard's (UTF-16, Shift_JIS, ISO-2022-JP, windows-1252, …), except
`US-ASCII`, which is strict 7-bit, and `ISO-8859-1`, which maps each byte
to its code point, as the XML spec and other XML parsers have it. The
writer encodes in the declared encoding (UTF-8 without one), starting
with the byte order mark `bom` names (a file with a BOM and no
declaration reads as `#xml: {bom: …}` alone). A `UTF-16` label always
gets a BOM, little-endian unless `bom` says otherwise; `UTF-16LE` and
`UTF-16BE` get one only when `bom` names it. Two cases keep what the bytes were in `detected`:

- UTF-16 with no BOM, told by its leading `<?`: `detected: UTF-16LE` or
  `UTF-16BE`, written back in that byte order with no BOM.
- UTF-8 bytes under a UTF-16 label (.NET's `XmlSerializer` writes
  `encoding="utf-16"` into UTF-8): read as UTF-8, `detected: UTF-8`, and
  written back as UTF-8 with no BOM under the same label, byte for byte.
  The writer takes `detected: UTF-8` only under a UTF-16 label (exit 1
  otherwise).

The reader refuses, with exit 3, naming in the message where the name at
fault begins (`XML parse error at line 2, column 4: element <a:b> uses the
prefix 'a', …`), or the declaration's `encoding` label (line 1, column 31
in `<?xml version="1.0" encoding="UTF-32"?>`, a BOM not counted), as
`dq validate` does in its `line` / `col` fields. Bytes the encoding
cannot read name their byte index instead:

- malformed markup, in dq's words at the markup, attribute or reference
  at fault: a tag, comment, CDATA section, PI, attribute value or
  reference left open; an attribute repeated, with no value, or unquoted
  (`attribute 'a' appears twice on <c>`); an entity XML does not
  predefine; a character reference that is no number (`&#X41;`) or past
  U+10FFFF; a closing tag that does not match; an element the input
  leaves open, the innermost named at its start tag (`element <c> is not
  closed`);
- content outside the root element (XML 1.0 §2.1): a second root element,
  and text, CDATA or a reference before or after the root. Only comments,
  PIs and white space may follow the root; the XML declaration, one
  DOCTYPE, comments, PIs and white space may precede it. A DOCTYPE after
  the root, inside it, or given twice is refused too. White space is
  space, tab, CR and LF, and in a `version="1.1"` document NEL and LS,
  which its end-of-line handling reads as LF;
- an XML declaration its grammar (XML 1.0 §2.8) rules out, at the
  pseudo-attribute or value at fault: `version` missing or not first,
  `standalone` before `encoding`, a pseudo-attribute given twice or one
  the declaration does not take, a `version` other than `1.` and digits,
  a `standalone` other than `yes` / `no`, and a declaration anywhere but
  the very start of the document. A processing instruction named `xml`
  in any case (`<?XML …?>`) is refused too;
- an encoding dq does not know (`UTF-32`) or cannot read (`ISO-2022-KR`),
  and an `encoding` that is not an encoding name;
- a declaration that the BOM, or the UTF-16 a BOM-less file begins in,
  contradicts (a UTF-16 BOM declaring `ISO-8859-1`);
- a namespace prefix no `xmlns:p` declares, on an element (`<a:b/>`) or
  an attribute (`<r a:x="1"/>`);
- `xmlns:p=""` unless the declaration says `version="1.1"`. Namespaces in
  XML 1.1 allow it, and the prefix is then undeclared inside that element.
- a character the version does not allow, raw or as a character
  reference, wherever it stands (text, attribute values, comments, PIs,
  CDATA): NUL, a surrogate, U+FFFE or U+FFFF in either version, a C0
  control but tab, LF and CR in XML 1.0, and in XML 1.1 a restricted
  character standing raw — 1.1 takes the C0 controls and U+007F–U+0084,
  U+0086–U+009F only as references. C1 controls are characters in 1.0.

The writer refuses the same, and a character the declared encoding cannot
hold, with exit 1, leaving an `-i` file as it was. It refuses a value of the
wrong shape rather than drop it — a map for `#text` or an attribute, a
child entry that is no list of elements (`c: 5`; a null one is no
content) — and names what it got: a string quoted (`(got "1.0")`), a
number or a boolean by kind and text (`(got a number 5)`), anything else
by kind (`(got a map)`, `(got an array)`).

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

Read-only: writing returns a format error. Diagnostics carry positions: a
pointer to an instruction (`/N`, `/N/instruction`, `/N/line`) resolves to
its keyword on the line where the instruction starts — not a comment above
it, a continuation line or a heredoc body below it; a pointer to
`/N/arguments` or anything inside it (an `ENV` name, an exec-form element)
resolves to the first argument on that line (the keyword when the arguments
begin on a continuation line). Columns count bytes, a leading BOM not
included. The document root has no position: a finding about the whole
file, such as a missing `HEALTHCHECK`, reports `1:1`.

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
