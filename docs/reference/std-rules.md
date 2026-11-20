# The `@std` rule library

`dq-lint` embeds a standard library of rules into the binary at compile time.
Today it holds **66 rules in 9 namespaces**. Each rule is a YAML file under
`crates/dq-lint/rules/<namespace>/` with a `*.test.yml` fixture beside it;
JSON Schema sidecars (`*.schema.json`) live there too.

## Using it

- `dq lint` with no `--rules` auto-binds every namespace that has a rule for
  one of the input formats; `--rules @std/<ns>` picks namespaces explicitly.
  Each rule then narrows itself with `match.filter` (for example `k8s` rules
  only fire on objects with a workload `kind`, `github-actions` rules only on
  workflow-shaped YAML).
- `dq explain <ns>.<rule>` (or `@std/<ns>.<rule>`) prints a rule's
  description, severity and references.
- `dq rules list [--namespace NS]` lists them; `dq rules add @std/<ns>` copies
  a namespace into `./.dq/rules/<ns>/` to override or vendor it.
- `dq test @std` runs every embedded fixture (436 cases at the time of
  writing), `dq test @std/<ns>` one namespace's; in a checkout,
  `dq test crates/dq-lint/rules/` runs them from the source tree, and
  `cargo test -p dq-lint` runs the same suite.

## Conventions

- Rule id: `<namespace>.<kebab-case-name>`, where the namespace is the
  directory name (`k8s.no-latest-tag`, `github-actions.action-pinned-by-sha`).
- Every rule has a fixture with at least one case where it fires and one
  where it stays silent.
- A rule must not fail on shapes it does not expect: guard field access
  (`.spec.containers[]?`, `objects`, type checks) so that an unexpected type
  produces no finding instead of a jq runtime error.
- Adding a rule: drop `<rule>.yml` and `<rule>.test.yml` into the namespace
  directory; a new namespace is a new directory. See
  [lint-engine.md](lint-engine.md) for the rule format.

Public API (`crates/dq-lint/src/lib.rs`): `std_ruleset(ns)` (the
concatenated YAML of a namespace), `list_std_rulesets()`,
`std_rule_files(ns)`, `std_test_files(ns)`, `std_schema(ns, file)`,
`std_schema_files(ns)`.

## Rules

Severity in brackets. `fix` marks rules with an autofix.

**`k8s`** (14, YAML; Deployment / StatefulSet / DaemonSet / Pod and similar):
`no-latest-tag` [error], `privileged-container` [error], `host-network`
[error], `host-pid` [error], `deprecated-api` [error],
`allow-privilege-escalation` [warn], `run-as-root` [warn],
`missing-resources-limits` [warn], `missing-liveness-probe` [warn],
`missing-readiness-probe` [warn], `hostpath-volume` [warn],
`service-no-selector` [warn], `missing-labels` [info],
`image-pull-policy-always` [info, fix: `fix.jq`, sets `IfNotPresent` on
non-`:latest` images].

**`dockerfile`** (4, Dockerfile): `no-latest-base-image` [error],
`no-add-use-copy` [warn], `no-curl-pipe-bash` [warn], `has-healthcheck`
[info]. They run on Dockerfiles only. A finding points at its instruction
(the line it starts on, the keyword's column); `has-healthcheck` is about the
whole file and reports `1:1`.

**`github-actions`** (4, YAML workflows): `no-pull-request-target-with-checkout`
[error], `action-pinned-by-sha` [warn], `has-permissions` [warn],
`has-timeout` [info].

**`npm`** (6, `package.json`): `no-wildcard-deps` [error],
`scripts-no-rm-rf-root` [error], `has-license` [warn, fix: `fix.ops`, the
reference comment-preserving fix — sets `license: "UNLICENSED"` unless the
package is private], `no-deprecated-fields` [warn], `has-engines` [info],
`has-repository` [info].

**`markdown`** (19, Markdown tree): `code-blocks-yaml-valid` [error,
composite: YAML code blocks must parse], `frontmatter-required-fields`
[error], `heading-order` [error], `no-empty-headings` [error],
`no-empty-links` [error], `single-h1` [error], `code-blocks-have-lang`
[warn], `frontmatter-date-format` [warn], `image-alt-required` [warn],
`no-bare-urls` [warn], `no-duplicate-headings` [warn],
`no-empty-paragraphs` [warn], `no-multiple-blank-lines` [warn],
`no-trailing-spaces-in-headings` [warn], `table-header-required` [warn],
`table-pipes-aligned` [warn], `code-block-fenced` [info],
`link-text-not-here` [info], `no-inline-html` [info]. A finding points at
the line of the block it is about, counted from the start of the file
(frontmatter included); `code-blocks-yaml-valid` points at the YAML error's
line and column inside the block; the frontmatter rules report `1:1`.

**`jsonschema`** (3, schema rules): `kubernetes-crd-shape` [error, YAML/JSON,
basic `kind` / `apiVersion` / `metadata` shape], `openapi-3.1-shape` [error,
YAML/JSON, OpenAPI 3.1 root structure], `helm-values-against-schema` [error,
YAML, a template rule: copy it with `dq rules add` and point its
`schema_file` at your chart's `values.schema.json`].

**`terraform`** (8, HCL): `no-hardcoded-secrets` [error; literal
`password` / `api_key` / `token` assignments — `${…}` references are
allowed], `no-public-ingress` [error; `0.0.0.0/0` in AWS security groups and
GCP firewalls], `output-no-sensitive-without-flag` [error],
`tag-required` [warn; `resource "aws_*"` blocks need `tags`],
`provider-pinned` [warn], `state-backend-required` [warn],
`module-pin-version` [warn; remote module sources need `version`, local
`./` / `../` are exempt], `variable-has-description` [info]. HCL has no spans,
so every finding reports line 1.

**`openapi`** (6, YAML/JSON documents with a string `openapi` field; plain
jq and JSON Schema, no OpenAPI library): `info-required-fields` [error,
`schema_file`], `operation-id-unique` [error], `paths-non-empty` [warn],
`response-200-or-201-required` [warn; any 2xx or `2XX` counts],
`no-trailing-slash` [warn], `security-defined` [warn; top-level `security`
or per operation, `security: []` opts out].

**`skills`** (2): `frontmatter` [error, Markdown `SKILL.md` frontmatter] and
`evals-schema` [error, JSON `evals.json` against `evals.schema.json`; each
eval's statements as skill-creator's `expectations` strings or as
`assertions` objects or strings]. Being
expanded; see [../plans/add-std-skills-rules.md](../plans/add-std-skills-rules.md).

## Known gaps

Tracked in [../roadmap.md](../roadmap.md):

- Rules that raise a jq runtime error on nested fields of an unexpected type
  (`kind: Pod` with `spec: x`) are skipped with a warning instead of staying
  silent by design. The fixture runner fails a case whose rule raises, but no
  fixture covers these shapes yet.
- `github-actions.no-pull-request-target-with-checkout` does not recognise a
  checkout of `${{ github.head_ref }}`.
- Terraform findings report line 1.
- The rule counts originally targeted for the first release (k8s 18,
  dockerfile 8, npm 8, github-actions 6) were not reached. Named but never
  written: k8s `missing-security-context`, `default-capabilities`,
  `deployment-no-replicas`, `missing-namespace`; dockerfile `run-as-root`,
  `no-update-without-install`, `multiple-cmd`, `pin-base-image-by-digest`;
  npm `no-pinned-deps`, `lockfile-required`; github-actions
  `no-bash-curl-pipe`, `no-deprecated-actions`. The plan also sketched
  markdown `no-broken-relative-links` and npm `tsconfig.json` rules.
