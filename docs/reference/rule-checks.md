# Check kinds: jq, JSON Schema, composite

A rule's `check:` holds exactly one of four kinds. The shape is decided when
the rule loads:

| Kind | Fields | Message |
|---|---|---|
| jq | `jq`, `message` (required) | the template, per violation |
| inline schema | `schema`, optional `message` prefix | generated from the validator |
| schema file | `schema_file`, optional `message` prefix | generated from the validator |
| composite | `extract`, `nested`, `message` (required) | `<outer message>: <inner message>` |

Load-time errors (exit 3): more than one kind → `check_mutually_exclusive`
(naming the fields present); none → `check_missing`; `extract` without
`nested` or the reverse → `composite_incomplete`.

All compiled artefacts — jq programs, JSON Schema validators, nested
composite rules — are built once in `Evaluator::new` and reused for every
file. The evaluator is `Send + Sync + Clone` so bulk runs share it.

## `jq`

See [lint-engine.md](lint-engine.md#evaluation): every value the expression
emits is one violation, rendered through `message`, positioned by `loc`.

## JSON Schema (`schema`, `schema_file`)

JSON Schema **2020-12** through the `jsonschema` crate. Validation runs on
dq's own value of the document, not on a JSON rendering of it; each
validation error becomes one diagnostic:

- the error's `instancePath` is the diagnostic's pointer, and its position
  comes from the IR's spans (`line:col` on YAML / JSON / TOML /
  Dockerfile, line 1 elsewhere);
- the message is the optional `message` prefix, the `keywordLocation`, and the
  validator's text, joined with `: `: `/properties/age/type: "twelve" is
  not of type "integer"`, or with `message: shape`, `shape:
  /properties/age/type: …` — the separator a composite puts between its
  `message` and a nested message. Trailing spaces and one trailing colon of
  the prefix are dropped first, so `shape`, `shape:` and `"shape: "` read
  alike; a blank prefix is no prefix. Values are spelled as the document
  spells them (`.nan is not of type "string"`), and a limit as the rule or
  the schema file writes it (`the maximum of 1E2`). That holds wherever the
  limit sits: behind a YAML alias (`x: *lim`), or in a place a `$ref`
  reaches outside the schema keywords (`$ref: '#/x-limits/a'`).

Numbers a JSON document cannot hold follow these rules:

- A NaN or an infinity (`.nan`, `-.inf`, TOML `nan`) is a `number`, never a
  `string` or an `integer`.
- An infinity is past every bound on its side; a NaN fails every bound
  (`minimum`, `maximum`, the exclusive ones) and every `multipleOf`, and so
  does a number past the `f64` range.
- `integer`, `enum` and `const` read a big number by its exact value
  (`18446744073709551616` and `1e400` are integers, `1e-400` is not).
- Two NaNs equal nothing, though `uniqueItems` counts them as one item.

`schema` holds the schema inline, as YAML. `schema_file` names a JSON file
**relative to the rule's directory**. Absolute paths
(`schema_file_absolute_path`) and paths that leave the rule directory through
`..` (`schema_file_escapes_rule_dir`) are refused at load time, exit 3. For
the embedded `@std` rules the sidecar schemas are compiled into the binary
(`dq_lint::std_schema`), so no file system access is needed.

### `$ref` stays local

Schemas validate in isolation: no network and no file reads beyond the
rule's own sidecar. Three layers enforce it: the validator is built offline
(any resource outside the schema's own registry fails the build with
`external $ref is not supported: <uri>`); a pre-scan names the external
`$ref`s it can see, for a clearer message; and the crate's `resolve-http` /
`resolve-file` features are off. A schema with an unresolvable `$ref` fails
with `schema_compile` (exit 3).

A `$ref` is internal when it resolves inside the schema document. That
is a fragment (`#/$defs/x`, `#anchor`), or a reference that names the root
or a schema's `$id` once it is resolved against the base of the schema it
sits in (that schema's own `$id`, else the nearest enclosing one). Both of
these resolve:

```yaml
$id: https://example.com/root.json
$defs:
  inner: { $id: inner.json, maximum: 5 }   # https://example.com/inner.json
  x: { minimum: 0 }
properties:
  a: { $ref: inner.json }                  # the embedded $id, relative
  b: { $ref: 'https://example.com/root.json#/$defs/x' }   # the schema's own $id
```

Everything else is external and refused, even when it looks close: a URI
no `$id` names (`other.json`, `https://example.com/other.json`), a `file:`
path, a `$id` inside data (`const`, `default`, an unknown keyword), which
names no resource, a `$dynamicRef` outside the document, and a `$schema` that
names a meta-schema dq does not bundle. Nothing is fetched to decide: dq
never opens a connection or reads a file for a reference.

Not supported, by decision: XSD, RelaxNG, Schematron, and OpenAPI request /
response validation. OpenAPI documents are checked for shape only
(`@std/openapi`, `@std/jsonschema/openapi-3.1-shape`).

## Composite (`extract` + `nested`)

A composite rule lints one format embedded in another — YAML in a Markdown
code block, JSON in a YAML block scalar — and reports findings at their
position in the outer file.

```yaml
id: markdown.code-blocks-yaml-valid
description: YAML code blocks must parse.
severity: error
match: { format: markdown }
check:
  extract: |
    [paths(objects | select(.type == "code_block" and .fenced == true and .lang == "yaml")) as $p |
     getpath($p) as $node |
     {value: $node.value, format: "yaml", anchor: ($p | map("/" + tostring) | add // "")}]
  nested:
    id: markdown.code-blocks-yaml-valid.inner
    description: Parse-only; the outer rule's parse-failed branch is the finding.
    severity: error
    match: { format: yaml }
    check: { jq: 'empty', message: 'never fires' }
  message: "YAML code block invalid"
```

(Abridged from `crates/dq-lint/rules/markdown/code-blocks-yaml-valid.yml`.)

### The `extract` contract

`extract` runs on the outer document and must produce one array of objects,
each with three string fields:

- `value` — the text to re-parse;
- `format` — a parser name (`yaml`, `json`, `toml`, `markdown`, `xml`, …);
- `anchor` — the RFC 6901 pointer of the scalar in the outer document that
  holds the text.

Shape errors carry stable kinds: a non-array result
(`composite_extract_not_array`), an item without one of the fields
(`composite_extract_malformed`, naming it), an unknown format
(`composite_extract_unknown_format`). In the current runtime these surface as
diagnostics on the file and evaluation continues with the next file (the
original design made them errors with exit 1).

### Per item

1. The `value` is parsed with the named parser. If that fails, one
   diagnostic `<outer-id>.parse-failed` (severity error, message
   `<format> parse failed: <error>`) is placed at the anchor, `nested` is not
   run for that item, and the remaining items are still processed.
2. Otherwise the `nested` rule — a full rule with its own `match`, `check`,
   and possibly another composite — runs on the parsed value.
3. Each nested diagnostic is projected onto the outer file and its message is
   prefixed with the outer `message`.

### Projection

With the anchor's position (`line`, `col`) and the anchor's inline offset
(where the scalar's content starts, see
[ir-and-edit-ops.md](ir-and-edit-ops.md#inline-offsets)):

```
line = anchor.line + inner.line - 1
col  = inner.line == 1 ? inline.col + inner.col - 1 : inner.col
```

So a YAML block starting at line 10 with an inner finding at 3:7 lands on
12:7. Without an inline offset only the anchor position is used; without an
anchor position the inner coordinates are kept and a warning is logged.

### Depth bound

Composites can nest, but at most `MAX_EXTRACT_DEPTH = 4` levels (the outer
rule is depth 0); deeper chains fail with `composite_depth_exceeded`. The
bound is fixed — rule YAML cannot change it; `Evaluator::with_max_extract_depth`
exists for unit tests.

The one composite rule shipped today is `@std/markdown/code-blocks-yaml-valid`
(README also calls the `@std/jsonschema` rules composite; they are schema
rules — see [std-rules.md](std-rules.md)). Plugins do not see inline offsets
(the plugin ABI does not carry them).
