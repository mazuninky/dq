# Fix everything found on `fix/after-deps-update`

**Status: in progress.** Branch `fix/after-deps-update`, PR #40.

The dependency update surfaced a long list of bugs: some regressions, most of
them old. The goal of this branch is to fix all of them here and not leave a
"not fixed here" list behind. User-visible changes go into the `[Unreleased]`
section of [CHANGELOG.md](../../CHANGELOG.md) as they land. The open items are
mirrored in the [roadmap](../roadmap.md) bug queue.

## Done

Each block landed with regression tests that fail on the binary from before
the fix.

- **Multi-document YAML.** `lint`, `check` and `fix` evaluate every document of a
  stream, `fix` keeps every document, and `--doc N|all` works on every command.
  An out-of-range `--doc` exits 2.
- **Big numbers in readers.** Integers past `i64`/`u64` and decimals that no
  `f64` holds are read and written as spelled in YAML, JSON, TOML and the jq
  bridge.
- **Textual edits.** These cover whole-document `fix.ops`, `set` of the root,
  and a read-back check that refuses an edit the file would not read back as
  (exit 7).
- **Non-finite numbers outside jq.** `keys` and `values` on arrays.
- **Numbers, HCL, XML, YAML, TOON.**
  - A float on a 17th-digit tie is spelled with the even digit by every
    writer, and schema checks compare by that spelling. A schema limit is
    quoted in messages as the rule writes it (`1e2`).
  - HCL is read with one `hcl-edit` parse, and numbers keep their literal.
    Repeated blocks restore numbers from the last block. The writer is linear
    again (100k blocks: 30 s → 1.2 s), and a root that is not a map is
    refused in dq's own words.
  - XML: BOM-less UTF-16 keeps its byte order (`#xml.detected`), and UTF-8
    under a `utf-16` label is written back as UTF-8. The reader refuses
    undeclared prefixes, unknown encodings and characters the document's
    version does not allow. `xmlns:p=""` is allowed in XML 1.1. Whitespace
    and Unicode-space text survives a round trip. Line ends are handled per
    version everywhere. Parse errors name line and column at the name at
    fault. Writer refusals name what they got.
  - YAML: LS and PS are written escaped (`\L`, `\P`), including by textual
    `set`.
  - TOON: tie floats are spelled the same whatever strings the document holds.
- **Last round: schema notes, console, XML declaration, YAML controls, HCL
  non-finite** (2026-11-20).
  - Schema notes live out of band in a `SchemaNotes` table beside the rule's
    schema, keyed by RFC 6901 place; a user member named like a note is data.
    Tests cover a project rule, a `schema_file` sidecar, the embedded `@std`
    path, a composite `nested:` rule and limits behind `$ref`, `$id` and
    `$anchor`.
  - Console output and `validate` snippets keep non-ASCII text whole next to
    an escaped control character.
  - XML: the declaration is read by its grammar (§2.8), and parse errors are
    in dq's words at the fault, without quick-xml's wording or byte offsets.
  - YAML: textual `set` writes C1 controls, DEL, U+FEFF, U+FFFE and U+FFFF
    double-quoted with escapes; the native writer already did.
  - HCL output refuses NaN and infinity (exit 1) instead of writing `null` or
    a string.
  - The whole range since the dependency update was reviewed independently
    (Dockerfile/lint/schema, XML/HCL/TOON/numbers, YAML/jq/edit/patch).

## Next, in order

### `@std` rules and the fixture runner

- **Errors are silent.** On `kind: Pod` with `spec: x`, 11 `@std/k8s` rules
  raise a jq runtime error. The engine logs a warning and skips the rule, so
  `dq lint` reports no diagnostics and exits 0. The fixture runner goes
  through the same path and counts a raised rule as zero violations, so no
  fixture can catch it.
  - Plan: the evaluator reports rule errors through a channel of its own.
    `dq lint` keeps the warning; the fixture runner and `dq test` fail the
    case.
- **Wrong-type fields.** Every `@std` rule, in every namespace, tolerates a
  nested field of the wrong type.
  - A sweep test substitutes a string, a number, null, an array and a map at
    every pointer of every fixture input, and asserts that no rule raises.
- **`github-actions.no-pull-request-target-with-checkout`** must catch every
  spelling of the PR head:
  - `github.head_ref`;
  - `github.event.pull_request.head.ref` and `.head.sha`;
  - `head.repo.full_name` in `repository:`;
  - `refs/pull/<n>/merge` and `/head`, including `github.event.number`;
  - `format(...)`.

### Positions and schemas

- Dockerfile diagnostics carry the instruction's line. Today all of them
  report line 1.
- JSON Schema rejects two references as external that should resolve: a
  relative `$ref` to an embedded `$id`, and an absolute reference to the
  schema's own `$id`.
- A schema limit is not quoted as written in two places (tests in
  `cli_schema_forged_notes.rs`, `#[ignore]`d): behind a `$ref` that reaches
  outside the schema keywords (`$ref: '#/x-limits/a'` prints `1e+2` for
  `1E2`), and behind a YAML alias (`*lim` prints `100.0`).
- Console output lets C1 controls (U+0080–U+009F, e.g. U+009B CSI) through to
  the terminal; only C0 and DEL are escaped.

### Formats, reporters, globs

- HCL keeps only the last of repeated blocks with the same type and labels.
  No data may be lost.
- SARIF results carry no `ruleId`.
- jq path assignment through a missing key fails with "cannot use null as
  iterable". This hits `fix.jq`, and `set --jq` on a null document.
- `2-*.yaml` (no directory part) matches zero files.
- The JSON writer's trailing newlines are inconsistent, and so is the layout
  of `patch` `move`.
- `dq lint -` from stdin has no way to name the input format.

### README and docs against the code

The items are listed under "Found while writing these docs" in the
[roadmap](../roadmap.md). The most important ones:

- `dq self update` verifies no checksum, although the README says it
  verifies SHA256.
- Several README examples are wrong: `-F text`, `check` without `--rule`,
  `query` with its arguments reversed, rules without `description`, and the
  claim that a `check.jq` boolean result works.
- `.pom`, `.xsd` and `.env.*` are not detected.
- `fix.ops` is undocumented.
- `--no-pager` does nothing, and there are no `DQ_*` environment overrides.
- `-F` acts as the input format on most commands.
- Bulk `-i` counts unchanged files as Modified, and `*` crosses directories.
- The evals schema rejects the repository's own `evals.json`.
- `dq test @std/k8s` fails.
- A structural `dq set f.xml /a 2` is refused with exit 7 ("cannot set a
  value in it") for XML, HCL, INI, dotenv and CSV. The README lists these
  formats as writable, and `set --jq` works on them.

### Wrap-up

- The PR description lists everything fixed, with no deferred list.
- CI is green, CodeQL included.
- This page moves to a dated entry in [decisions.md](../decisions.md).
