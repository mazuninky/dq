//! Integration tests for the M3 §7 bulk-mode integration of `set` / `del`.
//!
//! These tests drive `dq::run` in-process (no binary spawn) so failed
//! assertions stay debuggable. Each test seeds a fresh tempdir, invokes
//! `dq::run` with a parsed `Cli` (via `Cli::try_parse_from(...)` so the clap
//! `global = true` plumbing is exercised), then asserts on stdout, on-disk
//! state, and the marker error type returned for non-zero exit codes.
//!
//! Coverage maps to M3 §7.3 (the eight scenarios required by the spec):
//!
//! 1. Bulk `set` on N files with the same pointer/value → all modified +
//!    summary printed.
//! 2. Bulk `set` with `--continue-on-error` against a templated file →
//!    `BulkPartialFailure` (exit 7), summary `Failed: 1`.
//! 3. Bulk `del` against multiple files → all 3 modified.
//! 4. `--check` happy path: every file already has the target value → exit 0.
//! 5. `--check` mixed: 2 of 5 files would change → `CheckPending { count: 2 }`.
//! 6. `--parallel 4` smoke against 10 files → all modified, output ordering
//!    matches matched-file order.
//! 7. Glob with no matches → `dq_core::Error::Io` (kind `"io"`, exit 5).
//! 8. Bulk `--diff` mode → per-file diff prefixed by `=== <path> ===` markers.

use std::fs;

use camino::Utf8PathBuf;
use clap::Parser;
use dq::Cli;
use dq::error::{BulkPartialFailure, CheckPending};
use tempfile::TempDir;

/// Build a populated temp directory with `count` YAML files named
/// `0.yaml`..`{count-1}.yaml`, each containing `content`. Returns the
/// `TempDir` (kept alive by the caller) and the directory path so callers
/// can compose globs.
fn setup_yaml_dir(count: usize, content: &str) -> (TempDir, Utf8PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8 tmpdir");
    for i in 0..count {
        let p = root.join(format!("{i}.yaml"));
        fs::write(p.as_std_path(), content).expect("write yaml seed file");
    }
    (dir, root)
}

/// Run a parsed `Cli` end-to-end via `dq::run` and return the captured
/// stdout buffer + the result.
fn run_dq(cli: &Cli) -> (Vec<u8>, anyhow::Result<()>) {
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(cli, false, &mut out, &mut err);
    (out, result)
}

// ---------------------------------------------------------------------------
// 1. Bulk set on 5 yaml files with same pointer/value.
// ---------------------------------------------------------------------------

#[test]
fn bulk_set_in_place_modifies_every_matched_yaml_file() {
    let (_dir, root) = setup_yaml_dir(5, "spec:\n  replicas: 1\n");
    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "set",
        &glob,
        "/spec/replicas",
        "5",
        "--no-color",
    ])
    .unwrap();
    let (out, result) = run_dq(&cli);
    result.expect("bulk set should succeed");

    // Every seeded file is now `replicas: 5`.
    for i in 0..5 {
        let p = root.join(format!("{i}.yaml"));
        let contents = fs::read_to_string(p.as_std_path()).unwrap();
        assert_eq!(
            contents, "spec:\n  replicas: 5\n",
            "file {i} should be modified",
        );
    }

    // Summary line names every counter and reflects "5 modified".
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("Modified: 5, Skipped: 0, Failed: 0"),
        "expected summary line, got:\n{s}",
    );
}

// ---------------------------------------------------------------------------
// 2. Bulk set with --continue-on-error and 1 templated file.
// ---------------------------------------------------------------------------

#[test]
fn bulk_set_continue_on_error_collects_templated_failure_into_summary() {
    // Three plain YAML files plus one templated file. Without
    // `--allow-templates`, the templated file triggers `TemplatedFile`
    // (exit 3 in single-file mode); with `--continue-on-error`, the bulk
    // driver collects it into the summary and the run returns
    // `BulkPartialFailure` (exit 7).
    let (_dir, root) = setup_yaml_dir(3, "spec:\n  replicas: 1\n");
    let templated = root.join("99.yaml");
    fs::write(
        templated.as_std_path(),
        "spec:\n  replicas: {{ .Values.replicas }}\n",
    )
    .unwrap();

    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "--continue-on-error",
        "set",
        &glob,
        "/spec/replicas",
        "5",
        "--no-color",
    ])
    .unwrap();
    let (out, result) = run_dq(&cli);
    let err = result.expect_err("partial failure must surface");
    let partial = err
        .downcast_ref::<BulkPartialFailure>()
        .expect("expected BulkPartialFailure marker");
    assert_eq!(partial.failed_count, 1, "exactly one file should fail");

    // Summary lists 3 modified + 1 failed and names the bad path.
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("Modified: 3"), "got:\n{s}");
    assert!(s.contains("Failed: 1"), "got:\n{s}");
    assert!(
        s.contains(templated.as_str()),
        "summary must list the failing path:\n{s}",
    );

    // The successfully-edited files are on disk; the templated file is
    // untouched (atomic-write happens per file independently).
    for i in 0..3 {
        let p = root.join(format!("{i}.yaml"));
        let contents = fs::read_to_string(p.as_std_path()).unwrap();
        assert_eq!(contents, "spec:\n  replicas: 5\n");
    }
    let templated_after = fs::read_to_string(templated.as_std_path()).unwrap();
    assert_eq!(
        templated_after, "spec:\n  replicas: {{ .Values.replicas }}\n",
        "templated file must remain unchanged",
    );

    // Per the spec, BulkPartialFailure maps to exit 7 (WRITE_FAILED).
    assert_eq!(
        dq::exit_code::exit_code_for_error(&err),
        dq::exit_code::WRITE_FAILED,
    );
}

// ---------------------------------------------------------------------------
// 3. Bulk del on 3 files.
// ---------------------------------------------------------------------------

#[test]
fn bulk_del_in_place_removes_pointer_from_every_file() {
    let (_dir, root) = setup_yaml_dir(3, "a: 1\nb: 2\n");
    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from(["dq", "-i", "del", &glob, "/a", "--no-color"]).unwrap();
    let (out, result) = run_dq(&cli);
    result.expect("bulk del should succeed");

    for i in 0..3 {
        let p = root.join(format!("{i}.yaml"));
        let contents = fs::read_to_string(p.as_std_path()).unwrap();
        assert_eq!(contents, "b: 2\n", "file {i} should have /a removed");
    }

    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("Modified: 3, Skipped: 0, Failed: 0"),
        "expected summary line, got:\n{s}",
    );
}

// ---------------------------------------------------------------------------
// 4. --check happy path: 3 already-modified files → exit 0.
// ---------------------------------------------------------------------------

#[test]
fn bulk_check_when_every_file_already_matches_returns_ok() {
    // Files already contain the target value, so a `--check` set-with-same-
    // value run should be a no-op idempotency confirmation.
    let (_dir, root) = setup_yaml_dir(3, "spec:\n  replicas: 5\n");
    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from([
        "dq",
        "--check",
        "set",
        &glob,
        "/spec/replicas",
        "5",
        "--no-color",
    ])
    .unwrap();
    let (out, result) = run_dq(&cli);
    result.expect("check with no pending changes should be Ok");

    // Stdout reports zero pending modifications.
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("0 files would be modified"),
        "expected zero-pending message, got:\n{s}",
    );

    // Files on disk are byte-identical.
    for i in 0..3 {
        let p = root.join(format!("{i}.yaml"));
        let contents = fs::read_to_string(p.as_std_path()).unwrap();
        assert_eq!(contents, "spec:\n  replicas: 5\n");
    }
}

// ---------------------------------------------------------------------------
// 5. --check mixed: 2 need changes, 3 don't → exit 1, list 2.
// ---------------------------------------------------------------------------

#[test]
fn bulk_check_with_mixed_pending_changes_returns_check_pending_count() {
    // Three files already have the target value; two need updates. The
    // `--check` run must surface `CheckPending { count: 2 }` and list the
    // two paths that would change.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8 tmpdir");
    for i in 0..3 {
        fs::write(
            root.join(format!("ok-{i}.yaml")).as_std_path(),
            "spec:\n  replicas: 5\n",
        )
        .unwrap();
    }
    for i in 0..2 {
        fs::write(
            root.join(format!("stale-{i}.yaml")).as_std_path(),
            "spec:\n  replicas: 1\n",
        )
        .unwrap();
    }

    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from([
        "dq",
        "--check",
        "set",
        &glob,
        "/spec/replicas",
        "5",
        "--no-color",
    ])
    .unwrap();
    let (out, result) = run_dq(&cli);
    let err = result.expect_err("check with pending changes must error");
    let pending = err
        .downcast_ref::<CheckPending>()
        .expect("expected CheckPending marker");
    assert_eq!(pending.count, 2, "two files would change");

    // Stdout names the two stale paths.
    let s = String::from_utf8(out).unwrap();
    for i in 0..2 {
        let p = root.join(format!("stale-{i}.yaml"));
        assert!(
            s.contains(p.as_str()),
            "stdout should list stale-{i}.yaml, got:\n{s}",
        );
    }

    // No file is modified (check never writes).
    for i in 0..3 {
        let p = root.join(format!("ok-{i}.yaml"));
        assert_eq!(
            fs::read_to_string(p.as_std_path()).unwrap(),
            "spec:\n  replicas: 5\n",
        );
    }
    for i in 0..2 {
        let p = root.join(format!("stale-{i}.yaml"));
        assert_eq!(
            fs::read_to_string(p.as_std_path()).unwrap(),
            "spec:\n  replicas: 1\n",
            "stale file must be untouched on disk in --check",
        );
    }

    // CheckPending → exit 1 (GENERIC).
    assert_eq!(
        dq::exit_code::exit_code_for_error(&err),
        dq::exit_code::GENERIC,
    );
}

// ---------------------------------------------------------------------------
// 6. --parallel 4 smoke against 10 files.
// ---------------------------------------------------------------------------

#[test]
fn bulk_parallel_run_modifies_every_file_with_ordered_summary() {
    // Per the spec, parallel runs must end with all files modified AND
    // per-file output order matches matched-file (alphabetic) order.
    let (_dir, root) = setup_yaml_dir(10, "spec:\n  replicas: 1\n");
    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from([
        "dq",
        "-i",
        "--parallel",
        "4",
        "set",
        &glob,
        "/spec/replicas",
        "5",
        "--no-color",
    ])
    .unwrap();
    let (out, result) = run_dq(&cli);
    result.expect("parallel bulk run should succeed");

    for i in 0..10 {
        let p = root.join(format!("{i}.yaml"));
        let contents = fs::read_to_string(p.as_std_path()).unwrap();
        assert_eq!(contents, "spec:\n  replicas: 5\n");
    }

    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("Modified: 10, Skipped: 0, Failed: 0"),
        "expected summary, got:\n{s}",
    );
}

// ---------------------------------------------------------------------------
// 7. Glob no matches → exit 5 (IO_ERROR).
// ---------------------------------------------------------------------------

#[test]
fn bulk_glob_no_matches_returns_io_error_marker() {
    // A glob with metacharacters under an empty directory is a "matched
    // zero files" situation that must surface as `dq_core::Error::Io`
    // (kind `"io"`) so the exit-code mapper picks 5 (IO_ERROR).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8 tmpdir");
    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from(["dq", "-i", "set", &glob, "/x", "1", "--no-color"]).unwrap();
    let (_out, result) = run_dq(&cli);
    let err = result.expect_err("zero-match glob must error");
    let domain = err
        .downcast_ref::<dq_core::Error>()
        .expect("error should downcast to dq_core::Error");
    assert_eq!(domain.kind_name(), "io", "expected io error variant");
    assert_eq!(
        dq::exit_code::exit_code_for_error(&err),
        dq::exit_code::IO_ERROR,
    );
}

// ---------------------------------------------------------------------------
// 8. Bulk --diff mode prints per-file diff with `=== <path> ===` markers.
// ---------------------------------------------------------------------------

#[test]
fn bulk_diff_mode_emits_per_file_marker_for_each_diff() {
    let (_dir, root) = setup_yaml_dir(3, "spec:\n  replicas: 1\n");
    let glob = format!("{root}/*.yaml");
    let cli = Cli::try_parse_from([
        "dq",
        "--diff",
        "set",
        &glob,
        "/spec/replicas",
        "5",
        "--no-color",
    ])
    .unwrap();
    let (out, result) = run_dq(&cli);
    result.expect("bulk diff should succeed");

    let s = String::from_utf8(out).unwrap();
    for i in 0..3 {
        let p = root.join(format!("{i}.yaml"));
        let marker = format!("=== {p} ===");
        assert!(s.contains(&marker), "missing per-file marker for {p}: {s}",);
    }
    // Diff body markers appear as well — the per-file diffs include the
    // `-replicas: 1` and `+replicas: 5` lines.
    assert!(s.contains("-  replicas: 1"), "missing diff body: {s}");
    assert!(s.contains("+  replicas: 5"), "missing diff body: {s}");

    // Files on disk are NOT modified in diff mode.
    for i in 0..3 {
        let p = root.join(format!("{i}.yaml"));
        let contents = fs::read_to_string(p.as_std_path()).unwrap();
        assert_eq!(
            contents, "spec:\n  replicas: 1\n",
            "diff mode must not touch on-disk file {i}",
        );
    }
}
