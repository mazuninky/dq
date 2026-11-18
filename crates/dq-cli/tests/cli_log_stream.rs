//! Log output goes to stderr, never stdout.
//!
//! `dq` writes `tracing` events (here: the XML mixed-content warning) to
//! stderr so that stdout carries only the command's output and stays
//! machine-readable. ANSI escapes in the log follow the same precedence as
//! the report (`--no-color` > `NO_COLOR` > `CLICOLOR_FORCE` > terminal),
//! applied to stderr.

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

#[test]
fn log_colour_follows_the_flag_no_color_and_clicolor_force() {
    // stderr is a pipe here, so only `CLICOLOR_FORCE` can colour the log,
    // and `--no-color` or a `NO_COLOR` that is set at all — even to the
    // empty string — takes the colour away again.
    let (_dir, path) = mixed_content_xml();
    for no_color in [None, Some(""), Some("1")] {
        for flag in [false, true] {
            for force in [None, Some("1")] {
                let mut cmd = dq();
                if let Some(value) = no_color {
                    cmd.env("NO_COLOR", value);
                }
                if let Some(value) = force {
                    cmd.env("CLICOLOR_FORCE", value);
                }
                if flag {
                    cmd.arg("--no-color");
                }
                let out = cmd
                    .args(["-F", "json", "convert", &path])
                    .assert()
                    .success();
                let output = out.get_output();
                let stderr = String::from_utf8_lossy(&output.stderr);
                let case =
                    format!("NO_COLOR={no_color:?} --no-color={flag} CLICOLOR_FORCE={force:?}");
                assert!(stderr.contains("mixed content"), "{case}: {stderr:?}");
                let coloured = no_color.is_none() && !flag && force.is_some();
                assert_eq!(stderr.contains('\u{1b}'), coloured, "{case}: {stderr:?}");
                assert!(
                    !output.stdout.contains(&0x1b),
                    "{case}: the log colour never reaches stdout",
                );
            }
        }
    }
}

/// A JSON file `set` edits in place of its text; `--sort-keys` is a no-op
/// there and says so in a `DEBUG` event.
fn json_file() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.json");
    std::fs::write(&path, "{\"b\": 1, \"a\": 2}\n").expect("write fixture");
    let path = path.to_str().expect("utf-8 temp path").to_owned();
    (dir, path)
}

#[test]
fn verbose_logs_leave_stdout_as_the_edited_document() {
    let (_dir, path) = json_file();
    // (extra arguments, RUST_LOG, whether the DEBUG event is logged)
    let cases: [(&[&str], Option<&str>, bool); 5] = [
        (&[], None, false),
        (&["-v"], None, false),
        (&["-vv"], None, true),
        (&["-vvv"], None, true),
        (&[], Some("debug"), true),
    ];
    for (extra, rust_log, debug) in cases {
        let mut cmd = dq();
        if let Some(filter) = rust_log {
            cmd.env("RUST_LOG", filter);
        }
        let out = cmd
            .args(extra)
            .args(["--no-color", "--sort-keys", "set", &path, "/a", "5"])
            .assert()
            .success();
        let output = out.get_output();
        let case = format!("{extra:?} RUST_LOG={rust_log:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "{\"b\": 1, \"a\": 5}\n",
            "{case}: stdout is the document and nothing else",
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.contains("DEBUG") && stderr.contains("--sort-keys"),
            debug,
            "{case}: {stderr:?}",
        );
    }
}

#[test]
fn verbose_warning_still_leaves_stdout_valid_json() {
    let (_dir, path) = mixed_content_xml();
    let out = dq()
        .args(["-v", "-F", "json", "convert", &path])
        .assert()
        .success();
    let output = out.get_output();
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is exactly one JSON document");
    assert_eq!(json["a"][0]["#text"], "texttail");
    assert!(String::from_utf8_lossy(&output.stderr).contains("WARN"));
}
