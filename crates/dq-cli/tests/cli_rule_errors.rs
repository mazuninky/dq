//! A rule whose jq raises at run time (`.spec.template` on `spec: x`).
//!
//! `dq lint` skips such a rule for the file and says so on stderr, one
//! warning per rule, without changing the exit code. `dq test` used to go
//! through the same path and count the raised rule as zero violations, so
//! a fixture case expecting no violations passed; it now fails the case,
//! naming the rule, the stage that raised and the jq error.

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

#[test]
fn lint_keeps_warning_and_exit_code_when_a_rule_raises() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("check.yml"), RAISES_IN_CHECK).expect("write rule");
    std::fs::write(dir.path().join("filter.yml"), RAISES_IN_FILTER).expect("write rule");
    std::fs::write(dir.path().join("pod.yaml"), WRONG_TYPE_SPEC).expect("write input");
    let assert = dq()
        .current_dir(dir.path())
        .args([
            "--no-color",
            "lint",
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
