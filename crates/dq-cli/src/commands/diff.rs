//! `dq diff A B` — emit an RFC 6902 JSON Patch transforming A into B.
//!
//! With `--unified`, emit a textual unified diff over the rendered
//! representations of A and B instead. By default, the diff is emitted as a
//! `Vec<PatchOp>` rendered through the active reporter (`-F json` is the
//! recommended representation per spec, but YAML/TOML/Toon/JSONL also work
//! by routing the resulting `Vec<PatchOp>` through the standard
//! [`Reporter`] pipeline).
//!
//! ## Read-only contract
//!
//! Unlike `set`/`del`/`patch`/`merge`, `diff` does not write anything to the
//! filesystem. It calls [`crate::cli::Cli::ensure_no_write_flags`] on its
//! first line so the global write-mode flags (`-i`, `--diff` (write-mode),
//! `--backup`, `--check`, `--continue-on-error`, `--parallel`) are rejected
//! with `INVALID_INPUT` before any I/O happens.
//!
//! ## Naming-conflict note
//!
//! `Command::Diff` (this subcommand) and `--diff` (the global flag used by
//! write subcommands) share the word "diff". Clap disambiguates by position
//! — `dq diff A B` parses as the subcommand, while `dq set f.yaml /x 1
//! --diff` parses the same word as the flag. The
//! [`tests::diff_subcommand_parses_distinctly_from_diff_flag`] unit test
//! pins that contract.
//!
//! ## Console output
//!
//! When `cli.format == OutputFormat::Console`, the handler routes the patch
//! through the JSON-pretty representation rather than the
//! [`crate::output::ConsoleReporter`]. The console reporter renders an array
//! of objects as one object-per-line with no structure markers, which is
//! unhelpful for an RFC 6902 patch. The spec calls JSON Patch the default
//! representation, so falling through to JSON-pretty is the closer match for
//! the user's intent.

use std::io::Write;

use crate::cli::{Cli, DiffArgs};
use crate::commands::io_helpers::load_document_with_path;
use crate::output::{OutputFormat, Reporter};

/// Run the `diff` command.
///
/// `input_format` is the optional input parser override (used in tests to
/// force a parser for both files). The dispatcher in [`crate::dispatch`]
/// always passes `None` — `-F` for `diff` controls the OUTPUT renderer, not
/// the input parser, mirroring `convert`'s behaviour.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag is set —
///   `diff` is a read-only subcommand.
/// - `dq_core::Error::Io` / `Parse` / `UnsupportedFormat` for I/O, parsing,
///   and unknown-format failures on either input file.
pub fn run(
    cli: &Cli,
    args: &DiffArgs,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;

    let (_fmt_a, doc_a) = load_document_with_path(&args.a, input_format)?;
    let (_fmt_b, doc_b) = load_document_with_path(&args.b, input_format)?;

    if args.unified {
        // Render both documents as pretty JSON (stable key order via
        // `preserve_order`) and unified-diff the textual forms. JSON is the
        // lowest-common-denominator stable representation that both YAML and
        // TOML inputs can be normalised through.
        let a_json = serde_json::to_string_pretty(&doc_a.value().to_serde_json())?;
        let b_json = serde_json::to_string_pretty(&doc_b.value().to_serde_json())?;
        let label = format!("{} -> {}", args.a, args.b);
        let unified = crate::diff::render_unified(&a_json, &b_json, &label, !cli.no_color);
        out.write_all(unified.as_bytes())?;
        return Ok(());
    }

    // Compute the structural diff. `dq_core::diff` returns an empty `Vec`
    // when `a == b` structurally; the empty array (`[]\n`) is the spec's
    // canonical "no difference" output.
    let ops = dq_core::diff(doc_a.value(), doc_b.value());
    // `PatchOp`'s `Serialize` impl renders to the RFC 6902 wire shape; convert
    // to a `serde_json::Value` so the standard reporter pipeline can format
    // it (JSON / YAML / TOML / TOON / JSONL all work this way).
    let json = serde_json::to_value(&ops)?;

    // Console output for `Vec<PatchOp>` would render as one cryptic line per
    // op (no structure markers). The spec calls JSON Patch the default
    // representation, so for the default `Console` format we fall through to
    // JSON-pretty — closer to the user's intent than the "console reporter"
    // shape. Other formats route through the supplied reporter unchanged.
    if cli.format == OutputFormat::Console {
        serde_json::to_writer_pretty(&mut *out, &json)?;
        out.write_all(b"\n")?;
        return Ok(());
    }
    reporter.report(&json, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use crate::output::JsonReporter;
    use camino::Utf8PathBuf;
    use clap::Parser;
    use tempfile::NamedTempFile;

    /// Write `content` to a tempfile with the given extension and return the
    /// handle (kept alive by the caller — `NamedTempFile` deletes on drop).
    fn write_tmp(content: &str, suffix: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(suffix).expect("tempfile");
        tmp.write_all(content.as_bytes()).expect("write tempfile");
        tmp
    }

    /// Build a `Cli` for `dq diff A B` plus optional extra args (e.g. `-F json`,
    /// `--unified`).
    fn cli_for(a: &Utf8PathBuf, b: &Utf8PathBuf, extra: &[&str]) -> Cli {
        let mut argv: Vec<&str> = vec!["dq"];
        argv.extend_from_slice(extra);
        argv.push("diff");
        let a_str = a.as_str();
        let b_str = b.as_str();
        argv.push(a_str);
        argv.push(b_str);
        Cli::try_parse_from(argv).expect("clap parse")
    }

    fn utf8_path(tmp: &NamedTempFile) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf-8 path")
    }

    #[test]
    fn diff_subcommand_parses_distinctly_from_diff_flag() {
        // `dq diff a.yaml b.yaml` is the diff subcommand.
        let cli = Cli::try_parse_from(["dq", "diff", "a.yaml", "b.yaml"]).unwrap();
        assert!(matches!(cli.command, Command::Diff(_)));
        assert!(
            !cli.diff,
            "the global --diff write-flag must NOT be set when invoking the subcommand",
        );

        // `dq set f.yaml /x 1 --diff` is set + write-mode --diff flag.
        let cli = Cli::try_parse_from(["dq", "set", "f.yaml", "/x", "1", "--diff"]).unwrap();
        assert!(
            cli.diff,
            "the global --diff flag must be set on `dq set ... --diff`"
        );
        assert!(matches!(cli.command, Command::Set(_)));
    }

    #[test]
    fn diff_equal_files_emits_empty_array() {
        // Spec scenario "Equal documents produce empty output": `dq diff a a -F json`
        // → stdout is `[]`.
        let content = "server:\n  port: 8080\n";
        let a = write_tmp(content, ".yaml");
        let b = write_tmp(content, ".yaml");
        let a_path = utf8_path(&a);
        let b_path = utf8_path(&b);
        let cli = cli_for(&a_path, &b_path, &["-F", "json"]);
        let args = DiffArgs {
            a: a_path,
            b: b_path,
            unified: false,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).expect("equal files diff");
        let stdout = String::from_utf8(out).expect("utf-8 stdout");
        // JsonReporter emits pretty-printed JSON with a trailing newline; the
        // empty array renders as `[]\n`.
        assert_eq!(
            stdout.trim(),
            "[]",
            "equal files must yield empty array, got: {stdout:?}",
        );
    }

    #[test]
    fn diff_single_replace_emits_one_op() {
        // Spec scenario "Default diff emits JSON Patch ops": single field change
        // produces exactly one `replace` op.
        let a = write_tmp("{\"x\": 1}\n", ".json");
        let b = write_tmp("{\"x\": 2}\n", ".json");
        let a_path = utf8_path(&a);
        let b_path = utf8_path(&b);
        let cli = cli_for(&a_path, &b_path, &["-F", "json"]);
        let args = DiffArgs {
            a: a_path,
            b: b_path,
            unified: false,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).expect("single replace diff");
        let stdout = String::from_utf8(out).expect("utf-8 stdout");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("output must be JSON array");
        let arr = parsed.as_array().expect("expected JSON array");
        assert_eq!(arr.len(), 1, "expected one op, got: {stdout}");
        assert_eq!(arr[0]["op"], "replace");
        assert_eq!(arr[0]["path"], "/x");
        assert_eq!(arr[0]["value"], 2);
    }

    #[test]
    fn diff_type_change_at_root_emits_one_root_replace() {
        // Type mismatch at root: emit a single `replace` at the empty pointer
        // (`""` per RFC 6902). The minimality rule forbids emitting child ops
        // on top of a root replace — pinned at the dq-core layer; this test
        // only checks the CLI surface.
        let a = write_tmp("{\"a\": 1}\n", ".json");
        let b = write_tmp("[1, 2]\n", ".json");
        let a_path = utf8_path(&a);
        let b_path = utf8_path(&b);
        let cli = cli_for(&a_path, &b_path, &["-F", "json"]);
        let args = DiffArgs {
            a: a_path,
            b: b_path,
            unified: false,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).expect("type-change diff");
        let stdout = String::from_utf8(out).expect("utf-8 stdout");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("output must be JSON array");
        let arr = parsed.as_array().expect("expected JSON array");
        assert_eq!(
            arr.len(),
            1,
            "type change must yield exactly one op, got: {stdout}"
        );
        assert_eq!(arr[0]["op"], "replace");
        // RFC 6902: empty pointer renders as `""`.
        assert_eq!(arr[0]["path"], "");
    }

    #[test]
    fn diff_cross_format_yaml_vs_json_yields_empty_for_equal_content() {
        // YAML and JSON files with semantically equal content must diff to
        // an empty patch — the structural diff operates on the parsed
        // `Value`, not the textual form, so format does not matter.
        let a = write_tmp("a: 1\n", ".yaml");
        let b = write_tmp("{\"a\":1}\n", ".json");
        let a_path = utf8_path(&a);
        let b_path = utf8_path(&b);
        let cli = cli_for(&a_path, &b_path, &["-F", "json"]);
        let args = DiffArgs {
            a: a_path,
            b: b_path,
            unified: false,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).expect("cross-format diff");
        let stdout = String::from_utf8(out).expect("utf-8 stdout");
        assert_eq!(
            stdout.trim(),
            "[]",
            "semantically equal cross-format docs must diff to empty, got: {stdout:?}",
        );
    }

    #[test]
    fn diff_unified_mode_emits_unified_diff_markers() {
        // Spec scenario "Unified flag emits textual diff": `--unified` produces
        // a unified diff with `---`/`+++` headers and `@@` hunk markers.
        let a = write_tmp("server:\n  port: 8080\n", ".yaml");
        let b = write_tmp("server:\n  port: 9090\n", ".yaml");
        let a_path = utf8_path(&a);
        let b_path = utf8_path(&b);
        let cli = cli_for(&a_path, &b_path, &["--no-color"]);
        let args = DiffArgs {
            a: a_path,
            b: b_path,
            unified: true,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).expect("unified diff");
        let stdout = String::from_utf8(out).expect("utf-8 stdout");
        assert!(
            stdout.contains("---"),
            "unified diff must contain `---` header, got:\n{stdout}",
        );
        assert!(
            stdout.contains("+++"),
            "unified diff must contain `+++` header, got:\n{stdout}",
        );
        assert!(
            stdout.contains("@@"),
            "unified diff must contain `@@` hunk markers, got:\n{stdout}",
        );
    }

    #[test]
    fn diff_rejects_in_place_flag() {
        // `diff` is a read-only subcommand: passing `-i` must be rejected
        // with `INVALID_INPUT` (exit 6) before any I/O happens.
        let cli =
            Cli::try_parse_from(["dq", "-i", "diff", "/nope/a.yaml", "/nope/b.yaml"]).unwrap();
        let args = DiffArgs {
            a: Utf8PathBuf::from("/nope/a.yaml"),
            b: Utf8PathBuf::from("/nope/b.yaml"),
            unified: false,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out).expect_err("must reject -i");
        assert!(
            err.downcast_ref::<crate::error::InvalidInput>().is_some(),
            "rejection must carry InvalidInput so exit-code mapper picks 6, got: {err:?}",
        );
        assert!(
            err.to_string().contains("--in-place"),
            "error message should mention --in-place, got: {err}",
        );
    }
}
