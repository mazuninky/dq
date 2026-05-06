//! `dq test RULES_DIR` — run rule fixture tests under a directory.
//!
//! The handler bypasses the lint-engine [`crate::output::Reporter`] trait
//! because test results have their own shape (`{ "results": [...] }`) that
//! doesn't fit the canonical `{ "diagnostics": [...] }` envelope the lint
//! reporters expect. Instead, the handler matches on `cli.format` directly
//! and writes a per-format rendering. Supported formats:
//!
//! - `Console` (default) — TAP-like one-line-per-test.
//! - `Json` — machine-readable `{"results": [...]}` envelope.
//! - `Tap` — proper TAP 13 with YAML diagnostic blocks for failures.
//!
//! Other formats produce an `InvalidInput` rejection so the user gets a
//! clear error rather than malformed output.
//!
//! Exit-code policy:
//! - Zero failures → exit 0.
//! - Any fail/error → exit 4 via [`crate::error::LintFail`].
//! - No fixtures found → exit 6 via [`crate::error::InvalidInput`].

use std::io::Write;

use dq_exec::{RuleTester, TestOutcome};

use crate::cli::{Cli, TestArgs};
use crate::error::{InvalidInput, LintFail};
use crate::output::OutputFormat;

/// Run the `test` command.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when the rules directory does not exist or
///   contains no fixtures, or when an unsupported output format is set.
/// - [`LintFail`] (exit 4) when at least one test outcome is `Fail` or
///   `Error`.
/// - `dq_exec::ExecError` when the directory walk itself fails.
pub fn run(cli: &Cli, args: &TestArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    if !args.rules_dir.as_std_path().is_dir() {
        return Err(anyhow::Error::new(InvalidInput::new(format!(
            "rules directory does not exist or is not a directory: {dir}",
            dir = args.rules_dir,
        ))));
    }

    let outcomes = RuleTester::run_dir(&args.rules_dir).map_err(anyhow::Error::new)?;

    if outcomes.is_empty() {
        return Err(anyhow::Error::new(InvalidInput::new(format!(
            "no test fixtures under {dir}",
            dir = args.rules_dir,
        ))));
    }

    match cli.format {
        OutputFormat::Console => render_console(&outcomes, out)?,
        OutputFormat::Json => render_json(&outcomes, out)?,
        OutputFormat::Tap => render_tap(&outcomes, out)?,
        other => {
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "format '{other:?}' is not supported by `dq test` (use console / json / tap)"
            ))));
        }
    }

    let failures = outcomes
        .iter()
        .filter(|o| !matches!(o, TestOutcome::Pass { .. }))
        .count();
    if failures > 0 {
        return Err(anyhow::Error::new(LintFail { count: failures }));
    }
    Ok(())
}

fn render_console(outcomes: &[TestOutcome], out: &mut dyn Write) -> std::io::Result<()> {
    let total = outcomes.len();
    let mut pass = 0;
    let mut fail = 0;
    let mut errors = 0;
    for outcome in outcomes {
        match outcome {
            TestOutcome::Pass { fixture, name } => {
                pass += 1;
                writeln!(out, "ok {fixture}: {name}")?;
            }
            TestOutcome::Fail {
                fixture,
                name,
                missing,
                extra,
            } => {
                fail += 1;
                writeln!(out, "FAIL {fixture}: {name}")?;
                for m in missing {
                    writeln!(out, "  missing: {m}")?;
                }
                for e in extra {
                    writeln!(out, "  extra: {e}")?;
                }
            }
            TestOutcome::Error {
                fixture,
                name,
                error,
            } => {
                errors += 1;
                writeln!(out, "ERROR {fixture}: {name} ({error})")?;
            }
        }
    }
    writeln!(
        out,
        "summary: {pass} passed, {fail} failed, {errors} errored, {total} total"
    )?;
    Ok(())
}

fn render_json(outcomes: &[TestOutcome], out: &mut dyn Write) -> anyhow::Result<()> {
    let arr: Vec<serde_json::Value> = outcomes.iter().map(outcome_to_json).collect();
    let envelope = serde_json::json!({ "results": arr });
    serde_json::to_writer_pretty(&mut *out, &envelope)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn outcome_to_json(outcome: &TestOutcome) -> serde_json::Value {
    match outcome {
        TestOutcome::Pass { fixture, name } => serde_json::json!({
            "fixture": fixture.as_str(),
            "name": name,
            "outcome": "pass",
        }),
        TestOutcome::Fail {
            fixture,
            name,
            missing,
            extra,
        } => serde_json::json!({
            "fixture": fixture.as_str(),
            "name": name,
            "outcome": "fail",
            "missing": missing,
            "extra": extra,
        }),
        TestOutcome::Error {
            fixture,
            name,
            error,
        } => serde_json::json!({
            "fixture": fixture.as_str(),
            "name": name,
            "outcome": "error",
            "error": error,
        }),
    }
}

fn render_tap(outcomes: &[TestOutcome], out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "TAP version 13")?;
    writeln!(out, "1..{}", outcomes.len())?;
    for (i, outcome) in outcomes.iter().enumerate() {
        let n = i + 1;
        match outcome {
            TestOutcome::Pass { fixture, name } => {
                writeln!(out, "ok {n} - {fixture}: {name}")?;
            }
            TestOutcome::Fail {
                fixture,
                name,
                missing,
                extra,
            } => {
                writeln!(out, "not ok {n} - {fixture}: {name}")?;
                writeln!(out, "  ---")?;
                if !missing.is_empty() {
                    writeln!(out, "  missing:")?;
                    for m in missing {
                        writeln!(out, "    - {m}")?;
                    }
                }
                if !extra.is_empty() {
                    writeln!(out, "  extra:")?;
                    for e in extra {
                        writeln!(out, "    - {e}")?;
                    }
                }
                writeln!(out, "  ...")?;
            }
            TestOutcome::Error {
                fixture,
                name,
                error,
            } => {
                writeln!(out, "not ok {n} - {fixture}: {name} # ERROR")?;
                writeln!(out, "  ---")?;
                writeln!(out, "  error: {error}")?;
                writeln!(out, "  ...")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use clap::Parser;
    use tempfile::TempDir;

    fn cli_no_flags() -> Cli {
        Cli::try_parse_from(["dq", "test", "."]).expect("clap parse")
    }

    fn write_passing_fixture(dir: &Utf8PathBuf) {
        let rule_yaml = r#"
id: test.name-not-empty
description: name must not be empty
severity: error
match:
  format: yaml
check:
  jq: 'select(.name == "") | .'
  message: 'name is empty'
"#;
        let fixture_yaml = r#"
tests:
  - name: empty fires
    input: |
      name: ""
    expected:
      violations:
        - rule: test.name-not-empty
"#;
        std::fs::write(dir.join("name-not-empty.yml"), rule_yaml).expect("write rule");
        std::fs::write(dir.join("name-not-empty.test.yml"), fixture_yaml).expect("write fixture");
    }

    fn write_failing_fixture(dir: &Utf8PathBuf) {
        let rule_yaml = r#"
id: test.name-not-empty
description: x
severity: error
match:
  format: yaml
check:
  jq: 'select(.name == "") | .'
  message: 'name is empty'
"#;
        // expected fires but input doesn't trigger
        let fixture_yaml = r#"
tests:
  - name: rule should fire but does not
    input: |
      name: "ok"
    expected:
      violations:
        - rule: test.name-not-empty
"#;
        std::fs::write(dir.join("name-not-empty.yml"), rule_yaml).expect("write rule");
        std::fs::write(dir.join("name-not-empty.test.yml"), fixture_yaml).expect("write fixture");
    }

    #[test]
    fn missing_rules_dir_returns_invalid_input() {
        let cli = cli_no_flags();
        let args = TestArgs {
            rules_dir: Utf8PathBuf::from("/nope/totally-missing"),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, &mut out).expect_err("missing dir must error");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }

    #[test]
    fn empty_rules_dir_returns_invalid_input_no_fixtures() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 path");
        let cli = cli_no_flags();
        let args = TestArgs { rules_dir: dir };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, &mut out).expect_err("empty dir must error");
        let invalid = err
            .downcast_ref::<InvalidInput>()
            .expect("empty dir is InvalidInput");
        assert!(format!("{invalid}").contains("no test fixtures"));
    }

    #[test]
    fn passing_fixture_returns_ok_and_writes_summary() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 path");
        write_passing_fixture(&dir);

        let cli = cli_no_flags();
        let args = TestArgs { rules_dir: dir };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, &mut out).expect("passing fixture must return Ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("ok "),
            "console output should mark the pass: {s}"
        );
        assert!(s.contains("summary:"));
    }

    #[test]
    fn failing_fixture_returns_lint_fail() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 path");
        write_failing_fixture(&dir);

        let cli = cli_no_flags();
        let args = TestArgs { rules_dir: dir };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, &mut out).expect_err("failing fixture must error");
        let fail = err
            .downcast_ref::<LintFail>()
            .expect("failing fixture must produce LintFail");
        assert!(fail.count >= 1);
    }

    #[test]
    fn json_format_emits_results_envelope() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 path");
        write_passing_fixture(&dir);

        let cli = Cli::try_parse_from(["dq", "-F", "json", "test", dir.as_str()]).expect("parse");
        let args = TestArgs { rules_dir: dir };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, &mut out).expect("json render must succeed");
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid json");
        assert!(parsed["results"].is_array());
        assert_eq!(parsed["results"][0]["outcome"], "pass");
    }

    #[test]
    fn unsupported_format_returns_invalid_input() {
        let temp = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8 path");
        write_passing_fixture(&dir);

        // YAML output is not supported by `dq test` — only console/json/tap.
        let cli = Cli::try_parse_from(["dq", "-F", "yaml", "test", dir.as_str()]).expect("parse");
        let args = TestArgs { rules_dir: dir };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, &mut out).expect_err("yaml format must be rejected");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
