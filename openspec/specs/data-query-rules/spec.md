# data-query-rules Specification

## Purpose
TBD - created by archiving change add-exec-engine. Update Purpose after archive.
## Requirements
### Requirement: `dq-lint` standard rule library

The `dq-lint` crate SHALL embed the standard rule library at compile time. Rules live under `crates/dq-lint/rules/<namespace>/*.yml` and are surfaced through the public function `pub fn std_ruleset(name: &str) -> Option<&'static str>` which returns the concatenated YAML for `@std/<namespace>`. Companion fixture files `crates/dq-lint/rules/<namespace>/*.test.yml` ship alongside each rule and are also embedded so `dq test @std/k8s` works against the in-binary copies.

A second public function `pub fn list_std_rulesets() -> &'static [&'static str]` enumerates the namespaces (e.g. `["k8s", "dockerfile", "npm", "github-actions"]`) for `dq rules list` to render.

#### Scenario: `std_ruleset` returns embedded YAML
- **WHEN** the caller invokes `std_ruleset("k8s")`
- **THEN** the result is `Some(<yaml-string>)` whose content is the concatenation of every `crates/dq-lint/rules/k8s/*.yml` file (excluding `*.test.yml`)

#### Scenario: Unknown namespace returns None
- **WHEN** the caller invokes `std_ruleset("not-a-namespace")`
- **THEN** the result is `None`

#### Scenario: List of namespaces is non-empty
- **WHEN** the caller invokes `list_std_rulesets()`
- **THEN** the slice contains at least the four M8 namespaces: `"k8s"`, `"dockerfile"`, `"npm"`, `"github-actions"`

### Requirement: Standard ruleset rule count

The M8 standard rule library SHALL ship with at least 40 rules across the four namespaces:

- `@std/k8s` — at least 18 rules covering: `no-latest-tag`, `missing-resources-limits`, `missing-liveness-probe`, `missing-readiness-probe`, `host-network`, `host-pid`, `run-as-root`, `privileged-container`, `allow-privilege-escalation`, `default-capabilities`, `missing-security-context`, `image-pull-policy-always`, `deployment-no-replicas`, `service-no-selector`, `deprecated-api`, `missing-labels`, `missing-namespace`, `hostpath-volume`.
- `@std/dockerfile` — at least 8 rules covering: `no-latest-base-image`, `has-healthcheck`, `no-add-use-copy`, `run-as-root`, `no-update-without-install`, `multiple-cmd`, `no-curl-pipe-bash`, `pin-base-image-by-digest`.
- `@std/npm` — at least 8 rules covering: `no-pinned-deps`, `no-wildcard-deps`, `has-license`, `has-repository`, `has-engines`, `no-deprecated-fields`, `scripts-no-rm-rf-root`, `lockfile-required`.
- `@std/github-actions` — at least 6 rules covering: `action-pinned-by-sha`, `no-pull-request-target-with-checkout`, `no-bash-curl-pipe`, `has-permissions`, `has-timeout`, `no-deprecated-actions`.

Every rule SHALL have a co-located `*.test.yml` fixture file with **at least one positive case** (rule fires) and **at least one negative case** (rule is silent). `dq test crates/dq-lint/rules/` SHALL exit 0 for every rule in the standard library.

#### Scenario: K8s ruleset has ≥18 rules
- **WHEN** the loader counts files under `crates/dq-lint/rules/k8s/` matching `*.yml` and not `*.test.yml`
- **THEN** the count is ≥ 18

#### Scenario: Every rule has a fixture
- **WHEN** the test runner walks `crates/dq-lint/rules/` and lists the rules and their fixtures
- **THEN** every `<rule>.yml` has a matching `<rule>.test.yml`

#### Scenario: All standard fixtures pass
- **WHEN** `dq test crates/dq-lint/rules/` runs
- **THEN** the exit code is 0 and the summary reports zero failures

### Requirement: Rule id namespace policy

Standard rule ids SHALL follow the convention `<namespace>.<kebab-case-name>` where `<namespace>` is the directory name under `crates/dq-lint/rules/` (e.g. `k8s.no-latest-tag`, `npm.has-license`, `github-actions.action-pinned-by-sha`). The `id` field in each rule's YAML SHALL match this convention.

When the user runs `dq explain <id>`, the resolver:

1. If `<id>` starts with `@std/`, takes the substring after `@std/` and splits on the first `.` to recover `(namespace, rule-name)`.
2. If `<id>` starts with a namespace recognised by `list_std_rulesets()`, treats it as a fully-qualified `<namespace>.<name>`.
3. Otherwise, searches every loaded ruleset for an exact id match.

#### Scenario: Resolver finds rule by full id
- **WHEN** the user runs `dq explain k8s.no-latest-tag`
- **THEN** the explain handler prints the rule's description / severity / references

#### Scenario: Resolver accepts `@std/`-prefixed id
- **WHEN** the user runs `dq explain @std/k8s.no-latest-tag`
- **THEN** the same output is produced

#### Scenario: Unknown id with did_you_mean
- **WHEN** the user runs `dq explain k8s.no-latest-tags` (extra `s`)
- **THEN** the handler exits 6 with a message naming `k8s.no-latest-tag` in the suggestion list

