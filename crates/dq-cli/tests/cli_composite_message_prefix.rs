//! A composite rule's `message` prefix is trimmed the way a schema rule's is.
//!
//! A schema rule drops trailing blanks and one trailing colon of its
//! `message` and joins it to the rest with `: `, so `shape`, `shape:` and
//! `"shape: "` read alike. A composite rule glued its `message` as written,
//! so `cfg:` gave `cfg:: debug is on`. Both kinds now go through one helper.

use assert_cmd::Command;

fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

fn rule(message: &str) -> String {
    format!(
        r#"
id: test.inline-config
description: inline JSON config must parse and not enable debug
severity: error
match:
  format: yaml
check:
  extract: '[.items[] | {{value: .config, format: "json", anchor: "/items/0/config"}}]'
  nested:
    id: test.inline-config.inner
    description: debug off
    severity: error
    match:
      format: json
    check:
      jq: 'select(.debug == true) | {{pointer: "/debug"}}'
      message: "debug is on"
  message: {message}
"#
    )
}

const DOC: &str = "items:\n  - config: '{\"debug\": true}'\n  - config: '{\"debug\": '\n";

/// The messages `dq lint` reports for [`DOC`] under a composite rule with
/// `message: <message>` (a YAML scalar as written).
fn messages(message: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("r.yml"), rule(message)).unwrap();
    std::fs::write(dir.path().join("d.yaml"), DOC).unwrap();
    let out = dq()
        .current_dir(dir.path())
        .args([
            "--no-color",
            "lint",
            "-F",
            "json",
            "--rules",
            "r.yml",
            "d.yaml",
        ])
        .output()
        .expect("run dq lint");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stderr)));
    let mut all: Vec<String> = report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| d["message"].as_str().unwrap().to_owned())
        .collect();
    all.sort();
    all
}

#[test]
fn composite_prefix_reads_alike_with_or_without_a_colon() {
    let expected = messages("cfg");
    assert_eq!(expected.len(), 2, "{expected:?}");
    assert_eq!(expected[0], "cfg: debug is on");
    assert!(
        expected[1].starts_with("cfg: json parse failed: "),
        "{expected:?}"
    );
    for written in ["'cfg:'", "'cfg: '", "'cfg :  '"] {
        assert_eq!(messages(written), expected, "message: {written}");
    }
}

#[test]
fn blank_composite_prefix_is_no_prefix() {
    let found = messages("' : '");
    assert_eq!(found[0], "debug is on", "{found:?}");
    assert!(found[1].starts_with("json parse failed: "), "{found:?}");
}
