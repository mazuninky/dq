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
//! 9. `--check` / `--diff` with `--continue-on-error` and failed files → the
//!    failures are listed as in the bulk summary, `BulkPartialFailure`.

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

// ---------------------------------------------------------------------------
// 9. --check / --diff with --continue-on-error: a failed file is listed and
//    the run exits 7.
// ---------------------------------------------------------------------------

/// YAML that does not parse: a flow sequence left open.
const MALFORMED: &str = "a: [1\n";

/// A temp directory with the given `(name, content)` files, and the glob
/// that matches every `.yaml` file in it.
fn setup_files(files: &[(&str, &str)]) -> (TempDir, Utf8PathBuf, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8 tmpdir");
    for (name, content) in files {
        fs::write(root.join(name).as_std_path(), content).expect("write seed file");
    }
    let glob = format!("{root}/*.yaml");
    (dir, root, glob)
}

/// Run `dq --no-color <args>` in-process: the exit code, stdout, and the
/// error line the binary would print (empty on success).
fn run_args(args: &[&str]) -> (i32, String, String) {
    let cli = Cli::try_parse_from(["dq", "--no-color"].iter().chain(args)).expect("clap parse");
    let (out, result) = run_dq(&cli);
    let out = String::from_utf8(out).expect("utf-8 stdout");
    match result {
        Ok(()) => (dq::exit_code::SUCCESS, out, String::new()),
        Err(err) => (
            dq::exit_code::exit_code_for_error(&err),
            out,
            err.to_string(),
        ),
    }
}

/// The `  - <path>: <error>` lines of a bulk run's stdout.
fn failure_lines(stdout: &str) -> Vec<&str> {
    stdout.lines().filter(|l| l.starts_with("  - ")).collect()
}

/// Commands that run through the bulk driver, with the arguments after the
/// glob; each leaves `a: 1` in a file that held it alone. `del` and `fix`
/// run through it too and have a test of their own.
const SAME_VALUE_COMMANDS: &[(&str, &[&str])] = &[
    ("fmt", &[]),
    ("set", &["/a", "1"]),
    ("set", &["--jq", ".a = 1"]),
    ("patch", &[r#"[{"op":"replace","path":"/a","value":1}]"#]),
    ("merge", &[r#"{"a":1}"#]),
];

#[test]
fn bulk_check_continue_on_error_lists_the_failed_file_and_exits_7() {
    // Was: `0 files would be modified`, exit 0.
    for (command, rest) in SAME_VALUE_COMMANDS {
        let (_dir, root, glob) = setup_files(&[("y.yaml", "a: 1\n"), ("z.yaml", MALFORMED)]);
        let mut args = vec!["--check", "--continue-on-error", command, &glob];
        args.extend_from_slice(rest);
        let (code, stdout, error) = run_args(&args);

        assert_eq!(code, dq::exit_code::WRITE_FAILED, "{args:?}: {stdout}");
        assert_eq!(error, "1 file(s) failed in bulk run", "{args:?}");
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 2, "{args:?}: {stdout}");
        assert_eq!(lines[0], "Modified: 0, Skipped: 1, Failed: 1", "{args:?}");
        let entry = format!("  - {}: ", root.join("z.yaml"));
        assert!(lines[1].starts_with(&entry), "{args:?}: {stdout}");

        // The same entry as the run without `--check` prints.
        let mut plain = vec!["--continue-on-error", command, &glob];
        plain.extend_from_slice(rest);
        let (code, plain_stdout, _) = run_args(&plain);
        assert_eq!(code, dq::exit_code::WRITE_FAILED, "{plain:?}");
        assert_eq!(
            failure_lines(&stdout),
            failure_lines(&plain_stdout),
            "{args:?}"
        );
    }
}

#[test]
fn bulk_check_continue_on_error_covers_del_and_fix() {
    let rule = concat!(
        "id: test.no-b\n",
        "description: no b\n",
        "severity: warn\n",
        "match:\n",
        "  format: yaml\n",
        "check:\n",
        "  jq: 'select(has(\"b\"))'\n",
        "  message: b is set\n",
        "fix:\n",
        "  jq: 'del(.b)'\n",
    );
    let (_dir, root, glob) = setup_files(&[
        ("x.yaml", "a: 1\nb: 2\n"),
        ("z.yaml", MALFORMED),
        ("rule.yml", rule),
    ]);
    let rule_path = root.join("rule.yml");
    let would = format!("would modify: {}", root.join("x.yaml"));
    let entry = format!("  - {}: ", root.join("z.yaml"));
    for args in [
        &["--check", "--continue-on-error", "del", &glob, "/b"][..],
        &[
            "--check",
            "--continue-on-error",
            "fix",
            "--rules",
            rule_path.as_str(),
            &glob,
        ],
    ] {
        let (code, stdout, error) = run_args(args);
        assert_eq!(code, dq::exit_code::WRITE_FAILED, "{args:?}: {stdout}");
        assert_eq!(error, "1 file(s) failed in bulk run", "{args:?}");
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 3, "{args:?}: {stdout}");
        assert_eq!(lines[0], would, "{args:?}");
        assert_eq!(lines[1], "Modified: 1, Skipped: 0, Failed: 1", "{args:?}");
        assert!(lines[2].starts_with(&entry), "{args:?}: {stdout}");
    }
}

#[test]
fn bulk_check_failed_file_outranks_pending_changes_serial_and_parallel() {
    // One clean file, one that would change, two that do not parse: exit 7,
    // not 1, and `--parallel 4` prints the same bytes as a serial run.
    let files = [
        ("a.yaml", MALFORMED),
        ("b.yaml", "a:   1\n"),
        ("c.yaml", "a: 1\n"),
        ("d.yaml", "b: [2\n"),
    ];
    for (command, rest) in [("fmt", &[][..]), ("set", &["--jq", ".a = 1"])] {
        let (_dir, root, glob) = setup_files(&files);
        let mut serial = vec!["--check", "--continue-on-error", command, &glob];
        serial.extend_from_slice(rest);
        let (code, stdout, error) = run_args(&serial);

        assert_eq!(code, dq::exit_code::WRITE_FAILED, "{command}: {stdout}");
        assert_eq!(error, "2 file(s) failed in bulk run", "{command}");
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 4, "{command}: {stdout}");
        assert_eq!(lines[0], format!("would modify: {}", root.join("b.yaml")));
        assert_eq!(lines[1], "Modified: 1, Skipped: 1, Failed: 2", "{command}");
        for (line, name) in lines[2..].iter().zip(["a.yaml", "d.yaml"]) {
            let entry = format!("  - {}: ", root.join(name));
            assert!(line.starts_with(&entry), "{command}: {stdout}");
        }

        let mut parallel = vec!["--parallel", "4"];
        parallel.extend_from_slice(&serial);
        for _ in 0..5 {
            assert_eq!(
                run_args(&parallel),
                (code, stdout.clone(), error.clone()),
                "{command}: --parallel 4 must equal the serial run",
            );
        }

        for (name, content) in files {
            assert_eq!(
                fs::read_to_string(root.join(name).as_std_path()).unwrap(),
                content,
                "{command}: --check writes nothing",
            );
        }
    }
}

#[test]
fn bulk_check_continue_on_error_when_every_file_fails_exits_7() {
    for (command, rest) in SAME_VALUE_COMMANDS {
        let (_dir, root, glob) = setup_files(&[("y.yaml", MALFORMED), ("z.yaml", MALFORMED)]);
        let mut args = vec!["--check", "--continue-on-error", command, &glob];
        args.extend_from_slice(rest);
        let (code, stdout, error) = run_args(&args);

        assert_eq!(code, dq::exit_code::WRITE_FAILED, "{args:?}: {stdout}");
        assert_eq!(error, "2 file(s) failed in bulk run", "{args:?}");
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 3, "{args:?}: {stdout}");
        assert_eq!(lines[0], "Modified: 0, Skipped: 0, Failed: 2", "{args:?}");
        for (line, name) in lines[1..].iter().zip(["y.yaml", "z.yaml"]) {
            let entry = format!("  - {}: ", root.join(name));
            assert!(line.starts_with(&entry), "{args:?}: {stdout}");
        }
    }
}

#[test]
fn bulk_check_continue_on_error_without_a_failed_file_is_unchanged() {
    // No failed file: `--continue-on-error` changes neither the output nor
    // the exit code of `--check`.
    for flags in [&["--check"][..], &["--check", "--continue-on-error"]] {
        for (command, rest) in SAME_VALUE_COMMANDS {
            let (_dir, _root, glob) = setup_files(&[("y.yaml", "a: 1\n"), ("z.yaml", "a: 1\n")]);
            let mut args = flags.to_vec();
            args.extend_from_slice(&[command, &glob]);
            args.extend_from_slice(rest);
            assert_eq!(
                run_args(&args),
                (
                    dq::exit_code::SUCCESS,
                    "0 files would be modified\n".to_owned(),
                    String::new(),
                ),
                "{args:?}",
            );
        }

        for (command, rest) in [("fmt", &[][..]), ("set", &["--jq", ".a = 1"])] {
            let (_dir, root, glob) = setup_files(&[("y.yaml", "a:   1\n"), ("z.yaml", "a: 1\n")]);
            let mut args = flags.to_vec();
            args.extend_from_slice(&[command, &glob]);
            args.extend_from_slice(rest);
            assert_eq!(
                run_args(&args),
                (
                    dq::exit_code::GENERIC,
                    format!("would modify: {}\n", root.join("y.yaml")),
                    "1 file(s) would be modified".to_owned(),
                ),
                "{args:?}",
            );
        }
    }
}

#[test]
fn bulk_check_without_continue_on_error_stops_at_the_first_failing_file() {
    for (command, rest) in SAME_VALUE_COMMANDS {
        let (_dir, _root, glob) = setup_files(&[("y.yaml", "a:   1\n"), ("z.yaml", MALFORMED)]);
        let mut args = vec!["--check", command, &glob];
        args.extend_from_slice(rest);
        let (code, stdout, _) = run_args(&args);
        assert_eq!(code, dq::exit_code::PARSE_ERROR, "{args:?}: {stdout}");
        assert_eq!(stdout, "", "{args:?}");
    }
}

#[test]
fn check_continue_on_error_on_a_single_failing_file_keeps_its_own_error() {
    // A glob that matches exactly one file is a single-file run: the flag
    // changes nothing, no summary is printed.
    let (_dir, root, glob) = setup_files(&[("z.yaml", MALFORMED)]);
    let file = root.join("z.yaml");
    for target in [glob.as_str(), file.as_str()] {
        let (code, stdout, _) = run_args(&["--check", "--continue-on-error", "fmt", target]);
        assert_eq!(code, dq::exit_code::PARSE_ERROR, "{target}: {stdout}");
        assert_eq!(stdout, "", "{target}");
    }
}

#[test]
fn bulk_check_continue_on_error_with_a_doc_skip_still_reports_the_failure() {
    // The one readable file lacks document 1 and is skipped; the other
    // fails. Not "every file lacks it" (exit 2), but a failed file.
    let (_dir, root, glob) = setup_files(&[("y.yaml", "a: 1\n"), ("z.yaml", MALFORMED)]);
    let (code, stdout, error) = run_args(&[
        "--check",
        "--continue-on-error",
        "--doc",
        "1",
        "del",
        &glob,
        "/a",
    ]);
    assert_eq!(code, dq::exit_code::WRITE_FAILED, "{stdout}");
    assert_eq!(error, "1 file(s) failed in bulk run");
    let expected = format!(
        "Modified: 0, Skipped: 1, Failed: 1\n  - {}: ",
        root.join("z.yaml")
    );
    assert!(stdout.starts_with(&expected), "{stdout}");
}

#[test]
fn bulk_diff_continue_on_error_lists_the_failed_file_and_exits_7() {
    for (command, rest) in [("fmt", &[][..]), ("set", &["--jq", ".a = 1"])] {
        let (_dir, root, glob) = setup_files(&[
            ("a.yaml", MALFORMED),
            ("b.yaml", "a:   1\n"),
            ("c.yaml", "a: 1\n"),
        ]);
        let mut serial = vec!["--diff", "--continue-on-error", command, &glob];
        serial.extend_from_slice(rest);
        let (code, stdout, error) = run_args(&serial);

        assert_eq!(code, dq::exit_code::WRITE_FAILED, "{command}: {stdout}");
        assert_eq!(error, "1 file(s) failed in bulk run", "{command}");
        let marker = format!("=== {} ===\n", root.join("b.yaml"));
        assert!(stdout.starts_with(&marker), "{command}: {stdout}");
        assert!(
            stdout.contains("-a:   1\n+a: 1\n"),
            "{command}: missing diff body: {stdout}",
        );
        let summary = format!(
            "\nModified: 1, Skipped: 1, Failed: 1\n  - {}: ",
            root.join("a.yaml")
        );
        assert!(stdout.contains(&summary), "{command}: {stdout}");

        let mut parallel = vec!["--parallel", "4"];
        parallel.extend_from_slice(&serial);
        assert_eq!(run_args(&parallel), (code, stdout, error), "{command}");
    }
}
