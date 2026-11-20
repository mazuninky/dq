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
//! 1. Resolve the format (extension or `--input-format`) and load the document via the
//!    standard read-side helper. Unlike `set`/`del`, fmt uses the read-only
//!    parsers — there is no textual-edit splice to perform.
//! 2. Render the parsed [`dq_core::Document`] into a `Vec<u8>` via
//!    [`dq_core::Format::write_with_options`], passing the
//!    [`dq_core::WriteOptions`] snapshot built from the global flags by
//!    [`crate::cli::Cli::write_options`].
//!    JSON is handed over as a value tree, so it is re-emitted rather than
//!    copied (see [`FmtFileOp::apply`]).
//! 3. Return [`FileOpResult::Modified`] with the new bytes (and an optional
//!    unified-diff string when `--diff` mode is active and they differ),
//!    even when they equal the file's: stdout mode prints the document
//!    either way, and the driver treats equal bytes as unchanged for `-i`,
//!    `--diff` and `--check`.
//! 4. The bulk driver handles `-i` (atomic write), `--check` (compare-only
//!    gate), and `--diff` (stdout printing) uniformly — fmt itself only
//!    produces a `FileOpResult`.

use std::io::Write;

use camino::Utf8Path;
use dq_core::template_guard::TemplateMode;
use dq_core::{Document, FormatTag, WriteOptions};

use super::io_helpers::load_document_with_path;
use crate::bulk::{self, FileOp, FileOpResult};
use crate::cli::{Cli, FmtArgs};
use crate::error::InvalidInput;

/// Run `dq fmt`.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) for inconsistent write flags
///   (`-i + --diff`, `--backup` without `-i`, `-i + --check`,
///   etc. — see [`Cli::ensure_write_flags_consistent`]).
/// - A usage error (exit 2) for a `-F` naming another format than a file's,
///   or `-i` / `--check` on stdin — see
///   [`super::io_helpers::check_write_targets`].
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

    // fmt re-emits the whole file through the format's writer: neither
    // escape hatch can keep the templates, so both are refused as `fix`
    // refuses them. Without them the template guard runs on every file.
    for (set, flag) in [
        (cli.allow_templates, "--allow-templates"),
        (cli.raw_template_strings, "--raw-template-strings"),
    ] {
        if set {
            return Err(anyhow::Error::new(InvalidInput::new(format!(
                "dq fmt is incompatible with {flag}: re-emitting the file does not preserve template syntax. Use point-edits for templated files."
            ))));
        }
    }

    let op = FmtFileOp {
        input_format,
        opts,
        use_color,
        diff_enabled: cli.diff,
    };

    let files = bulk::expand_glob(&args.file)?;
    super::io_helpers::check_write_targets(cli, "fmt", &files, input_format)?;
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
        let (input_fmt, doc) =
            load_document_with_path(path, self.input_format, TemplateMode::Guard)?;

        // The JSON writer copies a document read from bytes verbatim (the
        // textual-edit contract) unless an option forces a re-emit. fmt is
        // the canonicaliser, so it always hands JSON over as a value tree:
        // two-space indent, one final line break, as every JSON re-emit.
        let value_tree;
        let to_emit = if doc.format() == FormatTag::Json {
            value_tree = match doc.values() {
                Some(values) => Document::multi_value_only(values.to_vec(), FormatTag::Json),
                None => Document::value_only(doc.value().clone(), FormatTag::Json),
            };
            &value_tree
        } else {
            &doc
        };

        // 2. Render through `write_with_options`. The format's default
        //    impl forwards to `write` (M2-baseline preserved) when neither
        //    `sort_keys` nor `indent` is set — overriding impls in
        //    json/jsonl/yaml/toml honour the knobs they understand.
        let mut buf: Vec<u8> = Vec::new();
        input_fmt
            .write_with_options(to_emit, &mut buf, self.opts)
            .map_err(anyhow::Error::new)?;

        // The guard let the file's templates through: the rewrite must
        // write each so it reads back as the same string, and still not
        // change the reading.
        if dq_core::template_guard::detect_templates(doc.original_bytes()).is_some() {
            dq_core::template_guard::check_templates_kept(doc.value(), &[], None, &buf, |b| {
                input_fmt.parse(b)
            })
            .map_err(anyhow::Error::new)?;
        }

        // 3. Always `Modified`, even when the bytes equal the file's: in
        //    stdout mode the output is the command's result and is printed
        //    either way. The bulk driver compares the bytes with the file
        //    for `-i` (no write, no `.bak`), `--diff` and `--check`, and
        //    counts an unchanged file under Skipped.
        // 4. Diff string is only required when `--diff` mode is active;
        //    skip the unified-diff cost otherwise. It compares against the
        //    file's bytes: several readers keep no source bytes in the
        //    document, and a diff against nothing would show every line
        //    as added.
        let diff = if self.diff_enabled {
            let source = if path == "-" {
                doc.original_bytes().to_vec()
            } else {
                super::io_helpers::read_bytes(path)?
            };
            let original_str = String::from_utf8_lossy(&source);
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
    fn fmt_smoke_emits_output_for_yaml_file() {
        // Smoke test: fmt against an already canonical YAML file prints the
        // document to stdout (stdout mode prints whether or not anything
        // changed) and leaves the file alone. Comprehensive coverage lives
        // in the integration tests.
        let tmp = write_yaml("a: 1\nb: 2\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = Cli::try_parse_from(["dq", "fmt", path.as_str()]).unwrap();
        let args = FmtArgs { file: path.clone() };
        let opts = cli.write_options();
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, false, &opts, &mut out).expect("fmt should succeed");
        assert_eq!(String::from_utf8(out).unwrap(), "a: 1\nb: 2\n");
        // File on disk untouched (no `-i`).
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "a: 1\nb: 2\n");
    }
}
