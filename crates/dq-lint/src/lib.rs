//! `dq-lint` — embedded standard rule library for the dq lint engine.
//!
//! Rules and their `*.test.yml` fixtures are embedded at compile time via
//! `include_str!`. The [`std_ruleset`] accessor returns the concatenation
//! of every rule under `crates/dq-lint/rules/<namespace>/*.yml` (excluding
//! `*.test.yml`); [`list_std_rulesets`] enumerates the namespaces.
//!
//! The crate has zero production dependencies — it is purely static data
//! plus accessor functions. Consumers (`dq-exec`, `dq-cli`) call into
//! these functions to resolve `@std/<namespace>` references without
//! needing any runtime I/O.

mod embed;

/// Return the concatenated YAML text for the standard rule namespace `name`.
///
/// Returns `None` for unknown namespaces. The returned text is a YAML
/// document stream (rules separated by `---` markers when there is more
/// than one) suitable for `dq_exec::RuleSet::from_str`.
#[must_use]
pub fn std_ruleset(name: &str) -> Option<&'static str> {
    embed::std_ruleset(name)
}

/// List every standard rule namespace shipped with this binary.
///
/// The order is stable: alphabetical by namespace name.
#[must_use]
pub fn list_std_rulesets() -> &'static [&'static str] {
    embed::NAMESPACES
}

/// Return the standard test fixtures for a namespace, as
/// `(filename, yaml-text)` pairs.
///
/// Returns `None` for unknown namespaces. Used by `dq test @std/<ns>` to
/// run the embedded fixtures against the embedded rules.
#[must_use]
pub fn std_test_files(namespace: &str) -> Option<&'static [(&'static str, &'static str)]> {
    embed::std_test_files(namespace)
}

/// Return the embedded rule files for namespace `namespace`, as
/// `(filename, yaml-text)` pairs.
///
/// Mirrors [`std_test_files`] but for the rule definitions themselves —
/// used by `dq rules add @std/<ns>` to materialise per-file rules under
/// `./.dq/rules/<ns>/` so the user can edit individual rules without having
/// to split the concatenated [`std_ruleset`] text.
#[must_use]
pub fn std_rule_files(namespace: &str) -> Option<&'static [(&'static str, &'static str)]> {
    embed::std_rule_files(namespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn std_ruleset_returns_yaml_for_known_namespace() {
        let yaml = std_ruleset("k8s").expect("k8s namespace must be present");
        assert!(
            yaml.contains("id: k8s.no-latest-tag"),
            "expected the no-latest-tag rule in the k8s ruleset, got: {yaml}",
        );
    }

    #[test]
    fn std_ruleset_returns_none_for_unknown_namespace() {
        assert!(std_ruleset("nope").is_none());
        assert!(std_ruleset("").is_none());
        assert!(std_ruleset("@std/k8s").is_none());
    }

    #[test]
    fn list_std_rulesets_contains_all_namespaces() {
        // M9 added the `markdown` namespace; total is now 5. Subsequent
        // milestones may add more — update the count and the per-namespace
        // assertions together when that happens.
        let namespaces = list_std_rulesets();
        assert_eq!(namespaces.len(), 5);
        assert!(namespaces.contains(&"k8s"));
        assert!(namespaces.contains(&"dockerfile"));
        assert!(namespaces.contains(&"npm"));
        assert!(namespaces.contains(&"github-actions"));
        assert!(namespaces.contains(&"markdown"));
    }

    #[test]
    fn list_std_rulesets_is_alphabetical() {
        let mut sorted: Vec<&'static str> = list_std_rulesets().to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, list_std_rulesets());
    }

    #[test]
    fn std_test_files_returns_non_empty_for_known_namespace() {
        let tests = std_test_files("k8s").expect("k8s namespace must have tests");
        assert!(
            !tests.is_empty(),
            "expected at least one fixture file for k8s",
        );
        let (name, body) = tests[0];
        assert!(name.ends_with(".test.yml"), "fixture filename: {name}");
        assert!(body.contains("tests:"), "fixture body: {body}");
    }

    #[test]
    fn std_test_files_returns_none_for_unknown_namespace() {
        assert!(std_test_files("nope").is_none());
    }

    #[test]
    fn every_namespace_has_a_ruleset_and_tests() {
        for ns in list_std_rulesets() {
            assert!(
                std_ruleset(ns).is_some(),
                "namespace {ns} missing from std_ruleset",
            );
            assert!(
                std_test_files(ns).is_some(),
                "namespace {ns} missing from std_test_files",
            );
        }
    }

    /// Round-trip every embedded ruleset through `dq_exec::RuleSet::from_str`
    /// to confirm the YAML is well-formed and the rule schema validates.
    /// This is the load-bearing assertion: if a rule file ever drifts away
    /// from the parsed shape, this test fires before the rule lands in any
    /// downstream pipeline.
    #[test]
    fn every_embedded_ruleset_parses_via_dq_exec() {
        use dq_exec::{RuleSet, RuleSource};

        for ns in list_std_rulesets() {
            let yaml = std_ruleset(ns).expect("namespace yaml present");
            let set = RuleSet::from_str(yaml, RuleSource::Std(ns))
                .unwrap_or_else(|err| panic!("RuleSet::from_str failed for namespace {ns}: {err}"));
            assert!(!set.rules.is_empty(), "namespace {ns} parsed to zero rules",);
        }
    }

    /// Each namespace must ship at least the M8 minimum rule count so
    /// `dq lint @std/<ns>` always returns a non-trivial ruleset.
    #[test]
    fn each_namespace_meets_minimum_rule_count() {
        use dq_exec::{RuleSet, RuleSource};

        // (namespace, minimum rules) — keep in sync with `embed.rs`.
        let expected: &[(&str, usize)] = &[
            ("k8s", 14),
            ("dockerfile", 4),
            ("github-actions", 4),
            ("npm", 6),
            ("markdown", 18),
        ];
        for (ns, min) in expected {
            let yaml = std_ruleset(ns).expect("namespace yaml present");
            let set = RuleSet::from_str(yaml, RuleSource::Std(ns))
                .unwrap_or_else(|err| panic!("parse failed for {ns}: {err}"));
            assert!(
                set.rules.len() >= *min,
                "namespace {ns}: expected at least {min} rules, got {}",
                set.rules.len(),
            );
        }
    }

    /// Each namespace must ship at least one fixture file per rule —
    /// `std_test_files` count >= rule-file count.
    #[test]
    fn each_namespace_has_a_fixture_per_rule() {
        for ns in list_std_rulesets() {
            let rules = std_rule_files(ns).expect("rule files present");
            let tests = std_test_files(ns).expect("tests present");
            assert_eq!(
                rules.len(),
                tests.len(),
                "namespace {ns}: rule-file count must equal fixture count",
            );
        }
    }

    #[test]
    fn k8s_has_at_least_fourteen_test_fixtures() {
        let tests = std_test_files("k8s").expect("k8s tests present");
        assert!(
            tests.len() >= 14,
            "expected ≥14 k8s fixtures, got {}",
            tests.len(),
        );
    }
}
