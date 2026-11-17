//! `dq del FILE POINTER` — remove the value at a JSON Pointer.
//!
//! Layered on [`Document::del_at`] (textual-edit splice) and
//! [`crate::bulk::run_per_file`] (glob expansion, `--check`,
//! `--continue-on-error`, `--parallel`, summary).
//!
//! Mirrors [`super::set::run`] minus the value-resolution step:
//!
//! 1. Validate the write-mode flag combination via
//!    [`Cli::ensure_write_flags_consistent`].
//! 2. Parse the pointer ONCE.
//! 3. Build a [`DelFileOp`] adapter that holds the parsed pointer + CLI
//!    flags by reference; hand it to [`crate::bulk::run_per_file`] which
//!    short-circuits to byte-identical M2 output when the glob matches a
//!    single file.
//!
//! Per-file work executed inside [`DelFileOp::apply`]:
//!
//! 1. Resolve the format (extension or `-F`).
//! 2. Read the original bytes.
//! 3. Apply the same template guard as `set`.
//! 4. Parse to a write-aware [`Document`].
//! 5. Call [`Document::del_at`] to splice the byte buffer in place.
//! 6. Restore template placeholders if the substitution pass ran.
//! 7. Compute a unified-diff string when `--diff` mode is active so the
//!    bulk driver can dispatch it; never write to disk here — `--check`,
//!    `--diff`, and `-i` are all driver concerns.

use std::io::Write;

use camino::Utf8Path;
use dq_core::{Document, Pointer};

use super::io_helpers::{pick_format, read_bytes};
use crate::bulk::{self, FileOp, FileOpResult};
use crate::cli::{Cli, DelArgs};

/// Run `dq del`.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) for inconsistent write flags.
/// - [`dq_core::Error::TemplatedFile`] (exit 3) when the file contains Go
///   template syntax and neither escape-hatch flag is set.
/// - [`dq_core::Error::Path`] (exit 2) when the pointer addresses a missing
///   node or the root (per spec, `del ""` is a `TypeMismatch`).
/// - [`dq_core::Error::WriteIo`] / [`dq_core::Error::WriteUnavailable`]
///   (exit 7) on write failure.
/// - [`crate::error::CheckPending`] (exit 1) when `--check` finds at least
///   one file that would be modified.
/// - [`crate::error::BulkPartialFailure`] (exit 7) when
///   `--continue-on-error` finishes with one or more failed files.
pub fn run(
    cli: &Cli,
    args: &DelArgs,
    input_format: Option<&str>,
    use_color: bool,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_write_flags_consistent()?;

    // M4 §4 / design D5: `--sort-keys` and `--indent` are no-ops on the
    // textual-edit splice path. See `commands::set::run` for the rationale.
    if cli.sort_keys || cli.indent.is_some() {
        tracing::debug!(
            "--sort-keys / --indent are no-ops for textual-edit splice; use `dq fmt` to canonicalize after `del`",
        );
    }

    // Parse the pointer ONCE — every file uses the same pointer.
    let pointer = Pointer::parse(&args.pointer).map_err(anyhow::Error::new)?;

    let op = DelFileOp {
        cli,
        input_format,
        use_color,
        pointer: &pointer,
    };

    let files = bulk::expand_glob(&args.file)?;
    bulk::run_per_file(files, &op, cli, out)
}

/// `FileOp` adapter that holds the parsed pointer + CLI flags by reference
/// so rayon can spread `apply` across worker threads without cloning.
struct DelFileOp<'a> {
    cli: &'a Cli,
    input_format: Option<&'a str>,
    use_color: bool,
    pointer: &'a Pointer,
}

impl<'a> FileOp for DelFileOp<'a> {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
        // Mirror `commands::set::SetFileOp::apply` minus the value: the per-
        // file pipeline is identical except `del_at` replaces `set_at`.
        let format = pick_format(path, self.input_format)?;
        let original_bytes = read_bytes(path)?;

        // Template guard: detect / substitute / pass-through. Same shape as
        // `commands::set` — kept inline rather than factored to keep each
        // handler readable end-to-end.
        let (parse_input, placeholder_map) = if self.cli.raw_template_strings {
            let (substituted, map) =
                dq_core::template_guard::substitute_placeholders(&original_bytes);
            (substituted, Some(map))
        } else if self.cli.allow_templates {
            (original_bytes.clone(), None)
        } else if let Some(marker) = dq_core::template_guard::detect_templates(&original_bytes) {
            return Err(anyhow::Error::new(dq_core::Error::templated_file(marker)));
        } else {
            (original_bytes.clone(), None)
        };

        let mut document = parse_to_document(format, &parse_input, path)?;
        document.del_at(self.pointer).map_err(anyhow::Error::new)?;

        let mut final_bytes = document.original_bytes().to_vec();
        if let Some(map) = &placeholder_map {
            final_bytes = dq_core::template_guard::restore_placeholders(&final_bytes, map);
        }

        // The bulk driver handles `-i` and `--check` uniformly — we only
        // need to compute the diff string when `--diff` mode is active so
        // the driver can dispatch it to stdout.
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

/// Parse `parse_input` as `format` into a write-aware [`Document`].
///
/// Same shape as [`super::set::parse_to_document`]; kept duplicated so the
/// two handlers can evolve their parse strategies independently in M3
/// (e.g. when `del` gains a `--format-fallback` flag).
fn parse_to_document(
    format: &'static dyn dq_core::Format,
    parse_input: &[u8],
    file: &Utf8Path,
) -> anyhow::Result<Document> {
    let path_label = camino::Utf8PathBuf::from(file);
    let doc_result: dq_core::Result<Document> = match format.name() {
        "yaml" => dq_core::parse_yaml_with_spans(parse_input),
        "json" => dq_core::parse_json_with_spans(parse_input),
        _ => format.parse(parse_input),
    };
    doc_result
        .map_err(|mut e| {
            if let dq_core::Error::Parse { ref mut file, .. } = e
                && file.is_none()
            {
                *file = Some(path_label.clone());
            }
            e
        })
        .map_err(anyhow::Error::new)
}

#[cfg(test)]
mod tests {
    // `super::*` already brings `std::io::Write` into scope (the handler
    // uses it for the `out: &mut dyn Write` parameter), so the `write_all`
    // call inside `write_yaml` resolves through it automatically.
    use super::*;
    use clap::Parser;
    use tempfile::NamedTempFile;

    // Returns a `TempPath` (not a `NamedTempFile`) so the underlying `File`
    // handle is released after writing. Required for Windows: production
    // atomic-write uses `MoveFileEx` which fails with `Access is denied` if
    // the target is still held open elsewhere in the same process.
    fn write_yaml(content: &str) -> tempfile::TempPath {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.into_temp_path()
    }

    #[test]
    fn del_removes_leaf_in_place() {
        let tmp = write_yaml("a: 1\nb: 2\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "-i", "del", path.as_str(), "/a"]).unwrap();
        let args = DelArgs {
            file: path.clone(),
            pointer: "/a".to_owned(),
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("del should succeed");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "b: 2\n");
    }

    #[test]
    fn del_missing_pointer_returns_path_error() {
        // Per spec: missing pointer is NOT silent; del must surface a
        // structured `Path` error so `dq del` can be relied upon for "this
        // key existed and is now gone" semantics.
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "del", path.as_str(), "/b"]).unwrap();
        let args = DelArgs {
            file: path,
            pointer: "/b".to_owned(),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "path");
    }

    #[test]
    fn del_root_returns_type_mismatch() {
        // Empty pointer == document root. Deleting the root would empty the
        // file; `del_at` rejects this with a `TypeMismatch` so users get a
        // clear "you cannot delete the document" error instead of a silent
        // empty file.
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "del", path.as_str(), ""]).unwrap();
        let args = DelArgs {
            file: path,
            pointer: String::new(),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, false, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "path");
        // The Path error's kind is TypeMismatch with `found = "root"` —
        // pin it via the Display message, which `dq-core` already exposes.
        let dq_core::Error::Path { kind, .. } = domain else {
            panic!("expected Path error variant, got: {domain:?}");
        };
        match kind {
            dq_core::PathErrorKind::TypeMismatch { found, .. } => {
                assert_eq!(*found, "root", "expected root rejection");
            }
            other => panic!("expected TypeMismatch, got: {other:?}"),
        }
    }

    #[test]
    fn del_diff_shows_removal() {
        let tmp = write_yaml("a: 1\nb: 2\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "--diff", "del", path.as_str(), "/a"]).unwrap();
        let args = DelArgs {
            file: path,
            pointer: "/a".to_owned(),
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("del should succeed");
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("-a: 1"),
            "expected removal line `-a: 1` in diff, got:\n{s}",
        );
    }

    #[test]
    fn del_writes_to_stdout_by_default() {
        // Default mode (no `-i`, no `--diff`) writes the modified document
        // to stdout and leaves the file on disk untouched. CRITICAL:
        // single-file mode must not emit a `=== <path> ===` marker.
        let tmp = write_yaml("a: 1\nb: 2\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "del", path.as_str(), "/a"]).unwrap();
        let args = DelArgs {
            file: path.clone(),
            pointer: "/a".to_owned(),
        };
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &mut out).expect("del should succeed");
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "b: 2\n");
        // File on disk is unchanged.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "a: 1\nb: 2\n");
    }
}
