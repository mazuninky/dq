//! `dq fmt FILE` — re-emit through the format's native writer.
//!
//! Layered on [`dq_core::Format::write_with_options`] (the M4 re-emit path
//! that consumes [`dq_core::WriteOptions::sort_keys`] /
//! [`dq_core::WriteOptions::indent`]) and [`crate::bulk::run_per_file`]
//! (glob expansion, `--check`, `--continue-on-error`, `--parallel`,
//! summary).
//!
//! Pipeline (per file, executed inside [`FmtFileOp::apply`]):
//!
//! 1. Resolve the format (extension or `-F`) and load the document via the
//!    standard read-side helper. Unlike `set`/`del`, fmt uses the read-only
//!    parsers — there is no textual-edit splice to perform.
//! 2. Render the parsed [`dq_core::Document`] into a `Vec<u8>` via
//!    [`dq_core::Format::write_with_options`], passing the
//!    [`dq_core::WriteOptions`] snapshot built from the global flags by
//!    [`crate::cli::Cli::write_options`].
//! 3. Compare the rendered bytes against [`dq_core::Document::original_bytes`].
//!    If equal → [`FileOpResult::Unchanged`]. Else →
//!    [`FileOpResult::Modified`] with the new bytes (and an optional
//!    unified-diff string when `--diff` mode is active).
//! 4. The bulk driver handles `-i` (atomic write), `--check` (compare-only
//!    gate), and `--diff` (stdout printing) uniformly — fmt itself only
//!    produces a `FileOpResult`.

use std::io::Write;

use camino::Utf8Path;
use dq_core::WriteOptions;

use super::io_helpers::load_document_with_path;
use crate::bulk::{self, FileOp, FileOpResult};
use crate::cli::{Cli, FmtArgs};

/// Run `dq fmt`.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) for inconsistent write flags
///   (`-i + --diff`, `--backup` without `-i`, `-i + -F`, `-i + --check`,
///   etc. — see [`Cli::ensure_write_flags_consistent`]).
/// - [`dq_core::Error`] family on parse / I/O / write failures.
/// - [`crate::error::CheckPending`] (exit 1) when `--check` finds at least
///   one file that would be modified.
/// - [`crate::error::BulkPartialFailure`] (exit 7) when
///   `--continue-on-error` finishes with one or more failed files.
pub fn run(
    cli: &Cli,
    args: &FmtArgs,
    input_format: Option<&str>,
    use_color: bool,
    opts: &WriteOptions,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_write_flags_consistent()?;

    let op = FmtFileOp {
        input_format,
        opts,
        use_color,
        diff_enabled: cli.diff,
    };

    let files = bulk::expand_glob(&args.file)?;
    bulk::run_per_file(files, &op, cli, out)
}

/// `FileOp` adapter that holds the input-format override + write options
/// by reference so rayon can spread `apply` across worker threads without
/// cloning [`WriteOptions`].
struct FmtFileOp<'a> {
    input_format: Option<&'a str>,
    opts: &'a WriteOptions,
    use_color: bool,
    diff_enabled: bool,
}

impl<'a> FileOp for FmtFileOp<'a> {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult> {
        // 1. Load + parse via the read-side helper. fmt does not need the
        //    write-aware (span-collecting) parsers because we are not
        //    splicing — we re-emit the parsed value tree through the
        //    format's native writer.
        let (input_fmt, doc) = load_document_with_path(path, self.input_format)?;

        // 2. Render through `write_with_options`. The format's default
        //    impl forwards to `write` (M2-baseline preserved) when neither
        //    `sort_keys` nor `indent` is set — overriding impls in
        //    json/jsonl/yaml/toml honour the knobs they understand.
        let mut buf: Vec<u8> = Vec::new();
        input_fmt
            .write_with_options(&doc, &mut buf, self.opts)
            .map_err(anyhow::Error::new)?;

        // 3. Idempotency check: if the rendered bytes equal the source's
        //    on-disk bytes, the file is already canonically formatted and
        //    we report `Unchanged`. The bulk driver translates that into
        //    a `Skipped` count (no per-file output, no `would modify`
        //    line in `--check`).
        let original = doc.original_bytes();
        if buf == original {
            return Ok(FileOpResult::Unchanged);
        }

        // 4. Diff string is only required when `--diff` mode is active;
        //    skip the unified-diff cost otherwise.
        let diff = if self.diff_enabled {
            let original_str = String::from_utf8_lossy(original);
            let modified_str = String::from_utf8_lossy(&buf);
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
            output_bytes: buf,
            diff,
        })
    }
}

#[cfg(test)]
mod tests {
    // `super::*` already brings `std::io::Write` into scope (the handler
    // uses it for the `out: &mut dyn Write` parameter), so the `write_all`
    // call inside `write_yaml` resolves through it automatically.
    use super::*;
    use clap::Parser;
    use tempfile::NamedTempFile;

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    #[test]
    fn fmt_smoke_emits_output_for_yaml_file() {
        // Smoke test: fmt against a YAML file produces non-empty stdout
        // (or, when the file is already canonical, leaves stdout empty
        // because the bulk driver short-circuits on `Unchanged`). The
        // assertion is intentionally lenient — comprehensive coverage is
        // the test-writer agent's job; this only proves the wiring runs
        // end-to-end without panicking.
        let tmp = write_yaml("a: 1\nb: 2\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "fmt", path.as_str()]).unwrap();
        let args = FmtArgs { file: path.clone() };
        let opts = cli.write_options();
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &opts, &mut out).expect("fmt should succeed");
        // Either the file was already canonical (stdout empty, no error)
        // or the writer reformatted it (stdout contains the rendered
        // YAML). Both are acceptable smoke outcomes.
        let s = String::from_utf8(out).expect("stdout must be UTF-8");
        if !s.is_empty() {
            assert!(
                s.contains("a:") && s.contains("b:"),
                "expected YAML keys in re-emitted output, got: {s}",
            );
        }
        // File on disk untouched (no `-i`).
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "a: 1\nb: 2\n");
    }
}
