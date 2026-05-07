## MODIFIED Requirements

### Requirement: Standard ruleset rule count

The standard rule library SHALL ship with at least 57 rules across seven namespaces:

- `@std/k8s` — at least 18 rules covering: `no-latest-tag`, `missing-resources-limits`, `missing-liveness-probe`, `missing-readiness-probe`, `host-network`, `host-pid`, `run-as-root`, `privileged-container`, `allow-privilege-escalation`, `default-capabilities`, `missing-security-context`, `image-pull-policy-always`, `deployment-no-replicas`, `service-no-selector`, `deprecated-api`, `missing-labels`, `missing-namespace`, `hostpath-volume`.
- `@std/dockerfile` — at least 8 rules covering: `no-latest-base-image`, `has-healthcheck`, `no-add-use-copy`, `run-as-root`, `no-update-without-install`, `multiple-cmd`, `no-curl-pipe-bash`, `pin-base-image-by-digest`.
- `@std/npm` — at least 8 rules covering: `no-pinned-deps`, `no-wildcard-deps`, `has-license`, `has-repository`, `has-engines`, `no-deprecated-fields`, `scripts-no-rm-rf-root`, `lockfile-required`.
- `@std/github-actions` — at least 6 rules covering: `action-pinned-by-sha`, `no-pull-request-target-with-checkout`, `no-bash-curl-pipe`, `has-permissions`, `has-timeout`, `no-deprecated-actions`.
- `@std/jsonschema` — at least 3 rules covering: `kubernetes-crd-shape`, `helm-values-against-schema`, `openapi-3.1-shape`.
- `@std/terraform` — at least 8 rules covering: `no-hardcoded-secrets`, `tag-required`, `provider-pinned`, `no-public-ingress`, `state-backend-required`, `module-pin-version`, `output-no-sensitive-without-flag`, `variable-has-description`.
- `@std/openapi` — at least 6 rules covering: `info-required-fields`, `paths-non-empty`, `operation-id-unique`, `response-200-or-201-required`, `no-trailing-slash`, `security-defined`.

Every rule SHALL have a co-located `*.test.yml` fixture file with **at least one positive case** (rule fires) and **at least one negative case** (rule is silent). `dq test crates/dq-lint/rules/` SHALL exit 0 for every rule compiled into the binary.

#### Scenario: K8s ruleset has ≥18 rules
- **WHEN** the loader counts files under `crates/dq-lint/rules/k8s/` matching `*.yml` and not `*.test.yml`
- **THEN** the count is ≥ 18

#### Scenario: JsonSchema ruleset has ≥3 rules
- **WHEN** the loader counts files under `crates/dq-lint/rules/jsonschema/` matching `*.yml` and not `*.test.yml`
- **THEN** the count is ≥ 3

#### Scenario: Terraform ruleset has ≥8 rules
- **WHEN** the loader counts files under `crates/dq-lint/rules/terraform/` matching `*.yml` and not `*.test.yml`
- **THEN** the count is ≥ 8

#### Scenario: OpenAPI ruleset has ≥6 rules
- **WHEN** the loader counts files under `crates/dq-lint/rules/openapi/` matching `*.yml` and not `*.test.yml`
- **THEN** the count is ≥ 6

#### Scenario: Every rule has a fixture
- **WHEN** the test runner walks `crates/dq-lint/rules/` and lists the rules and their fixtures
- **THEN** every `<rule>.yml` has a matching `<rule>.test.yml`

#### Scenario: All standard fixtures pass
- **WHEN** `dq test crates/dq-lint/rules/` runs
- **THEN** the exit code is 0 and the summary reports zero failures

### Requirement: `dq-lint` standard rule library

The `dq-lint` crate SHALL embed the standard rule library at compile time. Rules live under `crates/dq-lint/rules/<namespace>/*.yml` and are surfaced through the public function `pub fn std_ruleset(name: &str) -> Option<&'static str>` which returns the concatenated YAML for `@std/<namespace>`. Companion fixture files `crates/dq-lint/rules/<namespace>/*.test.yml` ship alongside each rule and are also embedded so `dq test @std/k8s` works against the in-binary copies.

A second public function `pub fn list_std_rulesets() -> &'static [&'static str]` enumerates the namespaces (e.g. `["k8s", "dockerfile", "npm", "github-actions", "jsonschema", "terraform", "openapi"]`) for `dq rules list` to render. Schema sidecar files `crates/dq-lint/rules/<namespace>/*.schema.json` are also embedded and surfaced through `pub fn std_schema(namespace: &str, file: &str) -> Option<&'static str>` so `check.schema_file` resolution works for embedded `@std/jsonschema` and `@std/openapi` rules without filesystem access.

#### Scenario: `std_ruleset` returns embedded YAML
- **WHEN** the caller invokes `std_ruleset("k8s")`
- **THEN** the result is `Some(<yaml-string>)` whose content is the concatenation of every `crates/dq-lint/rules/k8s/*.yml` file (excluding `*.test.yml`)

#### Scenario: Unknown namespace returns None
- **WHEN** the caller invokes `std_ruleset("not-a-namespace")`
- **THEN** the result is `None`

#### Scenario: List of namespaces grows with M11
- **WHEN** the caller invokes `list_std_rulesets()`
- **THEN** the slice contains at least the seven namespaces: `"k8s"`, `"dockerfile"`, `"npm"`, `"github-actions"`, `"jsonschema"`, `"terraform"`, `"openapi"`

#### Scenario: `std_schema` returns embedded sidecar
- **WHEN** the caller invokes `std_schema("jsonschema", "kubernetes-crd.schema.json")`
- **THEN** the result is `Some(<json-schema-string>)` if a corresponding sidecar file exists, otherwise `None`
