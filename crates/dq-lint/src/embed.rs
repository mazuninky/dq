//! Compile-time embedding of the standard rule library.
//!
//! Each namespace maps to a `&'static str` built by concatenating every
//! `crates/dq-lint/rules/<namespace>/*.yml` (excluding `*.test.yml`) with
//! `\n---\n` separators. `*.test.yml` files are embedded separately in the
//! per-namespace `(filename, contents)` slices.
//!
//! Adding or removing a rule requires editing this file: the `include_str!`
//! macro inputs are static and a missing entry will result in the rule
//! silently disappearing. Convention: keep the rule lists alphabetised so
//! diffs are easy to read. The very last `include_str!` in each
//! `concat!` block MUST NOT be followed by a `"\n---\n"` separator —
//! `serde_norway::Deserializer::from_str` treats a trailing separator as an
//! empty document and rejects it.

/// Names of every standard rule namespace shipped with `dq-lint`, in
/// alphabetical order. Every entry here must have matching arms in
/// [`std_ruleset`] and [`std_test_files`].
pub const NAMESPACES: &[&str] = &[
    "dockerfile",
    "github-actions",
    "jsonschema",
    "k8s",
    "markdown",
    "npm",
    "openapi",
    "skills",
    "terraform",
];

/// Look up the concatenated rule YAML for namespace `name`.
pub fn std_ruleset(name: &str) -> Option<&'static str> {
    match name {
        "dockerfile" => Some(DOCKERFILE_RULES),
        "github-actions" => Some(GITHUB_ACTIONS_RULES),
        "jsonschema" => Some(JSONSCHEMA_RULES),
        "k8s" => Some(K8S_RULES),
        "markdown" => Some(MARKDOWN_RULES),
        "npm" => Some(NPM_RULES),
        "openapi" => Some(OPENAPI_RULES),
        "skills" => Some(SKILLS_RULES),
        "terraform" => Some(TERRAFORM_RULES),
        _ => None,
    }
}

/// Look up the embedded test fixtures for namespace `namespace`.
pub fn std_test_files(namespace: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match namespace {
        "dockerfile" => Some(DOCKERFILE_TESTS),
        "github-actions" => Some(GITHUB_ACTIONS_TESTS),
        "jsonschema" => Some(JSONSCHEMA_TESTS),
        "k8s" => Some(K8S_TESTS),
        "markdown" => Some(MARKDOWN_TESTS),
        "npm" => Some(NPM_TESTS),
        "openapi" => Some(OPENAPI_TESTS),
        "skills" => Some(SKILLS_TESTS),
        "terraform" => Some(TERRAFORM_TESTS),
        _ => None,
    }
}

/// Look up the embedded rule files for namespace `namespace`, as
/// `(filename, contents)` pairs.
///
/// Mirrors [`std_test_files`] but for the rule definitions themselves —
/// used by `dq rules add @std/<ns>` to materialise the per-file rule YAML
/// under `./.dq/rules/<ns>/` so users can edit individual rules without
/// having to split the concatenated `std_ruleset` text.
pub fn std_rule_files(namespace: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match namespace {
        "dockerfile" => Some(DOCKERFILE_RULE_FILES),
        "github-actions" => Some(GITHUB_ACTIONS_RULE_FILES),
        "jsonschema" => Some(JSONSCHEMA_RULE_FILES),
        "k8s" => Some(K8S_RULE_FILES),
        "markdown" => Some(MARKDOWN_RULE_FILES),
        "npm" => Some(NPM_RULE_FILES),
        "openapi" => Some(OPENAPI_RULE_FILES),
        "skills" => Some(SKILLS_RULE_FILES),
        "terraform" => Some(TERRAFORM_RULE_FILES),
        _ => None,
    }
}

/// Look up an embedded JSON Schema sidecar `file` under `namespace`.
///
/// The `file` argument matches the rule's `check.schema_file:` field
/// verbatim (e.g. `helm-values-template.schema.json`). For now the
/// embedding lives only under the `jsonschema` namespace; other
/// namespaces return `None` to signal "no embedded schema lookup".
pub fn std_schema(namespace: &str, file: &str) -> Option<&'static str> {
    match namespace {
        "jsonschema" => JSONSCHEMA_SCHEMA_FILES
            .iter()
            .find(|(name, _)| *name == file)
            .map(|(_, contents)| *contents),
        "openapi" => OPENAPI_SCHEMA_FILES
            .iter()
            .find(|(name, _)| *name == file)
            .map(|(_, contents)| *contents),
        "skills" => SKILLS_SCHEMA_FILES
            .iter()
            .find(|(name, _)| *name == file)
            .map(|(_, contents)| *contents),
        _ => None,
    }
}

/// Look up every embedded JSON Schema sidecar file for `namespace`, as
/// `(filename, contents)` pairs. Returns an empty slice for namespaces
/// without sidecar schemas.
///
/// Used by integration test runners that stage a namespace into a
/// tempdir so the schema files land next to the rule YAML.
pub fn std_schema_files(namespace: &str) -> &'static [(&'static str, &'static str)] {
    match namespace {
        "jsonschema" => JSONSCHEMA_SCHEMA_FILES,
        "openapi" => OPENAPI_SCHEMA_FILES,
        "skills" => SKILLS_SCHEMA_FILES,
        _ => &[],
    }
}

// ---------------------------------------------------------------------------
// k8s — 14 rules
// ---------------------------------------------------------------------------

static K8S_RULES: &str = concat!(
    include_str!("../rules/k8s/allow-privilege-escalation.yml"),
    "\n---\n",
    include_str!("../rules/k8s/deprecated-api.yml"),
    "\n---\n",
    include_str!("../rules/k8s/host-network.yml"),
    "\n---\n",
    include_str!("../rules/k8s/host-pid.yml"),
    "\n---\n",
    include_str!("../rules/k8s/hostpath-volume.yml"),
    "\n---\n",
    include_str!("../rules/k8s/image-pull-policy-always.yml"),
    "\n---\n",
    include_str!("../rules/k8s/missing-labels.yml"),
    "\n---\n",
    include_str!("../rules/k8s/missing-liveness-probe.yml"),
    "\n---\n",
    include_str!("../rules/k8s/missing-readiness-probe.yml"),
    "\n---\n",
    include_str!("../rules/k8s/missing-resources-limits.yml"),
    "\n---\n",
    include_str!("../rules/k8s/no-latest-tag.yml"),
    "\n---\n",
    include_str!("../rules/k8s/privileged-container.yml"),
    "\n---\n",
    include_str!("../rules/k8s/run-as-root.yml"),
    "\n---\n",
    include_str!("../rules/k8s/service-no-selector.yml"),
);

static K8S_TESTS: &[(&str, &str)] = &[
    (
        "allow-privilege-escalation.test.yml",
        include_str!("../rules/k8s/allow-privilege-escalation.test.yml"),
    ),
    (
        "deprecated-api.test.yml",
        include_str!("../rules/k8s/deprecated-api.test.yml"),
    ),
    (
        "host-network.test.yml",
        include_str!("../rules/k8s/host-network.test.yml"),
    ),
    (
        "host-pid.test.yml",
        include_str!("../rules/k8s/host-pid.test.yml"),
    ),
    (
        "hostpath-volume.test.yml",
        include_str!("../rules/k8s/hostpath-volume.test.yml"),
    ),
    (
        "image-pull-policy-always.test.yml",
        include_str!("../rules/k8s/image-pull-policy-always.test.yml"),
    ),
    (
        "missing-labels.test.yml",
        include_str!("../rules/k8s/missing-labels.test.yml"),
    ),
    (
        "missing-liveness-probe.test.yml",
        include_str!("../rules/k8s/missing-liveness-probe.test.yml"),
    ),
    (
        "missing-readiness-probe.test.yml",
        include_str!("../rules/k8s/missing-readiness-probe.test.yml"),
    ),
    (
        "missing-resources-limits.test.yml",
        include_str!("../rules/k8s/missing-resources-limits.test.yml"),
    ),
    (
        "no-latest-tag.test.yml",
        include_str!("../rules/k8s/no-latest-tag.test.yml"),
    ),
    (
        "privileged-container.test.yml",
        include_str!("../rules/k8s/privileged-container.test.yml"),
    ),
    (
        "run-as-root.test.yml",
        include_str!("../rules/k8s/run-as-root.test.yml"),
    ),
    (
        "service-no-selector.test.yml",
        include_str!("../rules/k8s/service-no-selector.test.yml"),
    ),
];

static K8S_RULE_FILES: &[(&str, &str)] = &[
    (
        "allow-privilege-escalation.yml",
        include_str!("../rules/k8s/allow-privilege-escalation.yml"),
    ),
    (
        "deprecated-api.yml",
        include_str!("../rules/k8s/deprecated-api.yml"),
    ),
    (
        "host-network.yml",
        include_str!("../rules/k8s/host-network.yml"),
    ),
    ("host-pid.yml", include_str!("../rules/k8s/host-pid.yml")),
    (
        "hostpath-volume.yml",
        include_str!("../rules/k8s/hostpath-volume.yml"),
    ),
    (
        "image-pull-policy-always.yml",
        include_str!("../rules/k8s/image-pull-policy-always.yml"),
    ),
    (
        "missing-labels.yml",
        include_str!("../rules/k8s/missing-labels.yml"),
    ),
    (
        "missing-liveness-probe.yml",
        include_str!("../rules/k8s/missing-liveness-probe.yml"),
    ),
    (
        "missing-readiness-probe.yml",
        include_str!("../rules/k8s/missing-readiness-probe.yml"),
    ),
    (
        "missing-resources-limits.yml",
        include_str!("../rules/k8s/missing-resources-limits.yml"),
    ),
    (
        "no-latest-tag.yml",
        include_str!("../rules/k8s/no-latest-tag.yml"),
    ),
    (
        "privileged-container.yml",
        include_str!("../rules/k8s/privileged-container.yml"),
    ),
    (
        "run-as-root.yml",
        include_str!("../rules/k8s/run-as-root.yml"),
    ),
    (
        "service-no-selector.yml",
        include_str!("../rules/k8s/service-no-selector.yml"),
    ),
];

// ---------------------------------------------------------------------------
// dockerfile — 4 rules
// ---------------------------------------------------------------------------

static DOCKERFILE_RULES: &str = concat!(
    include_str!("../rules/dockerfile/has-healthcheck.yml"),
    "\n---\n",
    include_str!("../rules/dockerfile/no-add-use-copy.yml"),
    "\n---\n",
    include_str!("../rules/dockerfile/no-curl-pipe-bash.yml"),
    "\n---\n",
    include_str!("../rules/dockerfile/no-latest-base-image.yml"),
);

static DOCKERFILE_TESTS: &[(&str, &str)] = &[
    (
        "has-healthcheck.test.yml",
        include_str!("../rules/dockerfile/has-healthcheck.test.yml"),
    ),
    (
        "no-add-use-copy.test.yml",
        include_str!("../rules/dockerfile/no-add-use-copy.test.yml"),
    ),
    (
        "no-curl-pipe-bash.test.yml",
        include_str!("../rules/dockerfile/no-curl-pipe-bash.test.yml"),
    ),
    (
        "no-latest-base-image.test.yml",
        include_str!("../rules/dockerfile/no-latest-base-image.test.yml"),
    ),
];

static DOCKERFILE_RULE_FILES: &[(&str, &str)] = &[
    (
        "has-healthcheck.yml",
        include_str!("../rules/dockerfile/has-healthcheck.yml"),
    ),
    (
        "no-add-use-copy.yml",
        include_str!("../rules/dockerfile/no-add-use-copy.yml"),
    ),
    (
        "no-curl-pipe-bash.yml",
        include_str!("../rules/dockerfile/no-curl-pipe-bash.yml"),
    ),
    (
        "no-latest-base-image.yml",
        include_str!("../rules/dockerfile/no-latest-base-image.yml"),
    ),
];

// ---------------------------------------------------------------------------
// npm — 6 rules
// ---------------------------------------------------------------------------

static NPM_RULES: &str = concat!(
    include_str!("../rules/npm/has-engines.yml"),
    "\n---\n",
    include_str!("../rules/npm/has-license.yml"),
    "\n---\n",
    include_str!("../rules/npm/has-repository.yml"),
    "\n---\n",
    include_str!("../rules/npm/no-deprecated-fields.yml"),
    "\n---\n",
    include_str!("../rules/npm/no-wildcard-deps.yml"),
    "\n---\n",
    include_str!("../rules/npm/scripts-no-rm-rf-root.yml"),
);

static NPM_TESTS: &[(&str, &str)] = &[
    (
        "has-engines.test.yml",
        include_str!("../rules/npm/has-engines.test.yml"),
    ),
    (
        "has-license.test.yml",
        include_str!("../rules/npm/has-license.test.yml"),
    ),
    (
        "has-repository.test.yml",
        include_str!("../rules/npm/has-repository.test.yml"),
    ),
    (
        "no-deprecated-fields.test.yml",
        include_str!("../rules/npm/no-deprecated-fields.test.yml"),
    ),
    (
        "no-wildcard-deps.test.yml",
        include_str!("../rules/npm/no-wildcard-deps.test.yml"),
    ),
    (
        "scripts-no-rm-rf-root.test.yml",
        include_str!("../rules/npm/scripts-no-rm-rf-root.test.yml"),
    ),
];

static NPM_RULE_FILES: &[(&str, &str)] = &[
    (
        "has-engines.yml",
        include_str!("../rules/npm/has-engines.yml"),
    ),
    (
        "has-license.yml",
        include_str!("../rules/npm/has-license.yml"),
    ),
    (
        "has-repository.yml",
        include_str!("../rules/npm/has-repository.yml"),
    ),
    (
        "no-deprecated-fields.yml",
        include_str!("../rules/npm/no-deprecated-fields.yml"),
    ),
    (
        "no-wildcard-deps.yml",
        include_str!("../rules/npm/no-wildcard-deps.yml"),
    ),
    (
        "scripts-no-rm-rf-root.yml",
        include_str!("../rules/npm/scripts-no-rm-rf-root.yml"),
    ),
];

// ---------------------------------------------------------------------------
// github-actions — 4 rules
// ---------------------------------------------------------------------------

static GITHUB_ACTIONS_RULES: &str = concat!(
    include_str!("../rules/github-actions/action-pinned-by-sha.yml"),
    "\n---\n",
    include_str!("../rules/github-actions/has-permissions.yml"),
    "\n---\n",
    include_str!("../rules/github-actions/has-timeout.yml"),
    "\n---\n",
    include_str!("../rules/github-actions/no-pull-request-target-with-checkout.yml"),
);

static GITHUB_ACTIONS_TESTS: &[(&str, &str)] = &[
    (
        "action-pinned-by-sha.test.yml",
        include_str!("../rules/github-actions/action-pinned-by-sha.test.yml"),
    ),
    (
        "has-permissions.test.yml",
        include_str!("../rules/github-actions/has-permissions.test.yml"),
    ),
    (
        "has-timeout.test.yml",
        include_str!("../rules/github-actions/has-timeout.test.yml"),
    ),
    (
        "no-pull-request-target-with-checkout.test.yml",
        include_str!("../rules/github-actions/no-pull-request-target-with-checkout.test.yml"),
    ),
];

static GITHUB_ACTIONS_RULE_FILES: &[(&str, &str)] = &[
    (
        "action-pinned-by-sha.yml",
        include_str!("../rules/github-actions/action-pinned-by-sha.yml"),
    ),
    (
        "has-permissions.yml",
        include_str!("../rules/github-actions/has-permissions.yml"),
    ),
    (
        "has-timeout.yml",
        include_str!("../rules/github-actions/has-timeout.yml"),
    ),
    (
        "no-pull-request-target-with-checkout.yml",
        include_str!("../rules/github-actions/no-pull-request-target-with-checkout.yml"),
    ),
];

// ---------------------------------------------------------------------------
// markdown — 19 rules (M11 Phase 4 added the composite `code-blocks-yaml-valid`)
// ---------------------------------------------------------------------------

static MARKDOWN_RULES: &str = concat!(
    include_str!("../rules/markdown/code-block-fenced.yml"),
    "\n---\n",
    include_str!("../rules/markdown/code-blocks-have-lang.yml"),
    "\n---\n",
    include_str!("../rules/markdown/code-blocks-yaml-valid.yml"),
    "\n---\n",
    include_str!("../rules/markdown/frontmatter-date-format.yml"),
    "\n---\n",
    include_str!("../rules/markdown/frontmatter-required-fields.yml"),
    "\n---\n",
    include_str!("../rules/markdown/heading-order.yml"),
    "\n---\n",
    include_str!("../rules/markdown/image-alt-required.yml"),
    "\n---\n",
    include_str!("../rules/markdown/link-text-not-here.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-bare-urls.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-duplicate-headings.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-empty-headings.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-empty-links.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-empty-paragraphs.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-inline-html.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-multiple-blank-lines.yml"),
    "\n---\n",
    include_str!("../rules/markdown/no-trailing-spaces-in-headings.yml"),
    "\n---\n",
    include_str!("../rules/markdown/single-h1.yml"),
    "\n---\n",
    include_str!("../rules/markdown/table-header-required.yml"),
    "\n---\n",
    include_str!("../rules/markdown/table-pipes-aligned.yml"),
);

static MARKDOWN_TESTS: &[(&str, &str)] = &[
    (
        "code-block-fenced.test.yml",
        include_str!("../rules/markdown/code-block-fenced.test.yml"),
    ),
    (
        "code-blocks-have-lang.test.yml",
        include_str!("../rules/markdown/code-blocks-have-lang.test.yml"),
    ),
    (
        "code-blocks-yaml-valid.test.yml",
        include_str!("../rules/markdown/code-blocks-yaml-valid.test.yml"),
    ),
    (
        "frontmatter-date-format.test.yml",
        include_str!("../rules/markdown/frontmatter-date-format.test.yml"),
    ),
    (
        "frontmatter-required-fields.test.yml",
        include_str!("../rules/markdown/frontmatter-required-fields.test.yml"),
    ),
    (
        "heading-order.test.yml",
        include_str!("../rules/markdown/heading-order.test.yml"),
    ),
    (
        "image-alt-required.test.yml",
        include_str!("../rules/markdown/image-alt-required.test.yml"),
    ),
    (
        "link-text-not-here.test.yml",
        include_str!("../rules/markdown/link-text-not-here.test.yml"),
    ),
    (
        "no-bare-urls.test.yml",
        include_str!("../rules/markdown/no-bare-urls.test.yml"),
    ),
    (
        "no-duplicate-headings.test.yml",
        include_str!("../rules/markdown/no-duplicate-headings.test.yml"),
    ),
    (
        "no-empty-headings.test.yml",
        include_str!("../rules/markdown/no-empty-headings.test.yml"),
    ),
    (
        "no-empty-links.test.yml",
        include_str!("../rules/markdown/no-empty-links.test.yml"),
    ),
    (
        "no-empty-paragraphs.test.yml",
        include_str!("../rules/markdown/no-empty-paragraphs.test.yml"),
    ),
    (
        "no-inline-html.test.yml",
        include_str!("../rules/markdown/no-inline-html.test.yml"),
    ),
    (
        "no-multiple-blank-lines.test.yml",
        include_str!("../rules/markdown/no-multiple-blank-lines.test.yml"),
    ),
    (
        "no-trailing-spaces-in-headings.test.yml",
        include_str!("../rules/markdown/no-trailing-spaces-in-headings.test.yml"),
    ),
    (
        "single-h1.test.yml",
        include_str!("../rules/markdown/single-h1.test.yml"),
    ),
    (
        "table-header-required.test.yml",
        include_str!("../rules/markdown/table-header-required.test.yml"),
    ),
    (
        "table-pipes-aligned.test.yml",
        include_str!("../rules/markdown/table-pipes-aligned.test.yml"),
    ),
];

static MARKDOWN_RULE_FILES: &[(&str, &str)] = &[
    (
        "code-block-fenced.yml",
        include_str!("../rules/markdown/code-block-fenced.yml"),
    ),
    (
        "code-blocks-have-lang.yml",
        include_str!("../rules/markdown/code-blocks-have-lang.yml"),
    ),
    (
        "code-blocks-yaml-valid.yml",
        include_str!("../rules/markdown/code-blocks-yaml-valid.yml"),
    ),
    (
        "frontmatter-date-format.yml",
        include_str!("../rules/markdown/frontmatter-date-format.yml"),
    ),
    (
        "frontmatter-required-fields.yml",
        include_str!("../rules/markdown/frontmatter-required-fields.yml"),
    ),
    (
        "heading-order.yml",
        include_str!("../rules/markdown/heading-order.yml"),
    ),
    (
        "image-alt-required.yml",
        include_str!("../rules/markdown/image-alt-required.yml"),
    ),
    (
        "link-text-not-here.yml",
        include_str!("../rules/markdown/link-text-not-here.yml"),
    ),
    (
        "no-bare-urls.yml",
        include_str!("../rules/markdown/no-bare-urls.yml"),
    ),
    (
        "no-duplicate-headings.yml",
        include_str!("../rules/markdown/no-duplicate-headings.yml"),
    ),
    (
        "no-empty-headings.yml",
        include_str!("../rules/markdown/no-empty-headings.yml"),
    ),
    (
        "no-empty-links.yml",
        include_str!("../rules/markdown/no-empty-links.yml"),
    ),
    (
        "no-empty-paragraphs.yml",
        include_str!("../rules/markdown/no-empty-paragraphs.yml"),
    ),
    (
        "no-inline-html.yml",
        include_str!("../rules/markdown/no-inline-html.yml"),
    ),
    (
        "no-multiple-blank-lines.yml",
        include_str!("../rules/markdown/no-multiple-blank-lines.yml"),
    ),
    (
        "no-trailing-spaces-in-headings.yml",
        include_str!("../rules/markdown/no-trailing-spaces-in-headings.yml"),
    ),
    (
        "single-h1.yml",
        include_str!("../rules/markdown/single-h1.yml"),
    ),
    (
        "table-header-required.yml",
        include_str!("../rules/markdown/table-header-required.yml"),
    ),
    (
        "table-pipes-aligned.yml",
        include_str!("../rules/markdown/table-pipes-aligned.yml"),
    ),
];

// ---------------------------------------------------------------------------
// jsonschema — 3 rules (M11 Phase 3)
// ---------------------------------------------------------------------------

static JSONSCHEMA_RULES: &str = concat!(
    include_str!("../rules/jsonschema/helm-values-against-schema.yml"),
    "\n---\n",
    include_str!("../rules/jsonschema/kubernetes-crd-shape.yml"),
    "\n---\n",
    include_str!("../rules/jsonschema/openapi-3.1-shape.yml"),
);

static JSONSCHEMA_TESTS: &[(&str, &str)] = &[
    (
        "helm-values-against-schema.test.yml",
        include_str!("../rules/jsonschema/helm-values-against-schema.test.yml"),
    ),
    (
        "kubernetes-crd-shape.test.yml",
        include_str!("../rules/jsonschema/kubernetes-crd-shape.test.yml"),
    ),
    (
        "openapi-3.1-shape.test.yml",
        include_str!("../rules/jsonschema/openapi-3.1-shape.test.yml"),
    ),
];

static JSONSCHEMA_RULE_FILES: &[(&str, &str)] = &[
    (
        "helm-values-against-schema.yml",
        include_str!("../rules/jsonschema/helm-values-against-schema.yml"),
    ),
    (
        "kubernetes-crd-shape.yml",
        include_str!("../rules/jsonschema/kubernetes-crd-shape.yml"),
    ),
    (
        "openapi-3.1-shape.yml",
        include_str!("../rules/jsonschema/openapi-3.1-shape.yml"),
    ),
];

/// Sidecar JSON Schema files embedded alongside the `@std/jsonschema`
/// rules. Keys must match the `check.schema_file:` strings authors
/// write in the rule YAML (e.g. `helm-values-template.schema.json`).
static JSONSCHEMA_SCHEMA_FILES: &[(&str, &str)] = &[(
    "helm-values-template.schema.json",
    include_str!("../rules/jsonschema/helm-values-template.schema.json"),
)];

// ---------------------------------------------------------------------------
// terraform — 8 rules (M11 Phase 5)
// ---------------------------------------------------------------------------
//
// HCL backend has NO span information (see
// `crates/dq-core/src/parsers/hcl.rs` doc-comment) — every diagnostic
// emitted by these rules anchors at `(line=1, col=1)`. Rule descriptions
// document the limitation.

static TERRAFORM_RULES: &str = concat!(
    include_str!("../rules/terraform/module-pin-version.yml"),
    "\n---\n",
    include_str!("../rules/terraform/no-hardcoded-secrets.yml"),
    "\n---\n",
    include_str!("../rules/terraform/no-public-ingress.yml"),
    "\n---\n",
    include_str!("../rules/terraform/output-no-sensitive-without-flag.yml"),
    "\n---\n",
    include_str!("../rules/terraform/provider-pinned.yml"),
    "\n---\n",
    include_str!("../rules/terraform/state-backend-required.yml"),
    "\n---\n",
    include_str!("../rules/terraform/tag-required.yml"),
    "\n---\n",
    include_str!("../rules/terraform/variable-has-description.yml"),
);

static TERRAFORM_TESTS: &[(&str, &str)] = &[
    (
        "module-pin-version.test.yml",
        include_str!("../rules/terraform/module-pin-version.test.yml"),
    ),
    (
        "no-hardcoded-secrets.test.yml",
        include_str!("../rules/terraform/no-hardcoded-secrets.test.yml"),
    ),
    (
        "no-public-ingress.test.yml",
        include_str!("../rules/terraform/no-public-ingress.test.yml"),
    ),
    (
        "output-no-sensitive-without-flag.test.yml",
        include_str!("../rules/terraform/output-no-sensitive-without-flag.test.yml"),
    ),
    (
        "provider-pinned.test.yml",
        include_str!("../rules/terraform/provider-pinned.test.yml"),
    ),
    (
        "state-backend-required.test.yml",
        include_str!("../rules/terraform/state-backend-required.test.yml"),
    ),
    (
        "tag-required.test.yml",
        include_str!("../rules/terraform/tag-required.test.yml"),
    ),
    (
        "variable-has-description.test.yml",
        include_str!("../rules/terraform/variable-has-description.test.yml"),
    ),
];

static TERRAFORM_RULE_FILES: &[(&str, &str)] = &[
    (
        "module-pin-version.yml",
        include_str!("../rules/terraform/module-pin-version.yml"),
    ),
    (
        "no-hardcoded-secrets.yml",
        include_str!("../rules/terraform/no-hardcoded-secrets.yml"),
    ),
    (
        "no-public-ingress.yml",
        include_str!("../rules/terraform/no-public-ingress.yml"),
    ),
    (
        "output-no-sensitive-without-flag.yml",
        include_str!("../rules/terraform/output-no-sensitive-without-flag.yml"),
    ),
    (
        "provider-pinned.yml",
        include_str!("../rules/terraform/provider-pinned.yml"),
    ),
    (
        "state-backend-required.yml",
        include_str!("../rules/terraform/state-backend-required.yml"),
    ),
    (
        "tag-required.yml",
        include_str!("../rules/terraform/tag-required.yml"),
    ),
    (
        "variable-has-description.yml",
        include_str!("../rules/terraform/variable-has-description.yml"),
    ),
];

// ---------------------------------------------------------------------------
// openapi — 6 rules (M11 Phase 5)
// ---------------------------------------------------------------------------
//
// Phase 5 implementation took the no-`oas3` path (see
// `openspec/changes/add-validation-and-extended-formats/design.md` D6
// closing note) — every rule is authored with plain jq + JSON Schema
// over the YAML/JSON parser, so the namespace ships unconditionally
// without a feature gate.

static OPENAPI_RULES: &str = concat!(
    include_str!("../rules/openapi/info-required-fields.yml"),
    "\n---\n",
    include_str!("../rules/openapi/no-trailing-slash.yml"),
    "\n---\n",
    include_str!("../rules/openapi/operation-id-unique.yml"),
    "\n---\n",
    include_str!("../rules/openapi/paths-non-empty.yml"),
    "\n---\n",
    include_str!("../rules/openapi/response-200-or-201-required.yml"),
    "\n---\n",
    include_str!("../rules/openapi/security-defined.yml"),
);

static OPENAPI_TESTS: &[(&str, &str)] = &[
    (
        "info-required-fields.test.yml",
        include_str!("../rules/openapi/info-required-fields.test.yml"),
    ),
    (
        "no-trailing-slash.test.yml",
        include_str!("../rules/openapi/no-trailing-slash.test.yml"),
    ),
    (
        "operation-id-unique.test.yml",
        include_str!("../rules/openapi/operation-id-unique.test.yml"),
    ),
    (
        "paths-non-empty.test.yml",
        include_str!("../rules/openapi/paths-non-empty.test.yml"),
    ),
    (
        "response-200-or-201-required.test.yml",
        include_str!("../rules/openapi/response-200-or-201-required.test.yml"),
    ),
    (
        "security-defined.test.yml",
        include_str!("../rules/openapi/security-defined.test.yml"),
    ),
];

static OPENAPI_RULE_FILES: &[(&str, &str)] = &[
    (
        "info-required-fields.yml",
        include_str!("../rules/openapi/info-required-fields.yml"),
    ),
    (
        "no-trailing-slash.yml",
        include_str!("../rules/openapi/no-trailing-slash.yml"),
    ),
    (
        "operation-id-unique.yml",
        include_str!("../rules/openapi/operation-id-unique.yml"),
    ),
    (
        "paths-non-empty.yml",
        include_str!("../rules/openapi/paths-non-empty.yml"),
    ),
    (
        "response-200-or-201-required.yml",
        include_str!("../rules/openapi/response-200-or-201-required.yml"),
    ),
    (
        "security-defined.yml",
        include_str!("../rules/openapi/security-defined.yml"),
    ),
];

/// Sidecar JSON Schema files embedded alongside the `@std/openapi`
/// rules. Keys must match the `check.schema_file:` strings authors
/// write in the rule YAML (e.g. `openapi-info.schema.json`).
static OPENAPI_SCHEMA_FILES: &[(&str, &str)] = &[(
    "openapi-info.schema.json",
    include_str!("../rules/openapi/openapi-info.schema.json"),
)];

// ---------------------------------------------------------------------------
// skills — 2 rules (frontmatter + evals-schema)
// ---------------------------------------------------------------------------

static SKILLS_RULES: &str = concat!(
    include_str!("../rules/skills/evals-schema.yml"),
    "\n---\n",
    include_str!("../rules/skills/frontmatter.yml"),
);

static SKILLS_TESTS: &[(&str, &str)] = &[
    (
        "evals-schema.test.yml",
        include_str!("../rules/skills/evals-schema.test.yml"),
    ),
    (
        "frontmatter.test.yml",
        include_str!("../rules/skills/frontmatter.test.yml"),
    ),
];

static SKILLS_RULE_FILES: &[(&str, &str)] = &[
    (
        "evals-schema.yml",
        include_str!("../rules/skills/evals-schema.yml"),
    ),
    (
        "frontmatter.yml",
        include_str!("../rules/skills/frontmatter.yml"),
    ),
];

/// Sidecar JSON Schema files embedded alongside the `@std/skills`
/// rules. Keys must match the `check.schema_file:` strings authors
/// write in the rule YAML (e.g. `evals.schema.json`).
static SKILLS_SCHEMA_FILES: &[(&str, &str)] = &[(
    "evals.schema.json",
    include_str!("../rules/skills/evals.schema.json"),
)];
