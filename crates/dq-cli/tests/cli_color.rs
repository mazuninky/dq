//! Color resolution precedence tests.
//!
//! `dq::output::resolve_color` orders inputs:
//!   `--no-color` flag > `NO_COLOR` env > `CLICOLOR_FORCE` env > `is_terminal`
//!
//! Tests deliberately use `Command::env_clear` + selective `Command::env` to
//! isolate each precedence rung. Crucially, NONE of these tests call
//! `std::env::set_var(...)`: process-wide env mutation is not thread-safe and
//! breaks parallel test execution. The CLI is designed to be tested via
//! per-process env injection precisely so this constraint can be honoured.
//!
//! All tests run with stdout piped (assert_cmd captures it), so `is_terminal`
//! returns `false` by default — that's the floor we build precedence on top of.

use std::path::PathBuf;

use assert_cmd::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Build a `dq` command with a clean env and only `PATH` + `HOME` re-added.
/// Other env vars (NO_COLOR, CLICOLOR_FORCE, RUST_LOG) are NOT inherited from
/// the developer's shell.
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// Path to a fixture whose console output, with color enabled, contains ANSI
/// escapes. `paths` on an object emits cyan-coloured keys when use_color=true.
fn colorable_fixture() -> PathBuf {
    fixture("server_config.yaml")
}

#[test]
fn no_color_flag_alone_disables_ansi() {
    let out = dq()
        .args(["paths", colorable_fixture().to_str().unwrap(), "--no-color"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        !stdout.contains('\x1b'),
        "no ANSI under --no-color, got bytes: {:?}",
        stdout.as_bytes(),
    );
}

#[test]
fn no_color_env_alone_disables_ansi() {
    // No `--no-color` flag this time, but `NO_COLOR=1` is set.
    let out = dq()
        .env("NO_COLOR", "1")
        .args(["paths", colorable_fixture().to_str().unwrap()])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        !stdout.contains('\x1b'),
        "no ANSI under NO_COLOR=1, got: {stdout:?}",
    );
}

#[test]
fn clicolor_force_env_with_no_tty_enables_ansi() {
    // No flag, no NO_COLOR, but CLICOLOR_FORCE=1 — ANSI should appear in
    // stdout even though the test is piped (no TTY). This proves the
    // CLICOLOR_FORCE rung overrides the is_terminal default.
    let out = dq()
        .env("CLICOLOR_FORCE", "1")
        .args(["paths", colorable_fixture().to_str().unwrap()])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains('\x1b'),
        "expected ANSI under CLICOLOR_FORCE=1 with no TTY, got: {stdout:?}",
    );
}

#[test]
fn no_color_flag_overrides_clicolor_force() {
    // `--no-color` is the highest-precedence rung — it wins over any env var.
    let out = dq()
        .env("CLICOLOR_FORCE", "1")
        .args(["paths", colorable_fixture().to_str().unwrap(), "--no-color"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        !stdout.contains('\x1b'),
        "--no-color must beat CLICOLOR_FORCE, got: {stdout:?}",
    );
}

#[test]
fn no_color_env_overrides_clicolor_force() {
    // NO_COLOR is checked before CLICOLOR_FORCE. Both set → NO_COLOR wins.
    let out = dq()
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "1")
        .args(["paths", colorable_fixture().to_str().unwrap()])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        !stdout.contains('\x1b'),
        "NO_COLOR must beat CLICOLOR_FORCE, got: {stdout:?}",
    );
}

#[test]
fn default_when_piped_is_no_color() {
    // No flag, no env vars (env_clear), stdout is piped → no TTY → default
    // path returns `false`, so the output has no ANSI. This is the baseline
    // every other rung overrides.
    let out = dq()
        .args(["paths", colorable_fixture().to_str().unwrap()])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        !stdout.contains('\x1b'),
        "default piped output must be uncoloured, got: {stdout:?}",
    );
}

/// Two documents that differ, for `diff --unified`.
fn differing_pair() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.yaml");
    let b = dir.path().join("b.yaml");
    std::fs::write(&a, "x: 1\n").unwrap();
    std::fs::write(&b, "x: 2\n").unwrap();
    let path = |p: PathBuf| p.to_str().expect("UTF-8 path").to_owned();
    (dir, path(a), path(b))
}

/// `diff --unified` follows the colour rule of every other command: piped
/// stdout, no flag, no env var → plain text. Before, only `--no-color`
/// turned its colour off.
#[test]
fn diff_unified_when_piped_is_no_color() {
    let (_dir, a, b) = differing_pair();
    let out = dq().args(["diff", &a, &b, "--unified"]).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("UTF-8");
    assert_eq!(
        stdout,
        format!("--- {a}\n+++ {b}\n@@ -1,3 +1,3 @@\n {{\n-  \"x\": 1\n+  \"x\": 2\n }}\n"),
    );
}

#[test]
fn diff_unified_honours_no_color_env() {
    let (_dir, a, b) = differing_pair();
    let out = dq()
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "1")
        .args(["diff", &a, &b, "--unified"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        !stdout.contains('\x1b'),
        "NO_COLOR must win, got: {stdout:?}"
    );
}

#[test]
fn diff_unified_colours_under_clicolor_force() {
    let (_dir, a, b) = differing_pair();
    let out = dq()
        .env("CLICOLOR_FORCE", "1")
        .args(["diff", &a, &b, "--unified"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("\x1b[31m-  \"x\": 1\x1b[0m"),
        "CLICOLOR_FORCE must colour the diff, got: {stdout:?}",
    );
}
