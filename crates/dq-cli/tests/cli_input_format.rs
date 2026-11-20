//! `-F/--format` is the output format on every command; `--input-format`
//! names the format a file — or stdin `-` — is read in.
//!
//! `-F` used to pick the parser too on most commands, so
//! `dq paths config.yaml -F json` parsed the YAML as JSON and failed, and
//! `dq lint -` had no way to name the format of stdin at all (on `lint`
//! `-F` picks the reporter). Write commands put a file back in its own
//! format: a `-F` naming another one is a usage error (exit 2), the same
//! one is accepted so scripts passing `-F yaml -i` keep working.

use assert_cmd::Command;
use pretty_assertions::assert_eq;
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

/// A fresh directory holding `name` with `src` in it.
fn dir_with(name: &str, src: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join(name), src).expect("seed");
    dir
}

/// Run `dq` in `dir` with `args` and `stdin`; return (exit code, stdout,
/// stderr).
fn run(dir: &TempDir, args: &[&str], stdin: &str) -> (i32, String, String) {
    let out = dq()
        .current_dir(dir.path())
        .args(args)
        .write_stdin(stdin)
        .output()
        .expect("run dq");
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        String::from_utf8(out.stderr).expect("utf-8 stderr"),
    )
}

const CONFIG: &str = "a: 1\nb:\n  c: x\n";

#[test]
fn paths_on_a_yaml_file_renders_json_under_dash_f_json() {
    let dir = dir_with("config.yaml", CONFIG);
    let (code, stdout, stderr) = run(&dir, &["paths", "config.yaml", "-F", "json"], "");
    assert_eq!((code, stderr.as_str()), (0, ""), "stdout: {stdout}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(
        parsed,
        serde_json::json!({"": "object", "/a": "int", "/b": "object", "/b/c": "string"})
    );
}

#[test]
fn get_on_a_yaml_file_renders_json_under_dash_f_json() {
    let dir = dir_with("config.yaml", CONFIG);
    let (code, stdout, stderr) = run(&dir, &["get", "config.yaml", "/b", "-F", "json"], "");
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (0, "{\n  \"c\": \"x\"\n}\n", "")
    );
}

#[test]
fn input_format_reads_a_file_with_an_unknown_extension() {
    let dir = dir_with("config.conf", CONFIG);
    let (code, stdout, stderr) = run(
        &dir,
        &["--input-format", "yaml", "get", "config.conf", "/b/c"],
        "",
    );
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "x\n", ""));
    // Without it the extension is unknown, as it always was.
    let (code, _, _) = run(&dir, &["get", "config.conf", "/b/c"], "");
    assert_eq!(code, 6);
}

#[test]
fn input_format_overrides_a_known_extension() {
    // A JSON body in a `.yaml` file read as JSON, rendered as YAML.
    let dir = dir_with("data.yaml", "{\"a\": [1, 2]}");
    let (code, stdout, stderr) = run(
        &dir,
        &[
            "get",
            "data.yaml",
            "/a",
            "--input-format",
            "json",
            "-F",
            "yaml",
        ],
        "",
    );
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (0, "- 1\n- 2\n", "")
    );
}

#[test]
fn input_format_names_the_format_of_stdin() {
    let dir = TempDir::new().expect("tempdir");
    let (code, stdout, stderr) = run(
        &dir,
        &["get", "-", "/b", "--input-format", "yaml", "-F", "json"],
        CONFIG,
    );
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (0, "{\n  \"c\": \"x\"\n}\n", "")
    );
}

#[test]
fn query_reads_stdin_in_the_input_format_and_writes_dash_f() {
    let dir = TempDir::new().expect("tempdir");
    let (code, stdout, stderr) = run(
        &dir,
        &["query", ".b.c", "-", "--input-format", "json", "-F", "yaml"],
        "{\"b\": {\"c\": \"x\"}}",
    );
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "- x\n", ""));
}

#[test]
fn stdin_without_input_format_is_a_usage_error_naming_the_flag() {
    let dir = TempDir::new().expect("tempdir");
    for args in [
        &["get", "-", "/a"][..],
        &["get", "-", "/a", "-F", "json"][..],
        &["query", ".a", "-", "-F", "yaml"][..],
        &["lint", "-"][..],
        &["validate", "-"][..],
    ] {
        let (code, stdout, stderr) = run(&dir, args, CONFIG);
        assert_eq!((code, stdout.as_str()), (2, ""), "{args:?}: {stderr}");
        assert!(stderr.contains("--input-format"), "{args:?}: {stderr}");
    }
}

#[test]
fn input_format_rejects_an_unknown_value_at_parse_time() {
    let dir = dir_with("config.yaml", CONFIG);
    let (code, stdout, stderr) = run(
        &dir,
        &["--input-format", "console", "get", "config.yaml", "/a"],
        "",
    );
    assert_eq!((code, stdout.as_str()), (2, ""));
    assert!(stderr.contains("invalid value 'console'"), "{stderr}");
}

const ALWAYS_FIRES: &str = "id: test.always-fires
description: always fires
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'always fires'
";

#[test]
fn lint_reads_stdin_in_the_input_format_and_reports_in_dash_f() {
    let dir = dir_with("rule.yml", ALWAYS_FIRES);
    let (code, stdout, stderr) = run(
        &dir,
        &[
            "lint",
            "-",
            "--input-format",
            "yaml",
            "--rules",
            "rule.yml",
            "-F",
            "json",
        ],
        CONFIG,
    );
    assert_eq!((code, stderr.as_str()), (0, ""), "stdout: {stdout}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("JSON report");
    let ids: Vec<&str> = parsed["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .map(|d| d["rule_id"].as_str().or(d["id"].as_str()).unwrap_or(""))
        .collect();
    assert_eq!(ids, ["test.always-fires"], "{parsed}");
}

#[test]
fn in_place_edit_refuses_a_dash_f_naming_another_format() {
    let dir = dir_with("config.yaml", CONFIG);
    let (code, stdout, stderr) = run(
        &dir,
        &["set", "config.yaml", "/a", "2", "-i", "-F", "json"],
        "",
    );
    assert_eq!((code, stdout.as_str()), (2, ""), "{stderr}");
    assert!(stderr.contains("--input-format"), "{stderr}");
    assert!(stderr.contains("output format"), "{stderr}");
    let left = std::fs::read_to_string(dir.path().join("config.yaml")).expect("read back");
    assert_eq!(left, CONFIG, "the file must be left as it was");
}

#[test]
fn in_place_edit_accepts_a_dash_f_naming_the_file_format() {
    let dir = dir_with("config.yaml", CONFIG);
    let (code, stdout, stderr) = run(
        &dir,
        &["set", "config.yaml", "/a", "2", "-i", "-F", "yaml"],
        "",
    );
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "", ""));
    let left = std::fs::read_to_string(dir.path().join("config.yaml")).expect("read back");
    assert_eq!(left, "a: 2\nb:\n  c: x\n");
}

#[test]
fn in_place_edit_compares_dash_f_with_the_input_format() {
    // The document's format is the one `--input-format` names, not the
    // extension's.
    let dir = dir_with("config.conf", CONFIG);
    let (code, stdout, stderr) = run(
        &dir,
        &[
            "set",
            "config.conf",
            "/a",
            "2",
            "-i",
            "--input-format",
            "yaml",
            "-F",
            "yaml",
        ],
        "",
    );
    assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "", ""));
    let left = std::fs::read_to_string(dir.path().join("config.conf")).expect("read back");
    assert_eq!(left, "a: 2\nb:\n  c: x\n");
}

#[test]
fn in_place_edit_of_stdin_is_a_usage_error() {
    let dir = TempDir::new().expect("tempdir");
    let (code, stdout, stderr) = run(
        &dir,
        &["set", "-", "/a", "2", "-i", "--input-format", "yaml"],
        CONFIG,
    );
    assert_eq!((code, stdout.as_str()), (2, ""), "{stderr}");
    assert!(
        !dir.path().join("-").exists(),
        "no file named '-' may be written"
    );
}

#[test]
fn set_on_stdin_prints_the_edited_document() {
    let dir = TempDir::new().expect("tempdir");
    let (code, stdout, stderr) = run(
        &dir,
        &["set", "-", "/a", "2", "--input-format", "yaml"],
        CONFIG,
    );
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (0, "a: 2\nb:\n  c: x\n", "")
    );
}

#[test]
fn convert_in_place_still_takes_dash_f_as_the_target_format() {
    let dir = dir_with("config.yaml", CONFIG);
    let (code, _stdout, stderr) = run(
        &dir,
        &["-q", "convert", "config.yaml", "-i", "-F", "json"],
        "",
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(dir.path().join("config.json").exists());
}
