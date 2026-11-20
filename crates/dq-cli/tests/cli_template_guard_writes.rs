//! The template guard on the commands that edit a file.
//!
//! `set`, `del`, `patch` and `merge` refused any file holding a template
//! marker, so no GitHub Actions workflow could be edited without an escape
//! hatch. They now run the read side's check: a file whose templates do not
//! change what the parser reads is edited, and the edit must keep them — a
//! textual edit leaves every template it did not target byte for byte, a
//! rewrite (`set --jq`, the formats edited by rewriting them whole) must
//! write each template so it reads back as the same string. A Helm values
//! file, whose templates do change the reading, is still refused (exit 3).

use std::path::Path;

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

fn write(dir: &Path, name: &str, contents: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write fixture");
    path.to_str().expect("UTF-8 path").to_owned()
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).expect("read back")
}

fn stdout_of(args: &[&str]) -> String {
    let out = dq().args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).expect("UTF-8 stdout")
}

const WORKFLOW: &str = "\
name: ci
on: push
env:
  REF: \"${{ github.ref }}\"
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: echo ${{ github.sha }}
      - name: quoted
        run: \"echo ${{ github.ref_name }}\"
";

const HELM: &str = "\
image:
  tag: stable
service:
  port: {{ .Values.service.port }}
";

/// A point edit of a workflow succeeds and changes its one line: the
/// template in a plain scalar and the ones in quoted strings stay byte for
/// byte.
#[test]
fn set_edits_a_workflow_and_keeps_its_templates() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "ci.yml", WORKFLOW);
    dq().args([
        "--no-color",
        "set",
        &file,
        "/jobs/build/runs-on",
        "ubuntu-24.04",
        "-i",
    ])
    .assert()
    .success();
    assert_eq!(
        read(&file),
        WORKFLOW.replace("runs-on: ubuntu-latest", "runs-on: ubuntu-24.04")
    );
}

/// `del` may take a template away with the node it removes; the others
/// stay. A value holding a new template can be set.
#[test]
fn del_and_set_add_or_remove_templates_on_purpose() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "ci.yml", WORKFLOW);
    dq().args(["--no-color", "del", &file, "/jobs/build/steps/1", "-i"])
        .assert()
        .success();
    assert_eq!(
        read(&file),
        WORKFLOW.replace("      - run: echo ${{ github.sha }}\n", "")
    );
    dq().args([
        "--no-color",
        "set",
        &file,
        "/env/SHA",
        "${{ github.sha }}",
        "-i",
    ])
    .assert()
    .success();
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, "/env/SHA"]),
        "${{ github.sha }}\n"
    );
}

/// `patch` and `merge` edit a workflow the same way.
#[test]
fn patch_and_merge_edit_a_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "ci.yml", WORKFLOW);
    dq().args([
        "--no-color",
        "patch",
        &file,
        r#"[{"op":"replace","path":"/name","value":"ci2"}]"#,
        "-i",
    ])
    .assert()
    .success();
    dq().args([
        "--no-color",
        "merge",
        &file,
        r#"{"on":"pull_request"}"#,
        "-i",
    ])
    .assert()
    .success();
    assert_eq!(
        read(&file),
        WORKFLOW
            .replace("name: ci\n", "name: ci2\n")
            .replace("on: push", "on: pull_request")
    );
}

/// `set --jq` rewrites the workflow through the YAML writer: every
/// template reads back as the same string.
#[test]
fn set_jq_rewrites_a_workflow_and_keeps_its_templates_as_data() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "ci.yml", WORKFLOW);
    dq().args(["--no-color", "set", &file, "--jq", ".name = \"ci2\"", "-i"])
        .assert()
        .success();
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, "/jobs/build/steps/1/run"]),
        "echo ${{ github.sha }}\n"
    );
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, "/env/REF"]),
        "${{ github.ref }}\n"
    );
}

/// A format edited by rewriting it whole (dotenv) keeps a template value.
#[test]
fn a_rewritten_format_keeps_its_templates() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "app.env", "REF={{ .Ref }}\nN=1\n");
    dq().args(["--no-color", "set", &file, "/N", "2", "-i"])
        .assert()
        .success();
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, "/REF"]),
        "{{ .Ref }}\n"
    );
    assert_eq!(stdout_of(&["--no-color", "get", &file, "/N"]), "2\n");
}

/// A Helm values file is still refused by every edit, with the guard's
/// message and exit 3, and left as it was.
#[test]
fn helm_values_are_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "values.yaml", HELM);
    for args in [
        vec!["set", &file, "/image/tag", "v2", "-i"],
        vec!["del", &file, "/image", "-i"],
        vec!["patch", &file, r#"[{"op":"remove","path":"/image"}]"#, "-i"],
        vec!["merge", &file, r#"{"image":null}"#, "-i"],
    ] {
        let mut full = vec!["--no-color"];
        full.extend(args.iter().copied());
        let out = dq().args(&full).assert().code(3);
        let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
        assert!(
            stderr.contains("file contains Go template syntax (line 4)"),
            "{args:?}: {stderr}"
        );
        assert_eq!(read(&file), HELM, "{args:?} left the file alone");
    }
}

/// The guard runs on whatever `--input-format` names, stdin included.
#[test]
fn the_guard_runs_on_stdin_read_with_input_format() {
    let out = dq()
        .args([
            "--no-color",
            "--input-format",
            "yaml",
            "get",
            "-",
            "/service/port",
        ])
        .write_stdin(HELM)
        .assert()
        .code(3);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(stderr.contains("Go template syntax"), "{stderr}");
}
