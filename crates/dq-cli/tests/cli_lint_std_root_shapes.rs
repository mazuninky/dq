//! The `@std` rules run on every file of their format, so a document
//! whose root is not a mapping (an empty YAML file, a list, a scalar)
//! reaches each rule's `match.filter`. Those filters used to index the
//! root and raise, printing a `match.filter raised a runtime error`
//! warning per rule, and `jsonschema.helm-values-against-schema`
//! rejected every such YAML file with exit 4.
//!
//! The same filters also saw files of the right format and the wrong
//! family (a CircleCI `config.yml` matched the `github-actions.*`
//! rules) and files of the right family with a field of the wrong type
//! (`jobs: [a]`, `steps: x`, `paths: [/a/]`, `engines: "node"`,
//! `resource = "x"`); both raised the same warning.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Build a `dq` command with a clean environment (same discipline as
/// `cli_smoke.rs::dq`).
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

#[test]
fn a_non_mapping_root_lints_clean_without_a_warning() {
    for (name, src) in [
        ("empty.yaml", ""),
        ("scalar.yaml", "just a string\n"),
        ("list.yaml", "- a\n"),
        ("list.json", "[1]\n"),
        ("s.json", "\"s\"\n"),
        ("null.json", "null\n"),
    ] {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join(name), src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["--no-color", "lint", name])
            .assert()
            .success()
            .stdout("diagnostics: []\n")
            .stderr("");
    }
}

#[test]
fn a_non_mapping_markdown_frontmatter_raises_no_warning() {
    for src in ["---\n- a\n---\n# T\n", "---\njust text\n---\n# T\n"] {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("x.md"), src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["--no-color", "lint", "x.md"])
            .assert()
            // `frontmatter-required-fields` still reports the missing
            // fields (exit 4 and its summary line); no rule raises.
            .stderr(predicate::str::contains("runtime error").not());
    }
}

/// Lint `name` (created with `src` in a fresh directory) and require a
/// clean run: exit 0 and no rule raising `check.jq raised a runtime
/// error` on stderr. Diagnostics the rules legitimately report on the
/// file are not asserted; whether a rule skips or fires on a malformed
/// field is the rule's own fixture (`*.test.yml`) business.
fn assert_lints_without_a_rule_raising(name: &str, src: &str) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, src).expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", name])
        .assert()
        .success()
        .stderr(predicate::str::contains("runtime error").not());
}

#[test]
fn a_circleci_config_is_not_linted_as_a_github_workflow() {
    let src = "\
version: 2.1
jobs:
  build:
    docker:
      - image: cimg/node:20.10
    steps:
      - checkout
      - run:
          name: Install
          command: npm ci
workflows:
  main:
    jobs:
      - build
";
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir(dir.path().join(".circleci")).expect("mkdir");
    std::fs::write(dir.path().join(".circleci/config.yml"), src).expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "-F", "json", ".circleci/config.yml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("github-actions.").not())
        .stderr("");
}

#[test]
fn a_github_workflow_is_still_linted_by_the_github_actions_rules() {
    // The positive twin of the CircleCI test: the `has("on")` guard must
    // not switch the family off for a real workflow.
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("ci.yml"),
        "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: make\n",
    )
    .expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "-F", "json", "ci.yml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("github-actions.has-timeout"))
        .stderr("");
}

#[test]
fn a_workflow_with_jobs_as_a_list_raises_no_warning() {
    assert_lints_without_a_rule_raising("ci.yml", "on: push\njobs: [a]\n");
}

#[test]
fn a_workflow_job_with_steps_as_a_string_raises_no_warning() {
    assert_lints_without_a_rule_raising(
        "ci.yml",
        "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps: x\n",
    );
}

#[test]
fn an_openapi_document_with_paths_as_a_list_raises_no_warning() {
    // `paths: []` is the shape from the report; it never raised, so it
    // stays as a guard. `paths: [/a/]` is the one that did (`keys` over a
    // list yields integers that `endswith` rejects).
    for paths in ["[]", "[/a/]"] {
        assert_lints_without_a_rule_raising(
            "openapi.yaml",
            &format!("openapi: 3.0.0\ninfo:\n  title: T\n  version: \"1\"\npaths: {paths}\n"),
        );
    }
}

#[test]
fn a_package_json_with_engines_and_dependencies_of_the_wrong_type_raises_no_warning() {
    assert_lints_without_a_rule_raising(
        "package.json",
        "{\"name\":\"foo\",\"version\":\"1.0.0\",\"engines\":\"node\",\"dependencies\":[]}\n",
    );
}

#[test]
fn a_terraform_file_with_resource_as_a_string_raises_no_warning() {
    assert_lints_without_a_rule_raising("main.tf", "resource = \"x\"\n");
}

#[test]
fn dq_fix_applies_the_license_fix_to_a_package_json_and_leaves_a_list_root_alone() {
    let dir = TempDir::new().expect("tempdir");

    // Positive: the object root still gets its fix.
    let pkg = dir.path().join("package.json");
    std::fs::write(&pkg, "{\"name\":\"foo\",\"version\":\"1.0.0\"}\n").expect("write");
    dq().current_dir(dir.path())
        .args(["--no-color", "fix", "-i", "package.json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("runtime error").not());
    let fixed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pkg).expect("read")).expect("valid json");
    assert_eq!(fixed["license"], "UNLICENSED");

    // Negative: a list root is skipped by the filter, so the bytes stay.
    let list_dir = dir.path().join("list");
    std::fs::create_dir(&list_dir).expect("mkdir");
    let list = list_dir.join("package.json");
    std::fs::write(&list, "[1, 2]\n").expect("write");
    dq().current_dir(&list_dir)
        .args(["--no-color", "fix", "-i", "package.json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("runtime error").not());
    assert_eq!(std::fs::read_to_string(&list).expect("read"), "[1, 2]\n");
}
