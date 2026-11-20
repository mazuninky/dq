//! The template guard on the commands that read a file.
//!
//! The write commands (`set`, `del`, `patch`, `merge`) have refused a
//! templated file since the guard was written; the read commands parsed it
//! silently, so a Helm `{{ .Values.x }}` read as a YAML flow mapping and
//! `get`, `convert` and `validate` answered with garbage. Every command that
//! reads a file's data now runs the guard before it uses the parse: a
//! template that changes what the parser reads is refused with the guard's
//! error (exit 3); one that does not — a GitHub Actions `${{ … }}` inside a
//! plain scalar, a `{{ … }}` inside a quoted string — passes. The escape
//! hatches work as on the write commands: `--allow-templates` reads the
//! file as it is, `--raw-template-strings` reads every template as the
//! string it is written as.

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

const HELM: &str = "\
image:
  tag: stable
service:
  port: {{ .Values.service.port }}
";

const WORKFLOW: &str = "\
name: ci
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo ${{ github.ref }}
      - run: \"echo {{ not a template }}\"
";

const INLINE_RULE: &str = "id: t.any\ndescription: d\nseverity: error\nmatch:\n  format: yaml\ncheck:\n  jq: empty\n  message: m\n";

fn assert_refused(args: &[&str]) {
    let out = dq().args(args).assert().code(3);
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(
        stderr.contains("file contains Go template syntax (line 4)")
            && stderr.contains("--allow-templates")
            && stderr.contains("--raw-template-strings"),
        "{args:?}: stderr must carry the guard's message, got:\n{stderr}"
    );
    assert!(
        out.get_output().stdout.is_empty(),
        "{args:?}: nothing on stdout, got:\n{}",
        String::from_utf8_lossy(&out.get_output().stdout)
    );
}

/// Every command that reads a Helm values file refuses it, with the
/// guard's message and exit 3 — `get`, `convert` and `validate` among them.
#[test]
fn read_commands_refuse_a_template_that_changes_the_reading() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "values.yaml", HELM);
    let other = write(dir.path(), "other.yaml", "image:\n  tag: stable\n");
    for args in [
        vec!["get", &file, "/service/port"],
        vec!["exists", &file, "/image"],
        vec!["keys", &file, ""],
        vec!["values", &file, ""],
        vec!["len", &file, ""],
        vec!["type", &file, "/service"],
        vec!["paths", &file],
        vec!["select", &file, "$.image"],
        vec!["query", ".image", &file],
        vec!["-F", "json", "convert", &file],
        vec!["validate", &file],
        vec!["diff", &other, &file],
        vec!["lint", &file],
        vec!["check", "--inline", INLINE_RULE, &file],
        vec!["fmt", &file],
        vec!["fix", &file],
        vec!["set", &file, "--jq", "."],
    ] {
        let mut full = vec!["--no-color"];
        full.extend(args);
        assert_refused(&full);
    }
}

/// A template that reads the same once removed is no reason to refuse:
/// a GitHub Actions workflow reads as before, through every read command.
#[test]
fn read_commands_pass_a_template_that_does_not_change_the_reading() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "ci.yml", WORKFLOW);
    let out = dq()
        .args(["--no-color", "get", &file, "/jobs/build/steps/0/run"])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&out.get_output().stdout),
        "echo ${{ github.ref }}\n"
    );
    dq().args(["--no-color", "validate", &file])
        .assert()
        .success();
    dq().args(["--no-color", "-F", "json", "convert", &file])
        .assert()
        .success();
    // Lint runs its rules over the workflow; whatever they find, the
    // guard does not stand in the way (exit 3 would be the guard).
    let lint = dq().args(["--no-color", "lint", &file]).assert();
    assert_ne!(lint.get_output().status.code(), Some(3));
}

/// `--raw-template-strings` reads each template as the string it is
/// written as, so `get` and `convert` show the template text.
#[test]
fn raw_template_strings_reads_templates_as_strings() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "values.yaml", HELM);
    let out = dq()
        .args([
            "--no-color",
            "--raw-template-strings",
            "get",
            &file,
            "/service/port",
        ])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&out.get_output().stdout),
        "{{ .Values.service.port }}\n"
    );
    let out = dq()
        .args([
            "--no-color",
            "--raw-template-strings",
            "-F",
            "json",
            "convert",
            &file,
        ])
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("JSON output");
    assert_eq!(json["service"]["port"], "{{ .Values.service.port }}");
}

/// `--allow-templates` reads the file as it is, the way every read
/// command did before the guard ran on them: YAML sees a flow mapping.
#[test]
fn allow_templates_reads_the_file_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "values.yaml", HELM);
    let out = dq()
        .args([
            "--no-color",
            "--allow-templates",
            "type",
            &file,
            "/service/port",
        ])
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&out.get_output().stdout),
        "object\n"
    );
}

/// `fmt` re-emits the whole file, so neither escape hatch can keep its
/// templates; it refuses both flags as `fix` does (exit 6).
#[test]
fn fmt_refuses_both_escape_hatches() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "values.yaml", HELM);
    for flag in ["--allow-templates", "--raw-template-strings"] {
        let out = dq()
            .args(["--no-color", flag, "fmt", &file])
            .assert()
            .code(6);
        let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
        assert!(stderr.contains(flag), "{flag}: got:\n{stderr}");
    }
}

/// Text in the file that spells a placeholder is the file's text: the
/// placeholders are picked so that none of them is in the file. Was:
/// `d` read, and written back, as the template `{{ .x }}`.
#[test]
fn raw_template_strings_leave_placeholder_text_in_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let src = "a: \"{{ .x }}\"\nd: \"DQtplxq0qx\"\ne: DQtplxxq0qx\n";
    let file = write(dir.path(), "v.yaml", src);
    let out = dq()
        .args([
            "--no-color",
            "--raw-template-strings",
            "-F",
            "json",
            "get",
            &file,
            "",
        ])
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("JSON output");
    assert_eq!(
        json,
        serde_json::json!({"a": "{{ .x }}", "d": "DQtplxq0qx", "e": "DQtplxxq0qx"})
    );
    dq().args([
        "--no-color",
        "--raw-template-strings",
        "-i",
        "set",
        &file,
        "/z",
        "1",
    ])
    .assert()
    .success();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{src}z: 1\n")
    );
    // The guard reads the file twice, the second time with placeholders:
    // the text that spells one is not taken for a template either.
    let guarded = "a: \"x {{ .x }}\"\nd: \"DQtplxq0qx\"\n";
    let file = write(dir.path(), "g.yaml", guarded);
    dq().args(["--no-color", "-i", "set", &file, "/z", "1"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{guarded}z: 1\n")
    );
}
