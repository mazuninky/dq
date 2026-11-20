//! What a file or the user put in a line dq prints outside a reporter — a
//! file name in the bulk summary, a `--diff` header or a log line, a key in
//! a writer's refusal, a test case's name — reaches the terminal escaped as
//! the console reporter escapes text: a line break as `\x0a`, ESC as
//! `\x1b`, so it can neither start a line of its own nor send the terminal
//! a control sequence.

use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

fn dq(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd.current_dir(dir);
    cmd
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(dir: &Path, args: &[&str]) -> Run {
    let out = dq(dir).args(args).output().expect("run dq");
    Run {
        code: out.status.code().expect("exit code"),
        stdout: String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(out.stderr).expect("UTF-8 stderr"),
    }
}

/// A file named with ESC (and a colour sequence) and one named with a line
/// break.
const ESC_NAME: &str = "e\u{1b}[31m.yaml";
const LF_NAME: &str = "n\nl.yaml";
const ESC_SHOWN: &str = "e\\x1b[31m.yaml";
const LF_SHOWN: &str = "n\\x0al.yaml";

/// A directory holding [`ESC_NAME`], [`LF_NAME`] and `two.yaml`, a stream
/// of two documents; `None` where the file system refuses such names.
fn named_files() -> Option<TempDir> {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in [ESC_NAME, LF_NAME] {
        if std::fs::write(dir.path().join(name), "a: 1\n").is_err() {
            return None;
        }
    }
    std::fs::write(dir.path().join("two.yaml"), "a: 1\n---\na: 2\n").expect("seed");
    Some(dir)
}

/// Nothing raw on either stream: no ESC, no file name broken across lines.
fn assert_no_raw(run: &Run) {
    for text in [&run.stdout, &run.stderr] {
        assert!(!text.contains('\u{1b}'), "raw ESC: {text:?}");
        assert!(!text.contains("n\nl"), "raw line break in a name: {text:?}");
    }
}

#[test]
fn bulk_lines_escape_file_names() {
    let Some(dir) = named_files() else {
        eprintln!("skipped: the file system refuses control characters in names");
        return;
    };
    let dir = dir.path();
    // `[en]*.yaml`: the two named files, not the stream.

    // The `=== <path> ===` marker of each file's output.
    let out = run(dir, &["--no-color", "set", "[en]*.yaml", "/a", "2"]);
    assert_eq!(out.code, 0, "{:?}", out.stderr);
    assert!(
        out.stdout.contains(&format!("=== {ESC_SHOWN} ===\na: 2\n")),
        "{:?}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!("=== {LF_SHOWN} ===\na: 2\n")),
        "{:?}",
        out.stdout
    );
    assert_no_raw(&out);

    // `--check`: one `would modify:` line per file.
    let out = run(
        dir,
        &["--no-color", "--check", "set", "[en]*.yaml", "/a", "2"],
    );
    assert_eq!(out.code, 1, "{:?}", out.stderr);
    assert!(
        out.stdout.contains(&format!("would modify: {ESC_SHOWN}\n")),
        "{:?}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!("would modify: {LF_SHOWN}\n")),
        "{:?}",
        out.stdout
    );
    assert_no_raw(&out);

    // `--diff`: the marker and the `---` / `+++` header; the diff lines
    // are the file's text.
    let out = run(
        dir,
        &["--no-color", "--diff", "set", "[en]*.yaml", "/a", "2"],
    );
    assert_eq!(out.code, 0, "{:?}", out.stderr);
    assert!(
        out.stdout.contains(&format!(
            "=== {LF_SHOWN} ===\n--- a/{LF_SHOWN}\n+++ b/{LF_SHOWN}\n"
        )),
        "{:?}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!("--- a/{ESC_SHOWN}\n")),
        "{:?}",
        out.stdout
    );
    assert_no_raw(&out);

    // A failed file: one summary line, its path and its error.
    let out = run(
        dir,
        &[
            "--no-color",
            "-i",
            "--continue-on-error",
            "set",
            "[en]*.yaml",
            "/a/b",
            "2",
        ],
    );
    assert_eq!(out.code, 7, "{:?}", out.stderr);
    assert!(
        out.stdout.contains(&format!("  - {ESC_SHOWN}: ")),
        "{:?}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!("  - {LF_SHOWN}: ")),
        "{:?}",
        out.stdout
    );
    assert_no_raw(&out);

    // `--doc 1` skips the files of one document with a warning that names
    // each one, twice.
    let out = run(
        dir,
        &["--no-color", "--doc", "1", "set", "*.yaml", "/a", "3"],
    );
    assert_eq!(out.code, 0, "{:?}", out.stderr);
    assert!(
        out.stderr.contains(&format!(
            "skipped {LF_SHOWN}: --doc 1 is out of range: {LF_SHOWN} holds 1 document(s)"
        )),
        "{:?}",
        out.stderr
    );
    assert!(
        out.stderr.contains(&format!("skipped {ESC_SHOWN}: ")),
        "{:?}",
        out.stderr
    );
    assert_no_raw(&out);
}

/// A key or a section name from the file, quoted in the writer's refusal.
#[test]
fn a_writer_refusal_escapes_the_key_it_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir = dir.path();
    std::fs::write(dir.join("t.csv"), "\"k\nz\",b\n1,2\n").expect("seed");
    let out = run(
        dir,
        &["--no-color", "-i", "set", "t.csv", "/0/k\nz", r#"{"x": 1}"#],
    );
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (
            1,
            "csv format error: row 0 key 'k\\x0az': nested object cannot be serialized to csv\n"
        )
    );

    std::fs::write(dir.join("e.csv"), "k\u{1b}]0;x\u{7},b\n1,2\n").expect("seed");
    let out = run(
        dir,
        &[
            "--no-color",
            "-i",
            "set",
            "e.csv",
            "/0/k\u{1b}]0;x\u{7}",
            r#"{"x": 1}"#,
        ],
    );
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (
            1,
            "csv format error: row 0 key 'k\\x1b]0;x\\x07': nested object cannot be \
             serialized to csv\n"
        )
    );

    std::fs::write(dir.join("a.ini"), "[s\u{1b}[2Jt]\nk = v\n").expect("seed");
    let out = run(
        dir,
        &[
            "--no-color",
            "-i",
            "set",
            "a.ini",
            "/s\u{1b}[2Jt/k",
            r#"{"x": 1}"#,
        ],
    );
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (
            1,
            "ini format error: section 's\\x1b[2Jt' key 'k': nested object cannot be \
             serialized to INI\n"
        )
    );
}

/// The `test` runner's lines quote the case's name from the fixture file.
#[test]
fn the_test_runner_escapes_case_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir = dir.path();
    std::fs::write(
        dir.join("r.yml"),
        "id: t.name\ndescription: d\nseverity: error\nmatch:\n  format: yaml\n\
         check:\n  jq: 'select(.name == \"\") | .'\n  message: 'empty'\n",
    )
    .expect("seed rule");
    std::fs::write(
        dir.join("r.test.yml"),
        "tests:\n  - name: \"two\\nlines \\e[2J\"\n    input: |\n      name: \"\"\n    \
         expected:\n      violations:\n        - rule: t.name\n",
    )
    .expect("seed fixture");

    let out = run(dir, &["--no-color", "test", "."]);
    assert_eq!(out.code, 0, "{:?}", out.stderr);
    assert!(
        out.stdout.contains(": two\\x0alines \\x1b[2J\n"),
        "{:?}",
        out.stdout
    );
    assert!(!out.stdout.contains('\u{1b}'), "{:?}", out.stdout);

    let out = run(dir, &["--no-color", "-F", "tap", "test", "."]);
    assert_eq!(out.code, 0, "{:?}", out.stderr);
    assert!(
        out.stdout.contains(": two\\x0alines \\x1b[2J\n"),
        "{:?}",
        out.stdout
    );
    assert!(!out.stdout.contains('\u{1b}'), "{:?}", out.stdout);
}

/// A line logged through the `log` crate (jaq's `stderr`, here) prints its
/// message alone. Was: `ERROR ab log.target="jaq_std" log.module_path=…
/// log.file="/…/.cargo/registry/…/lib.rs" log.line=790`, the build
/// machine's paths included.
#[test]
fn a_log_crate_line_prints_no_log_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dir = dir.path();
    std::fs::write(dir.join("x.yaml"), "a: 1\n").expect("seed");
    for verbose in [None, Some("-vv")] {
        let mut args = vec!["--no-color"];
        args.extend(verbose);
        args.extend(["query", "\"ab\" | stderr", "x.yaml"]);
        let out = run(dir, &args);
        assert_eq!(out.code, 0, "{:?}", out.stderr);
        // Past the timestamp, the level and the message.
        let errors: Vec<&str> = out
            .stderr
            .lines()
            .filter_map(|line| line.split_once(' ').map(|(_, rest)| rest.trim_start()))
            .filter(|rest| rest.starts_with("ERROR"))
            .collect();
        assert_eq!(errors, ["ERROR ab"], "{verbose:?}: {:?}", out.stderr);
        assert!(
            !out.stderr.contains("log."),
            "{verbose:?}: {:?}",
            out.stderr
        );
    }
}
