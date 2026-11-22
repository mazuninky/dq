# Roadmap: what's next

Open work, in one place: the active plans, the bug queue of the current
branch, gaps found while writing the reference docs, what is left of the
original roadmap, items earlier changes deferred, and the things the project
has decided not to do. Current behaviour is in [reference/](reference/); why
things are the way they are is in [decisions.md](decisions.md).

Status words: **done** (in the code, only bookkeeping left), **partly done**,
**open** (not started).

## Active plans

Each has its own page with the design and the remaining steps.

- **[`@std/skills`: rules for `SKILL.md` and `evals.json`](plans/add-std-skills-rules.md)
  — partly done.** `skills.frontmatter` and `skills.evals-schema` ship with
  fixtures, and the changelog announces the namespace. `skills.evals-schema`
  follows skill-creator's contract: an eval's `name` / `eval_name` label,
  and `assertions` or `expectations`. This repository's own `evals.json`
  passes. Left: check `skills.frontmatter` against the upstream skill
  contract; remove the downstream copies of these rules in another
  repository; fixtures cannot carry a `path`, so `match.glob` cannot be
  tested.

## Open bugs

Found while testing parse-error positions (2026-11-22), not part of that
change:

- JSON Lines refuses a blank line behind a byte order mark
  (`\u{feff}\n{"a":1}\n` is `EOF while parsing a value`).
- INI and `.env` do not strip a byte order mark: it becomes part of the
  first section name or key, and `\u{feff}# c` in `.env` is a parse error.
- A composite rule's `parse-failed` diagnostic places the inner text only
  where it stands in the file as it is: a YAML block scalar loses the indent
  in the column, a block-quoted fence the quote mark, and a quoted string's
  escapes shift the lines.

Found while making `lint` fail closed (2026-11-22,
[plans/lint-fail-closed.md](plans/lint-fail-closed.md)). In each of the first
group dq can still check nothing and exit 0:

- Input globs drop a directory that cannot be read, a file that is a symbolic
  link, and a name that is not UTF-8.
- A nested rule whose `match.format` excludes the extracted items; `fix --check`
  with a fix on a schema check, a fix that is not idempotent, or an edit that
  was refused (a warning each).
- `--plugins DIR` without a plugin, or whose plugin is a link or `*.WASM`.
- `dq test`: a case whose `format` is not in the rule's `match.format`, a
  fixture with `tests: []`, a fixture name that is not UTF-8.
- `match.glob` against a path typed as `./x`.
- `dq rules add` of something that is no rule, or a directory without rules.

And:

- `schema_file` is resolved against the directory given to `--rules` (or
  `.dq/rules`), not the rule file's own; `dq rules add` does not copy it.
- `rules list` and `explain` leave out all of `.dq/rules` when one rule in it
  does not load.
- TAP and JUnit carry a raw ESC or line break of a file name or rule id in
  `file:` and in attributes.

## Known limits, not dq bugs

- `{n: !!str}` with no space before `}` is rejected by every YAML reader
  (saphyr, libyaml, PyYAML).
- CR-only line ends are invalid TOML per the TOML specification.

## Found while writing these docs

The reference pass turned up behaviour that disagreed with the original
design or the README. All of it was fixed on `fix/after-deps-update`; see
[decisions.md](decisions.md#2026-11-20--fix-everything-found-after-the-dependency-update-after-deps-update-fixes).
What is left is a feature, not a bug, plus a few known limits of that work:

- **Structured errors**: no caret under the offending byte, and no JSON
  error object on stderr under `-F json` (only `validate` and `lint` emit
  structured output for a file that does not parse). Agent-first error
  output was a founding goal. Design: [plans/structured-errors.md](plans/structured-errors.md).
- **Textual edits for XML, HCL, INI, dotenv and CSV.** Point edits on these
  formats rewrite the file through its writer, so comments and layout are
  lost (XML keeps its comments). Comment-keeping edits, as YAML and TOML
  have, are not built.
- **XML comments and processing instructions inside an element** are
  written before its child elements. Top-level ones keep their order.

## Left from the original roadmap

From the original plan ([archive/dq-plan-2026-05.md](archive/dq-plan-2026-05.md)),
not built:

- **Community rules registry (M12)**: git-based `dq rules add
  github:owner/repo[@ref]` into `~/.config/dq/rules/`, a `dq-rules.yml`
  manifest, `dq rules update`, a small curated index (never an npm-style
  registry, never OCI). Note that the project's anti-scope list currently
  rules out a "community plugin registry"; whether git-based *ruleset*
  installation is also out needs an explicit decision.
- **More plugin surface (M12)**: `parse` / `format` exports so a plugin can
  add a format, a per-rule `check(document, config)` hook, `dq plugin add`
  with a cache under `~/.config/dq/plugins/`, a published dogfood plugin, a
  plugin-enabled release build, CI jobs for the `plugins` and no-feature
  builds, inline offsets passed to plugins, possibly CBOR marshalling, and
  WIT 1.0 once a few third-party plugins exist.
- **Commands**: `dq config get|set|list`, `dq init`.
- **Environment overrides**: `DQ_FORMAT`, `DQ_IN_PLACE`, `DQ_NO_COLOR`,
  `DQ_CONFIG`.
- **Platforms and packaging**: a Windows build and CI job (and an atomic-write
  test on Windows), Docker Hub publication and a published scratch image, an
  AUR `.SRCINFO` and submission automation, signing beyond the build-provenance
  attestation (notarisation, Authenticode), pre-commit hooks with
  `language: rust` instead of `system`.
- **Performance baseline published in the README**: criterion benches exist in
  `dq-core`, `dq-exec`, `dq-transform`, but neither CI nor the release runs
  them, and the README publishes no numbers (the 2026-11-21 runs are in
  the archived perf plans).
- **jq**: conformance against a subset of the jq 1.7 test suite; `--arg` /
  `--argjson` / `--slurpfile`, raw output (`-r`), multi-file and per-line
  JSONL `query`; a fallback to an external `jq` when built without
  `embedded-jq`; a CI build of `dq-transform --no-default-features`.
- **`@std` coverage**: the rule targets of the first lint release were not
  reached (k8s 14 of 18, dockerfile 4 of 8, npm 6 of 8, github-actions 4 of
  6); named but missing: k8s `missing-security-context`,
  `default-capabilities`, `deployment-no-replicas`, `missing-namespace`;
  dockerfile `run-as-root`, `no-update-without-install`, `multiple-cmd`,
  `pin-base-image-by-digest`, `hadolint-style-shell-command`; npm
  `no-pinned-deps`, `lockfile-required`, `tsconfig.json` rules;
  github-actions `no-bash-curl-pipe`, `no-deprecated-actions`; markdown
  `no-trailing-whitespace`, `final-newline`, `no-broken-relative-links`;
  openapi `schema-no-additional-properties-true`. Only two rules carry fixes,
  and `k8s.image-pull-policy-always` still uses the deprecated `fix.jq`.
- **Library and docs**: publish the core crate (the name — `dquery`,
  `dq-core` or other — is still open; `dq` on crates.io is taken), docs.rs,
  a fuller documentation site.
- **Open questions from the plan**: should static-site frontmatter rules
  (Hugo, Jekyll) live in `@std/markdown` or a separate `@std/static-sites`;
  exotic formats (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL) only
  for a concrete use case or contribution.

## Deferred by earlier changes

Still open; the change that deferred each is in [decisions.md](decisions.md).

- **Writes**: textual edits for INI, dotenv, HCL, CSV, XML and Markdown
  (today they are re-emit only); INI quote-style preservation; a
  `--value-format` override for values read from stdin or a file in `set`; a
  `--no-glob` flag; transactional bulk writes; `set --jq` and `fix.jq` keep
  dropping comments until a comment-preserving emitter exists.
- **Reading**: the YAML reader still runs two parsers (`serde_norway` for
  values, `saphyr-parser` for spans); grapheme-cluster `len`; an
  order-sensitive `diff --strict`; JCS / canonical key order; a YAML
  `--indent`.
- **Formats**: `--csv-delimiter`, CSV type inference, `.env` interpolation,
  auto-detecting `*.yaml.tpl` / `*.toml.tpl`, flattening HCL `for_each` /
  `dynamic`, non-UTF-8 XML input, a strict XML writer, positions for inline
  Markdown nodes, `--no-gfm`.
- **Lint engine**: removing the deprecated `loc.line` (19 rules still use it,
  33 set no location) and `fix.jq`; `--no-std`; `dq test --parallel`; an
  `expected_error` fixture key; addressing a single rule as
  `@std/<ns>/<rule>` in `--rules`; more reporters (GitLab Code Quality, HTML,
  GitHub step summary); a rule-compile cache across runs.
- **Distribution**: `dq self update --dry-run`; `install.sh` only prints a
  `PATH` hint and does not edit shell profiles (open question).
- **Performance**: share jaq's parsed standard definitions across compiles
  (most of a compile's ~190 µs); one read-back
  parse per batch of textual edits instead of one per edit (`merge`, `patch`
  and `fix.ops` cost changes × file size); an incremental `pointer_for` in
  the JSON span builder; a guard that holds the benches to a number.

## Decided not to do

These are decisions, not omissions; changing one needs an explicit design
decision first.

From the project's anti-scope:

- A community plugin registry.
- `--quote-style`, `--flow-style`, `--strip-comments`: they need a
  comment-preserving emitter; today only the textual-edit path preserves
  comments.
- XSD, RelaxNG, Schematron validators; JSON Schema 2020-12 is the only schema
  language.
- OpenAPI request/response validation at runtime; only OpenAPI shape rules.
- HCL spans: Terraform diagnostics report line 1.
- Outbound network calls from rules: `$ref` and `schema_file` are local
  only, composite `extract` re-parses inline content. The CLI itself only
  uses the network for `dq self check` / `update`.

From the original plan ("never"):

- A query DSL of our own (JSON Pointer + jq is the language) and a
  declarative policy language of our own.
- A web playground, a GUI, a TUI.
- Native plugins via shared libraries (`dlopen`); WASM only.
- HTTP fetch as input.
- Output templating (Tera, Handlebars).
- Binary formats (Bencode, MessagePack, CBOR), Lua tables, Protobuf text,
  Avro, GraphQL, SQL.
- A Kubernetes admission controller: dq lives in pre-commit and CI.
- An OCI registry for rule sets.
- Pixel-perfect compatibility with existing linters.
