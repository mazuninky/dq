//! `*.test.yml` fixture runner — `dq test`.
//!
//! [`RuleTester::run_dir`] walks a directory looking for `*.test.yml` /
//! `*.test.yaml` files, parses them as [`RuleTestFile`], and executes
//! each test case against the matching rule (a sibling `*.yml` file
//! with the same stem minus the `.test` suffix).
//! [`RuleTester::run_std`] does the same over the fixtures embedded in
//! `dq-lint` for one `@std/<namespace>` (`dq test @std/k8s`).
//!
//! The comparison policy is described in design D10: every expected
//! violation must match at least one actual diagnostic, and every actual
//! diagnostic must be matched by some expected entry. Order does not
//! matter; flexibility comes from `message_contains` /
//! `message_equals` / `line` filters in the expected entries.
//!
//! ## Format-input parsing scope
//!
//! The runner parses a fixture's `input:` text with dq's own reader for
//! the format, as `dq check` reads a file: `yaml`, `json`, `markdown`
//! (CommonMark + GFM AST), `hcl` (so the `@std/terraform` ruleset can ship
//! with co-located fixtures) and `dockerfile`. Any other format produces a
//! [`TestOutcome::Error`] with a "format not yet supported in test runner"
//! message — that message intentionally points future authors at the
//! `dq_core::by_name` registry, since adding a format here is a one-word
//! change.

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;

use crate::diagnostic::Diagnostic;
use crate::error::{ExecError, Result};
use crate::evaluator::{Evaluation, Evaluator};
use crate::ruleset::{RuleSet, RuleSource};

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
    /// that weren't expected; `raised` lists the rule expressions that
    /// raised a runtime error on the input. Any one of them non-empty
    /// fails the case.
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
        /// One [`crate::RuleError`] per rule expression that raised,
        /// rendered as `rule=<id> <field>: <jq error>`. A rule that
        /// raises is a bug in the rule even when no violation is
        /// expected: `dq lint` would skip it on such a file.
        raised: Vec<String>,
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

    /// Execute every fixture embedded for the standard rule namespace
    /// `namespace` (`"k8s"`, without the `@std/` prefix), in file-name
    /// order, against the embedded rules — what [`RuleTester::run_dir`]
    /// does on `crates/dq-lint/rules/<namespace>/`, without the source
    /// tree. Outcomes name a fixture `@std/<namespace>/<file>.test.yml`.
    ///
    /// # Errors
    ///
    /// - [`ExecError::UnknownRule`] (id `@std/<namespace>`, with typo
    ///   suggestions) when `dq-lint` has no such namespace.
    pub fn run_std(namespace: &str) -> Result<Vec<TestOutcome>> {
        let arg = format!("@std/{namespace}");
        let Some(namespace) = dq_lint::list_std_rulesets()
            .iter()
            .copied()
            .find(|ns| *ns == namespace)
        else {
            return Err(crate::loader::unknown_rule(&arg));
        };
        let mut fixtures: Vec<(&str, &str)> = dq_lint::std_test_files(namespace)
            .unwrap_or_default()
            .to_vec();
        fixtures.sort_unstable_by_key(|(name, _)| *name);
        let mut outcomes = Vec::new();
        for (name, text) in fixtures {
            let label = Utf8PathBuf::from(format!("{arg}/{name}"));
            outcomes.extend(run_std_fixture(namespace, &label, text));
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
    let Some(rule_path) = parent_rule_path(fixture) else {
        return file_error(
            fixture,
            format!("could not derive parent rule path from fixture: {fixture}"),
        );
    };
    if !rule_path.is_file() {
        return file_error(fixture, format!("rule file not found: {rule_path}"));
    }
    let fixture_yaml = match std::fs::read_to_string(fixture) {
        Ok(s) => s,
        Err(err) => return file_error(fixture, format!("failed to read fixture: {err}")),
    };
    run_fixture_text(fixture, &fixture_yaml, &rule_path, || {
        RuleSet::from_path(&rule_path)
    })
}

/// Run one embedded `@std/<namespace>` fixture. `fixture` is the
/// `@std/<namespace>/<file>.test.yml` label outcomes carry; the rule is
/// the embedded sibling file, loaded as `RuleSource::Std` so a
/// `schema_file` resolves to the embedded sidecar, as in `dq lint`.
fn run_std_fixture(
    namespace: &'static str,
    fixture: &Utf8Path,
    fixture_yaml: &str,
) -> Vec<TestOutcome> {
    let Some(rule_path) = parent_rule_path(fixture) else {
        return file_error(
            fixture,
            format!("could not derive parent rule path from fixture: {fixture}"),
        );
    };
    let rule_name = rule_path.file_name().unwrap_or_default();
    let rule_text = dq_lint::std_rule_files(namespace)
        .unwrap_or_default()
        .iter()
        .find(|(name, _)| *name == rule_name)
        .map(|(_, text)| *text);
    let Some(rule_text) = rule_text else {
        return file_error(fixture, format!("rule file not found: {rule_path}"));
    };
    run_fixture_text(fixture, fixture_yaml, &rule_path, || {
        RuleSet::from_str(rule_text, RuleSource::Std(namespace))
    })
}

/// One [`TestOutcome::Error`] for a fixture that could not run at all.
fn file_error(fixture: &Utf8Path, error: String) -> Vec<TestOutcome> {
    vec![TestOutcome::Error {
        fixture: fixture.to_path_buf(),
        name: "<file>".to_owned(),
        error,
    }]
}

/// Parse `fixture_yaml`, load its rule with `load_rule` (`rule_path`
/// names it in errors) and run every case.
fn run_fixture_text(
    fixture: &Utf8Path,
    fixture_yaml: &str,
    rule_path: &Utf8Path,
    load_rule: impl FnOnce() -> Result<RuleSet>,
) -> Vec<TestOutcome> {
    // 2. Parse the fixture.
    let fixture_file: RuleTestFile = match serde_norway::from_str(fixture_yaml) {
        Ok(f) => f,
        Err(err) => return file_error(fixture, format!("fixture parse error: {err}")),
    };

    // 3. Load the rule and build a one-rule evaluator.
    let ruleset = match load_rule() {
        Ok(s) => s,
        Err(err) => return file_error(fixture, format!("rule load error: {err}")),
    };
    if ruleset.rules.is_empty() {
        return file_error(fixture, format!("rule file produced no rules: {rule_path}"));
    }
    let evaluator = match Evaluator::new(vec![ruleset.clone()]) {
        Ok(e) => e,
        Err(err) => return file_error(fixture, format!("rule compile error: {err}")),
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
    let doc = match parse_input(&case.input, &format) {
        Ok(doc) => doc,
        Err(msg) => {
            return TestOutcome::Error {
                fixture: fixture.to_path_buf(),
                name: case.name,
                error: msg,
            };
        }
    };
    // The document as the format's reader produced it, positions included:
    // where the reader records them (Dockerfile), a `loc.pointer` resolves
    // to the `line:col` `dq lint` reports, so a fixture's `line:` holds;
    // elsewhere (the span-less YAML and JSON readers) it falls through to
    // `(1, 1)`.
    let actual = evaluator.evaluate(fixture, &doc.as_ir(), &format);
    compare(
        fixture.to_path_buf(),
        case.name,
        &case.expected.violations,
        &actual,
    )
}

/// Parse a fixture's `input:` text under `format` with dq's own reader
/// for it ([`dq_core::by_name`]), so a fixture's input is the value
/// `dq check` sees in a file: a YAML `.nan` is a number, not `null`, and
/// `18446744073709551617` keeps its digits. `markdown` gives the AST
/// (CommonMark and GFM) the `@std/markdown` rules check; `hcl` and
/// `dockerfile` give their parsers' shapes, a Dockerfile with the
/// positions its instructions start at. A YAML stream of more than one
/// document, and a format the runner does not read, yield an
/// `Err(message)` for the caller to surface as a [`TestOutcome::Error`].
fn parse_input(input: &str, format: &str) -> std::result::Result<dq_core::Document, String> {
    const SUPPORTED: [&str; 5] = ["yaml", "json", "markdown", "hcl", "dockerfile"];
    if !SUPPORTED.contains(&format) {
        return Err(format!(
            "format not yet supported in test runner: {format} (supported: yaml, json, markdown, hcl, dockerfile; add it to SUPPORTED in test_runner::parse_input if dq_core::by_name reads it)",
        ));
    }
    let parser =
        dq_core::by_name(format).ok_or_else(|| format!("{format} format not registered"))?;
    let doc = parser.parse(input.as_bytes()).map_err(|err| {
        let place = err
            .place()
            .map(|(line, col)| format!(" (line {line}, column {col})"))
            .unwrap_or_default();
        format!("{format} parse error: {err}{place}")
    })?;
    if let Some(documents) = doc.values() {
        return Err(format!(
            "{format} parse error: a fixture input holds one document, this one holds {}",
            documents.len()
        ));
    }
    Ok(doc)
}

fn compare(
    fixture: Utf8PathBuf,
    name: String,
    expected: &[ExpectedViolation],
    evaluation: &Evaluation,
) -> TestOutcome {
    let actual = &evaluation.diagnostics;
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

    let raised: Vec<String> = evaluation
        .rule_errors
        .iter()
        .map(ToString::to_string)
        .collect();

    if missing.is_empty() && extra.is_empty() && raised.is_empty() {
        TestOutcome::Pass { fixture, name }
    } else {
        TestOutcome::Fail {
            fixture,
            name,
            missing,
            extra,
            raised,
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
    fn dockerfile_input_goes_through_the_dockerfile_parser() {
        let doc = parse_input("FROM alpine\nRUN [\"sh\"]\n", "dockerfile").expect("parse");
        let value = doc.value();
        assert_eq!(
            value.to_serde_json(),
            serde_json::json!([
                {"instruction": "FROM", "arguments": "alpine", "line": 1},
                {"instruction": "RUN", "arguments": ["sh"], "line": 2},
            ])
        );
        let err = parse_input("FOO x\n", "dockerfile").expect_err("unknown instruction");
        assert!(err.contains("dockerfile parse error"), "got: {err}");
        assert!(err.ends_with(" (line 1, column 1)"), "got: {err}");
        let err = parse_input("a: 1\nя: @\n", "yaml").expect_err("no token");
        assert_eq!(
            err,
            "yaml parse error: parse error: found character that cannot start any token, while \
             scanning for the next token (line 2, column 5)"
        );
    }

    #[test]
    fn unsupported_format_in_test_case_yields_error_outcome() {
        let (_t, dir) = tempdir_utf8();
        // Use a format that the test runner does NOT have a parse arm for.
        // M11 Phase 5 wired up `hcl`; pick something we have not extended
        // yet so the negative path stays exercised.
        let fixture = r#"
tests:
  - name: unsupported format
    format: ini
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
    fn yaml_and_json_inputs_read_as_check_reads_them() {
        let doc = parse_input("x: .nan\ny: 18446744073709551617\n", "yaml").expect("yaml");
        let value = doc.value();
        let dq_core::Value::Map(map) = value else {
            panic!("expected a map: {value:?}");
        };
        let x = map.get("x").and_then(dq_core::Value::non_finite_float);
        assert!(x.is_some_and(f64::is_nan), "{value:?}");
        assert_eq!(
            map.get("y"),
            Some(&dq_core::Value::BigInt("18446744073709551617".into()))
        );
        let doc = parse_input(r#"{"y": 18446744073709551617}"#, "json").expect("json");
        assert!(matches!(doc.value(), dq_core::Value::Map(map)
            if map.get("y") == Some(&dq_core::Value::BigInt("18446744073709551617".into()))));
        let err = parse_input("a: 1\n---\nb: 2\n", "yaml").expect_err("stream");
        assert!(
            err.contains("holds one document, this one holds 2"),
            "{err}"
        );
        let err = parse_input("a: [", "yaml").expect_err("broken");
        assert!(err.starts_with("yaml parse error: "), "{err}");
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

    /// A rule that raises on a case's input fails the case even when the
    /// case expects no violation, naming the rule, the field and the jq
    /// error. Before, the runner counted the raised rule as zero
    /// violations and passed the case.
    #[test]
    fn rule_that_raises_fails_the_case() {
        let (_t, dir) = tempdir_utf8();
        let rule = r#"
id: test.spec-template
description: indexes spec as an object
severity: error
match:
  format: yaml
check:
  jq: '.spec.template | select(. == null)'
  message: no template
"#;
        let fixture = r#"
tests:
  - name: wrong-type spec
    input: |
      spec: x
    expected:
      violations: []
"#;
        write_rule_and_fixture(&dir, rule, fixture);

        let outcomes = RuleTester::run_dir(&dir).expect("run");
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            TestOutcome::Fail {
                name,
                missing,
                extra,
                raised,
                ..
            } => {
                assert_eq!(name, "wrong-type spec");
                assert!(missing.is_empty() && extra.is_empty());
                assert_eq!(
                    raised,
                    &vec![
                        "rule=test.spec-template check.jq: jq runtime error: cannot index \"x\" with \"template\""
                            .to_owned()
                    ]
                );
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn run_std_runs_the_embedded_fixtures_of_a_namespace() {
        let outcomes = RuleTester::run_std("npm").expect("known namespace");
        assert!(!outcomes.is_empty());
        for outcome in &outcomes {
            match outcome {
                TestOutcome::Pass { fixture, .. } => {
                    assert!(fixture.as_str().starts_with("@std/npm/"), "{fixture}");
                }
                other => panic!("expected Pass, got {other:?}"),
            }
        }
    }

    #[test]
    fn run_std_rejects_an_unknown_namespace_with_suggestions() {
        match RuleTester::run_std("k8z") {
            Err(ExecError::UnknownRule { id, did_you_mean }) => {
                assert_eq!(id, "@std/k8z");
                assert_eq!(did_you_mean, vec!["@std/k8s".to_owned()]);
            }
            other => panic!("expected UnknownRule, got {other:?}"),
        }
    }
}
