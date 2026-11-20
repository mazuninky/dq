//! jq path assignment through a key that is missing or null creates the
//! path, as jq does, on every surface that runs jq: `set --jq`, `query`
//! and a rule's `fix.jq`. It used to fail with "cannot use null as
//! iterable (array or object)" (exit 1).

use assert_cmd::Command;
use tempfile::TempDir;

fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// A temporary directory holding `files` (relative path, content).
fn dir_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir fixture");
        std::fs::write(path, src).expect("write fixture");
    }
    dir
}

fn read(dir: &TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name)).expect("read back")
}

const A_B_1: &str = "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n";

#[test]
fn set_jq_assigns_through_a_missing_key_with_every_operator() {
    let dir = dir_with(&[("o.json", "{}\n")]);
    for (expr, want) in [
        (".a.b = 1", A_B_1),
        (".a.b |= 1", A_B_1),
        (".a.b += 1", A_B_1),
        (".a.b //= 1", A_B_1),
        (r#"setpath(["a", "b"]; 1)"#, A_B_1),
        (".a[0] = 1", "{\n  \"a\": [\n    1\n  ]\n}\n"),
        (".a[1] = 1", "{\n  \"a\": [\n    null,\n    1\n  ]\n}\n"),
        ("del(.a.b)", "{}\n"),
        ("del(.a[0])", "{}\n"),
    ] {
        dq().current_dir(dir.path())
            .args(["--no-color", "set", "o.json", "--jq", expr])
            .assert()
            .success()
            .stdout(want.to_owned())
            .stderr("");
    }
}

#[test]
fn set_jq_assigns_into_a_null_document_and_through_a_null_value() {
    let dir = dir_with(&[
        ("n.json", "null\n"),
        ("an.json", "{\"a\": null}\n"),
        ("e.yaml", ""),
    ]);
    for (file, want) in [
        ("n.json", A_B_1),
        ("an.json", A_B_1),
        ("e.yaml", "a:\n  b: 1\n"),
    ] {
        dq().current_dir(dir.path())
            .args(["--no-color", "-i", "set", file, "--jq", ".a.b = 1"])
            .assert()
            .success()
            .stdout("");
        assert_eq!(read(&dir, file), want, "{file}");
    }
}

#[test]
fn set_jq_deletes_several_indices_as_they_were() {
    let dir = dir_with(&[("l.json", "[1, 2, 3]\n")]);
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "l.json", "--jq", "del(.[0], .[1])"])
        .assert()
        .success()
        .stdout("[\n  3\n]\n");
}

#[test]
fn assignment_through_a_string_still_fails_with_exit_1() {
    let dir = dir_with(&[("s.json", "{\"a\": \"x\"}\n")]);
    dq().current_dir(dir.path())
        .args(["--no-color", "-i", "set", "s.json", "--jq", ".a.b = 1"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains("cannot index \"x\" with \"b\""));
    assert_eq!(read(&dir, "s.json"), "{\"a\": \"x\"}\n");
}

#[test]
fn query_assigns_through_a_missing_key() {
    let dir = dir_with(&[("o.json", "{}\n")]);
    dq().current_dir(dir.path())
        .args(["--no-color", "query", ".a.b = 1", "o.json", "-F", "jsonl"])
        .assert()
        .success()
        .stdout("{\"a\":{\"b\":1}}\n");
}

#[test]
fn fix_jq_creates_the_labels_a_manifest_lacks() {
    const RULE: &str = r#"id: owner-label
description: every manifest names its owner
severity: warn
match:
  format: [yaml]
check:
  jq: 'select(.metadata.labels.owner == null) | .'
  message: 'no owner label'
fix:
  jq: '.metadata.labels.owner = "team"'
"#;
    const FIXED: &str = "kind: Pod\nmetadata:\n  labels:\n    owner: team\n";
    let dir = dir_with(&[
        (".dq/rules/owner-label.yml", RULE),
        // No `metadata` at all, and a `metadata:` that is null.
        ("a.yaml", "kind: Pod\n"),
        ("b.yaml", "kind: Pod\nmetadata:\n"),
    ]);
    dq().current_dir(dir.path())
        .args(["--no-color", "-i", "fix", "a.yaml", "b.yaml"])
        .assert()
        .success()
        .stdout("Modified: 2, Skipped: 0, Failed: 0\n");
    assert_eq!(read(&dir, "a.yaml"), FIXED);
    assert_eq!(read(&dir, "b.yaml"), FIXED);
}

/// An index past jq's limit (536870911) is jq's runtime error "Array index
/// too large", exit 1 — it used to panic on a capacity overflow (exit 101),
/// or try to allocate gigabytes.
#[test]
fn an_index_past_jqs_limit_is_a_jq_runtime_error() {
    let dir = dir_with(&[("n.json", "{\"n\": 4000000000000000000}\n")]);
    dq().current_dir(dir.path())
        .args(["--no-color", "query", ".a[.n] = 1", "n.json"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains("Array index too large"));
    dq().current_dir(dir.path())
        .args(["--no-color", "-i", "set", "n.json", "--jq", ".a[.n] = 1"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains("Array index too large"));
    assert_eq!(read(&dir, "n.json"), "{\"n\": 4000000000000000000}\n");
    dq().current_dir(dir.path())
        .args([
            "--no-color",
            "query",
            ".[4000000000000000000] = 1",
            "-",
            "--input-format",
            "json",
        ])
        .write_stdin("null")
        .assert()
        .code(1)
        .stderr(predicates::str::contains("Array index too large"));
}
