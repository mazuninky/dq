# Lint & autofix reference

`dq lint` runs each rule from the resolved rule set against the given files and emits diagnostics with file / line / column anchors. `dq fix` does the same and, for each matching rule that reports a violation and carries a `fix:` block, applies its fix — a `fix.ops` edit script or a `fix.jq` whole-document transform. `dq check` runs a single rule (or an inline rule) — handy for "why did this fail?" investigation.

## Auto-binding

Without `--rules` (`lint`, `fix`), the loader auto-binds:

1. Every `@std/<namespace>` that has a rule for one of the input files' formats (`*.yaml` → the YAML namespaces such as `@std/k8s` and `@std/github-actions`, `Dockerfile` → `@std/dockerfile`, `*.tf` → `@std/terraform`, `package.json` → `@std/npm`, …). Each rule's own `match.filter` / `match.glob` then decides whether it applies to a file (a `kind: Deployment` for `@std/k8s`, a `.github/workflows/` path for `@std/github-actions`).
2. Every rule under `./.dq/rules/` (recursive), bound without a namespace prefix.

With `--rules`, **only** the sources you name are used — auto-binding is off. Repeat the flag to combine them:

```bash
dq lint --rules @std/k8s --rules @std/dockerfile --rules ./.dq/rules/custom.yml deploy.yaml Dockerfile
```

Rule sources accepted:
- `@std/<namespace>` — bundled ruleset
- absolute or relative path to a `*.yml` file — single rule
- absolute or relative path to a directory — every `*.yml` under it, recursively (`*.test.yml` fixtures excluded)

## Standard rulesets

66 rules across these namespaces:

| Namespace          | Focus                                                         | Example rule ids                                                                                  |
|--------------------|---------------------------------------------------------------|---------------------------------------------------------------------------------------------------|
| `@std/k8s`         | Kubernetes manifests (security + reliability)                 | `k8s.no-latest-tag`, `k8s.image-pull-policy-always`, `k8s.run-as-root`, `k8s.privileged-container`, `k8s.missing-resources-limits`, `k8s.missing-liveness-probe`, `k8s.deprecated-api` |
| `@std/dockerfile`  | Dockerfile hygiene                                            | `dockerfile.no-latest-base-image`, `dockerfile.has-healthcheck`, `dockerfile.no-curl-pipe-bash`, `dockerfile.no-add-use-copy` |
| `@std/github-actions` | GitHub Actions workflow safety                             | `github-actions.action-pinned-by-sha`, `github-actions.has-permissions`, `github-actions.has-timeout`, `github-actions.no-pull-request-target-with-checkout` |
| `@std/terraform`   | Terraform / OpenTofu hygiene                                  | `terraform.no-hardcoded-secrets`, `terraform.provider-pinned`, `terraform.module-pin-version`, `terraform.no-public-ingress`, `terraform.state-backend-required` |
| `@std/markdown`    | Markdown style                                                | `markdown.single-h1`, `markdown.heading-order`, `markdown.code-blocks-have-lang`, `markdown.code-blocks-yaml-valid` (composite), `markdown.no-bare-urls` |
| `@std/npm`         | `package.json` hygiene                                        | `npm.has-license`, `npm.has-engines`, `npm.no-wildcard-deps`, `npm.scripts-no-rm-rf-root` |
| `@std/jsonschema`  | JSON Schema 2020-12 + cross-format helpers                    | `jsonschema.kubernetes-crd-shape`, `jsonschema.helm-values-against-schema`, `jsonschema.openapi-3.1-shape` |
| `@std/openapi`     | OpenAPI shape (lint-time only — no runtime request validation) | `openapi.has-info`, `openapi.has-paths`, `openapi.path-params-defined`                          |
| `@std/skills`      | Claude Code skill bundles                                     | `skills.frontmatter`, `skills.evals-schema`                                                       |

Use `dq rules list` to dump the full live inventory and `dq explain <id>` to read the description, severity, references, and fix availability for any individual rule.

## Rule anatomy

```yaml
id: k8s.no-latest-tag           # fully-qualified, namespace.rule-id
description: |                  # human prose for `dq explain`
  Containers should not use the :latest tag because ...
severity: error                 # error | warn | info
match:
  format: yaml                  # a parser name (yaml, json, toml, hcl, xml, markdown, dockerfile, …) or a list
  filter: '.kind == "Deployment" or .kind == "DaemonSet"'   # optional jq pre-filter
  glob: 'k8s/**/*.yaml'         # optional glob; restricts the rule to matching files
check:
  jq: |                         # every value it emits is one violation
    (.spec.template.spec.containers // [])
    | to_entries[]
    | select(.value.image | tostring | test(":latest$"))
    | { name: .value.name,
        image: .value.image,
        pointer: ("/spec/template/spec/containers/" + (.key | tostring) + "/image") }
  message: "Container '{{ .name }}' uses :latest tag (image: {{ .image }})"
loc:
  pointer: '.pointer'           # which pointer in each emitted object marks the offending location
fix:                            # optional — applied by `dq fix`; ops: a JSON Patch the textual edit applies (comments kept)
  ops: '[(.spec.template.spec.containers // []) | to_entries[] | select(.value.image | tostring | test(":latest$")) | {op: "replace", path: ("/spec/template/spec/containers/" + (.key | tostring) + "/image"), value: (.value.image | sub(":latest$"; ":pinned"))}]'
references:
  - https://kubernetes.io/docs/concepts/containers/images/#image-names
```

The `check:` field is a `oneOf` over four kinds:

1. **`jq`** — embedded jq (jaq dialect). **Every value it emits is one violation** — `true`, `false` and `null` included — so emit only the failures (`select(...)`) and nothing when the file passes. An array is one violation; emit its items (`.[]`) for one each. `loc.pointer` turns an emitted value into the location.
2. **`schema`** — inline JSON Schema 2020-12. Every `$ref` is internal (HTTP / file `$ref` rejected at compile time).
3. **`schema_file`** — path to a JSON Schema 2020-12 file, resolved relative to the rule directory. `..` escapes rejected.
4. **`extract` + `nested`** — composite cross-format rule:
   - Outer jq returns `[{value, format, anchor}]`.
   - Each item is reparsed as the inner `format` and run through a `nested:` rule (referenced by id).
   - Recursion bounded at depth 4. Inner-format parse failures emit `<outer-id>.parse-failed`.
   - Inline-position spans for nested diagnostics live in `Provenance::Original.inline_offset`.

Reference composite rule: `@std/markdown/code-blocks-yaml-valid`. The `@std/jsonschema/*` rules are schema rules (`schema` / `schema_file`).

## Autofix discipline

Only some rules carry a `fix:` block; without one, `dq fix` skips the rule silently. Reference rules that ship with `fix:`: `@std/npm/has-license` (`fix.ops`), `@std/k8s/image-pull-policy-always` (`fix.jq`).

A `fix:` takes one of two forms (both → `ops` wins, with a warning):

- **`fix.ops` (preferred)** — a jq expression that returns a JSON Patch array limited to `add`, `replace`, `remove`. It is applied as a textual edit, so YAML / JSON / TOML comments and layout survive; on HCL, XML, INI, dotenv, CSV/TSV and JSON Lines the file is rewritten through its writer. An empty array means "already fine".
- **`fix.jq` (legacy)** — a whole-document transform whose one output replaces the document, re-emitted through the native writer like `dq set --jq`: **comments in it are dropped**.

**Idempotency is enforced at runtime.** A fix that would change the file again when applied a second time is skipped with a warning and the finding stays. A rule author who emits `count += 1` without a guard ships a regression.

```bash
dq fix --check 'k8s/**/*.yaml'           # pre-commit gate (exit 1 if changes pending)
dq fix --diff  'k8s/**/*.yaml'           # preview only
dq fix -i      'k8s/**/*.yaml'           # apply atomically
dq fix -i --backup --parallel 4 'src/**/*.yaml'
dq fix -i --rules @std/k8s deploy.yaml   # restrict to one ruleset
```

## Output formats for CI

| Format         | Flag         | Use case                            | Where it goes |
|----------------|--------------|-------------------------------------|---------------|
| Console (default) | `-F console` | Terminal                         | stdout        |
| SARIF 2.1.0    | `-F sarif`   | GitHub Code Scanning                | stdout        |
| JUnit XML      | `-F junit`   | GitLab CI, Jenkins                  | stdout        |
| TAP 13         | `-F tap`     | TAP consumers (Bats, `prove`)       | stdout        |
| JSON           | `-F json`    | scripting, jq pipelines             | stdout        |

`lint`, `check` and `fix` print every report on stdout; logs and warnings go to stderr. Redirect with `>`:

```yaml
- name: Lint manifests
  continue-on-error: true     # exit 4 on an error finding must not skip the upload
  run: dq lint -F sarif 'k8s/**/*.yaml' > lint.sarif
- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: lint.sarif
```

`--strict` promotes `warn`-severity violations to exit 1 (an `error` finding is exit 4 either way; `info` stays exit 0). A file that does not parse is a `lint.parse` result and the run exits 3; the other files are still linted and reported. A rule whose `match.filter` or `check.jq` raises is a `lint.rule_error` result and exit 3 too, because its findings may be missing: guard the expression (`type == "object" and .kind == "Pod"`), or pass `--allow-rule-errors` to accept it as a warning.

`dq validate -F sarif FILE` is the parse-only check of one file: it writes its report to **stderr**, and only when the file fails to parse (exit 4).

## User rules

Drop `*.yml` files under `./.dq/rules/` — one rule per file. They are auto-bound on every `dq lint` / `dq fix` invocation **without a namespace prefix** (the rule's own `id:` is the binding key).

```yaml
# .dq/rules/no-debug-images.yml
id: org.no-debug-images
description: Debug and nightly images must not reach the cluster.
severity: error
match:
  format: yaml
  filter: '.kind == "Deployment"'
check:
  jq: '(.spec.template.spec.containers // [])[] | select(.image | test("debug|nightly"))'
  message: "container '{{ .name }}' uses {{ .image }}: no debug/nightly images in k8s manifests"
```

Materialise a standard ruleset into your repo for vendoring or local overrides:

```bash
dq rules add @std/k8s             # writes to ./.dq/rules/k8s/
```

## Inline rules — quick one-offs

```bash
dq check --inline 'id: my.rule
description: A manifest declares its replicas.
severity: error
match: { format: yaml }
check: { jq: "select(.spec | has(\"replicas\") | not)", message: "must declare replicas" }' deploy.yaml
```

Useful for "let me check this one assertion" without saving a rule file.

## Testing rules (`dq test`)

Rule authors put a `<rule-name>.test.yml` next to each rule with fixture inputs:

```yaml
# .dq/rules/no-debug-images.test.yml
tests:
  - name: passes on pinned image
    input: |
      kind: Deployment
      spec:
        template:
          spec:
            containers:
              - name: app
                image: app:v1
    expected:
      violations: []
  - name: fails on debug image
    input: |
      kind: Deployment
      spec:
        template:
          spec:
            containers:
              - name: app
                image: app:debug-latest
    expected:
      violations:
        - rule: org.no-debug-images
          message_contains: "app:debug-latest"
```

A case is evaluated with the fixture file's own path, so a rule restricted by `match.glob` only fires in a fixture whose path the glob matches.

Run all fixture suites:

```bash
dq test @std/k8s                                 # the fixtures embedded for one standard namespace
dq test ./.dq/rules/                             # your user rules
```

Prints one `ok` / `FAIL` line per case and a `summary:` line; exits 4 if any case fails. `-F json` and `-F tap` render the same report.

## WASM plugins (experimental, feature-gated)

Behind `--features plugins`. WIT contract: `dq:plugin@0.1.0` (semver — host refuses different major). Sandbox runs **without WASI**: no network, no filesystem, no processes. Fuel ~1 s CPU; memory cap 64 MiB. Discovery via `--plugins <DIR>` is non-recursive, lexically sorted by file name.

```bash
cargo install --locked --features plugins --git https://github.com/mazuninky/dq dq-cli
dq lint --plugins ./plugins config.yaml
dq fix  --plugins ./plugins config.yaml
```

Without the feature flag, the flag is parsed but loading any `*.wasm` exits 6.

> v0.1.0 is a WIT preview — breaking changes possible before v1.0.0. Pin a specific dq version in CI until the ABI stabilises.

See [examples/plugin-rust/](https://github.com/mazuninky/dq/tree/master/examples/plugin-rust) for a minimal reference plugin.

## Anti-examples

**Capturing lint SARIF / JUnit from stderr**
```bash
# WRONG — lint writes the report to stdout; `2>` captures only log lines
dq lint -F sarif 'k8s/**/*.yaml' 2> lint.sarif
# RIGHT
dq lint -F sarif 'k8s/**/*.yaml' > lint.sarif
```

**Expecting a `fix.jq` rule to preserve YAML comments**
```bash
# A fix.jq rule (k8s.image-pull-policy-always) re-emits the document: comments in it go
dq fix -i deploy.yaml
# When comments matter, preview first, or pointer-edit by hand
dq fix --diff deploy.yaml
dq set deploy.yaml /spec/template/spec/containers/0/imagePullPolicy IfNotPresent -i
```
A `fix.ops` rule (such as `npm.has-license`) edits in place and keeps them.

**Running `dq lint` without `--strict` and treating warnings as failures in CI**
```bash
# WRONG — warns exit 0; CI passes regardless
dq lint 'k8s/**/*.yaml'
echo $?    # → 0 even with warns
# RIGHT — promote warns to failures
dq lint --strict 'k8s/**/*.yaml'
```

**Thinking `--rules` is additive over auto-bind**
```bash
# WRONG — passing ANY --rules disables auto-bind; @std/k8s is NOT also loaded
dq lint --rules @std/dockerfile 'k8s/**/*.yaml' Dockerfile
# RIGHT — list every namespace you want, since auto-bind is off the moment you pass --rules
dq lint --rules @std/dockerfile --rules @std/k8s 'k8s/**/*.yaml' Dockerfile
```
