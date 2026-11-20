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

/// A command given `-` for two inputs would read stdin for the first and
/// find it empty for the second: refused before anything is read.
#[test]
fn two_stdin_inputs_are_refused() {
    for (args, named) in [
        (&["diff", "-", "-"][..], "A and B"),
        (&["patch", "-", "-"][..], "FILE and OPS"),
        (&["merge", "-", "-"][..], "FILE and PATCH"),
        (&["set", "-", "/name", "-"][..], "FILE and VALUE"),
        (&["set", "-", "/name", "@-"][..], "FILE and VALUE"),
        (
            &["set", "-", "/name", "--value-from", "-"][..],
            "FILE and --value-from",
        ),
        (
            &["lint", "-", "-", "--rules", "@std/npm"][..],
            "FILES twice",
        ),
        (
            &["check", "-", "-", "--rule", "npm.has-license"][..],
            "FILES twice",
        ),
        (&["fix", "-", "-", "--rules", "@std/npm"][..], "FILES twice"),
    ] {
        let mut args = args.to_vec();
        args.extend(["--input-format", "json"]);
        let (code, stdout, stderr) = run(&args, PACKAGE);
        assert_eq!(code, 2, "dq {args:?}: stdout {stdout} stderr {stderr}");
        assert!(
            stderr.contains("stdin (`-`) can be read only once")
                && stderr.contains(&format!("for {named}")),
            "dq {args:?}: {stderr}"
        );
        assert_eq!(stdout, "", "dq {args:?}");
    }
}

/// A value read from stdin (`-`, `@-`, `--value-from -`) is parsed as the
/// inline value argument is: no format, the JSON-literal heuristic, and
/// `--value-string` to keep it a string. `@-` and `--value-from -` once
/// went through the file reader, which has no format for stdin, so they
/// always failed with exit 2.
#[test]
fn set_reads_its_value_from_stdin_as_an_inline_value() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("f.json"), PACKAGE).expect("write");
    let set = |value_args: &[&str], stdin: &str| {
        let out = dq()
            .current_dir(dir.path())
            .args(["--no-color", "set", "f.json", "/v"])
            .args(value_args)
            .write_stdin(stdin)
            .output()
            .expect("run dq");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{value_args:?} <<< {stdin:?}: {stderr}"
        );
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON on stdout");
        doc["v"].clone()
    };
    for value_args in [&["-"][..], &["@-"][..], &["--value-from", "-"][..]] {
        assert_eq!(
            set(value_args, "{\"k\": [1, 2]}"),
            serde_json::json!({"k": [1, 2]})
        );
        assert_eq!(set(value_args, "42"), serde_json::json!(42));
        assert_eq!(set(value_args, "plain"), serde_json::json!("plain"));
        let mut forced = value_args.to_vec();
        forced.push("--value-string");
        assert_eq!(set(&forced, "42"), serde_json::json!("42"));
    }
}

/// Run `dq --no-color ARGS` in `dir` with `stdin`; return (exit code,
/// stdout, stderr).
fn run_in(dir: &std::path::Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let out = dq()
        .current_dir(dir)
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

/// Only a `-` argument means stdin: a glob that matches a file named `-`
/// names that file (`./-`), so `fix - '*'` reads stdin once and the file
/// once, and `fmt '*'` formats the file rather than reading stdin.
#[test]
fn a_glob_never_expands_to_stdin() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("-"), "{\"name\":\"x\",\"version\":\"1\"}\n").unwrap();
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &[
            "fix",
            "--input-format",
            "json",
            "--rules",
            "@std/npm",
            "-",
            "*",
            "--diff",
        ],
        "{\"name\":\"y\",\"version\":\"1\"}\n",
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("=== - ===\n"), "{stdout}");
    assert!(stdout.contains("=== ./- ===\n"), "{stdout}");
    assert!(stdout.contains("+{\"name\":\"x\""), "{stdout}");
    assert!(stdout.contains("+{\"name\":\"y\""), "{stdout}");

    let (code, stdout, stderr) = run_in(
        dir.path(),
        &[
            "lint",
            "-F",
            "json",
            "--input-format",
            "json",
            "--rules",
            "@std/npm",
            "-",
            "*",
        ],
        "{\"name\":\"y\",\"version\":\"1\"}\n",
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    let mut paths: Vec<&str> = report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    paths.dedup();
    assert_eq!(paths, ["-", "./-"]);

    std::fs::write(dir.path().join("-"), "a:   1\n").unwrap();
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &["fmt", "--input-format", "yaml", "*", "--diff"],
        "stdin: never read\n",
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("+a: 1\n"), "{stdout}");
    assert!(!stdout.contains("stdin"), "{stdout}");
}

/// `-` is stdin wherever `patch` and `merge` name their source: `@-`,
/// `--ops-from -` and `--patch-from -` read it, as `-` does.
#[test]
fn patch_and_merge_read_stdin_through_at_dash_and_from_dash() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("a.yaml"), "b: 2\n").unwrap();
    let ops = "[{\"op\":\"replace\",\"path\":\"/b\",\"value\":3}]";
    for args in [
        ["patch", "a.yaml", "@-"],
        ["patch", "a.yaml", "--ops-from=-"],
    ] {
        let (code, stdout, stderr) = run_in(dir.path(), &args, ops);
        assert_eq!(code, 0, "{args:?}: {stderr}");
        assert_eq!(stdout, "b: 3\n", "{args:?}");
    }
    for args in [
        ["merge", "a.yaml", "@-"],
        ["merge", "a.yaml", "--patch-from=-"],
    ] {
        let (code, stdout, stderr) = run_in(dir.path(), &args, "{\"b\":5}");
        assert_eq!(code, 0, "{args:?}: {stderr}");
        assert_eq!(stdout, "b: 5\n", "{args:?}");
    }
}

/// Stdin named twice through the new spellings is refused before any read.
#[test]
fn stdin_twice_through_at_dash_or_from_dash_is_refused() {
    for (args, inputs) in [
        (
            &["patch", "-", "--input-format", "yaml", "--ops-from", "-"][..],
            "FILE and --ops-from",
        ),
        (
            &["patch", "-", "--input-format", "yaml", "@-"][..],
            "FILE and OPS",
        ),
        (
            &["merge", "-", "--input-format", "yaml", "--patch-from", "-"][..],
            "FILE and --patch-from",
        ),
        (
            &["merge", "-", "--input-format", "yaml", "@-"][..],
            "FILE and PATCH",
        ),
    ] {
        let (code, _, stderr) = run(args, "a: 1\n");
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(stderr.contains(inputs), "{args:?}: {stderr}");
    }
}
