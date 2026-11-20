//! `-F sarif` names the rule behind every result.
//!
//! GitHub Code Scanning groups alerts by `ruleId`, so each result carries
//! the diagnostic's `rule_id` as `ruleId`, plus a `ruleIndex` into
//! `tool.driver.rules`, which describes every rule the results name: its
//! `id`, a `shortDescription` (the first sentence of the rule's
//! `description`), a `fullDescription` (the whole description) and a
//! `helpUri` (the rule's first `http(s)` reference). `validate` reports
//! under the built-in id `validate.parse`.

use std::path::Path;

use assert_cmd::Command;
use serde_json::{Value, json};

fn dq(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp").current_dir(dir);
    cmd
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(bytes)))
}

/// Every result's `ruleIndex` points at the descriptor of its `ruleId`.
fn assert_indexes_match(sarif: &Value) {
    let rules = sarif["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .expect("driver.rules array");
    for result in sarif["runs"][0]["results"].as_array().expect("results") {
        let id = result["ruleId"]
            .as_str()
            .expect("every result has a ruleId");
        let index = result["ruleIndex"].as_u64().expect("ruleIndex") as usize;
        assert_eq!(rules[index]["id"], id, "ruleIndex of {result}");
    }
}

const NO_NAME: &str = "id: t.no-name\n\
description: |\n  Every document needs a name. Without one the\n  deployer cannot\n  tell documents apart.\n\n  A second paragraph.\n\
severity: error\nmatch: { format: yaml }\n\
check:\n  jq: 'select(.name == null) | .'\n  message: 'no name'\n\
references:\n  - RFC 1234\n  - https://example.com/rules/no-name\n";

const PORT: &str = "id: t.port\ndescription: Ports stay below 1024\nseverity: warn\n\
match: { format: yaml }\n\
check:\n  jq: 'select(.port > 1024) | .port'\n  message: 'port {{ . }}'\n";

#[test]
fn lint_sarif_results_carry_rule_ids_and_driver_rules() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "no-name.yml", NO_NAME);
    write(dir.path(), "port.yml", PORT);
    write(dir.path(), "a.yaml", "port: 8080\n");
    write(dir.path(), "b.yaml", "name: b\nport: 9090\n");
    let out = dq(dir.path())
        .args([
            "-F",
            "sarif",
            "lint",
            "--rules",
            "port.yml",
            "--rules",
            "no-name.yml",
            "a.yaml",
            "b.yaml",
        ])
        .assert()
        .code(4);
    let sarif = json_of(&out.get_output().stdout);
    let ids: Vec<(&str, u64)> = sarif["runs"][0]["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| {
            (
                r["ruleId"].as_str().expect("ruleId"),
                r["ruleIndex"].as_u64().expect("ruleIndex"),
            )
        })
        .collect();
    assert_eq!(ids, [("t.port", 0), ("t.no-name", 1), ("t.port", 0)]);
    assert_eq!(
        sarif["runs"][0]["tool"]["driver"]["rules"],
        json!([
            {
                "id": "t.port",
                "shortDescription": { "text": "Ports stay below 1024" },
            },
            {
                "id": "t.no-name",
                "shortDescription": {
                    "text": "Every document needs a name.",
                },
                "fullDescription": {
                    "text": "Every document needs a name. Without one the\ndeployer cannot\n\
                             tell documents apart.\n\nA second paragraph.",
                },
                "helpUri": "https://example.com/rules/no-name",
            },
        ])
    );
    assert_indexes_match(&sarif);
}

/// An `@std` rule is described from its own YAML; its id is stable.
#[test]
fn lint_sarif_describes_an_std_rule() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Dockerfile", "FROM alpine:3.20\nRUN true\n");
    let out = dq(dir.path())
        .args(["-F", "sarif", "lint", "Dockerfile"])
        .assert()
        .success();
    let sarif = json_of(&out.get_output().stdout);
    let result = &sarif["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "dockerfile.has-healthcheck");
    let rule = &sarif["runs"][0]["tool"]["driver"]["rules"][0];
    assert_eq!(rule["id"], "dockerfile.has-healthcheck");
    assert_eq!(
        rule["shortDescription"]["text"],
        "Long-running Docker images benefit from a HEALTHCHECK directive so \
         Docker / Kubernetes can detect frozen processes."
    );
    assert_eq!(
        rule["helpUri"],
        "https://docs.docker.com/engine/reference/builder/#healthcheck"
    );
    assert_indexes_match(&sarif);
}

/// No finding: an empty `results` and an empty `rules`.
#[test]
fn lint_sarif_without_findings_has_empty_rules() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "port.yml", PORT);
    write(dir.path(), "a.yaml", "port: 80\n");
    let out = dq(dir.path())
        .args(["-F", "sarif", "lint", "--rules", "port.yml", "a.yaml"])
        .assert()
        .success();
    let sarif = json_of(&out.get_output().stdout);
    assert_eq!(sarif["runs"][0]["results"], json!([]));
    assert_eq!(sarif["runs"][0]["tool"]["driver"]["rules"], json!([]));
}

#[test]
fn validate_sarif_reports_under_validate_parse() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "broken.yaml", "a: [\n");
    let out = dq(dir.path())
        .args(["-F", "sarif", "validate", "broken.yaml"])
        .assert()
        .code(4);
    let sarif = json_of(&out.get_output().stderr);
    let result = &sarif["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "validate.parse");
    assert_eq!(result["ruleIndex"], 0);
    assert_eq!(
        sarif["runs"][0]["tool"]["driver"]["rules"],
        json!([{
            "id": "validate.parse",
            "shortDescription": { "text": "The file does not parse in its format." },
        }])
    );
}

/// The structured `validate` diagnostic names the same id.
#[test]
fn validate_json_names_the_rule_id() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "broken.json", "{\"a\": 1,}");
    let out = dq(dir.path())
        .args(["-F", "json", "validate", "broken.json"])
        .assert()
        .code(4);
    let report = json_of(&out.get_output().stderr);
    assert_eq!(report["diagnostics"][0]["rule_id"], "validate.parse");
    assert!(
        report.get("rules").is_none(),
        "the rule catalogue is SARIF's alone: {report}"
    );
}
