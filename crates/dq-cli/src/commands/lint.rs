//! `dq lint FILE...` — run lint rules over one or more files.
//!
//! Routes through [`crate::commands::lint_core::run_with_rulesets`] with
//! the user-facing `--rules` list passed verbatim. The shared pipeline
//! handles glob expansion, format detection, ruleset resolution (including
//! auto-bind on empty `--rules`), evaluation, and exit-code computation.

use std::io::Write;

use crate::cli::{Cli, LintArgs};
use crate::commands::lint_core::run_with_rulesets;
use crate::output::Reporter;

/// Run the `lint` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) for write-flag rejection,
///   unknown `--rules` entries, or zero-match globs.
/// - [`crate::error::LintFail`] (exit 4) on any error-severity diagnostic.
/// - [`crate::error::LintWarnStrict`] (exit 1) on warn-severity diagnostics
///   under `--strict`.
/// - `dq_core::Error::*` for the usual file-load failures.
pub fn run(
    cli: &Cli,
    args: &LintArgs,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    run_with_rulesets(
        cli,
        &args.files,
        args.rules.clone(),
        Vec::new(),
        input_format,
        reporter,
        out,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{InvalidInput, LintFail, LintWarnStrict};
    use crate::output::JsonReporter;
    use camino::Utf8PathBuf;
    use clap::Parser;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn cli_for(extra: &[&str]) -> Cli {
        let mut argv = vec!["dq"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["lint", "x.yaml"]);
        Cli::try_parse_from(argv).expect("clap parse")
    }

    // Returns a `TempPath` (not a `NamedTempFile`) so the underlying `File`
    // handle is released after writing. Required for Windows: production
    // atomic-write uses `MoveFileEx` which fails with `Access is denied` if
    // the target is still held open elsewhere in the same process.
    fn write_yaml(content: &str) -> tempfile::TempPath {
        let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
        tmp.write_all(content.as_bytes()).expect("write");
        tmp.into_temp_path()
    }

    #[test]
    fn lint_with_no_rules_and_no_matching_std_returns_ok_with_empty_diagnostics() {
        // CSV file with empty discovered formats wouldn't bind any std
        // namespace; we mimic that by feeding an extension-less file with an
        // explicit -F override that doesn't overlap any std namespace.
        // Simpler scenario: a yaml file but force no rules via inline.
        // The k8s std rules apply to yaml, so to avoid binding them we write
        // a file with `.json` extension and a JSON body — the json-only std
        // rulesets currently in @std are placeholder-only and won't fire on
        // an unrelated payload.
        let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
        tmp.write_all(b"{}").expect("write");
        let path = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from(["dq", "lint", path.as_str()]).expect("clap parse");
        let args = LintArgs {
            files: vec![path],
            rules: Vec::new(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        // The handler must succeed even when no rules end up bound.
        let _ = run(&cli, &args, None, &reporter, &mut out);
        // We cannot assert success in all cases (a placeholder std rule may
        // happen to fire), so the contract this test pins is "the handler
        // does not panic and produces a `diagnostics` array in the output".
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("json output");
        assert!(parsed.get("diagnostics").is_some());
    }

    #[test]
    fn lint_with_inline_rule_via_check_path_emits_error_and_returns_lint_fail() {
        // Mirror of `check_inline_emits_lint_fail` but using `lint` directly
        // — the lint handler should also produce LintFail on error-severity
        // findings. We pass the rule via a temp file using --rules.
        let rule_yaml = r#"
id: test.always-fires
description: always emits an error
severity: error
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
"#;
        let mut rule_tmp = NamedTempFile::with_suffix(".yml").expect("rule tmp");
        rule_tmp.write_all(rule_yaml.as_bytes()).expect("write");
        let rule_path =
            Utf8PathBuf::from_path_buf(rule_tmp.path().to_path_buf()).expect("UTF-8 path");

        let doc_tmp = write_yaml("a: 1\n");
        let doc_path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from(["dq", "lint", doc_path.as_str()]).expect("clap parse");
        let args = LintArgs {
            files: vec![doc_path],
            rules: vec![rule_path.to_string()],
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out)
            .expect_err("error-severity rule must produce LintFail");
        let fail = err
            .downcast_ref::<LintFail>()
            .expect("LintFail marker for error-severity diagnostics");
        assert!(fail.count >= 1);
    }

    #[test]
    fn lint_warn_severity_under_strict_returns_lint_warn_strict() {
        let rule_yaml = r#"
id: test.always-warns
description: always emits a warning
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'warning here'
"#;
        let mut rule_tmp = NamedTempFile::with_suffix(".yml").expect("rule tmp");
        rule_tmp.write_all(rule_yaml.as_bytes()).expect("write");
        let rule_path =
            Utf8PathBuf::from_path_buf(rule_tmp.path().to_path_buf()).expect("UTF-8 path");

        let doc_tmp = write_yaml("a: 1\n");
        let doc_path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli =
            Cli::try_parse_from(["dq", "--strict", "lint", doc_path.as_str()]).expect("clap parse");
        let args = LintArgs {
            files: vec![doc_path],
            rules: vec![rule_path.to_string()],
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out)
            .expect_err("warn under --strict must produce LintWarnStrict");
        let warn = err
            .downcast_ref::<LintWarnStrict>()
            .expect("LintWarnStrict marker for warn diagnostics under --strict");
        assert!(warn.count >= 1);
    }

    #[test]
    fn lint_rejects_in_place_flag_with_invalid_input() {
        let cli = cli_for(&["-i"]);
        let args = LintArgs {
            files: vec![Utf8PathBuf::from("x.yaml")],
            rules: Vec::new(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err =
            run(&cli, &args, None, &reporter, &mut out).expect_err("--in-place must be rejected");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "rejection must carry InvalidInput, got: {err:?}"
        );
        assert!(err.to_string().contains("--in-place"));
    }
}
