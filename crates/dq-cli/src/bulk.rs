//! Multi-file driver: glob expansion + parallel runner + summary reporter.
//!
//! Every write command (`set`, `del`, `patch`, `merge`, `convert -i`) goes
//! through [`run_per_file`] so the contract for `--continue-on-error`,
//! `--parallel`, `--check`, summary reporting, and exit-code aggregation is
//! documented and tested in one place. Command handlers stay thin: they
//! build an [`FileOp`] adapter (capturing their resolved args) and hand it
//! to the driver.
//!
//! The driver buffers per-file output in `Vec<u8>` and flushes serially in
//! matched-file order at the end of the run. This satisfies the spec's
//! "per-file output ordering matches matched-file order regardless of
//! execution order" requirement (see
//! `docs/reference/bulk.md`, and the `--parallel` decision in the
//! add-bulk-and-ci section of `docs/decisions.md`).

use std::io::{self, Write};

use camino::{Utf8Path, Utf8PathBuf};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::cli::Cli;
use crate::commands::io_helpers::every_file_lacks;
use crate::error::{BulkPartialFailure, CheckPending, DocOutOfRange};
use crate::glob::{self, GlobError};

/// Per-file outcome reported by an [`FileOp`] implementation.
#[derive(Debug)]
pub enum FileOpResult {
    /// File was processed (or in `--check` mode, would be processed).
    /// `output_bytes` is the new file contents — always populated, because
    /// `--check` needs them for the byte-equality comparison and `-i` needs
    /// them for the atomic write.
    /// `diff` is populated only when `--diff` mode is active and the op
    /// computes a per-file diff string.
    Modified {
        /// Final byte representation of the file after the op.
        output_bytes: Vec<u8>,
        /// Optional unified-diff string for `--diff` mode.
        diff: Option<String>,
    },
    /// File is byte-identical to its prospective output (idempotent run).
    /// Counted under `Skipped`, nothing printed. An op may return
    /// [`FileOpResult::Modified`] with the same bytes instead: the driver
    /// compares them with the file and treats that as unchanged too.
    Unchanged,
    /// In a bulk run, the file lacks the document `--doc N` names. The
    /// driver counts it under `Skipped` and notes the reason on stderr (a
    /// `tracing::warn!` line), keeping stdout for the files' output — or,
    /// when *every* file of the run lacks it, fails the run with
    /// [`DocOutOfRangeEverywhere`](crate::error::DocOutOfRangeEverywhere)
    /// (exit 2) before writing anything.
    Skipped(DocOutOfRange),
}

/// Strategy interface implemented by each command handler. The trait is
/// `Sync` so rayon can spread it across worker threads.
pub trait FileOp: Sync {
    /// Apply the op to `path`, returning the new bytes (and optionally a
    /// diff string).
    ///
    /// The op MUST NOT write to disk. The driver handles `-i` (atomic
    /// write), `--diff` (stdout printing), and `--check` (compare-only)
    /// uniformly. Returning [`FileOpResult::Modified`] always — regardless
    /// of mode — is the contract.
    ///
    /// # Errors
    ///
    /// Any error returned here bubbles up through the driver. With
    /// `--continue-on-error` it is collected into the per-file failure
    /// list; otherwise the bulk run aborts and the error is propagated to
    /// the caller verbatim.
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult>;
}

/// Expand a glob pattern (or pass through a literal path).
///
/// Behaviour:
/// - Not a glob ([`crate::glob::is_glob`]: no `*`/`?`/`[`/`{`, or a file
///   exists at that literal path) → returns `vec![pattern.to_owned()]`; the
///   command handler hits the real I/O error downstream if the file is
///   missing.
/// - Otherwise the matching files, sorted ([`crate::glob::expand`]: `*` and
///   `?` stop at `/`, `**` crosses directories).
///
/// # Errors
///
/// - [`dq_core::Error::Io`] (kind `"io"`, exit 5) when the walk root does
///   not exist or the glob matched no files.
/// - `anyhow::Error` wrapping the `globset` error when the pattern is
///   syntactically invalid.
pub fn expand_glob(pattern: &Utf8Path) -> anyhow::Result<Vec<Utf8PathBuf>> {
    if !glob::is_glob(pattern) {
        return Ok(vec![pattern.to_owned()]);
    }
    glob::expand(pattern).map_err(|err| match err {
        GlobError::Invalid { pattern, source } => {
            anyhow::Error::new(source).context(format!("invalid glob pattern: {pattern}"))
        }
        GlobError::RootMissing { ref root } => anyhow::Error::new(dq_core::Error::Io {
            path: root.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, err.to_string()),
        }),
        GlobError::NoMatch { ref pattern } => anyhow::Error::new(dq_core::Error::Io {
            path: pattern.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, err.to_string()),
        }),
    })
}

/// Resolve `cli.parallel` into a concrete worker count.
///
/// - `None` → `1` (sequential default).
/// - `Some(0)` → `rayon::current_num_threads()`.
/// - `Some(n)` → `n`.
fn resolve_parallel(n: Option<usize>) -> usize {
    match n {
        None => 1,
        Some(0) => rayon::current_num_threads(),
        Some(n) => n,
    }
}

/// Run `op` against every file in `files`, honoring `cli.parallel`,
/// `cli.continue_on_error`, `cli.check`, `cli.in_place`, `cli.diff`, and
/// `cli.backup`.
///
/// Behaviour summary:
/// - **Single file (`files.len() == 1`)**: no summary line. Output goes
///   straight to `out` so the M2 byte-identical contract is preserved for
///   non-glob single-file calls. `--continue-on-error` is a no-op in this
///   mode (an error still aborts).
/// - **Bulk (`files.len() > 1`)**: per-file output is buffered in
///   matched-file order; a `Modified: N, Skipped: M, Failed: K` summary
///   line follows the per-file output. Skipped counts files whose bytes
///   would not change (never rewritten, so their mtime stays) and files
///   without the `--doc N` document. `--diff` and stdout-mode bulk
///   output are prefixed with `=== <path> ===` markers so consumers can
///   tell files apart.
/// - **`--check`**: never writes to disk; the driver compares each op's
///   `output_bytes` to the on-disk source bytes and accumulates a list of
///   files that would change. Returns [`CheckPending`] with that count
///   (mapped to exit 1) if any file would change; `Ok(())` otherwise.
/// - **`--continue-on-error`**: per-file errors are collected instead of
///   aborting; if any failed, the function returns
///   [`BulkPartialFailure`] (mapped to exit 7). Without the flag, the
///   first error short-circuits.
///
/// # Errors
///
/// - [`CheckPending`] when `--check` finds files that would be modified.
/// - [`BulkPartialFailure`] when `--continue-on-error` completes with one
///   or more file failures.
/// - [`DocOutOfRangeEverywhere`](crate::error::DocOutOfRangeEverywhere)
///   when every file of a bulk run lacks the document `--doc N` names
///   (each op returned [`FileOpResult::Skipped`]).
/// - The first per-file error verbatim when `--continue-on-error` is off.
/// - I/O errors from writing to `out` or from the per-file atomic write.
pub fn run_per_file(
    files: Vec<Utf8PathBuf>,
    op: &dyn FileOp,
    cli: &Cli,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let is_bulk = files.len() > 1;
    let parallel = resolve_parallel(cli.parallel);

    // Apply the op to every file, collecting results in matched-file
    // order. Sequential when `parallel == 1` or when there's just one
    // file — avoids spinning up a rayon pool for the common case.
    let results: Vec<(Utf8PathBuf, anyhow::Result<FileOpResult>)> =
        if parallel <= 1 || files.len() <= 1 {
            files.iter().map(|f| (f.clone(), op.apply(f))).collect()
        } else {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(parallel)
                .build()?;
            pool.install(|| {
                files
                    .par_iter()
                    .map(|f| (f.clone(), op.apply(f)))
                    .collect::<Vec<_>>()
            })
        };

    // Aggregation state.
    let mut modified: usize = 0;
    let mut skipped: usize = 0;
    let mut failed: Vec<(Utf8PathBuf, anyhow::Error)> = Vec::new();
    let mut would_modify: Vec<Utf8PathBuf> = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    let mut missing: Vec<(Utf8PathBuf, DocOutOfRange)> = Vec::new();

    for (path, result) in results {
        match result {
            Ok(FileOpResult::Modified { output_bytes, diff }) => {
                if cli.check {
                    handle_check(&path, &output_bytes, &mut would_modify)?;
                    continue;
                }
                // A file whose bytes would not change is never rewritten
                // (its mtime and any `.bak` stay as they are) and counts
                // under Skipped, like an op's own `Unchanged`.
                let unchanged = source_matches(&path, &output_bytes);
                if cli.in_place {
                    if unchanged {
                        skipped += 1;
                        continue;
                    }
                    match dq_core::atomic_write::write(&path, &output_bytes, cli.backup) {
                        Ok(()) => modified += 1,
                        Err(e) => {
                            if cli.continue_on_error && is_bulk {
                                failed.push((path.clone(), anyhow::Error::new(e)));
                            } else {
                                return Err(anyhow::Error::new(e));
                            }
                        }
                    }
                } else if cli.diff {
                    if unchanged {
                        skipped += 1;
                        continue;
                    }
                    if let Some(d) = diff {
                        if is_bulk {
                            writeln!(buffer, "=== {path} ===")?;
                        }
                        buffer.write_all(d.as_bytes())?;
                    }
                    modified += 1;
                } else {
                    // Plain stdout mode: the output is the command's
                    // result, so every file is printed, changed or not.
                    // In bulk we prefix a marker so streams can be split
                    // per file; single-file mode matches M2 byte-for-byte
                    // (no marker, no summary).
                    if is_bulk {
                        writeln!(buffer, "=== {path} ===")?;
                    }
                    buffer.write_all(&output_bytes)?;
                    if unchanged {
                        skipped += 1;
                    } else {
                        modified += 1;
                    }
                }
            }
            Ok(FileOpResult::Unchanged) => {
                skipped += 1;
            }
            Ok(FileOpResult::Skipped(reason)) => {
                skipped += 1;
                missing.push((path, reason));
            }
            Err(e) => {
                if cli.continue_on_error && is_bulk {
                    failed.push((path, e));
                } else {
                    // Single-file mode or no continue-on-error: short-
                    // circuit and propagate the error verbatim. We do NOT
                    // flush the buffer here — partial output before an
                    // abort would mislead callers.
                    return Err(e);
                }
            }
        }
    }

    // Every file lacked the `--doc N` document: nothing was done, so the
    // run fails as a single such file does. Skipped files are never
    // written, so nothing reached the disk either.
    if let Some(err) = every_file_lacks(&missing, files.len()) {
        return Err(anyhow::Error::new(err));
    }
    for (path, reason) in &missing {
        tracing::warn!("skipped {path}: {reason}");
    }

    out.write_all(&buffer)?;

    // `--check` short-circuits before any summary.
    if cli.check {
        for p in &would_modify {
            writeln!(out, "would modify: {p}")?;
        }
        let count = would_modify.len();
        if count > 0 {
            return Err(anyhow::Error::new(CheckPending { count }));
        }
        if is_bulk {
            writeln!(out, "0 files would be modified")?;
        }
        return Ok(());
    }

    // Bulk summary follows per-file output. Single-file invocations omit
    // it so the M2 byte-identical contract holds.
    if is_bulk {
        writeln!(
            out,
            "Modified: {modified}, Skipped: {skipped}, Failed: {}",
            failed.len(),
        )?;
        for (path, err) in &failed {
            writeln!(out, "  - {path}: {err:#}")?;
        }
    }

    if !failed.is_empty() {
        return Err(anyhow::Error::new(BulkPartialFailure {
            failed_count: failed.len(),
        }));
    }
    Ok(())
}

/// Whether the file at `path` already holds exactly `output_bytes`. A file
/// that cannot be read counts as different, so the write that follows
/// reports the real error.
fn source_matches(path: &Utf8Path, output_bytes: &[u8]) -> bool {
    std::fs::read(path.as_std_path()).is_ok_and(|source| source == output_bytes)
}

/// Compare `output_bytes` to the file's on-disk bytes and append `path` to
/// `would_modify` if they differ. Read failures are bubbled up as
/// [`dq_core::Error::Io`] (read-side, IO_ERROR=5) — they are NOT
/// reclassified as write errors.
fn handle_check(
    path: &Utf8Path,
    output_bytes: &[u8],
    would_modify: &mut Vec<Utf8PathBuf>,
) -> anyhow::Result<()> {
    let source_bytes = std::fs::read(path.as_std_path()).map_err(|source| {
        anyhow::Error::new(dq_core::Error::Io {
            path: path.to_owned(),
            source,
        })
    })?;
    if source_bytes != output_bytes {
        would_modify.push(path.to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    /// Minimal `FileOp` for driver tests: returns a constant `output_bytes`
    /// payload regardless of input. Counts invocations so tests can assert
    /// the driver visited every file.
    struct ConstOp {
        bytes: Vec<u8>,
        diff: Option<String>,
        calls: AtomicUsize,
    }

    impl ConstOp {
        fn new(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                diff: None,
                calls: AtomicUsize::new(0),
            }
        }

        fn with_diff(bytes: &[u8], diff: &str) -> Self {
            Self {
                bytes: bytes.to_vec(),
                diff: Some(diff.to_owned()),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl FileOp for ConstOp {
        fn apply(&self, _path: &Utf8Path) -> anyhow::Result<FileOpResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(FileOpResult::Modified {
                output_bytes: self.bytes.clone(),
                diff: self.diff.clone(),
            })
        }
    }

    /// `FileOp` that fails on a specific path. Used to exercise the
    /// `--continue-on-error` aggregator.
    struct FailOnPath {
        bad: Utf8PathBuf,
        bytes: Vec<u8>,
    }

    impl FileOp for FailOnPath {
        fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
            if path == self.bad {
                anyhow::bail!("synthetic failure for {path}")
            }
            Ok(FileOpResult::Modified {
                output_bytes: self.bytes.clone(),
                diff: None,
            })
        }
    }

    /// Build a `Cli` for tests. Extra args go before the subcommand;
    /// `set` plus dummy positionals satisfy clap's required-argument
    /// validation.
    fn cli_for(extra: &[&str]) -> Cli {
        let mut argv = vec!["dq"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["set", "ignored.yaml", "/x", "1"]);
        Cli::try_parse_from(argv).expect("clap parse")
    }

    #[test]
    fn expand_glob_literal_path_returns_single_element_vec_without_fs_access() {
        // Literal path: no metacharacters, no FS access. Even a path that
        // does NOT exist must still pass through (the command handler
        // will surface the IO error downstream).
        let path = Utf8PathBuf::from("/no/such/path/that/exists.yaml");
        let result = expand_glob(&path).expect("literal path should pass through");
        assert_eq!(result, vec![path]);
    }

    #[test]
    fn expand_glob_with_metachars_walks_and_filters() {
        // Populate a temp directory with mixed extensions and assert only
        // the YAML files are returned, sorted alphabetically.
        let dir = tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        for name in ["a.yaml", "b.yaml", "c.json", "d.txt"] {
            fs::write(root.join(name).as_std_path(), b"x: 1\n").unwrap();
        }
        // Build the pattern with forward slashes so it works on both Unix
        // (native) and Windows. `Utf8PathBuf::join` uses the platform
        // separator, which on Windows produces `…\tmpXXX\**/*.yaml` — a
        // mix of backslashes and forward slashes that the `glob` crate
        // matches literally and finds zero files. The `glob` crate
        // accepts `/` on Windows and matches against any path separator.
        let pattern = Utf8PathBuf::from(format!("{}/**/*.yaml", root.as_str().replace('\\', "/")));
        let mut result = expand_glob(&pattern).expect("glob should match yaml files");
        // Compare basenames only — the absolute prefix varies by tempdir
        // location.
        let mut names: Vec<String> = result
            .iter_mut()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.yaml", "b.yaml"]);
    }

    #[test]
    fn expand_glob_zero_matches_returns_io_error() {
        let dir = tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let pattern = root.join("**/*.yaml");
        let err = expand_glob(&pattern).expect_err("empty match must error");
        let domain = err
            .downcast_ref::<dq_core::Error>()
            .expect("error should downcast to dq_core::Error so exit code is 5");
        assert_eq!(domain.kind_name(), "io");
    }

    #[test]
    fn run_per_file_single_file_emits_no_summary() {
        // Non-bulk path: output is byte-identical to the op's bytes, with
        // no `Modified: ...` summary appended.
        let dir = tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("only.yaml")).unwrap();
        fs::write(path.as_std_path(), b"a: 1\n").unwrap();

        let op = ConstOp::new(b"a: 2\n");
        let cli = cli_for(&[]);
        let mut out: Vec<u8> = Vec::new();
        run_per_file(vec![path.clone()], &op, &cli, &mut out).expect("single-file run");

        assert_eq!(op.call_count(), 1);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "a: 2\n", "single-file mode must not add a summary");
        assert!(!s.contains("Modified:"));
    }

    #[test]
    fn run_per_file_bulk_in_place_writes_summary_and_files() {
        // Two-file bulk run with `-i`: both files are atomic-written and a
        // `Modified: 2` summary appears on stdout.
        let dir = tempdir().unwrap();
        let p1 = Utf8PathBuf::from_path_buf(dir.path().join("a.yaml")).unwrap();
        let p2 = Utf8PathBuf::from_path_buf(dir.path().join("b.yaml")).unwrap();
        fs::write(p1.as_std_path(), b"x: 1\n").unwrap();
        fs::write(p2.as_std_path(), b"x: 1\n").unwrap();

        let op = ConstOp::new(b"x: 2\n");
        let cli = cli_for(&["-i"]);
        let mut out: Vec<u8> = Vec::new();
        run_per_file(vec![p1.clone(), p2.clone()], &op, &cli, &mut out).expect("bulk in-place run");

        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("Modified: 2, Skipped: 0, Failed: 0"),
            "expected summary, got: {s}",
        );
        assert_eq!(fs::read(p1.as_std_path()).unwrap(), b"x: 2\n");
        assert_eq!(fs::read(p2.as_std_path()).unwrap(), b"x: 2\n");
    }

    #[test]
    fn run_per_file_bulk_stdout_includes_per_file_markers() {
        // Bulk + plain stdout mode → each file's output is preceded by
        // `=== <path> ===` so the consumer can demultiplex.
        let dir = tempdir().unwrap();
        let p1 = Utf8PathBuf::from_path_buf(dir.path().join("a.yaml")).unwrap();
        let p2 = Utf8PathBuf::from_path_buf(dir.path().join("b.yaml")).unwrap();
        fs::write(p1.as_std_path(), b"x: 1\n").unwrap();
        fs::write(p2.as_std_path(), b"x: 1\n").unwrap();

        let op = ConstOp::new(b"x: 2\n");
        let cli = cli_for(&[]);
        let mut out: Vec<u8> = Vec::new();
        run_per_file(vec![p1.clone(), p2.clone()], &op, &cli, &mut out).expect("bulk stdout run");

        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("=== "), "expected per-file marker in: {s}");
        assert!(s.contains(p1.as_str()), "missing path 1 in: {s}");
        assert!(s.contains(p2.as_str()), "missing path 2 in: {s}");
        assert!(s.contains("Modified: 2"));
    }

    #[test]
    fn run_per_file_check_with_changes_returns_check_pending() {
        // `--check` mode: source bytes differ from op output → CheckPending
        // with the right count, no FS write happens.
        let dir = tempdir().unwrap();
        let p1 = Utf8PathBuf::from_path_buf(dir.path().join("a.yaml")).unwrap();
        let p2 = Utf8PathBuf::from_path_buf(dir.path().join("b.yaml")).unwrap();
        fs::write(p1.as_std_path(), b"x: 1\n").unwrap();
        fs::write(p2.as_std_path(), b"x: 1\n").unwrap();

        let op = ConstOp::new(b"x: 2\n");
        let cli = cli_for(&["--check"]);
        let mut out: Vec<u8> = Vec::new();
        let err = run_per_file(vec![p1.clone(), p2.clone()], &op, &cli, &mut out)
            .expect_err("changes pending should error");
        let pending = err
            .downcast_ref::<CheckPending>()
            .expect("expected CheckPending marker");
        assert_eq!(pending.count, 2);

        // Files on disk untouched.
        assert_eq!(fs::read(p1.as_std_path()).unwrap(), b"x: 1\n");
        assert_eq!(fs::read(p2.as_std_path()).unwrap(), b"x: 1\n");
    }

    #[test]
    fn run_per_file_check_when_idempotent_returns_ok() {
        // `--check` mode: source bytes already match op output → Ok(()).
        let dir = tempdir().unwrap();
        let p1 = Utf8PathBuf::from_path_buf(dir.path().join("a.yaml")).unwrap();
        let p2 = Utf8PathBuf::from_path_buf(dir.path().join("b.yaml")).unwrap();
        fs::write(p1.as_std_path(), b"x: 2\n").unwrap();
        fs::write(p2.as_std_path(), b"x: 2\n").unwrap();

        let op = ConstOp::new(b"x: 2\n");
        let cli = cli_for(&["--check"]);
        let mut out: Vec<u8> = Vec::new();
        run_per_file(vec![p1, p2], &op, &cli, &mut out).expect("idempotent run");
        // No surprises on stdout besides the bulk-mode "0 files" line.
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("0 files would be modified"), "got: {s}");
    }

    #[test]
    fn run_per_file_continue_on_error_returns_partial_failure() {
        // One failing file, two succeeding → returns BulkPartialFailure
        // with count=1 and the summary lists the failed path.
        let dir = tempdir().unwrap();
        let p1 = Utf8PathBuf::from_path_buf(dir.path().join("a.yaml")).unwrap();
        let p2 = Utf8PathBuf::from_path_buf(dir.path().join("b.yaml")).unwrap();
        let p3 = Utf8PathBuf::from_path_buf(dir.path().join("c.yaml")).unwrap();
        fs::write(p1.as_std_path(), b"x: 1\n").unwrap();
        fs::write(p2.as_std_path(), b"x: 1\n").unwrap();
        fs::write(p3.as_std_path(), b"x: 1\n").unwrap();

        let op = FailOnPath {
            bad: p2.clone(),
            bytes: b"x: 2\n".to_vec(),
        };
        let cli = cli_for(&["-i", "--continue-on-error"]);
        let mut out: Vec<u8> = Vec::new();
        let err = run_per_file(
            vec![p1.clone(), p2.clone(), p3.clone()],
            &op,
            &cli,
            &mut out,
        )
        .expect_err("expected partial failure");
        let partial = err
            .downcast_ref::<BulkPartialFailure>()
            .expect("expected BulkPartialFailure marker");
        assert_eq!(partial.failed_count, 1);

        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Modified: 2"));
        assert!(s.contains("Failed: 1"));
        assert!(s.contains(p2.as_str()), "summary should list bad path: {s}");

        // Successful files were written; bad file untouched.
        assert_eq!(fs::read(p1.as_std_path()).unwrap(), b"x: 2\n");
        assert_eq!(fs::read(p2.as_std_path()).unwrap(), b"x: 1\n");
        assert_eq!(fs::read(p3.as_std_path()).unwrap(), b"x: 2\n");
    }

    #[test]
    fn run_per_file_bulk_diff_mode_emits_per_file_markers_only_with_diff() {
        // `--diff` + bulk: each non-empty diff is preceded by `=== <path> ===`.
        let dir = tempdir().unwrap();
        let p1 = Utf8PathBuf::from_path_buf(dir.path().join("a.yaml")).unwrap();
        let p2 = Utf8PathBuf::from_path_buf(dir.path().join("b.yaml")).unwrap();
        fs::write(p1.as_std_path(), b"x: 1\n").unwrap();
        fs::write(p2.as_std_path(), b"x: 1\n").unwrap();

        let op = ConstOp::with_diff(b"x: 2\n", "--- old\n+++ new\n@@\n-x: 1\n+x: 2\n");
        let cli = cli_for(&["--diff"]);
        let mut out: Vec<u8> = Vec::new();
        run_per_file(vec![p1.clone(), p2.clone()], &op, &cli, &mut out).expect("bulk diff run");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains(&format!("=== {p1} ===")),
            "missing marker 1: {s}"
        );
        assert!(
            s.contains(&format!("=== {p2} ===")),
            "missing marker 2: {s}"
        );
        assert!(s.contains("-x: 1"), "missing diff body: {s}");
        // Diff mode in bulk still ends with a summary so users see counts.
        assert!(s.contains("Modified: 2"));
    }
}
