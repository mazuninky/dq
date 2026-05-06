//! `dq fix FILE...` — apply every applicable rule's `fix.jq` to the
//! given files.
//!
//! Pipeline mirrors [`crate::commands::lint_core::run_with_rulesets`] up
//! to the evaluator-build step, then forks: instead of collecting
//! diagnostics through a [`crate::output::Reporter`], it builds a
//! [`dq_exec::Fixer`] and dispatches per-file work through
//! [`crate::bulk::run_per_file`].
//!
//! ## Comment preservation
//!
//! Phase 4 of `add-ir-foundation` introduced two paths:
//!
//! - **`fix.ops` (preferred)**: rules emit a JSON Patch array
//!   (`add`/`replace`/`remove`); the patch is applied via
//!   `EditScript::apply` against the parsed [`Document`], which routes
//!   through the per-format `ScalarRenderer` to splice individual byte
//!   ranges. Comments and surrounding whitespace are preserved
//!   byte-for-byte. The handler then writes `doc.original_bytes()` to
//!   disk directly.
//! - **`fix.jq` (legacy, deprecated)**: whole-document jq transform
//!   replaces the document tree, dropping `original_bytes` and the
//!   span map. The handler re-emits via `Format::write_with_options`,
//!   same comment-loss trade-off as `dq set --jq`.
//!
//! `FixOutcome::legacy_jq_applied` distinguishes the two paths: when
//! `false`, `original_bytes` is canonical and goes to disk verbatim;
//! when `true`, the document is re-emitted to recover a renderable
//! byte buffer.
//!
//! ## Write-mode flags
//!
//! - `-i` → atomic in-place write (via [`dq_core::atomic_write::write`]).
//! - `--diff` → unified diff to stdout, no write.
//! - `--check` → exit 1 if any file would be modified, no write. M10
//!   reuses [`crate::error::CheckPending`] (already mapped to exit 1)
//!   rather than introducing a new marker.
//! - `--continue-on-error`, `--parallel`, `--backup` work the same as
//!   for `dq set` / `dq del` / `dq fmt` because the bulk driver is the
//!   same code path.
//!
//! ## Template guard
//!
//! Templated files (Helm / Argo / GitHub Actions) cannot round-trip
//! through the re-emit path. `--allow-templates` and
//! `--raw-template-strings` are explicitly rejected with `InvalidInput`
//! (exit 6), mirroring the `dq set --jq` rejection.

use std::io::Write;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use dq_exec::{Evaluator, FixOutcome, Fixer, LoaderArgs, RuleLoader};
use indexmap::IndexSet;

use crate::bulk::{self, FileOp, FileOpResult};
use crate::cli::{Cli, FixArgs};
use crate::commands::io_helpers::{load_document_for_lint, pick_format, read_bytes};
use crate::commands::lint_core::expand_lint_inputs;
use crate::error::InvalidInput;

/// Run the `dq fix` command.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) for inconsistent write flags, template-
///   guard incompatibilities, unknown `--rules` entries, zero-match
///   globs, missing format hints, etc.
/// - [`crate::error::CheckPending`] (exit 1) when `--check` finds at
///   least one fixable file.
/// - [`crate::error::BulkPartialFailure`] (exit 7) when
///   `--continue-on-error` finishes with one or more failed files.
/// - [`dq_core::Error::*`] for the usual file-load / write failures.
/// - [`dq_exec::ExecError::FixApply`] (exit 3) for `fix.jq` runtime or
///   wrong-arity errors.
pub fn run(
    cli: &Cli,
    args: &FixArgs,
    input_format: Option<&str>,
    use_color: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_write_flags_consistent()?;

    // The re-emit path can't preserve template placeholder positions;
    // mirror `dq set --jq`'s rejection.
    if cli.allow_templates {
        return Err(anyhow::Error::new(InvalidInput::new(
            "dq fix is incompatible with --allow-templates: the re-emit path does not preserve template placeholders. Use point-edits for templated files.",
        )));
    }
    if cli.raw_template_strings {
        return Err(anyhow::Error::new(InvalidInput::new(
            "dq fix is incompatible with --raw-template-strings: the re-emit path does not restore template placeholders. Use point-edits for templated files.",
        )));
    }

    tracing::debug!(
        "dq fix routes through Format::write_with_options; comments will be lost in re-emit"
    );

    let expanded = expand_lint_inputs(&args.files)?;
    if expanded.is_empty() {
        // No files to process — nothing to do.
        return Ok(());
    }

    // Detect formats up front so the loader can pick which `@std/*`
    // namespaces to auto-bind. Same approach as `dq lint`.
    let mut discovered_formats: IndexSet<String> = IndexSet::new();
    for file in &expanded {
        let fmt = pick_format(file, input_format)?;
        discovered_formats.insert(fmt.name().to_owned());
    }

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| Utf8PathBuf::from("."));

    let loader_args = LoaderArgs {
        rules: args.rules.clone(),
        cwd,
        discovered_formats,
    };
    let rulesets = RuleLoader::resolve(&loader_args).map_err(anyhow::Error::new)?;
    let evaluator = Evaluator::new(rulesets).map_err(anyhow::Error::new)?;
    let fixer = Arc::new(Fixer::new(&evaluator));

    let op = FixFileOp {
        cli,
        input_format,
        use_color,
        fixer,
    };

    bulk::run_per_file(expanded, &op, cli, out)
}

/// Per-file `FileOp` adapter for `dq fix`. Holds an [`Arc<Fixer>`] so
/// rayon workers share the compiled rule engines without cloning every
/// per-rule `JqEngine`.
struct FixFileOp<'a> {
    cli: &'a Cli,
    input_format: Option<&'a str>,
    use_color: bool,
    fixer: Arc<Fixer>,
}

impl<'a> FileOp for FixFileOp<'a> {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
        // Read the bytes once for the diff renderer below; route the
        // parse through the lint helper so YAML / JSON pick up the
        // span-aware parsers (otherwise `EditScript::apply` would fail
        // with `WriteUnavailable` against a value-only `Document`).
        // The helper re-reads the file internally — accept the 2× I/O
        // for the simpler call site.
        let original_bytes = read_bytes(path)?;
        let (format, mut document) = load_document_for_lint(path, self.input_format)?;

        let outcome = self
            .fixer
            .apply(path, &mut document, format.name())
            .map_err(anyhow::Error::new)?;

        // Audit log — visible at `-v` (info) verbosity. The check path
        // also benefits from this so users see what would change.
        log_outcome(path, &outcome);

        if !outcome.fixed {
            return Ok(FileOpResult::Unchanged);
        }

        // Phase 4: choose the output path based on which engine ran.
        //
        // - Only `fix.ops` ran → `document.original_bytes()` is the
        //   canonical post-fix byte buffer; comments are preserved.
        // - Any `fix.jq` ran → the document was replaced with a
        //   value-only one (no spans, no `original_bytes`); re-emit
        //   through the format writer, same trade-off as
        //   `dq set --jq`.
        let final_bytes = if outcome.legacy_jq_applied {
            let mut buf = Vec::new();
            format
                .write_with_options(&document, &mut buf, &self.cli.write_options())
                .map_err(anyhow::Error::new)?;
            buf
        } else {
            document.original_bytes().to_vec()
        };

        let diff = if self.cli.diff {
            let original_str = String::from_utf8_lossy(&original_bytes);
            let modified_str = String::from_utf8_lossy(&final_bytes);
            Some(crate::diff::render_unified(
                &original_str,
                &modified_str,
                path.as_str(),
                self.use_color,
            ))
        } else {
            None
        };

        Ok(FileOpResult::Modified {
            output_bytes: final_bytes,
            diff,
        })
    }
}

/// Audit log entry — info level so `-v` users see which fixes ran.
fn log_outcome(path: &Utf8Path, outcome: &FixOutcome) {
    tracing::info!(
        path = %path,
        applied = ?outcome.applied_rules,
        skipped_non_idempotent = ?outcome.skipped_non_idempotent,
        "dq fix",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CheckPending;
    use clap::Parser;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Returns a `TempPath` (not a `NamedTempFile`) so the underlying `File`
    // handle is released after writing. Required for Windows: production
    // atomic-write uses `MoveFileEx` which fails with `Access is denied` if
    // the target is still held open elsewhere in the same process.
    fn write_yaml(content: &str) -> tempfile::TempPath {
        let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
        tmp.write_all(content.as_bytes()).expect("write");
        tmp.into_temp_path()
    }

    /// Write an inline rule YAML to disk so we can pass it via
    /// `--rules <path>`. Returns the tempfile (kept alive by the
    /// caller) and its UTF-8 path.
    fn write_rule(yaml: &str) -> (NamedTempFile, Utf8PathBuf) {
        let mut rule_tmp = NamedTempFile::with_suffix(".yml").expect("rule tmp");
        rule_tmp.write_all(yaml.as_bytes()).expect("write");
        let path = Utf8PathBuf::from_path_buf(rule_tmp.path().to_path_buf()).expect("UTF-8 path");
        (rule_tmp, path)
    }

    /// A canonical "always-fires + idempotent fix" rule used by several
    /// happy-path tests below. The fix sets `.fixed = true`; the check
    /// only fires when `.fixed` is not already true so the second
    /// invocation is a no-op (proves idempotency).
    const SET_FIXED_RULE: &str = r#"
id: test.set-fixed
description: ensure .fixed is true
severity: warn
match:
  format: yaml
check:
  jq: 'select(.fixed != true) | .'
  message: 'not fixed'
fix:
  jq: '.fixed = true'
"#;

    #[test]
    fn fix_with_inline_rule_writes_diff_to_stdout() {
        let (_rule_tmp, rule_path) = write_rule(SET_FIXED_RULE);
        let doc_tmp = write_yaml("name: x\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from([
            "dq",
            "--diff",
            "fix",
            "--rules",
            rule_path.as_str(),
            path.as_str(),
        ])
        .expect("clap parse");
        let args = FixArgs {
            files: vec![path.clone()],
            rules: vec![rule_path.to_string()],
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("--diff fix should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("+fixed: true") || s.contains("+ fixed: true") || s.contains("fixed: true"),
            "expected fix in diff output, got:\n{s}",
        );
        // File on disk untouched in --diff mode.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "name: x\n");
    }

    #[test]
    fn fix_with_inline_rule_returns_check_pending_under_check_flag() {
        let (_rule_tmp, rule_path) = write_rule(SET_FIXED_RULE);
        let doc_tmp = write_yaml("name: x\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from([
            "dq",
            "--check",
            "fix",
            "--rules",
            rule_path.as_str(),
            path.as_str(),
        ])
        .expect("clap parse");
        let args = FixArgs {
            files: vec![path.clone()],
            rules: vec![rule_path.to_string()],
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("--check must error when fixes are pending");
        let pending = err
            .downcast_ref::<CheckPending>()
            .expect("expected CheckPending marker so exit code is 1");
        assert_eq!(pending.count, 1);
        // File untouched.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "name: x\n");
    }

    #[test]
    fn fix_writes_in_place_with_i_flag() {
        let (_rule_tmp, rule_path) = write_rule(SET_FIXED_RULE);
        let doc_tmp = write_yaml("name: x\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from([
            "dq",
            "-i",
            "fix",
            "--rules",
            rule_path.as_str(),
            path.as_str(),
        ])
        .expect("clap parse");
        let args = FixArgs {
            files: vec![path.clone()],
            rules: vec![rule_path.to_string()],
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("-i fix should succeed");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("fixed: true"),
            "expected .fixed = true on disk, got:\n{on_disk}",
        );
        assert!(
            on_disk.contains("name: x"),
            "expected name preserved, got:\n{on_disk}",
        );
    }

    #[test]
    fn fix_rejects_allow_templates() {
        // `dq fix --allow-templates` cannot round-trip — reject up
        // front with InvalidInput (exit 6). Mirrors the `dq set --jq`
        // rejection in `set.rs`.
        let (_rule_tmp, rule_path) = write_rule(SET_FIXED_RULE);
        let doc_tmp = write_yaml("name: x\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from([
            "dq",
            "--allow-templates",
            "-i",
            "fix",
            "--rules",
            rule_path.as_str(),
            path.as_str(),
        ])
        .expect("clap parse");
        let args = FixArgs {
            files: vec![path],
            rules: vec![rule_path.to_string()],
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("--allow-templates must be rejected");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "rejection must carry InvalidInput, got: {err:?}",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("--allow-templates"),
            "error must name --allow-templates, got: {msg}",
        );
    }

    #[test]
    fn fix_rejects_raw_template_strings() {
        let (_rule_tmp, rule_path) = write_rule(SET_FIXED_RULE);
        let doc_tmp = write_yaml("name: x\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from([
            "dq",
            "--raw-template-strings",
            "-i",
            "fix",
            "--rules",
            rule_path.as_str(),
            path.as_str(),
        ])
        .expect("clap parse");
        let args = FixArgs {
            files: vec![path],
            rules: vec![rule_path.to_string()],
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out)
            .expect_err("--raw-template-strings must be rejected");
        assert!(err.downcast_ref::<InvalidInput>().is_some());
        assert!(err.to_string().contains("--raw-template-strings"));
    }

    #[test]
    fn fix_with_no_matching_rules_returns_unchanged() {
        // Inline rule is report-only (no `fix:`). The handler must
        // succeed with no writes and leave the file alone.
        let report_only = r#"
id: test.report-only
description: no fix
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: 'fires'
"#;
        let (_rule_tmp, rule_path) = write_rule(report_only);
        let doc_tmp = write_yaml("name: x\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from([
            "dq",
            "-i",
            "fix",
            "--rules",
            rule_path.as_str(),
            path.as_str(),
        ])
        .expect("clap parse");
        let args = FixArgs {
            files: vec![path.clone()],
            rules: vec![rule_path.to_string()],
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("no-fix run should succeed");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "name: x\n");
    }

    #[test]
    fn fix_default_stdout_path_emits_full_doc() {
        // Without -i / --diff / --check, the bulk driver writes the
        // resulting bytes to stdout (single-file path).
        let (_rule_tmp, rule_path) = write_rule(SET_FIXED_RULE);
        let doc_tmp = write_yaml("name: x\n");
        let path = Utf8PathBuf::from_path_buf(doc_tmp.to_path_buf()).expect("UTF-8 path");

        let cli = Cli::try_parse_from(["dq", "fix", "--rules", rule_path.as_str(), path.as_str()])
            .expect("clap parse");
        let args = FixArgs {
            files: vec![path.clone()],
            rules: vec![rule_path.to_string()],
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("fix should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("fixed: true"),
            "expected fix in output, got:\n{s}"
        );
        // File on disk untouched.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "name: x\n");
    }
}
