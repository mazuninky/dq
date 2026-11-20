//! Stdin can be read once: every command that takes `-` reads it exactly
//! once, and a command asked to read it for two inputs refuses (exit 2).
//!
//! `dq fix -` once read stdin for the diff and again for the parse, so the
//! parse saw nothing and every run on stdin failed ("EOF while parsing",
//! exit 3).

use assert_cmd::Command;
use pretty_assertions::assert_eq;

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

/// Run `dq --no-color ARGS` in a fresh directory with `stdin`; return
/// (exit code, stdout, stderr).
fn run(args: &[&str], stdin: &str) -> (i32, String, String) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = dq()
        .current_dir(dir.path())
        .arg("--no-color")
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

/// A package.json without a license: `@std/npm/has-license` fixes it.
const PACKAGE: &str = "{\"name\":\"x\",\"version\":\"1\"}\n";

#[test]
fn fix_reads_stdin_once_and_prints_the_fixed_document() {
    let (code, stdout, stderr) = run(
        &["fix", "-", "--input-format", "json", "--rules", "@std/npm"],
        PACKAGE,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let fixed: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
    assert_eq!(fixed["name"], "x", "stdout: {stdout}");
    assert!(fixed.get("license").is_some(), "stdout: {stdout}");
}

#[test]
fn fix_diff_reads_stdin_once() {
    let (code, stdout, stderr) = run(
        &[
            "--diff",
            "fix",
            "-",
            "--input-format",
            "json",
            "--rules",
            "@std/npm",
        ],
        PACKAGE,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("+") && stdout.contains("license"),
        "stdout: {stdout}"
    );
}

/// Every command that reads a data file reads `-` once: each one sees the
/// whole of stdin and succeeds.
#[test]
fn every_command_reads_stdin_once() {
    let cases: &[&[&str]] = &[
        &["get", "-", "/name"],
        &["exists", "-", "/name"],
        &["keys", "-", ""],
        &["values", "-", ""],
        &["len", "-", ""],
        &["type", "-", ""],
        &["paths", "-"],
        &["select", "-", "$.name"],
        &["query", ".name", "-"],
        &["convert", "-", "-F", "yaml"],
        &["fmt", "-"],
        &["validate", "-"],
        &["set", "-", "/name", "y"],
        &["set", "-", "--jq", ".name = \"y\""],
        &["del", "-", "/name"],
        &["patch", "-", "[{\"op\":\"remove\",\"path\":\"/name\"}]"],
        &["merge", "-", "{\"name\":null}"],
        &["lint", "-", "--rules", "@std/npm"],
        &["check", "-", "--rule", "npm.has-license"],
        &["fix", "-", "--rules", "@std/npm"],
    ];
    for case in cases {
        let mut args = case.to_vec();
        args.extend(["--input-format", "json"]);
        let (code, stdout, stderr) = run(&args, PACKAGE);
        assert!(
            !stderr.contains("EOF while parsing"),
            "dq {args:?} read stdin twice: {stderr}"
        );
        // `lint` and `check` report the missing license as a warning: exit 0.
        assert_eq!(code, 0, "dq {args:?}: stdout {stdout} stderr {stderr}");
    }
}
