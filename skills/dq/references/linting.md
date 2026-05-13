# Lint & autofix reference

`dq lint` runs each rule from the resolved rule set against the given files and emits diagnostics with file / line / column anchors. `dq fix` does the same and, for each matching rule that carries a `fix:` block, applies the rule's whole-document jq transform. `dq check` runs a single rule (or an inline rule) — handy for "why did this fail?" investigation.

## Auto-binding

Without `--rules`, the loader auto-binds:

1. Every `@std/<namespace>` whose rules apply to at least one of the discovered input formats. Filename heuristics:
   - `Dockerfile`, `*.Dockerfile` → `@std/dockerfile`
   - `.github/workflows/*.yml`, `.gitlab-ci.yml` → `@std/github-actions`
   - `*.tf`, `*.hcl` → `@std/terraform`
   - `*.md`, `*.markdown` → `@std/markdown`
   - `package.json` → `@std/npm`
   - Plain `*.yaml` / `*.yml` with k8s `kind:` field → `@std/k8s`
   - Schemas under `*.schema.json` / `openapi.{yaml,json}` → `@std/jsonschema`, `@std/openapi`
2. Everything under `./.dq/rules/*.yml` (recursive), bound without a namespace prefix.

Explicit `--rules` is **additive** — you can repeat it to combine namespaces:

```bash
dq lint --rules @std/k8s --rules @std/dockerfile --rules ./.dq/rules/custom.yml deploy.yaml Dockerfile
```

Rule sources accepted:
- `@std/<namespace>` — bundled ruleset
- absolute or relative path to a `*.yml` file — single rule
- absolute or relative path to a directory — every `*.yml` inside (non-recursive)

## Standard rulesets

64 rules across these namespaces:

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

Use `dq rules list` to dump the full live inventory and `dq explain <id>` to read the description, severity, references, and fix availability for any individual rule.

## Rule anatomy

```yaml
id: k8s.no-latest-tag           # fully-qualified, namespace.rule-id
description: |                  # human prose for `dq explain`
  Containers should not use the :latest tag because ...
severity: error                 # error | warn | info
match:
  format: yaml                  # one of yaml/json/jsonl/toml/hcl/xml/ini/dotenv/csv/markdown/dockerfile/ignore
  filter: '.kind == "Deployment" or .kind == "DaemonSet"'   # optional jq pre-filter
  path: 'k8s/**/*.yaml'         # optional glob; restricts the rule to matching files
check:
  jq: |                         # boolean OR array-of-failures
    (.spec.template.spec.containers // [])
    | to_entries[]
    | select(.value.image | tostring | test(":latest$"))
    | { name: .value.name,
        image: .value.image,
        pointer: ("/spec/template/spec/containers/" + (.key | tostring) + "/image") }
  message: "Container '{{ .name }}' uses :latest tag (image: {{ .image }})"
loc:
  pointer: '.pointer'           # which pointer in each emitted object marks the offending location
fix:                            # optional — applied by `dq fix`
  jq: 'walk(if type == "object" and .image | test(":latest$") then .image |= sub(":latest"; ":pinned") else . end)'
references:
  - https://kubernetes.io/docs/concepts/containers/images/#image-names
```

The `check:` field is a `oneOf` over four kinds:

1. **`jq`** — embedded jq (jaq dialect). Returns either a boolean (`true` = pass, `false` = fail) or an array of objects (each object is one diagnostic, with `loc.pointer` selecting the location).
2. **`schema`** — inline JSON Schema 2020-12. Every `$ref` is internal (HTTP / file `$ref` rejected at compile time).
3. **`schema_file`** — path to a JSON Schema 2020-12 file, resolved relative to the rule directory. `..` escapes rejected.
4. **`extract` + `nested`** — composite cross-format rule:
   - Outer jq returns `[{value, format, anchor}]`.
   - Each item is reparsed as the inner `format` and run through a `nested:` rule (referenced by id).
   - Recursion bounded at depth 4. Inner-format parse failures emit `<outer-id>.parse-failed`.
   - Inline-position spans for nested diagnostics live in `Provenance::Original.inline_offset`.

Reference composite rules: `@std/markdown/code-blocks-yaml-valid`, `@std/jsonschema/kubernetes-crd-shape`, `@std/jsonschema/helm-values-against-schema`, `@std/jsonschema/openapi-3.1-shape`.

## Autofix discipline

Only some rules carry a `fix:` block; without one, `dq fix` skips the rule silently. Reference rules that ship with `fix:`: `@std/k8s/image-pull-policy-always`, `@std/npm/has-license`.

**Idempotency is enforced at runtime.** The engine applies the fix once, then re-checks: if a second application would yield a different document, the fix is rejected with a `tracing::warn!("rule X violates idempotency contract")` and the file is left untouched. This is a load-bearing contract — a rule author who emits `count += 1` without a guard ships a regression.

**Comments are dropped.** `fix:` re-emits the whole document through the format's native writer, like `dq set --jq`. Use point-edits (`dq set FILE POINTER VALUE`) when comment preservation matters; reach for `fix` when you want bulk lint-and-correct.

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
| Text (default) | `-F text`    | Terminal — one line per diagnostic  | stdout        |
| SARIF 2.1.0    | `-F sarif`   | GitHub Code Scanning                | **stderr**    |
| JUnit XML      | `-F junit`   | GitLab CI, Jenkins                  | stderr        |
| TAP 13         | `-F tap`     | TAP consumers (Bats, `prove`)       | stdout        |
| JSON           | `-F json`    | scripting, jq pipelines             | stdout        |

SARIF and JUnit go to **stderr** by design, so progress logging can still print to stdout in CI. Redirect with `2>`, not `>`:

```yaml
- name: Lint manifests
  continue-on-error: true     # exit 4 on parse must not skip the upload
  run: dq lint -F sarif 'k8s/**/*.yaml' 2> lint.sarif
- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: lint.sarif
```

`--strict` promotes `warn`-severity violations to exit 1 (errors are already exit 1; info stays exit 0).

## User rules

Drop `*.yml` files under `./.dq/rules/` — one rule per file. They are auto-bound on every `dq lint` / `dq fix` invocation **without a namespace prefix** (the rule's own `id:` is the binding key).

```yaml
# .dq/rules/no-debug-images.yml
id: org.no-debug-images
severity: error
match:
  format: yaml
  path: 'k8s/**/*.yaml'
check:
  jq: '(.spec.template.spec.containers // [])[].image | test("debug|nightly") | not'
  message: 'no debug/nightly images allowed in k8s manifests'
```

Materialise a standard ruleset into your repo for vendoring or local overrides:

```bash
dq rules add @std/k8s             # writes to ./.dq/rules/k8s/
```

## Inline rules — quick one-offs

```bash
dq check --inline 'id: my.rule
severity: error
match: { format: yaml }
check: { jq: ".spec | has(\"replicas\")", message: "must declare replicas" }' deploy.yaml
```

Useful for "let me check this one assertion" without saving a rule file.

## Testing rules (`dq test`)

Rule authors put a `<rule-name>.test.yml` next to each rule with fixture inputs:

```yaml
# .dq/rules/no-debug-images.test.yml
cases:
  - name: passes on pinned image
    input: |
      spec:
        template:
          spec:
            containers:
              - name: app
                image: app:v1
    expect: pass
  - name: fails on debug image
    input: |
      spec:
        template:
          spec:
            containers:
              - name: app
                image: app:debug-latest
    expect: fail
    diagnostics: 1
```

Run all fixture suites:

```bash
dq test crates/dq-lint/rules/                    # the standard suite
dq test ./.dq/rules/                             # your user rules
```

Exits 1 if any case mismatches; emits per-case `tracing::info!` lines.

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

**Linting in CI without `2>` for SARIF / JUnit**
```bash
# WRONG — SARIF goes to stderr; `>` writes empty stdout to the file
dq lint -F sarif 'k8s/**/*.yaml' > lint.sarif
# RIGHT — `2>` captures stderr
dq lint -F sarif 'k8s/**/*.yaml' 2> lint.sarif
```

**Expecting `fix` to preserve YAML comments**
```bash
# WRONG — `fix` re-emits via native writer, drops comments
dq fix -i deploy.yaml
# RIGHT — when comments matter, pointer-edit one rule at a time
dq set deploy.yaml /spec/template/spec/imagePullPolicy IfNotPresent -i
```

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
