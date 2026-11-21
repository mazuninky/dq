# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions are
calendar-based, `YYYY.WW.BUILD` (see [Releases & versioning](README.md#releases--versioning)), so
a version number says when a release was cut, not whether it is compatible:
breaking changes are listed first under Changed.

## [Unreleased]

### Changed

- Versions in ISO weeks 1–9 have no leading zero: the first release of
  2027 is `2027.1.1`, not `2027.01.1`. Cargo versions are SemVer, which
  forbids a leading zero, so `scripts/bump-version.sh` would have written a
  version Cargo refuses from 2027-01-04. The release and Docker workflows
  accept the new spelling.

### Fixed

- Install and pin examples in the README and the skill named `v2026.20.1`,
  a release that does not exist; they now name `v2026.47.3`.

## [2026.47.3] - 2026-11-21

### Added

- Prebuilt binaries for Intel macOS (`x86_64-apple-darwin`). The release
  ships `dq-<version>-x86_64-apple-darwin.tar.gz`, `scripts/install.sh`
  installs it on an Intel Mac instead of refusing, and `dq self update`
  finds it. The Homebrew formula can now name a URL for every platform:
  current Homebrew checks a tap's formulae on every OS and architecture,
  and `brew tap mazuninky/tap` failed with "invalid syntax in tap" while
  Intel macOS had none.

## [2026.47.1] - 2026-11-20

### Added

- A JSON Schema written in YAML may list `.inf` and `-.inf` in `enum`
  and `const`.
- `--input-format <FORMAT>`, a global flag, names the input format.
  It overrides detection for files, globs and stdin, and stdin (`-`)
  needs it: `dq lint - --input-format yaml -F sarif < deploy.yaml`.
  `-F` is the output format only (see Changed).
- `dq self update --dry-run` downloads and verifies the release and
  reports what it would install, without replacing anything.
- `@std/skills` checks Claude Code skills. `skills.frontmatter` checks
  a `SKILL.md` frontmatter. `skills.evals-schema` checks an
  `evals.json` against skill-creator's contract: an eval's `name` /
  `eval_name` label, and `assertions` or `expectations`.

### Changed

- `dq select` now follows RFC 9535 strictly (jsonpath-rust 0.7 → 1.0).
  - Filters without parentheses (`$.items[?@.n > 1].name`), function
    extensions such as `length(@.tags)`, and negative indices
    (`$.items[-1]`) now work. They used to exit with code 1.
  - The 0.7 extensions `.length()`, `.['x']` and `=~` are rejected with
    exit code 1 and `invalid JSONPath expression:`. Use `length(@.x)`,
    `['x']`, and `match()` or `search()` instead.
  - `in` is not an RFC 9535 operator: `[?(@.name in ["x"])]` returns `[]`
    with exit code 0 instead of matching. A space inside a dot-name is
    part of the name, so `$.s y` looks up the key `"s y"`.
  - `$..*` lists a node's children before descending into them. The
    values are the same; the order differs.
- `-F toon` writes TOON spec v4 (toon-format 0.4 → 0.7). An empty array
  is written as `roles: []` instead of `roles[0]:`. Strings such as
  `#bob` and `+1`, and non-ASCII keys such as `ключ`, are quoted. Tools
  that read dq's TOON output need a v4 reader.
- Markdown IR `position.end` moved for some nodes (comrak 0.52 → 1.0).
  Lists and list items end at their last child: in `> - a\n>\n> b\n` the
  list ends at `1:5`, not `2:1`. Indented code blocks end at their last
  character instead of the start of the next line: a block on lines 8–9
  ends at `9:8`, not `10:0`. Rules and scripts that compare these
  positions need new expectations.
- JSON Schema rules whose root `$schema` names draft-04, draft-06 or
  draft-07 now validate (jsonschema 0.34 → 0.61). jsonschema 0.34 skipped
  the root keywords of such schemas, so these rules reported nothing.
  dq applies 2020-12 semantics to them, so syntax from the older drafts
  fails to compile: array-form `items` (draft-07) and a boolean
  `exclusiveMinimum` (draft-04) are rejected. Schemas copied from
  SchemaStore often declare draft-07 and may start reporting violations.
- Some JSON Schema messages read differently. `pattern` messages quote
  the regex as written in the schema (`\w+@\w+`) instead of a rewritten
  form, a `type` list is printed in a different order (`"null",
  "integer"`), and `$dynamicRef` paths include every evaluation step.
  Exit codes and positions are unchanged.
- TOML parse errors use the toml_edit 0.25 wording. For `bad = ` the
  message is now `string values must be quoted, expected literal string`.
- Building from source needs Rust 1.100 (was 1.94). The Docker images
  build on `rust:1.100-alpine` and run on `alpine:3.24`.
- The plugin host runs on wasmtime 50. The WIT contract
  `dq:plugin@0.1.0` is unchanged, and plugins built with wit-bindgen 0.57
  still load.
- Plugins are limited in the number of core instances, memories and
  tables they create, and plugins with 64-bit memories or tables are no
  longer accepted.
- Dockerfile IR: `arguments` of a shell-form instruction is the text as
  written, so `RUN curl -fsSL https://x | bash` reads as
  `"curl -fsSL https://x | bash"`; it used to be split on spaces into an
  array, with COPY/ADD flags dropped. The JSON form
  (`RUN ["sh", "-c", "echo hi"]`) is an array of its elements; it used to
  be one comma-joined string. `line` is the line the instruction starts
  on; it used to be the instruction's position in the file. dq parses
  Dockerfiles itself now, and the `dockerfile-parser-rs` dependency is
  gone.
- `@std/dockerfile` rules run on Dockerfiles only. They used to run on any
  YAML file shaped like a list of instructions.
- `--doc N|all` is honoured by `get`, `exists`, `keys`, `values`, `len`,
  `type`, `paths`, `select`, `query`, `convert`, `set`, `del`, `lint`,
  `check` and `fix`.
  - `dq --doc 1 set bundle.yaml /metadata/name z` used to fail with
    "path not found", exit 2; it now edits document 1. `/1/...` without
    `--doc` still works.
  - Over several files, a file without document N is skipped with a
    note on stderr and counted under Skipped; the run used to abort at
    that file with exit 2. When no file holds document N, the run exits
    with 2 (`--doc 7 is out of range: none of the 3 files holds
    document 7`) and writes nothing. It used to skip every file and
    exit 0, so `dq --doc 7 lint 'g/*.yaml'` printed `diagnostics: []`.
  - On a file of one document, `--doc N` with N > 0 used to be ignored
    and is now out of range, exit 2: `dq --doc 1 get one.yaml /kind`
    printed `Pod` and now prints `--doc 1 is out of range: one.yaml
    holds 1 document(s) (--doc 0)`. `--doc all` wraps a single document
    in an array of one, so `/0/...` addresses it as in a stream: with
    `printf 'a: 1\nb: 2\n' > x.yaml`, `dq --doc all query 'length'
    x.yaml` printed `2` and now prints `1`.
  - An out-of-range `--doc` on a stream (`dq --doc 5 get bundle.yaml
    /kind`) was exit 1 and is now exit 2. A value that is neither an
    index nor `all` (`dq --doc x get f.yaml /a`) was exit 1 and is now
    a usage error, exit 2.
  - `--doc` on a command that cannot honour it used to be ignored and
    is now a usage error, exit 2: ``error: `--doc 1` is not supported
    by `dq fmt`: it re-emits the whole file``. This covers `fmt`,
    `validate`, `diff`, `patch`, `merge`, `convert -i`, and `--doc all`
    with `set` or `del`.
- A plugin runs once per document of a YAML stream. Its diagnostics
  without a position land at each document's first line.
- YAML collections set into a flow context are written in YAML flow
  style instead of JSON: `{}` with `set '' '{"b": 2}'` gave `{"b": 2}`
  and now gives `{b: 2}`. Strings are quoted only when they need it
  (`"1"`, `"on"`, `"a: b"`).
- `dq patch --no-create` is documented as a no-op kept for symmetry
  with `set`: `patch` follows RFC 6902 and never creates missing
  parents. On `a: 1`,
  `dq patch f.yaml '[{"op":"add","path":"/b/c","value":2}]'` exits 2
  with "path '/b/c' not found" with or without the flag, as it always
  did.
- Some messages are clearer; the exit codes stay. `dq set` and `dq del`
  on Markdown and other formats without textual edits say "markdown has
  no textual edits: dq can read a markdown file but cannot set a value
  in it" instead of "loaded read-only; reload via a write-aware
  parser". An insert that cannot be written says "cannot insert
  '/arr/1'" instead of "cannot set '/arr/1/k'". A `fix.ops`
  `remove ""` that would take every document of a stream says "every
  document of the stream would go".
- `values` and `keys` work on arrays, and a node of the wrong type is
  exit 1. `dq values f.yaml /list` used to fail with "path '/list' not
  found" (exit 2) and now prints the items; `dq keys f.yaml /list`
  prints `0`, `1`, …. `dq keys f.yaml /n` on a number fails with
  `cannot list the keys of '/n': it is int, not an object or an array`
  (exit 1), and so does `len` on a number, a bool or null.
- `convert -F hcl` and `-F toon` refuse a number they cannot hold with
  exit 1, naming it as dq read it (`hcl format error: HCL cannot
  represent arbitrary-precision number 18446744073709551616`). HCL used
  to write it as a string; TOON wrote `null` or a rounded value.
  `dq set` of such a number in a TOML file says "TOML cannot represent
  arbitrary-precision number <n>" (exit 7) instead of suggesting
  `dq set --jq`.
- Console output of `get`, `values`, `select` and `query` spells floats
  the way `-F yaml` does: `get f.yaml /a` printed `1e+300` and now
  prints `1.0e+300`, `1e400` printed `1e+400` and now `1.0e+400`, and
  `1e-7` is now `1.0e-7`.
- Plugins: `get-root`, `get-at` and `jq.eval` spell a float in its
  shortest form and keep a big number exact, as dq holds it (`1e300`
  was `1e+300`, `1e400` was `1e+400`). A NaN or an infinity is sent as
  the string `".nan"`, `".inf"` or `"-.inf"`.
- HCL output writes every float in its shortest spelling. Writing a
  large HCL file is no longer superlinear: 100k blocks took 30 s and
  now take 1.2 s.
- An HCL file is read with a single `hcl-edit` parse that keeps every
  number's literal, so `3.0` stays a float and big integers stay exact.
  Reading Terraform-like files takes about 1.05x the time it did, and a
  file made only of 17-digit floats about 1.27x.
- JSON Schema numeric checks are faster on plain numbers: they compare
  them without formatting them first.
- The XML reader refuses a namespace prefix that no `xmlns:` declares
  (`<a:b/>`) and an `encoding` it does not know or cannot read
  (`UTF-32`, `ISO-2022-KR`) with exit 3, as the writer does.
  ISO-2022-JP files now read correctly; they used to read as UTF-8 with
  the escape bytes in the text.
- **Breaking: `-F/--format` is the output format on every command** and
  no longer picks the parser. On `get`, `paths`, `keys`, `values`,
  `validate`, `set` and most other commands, `-F json` used to force the
  JSON parser, so `dq paths config.yaml -F json` failed to parse. Use
  `--input-format` to override detection.
- **Breaking: stdin (`-`) needs `--input-format`.** Without it every
  command exits 2. It used to exit 6, and `query` took the format from
  `-F`.
- **Breaking: `set`, `del`, `patch`, `merge`, `fmt` and `fix` keep a file
  in its own format.**
  - A `-F` that names another format is a usage error (exit 2).
  - The file's own format is accepted, so `-i -F yaml` on a YAML file
    now works (it was exit 6).
  - `convert` is how a file changes format.
- **stdin can be read only once.** A command given `-` for more than one
  input is refused with exit 2: `diff - -`, `patch - -`, `merge - -`,
  `set - /a @-`, and `-` twice in the files of `lint`, `check` or `fix`.
  - Only a literal `-` argument means stdin. A glob or a directory
    yields a file named `-` as `./-`.
  - `set … @-`, `--value-from -`, `patch … @-`, `--ops-from -`,
    `merge … @-` and `--patch-from -` read stdin. The last four used to
    read a file named `-`.
  - A value read from stdin is parsed like an inline value.
- `--input-format` on `test`, `explain`, `rules`, `completions`, `man`,
  `self` and `generate-docs` is a usage error (exit 2), as `--doc` already
  was. It used to be ignored.
- `--no-pager` is hidden from `--help`. It is still accepted and does
  nothing; dq has no pager.
- `set`, `del`, `patch` and `merge` edit files with Go-template or GitHub
  Actions markers whose templates do not change what the file reads
  as, e.g. a workflow's `run: echo ${{ github.sha }}`. Any marker used to
  be exit 3. Helm-style templates (`port: {{ .x }}`) are still refused. An
  edit that would not keep a template is refused (exit 3) and the file is
  left alone. See [docs/reference/template-guard.md](docs/reference/template-guard.md).
- `fmt` refuses `--allow-templates` and `--raw-template-strings` (exit
  6), as `fix` and `set --jq` already did.
- `dq lint` and `dq check` report a file that does not parse as a
  `lint.parse` result (`lint.templated_file` for a refused template),
  lint the other files and still exit 3. SARIF and the other structured
  reports are written, so a CI upload step gets them. The run used to
  stop at the first such file and write no report.
- `dq fmt` canonicalises JSON with no options: two-space indent and one
  final line break. It used to copy a JSON file unchanged, so
  `{"a":   1}` counted as formatted. Stdout mode always prints the
  document, even an unchanged one. A JSON file's BOM is dropped, as
  every other rewrite drops it.
- XML output always ends with exactly one line break, `\r\n` if the
  file used it. The declaration and every top-level comment and
  processing instruction stand on their own lines, in document order,
  before and after the root.
- JSON Lines takes point edits (`set`, `del`, `patch`, `merge`), with
  records addressed by index (`/0/a`, `/-`). They were exit 7. A root
  that is not an array of records is refused with exit 1.
- Schema-check messages separate the rule's `message` prefix from the
  keyword location with `: ` (`shape: /required: …`, was
  `shape/required: …`). Composite rules trim their prefix the same way:
  `message: "cfg:"` no longer gives `cfg:: …`.
- `validate -F json` diagnostics carry a `rule_id` (`validate.parse`, …).
- `dq test -F json`: a failed result has a `raised` list.
- `convert` to stdout takes a glob that matches exactly one file; more
  matches are exit 6. Glob matches in the current directory print
  without `./`.
- `diff --unified` labels files with the paths as given (`--- a.yaml`),
  not `a/` joined with an absolute path.
- Console output quotes strings inside a collection as YAML flow
  (`{name: '{{ .p }}'}`), so such a line reads back as the same value.
  This includes the console lint, check and validate diagnostics, so a
  message with `: ` in it is quoted.

### Fixed

- XML: the declaration is read by its grammar.
  `<?xml version="1.0" standalone="yes" encoding="UTF-8"?>` was accepted;
  it is now exit 3, `XML parse error at line 1, column 38: the XML
  declaration gives 'encoding' after 'standalone': …`. Also refused: a
  missing, repeated or unknown pseudo-attribute, `standalone` other than
  `yes`/`no`, a `version` other than `1.` and digits, a declaration not at
  the very start of the document, and `<?XML …?>`.
- XML: parse errors are in dq's words, placed at the fault, without
  quick-xml's wording or byte offsets. `<c a="1" a="2"/>` used to give
  `malformed attribute: position 8: duplicated attribute, previous
  declaration at position 2`; it now gives `XML parse error at line 2,
  column 12: attribute 'a' appears twice on <c>`.
- YAML: textual `set` writes a value or key holding a C1 control, DEL,
  U+FEFF, U+FFFE or U+FFFF double-quoted with escapes, whatever scalar it
  replaces. It used to refuse the edit (exit 7), or write U+FEFF raw:
  `dq set f.yaml /a $'a\u0080b'` now writes `a: "a\x80b"`.
- HCL output refuses NaN and infinity with exit 1 and writes nothing
  (`hcl format error: HCL cannot represent non-finite number .nan`).
  `dq set s.tf --jq '.a = nan'` wrote `a = null`, and `convert -F hcl` of a
  YAML `.nan` wrote the string `a = ".nan"`; `fix` and `convert -i` are
  refused the same way.
- Console output keeps non-ASCII text whole next to an escaped control
  character. `{"a": "é\u0001"}` printed `{a: Ã©\x01}` and now prints
  `{a: é\x01}`; `validate` snippets broke the same way.
- A JSON Schema member named like one of dq's internal notes is the
  user's own data. It used to be read as dq's note and could change a
  message: `maximum: 5` with a forged spelling note printed `greater than
  the maximum of garbage`.
- A YAML file with a UTF-8 BOM parses and can be edited, the BOM kept on
  write. With one key, `set /key` used to fail with `path '/key' not
  found`; with more, parsing failed with "did not find expected
  <document start>". Some other documents that saphyr-parser 0.0.6
  rejected parse too (saphyr-parser 0.0.6 → 0.3).
- Plugins: the 64 MiB memory limit applies to all of a plugin's memories
  together, and a plugin stopped by it reports `exceeded memory limit`.
- Log messages (`WARN` and the `-v` levels) go to stderr instead of
  stdout, so `dq -F json … | jq` gets clean JSON. They are coloured only
  when stderr is a terminal and colour is not turned off.
- `dq lint` of a YAML file no longer prints four `check.jq raised a
  runtime error` warnings from the `@std/dockerfile` rules. A Dockerfile
  with no instructions (empty, or comments only) no longer gets
  `@std/dockerfile/has-healthcheck`.
- `dq lint` of a file whose root is not a mapping (a list, a scalar, an
  empty YAML file) no longer prints `match.filter raised a runtime error`
  warnings from the github-actions, k8s, openapi, jsonschema, npm and
  skills rules; a list root used to print 26 of them.
  `@std/jsonschema/helm-values-against-schema` checks mapping roots
  only: it used to fail every other YAML file, an empty one included,
  with exit 4. The rules also stopped raising on a mapping whose fields
  have an unexpected type, such as `jobs` or `paths` given as a list,
  `"engines": "node"`, or a Terraform `resource = "x"` attribute.
- `@std/github-actions` rules need the `on` key that every workflow has,
  so a CircleCI config, which has `jobs:` too, no longer gets
  `has-permissions` and `has-timeout`.
- `@std/github-actions/no-pull-request-target-with-checkout` fires when
  `on` is written as a string (`on: pull_request_target`) or a list
  (`on: [push, pull_request_target]`); it used to check only the
  mapping form.
- `@std/markdown/frontmatter-required-fields` reports the missing fields
  on scalar frontmatter, as it does on list frontmatter; it used to
  print a runtime-error warning and report nothing.
- JSON Schema rules: a `$ref` inside `const`, `enum`, `default` or
  `examples` is data and no longer rejected as an external reference. A
  `$ref` in a schema position, including under a property named `enum`,
  still is. Every external reference the validator would follow fails
  with `external $ref is not supported: <uri>`, including one reached
  through a local `$ref` and an external `$dynamicRef`; some used to fail
  with a message about the `resolve-http` feature.
- Dockerfiles that Docker builds no longer fail to parse with exit 3:
  `HEALTHCHECK`, `ONBUILD` and `MAINTAINER`, a one-character argument
  (`WORKDIR /`, `USER 0`, `EXPOSE 8`), lower-case instructions, heredocs
  (`RUN <<EOF`), a UTF-8 BOM, and the `# escape=` directive. `ENV A 1`
  keeps its value; it used to read as `{}`. A Dockerfile that Docker
  rejects fails with the line of the bad instruction.
- `@std/dockerfile` on real Dockerfiles:
  - `has-healthcheck` passes on a Dockerfile with a `HEALTHCHECK`. It
    used to fire on every Dockerfile that parsed.
  - `no-curl-pipe-bash` catches `RUN curl … | bash`, including
    `| /bin/sh`, `| sudo bash` and a command split over lines. It used to
    catch only the exec form (`RUN ["sh", "-c", "curl … | bash"]`).
  - `no-add-use-copy` no longer flags a remote source (`ADD https://…`,
    `git@…`, `ssh://…`), a local tar archive, which ADD unpacks, or a
    heredoc.
  - `no-latest-base-image` no longer flags `FROM scratch` or an earlier
    build stage (`FROM build`), and catches an untagged image on a
    registry with a port (`localhost:5000/img`).
- `dq set` / `del` / `lint` no longer panic on a file with an empty or
  null key: `"": 1` in YAML, `{"": 1}` in JSON, `"" = 1` in TOML,
  `f: {? &a}` in YAML. `set` of a new key next to an empty key that is
  its map's only key writes it after that key; it used to fail with
  "not found". `del` of a key in a compact list entry before an empty
  key (`- a: 1` + `  "": 2`, `del /0/a`) works; it used to fail with
  exit 7.
- YAML `del` of an explicit key with no value (`? k` + `? j`, `del /k`)
  removes only that key; it used to fail with exit 7.
- A composite `extract` item whose `value`, `format` or `anchor` is not
  a string says "field `value` must be a string, got a number"; it said
  "missing field `value`".
- `-F hcl` of an array or scalar root is refused in dq's own words
  ("its root is a map, not an array") instead of a serde message. The
  exit code is still 1.

Numbers — NaN, infinities, big integers and long floats:

- YAML `.nan` / `.inf` / `-.inf`: `convert -F json` fails, as it already
  did for TOML `nan`, since JSON has no such number; it used to print
  `.nan` into the JSON. `convert -F yaml` writes them as floats again (it
  wrote the strings `'.nan'`), `convert -F toml` and TOML `set` write
  `nan` / `inf` (they failed). JSON `set` of NaN or infinity fails with
  exit 7 and leaves the file alone; it used to crash.
- NaN and infinity reach jq as numbers in `query`, `set --jq`, `fix` and
  lint rules: `.x | type` is `"number"` and `isnan` works. They used to
  reach it as the strings `".nan"` or `"NaN"`, so `set --jq '.y = 2'` on
  a file with `x: .nan` rewrote `x` as the string `'.nan'` (TOML:
  `x = "NaN"`). `query 'nan'` and `query 'infinite'` print the number
  instead of failing with a conversion error. `query -F json` and
  `-F jsonl` of a NaN fail with exit 1, as `convert -F json` does; they
  used to print the string `".nan"`. `get` and `query` print them as
  `.nan`, `.inf` and `-.inf` whatever the source format; a TOML `nan`
  used to print as `NaN`. `set --jq` writes YAML
  `.NaN` back as `.nan` and TOML `+inf` as `inf`.
- `set --jq` on a JSON file with a number beyond f64 (`1e400`) keeps the
  number; it used to write `{"$serde_json::private::Number": "1e+400"}`
  in its place.
- A YAML integer past `u64` or below `i64::MIN` no longer fails every
  command. On `a: 18446744073709551616`, `dq get f.yaml /a` printed
  `invalid type: integer as u128` (exit 3); it now prints
  `18446744073709551616`.
- YAML and JSON numbers keep their exact value. An integer past i64,
  or a float no f64 holds (more than 17 significant digits, or out of
  f64's range like `1e400`), is read as written; it used to be rounded
  or read as a string. On YAML, `convert -F json` turned
  `c: 123456789012345678901234567890123456789012` into
  `123456789012345680000000000000000000000000.0` and `b: 1e400` into
  `"1e400"`. On JSON, `dq get f.json /b` turned
  `0.1000000000000000000000000001` into `0.1`. Ordinary floats,
  including 17-digit `%.17g` data such as `0.10000000000000001`, stay
  plain floats.
- YAML `!!float 1e400`, `!!float "1e400"` and `!!int` values past 128
  bits are read as numbers instead of failing to parse.
  `!!float 18446744073709551617` is written back as
  `18446744073709551617.0`.
- `dq set f.json /b 0.1000000000000000000000000002` wrote `0.1` and
  exited 0. It now writes the full literal.
- HCL keeps integers past 64 bits exact: `x = 18446744073709551617`
  was read as `1.8446744073709552e+19`, and `dq get m.tf /x` now prints
  `18446744073709551617`. When a block repeats, the exact number comes
  from the last block in source order.
- HCL whole floats stay floats. `dq convert f.yaml -F hcl` wrote
  `e: 3.0` as `e = 3` and `1e16` as `h = 10000000000000000`; it now
  writes `e = 3.0` and `h = 1e16`. dq read `e = 3.0` in a `.tf` file as
  the integer `3`: `dq type w.tf /e` printed `int` and now prints
  `float`, and `convert w.tf -F json` prints `"e": 3.0` (was `"e": 3`).
- JSON output prints floats in their shortest round-trip form. With
  `a: 1e300`, `dq -F json convert f.yaml` printed a 301-digit number,
  and `1e-300` came out as `0.` and 300 zeros; they are now `1e300` and
  `1e-300`. `1.5e20` is no longer `150000000000000000000.0`, and `3.0`
  stays `3.0`. This covers `convert`; `query`, `get`, `values` and
  `select` with `-F json` or `-F jsonl`, which now also spell `1e400`
  one way; `set --jq`, `fmt`, JSON frontmatter and `diff`.
- The TOML writer, and `set` on YAML and TOML, no longer write runs of
  digits: `convert -F toml` gives `a = 1e300`,
  `dq set s.toml /a 1.5e20` gives `a = 1.5e20`, and
  `dq set s.yaml /a 1e300` gives `a: 1.0e+300`. `-F toon` still writes
  plain decimals, as the TOON spec requires.
- The .env, INI, CSV/TSV and XML writers no longer write floats as long
  runs of digits. With `a: 1e300`, `dq convert v.yaml -F dot-env`
  printed a 301-digit `a` and now prints `a=1e300`. `1.5e20` printed as
  `150000000000000000000` and is now `1.5e20`; `3.0` printed as `3` and
  is now `3.0`.
- A float on a 17th-digit tie, such as `1658206780088562.2`, is spelled
  with the even digit by every writer, TOON included; TOML wrote
  `…562.3`. Schema checks compare it by that spelling, so
  `maximum: 1658206780088562.25` no longer rejects
  `1658206780088562.2`.
- `dq patch` `test` ops and `dq diff` treat `1e400` and `1e+400` as the
  same number. `diff` prints big numbers as numbers; it printed
  `"0.1000000000000000000000000002"` as a string.
- Read commands write NaN and infinity one way. `dq values n.toml ''`
  printed `NaN` / `inf` and now prints `.nan` / `.inf`;
  `dq select n.yaml '$.x' -F yaml` printed `- '.nan'` and now prints
  `- .nan`.
- In `select` filters a NaN or an infinity is a number, not the string
  `".nan"`: `$[?@.n > 1]` matches `.inf` and never `.nan`, and
  `$[?@.n == '.nan']` matches only the string. Filters still compare in
  f64, so `.inf == .inf` is false and a filter literal past i64 is a
  parse error; use `query` for exact numeric tests.
- `isnan` works in `loc:`, in composite `extract:` and in the plugin
  `jq.eval` import; it used to raise on the string `".nan"`.
- `dq test` reads fixture inputs the way `dq check` reads files: a YAML
  `x: .nan` was `null` in a fixture and is now a number, and JSON big
  integers keep their digits.

The YAML writer, used by `convert -F yaml`, by `set --jq` and `fix` on
YAML files, and by `get` / `query -F yaml`:

- It quotes strings that a YAML 1.1 reader (PyYAML; go-yaml, as in
  Kubernetes and Helm) would read as another type, both as values and
  as keys: `'1:20'`, `'on'`, `'y'`, `'2001-12-14'`, `'1_000'`, `'0b1'`.
  It used to write them plain, and PyYAML read `1:20` as the number 80
  and `on` as `true`. In a GitHub Actions workflow that `set --jq`
  rewrites, the `on:` key is now written `'on':`. A `<<` merge key over
  a map stays plain.
- It writes an integer beyond i64 as a plain number, however large
  (`c: 123…`); it used to write it as a quoted string (`c: '123…'`).
- It writes a float in exponent form with a dot and a signed exponent,
  `1.0e+16` and `1.5e+300`, which YAML 1.1 reads as a float. It used to
  write `1e16` and `1.5e300`, which PyYAML reads as strings. A float
  literal no f64 holds is written the same way (`1.0e+400`); it used to
  be quoted, or written as `1e+400`. dq and PyYAML read such an integer
  or float back as a number.
- It writes a string holding LS or PS (U+2028, U+2029) double-quoted,
  with `\L` / `\P`, never raw in single quotes or a block scalar, so
  YAML 1.1 and 1.2 readers read it alike. Textual-edit `set` writes
  NEL, LS and PS escaped; it used to refuse such values.

JSON Schema rules:

- A NaN or an infinity is a number. `x: .nan` against `{type: number}`
  failed with `".nan" is not of type "number"` and now passes. Against
  `{maximum: 10}` it passed and now fails with `.nan has no order, so
  it is not at most the maximum of 10`. `.inf` against `{maximum: 10}`
  passed and now fails.
- `integer`, `enum` and `const` compare big numbers by exact value:
  `x: 1e400` against `{type: integer}` failed and now passes, and
  `x: 1e-400` passed and now fails.
- A big number in a YAML rule's schema no longer fails the rule:
  `const: 18446744073709551616` gave `rule parse error … as u128`,
  exit 3. `const`, `enum`, `minimum`, `maximum` and the exclusive
  bounds hold exact values, and `maximum: 1e400` compiles.
- Bounds compare exactly, in a `schema_file` JSON too:
  `maximum: 18446744073709551616` let `18446744073709551617` pass
  (exit 0); it now fails with `18446744073709551617 is greater than the
  maximum of 18446744073709551616`, exit 4.
- `multipleOf` compares exact values. `multipleOf: 1e400` accepted
  every number, and now only `0` passes; `0.3` is a multiple of `0.1`,
  and `1e400` of `2`. `multipleOf: 1e-400` compiles; it used to fail
  with `1e-400 is less than or equal to the minimum of 0`, exit 3.
  `multipleOf: 18446744073709551616` on `4` said `… multiple of
  18446744073709552000`; the message now gives the exact divisor.
- A message quotes a limit (`multipleOf`, `maximum`, …) as the rule or
  schema file writes it: `0.00000000001`, not `1e-11`, and `1e2`, not
  `1e+2`. A schema printed in a `not` message reads as the rule wrote
  it too, with limits in their source text, `.inf`, and a `multipleOf`
  divisor below the smallest float as written
  (`{"multipleOf":1e-400}`).
- `dq check --rule rule.yml` with a `schema_file:` resolves the schema
  next to `rule.yml`; it used to fail with `io error reading : No such
  file or directory` (exit 5).

Textual edits — `set` and `del` on YAML, JSON and TOML:

- They check that the edited file reads back as the requested change.
  When it does not, they fail with exit 7, print nothing and leave the
  file alone; they used to write the damaged output. The message says
  why — dq cannot write the value there as a textual edit, the edit
  would change other parts of the document, or the edited document would
  not parse — and points to `dq set --jq`, which rewrites the whole file
  through its writer and drops comments.
- `set` where a non-empty collection is replaces it; it used to fail
  with "not found" though `get` found the path. A JSON value, a TOML
  inline table or array and a YAML flow collection are replaced between
  their brackets; a YAML block collection loses its lines, the key
  keeping its comment and anchor (`a: &x 5 # c`). Comment lines inside
  the collection go with it, as do those after it that are indented as
  deep as its entries; they used to stay behind under the new value. A
  core tag goes with the collection, so `a: !!map` + `  b: 1`,
  `set /a 5` gives `a: 5`; it used to fail with exit 7. The same holds
  for `!<tag:yaml.org,2002:map>` and a `%TAG` handle for the core
  schema, which used to stay on the new scalar and leave a file PyYAML
  rejects. An
  anchored node
  replaced this way stays anchored, so its aliases change with it: with
  `a: &m {x: 1}` and `b: *m`, `set /a 42` makes `b` read 42 too. A TOML
  `[table]` or dotted table is removed and the key written anew among
  the keys of its table; when no other key or section makes its parent
  table, the parent is written inline (`[a.b]` + `c = 1`, `set /a/b 5`
  gives `a = { b = 5 }`). When another section still makes the parent
  (`[a.b]` next to `[a.d]`), the key goes under a new `[a]` header
  before the parent's first section. A dotted table is written where its
  first line was (`a.b.c = 1`, `set /a/b 5` gives `a.b = 5`).
- `del` of an entry that shares its line with others — any JSON member
  or item, a YAML flow collection, a TOML inline table or array —
  removes the entry and one comma: `{"a": 1, "b": 2}` `del /a` gives
  `{"b": 2}`. It used to remove the whole line (an empty file for
  single-line JSON), or leave `{x: , y: 2}` / `[, 2]` behind; `del` of
  the last member of a multi-line JSON object left a trailing comma. A
  collection inside a YAML flow collection or a JSON value can be
  deleted too; it used to fail with "not found".
- `set` of a new key next to such an entry keeps it on the line
  (`{"a": 1, "c": 3}`, `a: {x: 1, z: 3}`, `a = {x = 1, z = 3}`), or on a
  line of its own at the sibling's column in multi-line JSON. YAML and
  TOML used to write it as a block line inside the braces, which did not
  parse; JSON started a new line indented two columns too far. After an
  empty `[]` / `{}` value of a block entry the new key no longer gets a
  blank line after it.
- `set` of a new key in YAML or JSON goes after the map's last child,
  also when that child is a collection: `set /spec/replicas 3` next to
  `template:` works, and in `a: 1` + `b:` + `  x: 1`, `set /c 3` adds
  `c` after `b`. It used to fail with "not found" when every child was a
  collection, and otherwise put the key after the last scalar child.
- `set` of a new key in a YAML or TOML file with CRLF line breaks ends
  the lines it adds with CRLF, as JSON already did; it used to add LF
  lines to the file.
- `set` with the root path `''` replaces the whole document in YAML,
  JSON and TOML, keeping its leading comment lines; it used to fail
  with exit 2. `# head` + `a: 1` with `set f.yaml '' '{"b":2}'` gives
  `# head` + `b: 2`. A TOML root must be a table (exit 7 otherwise).
  `dq del f.yaml ''` is still exit 2 and now says "cannot delete the
  root ''" instead of "path '' not found".
- `set` creates missing intermediate keys as mappings, as `--help`
  says. On `a: 1`, `dq set f.yaml /b/c 2` failed with "path '/b/c' not
  found", exit 2, and now gives `a: 1` + `b:` + `  c: 2`. JSON gives
  `{"a": 1, "b": {"c":2}}`, and TOML gives `b.c = 2`, or a `[b]` header
  in a file that uses headers. `--no-create` still exits 2. The help
  now says "created as mappings"; it said "map/array nodes", though
  arrays were never created.
- Edits in a YAML file with a byte order mark before an indented root,
  and in YAML or JSON files with lone-CR line breaks, failed with
  exit 7. They now work, and new lines end the way the file's lines do:
  `a: 1\rb: 2\r` with `set /c 3` gives `…c: 3\r`. Editing a YAML file
  with lone-CR line breaks no longer takes quadratic time: on 10k lines
  it took 28 s and now takes about as long as the same file with `\n`.
- A TOML table that `set` creates gets a blank line before it also when
  the next header has a comment line above it. A comment on a YAML
  `---` line is kept when the document's tag is dropped:
  `--- !!map # c` with `set '' [1]` gives `--- # c` + `- 1`.

YAML:

- `set` of a string a YAML reader would take as something else writes
  it quoted. That covers what dq itself types — `.nan`, `.inf`, `0x10`,
  `0o17`, `~`, `Null`, `+1` — and what YAML 1.1 readers such as go-yaml
  (Kubernetes, GitHub Actions) and PyYAML type: `1_000`, `017`, `1:20`,
  `2001-12-14`, `y`, `n`, `<<`, `=`. It used to write them plain, so
  they read back as a number, a date, a bool or `null`. The same goes
  for a new key such as `null` or `<<`.
- `set` of an object or array where a scalar was writes it in flow style
  (`a: {"x": 1}`); it used to write `null`. A new key's value holding a
  collection gets one in flow style too (`b:` + `  - {"x": 1}`), not
  `{}` / `[]`. Inside a flow mapping the new entry stays on its line
  (`a: {x: 1, z: [1]}`); one holding a collection used to break the
  file.
- `set` on an empty value — `key:`, `key: # c`, `key: &a`,
  `key: !!str`, an empty `-` item — writes the value after the `:` / `-`
  (`key: 5`); it used to write it before the `:` (`key5:`). On a `|` /
  `>` block scalar it replaces the scalar, header included, with an
  inline value: `k: |` + `  x` set to `42` gives `k: 42`; it used to
  glue the value to the next line (`k: |` + `  42r: 1`). A string that
  needs several lines is written double-quoted. On an explicit key with
  no value (`? v` alone) it writes `: 42` on a line of its own; it used
  to append it to the key. Anchors and comments on the line stay; a `!!`
  tag (`!!binary |`, `!!int 5`) goes, so the new value is typed by what
  it is — it used to stay, and `!!binary hi` no longer parses. Local
  tags (`!Ref`) stay.
- `set` of a new key after a block scalar (`|`, `>`) or a value on the
  next line puts the key at the mapping's indent instead of inside the
  value. When the file ends inside that block scalar without a final
  line break, its header turns to strip chomping (`|` → `|-`, `|+` →
  `|-`), so the value stays as it was.
- `set` / `del` after an alias (`*name`) edit the node you point at; they
  used to edit the node after it, or fail with "not found". Lint
  diagnostics on such files point at the right line instead of line 1.
  An edit under an anchor also changes its aliases (`<<: *base`), as
  YAML defines. An edit below an alias (`b: *m`, `set /b/x 1`) fails
  with exit 7 and names the anchored node to edit instead; it used to
  fail with "not found" though `get` found the path.
- `del` of a key whose value is a collection, a block scalar or a value
  on the next line removes the whole entry; for a block collection it
  used to fail with "not found", for `{}` / `[]` it left the key with a
  `null` value. `del` of an empty value — `key: |` with no lines,
  `key: !!str`, `key: &a`, `- &a` — removes its own entry; it used to
  remove the entry after it and keep its own.
- `del` of the last key or item writes `a: {}` / `a: []` instead of
  leaving `a:`, which reads as `null`; an emptied document becomes `{}`.
- `del` of the first key of a compact sequence item (`- name: web` over
  `  image: x`) hands the `-` on to the next key: `- image: x`. It used
  to remove the line with the `-`, merging the item's other keys into
  the item before it. A comment line between the two keys stays, with
  the `-` before it (`- # note`). The only key leaves `- {}`, the only
  item of `- - 1` leaves `- []`. A new key of such an item lines up with
  its keys instead of with the `-`.
- `del` of a node that holds an anchor (`&name`) an alias elsewhere
  refers to fails with exit 7 and leaves the file alone; it used to
  leave the alias dangling, so the file no longer parsed. `lint --fix`
  skips such a fix with a warning. `set` over a collection that holds
  such an anchor fails the same way and names the alias's line.
- Mapping keys that become the same key (`1` and `"1"`, `true` and
  `"true"`, `~` and `""`, a big integer and the same digits quoted)
  are a parse error (exit 3) that names the first key and points at the
  repeated one. One key used to replace the other silently. Equal float
  keys (`-0.0` and `0.0`) are duplicates as before, and two spellings
  of one big number (`1e400`, `1.0e+400`) stay two keys. HCL object
  keys `a` and `"a"` are a parse error too.
- A key that is a sequence or a mapping is named by its compact flow
  YAML (`[1, 2]`, `{k: v}`, `['on', 'a: b']`), without its tags; it
  used to be Rust debug text (`Sequence [Number(1), Number(2)]`). A
  collection key holding `!!float 1e400` reads (`[1e400]`) instead of
  failing. Two keys that get the same name, or that YAML calls equal
  (`{a: 1, b: 2}` and `{b: 2, a: 1}`), are a duplicate (exit 3). `set`
  and `del` next to such a key, or on it (`set '/[1, 2]' x`), edit the
  file in place and keep comments; they failed with "path not found".
  The YAML writer writes the key back as a quoted string, and strings
  starting with `?` or `:` inside such a key are quoted, so the key
  name reads back as YAML. Deletes in a flow mapping with such keys are
  no longer refused, and `del` of such a key inside a flow mapping
  (`{[1]: a, z: 1}`) removes the entry in place.
- A delete in a flow mapping leaves no blank before `,` or `}`:
  `{a: 1 , b: 2}` with `del /b` gives `{a: 1}`. No extra blank follows
  a flow scalar ending in a `!word`: `{a: x !y}` with `set /new 1`
  gives `{a: x !y, new: 1}`, and `z: [a: ]` with `del /z/0/a` gives
  `z: [{}]` (was `z: [{} ]`).
- An implicit-null entry in a flow mapping can be set and deleted:
  `z: {n}` with `set /z/n 1` failed with exit 7 and now gives
  `z: {n: 1}`, and `del /z/n` gives `z: {}`.
- `del` from a mapping without braces inside a flow sequence works:
  `z: [? a]` with `del /z/0/a` failed with exit 7 and now gives
  `z: [{}]`, and `[x, a: 1, y]` with `del /1/a` gives `[x, {}, y]`.
- An empty tagged item of a flow sequence can be set and deleted:
  `[!!str , 1]` with `set /0 2` failed with exit 7 and now gives
  `[2 , 1]`.
- `del` of the only entry of a flow collection keeps the comment next
  to it: `[1 # c\n]` with `del /0` gave `[]` and now gives
  `[ # c\n]`.
- In a flow collection, an empty quoted `!!null` key reads as the null
  key: `{!!null "":1}` reads as `{"": 1}` and `[!!null "":1]` as
  `[{"": 1}]`, where both failed with exit 3, and so does
  `{!!null "" :1}`, with a blank before the `:`. A flow key whose `:`
  is on the next line, with no explicit `?`, is a parse error (exit 3)
  instead of a wrong read.
- An empty node tagged `!!null` reads as null: `{n: !!null }` failed
  with exit 3 (`invalid value: string "", expected null`) and now reads
  as `{"n": null}`; `set /n 1` gives `{n: 1 }`.
- With `%TAG !! tag:example.com,2000:` and `a: !!str 1`,
  `dq set t.yaml /a 2` failed with exit 7 and now gives `a: !!str 2`. A
  plain scalar under a tag outside the core schema reads as untagged
  (`1`), as it does under `!foo`.
- `set` on an empty YAML file, or on one holding only comments or
  `---`, writes the key; it used to fail with "loaded read-only"
  (exit 7) or "path not found" (exit 2). On an empty file
  `dq set f.yaml /a 1` gives `a: 1`, and on `# c` it gives `# c` +
  `a: 1`. An empty document of a stream works too: `a: 1\n---\n` with
  `set /1/a 1` gives `a: 1\n---\na: 1`.
- Replacing a whole YAML document keeps the comment lines between
  `---` and its content: `---\n# lead\nb: 2\n` with
  `set '' '{"x": 1}'` gave `---\nx: 1\n` and now gives
  `---\n# lead\nx: 1\n`. This holds for `set /N`, `fix.ops` and
  `fix.jq` too.
- A `fix.jq` on a YAML file of one document keeps its directives, the
  `---` line and the comment lines around the content:
  `---\n# lead\n\nbad: true\nx: 1\n` gives
  `---\n# lead\n\nx: 1\nok: true\n`; it gave `x: 1\nok: true\n`.

TOML:

- `set` of a table or array writes it inline (`a = { x = 1 }`,
  `b = [{ c = 1 }]`), over a scalar or as a new key; it used to write
  `""` or `{}` / `[]`.
- `set` of a new key next to a dotted one (`r0.k0 = 1`) writes it under
  the same dotted key (`r0.zq = 7`), and after an inline table or array
  when that is the last child. It was refused, or failed with "not
  found" when the only sibling was an inline collection.
- `set` of a new key in a table with no key of its own — the root or a
  `[table]` holding only tables, or nothing — writes it right under the
  header, or for the root before the first table header (and the
  comments right above it) or at the end. It used to fail with "not
  found", and an empty file was reported as read-only.
- `del` of a `[table]`, an `[[array]]` item or a dotted table
  (`a.b = 1`) removes its sections and lines, subtables included; `del`
  of a non-empty inline table or array removes its `key = …` lines, or
  the entry and one comma inside another inline collection. They used
  to fail with "not found". So did `del` of a dotted key inside an
  inline table (`{x."y z".w = 1}`), which now removes its entry. The last
  item of an array of tables leaves `arr = []`, and `del` of `key = {}`
  removes the line instead of leaving `key = `.
- `del` of the only key under a dotted key (`a.b = 1`) writes `a = {}`;
  it used to remove `a` with it. A parent table left empty stays where
  it was: `a.b.c = 1` + `a.b.d = 2` + `e = 1`, `del /a/b` gives
  `a = {}` before `e = 1` instead of after it, and an emptied `[a.b]`
  section leaves an `[a]` header in its place.
- `set` of a new key in a TOML table that has no header of its own
  (`[a.d]` + `x = 1`, `set /a/z 3`) writes `[a]` + `z = 3` before the
  table's first section, or `a.z = 3` after its last dotted line; inside
  an `[[array]]` item it writes `[t.a]` among that item's sections. It
  used to fail with "not found". Inside an inline table, the last key
  under a dotted key that comes first leaves `{x = {}, b = 2}`; it used
  to lose the comma.
- `del` of the last `[[array]]` item in a TOML file with CRLF line
  breaks keeps CRLF: `[[arr]]\r\nx = 1\r\n` with `del /arr/0` gave
  `arr = []\n` and now gives `arr = []\r\n`.
- A TOML result that is not a table says why. `dq set f.toml --jq 5`
  said "toml format error: unsupported i64 type" and now says "toml
  format error: a TOML document is a table, so its root is one, not an
  integer" (exit 1).

JSON:

- A JSON file that starts with a UTF-8 byte order mark is read; it
  failed with exit 3. Edits and `-i` keep the mark, and `convert`
  output has none.
- `dq -F json convert` no longer prints a second blank line:
  `{"a": 1}\n\n` is now `{"a": 1}\n`. `set --jq`, and `fmt --indent`
  or `--sort-keys`, end a JSON file with a newline. A value moved or
  inserted into `{}` inside one-line JSON stays on that line:
  `{"a": {\n  "b": 1\n}}` is now `{"a": {"b": 1}}`.
- A value set into a JSON file that is just `{}` stays on one line
  (`{"x": {"y":1}}`), and `{\n}` no longer gets a blank line before the
  new entry.

YAML streams — files of several documents separated by `---`:

- `dq lint`, `check` and `fix` evaluate each document of a stream on
  its own. With a second Pod using `nginx:latest`, `dq lint
  bundle.yaml` printed `diagnostics: []` and exited 0; it now reports
  `k8s.no-latest-tag` at `bundle.yaml:9:38`, exit 4. A diagnostic with
  no position of its own lands at its document's first line. An empty
  document (`---` and nothing) reports at its own `---` line; it used
  to be reported on the next document's separator or past the end of
  the file.
- `dq fix` on a stream keeps every document. `fix.ops` edits apply in
  place, comments included. A `fix.jq` re-emits only its own
  document's content: its `---` line (with any `!tag` / `&anchor` on
  it, while they still fit the new value), the comment and blank lines
  before the next `---`, and every other document keep their bytes,
  and a rewritten document keeps the file's CRLF line ends. On
  `# head\nname: a # keep\n---\nx: 1 # c\n…`, fixing only the second
  document lost `# head` and `# keep`, and every later `fix.ops` was
  skipped.
- `dq set` and `dq del` work on a whole document of a stream. With
  `printf 'a: 1\n---\nb: 2\n' > s.yaml`, `dq set s.yaml /1 '{"c":3}'`
  failed with exit 7 and now gives `a: 1\n---\nc: 3\n`, and
  `dq del s.yaml /1` failed with exit 2 ("path not found") and now
  gives `a: 1\n`. `dq --doc 1 set s.yaml '' …` works the same way. A
  `fix.ops` `replace ""` or `add ""` now applies, and `remove ""`
  drops a stream document. In a stream the replace used to be skipped
  with exit 0, leaving the finding, and the remove failed with "path
  '/1' not found".
- `dq del /N` also removes the document's `%YAML` / `%TAG` directives.
  `del /1` on `%YAML 1.2\n---\na: 1\n...\n%YAML 1.2\n---\nb: 2` failed
  with exit 7 and now gives `%YAML 1.2\n---\na: 1`.
- `dq set FILE --jq` on a stream writes a stream:
  `dq set bundle.yaml --jq '.[1].metadata.name = "z"'` printed one YAML
  sequence and now prints both documents, separated by `---`. A filter
  over a whole stream must return a non-empty array of documents:
  `-i --jq '[]'` wrote an empty file and is now exit 6, with the file
  untouched.

`dq patch` and `dq merge`:

- `patch` `add` into an array inserts, as RFC 6902 §4.1 says. On
  `[1,2]`, `add /0 9` gives `[9,1,2]` (it gave `[9,2]`), and `/-` or
  `/2` gives `[1,2,9]` (it failed with exit 2). `copy` and `move` into
  an index insert too. Comments are kept in YAML, JSON and TOML,
  arrays of tables included. `set` still replaces.
- `add` into an anchored YAML array that an alias shares applies, and
  the alias sees the new item as it does after `set`: `a: &q [1]` +
  `b: *q` with `add /a/0 9` failed with exit 7 and now gives
  `a: &q [9, 1]`. An insert under the alias (`/b/0`) is refused with
  the same message as `set`.
- `add` before the first item of a compact YAML sequence inserts and
  keeps comments: `- - 1 # c` with `add /0/0 9` failed with exit 7 and
  now gives `- - 9\n  - 1 # c`.
- `patch` refuses a `move` into the value's own child (RFC 6902 §4.4)
  with exit 2, naming both paths. Patch ops never create missing
  parents, also after a move's removal.
- `patch` addresses the documents of a YAML stream as `/N`, like `set`
  and `del`. On `a: 1\n---\nb: 2\n`, `remove /1` gives `a: 1\n` (it
  failed with exit 2) and `replace /1 {"x":1}` gives
  `a: 1\n---\nx: 1\n` (it failed with exit 7).
- `add /N` on a stream inserts a new document with its `---` line, and
  `add /-` appends one; `add /1` used to replace document 1. Added
  before a document with `%YAML` / `%TAG` directives, the new document
  goes before them and ends with `...`, so the directives stay with
  their document.
- `merge` with a mapping patch on a root that is not a mapping replaces
  the root with the patch minus its null members (RFC 7396 §2):
  `- 1\n- 2\n` with `{"a":1,"b":null}` gives `a: 1\n`; it failed with
  exit 2. Nested nulls under a key whose old value was not a mapping
  are dropped; they used to be written.
- `merge` on a YAML stream is refused with exit 6. It used to fail with
  "not found" or merge into the wrong place.

XML:

- A `#text` that is a number or a boolean is written instead of lost:
  `"#text": 1.5e20` gave `<x></x>` and now gives `<x>1.5e20</x>`, and
  `true` gives `<t>true</t>`. An array or a map under `#text`, or an
  `@attrs` that is not a map, was dropped silently and is now refused
  with exit 1. `"@attrs": null` writes no attributes; it used to fail.
- Non-string `#cdata`, `#comments` and `#pi` items and `#xml`
  declaration fields are no longer dropped silently; a YAML
  `version: 1.1` was written as `version="1.0"`. Scalars are written as
  text. Containers, unknown `#xml` fields, a second root element and
  top-level text are refused with exit 1.
- Every writer refusal names the key and what it got: `(got a map)`,
  `(got a number 5)`, `(got "hi")`. A child entry that is not a list
  (`r: [{c: 5}]`) is refused instead of dropped silently.
- Output is always well-formed. Names, PI targets, comments holding
  `--`, PIs holding `?>`, control characters and declaration fields
  that XML does not allow are refused with exit 1 and a message naming
  the key; they used to be written. `standalone: true` is written as
  `yes`. CDATA holding `]]>` is split across sections. Comments are no
  longer escaped, so `<!-- a < b -->` round-trips unchanged; it came
  back with `&lt;`.
- Element and attribute names must be namespace QNames, and PI targets
  may not contain a colon: `x:y:z`, `:a` and `a:` fail with exit 1
  instead of being written.
- The writer refuses a name whose prefix no `xmlns:` declares in scope,
  `xmlns` as an element name, and the bindings XML Namespaces forbids
  (`xmlns:xmlns`, `xmlns:xml` bound to another URI). `xmlns:p=""`
  undeclares a prefix in a `version="1.1"` document, as Namespaces in
  XML 1.1 allow; in XML 1.0 it is refused, with exit 3 on read and
  exit 1 on write.
- Whitespace survives XML's normalization. A CR in text, and a tab, LF
  or CR in an attribute value, are written as `&#13;`, `&#9;` and
  `&#10;`; they used to read back as an LF or a space. Text that is
  only whitespace (`&#9;`, `&#10;`, `&#x85;`) is written as character
  references; it read back as an empty element. Text made only of
  Unicode spaces that are not XML whitespace (U+3000, U+2028, NEL) is
  kept instead of dropped, and the writer references CR, NEL and LS so
  they read back unchanged.
- Line ends read as an LF in PIs and CDATA too (CRLF, CR). In a
  `version="1.1"` document NEL, LS and CR NEL also read as an LF, or a
  space in attribute values. The writer refuses a CR in a comment, PI
  or CDATA, and NEL or LS there in a 1.1 document, with exit 1, since
  they would read back as an LF.
- In an XML 1.1 document the writer writes restricted control
  characters as character references in text and attributes, and
  refuses them in comments, PIs and CDATA. The reader refuses
  characters the document's version does not allow, raw or as a
  character reference, with exit 3 and the line and column: NUL,
  surrogates, U+FFFE and U+FFFF; C0 controls other than tab, LF and CR
  in 1.0; raw restricted characters in 1.1.
- dq reads files in ISO-8859-1, windows-1252, Shift_JIS, UTF-16 (LE or
  BE, with a BOM) and other encodings their declaration names; they
  failed with "invalid UTF-8" (exit 3). Such a file writes back byte
  for byte. `US-ASCII` is strict 7-bit, and `ISO-8859-1` is true
  Latin-1 (IANA), not windows-1252. A byte order mark shows as
  `#xml.bom`. A UTF-8 one, which used to be dropped, is kept as
  `#xml.bom: UTF-8` and written back by `convert -F xml` and `fmt -i`,
  so such files round-trip byte for byte.
- UTF-16 without a BOM keeps its byte order on write
  (`#xml.detected`), and a declaration that contradicts it is a parse
  error (exit 3). A file in 8-bit bytes that declares
  `encoding="utf-16"`, as .NET `XmlSerializer` over a `StringWriter`
  writes it, reads as UTF-8, is kept as `#xml.detected: UTF-8` and is
  written back as UTF-8 under its label, byte for byte. The writer
  refuses `detected: UTF-8` under any other label.
- A file whose byte order mark contradicts its declared encoding (a
  UTF-16 or UTF-8 BOM declaring `ISO-8859-1`), or whose `encoding` is
  not an encoding name (`" ISO-8859-1 "`), is a parse error (exit 3),
  as XML 1.0 requires. It used to read fine and then fail to write
  back, or be written in the wrong encoding.
- `convert -F xml` encodes its output in the encoding `#xml` declares.
  It refuses with exit 1 a character that encoding cannot hold (the
  message names the character and where it is), an encoding dq does
  not know, and a `bom` the declared encoding contradicts.
- Parse errors name the line and column of the name at fault, the
  attribute or the tag, in every command and not only in `validate`.
  A refused `encoding` label names its line and column too, in
  messages and in `dq validate`.
- The XML reader refuses ill-formed input with exit 3 at line and
  column. That covers:
  - an element left open at the end of input (`<r/><c>` used to read,
    with `<c>` dropped);
  - a second root;
  - text, CDATA or a reference outside the root element;
  - a DOCTYPE after the root, inside it, or given twice;
  - `]]>` in text;
  - a raw `<` in an attribute value;
  - element, attribute or PI-target names that are not XML names
    (`<1r/>`, `<a 1b="x"/>`, `<?1p x?>`).
- **`@std` rules no longer fail silently.** A rule that raised a jq
  runtime error was logged and skipped, so `dq lint` reported nothing
  and exited 0; on `kind: Pod` with `spec: x`, 11 `@std/k8s` rules did
  this.
  - Every `@std` rule now tolerates a field of the wrong type and skips
    what it cannot read. This covers `k8s`, `dockerfile`, `terraform`,
    `markdown` and `skills`.
  - A test substitutes a string, a number, null, an array and a map at
    every pointer of every fixture input, and asserts that no rule
    raises.
  - `dq test` fails a case whose rule raises, even when no violation is
    expected, and names it: `raised: rule=k8s.run-as-root check.jq: …`.
  - `dq lint` keeps the warning.
- `dq test @std/k8s` runs the fixtures built into the binary, and
  `dq test @std` runs every namespace. Both used to exit 6.
- `github-actions.no-pull-request-target-with-checkout` catches every
  spelling of the PR head in a checkout's `ref:` and `repository:`. It
  missed:
  - `github.head_ref`;
  - `github.event.pull_request.head.ref`;
  - `head.repo.full_name`;
  - `refs/pull/<n>/merge|head`, whether written out, built from
    `github.event.number`, or built with `format()`;
  - bracket access.

  It no longer fires on `github.event.pull_request.base.*`.
- Dockerfile diagnostics point at the instruction they are about,
  instead of line 1, column 1. That is the line it starts on (not a
  comment above it, nor a continuation or heredoc line below it), at the
  keyword, or at the first argument for a pointer into `arguments`.
  `@std/dockerfile` messages no longer repeat the line. `has-healthcheck`
  is a whole-file finding and still reports 1:1.
- Markdown positions count from the start of the file. Before, lint
  findings in a file with frontmatter were off by the frontmatter's
  length.
  - `markdown.code-blocks-yaml-valid` reports at the YAML error inside
    the block, not at 1:1.
  - Columns are right on fence lines that are indented less than the
    fence.
  - Frontmatter after a UTF-8 BOM is read.
  - A composite rule's parse failure lands on the parse error.
- JSON Schema:
  - A relative `$ref` to an embedded `$id`, and an absolute `$ref` to
    the schema's own `$id`, resolve. They used to fail with
    `external $ref is not supported`. Nothing is fetched.
  - A limit reached through a `$ref` outside the schema keywords
    (`#/x-limits/a`) or through a YAML alias (`*lim`) is quoted as
    written (`1E2`), not as `1e+2` or `100.0`.
- jq path assignment through a missing or null key creates the path, as
  jq does. This covers `=`, `|=`, `+=`…, `//=` and `setpath`, in
  `set --jq`, `query` and `fix.jq`. It used to fail with "cannot use
  null as iterable".
  - `del` of a missing path is a no-op, and `del(.[0], .[1])` deletes
    both original items.
  - An integral float index (`.[2.0]`, `.[1e3]`, `setpath([2.0]; 1)`) is
    taken as the integer.
  - An index past 536870911 is the jq error "Array index too large"
    (exit 1). It used to panic (exit 101), or try to allocate gigabytes.
  - Building an array by index in a loop takes linear time, not
    quadratic.
  - An object takes only a string key, as in jq. `.[1]`, `.[1] = 1`,
    `setpath`, `getpath`, `del`, `has`, `{(1): 2}` and `from_entries`
    with a number key are runtime errors (exit 1). Before, they read
    `null` or wrote the key `"1"`, and `.[1e300] = 1` on an object wrote
    the key `9223372036854775807`. An error quotes the index as it was
    written.
  - Reads and `has()` truncate a number index toward zero, and a NaN or
    out-of-range index reads `null`, as in jq 1.8.2. `null | .[true]` is
    an error.
- Structural `set`, `del`, `patch`, `merge` and `fix.ops` work on XML,
  HCL, INI, dotenv and CSV/TSV. They exited 7 ("cannot set a value in
  it"), although the README lists these formats as writable.
  - They rewrite the file through the format's writer, so formatting
    and comments are lost; XML keeps its comments.
  - A value the format cannot hold exits 1 and leaves the file
    untouched.
  - `--no-create` on a missing path is exit 2, not 7.
- Rewrites through a format's writer, by point edits or by jq (`set
  --jq`, `fix.jq`), keep the data:
  - HCL writes expressions, templates, heredocs and expression keys back
    as their source. `ami = var.ami` used to become
    `ami = "$${var.ami}"`, which Terraform reads as a literal string. A
    string the user sets stays a literal, even `'${var.a}'` set over
    `var.a`. An edit that would have to change an expression, or a
    string that two expressions read as, is refused with exit 1.
  - HCL keeps every repeated block with the same type and labels, as a
    list of bodies in source order. They used to silently merge into
    the last one.
    - Blocks are written back as blocks, also with one body left.
    - An attribute and a block of one name, or blocks of one type with
      different label counts, are exit 3.
    - `@std/terraform` checks every repeated block.
  - dotenv keeps `export`.
  - INI keeps an emptied section; only deleting the section removes it.
  - On a point edit, the CSV/TSV header keeps the file's columns in
    their order, followed by any new keys in first-seen order. The
    header is also kept when no rows are left, and a deleted cell is
    left empty. A write that replaces the whole file (`set ''`, a
    `patch` at `""`, `merge` of an array) and a rewrite through jq
    (`set --jq`, `fix.jq`) build the header from the rows' keys.
  - XML keeps top-level comments and processing instructions on their
    side of the root when a point edit deletes or inserts one. An `#xml`
    set below the root is refused (exit 7); before, it was silently
    dropped.
  - An edit that changes no data leaves the file alone: `--check` exits
    0, a bulk run counts the file as Skipped, and comments survive.
  - The file must read back as the edited data, or the edit is refused
    with exit 7. Before, a JSON Lines root set to an object dropped
    every record with exit 0.
  - On these formats, `patch`, `merge` and `fix.ops` apply every op to
    the tree and write once, so ops on repeated HCL blocks follow RFC
    6902. A failed op leaves the file untouched.
- The template guard runs on every command that reads a file: read
  commands, `convert`, `validate`, `diff`, `lint`, `check`, `fmt`,
  `fix` and `set --jq`.
  - A Helm `{{ .x }}` used to read silently as a YAML flow mapping. A
    file whose templates change what it reads as is now refused (exit
    3).
  - `--allow-templates` and `--raw-template-strings` are honoured.
  - The guard also catches `{{- if … }}` and `{{ $v }}`.
  - GitHub Actions files pass, escaped quotes inside `${{ }}`
    included, and so do Markdown and JSON that mention `{{ }}` in text.
  - A refused file names the template that changes the reading. It
    used to name the first template in the file.
  - `--raw-template-strings` keeps templates as text in Markdown and in
    bare HCL values. In Markdown the placeholder used to read as bold
    text. Text in the file that looks like a placeholder is no longer
    turned into a template; a file that spells every placeholder dq
    tries is refused (exit 3). It is much faster on files with many
    templates.
- `--input-format frontmatter` reads a header after a UTF-8 BOM and
  writes the BOM back. A header keeps its delimiters whatever it holds:
  an empty YAML header is written back empty (`---\n---`), not as
  `null`, and an empty TOML header or a `{}` header no longer loses its
  delimiters on `fmt`. `set --jq` and `fix.jq` work on frontmatter
  input; they failed with "no payload".
- Globs:
  - A glob with no directory part (`2-*.yaml`) matches files in the
    current directory. It used to match nothing.
  - `*` and `?` no longer match `/`; only `**` crosses directories.
  - An existing path containing `[` is no longer read as a glob.
- Bulk `-i` leaves alone a file whose bytes would not change. Such a
  file keeps its mtime, gets no `.bak`, and counts as Skipped instead of
  Modified.
- `.pom` and `.xsd` read as XML, and `.env.*` files (`.env.local`,
  `.env.production`, …) as dotenv. A known extension still wins:
  `.env.json` is JSON.
- `dq fix -`, which failed on every input with "EOF while parsing", reads
  stdin once. `dq lint -`, `check -` and `fix -` work with
  `--input-format`. `-i` and `--check` on stdin are refused, instead of
  writing a file named `-`.
- SARIF results carry `ruleId` and `ruleIndex`, and `tool.driver.rules`
  describes each rule.
- `diff --unified` no longer colours piped output; it follows
  `--no-color`, `NO_COLOR`, `CLICOLOR_FORCE` and TTY detection. It no
  longer ends every diff with a false `\ No newline at end of file`.
- Escaping of control characters in output:
  - Console output escapes C1 controls (U+0080–U+009F, e.g. U+009B
    CSI) as `\xNN`, like C0 controls.
  - It escapes the bidi controls U+200E, U+200F, U+202A–U+202E and
    U+2066–U+2069, and U+2028/U+2029, as `\uNNNN`.
  - Error messages on stderr are escaped the same way.
  - A line break inside user data quoted in an error (a pointer, a
    key, a path) prints as `\x0a`, so input cannot forge an error line.
  - File names, keys, rule ids and test case names are escaped wherever
    dq prints them: bulk summaries, `--diff` headers, log lines, writer
    errors, and `test` console and TAP output. A newline or ESC in a
    file name can no longer break or forge a line.
- `convert -F json` of a JSON file that ends with blank lines prints
  exactly one final line break. `set --jq` and `fmt --indent` end JSON
  with one line break too. A `patch` `move` lays the moved value out as
  `add` does.
- `dq fmt --diff` compares with the file. On YAML, INI, HCL, dotenv and
  XML it compared with empty bytes.
- The README and the skill were checked against the binary, and these
  are fixed:
  - `-F console`, not `text`;
  - `check --rule`;
  - `query EXPR FILE`;
  - rule examples carry their required `description`;
  - every value a `check.jq` emits is one violation;
  - `fix.ops` is documented;
  - lint reports go to stdout;
  - `validate` takes one file.

### Security

- `dq self update` verifies the downloaded archive's SHA-256 against the
  release's `dq-checksums.txt` before extracting it, as the README said
  it did. A mismatch or a missing or malformed checksum file is refused
  with exit 4, and a checksum file that cannot be downloaded is exit 5.
  Before, nothing was verified.

- `cargo audit` reports no advisories; it reported 37 vulnerabilities
  and 3 unsound warnings before these updates.
  - wasmtime 44 → 50 in the plugin runtime: RUSTSEC-2026-0222, -0269,
    -0316, -0327, -0359, -0362, -0363, -0379, -0424.
  - self_update 0.44 → 1.3 for `dq self update` drops zip 6
    (RUSTSEC-2026-0367, -0368, -0369, -0429, -0545, -0547, -0548) and
    quick-xml 0.38.
  - quick-xml 0.40 → 0.42 in the XML parser: RUSTSEC-2026-0194, -0195.
  - rustls, rustls-webpki, webpki-roots and webpki-root-certs, used by
    `dq self check` and `dq self update`: RUSTSEC-2026-0285, -0346,
    -0348, -0349, -0381, -0382, -0383.
  - The remaining advisories (crossbeam-epoch, h2, quinn-proto,
    quinn-udp, and the unsound warnings for anyhow, rustix and tokio)
    were closed by updating `Cargo.lock`.

## [2026.20.2] - 2026-11-18

### Added

- M11 Phase 5 — extended rulesets `@std/terraform` and `@std/openapi`
  land, raising the standard rule library to 8 namespaces (`k8s`,
  `dockerfile`, `npm`, `github-actions`, `markdown`, `jsonschema`,
  `terraform`, `openapi`) and 64 embedded rules. Both namespaces ship
  in every build (no feature gates).
  - `@std/terraform` ships 8 rules covering common security, hygiene,
    and reproducibility footguns: `no-hardcoded-secrets` (literal
    `password = "..."` / `api_key = "..."` / `token = "..."`
    assignments — variable and data-source references that the HCL
    parser surfaces as `${...}` template strings are silently
    allowed), `tag-required` (every `resource "aws_*"` block has a
    top-level `tags` map), `provider-pinned` (every `provider` block
    has `version = "..."`), `no-public-ingress` (`cidr_blocks` /
    `source_ranges` containing `0.0.0.0/0` on AWS security groups
    and GCP firewalls), `state-backend-required` (every `terraform`
    block has a nested `backend "..." { ... }`), `module-pin-version`
    (remote `module { source = "..." }` blocks have `version`;
    local-path sources `./` and `../` are exempt),
    `output-no-sensitive-without-flag` (output names matching
    `secret|password|token|key|credential` must set
    `sensitive = true`), and `variable-has-description` (every
    `variable` block has a non-empty `description`). Fixtures cover
    35 cases across the 8 rules.
  - `@std/openapi` ships 6 rules — implemented entirely through plain
    jq + JSON Schema over the YAML/JSON parser, with no `oas3`
    dependency (see design D6 implementation note for the rationale):
    `info-required-fields` (uses `check.schema_file:
    ./openapi-info.schema.json` to require non-empty
    `info.title`/`info.version`), `paths-non-empty` (the document
    declares at least one path), `operation-id-unique` (group every
    `operationId` and emit one diagnostic per duplicated id with a
    count), `response-200-or-201-required` (every operation under
    `get|post|put|delete|patch|options|head|trace` declares at least
    one 2xx success response — any specific code from `200`–`299`
    such as `200`/`201`/`202`/`204` or the wildcard `2XX`), `no-trailing-slash`
    (path keys other than the root `/` must not end with `/`), and
    `security-defined` (top-level `security:` is non-empty OR every
    operation has its own `security:` — operations can opt out
    explicitly with `security: []`). Each rule's `match.filter`
    pins on `.openapi != null and (.openapi | type == "string")` so
    non-OpenAPI YAML/JSON is silently skipped. Fixtures cover 28
    cases across the 6 rules.
  - HCL backend has **no span information** (see
    `crates/dq-core/src/parsers/hcl.rs` doc-comment) — every Terraform
    diagnostic resolves to `(line=1, col=1)`. The fixture runner
    exercises rule logic; precise sub-line coordinates land when the
    HCL parser grows a span-aware path. OpenAPI diagnostics, by
    contrast, carry full YAML/JSON spans through the existing
    `loc.pointer` lookup.
  - New `dq_lint::std_schema_files(namespace)` accessor enumerates
    every embedded JSON Schema sidecar for a namespace as
    `(filename, contents)` pairs. The integration test runner uses
    it to stage `helm-values-template.schema.json` and
    `openapi-info.schema.json` next to the rule YAML so
    `check.schema_file:` resolution works end-to-end against
    tempdir-staged copies of the embedded namespaces.

- M11 Phase 3 — `Rule.check` becomes a four-variant enum and JSON Schema
  2020-12 lands as a first-class rule type. The `dq-exec::rule::Check`
  enum now covers `Jq` (legacy), `Schema` (inline JSON Schema 2020-12),
  `SchemaFile` (sibling-relative schema document), and `Composite`
  (parser-side stub for Phase 4 cross-format checks). Existing
  `check: { jq, message }` rules continue to parse unchanged through the
  `RuleCheck` alias. Schema validators are compiled once at
  `Evaluator::new` (via the new `dq-exec::schema_check` module) and
  reused across every `evaluate_file` call. Each validation error
  produces one `Diagnostic` with `pointer` recovered from
  `error.instance_path` (RFC 6901), the `keyword_location` embedded in
  the message, and `(line, col)` looked up through
  `Ir::line_col_for(&pointer)` (or `(1, 1)` fallback when no span is
  available). The `schema_file:` resolver canonicalises both the rule
  directory and the schema path and rejects absolute paths
  (`ExecError::SchemaFileAbsolutePath`) and `..`-escapes
  (`ExecError::SchemaFileEscapesRuleDir`). External `$ref` targets
  (`http://`, `https://`, `file://`, or any scheme other than internal
  fragment refs) are rejected at compile time
  (`ExecError::SchemaCompile`) — the validator runs with the default
  `referencing::DefaultRetriever`, which performs no network or
  filesystem reads. New `ExecError` variants:
  `CheckMutuallyExclusive`, `CheckMissing`, `CompositeIncomplete`,
  `CompositeExtractNotArray`, `CompositeExtractMalformed`,
  `CompositeExtractUnknownFormat`, `CompositeDepthExceeded`,
  `SchemaCompile`, `SchemaFileAbsolutePath`,
  `SchemaFileEscapesRuleDir` — each with stable `kind_name()` strings
  and CLI exit-code mappings (`composite_extract_*` /
  `composite_depth_exceeded` → `GENERIC` (1); the rest →
  `PARSE_ERROR` (3)). New `@std/jsonschema` ruleset shipping three
  reference rules: `jsonschema.kubernetes-crd-shape` (inline schema
  validating `apiVersion` / `kind` / `metadata.name`),
  `jsonschema.helm-values-against-schema` (template using
  `schema_file: ./helm-values-template.schema.json`), and
  `jsonschema.openapi-3.1-shape` (inline schema for OpenAPI 3.1 root
  structure). Each ships with a co-located `*.test.yml` fixture; new
  `dq_lint::std_schema(namespace, file)` accessor exposes embedded
  schema sidecars. Workspace dependency:
  `jsonschema = { version = "0.34", default-features = false }` —
  the `resolve-http` and `resolve-file` features stay off so no HTTP
  or FS access leaks into the validator.

- M11 Phase 1 — XML 1.0 read+write format. New `XmlFormat` (registered for
  `.xml` extension; selectable via `-F xml`) backed by `quick-xml 0.36`
  parses and serialises XML through a conventional-key mapping onto the
  existing `Value` enum: `<tag>` becomes `Map { tag => Array<Map { ... }> }`
  on the parent (multi-element same-tag children stored in a single Array
  to preserve document order — even single occurrences are wrapped in a
  one-element array so `Pointer` indexing is stable). Element attributes
  live under `@attrs`; element text body under `#text`; comments under
  `#comments`; CDATA blocks under `#cdata`; processing instructions under
  `#pi`; the `<?xml ...?>` declaration under top-level `#xml`. Namespace
  prefixes (`xmlns:foo`, `foo:tag`) are retained verbatim. Round-trip is
  **partial**: structure, attributes, comments, CDATA, PIs, namespace
  prefixes, and the XML declaration are preserved, but mixed content
  (text interleaved with elements in the same parent — e.g.
  `<p>Hello <b>world</b>!</p>`) is folded into `#text` and inner element
  positions are lost. Mixed-content detection emits a `tracing::warn!`
  line so users are aware. New `FormatTag::Xml` variant; `OutputFormat::Xml`
  is wired into `dq convert -F xml`. XML query verbs (`get`, `paths`,
  `keys`, …) work via the conventional-key tree like any other map-shaped
  format. Textual-edit `set` / `del` operations are not supported on XML
  documents — writes go through `Format::write` whole-document re-emission.
- `loc.pointer` rule field (jq expression returning a JSON Pointer string).
  When set, the lint evaluator looks up the pointer in the input document's
  provenance map and resolves the diagnostic's `(line, col)` from the
  parser-recorded byte span — no more `loc.line` int-coercion to recover
  positions for span-aware formats (YAML, JSON, TOML).
- `dq_core::Ir::line_col_for(&Pointer) -> Option<(u32, u32)>` for callers
  that need the `(line, col)` of a pointer's value through the IR.
- `dq_transform::ir_to_val` / `dq_transform::val_to_owned_ir` IR-aware
  variants of the value adapter (provenance is dropped on the way into jaq
  and reconstructed as `Provenance::Synthetic { Computed }` on the way out
  — see `data-query-transform` spec for the contract).
- `dq_core::Value::to_serde_json` and `dq_core::Value::from_serde_json`
  promoted from private duplicates in `dq-cli` and `dq-core::transform` to
  a single public API.
- `fix.ops` rule field (jq expression returning a JSON Patch array). When
  set, the autofix engine applies the patch via `EditScript::apply`
  against the parsed document, preserving comments and surrounding bytes.
  `add`, `replace`, and `remove` ops are supported (RFC 6902 subset;
  `move` / `copy` / `test` rejected).
- `dq_core::{EditOp, EditScript}` re-exported at the crate root — the
  per-rule edit vocabulary used by `Fixer` and (later) WASM plugins.
- `@std/npm/has-license` migrated to `fix.ops` as the reference rule with
  comment-preserving autofix. The migration covers the empty-string-value
  case (`/license: ""`); the missing-key case is deferred until mkdir-p
  insertion lands in `Document::set_at`.
- Plugin ABI v0.1.0 (experimental). New `dq-plugin` crate exposes
  `PluginRuntime` over WIT + Component-Model wasmtime. The WIT package is
  `dq:plugin@0.1.0` with host-imported `ir` (read-only document access)
  and `jq` (compile/eval against the document) interfaces, plus a
  `world plugin` that exports `lint() -> list<diagnostic>` and
  `fix() -> result<list<u8>, string>`. Plugins run sandboxed: no WASI,
  no filesystem / network / process control, ~1s of CPU per invocation
  (fuel budget), 64 MiB linear-memory cap. New global `--plugins <DIR>`
  flag on `dq lint` / `dq fix` discovers `*.wasm` files non-recursively
  under `<DIR>` (lexical sort) and loads them through the runtime.
  Feature-gated behind `--features plugins`; without the feature the
  flag still parses, but encountering any `*.wasm` errors with exit `6`
  (`InvalidInput`). Breaking changes to the WIT schema and marshalling
  shapes are possible before `v1.0.0`. See `examples/plugin-rust/` for a
  working Rust reference plugin and the full build recipe.

### Changed

- `dq_exec::Evaluator::evaluate_file` now takes `&dq_core::Ir<'_>` instead
  of `&serde_json::Value`. Internally the evaluator still feeds `serde`
  values into jaq; the `Ir` is required so the new `loc.pointer` chain has
  access to the provenance map.
- `dq lint` (and `dq check`) now route YAML and JSON inputs through the
  span-aware parsers, so `loc.pointer`-using rules emit accurate
  `(line, col)` instead of falling through to `(1, 1)`.
- `@std/k8s/image-pull-policy-always` migrated to `loc.pointer`; its
  `check.jq` now emits a pointer per violation that anchors at the
  offending container's `name:` scalar.
- `dq_exec::Fixer::apply` now takes `&mut Document` instead of
  `&serde_json::Value`. The CLI's `dq fix` handler routes the post-fix
  document's bytes directly to disk when only `fix.ops` rules ran,
  preserving comments byte-for-byte. Legacy `fix.jq` rules continue to
  re-emit through the format writer (same comment-loss trade-off as
  `dq set --jq`).
- `dq_exec::FixOutcome` lost its `new_value: serde_json::Value` field;
  the document itself is now the source of truth. A new
  `legacy_jq_applied: bool` field tells the CLI which output path to
  take.
- `RuleFix` schema now accepts both `jq` (legacy) and `ops` (new) fields,
  each `Option<String>`. At least one must be set; both is allowed and
  `ops` wins at runtime (with a `tracing::warn!` shadowing log).

### Deprecated

- `loc.line` jq override. Backwards-compatible fallback when `loc.pointer`
  is unset or fails to resolve to a span. Removal is deferred to a future
  change once the `@std/*` rule library has fully migrated.
- `fix.jq` whole-document jq fixes. Backwards-compatible; new rules
  should prefer `fix.ops` for comment preservation. Removal is deferred
  to a future change once the `@std/*` rule library has fully migrated.
