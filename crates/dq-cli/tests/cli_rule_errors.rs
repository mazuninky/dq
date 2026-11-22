//! A rule whose jq raises at run time (`.spec.template` on `spec: x`).
//!
//! `dq lint` skips such a rule for the file, and "no findings" would mean
//! "not checked": the other rules and files are still checked, the rule
//! error is a `lint.rule_error` result and a line on stderr, and the run
//! exits 3. `dq fix` does the same for a rule whose fix it skipped.
//! `--allow-rule-errors` is the behaviour before that: one warning per rule
//! on stderr, the exit code unchanged. `dq test` fails the case, naming the
//! rule, the stage that raised and the jq error.

use assert_cmd::Command;
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

/// A rule that indexes `.spec` as an object in `check.jq`.
const RAISES_IN_CHECK: &str = r#"id: local.raises-in-check
description: indexes spec as an object
severity: error
match:
  format: yaml
check:
  jq: '.spec.template | select(. == null)'
  message: no template
"#;

/// A rule that indexes `.spec` as an object in `match.filter`.
const RAISES_IN_FILTER: &str = r#"id: local.raises-in-filter
description: indexes spec as an object in its filter
severity: error
match:
  format: yaml
  filter: '.spec.containers != null'
check:
  jq: '.spec.containers[] | select(.image == null)'
  message: no image
"#;

const WRONG_TYPE_SPEC: &str = "kind: Pod\nspec: x\n";

/// Drop the leading timestamp of every tracing line on stderr.
fn without_timestamps(stderr: &[u8]) -> String {
    String::from_utf8(stderr.to_vec())
        .expect("UTF-8 stderr")
        .lines()
        .map(|line| line.split_once(' ').map_or(line, |(_, rest)| rest))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// The stderr of a run on which only [`RAISES_IN_CHECK`] raised, on
/// `pod.yaml`.
const CHECK_RAISED: &str = concat!(
    "ERROR rule did not run rule_id=local.raises-in-check file=pod.yaml field=check.jq",
    " error=jq runtime error: cannot index \"x\" with \"template\"\n",
    "1 rule error(s): findings may be missing (--allow-rule-errors to accept)\n",
);

#[test]
fn lint_fails_when_a_rule_raises_and_still_reports_the_others() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("check.yml"), RAISES_IN_CHECK).expect("write rule");
    std::fs::write(dir.path().join("fires.yml"), FIRES_WITH_AWKWARD_MESSAGE).expect("write rule");
    std::fs::write(dir.path().join("pod.yaml"), WRONG_TYPE_SPEC).expect("write input");
    let lint = |flags: &[&str]| {
        dq().current_dir(dir.path())
            .arg("--no-color")
            .args(flags)
            .args(["lint", "--rules", "check.yml", "--rules", "fires.yml"])
            .arg("pod.yaml")
            .assert()
            .code(3)
    };
    // `-q` names the rule error too; the findings (exit 4) do not win.
    for flags in [&[][..], &["-q"], &["--strict"]] {
        let out = lint(flags);
        assert_eq!(rule_errors(&out.get_output().stderr), CHECK_RAISED);
        let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
        assert!(
            stdout.contains("local.fires") && !stdout.contains("lint.rule_error"),
            "{stdout}"
        );
    }
    let out = lint(&["-F", "json"]);
    let report: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("JSON stdout");
    let results = report["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0]["rule_id"], "local.fires");
    assert_eq!(
        results[1],
        serde_json::json!({
            "path": "pod.yaml",
            "line": 1,
            "col": 1,
            "message": "rule 'local.raises-in-check' did not run: check.jq: jq runtime error: cannot index \"x\" with \"template\"",
            "severity": "error",
            "rule_id": "lint.rule_error",
            "references": [],
        })
    );
    let out = lint(&["-F", "sarif"]);
    let sarif: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("SARIF stdout");
    assert_eq!(
        sarif["runs"][0]["tool"]["driver"]["rules"][1],
        serde_json::json!({
            "id": "lint.rule_error",
            "shortDescription": {
                "text": "A rule raised a jq error and did not run on the file: its findings may be missing.",
            },
        })
    );
}

/// stderr with the timestamps dropped from its log lines.
fn rule_errors(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(|line| match line.split_once("Z ERROR ") {
            Some((_, rest)) => format!("ERROR {rest}\n"),
            None => format!("{line}\n"),
        })
        .collect()
}

/// A fix rule that indexes `.spec` as an object in `check.jq`.
const FIX_RAISES: &str = r#"id: local.fix-raises
description: fix of a rule whose check raises
severity: error
match:
  format: yaml
check:
  jq: '.spec.template | select(. == null)'
  message: no template
fix:
  ops: '[{"op":"add","path":"/x","value":1}]'
"#;

/// A rule whose fix sets `fixed: true`.
const FIX_SETS_FIXED: &str = r#"id: local.set-fixed
description: sets fixed
severity: warn
match:
  format: yaml
check:
  jq: 'select(.fixed != true)'
  message: not fixed
fix:
  ops: 'if .fixed == true then [] else [{"op":"add","path":"/fixed","value":true}] end'
"#;

/// A directory with both fix rules and one file per `spec: xN`.
fn stage_fix(files: usize) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("raises.yml"), FIX_RAISES).expect("write rule");
    std::fs::write(dir.path().join("fixes.yml"), FIX_SETS_FIXED).expect("write rule");
    for n in 0..files {
        std::fs::write(
            dir.path().join(format!("f{n}.yaml")),
            format!("spec: x{n}\n"),
        )
        .expect("write input");
    }
    dir
}

const FIX_RULES: [&str; 5] = ["fix", "--rules", "raises.yml", "--rules", "fixes.yml"];

#[test]
fn fix_fails_when_a_rule_raises_and_still_writes_the_other_fixes() {
    let dir = stage_fix(8);
    let run = |flags: &[&str]| {
        dq().current_dir(dir.path())
            .timeout(std::time::Duration::from_secs(60))
            .arg("--no-color")
            .args(flags)
            .args(FIX_RULES)
            .arg("f*.yaml")
            .assert()
    };
    let raised: String = (0..8)
        .map(|n| {
            format!(
                "ERROR rule did not run rule_id=local.fix-raises file=f{n}.yaml field=check.jq \
                 error=jq runtime error: cannot index \"x{n}\" with \"template\"\n"
            )
        })
        .chain([
            "8 rule error(s): findings may be missing (--allow-rule-errors to accept)\n".to_owned(),
        ])
        .collect();
    // Pending changes (exit 1) do not hide a rule that did not run.
    let out = run(&["--check"]).code(3);
    assert_eq!(rule_errors(&out.get_output().stderr), raised);
    // In file order, whatever order the workers finish in.
    let out = run(&["-i", "--parallel", "4"]).code(3);
    assert_eq!(rule_errors(&out.get_output().stderr), raised);
    for n in 0..8 {
        assert_eq!(
            std::fs::read_to_string(dir.path().join(format!("f{n}.yaml"))).expect("read"),
            format!("spec: x{n}\nfixed: true\n")
        );
    }
    // Nothing left to fix, and the rule still did not run.
    run(&["--check"]).code(3);
}

#[test]
fn fix_allows_rule_errors_and_logs_from_parallel_workers() {
    let dir = stage_fix(4);
    let run = |flags: &[&str]| {
        dq().current_dir(dir.path())
            // The workers' warnings used to wait for the stderr lock the
            // main thread held while it waited for the workers.
            .timeout(std::time::Duration::from_secs(60))
            .arg("--no-color")
            .args(flags)
            .args(FIX_RULES)
            .args(["--allow-rule-errors", "f*.yaml"])
            .assert()
    };
    let out = run(&["-i", "--parallel", "4"]).success();
    let mut lines: Vec<String> = warnings(&out.get_output().stderr)
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    let expected: Vec<String> = (0..4)
        .map(|n| {
            format!(
                " WARN check.jq raised a runtime error before fix; skipping rule \
                 rule_id=local.fix-raises \
                 error=jq runtime error: cannot index \"x{n}\" with \"template\""
            )
        })
        .collect();
    assert_eq!(lines, expected);
    run(&["--check"]).success();
}

#[test]
fn fix_keeps_exit_7_when_a_file_failed_and_a_rule_raised() {
    let dir = stage_fix(1);
    std::fs::write(dir.path().join("broken.yaml"), "a: [\n").expect("write input");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "-i", "--continue-on-error"])
        .args(FIX_RULES)
        .args(["f0.yaml", "broken.yaml"])
        .assert()
        .code(7);
    let stderr = rule_errors(&out.get_output().stderr);
    assert!(
        stderr.starts_with("ERROR rule did not run rule_id=local.fix-raises file=f0.yaml")
            && stderr.ends_with("1 file(s) failed in bulk run\n"),
        "{stderr}"
    );
}

#[test]
fn a_loc_expression_that_raises_stays_a_warning() {
    let dir = TempDir::new().expect("tempdir");
    let rule = "id: t.loc\ndescription: d\nseverity: warn\nmatch:\n  format: yaml\ncheck:\n  jq: '.'\n  message: m\nloc:\n  file: '.kind.x'\n  line: '.kind.x'\n";
    std::fs::write(dir.path().join("loc.yml"), rule).expect("write rule");
    std::fs::write(dir.path().join("pod.yaml"), WRONG_TYPE_SPEC).expect("write input");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "lint", "--rules", "loc.yml", "pod.yaml"])
        .assert()
        .success();
    assert_eq!(
        without_timestamps(&out.get_output().stderr),
        concat!(
            " WARN loc.file raised a runtime error; falling back to file under check",
            " rule_id=t.loc error=jq runtime error: cannot index \"Pod\" with \"x\"\n",
            " WARN loc.line raised a runtime error; defaulting to line 1",
            " rule_id=t.loc error=jq runtime error: cannot index \"Pod\" with \"x\"\n",
        )
    );
}

#[test]
fn a_stream_and_a_nested_rule_give_one_result_per_rule_file_and_error() {
    let dir = TempDir::new().expect("tempdir");
    // Runs `RAISES_IN_CHECK`, nested, on `spec: x` once per document.
    let nested: String = RAISES_IN_CHECK
        .lines()
        .map(|line| format!("    {line}\n"))
        .collect();
    let composite = format!(
        "id: t.outer\ndescription: d\nseverity: error\nmatch:\n  format: yaml\ncheck:\n  extract: '[{{value: \"spec: x\", format: \"yaml\", anchor: \"\"}}]'\n  message: outer\n  nested:\n{nested}"
    );
    std::fs::write(dir.path().join("composite.yml"), composite).expect("write rule");
    std::fs::write(dir.path().join("pod.yaml"), "a: 1\n---\na: 2\n").expect("write input");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "lint", "--rules", "composite.yml", "pod.yaml"])
        .assert()
        .code(3)
        .stdout("");
    assert_eq!(rule_errors(&out.get_output().stderr), CHECK_RAISED);
}

#[test]
fn a_rule_source_that_can_check_nothing_is_an_error() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("pod.yaml"), WRONG_TYPE_SPEC).expect("write input");
    std::fs::create_dir_all(dir.path().join(".dq/rules")).expect("mkdir");
    std::fs::create_dir(dir.path().join("only-tests")).expect("mkdir");
    std::fs::write(dir.path().join("only-tests/a.test.yml"), "tests: []\n").expect("write");
    let bad = RAISES_IN_CHECK.replace("format: yaml", "format: [jsn]");
    std::fs::write(dir.path().join("bad.yml"), &bad).expect("write rule");
    let run = |args: &[&str]| dq().current_dir(dir.path()).args(args).assert();

    for command in [["lint", "--rules"], ["check", "--rule"], ["fix", "--rules"]] {
        let [name, flag] = command;
        run(&[name, flag, "only-tests", "pod.yaml"])
            .code(6)
            .stdout("")
            .stderr("no rules in 'only-tests'\n");
        run(&[name, flag, "bad.yml", "pod.yaml"])
            .code(3)
            .stdout("")
            .stderr(
                "rule local.raises-in-check has unknown format 'jsn' in match.format \
                 (did you mean: json, csv, jsonl)\n",
            );
    }
    run(&["check", "--inline", &bad, "pod.yaml"]).code(3);
    // Found automatically, `.dq/rules/` may be empty.
    run(&["lint", "pod.yaml"]).success();
    std::fs::write(dir.path().join(".dq/rules/bad.yml"), &bad).expect("write rule");
    run(&["lint", "pod.yaml"]).code(3);
}

#[test]
fn a_rule_without_a_format_is_an_error() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("pod.yaml"), WRONG_TYPE_SPEC).expect("write input");
    let rule = RAISES_IN_CHECK.replace("format: yaml", "format: []");
    std::fs::write(dir.path().join("rule.yml"), rule).expect("write rule");
    dq().current_dir(dir.path())
        .args(["lint", "--rules", "rule.yml", "pod.yaml"])
        .assert()
        .code(3)
        .stdout("")
        .stderr("rule local.raises-in-check: match.format lists no format\n");
}

/// The rules of a directory that are there under another name: links, and
/// the extension in another case.
#[cfg(unix)]
mod rule_directories {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A directory with `FIRES_WITH_AWKWARD_MESSAGE` and a fixture of it in
    /// `src/`, and a file it reports.
    fn stage() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir");
        let write = |name: &str, text: &str| {
            std::fs::write(dir.path().join(name), text).expect("write");
        };
        write("src/fires.yml", FIRES_WITH_AWKWARD_MESSAGE);
        write(
            "src/fires.test.yml",
            "tests:\n  - name: pod\n    input: 'kind: Pod'\n    expected:\n      violations:\n        - rule: local.fires\n",
        );
        write("pod.yaml", "kind: Pod\n");
        dir
    }

    /// The rule ids `dq -F json lint ARGS pod.yaml` reports, without `@std`.
    fn found(dir: &TempDir, args: &[&str]) -> Vec<String> {
        let out = dq()
            .current_dir(dir.path())
            .args(["-F", "json", "lint"])
            .args(args)
            .arg("pod.yaml")
            .assert()
            .code(4);
        let report: serde_json::Value =
            serde_json::from_slice(&out.get_output().stdout).expect("JSON stdout");
        report["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .filter_map(|d| d["rule_id"].as_str())
            .filter(|id| id.starts_with("local."))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn rules_add_symlink_adds_rules_that_run() {
        for source in ["src/fires.yml", "src"] {
            let dir = stage();
            dq().current_dir(dir.path())
                .args(["rules", "add", source, "--symlink"])
                .assert()
                .success();
            assert_eq!(found(&dir, &[]), ["local.fires"], "{source}");
        }
    }

    #[test]
    fn rules_add_of_a_directory_takes_its_links_and_any_extension() {
        let dir = stage();
        std::fs::create_dir(dir.path().join("set")).expect("mkdir");
        symlink("../src", dir.path().join("set/linked")).expect("link");
        std::fs::rename(
            dir.path().join("src/fires.yml"),
            dir.path().join("src/fires.YAML"),
        )
        .expect("rename");
        dq().current_dir(dir.path())
            .args(["rules", "add", "set"])
            .assert()
            .success();
        assert_eq!(found(&dir, &[]), ["local.fires"]);
    }

    #[test]
    fn a_linked_directory_is_loaded_and_tested_once() {
        let dir = stage();
        std::fs::create_dir(dir.path().join("set")).expect("mkdir");
        for link in ["a", "b"] {
            symlink("../src", dir.path().join("set").join(link)).expect("link");
        }
        symlink("../src", dir.path().join("src/loop")).expect("link");
        assert_eq!(found(&dir, &["--rules", "set"]), ["local.fires"]);
        dq().current_dir(dir.path())
            .args(["--no-color", "test", "set"])
            .assert()
            .success()
            .stdout(concat!(
                "ok set/a/fires.test.yml: pod\n",
                "summary: 1 passed, 0 failed, 0 errored, 1 total\n",
            ));
    }

    #[test]
    fn a_link_to_nothing_is_an_error() {
        let dir = stage();
        symlink("../gone.yml", dir.path().join("src/gone.yml")).expect("link");
        let run = |args: &[&str]| dq().current_dir(dir.path()).args(args).assert();
        for args in [
            &["lint", "--rules", "src", "pod.yaml"][..],
            &["fix", "--check", "--rules", "src", "pod.yaml"],
            &["test", "src"],
        ] {
            let out = run(args).code(5).stdout("");
            let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
            assert!(
                stderr.contains("src/gone.yml: a link to '../gone.yml', where nothing is"),
                "{stderr}"
            );
        }
        // `.dq/rules` itself.
        std::fs::create_dir(dir.path().join(".dq")).expect("mkdir");
        symlink("../gone", dir.path().join(".dq/rules")).expect("link");
        run(&["lint", "pod.yaml"]).code(5).stdout("");
    }

    #[test]
    fn a_linked_schema_file_outside_the_rule_directory_is_refused() {
        let dir = stage();
        std::fs::create_dir(dir.path().join("set")).expect("mkdir");
        std::fs::write(dir.path().join("out.json"), "{}").expect("write schema");
        std::fs::write(
            dir.path().join("schema.yml"),
            "id: t.schema\ndescription: d\nseverity: error\nmatch:\n  format: yaml\ncheck:\n  schema_file: s.json\n",
        )
        .expect("write rule");
        symlink("../schema.yml", dir.path().join("set/schema.yml")).expect("link");
        symlink("../out.json", dir.path().join("set/s.json")).expect("link");
        dq().current_dir(dir.path())
            .args(["lint", "--rules", "set", "pod.yaml"])
            .assert()
            .code(3)
            .stderr("rule t.schema schema_file escapes rule directory: s.json\n");
    }
}

#[test]
fn lint_allow_rule_errors_keeps_warning_and_exit_code_when_a_rule_raises() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("check.yml"), RAISES_IN_CHECK).expect("write rule");
    std::fs::write(dir.path().join("filter.yml"), RAISES_IN_FILTER).expect("write rule");
    std::fs::write(dir.path().join("pod.yaml"), WRONG_TYPE_SPEC).expect("write input");
    let assert = dq()
        .current_dir(dir.path())
        .args([
            "--no-color",
            "lint",
            "--allow-rule-errors",
            "--rules",
            "check.yml",
            "--rules",
            "filter.yml",
            "pod.yaml",
        ])
        .assert()
        .success()
        .stdout("diagnostics: []\n");
    assert_eq!(
        without_timestamps(&assert.get_output().stderr),
        concat!(
            " WARN check.jq raised a runtime error; skipping rule for this file",
            " rule_id=local.raises-in-check",
            " error=jq runtime error: cannot index \"x\" with \"template\"\n",
            " WARN match.filter raised a runtime error; skipping rule for this file",
            " rule_id=local.raises-in-filter",
            " error=jq runtime error: cannot index \"x\" with \"containers\"\n",
        )
    );
}

/// The `WARN` lines of stderr, timestamps dropped.
fn warnings(stderr: &[u8]) -> String {
    without_timestamps(stderr)
        .lines()
        .filter(|line| line.starts_with(" WARN "))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Stage one rule and a fixture whose single case expects no violation
/// on `spec: x`.
fn stage_raising_fixture(rule: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("rule.yml"), rule).expect("write rule");
    std::fs::write(
        dir.path().join("rule.test.yml"),
        "tests:\n  - name: wrong-type spec\n    input: |\n      kind: Pod\n      spec: x\n    expected:\n      violations: []\n",
    )
    .expect("write fixture");
    dir
}

#[test]
fn test_fails_a_case_whose_check_raises() {
    let dir = stage_raising_fixture(RAISES_IN_CHECK);
    dq().current_dir(dir.path())
        .args(["--no-color", "test", "."])
        .assert()
        .code(4)
        .stdout(concat!(
            "FAIL ./rule.test.yml: wrong-type spec\n",
            "  raised: rule=local.raises-in-check check.jq: jq runtime error: cannot index \"x\" with \"template\"\n",
            "summary: 0 passed, 1 failed, 0 errored, 1 total\n",
        ))
        .stderr("lint reported 1 error-severity diagnostic(s)\n");
}

#[test]
fn test_fails_a_case_whose_filter_raises() {
    let dir = stage_raising_fixture(RAISES_IN_FILTER);
    dq().current_dir(dir.path())
        .args(["--no-color", "test", "."])
        .assert()
        .code(4)
        .stdout(concat!(
            "FAIL ./rule.test.yml: wrong-type spec\n",
            "  raised: rule=local.raises-in-filter match.filter: jq runtime error: cannot index \"x\" with \"containers\"\n",
            "summary: 0 passed, 1 failed, 0 errored, 1 total\n",
        ));
}

#[test]
fn test_json_and_tap_name_the_raised_rule() {
    let dir = stage_raising_fixture(RAISES_IN_CHECK);
    let assert = dq()
        .current_dir(dir.path())
        .args(["--no-color", "-F", "json", "test", "."])
        .assert()
        .code(4);
    let json: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("JSON stdout");
    assert_eq!(
        json,
        serde_json::json!({"results": [{
            "fixture": "./rule.test.yml",
            "name": "wrong-type spec",
            "outcome": "fail",
            "missing": [],
            "extra": [],
            "raised": [
                "rule=local.raises-in-check check.jq: jq runtime error: cannot index \"x\" with \"template\"",
            ],
        }]})
    );
    dq().current_dir(dir.path())
        .args(["--no-color", "-F", "tap", "test", "."])
        .assert()
        .code(4)
        .stdout(concat!(
            "TAP version 13\n",
            "1..1\n",
            "not ok 1 - ./rule.test.yml: wrong-type spec\n",
            "  ---\n",
            "  raised:\n",
            "    - \"rule=local.raises-in-check check.jq: jq runtime error: cannot index \\\"x\\\" with \\\"template\\\"\"\n",
            "  ...\n",
        ));
}

/// A rule that fires on `kind: Pod` with a message holding `: ` and `"`.
const FIRES_WITH_AWKWARD_MESSAGE: &str = r#"id: local.fires
description: fires on every pod
severity: error
match:
  format: yaml
check:
  jq: 'select(.kind == "Pod") | .'
  message: 'broken: "pod" # not - fine'
"#;

/// The `---` .. `...` YAML block of every `not ok` entry of a TAP stream.
fn tap_yaml_blocks(tap: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in tap.lines() {
        match (line, current.as_mut()) {
            ("  ---", None) => current = Some(String::new()),
            ("  ...", Some(_)) => blocks.extend(current.take()),
            (_, Some(block)) => {
                block.push_str(line);
                block.push('\n');
            }
            _ => {}
        }
    }
    blocks
}

#[test]
fn test_tap_yaml_blocks_parse_back_to_the_json_lists() {
    let dir = TempDir::new().expect("tempdir");
    let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).expect(name);
    // One case whose rule raises, one whose expected and actual
    // violations differ: every list item holds `: ` and `"`.
    write("a-raises.yml", RAISES_IN_CHECK);
    write(
        "a-raises.test.yml",
        "tests:\n  - name: wrong-type spec\n    input: |\n      kind: Pod\n      spec: x\n    expected:\n      violations: []\n",
    );
    write("b-fires.yml", FIRES_WITH_AWKWARD_MESSAGE);
    write(
        "b-fires.test.yml",
        "tests:\n  - name: other message\n    input: |\n      kind: Pod\n    expected:\n      violations:\n        - rule: local.fires\n          message_equals: 'other: \"msg\"'\n",
    );

    let json_out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "-F", "json", "test", "."])
        .assert()
        .code(4);
    let json: serde_json::Value =
        serde_json::from_slice(&json_out.get_output().stdout).expect("JSON stdout");
    let results = json["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);

    let tap_out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "-F", "tap", "test", "."])
        .assert()
        .code(4);
    let tap = String::from_utf8(tap_out.get_output().stdout.clone()).expect("UTF-8 TAP");
    let blocks = tap_yaml_blocks(&tap);
    assert_eq!(blocks.len(), 2, "one YAML block per failed case:\n{tap}");

    for (i, (block, result)) in blocks.iter().zip(results).enumerate() {
        let path = dir.path().join(format!("block{i}.yaml"));
        std::fs::write(&path, block).expect("write block");
        let parsed = dq()
            .current_dir(dir.path())
            .args([
                "--no-color",
                "convert",
                "-F",
                "json",
                path.to_str().unwrap(),
            ])
            .assert()
            .success();
        let parsed: serde_json::Value =
            serde_json::from_slice(&parsed.get_output().stdout).expect("JSON stdout");
        for key in ["missing", "extra", "raised"] {
            // The block omits an empty list.
            let want = result[key].clone();
            let got = parsed.get(key).cloned().unwrap_or(serde_json::json!([]));
            assert_eq!(got, want, "`{key}` of block {i}:\n{block}");
        }
    }
    let awkward = results.iter().flat_map(|r| {
        ["missing", "extra", "raised"]
            .into_iter()
            .flat_map(|k| r[k].as_array().cloned().unwrap_or_default())
    });
    assert!(
        awkward
            .filter(|s| s.as_str().is_some_and(|s| s.contains(": ")))
            .count()
            >= 3,
        "the fixture must exercise `: ` in all three lists",
    );
}

/// A composite rule whose `extract` yields a non-array.
const COMPOSITE_EXTRACT_NOT_AN_ARRAY: &str = r#"id: local.comp
description: composite whose extract is no array
severity: error
match:
  format: yaml
check:
  extract: '1'
  nested:
    id: local.inner
    description: inner
    severity: error
    match:
      format: yaml
    check:
      jq: '.'
      message: inner
  message: composite outer
"#;

#[test]
fn lint_warns_in_the_order_the_rules_run() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("a.yml"), RAISES_IN_CHECK).expect("write rule");
    std::fs::write(dir.path().join("b.yml"), COMPOSITE_EXTRACT_NOT_AN_ARRAY).expect("write rule");
    std::fs::write(dir.path().join("pod.yaml"), "spec: x\n").expect("write input");
    let assert = dq()
        .current_dir(dir.path())
        .args([
            "--no-color",
            "lint",
            "--allow-rule-errors",
            "--rules",
            "a.yml",
            "--rules",
            "b.yml",
            "pod.yaml",
        ])
        .assert()
        .code(4);
    assert_eq!(
        warnings(&assert.get_output().stderr),
        concat!(
            " WARN check.jq raised a runtime error; skipping rule for this file",
            " rule_id=local.raises-in-check",
            " error=jq runtime error: cannot index \"x\" with \"template\"\n",
            " WARN composite extract did not return an array",
            " rule_id=local.comp shape=number\n",
        )
    );
}
