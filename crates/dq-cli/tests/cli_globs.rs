//! Glob expansion and bulk accounting, end to end through the binary.
//!
//! - A pattern with no directory part (`2-*.yaml`) matches in the current
//!   directory.
//! - `*` and `?` stop at `/`; `**` crosses directories; `[...]` and
//!   `{a,b}` still work; an existing path is taken literally even when it
//!   holds a metacharacter.
//! - A file whose bytes would not change is neither rewritten nor counted
//!   as Modified: it lands under Skipped and keeps its mtime.
//! - `convert` to stdout takes a glob only when it matches one file.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

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

/// A fixed point in the past, far from "now", so a rewrite is visible.
fn past() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

fn set_mtime(path: &Path, when: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open for mtime")
        .set_modified(when)
        .expect("set mtime");
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .expect("metadata")
        .modified()
        .expect("mtime")
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

// ---------------------------------------------------------------------------
// Item 3: unchanged files are not rewritten and not counted as Modified.
// ---------------------------------------------------------------------------

#[test]
fn bulk_in_place_leaves_unchanged_files_alone_and_counts_them_skipped() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "1.yaml", "a: 1\n");
    seed(dir.path(), "2.yaml", "a: 2\n");
    seed(dir.path(), "3.yaml", "a: 2\n");
    for name in ["1.yaml", "2.yaml", "3.yaml"] {
        set_mtime(&dir.path().join(name), past());
    }

    dq().current_dir(dir.path())
        .args(["-i", "--backup", "--no-color", "set", "*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout("Modified: 1, Skipped: 2, Failed: 0\n");

    assert_eq!(read(dir.path(), "1.yaml"), "a: 2\n");
    assert_ne!(mtime(&dir.path().join("1.yaml")), past());
    assert!(dir.path().join("1.yaml.bak").exists());
    for name in ["2.yaml", "3.yaml"] {
        assert_eq!(
            mtime(&dir.path().join(name)),
            past(),
            "{name} was rewritten"
        );
        assert!(
            !dir.path().join(format!("{name}.bak")).exists(),
            "{name} got a backup"
        );
    }
}

#[test]
fn single_file_in_place_does_not_rewrite_an_unchanged_file() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "1.yaml", "a: 2\n");
    let path = dir.path().join("1.yaml");
    set_mtime(&path, past());

    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "1.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(mtime(&path), past());
}

#[test]
fn bulk_diff_counts_unchanged_files_skipped_and_prints_no_marker_for_them() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "1.yaml", "a: 1\n");
    seed(dir.path(), "2.yaml", "a: 2\n");

    dq().current_dir(dir.path())
        .args(["--diff", "--no-color", "set", "*.yaml", "/a", "2"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("=== 1.yaml ===")
                .and(predicate::str::contains("=== 2.yaml ===").not())
                .and(predicate::str::contains(
                    "Modified: 1, Skipped: 1, Failed: 0",
                )),
        );
}

// ---------------------------------------------------------------------------
// Item 4: `convert` to stdout and globs.
// ---------------------------------------------------------------------------

#[test]
fn convert_to_stdout_takes_a_glob_that_matches_one_file() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "g/a.yaml", "a: 1\n");

    dq().current_dir(dir.path())
        .args(["--no-color", "convert", "g/*.yaml", "-F", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"a\": 1"));
}

#[test]
fn convert_to_stdout_refuses_a_glob_that_matches_several_files() {
    let dir = TempDir::new().expect("tempdir");
    seed(dir.path(), "g/a.yaml", "a: 1\n");
    seed(dir.path(), "g/b.yaml", "b: 1\n");

    dq().current_dir(dir.path())
        .args(["--no-color", "convert", "g/*.yaml", "-F", "json"])
        .assert()
        .code(6)
        .stdout("")
        .stderr(
            predicate::str::contains("convert to stdout takes one file")
                .and(predicate::str::contains("matched 2")),
        );
}
