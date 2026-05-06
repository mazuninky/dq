//! `*.test.yml` fixture runner — `dq test`.
//!
//! [`RuleTester::run_dir`] walks a directory looking for `*.test.yml` /
//! `*.test.yaml` files, parses them as [`RuleTestFile`], and executes
//! each test case against the matching rule (a sibling `*.yml` file
//! with the same stem minus the `.test` suffix).
//!
//! The comparison policy is described in design D10: every expected
//! violation must match at least one actual diagnostic, and every actual
//! diagnostic must be matched by some expected entry. Order does not
//! matter; flexibility comes from `message_contains` /
//! `message_equals` / `line` filters in the expected entries.
//!
//! ## Format-input parsing scope (M8)
//!
//! The runner parses a fixture's `input:` text by name. Only `yaml` and
//! `json` are supported directly; any other format produces an
//! [`TestOutcome::Error`] with a "format not yet supported in test
//! runner" message. The standard rules in the §7-§8 batch all target
//! `yaml` or `json`; richer format support will land alongside §4.5
//! when the test runner gains a dependency on the full `dq-core`
//! [`dq_core::Format`] machinery.

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;

use crate::diagnostic::Diagnostic;
use crate::error::{ExecError, Result};
use crate::evaluator::Evaluator;
use crate::ruleset::RuleSet;

/// Top-level fixture file shape.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleTestFile {
    /// Test cases declared in this fixture file.
    pub tests: Vec<RuleTestCase>,
}

/// One test case within a [`RuleTestFile`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleTestCase {
    /// Human-readable test name — surfaces in the runner's pass/fail
    /// output.
    pub name: String,
    /// Raw input text. The runner parses it according to `format` (or
    /// the first format in the rule's `match.format` if `format` is
    /// `None`).
    pub input: String,
    /// Optional explicit format override. When `None`, the runner uses
    /// the first format declared in the rule's `match.format` list.
    #[serde(default)]
    pub format: Option<String>,
    /// What the runner expects to see — see [`ExpectedOutcome`].
    pub expected: ExpectedOutcome,
}

/// Expected diagnostics for a test case.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedOutcome {
    /// Expected violation entries. Each must match at least one actual
    /// diagnostic; every actual diagnostic must be matched by at least
    /// one expected entry.
    #[serde(default)]
    pub violations: Vec<ExpectedViolation>,
}

/// One expected violation entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedViolation {
    /// Required `rule_id` of the matching diagnostic.
    pub rule: String,
    /// Optional substring assertion against the diagnostic message.
    #[serde(default)]
    pub message_contains: Option<String>,
    /// Optional exact-equality assertion against the diagnostic message.
    #[serde(default)]
    pub message_equals: Option<String>,
    /// Optional line-number assertion.
    #[serde(default)]
    pub line: Option<u32>,
}

/// One test outcome, emitted by [`RuleTester::run_dir`] per test case.
#[derive(Debug, Clone)]
pub enum TestOutcome {
    /// All expected violations matched and no actual diagnostic was
    /// unmatched.
    Pass {
        /// Fixture file that produced this outcome.
        fixture: Utf8PathBuf,
        /// Test case name.
        name: String,
    },
    /// Test failed — `missing` lists expected violations that didn't
    /// match any actual diagnostic; `extra` lists actual diagnostics
    /// that weren't expected.
    Fail {
        /// Fixture file that produced this outcome.
        fixture: Utf8PathBuf,
        /// Test case name.
        name: String,
        /// Human-readable descriptions of expected violations that
        /// couldn't be matched against any actual diagnostic.
        missing: Vec<String>,
        /// Human-readable descriptions of actual diagnostics that
        /// weren't expected.
        extra: Vec<String>,
    },
    /// The runner couldn't even attempt the comparison — fixture parse
    /// error, missing rule file, unsupported format, etc. The `error`
    /// string carries a fixture-author-facing description.
    Error {
        /// Fixture file that produced this outcome.
        fixture: Utf8PathBuf,
        /// Test case name (or `"<file>"` when the failure happened
        /// before per-case parsing).
        name: String,
        /// Human-readable description of the runner-level failure.
        error: String,
    },
}

/// Stateless namespace for the runner.
#[derive(Debug, Clone, Copy)]
pub struct RuleTester;

impl RuleTester {
    /// Walk `dir` recursively and execute every `*.test.yml` /
    /// `*.test.yaml` fixture found.
    ///
    /// # Errors
    ///
    /// - [`ExecError::Io`] when the directory walk itself fails (e.g.
    ///   the directory doesn't exist). Per-fixture failures are
    ///   reported as [`TestOutcome::Error`] entries — they don't bubble
    ///   up.
    pub fn run_dir(dir: &Utf8Path) -> Result<Vec<TestOutcome>> {
        let mut fixtures: Vec<Utf8PathBuf> = Vec::new();
        for entry in walkdir::WalkDir::new(dir.as_std_path()).sort_by_file_name() {
            let entry = entry.map_err(|err| ExecError::Io {
                path: dir.to_path_buf(),
                source: err.into(),
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(utf8) = Utf8PathBuf::from_path_buf(entry.path().to_path_buf()) else {
                continue;
            };
            if is_fixture_file(&utf8) {
                fixtures.push(utf8);
            }
        }

        let mut outcomes = Vec::new();
        for fixture in &fixtures {
            outcomes.extend(run_fixture(fixture));
        }
        Ok(outcomes)
    }
}

/// Returns `true` for `*.test.yml` / `*.test.yaml` files.
fn is_fixture_file(path: &Utf8Path) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    name.ends_with(".test.yml") || name.ends_with(".test.yaml")
}

/// Run one fixture file and emit one [`TestOutcome`] per test case.
fn run_fixture(fixture: &Utf8Path) -> Vec<TestOutcome> {
    // 1. Locate the parent rule file (same stem minus `.test`).
    let rule_path = match parent_rule_path(fixture) {
        Some(p) => p,
        None => {
            return vec![TestOutcome::Error {
                fixture: fixture.to_path_buf(),
                name: "<file>".to_owned(),
                error: format!("could not derive parent rule path from fixture: {fixture}"),
            }];
        }
    };
    if !rule_path.is_file() {
        return vec![TestOutcome::Error {
            fixture: fixture.to_path_buf(),
            name: "<file>".to_owned(),
            error: format!("rule file not found: {rule_path}"),
        }];
    }

    // 2. Parse the fixture.
    let fixture_yaml = match std::fs::read_to_string(fixture) {
        Ok(s) => s,
        Err(err) => {
            return vec![TestOutcome::Error {
                fixture: fixture.to_path_buf(),
                name: "<file>".to_owned(),
                error: format!("failed to read fixture: {err}"),
            }];
        }
    };
    let fixture_file: RuleTestFile = match serde_yml::from_str(&fixture_yaml) {
        Ok(f) => f,
        Err(err) => {
            return vec![TestOutcome::Error {
                fixture: fixture.to_path_buf(),
                name: "<file>".to_owned(),
                error: format!("fixture parse error: {err}"),
            }];
        }
    };

    // 3. Load the rule and build a one-rule evaluator.
    let ruleset = match RuleSet::from_path(&rule_path) {
        Ok(s) => s,
        Err(err) => {
            return vec![TestOutcome::Error {
                fixture: fixture.to_path_buf(),
                name: "<file>".to_owned(),
                error: format!("rule load error: {err}"),
            }];
        }
    };
    if ruleset.rules.is_empty() {
        return vec![TestOutcome::Error {
            fixture: fixture.to_path_buf(),
            name: "<file>".to_owned(),
            error: format!("rule file produced no rules: {rule_path}"),
        }];
    }
    let evaluator = match Evaluator::new(vec![ruleset.clone()]) {
        Ok(e) => e,
        Err(err) => {
            return vec![TestOutcome::Error {
                fixture: fixture.to_path_buf(),
                name: "<file>".to_owned(),
                error: format!("rule compile error: {err}"),
            }];
        }
    };
    // The rule's primary format — used as a default when a test case
    // omits the `format:` field.
    let default_format = ruleset
        .rules
        .first()
        .and_then(|r| r.match_.format.first())
        .cloned()
        .unwrap_or_else(|| "yaml".to_owned());

    // 4. Execute each test case.
    let mut outcomes = Vec::new();
    for case in fixture_file.tests {
        outcomes.push(run_case(fixture, &evaluator, &default_format, case));
    }
    outcomes
}

/// Derive the parent rule path from a fixture file path
/// (`foo.test.yml` → `foo.yml`, `foo.test.yaml` → `foo.yaml`).
fn parent_rule_path(fixture: &Utf8Path) -> Option<Utf8PathBuf> {
    let name = fixture.file_name()?;
    let parent = fixture.parent()?;
    if let Some(stem) = name.strip_suffix(".test.yml") {
        return Some(parent.join(format!("{stem}.yml")));
    }
    if let Some(stem) = name.strip_suffix(".test.yaml") {
        return Some(parent.join(format!("{stem}.yaml")));
    }
    None
}

/// Execute one [`RuleTestCase`] against `evaluator`.
fn run_case(
    fixture: &Utf8Path,
    evaluator: &Evaluator,
    default_format: &str,
    case: RuleTestCase,
) -> TestOutcome {
    let format = case
        .format
        .clone()
        .unwrap_or_else(|| default_format.to_owned());
    let value = match parse_input(&case.input, &format) {
        Ok(v) => v,
        Err(msg) => {
            return TestOutcome::Error {
                fixture: fixture.to_path_buf(),
                name: case.name,
                error: msg,
            };
        }
    };
    let actual = evaluator.evaluate_file(fixture, &value, &format);
    compare(
        fixture.to_path_buf(),
        case.name,
        &case.expected.violations,
        &actual,
    )
}

/// Parse a fixture's `input:` text under `format`. `yaml` and `json` are
/// wired through `serde_yml` / `serde_json` directly; `markdown` (M9 — the
/// CommonMark + GFM AST format) routes through `dq_core::by_name("markdown")`
/// so the fixture sees the same typed-discriminator-Map shape that the
/// `@std/markdown` rules check.dispatch against. Other formats yield an
/// `Err(message)` for the caller to surface as a [`TestOutcome::Error`].
fn parse_input(input: &str, format: &str) -> std::result::Result<serde_json::Value, String> {
    match format {
        "yaml" => serde_yml::from_str::<serde_json::Value>(input)
            .map_err(|err| format!("yaml parse error: {err}")),
        "json" => serde_json::from_str::<serde_json::Value>(input)
            .map_err(|err| format!("json parse error: {err}")),
        "markdown" => {
            let parser = dq_core::by_name("markdown")
                .ok_or_else(|| "markdown format not registered".to_owned())?;
            let doc = parser
                .parse(input.as_bytes())
                .map_err(|err| format!("markdown parse error: {err}"))?;
            // The evaluator and check.jq path consume `serde_json::Value`,
            // so convert the typed AST tree through `serde_json::to_value`.
            // `Value` (the dq-core type) implements `Serialize` per the
            // top-level `document/mod.rs`, so this is a single hop.
            serde_json::to_value(doc.value())
                .map_err(|err| format!("markdown AST serialize error: {err}"))
        }
        other => Err(format!(
            "format not yet supported in test runner: {other} (M8 supports yaml/json/markdown; richer support lands with §4.5)",
        )),
    }
}

/// Match expected violations against actual diagnostics per design D10.
fn compare(
    fixture: Utf8PathBuf,
    name: String,
    expected: &[ExpectedViolation],
    actual: &[Diagnostic],
) -> TestOutcome {
    let mut actual_matched = vec![false; actual.len()];
    let mut missing: Vec<String> = Vec::new();
    for exp in expected {
        let mut found = false;
        for (idx, diag) in actual.iter().enumerate() {
            if actual_matched[idx] {
                continue;
            }
            if matches_expected(exp, diag) {
                actual_matched[idx] = true;
                found = true;
                break;
            }
        }
        if !found {
            missing.push(describe_expected(exp));
        }
    }
    let extra: Vec<String> = actual
        .iter()
        .enumerate()
        .filter(|(i, _)| !actual_matched[*i])
        .map(|(_, d)| describe_actual(d))
        .collect();

    if missing.is_empty() && extra.is_empty() {
        TestOutcome::Pass { fixture, name }
    } else {
        TestOutcome::Fail {
            fixture,
            name,
            missing,
            extra,
        }
    }
}

/// Predicate: does `diag` satisfy every constraint in `exp`?
fn matches_expected(exp: &ExpectedViolation, diag: &Diagnostic) -> bool {
    if exp.rule != diag.rule_id {
        return false;
    }
    if let Some(needle) = exp.message_contains.as_deref()
        && !diag.message.contains(needle)
    {
        return false;
    }
    if let Some(want) = exp.message_equals.as_deref()
        && diag.message != want
    {
        return false;
    }
    if let Some(line) = exp.line
        && diag.line != line
    {
        return false;
    }
    true
}

fn describe_expected(exp: &ExpectedViolation) -> String {
    let mut parts = vec![format!("rule={}", exp.rule)];
    if let Some(s) = exp.message_contains.as_deref() {
        parts.push(format!("message_contains={s:?}"));
    }
    if let Some(s) = exp.message_equals.as_deref() {
        parts.push(format!("message_equals={s:?}"));
    }
    if let Some(l) = exp.line {
        parts.push(format!("line={l}"));
    }
    parts.join(" ")
}

fn describe_actual(diag: &Diagnostic) -> String {
    format!(
        "rule={} message={:?} line={}",
        diag.rule_id, diag.message, diag.line
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn tempdir_utf8() -> (TempDir, Utf8PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 temp path");
        (temp, path)
    }

    /// A minimal rule that flags any object with `name == ""`.
    const RULE_NAME_NOT_EMPTY: &str = r#"
id: test.name-not-empty
description: name must not be empty
severity: error
match:
  format: yaml
check:
  jq: 'select(.name == "") | .'
  message: 'name is empty'
"#;

    fn write_rule_and_fixture(
        dir: &Utf8Path,
        rule_yaml: &str,
        fixture_yaml: &str,
    ) -> (Utf8PathBuf, Utf8PathBuf) {
        let rule_path = dir.join("name-not-empty.yml");
        let fixture_path = dir.join("name-not-empty.test.yml");
        std::fs::write(&rule_path, rule_yaml).expect("write rule");
        std::fs::write(&fixture_path, fixture_yaml).expect("write fixture");
        (rule_path, fixture_path)
    }

    #[test]
    fn passing_test_case_yields_pass_outcome() {
        let (_t, dir) = tempdir_utf8();
        let fixture = r#"
tests:
  - name: empty name fires the rule
    input: |
      name: ""
    expected:
      violations:
        - rule: test.name-not-empty
"#;
        write_rule_and_fixture(&dir, RULE_NAME_NOT_EMPTY, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            TestOutcome::Pass { name, .. } => {
                assert_eq!(name, "empty name fires the rule");
            }
            other => panic!("expected Pass, got {other:?}"),
        }
    }

    #[test]
    fn failing_test_case_lists_missing_and_extra() {
        let (_t, dir) = tempdir_utf8();
        // The fixture expects a violation but the input doesn't trigger
        // one — so the expected entry ends up in `missing` and the
        // actual list is empty (no `extra`).
        let fixture = r#"
tests:
  - name: rule should fire but doesn't
    input: |
      name: "ok"
    expected:
      violations:
        - rule: test.name-not-empty
"#;
        write_rule_and_fixture(&dir, RULE_NAME_NOT_EMPTY, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            TestOutcome::Fail { missing, extra, .. } => {
                assert_eq!(missing.len(), 1);
                assert!(missing[0].contains("test.name-not-empty"));
                assert!(extra.is_empty());
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn unexpected_actual_violation_is_reported_as_extra() {
        let (_t, dir) = tempdir_utf8();
        // Fixture expects no violations but the input triggers one.
        let fixture = r#"
tests:
  - name: rule fires when it should not
    input: |
      name: ""
    expected:
      violations: []
"#;
        write_rule_and_fixture(&dir, RULE_NAME_NOT_EMPTY, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            TestOutcome::Fail { missing, extra, .. } => {
                assert!(missing.is_empty());
                assert_eq!(extra.len(), 1);
                assert!(extra[0].contains("test.name-not-empty"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn message_contains_matcher_filters_violations() {
        let (_t, dir) = tempdir_utf8();
        let fixture = r#"
tests:
  - name: matches via substring
    input: |
      name: ""
    expected:
      violations:
        - rule: test.name-not-empty
          message_contains: "empty"
"#;
        write_rule_and_fixture(&dir, RULE_NAME_NOT_EMPTY, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert!(matches!(outcomes[0], TestOutcome::Pass { .. }));
    }

    #[test]
    fn message_contains_with_wrong_substring_fails() {
        let (_t, dir) = tempdir_utf8();
        let fixture = r#"
tests:
  - name: substring no match
    input: |
      name: ""
    expected:
      violations:
        - rule: test.name-not-empty
          message_contains: "this-is-not-in-the-message"
"#;
        write_rule_and_fixture(&dir, RULE_NAME_NOT_EMPTY, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        match &outcomes[0] {
            TestOutcome::Fail { missing, extra, .. } => {
                assert_eq!(missing.len(), 1);
                // The actual diagnostic remains unmatched → extra list.
                assert_eq!(extra.len(), 1);
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn missing_rule_file_yields_error_outcome() {
        let (_t, dir) = tempdir_utf8();
        let fixture = r#"
tests:
  - name: orphan
    input: ""
    expected:
      violations: []
"#;
        // Note: no parent rule file written.
        std::fs::write(dir.join("orphan.test.yml"), fixture).expect("write fixture");

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            TestOutcome::Error { error, .. } => {
                assert!(
                    error.contains("rule file not found"),
                    "expected 'rule file not found' in: {error}",
                );
            }
            other => panic!("expected Error outcome, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_format_in_test_case_yields_error_outcome() {
        let (_t, dir) = tempdir_utf8();
        let fixture = r#"
tests:
  - name: unsupported format
    format: hcl
    input: ""
    expected:
      violations: []
"#;
        write_rule_and_fixture(&dir, RULE_NAME_NOT_EMPTY, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            TestOutcome::Error { error, .. } => {
                assert!(
                    error.contains("not yet supported"),
                    "expected unsupported-format message, got: {error}",
                );
            }
            other => panic!("expected Error outcome, got {other:?}"),
        }
    }

    #[test]
    fn json_input_is_parsed_when_format_is_json() {
        let (_t, dir) = tempdir_utf8();
        let rule_yaml = r#"
id: test.name-not-empty
description: name must not be empty
severity: error
match:
  format: [yaml, json]
check:
  jq: 'select(.name == "") | .'
  message: 'name is empty'
"#;
        let fixture = r#"
tests:
  - name: json input fires
    format: json
    input: |
      {"name": ""}
    expected:
      violations:
        - rule: test.name-not-empty
"#;
        write_rule_and_fixture(&dir, rule_yaml, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], TestOutcome::Pass { .. }));
    }

    #[test]
    fn parent_rule_path_strips_test_suffixes() {
        assert_eq!(
            parent_rule_path(Utf8Path::new("/d/foo.test.yml")),
            Some(Utf8PathBuf::from("/d/foo.yml"))
        );
        assert_eq!(
            parent_rule_path(Utf8Path::new("/d/foo.test.yaml")),
            Some(Utf8PathBuf::from("/d/foo.yaml"))
        );
        assert_eq!(parent_rule_path(Utf8Path::new("/d/foo.yml")), None);
    }
}
