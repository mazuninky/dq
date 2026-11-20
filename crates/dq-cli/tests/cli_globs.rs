//! Glob expansion and bulk accounting, end to end through the binary.
//!
//! - A pattern with no directory part (`2-*.yaml`) matches in the current
//!   directory.
//! - `*` and `?` stop at `/`; `**` crosses directories; `[...]` and
//!   `{a,b}` still work; an existing path is taken literally even when it
//!   holds a metacharacter.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
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

/// Write `content` to `dir/rel`, creating parent directories.
fn seed(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, content).expect("write fixture");
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).expect("read back")
}

// ---------------------------------------------------------------------------
// Item 1: a pattern with no directory part.
// ---------------------------------------------------------------------------

#[test]
fn glob_without_directory_part_matches_in_the_current_directory() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "2-a.yaml", "a: 1\n");
    seed(dir.path(), "2-b.yaml", "a: 1\n");
    seed(dir.path(), "3-c.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "2-*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Modified: 2, Skipped: 0, Failed: 0",
        ));
    assert_eq!(read(dir.path(), "2-a.yaml"), "a: 2\n");
    assert_eq!(read(dir.path(), "2-b.yaml"), "a: 2\n");
    assert_eq!(read(dir.path(), "3-c.yaml"), "a: 1\n");
}

#[test]
fn lint_inputs_without_directory_part_match_in_the_current_directory() {
    // `fix --check` expands its inputs through the lint pipeline's glob
    // expansion; before the fix `2-*.yaml` matched nothing there (exit 6).
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "2-a.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["--no-color", "fix", "--check", "2-*.yaml"])
        .assert()
        .success();
}

#[test]
fn matched_paths_keep_the_pattern_spelling() {
    // A cwd-relative pattern names files as `2-a.yaml`, not `./2-a.yaml`.
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "2-a.yaml", "a: 1\n");
    seed(dir.path(), "2-b.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["--no-color", "set", "2-*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("=== 2-a.yaml ===\na: 2\n=== 2-b.yaml ===\na: 2\n")
                .and(predicate::str::contains("./").not()),
        );
}

// ---------------------------------------------------------------------------
// Item 2: `*` and `?` stop at `/`; `**` crosses.
// ---------------------------------------------------------------------------

#[test]
fn star_does_not_cross_a_directory() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "k8s/a.yaml", "a: 1\n");
    seed(dir.path(), "k8s/b.yaml", "a: 1\n");
    seed(dir.path(), "k8s/sub/x.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "k8s/*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Modified: 2, Skipped: 0, Failed: 0",
        ));
    assert_eq!(read(dir.path(), "k8s/a.yaml"), "a: 2\n");
    assert_eq!(read(dir.path(), "k8s/sub/x.yaml"), "a: 1\n");
}

#[test]
fn star_in_a_cwd_pattern_does_not_descend() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "a.yaml", "a: 1\n");
    seed(dir.path(), "b.yaml", "a: 1\n");
    seed(dir.path(), "sub/x.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Modified: 2, Skipped: 0, Failed: 0",
        ));
    assert_eq!(read(dir.path(), "sub/x.yaml"), "a: 1\n");
}

#[test]
fn question_mark_does_not_match_a_slash() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "k8s/a/b.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "k8s/a?b.yaml", "/a", "2"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("matched zero files"));
    assert_eq!(read(dir.path(), "k8s/a/b.yaml"), "a: 1\n");
}

#[test]
fn double_star_crosses_directories() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "k8s/a.yaml", "a: 1\n");
    seed(dir.path(), "k8s/sub/x.yaml", "a: 1\n");
    seed(dir.path(), "k8s/sub/deeper/y.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "k8s/**/*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Modified: 3, Skipped: 0, Failed: 0",
        ));
    assert_eq!(read(dir.path(), "k8s/sub/deeper/y.yaml"), "a: 2\n");
}

#[test]
fn classes_and_alternation_still_match() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "k8s/a.yaml", "a: 1\n");
    seed(dir.path(), "k8s/b.yaml", "a: 1\n");
    seed(dir.path(), "k8s/c.yaml", "a: 1\n");
    seed(dir.path(), "other/d.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["--no-color", "set", "k8s/[ab].yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Modified: 2, Skipped: 0, Failed: 0",
        ));
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "k8s/{a,c}.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("=== k8s/a.yaml ===")
                .and(predicate::str::contains("=== k8s/c.yaml ==="))
                .and(predicate::str::contains(
                    "Modified: 2, Skipped: 0, Failed: 0",
                )),
        );
    dq().current_dir(dir.path())
        .args(["--no-color", "set", "{k8s,other}/*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("=== other/d.yaml ===").and(predicate::str::contains(
                "Modified: 4, Skipped: 0, Failed: 0",
            )),
        );
}

#[test]
fn an_existing_path_with_a_bracket_is_taken_literally() {
    // A shell-expanded path is a plain path even when it holds `[`.
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "a[1].yaml", "a: 1\n");
    seed(dir.path(), "a1.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "a[1].yaml", "/a", "2"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(read(dir.path(), "a[1].yaml"), "a: 2\n");
    assert_eq!(read(dir.path(), "a1.yaml"), "a: 1\n");
}
