//! Atomic-write side-effect tests.
//!
//! These confirm the contract of `dq_core::atomic_write::write` end-to-end
//! through the binary:
//! - After `dq set f.yaml /x 1 -i`, the target file holds the new content
//!   AND no `tempfile`-style intermediate files (`.tmp.<random>`) remain in
//!   the parent directory.
//! - With `--backup`, both the target file and the corresponding `.bak` file
//!   exist with the right content.
//!
//! NOTE: simulating a SIGKILL mid-rename to verify atomicity in the strict
//! "no torn writes" sense is out of scope for the Rust test harness — that
//! would need an external supervisor. M2 tasks.md flags this as a TODO.

use std::path::PathBuf;
use std::process::Command as StdCommand;

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

/// Read every file in `dir` and return their (relative) names sorted. Used
/// to assert "only these files are present, nothing else".
fn list_files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn in_place_set_leaves_no_temp_files_behind() {
    // Seed an isolated tempdir with one YAML file so we can assert "exactly
    // these files exist after the write". If atomic_write leaks a `.tmp.*`
    // file, the post-condition will catch it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path: PathBuf = dir.path().join("config.yaml");
    std::fs::write(&path, "spec:\n  replicas: 3\n").expect("seed file");

    let path_str = path.to_str().unwrap();
    dq().args(["set", path_str, "/spec/replicas", "5", "-i", "--no-color"])
        .assert()
        .success();

    // File contents updated.
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("replicas: 5"),
        "in-place mutation must apply, got:\n{contents}",
    );

    // Only the original file remains — no `.tmp.<random>` leftovers.
    let files = list_files(dir.path());
    assert_eq!(
        files,
        vec!["config.yaml".to_owned()],
        "tempdir must contain only the target file, found: {files:?}",
    );
}

#[test]
fn backup_flag_creates_bak_file_alongside() {
    // With `-i --backup`, the target file is updated AND a `.bak` sibling
    // contains the original bytes. The backup-path rule in
    // `atomic_write::backup_path_for` is "always append .bak", so
    // `config.yaml` → `config.yaml.bak`.
    let dir = tempfile::tempdir().expect("tempdir");
    let path: PathBuf = dir.path().join("config.yaml");
    let original = "spec:\n  replicas: 3\n";
    std::fs::write(&path, original).expect("seed file");

    let path_str = path.to_str().unwrap();
    dq().args([
        "set",
        path_str,
        "/spec/replicas",
        "5",
        "-i",
        "--backup",
        "--no-color",
    ])
    .assert()
    .success();

    // Target file updated.
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("replicas: 5"),
        "in-place mutation must apply, got:\n{contents}",
    );

    // Backup contains the original bytes.
    let bak_path = dir.path().join("config.yaml.bak");
    let bak_contents = std::fs::read_to_string(&bak_path)
        .unwrap_or_else(|e| panic!("backup file missing at {}: {e}", bak_path.display()));
    assert_eq!(bak_contents, original, "backup must contain original bytes",);

    // Exactly two files in the dir: the target and the .bak. No leftover
    // tempfiles from atomic_write.
    let files = list_files(dir.path());
    assert_eq!(
        files,
        vec!["config.yaml".to_owned(), "config.yaml.bak".to_owned()],
        "tempdir must contain only target + backup, found: {files:?}",
    );
}

// Reference an unused std::process::Command so the import does not become
// dead-code if the assert_cmd shape ever changes.
#[allow(dead_code)]
fn _std_command_unused() -> StdCommand {
    StdCommand::new("true")
}
