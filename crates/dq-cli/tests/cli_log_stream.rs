//! Log output goes to stderr, never stdout.
//!
//! `dq` writes `tracing` events (here: the XML mixed-content warning) to
//! stderr so that stdout carries only the command's output and stays
//! machine-readable. ANSI escapes in the log follow the same precedence as
//! the report (`--no-color` > `NO_COLOR` > `CLICOLOR_FORCE` > terminal),
//! applied to stderr.
//!
//! Sanity coverage only; the full matrix is left to `rust-cli-test-writer`.

use assert_cmd::Command;
use tempfile::TempDir;

/// Build a `dq` command with a clean environment, so a developer's
/// `NO_COLOR`, `CLICOLOR_FORCE` or `RUST_LOG` cannot influence the test.
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// An XML document with mixed content, which makes the parser log a
/// `WARN`. Returns the directory (kept alive by the caller) and the path.
fn mixed_content_xml() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("mixed.xml");
    std::fs::write(&path, "<a>text<b/>tail</a>\n").expect("write fixture");
    let path = path.to_str().expect("utf-8 temp path").to_owned();
    (dir, path)
}

#[test]
fn warning_goes_to_stderr_and_stdout_stays_valid_json() {
    let (_dir, path) = mixed_content_xml();
    let out = dq()
        .args(["-F", "json", "convert", &path])
        .assert()
        .success();
    let output = out.get_output();

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is exactly one JSON document");
    assert_eq!(json["a"][0]["#text"], "texttail");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("WARN") && stderr.contains("mixed content"),
        "the warning must be on stderr, got: {stderr:?}",
    );
    // stderr is a pipe here, not a terminal: no ANSI escapes.
    assert!(
        !stderr.contains('\u{1b}'),
        "no ANSI on non-terminal stderr: {stderr:?}"
    );
}

#[test]
fn clicolor_force_colors_the_log() {
    let (_dir, path) = mixed_content_xml();
    let out = dq()
        .env("CLICOLOR_FORCE", "1")
        .args(["-F", "json", "convert", &path])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(
        stderr.contains('\u{1b}'),
        "CLICOLOR_FORCE colours the log: {stderr:?}"
    );
}

#[test]
fn no_color_flag_wins_over_clicolor_force_for_the_log() {
    let (_dir, path) = mixed_content_xml();
    let out = dq()
        .env("CLICOLOR_FORCE", "1")
        .args(["--no-color", "-F", "json", "convert", &path])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("mixed content"), "{stderr:?}");
    assert!(
        !stderr.contains('\u{1b}'),
        "--no-color drops ANSI from the log: {stderr:?}"
    );
}
