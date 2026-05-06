//! Verifies that every embedded `*.test.yml` fixture's expected outcome
//! matches the actual evaluator output for its companion rule. This is the
//! `dq test crates/dq-lint/rules/` smoke test, run as part of the standard
//! cargo test suite so a broken rule blocks merge.
//!
//! The integration test stages each namespace's rule + fixture pair into
//! a tempdir and feeds it through `RuleTester::run_dir`, then asserts
//! every emitted [`TestOutcome`] is a `Pass`.

use camino::Utf8PathBuf;
use dq_exec::{RuleTester, TestOutcome};
use tempfile::TempDir;

/// Stage `ns`'s embedded rule and fixture files into a fresh tempdir,
/// returning the [`TempDir`] (kept alive by the caller) so the runner can
/// walk it.
fn stage_namespace_to_tempdir(ns: &str) -> TempDir {
    let tmp = TempDir::new().expect("create tempdir for staged ruleset");
    let dir = tmp.path().join(ns);
    std::fs::create_dir_all(&dir).expect("create namespace subdir");

    if let Some(rules) = dq_lint::std_rule_files(ns) {
        for (filename, contents) in rules {
            std::fs::write(dir.join(filename), contents).expect("write rule file");
        }
    }

    if let Some(tests) = dq_lint::std_test_files(ns) {
        for (filename, contents) in tests {
            std::fs::write(dir.join(filename), contents).expect("write fixture file");
        }
    }

    tmp
}

/// Run every fixture in `ns` through the [`RuleTester`] and assert every
/// outcome is a [`TestOutcome::Pass`]. On failure, dump the offending
/// fixtures so the author can see exactly which fixtures mismatched.
fn assert_all_fixtures_pass(ns: &str) {
    let tmp = stage_namespace_to_tempdir(ns);
    let dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf())
        .expect("UTF-8 tempdir path required for RuleTester::run_dir");
    let outcomes = RuleTester::run_dir(&dir).expect("test runner ok");
    assert!(
        !outcomes.is_empty(),
        "@std/{ns} produced zero test outcomes — fixtures missing?",
    );
    let failures: Vec<&TestOutcome> = outcomes
        .iter()
        .filter(|o| !matches!(o, TestOutcome::Pass { .. }))
        .collect();
    assert!(
        failures.is_empty(),
        "@std/{ns} fixtures failed:\n{failures:#?}",
    );
}

#[test]
fn std_k8s_fixtures_pass() {
    assert_all_fixtures_pass("k8s");
}

#[test]
fn std_dockerfile_fixtures_pass() {
    assert_all_fixtures_pass("dockerfile");
}

#[test]
fn std_github_actions_fixtures_pass() {
    assert_all_fixtures_pass("github-actions");
}

#[test]
fn std_npm_fixtures_pass() {
    assert_all_fixtures_pass("npm");
}

/// Smoke test: every fixture in @std/markdown passes its rule.
#[test]
fn std_markdown_fixtures_pass() {
    assert_all_fixtures_pass("markdown");
}
