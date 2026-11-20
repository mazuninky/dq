//! CLI-internal error helpers.
//!
//! - [`SilentError`] is the marker `exists` returns when the pointer does not
//!   address a node — `main.rs` recognises it and *suppresses* the stderr
//!   line so the command remains shell-pipeline-friendly (`dq exists … && echo ok`).
//! - [`ValidateFail`] is the marker `validate` wraps parse errors in so they
//!   map to exit 4 (`VALIDATE_FAIL`) rather than the default exit 3
//!   (`PARSE_ERROR`). Without it, `dq validate` would share `PARSE_ERROR`
//!   with every other command's parse-time failure, defeating the spec's
//!   distinct exit code.
//! - [`InvalidInput`] is the marker for caller-side input errors — bad CLI
//!   flag combinations, missing format hints for stdin, etc. — that should map
//!   to exit 6 (`INVALID_INPUT`) instead of the default exit 1 (`GENERIC`).

use std::fmt;

/// "Fail with exit code 1, but write nothing to stderr."
///
/// `main.rs` checks for this via `downcast_ref::<SilentError>()` before
/// rendering the standard `{:?}` chain.
#[derive(Debug, Clone, Copy)]
pub struct SilentError;

impl fmt::Display for SilentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Even though we suppress stderr, the message must exist so anyhow's
        // chain machinery doesn't crash if some caller does dump it.
        f.write_str("(silent)")
    }
}

impl std::error::Error for SilentError {}

/// "Validate failed — map to exit 4 instead of 3."
///
/// Wraps the underlying `dq_core::Error::Parse` so the error chain still
/// carries the diagnostic, while letting the exit-code mapper differentiate
/// validate-time failures from generic parse-time failures.
#[derive(Debug)]
pub struct ValidateFail {
    /// The underlying parse error, kept so callers / renderers can still
    /// reach into its structured fields.
    pub source: dq_core::Error,
}

impl fmt::Display for ValidateFail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for ValidateFail {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// "Caller-side input error — map to exit 6 instead of the catch-all exit 1."
///
/// Used for cases like `--in-place` rejection on a read command, where the user can
/// correct the mistake by passing different args.
/// `exit_code_for_error` recognises this marker via `downcast_ref` before the
/// generic fallback.
#[derive(Debug)]
pub struct InvalidInput(String);

impl InvalidInput {
    /// Wrap a human-readable message in an `InvalidInput`.
    #[must_use]
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl fmt::Display for InvalidInput {
    /// The message on one line ([`dq_core::one_line`]): it quotes paths,
    /// globs and names as the user gave them, and has no line break of its
    /// own.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&dq_core::one_line(&self.0))
    }
}

impl std::error::Error for InvalidInput {}

/// Marker error returned by the bulk driver when `--check` mode finds at
/// least one file that would be modified. Maps to exit code 1 (`GENERIC`)
/// via [`crate::exit_code::exit_code_for_error`].
#[derive(Debug, thiserror::Error)]
#[error("{count} file(s) would be modified")]
pub struct CheckPending {
    /// Number of files in the bulk run whose prospective output differed
    /// from their on-disk bytes.
    pub count: usize,
}

/// Marker error returned by the bulk driver when `--continue-on-error`
/// completes with one or more failed files. Maps to exit code 7
/// (`WRITE_FAILED`). Per-file errors have already been printed to the
/// caller-supplied writer by the driver; this marker only carries the
/// aggregate count for the shell to branch on.
#[derive(Debug, thiserror::Error)]
#[error("{failed_count} file(s) failed in bulk run")]
pub struct BulkPartialFailure {
    /// Number of files that returned an error during the bulk run.
    pub failed_count: usize,
}

/// Marker error returned by the lint engine when at least one diagnostic
/// has `Severity::Error`. Maps to exit code 4 (`VALIDATE_FAIL`) — the same
/// "document failed a quality gate" family as `validate`.
///
/// The diagnostics themselves are already rendered through the configured
/// reporter before this marker bubbles up; the marker only carries the
/// aggregate count for the shell to branch on. The lint exit-code mapping
/// is documented in the add-exec-engine section of `docs/decisions.md`
/// and in `docs/reference/cli.md`.
#[derive(Debug, thiserror::Error)]
#[error("lint reported {count} error-severity diagnostic(s)")]
pub struct LintFail {
    /// Number of error-severity diagnostics emitted during the run.
    pub count: usize,
}

/// Marker error returned by the lint engine when at least one diagnostic
/// has `Severity::Warn` AND `--strict` is active. Maps to exit code 1
/// (`GENERIC`).
///
/// Without `--strict`, warn-severity diagnostics never drive a non-zero
/// exit code. The diagnostics are already rendered before this marker
/// bubbles up; the marker only carries the aggregate count for shells to
/// branch on. Distinct from [`LintFail`] so a `--strict`-only failure
/// doesn't masquerade as a hard "error" failure (exit 4).
#[derive(Debug, thiserror::Error)]
#[error("lint reported {count} warning(s) under --strict")]
pub struct LintWarnStrict {
    /// Number of warn-severity diagnostics emitted during the run.
    pub count: usize,
}

/// `--doc N` named a document the file does not have. Maps to exit code 2
/// (`NOT_FOUND`) — the same family as a JSON Pointer that addresses no
/// node: the file is fine, the address is not in it.
#[derive(Debug, thiserror::Error)]
#[error("--doc {index} is out of range: {} holds {count} document(s){}", dq_core::one_line(.file), index_hint(*.count))]
pub struct DocOutOfRange {
    /// The requested index.
    pub index: usize,
    /// How many documents the file holds (1 for a file that is not a
    /// multi-document stream).
    pub count: usize,
    /// The file, as given on the command line (`-` for stdin).
    pub file: String,
}

/// `--doc N` over several files, none of which holds document N. Maps to
/// exit code 2 (`NOT_FOUND`), as [`DocOutOfRange`] does for one file: a
/// run where every file would be skipped did nothing, so it is the same
/// error, not a success.
#[derive(Debug, thiserror::Error)]
#[error("--doc {index} is out of range: none of the {files} files holds document {index}")]
pub struct DocOutOfRangeEverywhere {
    /// The requested index.
    pub index: usize,
    /// How many files the run covered.
    pub files: usize,
}

/// The pointer addresses a node of a type the command cannot read: `keys`
/// or `values` on a scalar, `len` on a number, a bool or null. Maps to exit
/// code 1 (`GENERIC`): the node is there (not exit 2), it is the wrong
/// kind of node for the question.
#[derive(Debug, thiserror::Error)]
#[error("cannot {action} {}: it is {found}, not {expected}", node(pointer))]
pub struct WrongType {
    /// What the command does, as a verb phrase: `"list the keys of"`.
    pub action: &'static str,
    /// The canonical pointer of the node (`""` for the document root).
    pub pointer: String,
    /// The node's type, as `dq type` names it.
    pub found: &'static str,
    /// The types the command reads: `"an object or an array"`.
    pub expected: &'static str,
}

/// `'/a/b'`, or `the document root` for the empty pointer.
fn node(pointer: &str) -> String {
    if pointer.is_empty() {
        "the document root".to_owned()
    } else {
        format!("'{}'", dq_core::one_line(pointer))
    }
}

/// `" (0..=N)"` — the valid `--doc` indices for `count` documents.
fn index_hint(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => " (--doc 0)".to_owned(),
        n => format!(" (--doc 0 to {})", n - 1),
    }
}
