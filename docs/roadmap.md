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

- **[Linear-cost pointers in recursive walks](plans/perf-pointer-recursive-walks.md)
  — done.** `Pointer::with_segment` copied the whole path on every call, so
  recursive walks paid O(depth²) per leaf (65 ns at depth 1, 115 µs at depth
  64). `Pointer::push_segment` / `pop_segment` now let `transform::diff` and
  `transform::merge_into` walk with one `&mut Pointer`; a property test checks
  equivalence with `with_segment`, and a `pointer/recursive_walk` bench exists.
  Left: record the after-numbers and add a deep-document case to the
  `transform/diff` bench (it only diffs a flat map today).
- **[Linear-time JSON span map](plans/perf-json-parse-linear.md) — done.** The
  JSON span builder scanned backwards per value, making compact JSON
  quadratic (10 000 records: 13.9 s, against about 50 ms for YAML). A
  precomputed `LineIndex` (newline offsets, binary search, cached indent per
  line) replaced those scans; a smoke test fails if 10 000 elements take over
  a second. Left: record the after-numbers (target ≤ 300 ms), a changelog
  line, and three optional follow-ups (incremental `pointer_for`, a YAML span
  audit, sharing `LineIndex`).
- **[Compile each jq expression once per evaluator](plans/perf-jq-compile-cache.md)
  — done.** `Evaluator::new` compiled every jq expression from scratch (about
  350 µs each, about 17 ms for `@std/k8s`). A `JqCache` scoped to one
  `Evaluator::new` now shares one engine per distinct expression, composite
  `extract` included. Left: measure `evaluate/evaluator_new` and reset the
  6 ms target to the real rule set, fix a stale doc comment ("28 rules"), a
  changelog line. Idea recorded in the plan: every compile re-parses jaq's
  standard definitions, which may be most of the 350 µs.
- **[`@std/skills`: rules for `SKILL.md` and `evals.json`](plans/add-std-skills-rules.md)
  — partly done.** `skills.frontmatter` and `skills.evals-schema` ship with
  fixtures. Left: the evals schema rejects this repository's own
  `skills/dq/evals/evals.json` (seven errors: every eval has a `name` key);
  both rules have drifted from the upstream skill contracts and need a
  decision on which one to follow; the downstream copies of these rules in
  another repository still need removing; fixtures cannot carry a `path`, so
  `match.glob` cannot be tested; the changelog never announced the namespace.

## Bug queue of the current branch

Found while fixing the YAML-stream, big-number and textual-edit work on this
branch; the items below are still open.

**jq**

- jq path assignment through a missing key fails: `.metadata.labels.team =
  "core"` with no `labels` gives "cannot use null as iterable" (exit 3) in
  `fix.jq` and in `set --jq` on a null document.

(Fixed since this list was drawn up: NaN, infinities and big numbers in JSON
Schema checks, `loc.*`, composite `extract` and plugin `jq.eval`; `keys` /
`values` on arrays; wrong-node-type errors now exit 1.)

**`@std` rules**

- Rules raise jq runtime errors on nested fields of an unexpected type
  (`kind: Pod` with `spec: x`); the engine logs and skips them, so the file
  silently passes.
- The fixture runner counts a rule that raised as zero violations, so a
  fixture cannot catch the above.
- `github-actions.no-pull-request-target-with-checkout` misses a checkout of
  `${{ github.head_ref }}`.

**Positions and schemas**

- Dockerfile diagnostics all report line 1: the format has no spans, although
  every instruction record knows its `line`.
- JSON Schema: a relative `$ref` to an embedded `$id`, and an absolute
  reference to the schema's own `$id`, are rejected as external.

**Formats, reporters, globs**

- HCL: repeated blocks with the same type and labels (`x { a = 1 }` twice)
  keep only the last one, silently. Data must never be lost; check how
  `hcl-rs` represents them and what the `@std/terraform` rules expect.
- SARIF results carry no `ruleId` (`crates/dq-cli/src/output/sarif.rs`).
- An unquoted-prefix glob without a directory part (`2-*.yaml`) matches zero
  files because the walk from `.` yields `./2-x.yaml`; `./2-*.yaml` works.
- JSON writer output: `dq -F json convert f.json` on `{"a": 1}\n` prints an
  extra blank line; the re-emit writer (`set --jq`, `fmt --indent 2 -i`)
  leaves no trailing newline; `patch` `move /ab → /a/b` on
  `{"ab": 1, "a": {}}` writes a multi-line object inside a one-line one.
- `dq lint -` from stdin cannot work: `-F` picks the reporter, so stdin has
  no input format (exit 6). Needs a separate input-format flag or a rule.

**Known limits, not dq bugs**

- `{n: !!str}` with no space before `}` is rejected by every YAML reader
  (saphyr, libyaml, PyYAML).
- CR-only line ends are invalid TOML per the TOML specification.

## Found while writing these docs

Behaviour that disagrees with the original design or the README, verified
against the binary on this branch:

- **`-F` is both output and input**: on `get`, `paths`, `keys`, `validate`,
  `set` and most other commands `-F json` also forces the JSON parser, so
  `dq paths config.yaml -F json` fails with a parse error; only `convert`,
  `diff` and `query` (on a file) treat `-F` as output only. Decide on one
  rule (for example: `-F` overrides the parser only when detection fails, as
  a code comment already claims), or add a separate input-format flag.
- **Structured errors are incomplete**: no caret under the offending byte,
  and no JSON error object on stderr under `-F json` (only `validate` emits
  structured output). Agent-first error output was a founding goal.
- **Template guard** runs only in `set`, `del`, `patch`, `merge`; `get`,
  `convert`, `validate` parse templated files silently (a Helm `{{ .x }}`
  reads as a flow mapping).
- **Bulk `-i` counts unchanged files as Modified**; the summary's Skipped
  only counts files without the requested `--doc`.
- **Globs**: `*` matches across directories (globset's default), so
  `k8s/*.yaml` also picks up `k8s/sub/x.yaml`; `convert` to stdout does not
  take globs.
- **`diff --unified` emits ANSI colour when stdout is not a terminal** (only
  `--no-color` turns it off).
- **`dq self update` verifies no checksum** (the `self_update` checksums
  feature is off); README says it verifies SHA256. No `--dry-run` either.
- **Markdown positions** are counted from the end of the frontmatter, so lint
  findings in a file with frontmatter are off by its length;
  `markdown.code-blocks-yaml-valid` reports at 1:1 because code-block nodes
  carry no byte span.
- **Schema check messages** glue the rule's `message` prefix to the keyword
  location without a separator.
- **`--no-pager`** is accepted, but dq has no pager; implement one or drop the
  flag.
- **README drift**: `-F text` does not exist (`console`); `dq check
  k8s.no-latest-tag f.yaml` needs `--rule`; `dq query post.md '…'` has the
  arguments reversed; the `check --inline` and `.dq/rules/` examples lack the
  required `description` and fail with exit 3; a `check.jq` "returning a
  boolean" fires on every file (every emitted value is a violation); the XML
  row lists `.pom` / `.xsd` and the dotenv row `.env.*`, none of which are
  detected; the `@std/jsonschema` rules are schema rules, not composite;
  autofix is described only as `fix.jq`, while `fix.ops` (comment-preserving)
  is the preferred form.

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
  AUR `.SRCINFO` and submission automation, signing beyond the optional
  cosign step (notarisation, Authenticode), pre-commit hooks with
  `language: rust` instead of `system`.
- **Performance baseline published in the README**: criterion benches exist in
  `dq-core`, `dq-exec`, `dq-transform`, but neither CI nor the release runs
  them and no numbers are published.
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
