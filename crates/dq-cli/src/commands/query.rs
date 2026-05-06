//! `dq query EXPR FILE` — evaluate a jq expression over the document.
//!
//! Pipeline:
//!
//! 1. Reject every write-mode flag via [`Cli::ensure_no_write_flags`] —
//!    `query` is a read subcommand.
//! 2. Load + parse the file (or stdin) through the same shared helpers the
//!    other read commands use.
//! 3. Resolve `--doc <idx|all>` against the parsed [`Document`].
//! 4. Convert the selected view into a [`serde_json::Value`].
//! 5. Compile the jq expression once via [`dq_transform::JqEngine::compile`].
//!    A compile failure becomes a [`dq_core::Error::Parse`] so the exit-code
//!    mapper picks `PARSE_ERROR` (3) — same family as file-parse failures.
//! 6. Evaluate the filter against the input value. A runtime error becomes a
//!    plain `anyhow::anyhow!` so the exit-code mapper picks `GENERIC` (1) —
//!    the file and the expression were both fine, only the evaluation failed.
//! 7. Materialise the output stream and hand it to the configured reporter as
//!    a `serde_json::Value::Array`. The console reporter's "array → one
//!    element per line" rendering matches the M1 contract for `select`, so we
//!    reuse it without a special case.

use std::io::Write;

use camino::Utf8Path;

use super::io_helpers::{load_document_with_path, select_document, value_to_serde_json};
use crate::cli::{Cli, QueryArgs};
use crate::output::Reporter;

/// Run the `query` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`, `--check`, `--continue-on-error`,
///   `--parallel`) is set.
/// - [`dq_core::Error::Io`] / `Parse` / `UnsupportedFormat` for the usual
///   file-load failures.
/// - [`dq_core::Error::Parse`] (exit 3) when the jq expression fails to
///   compile — the byte offset, snippet, and message come from
///   [`dq_transform::JqError::Compile`].
/// - `anyhow::Error` (exit 1) when the compiled filter raises a runtime
///   exception during evaluation.
pub fn run(
    cli: &Cli,
    args: &QueryArgs,
    _input_format: Option<&str>,
    doc_arg: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;

    // For `query`, `-F` is the **output** format (the reporter format), not
    // the input parser. The input format is detected from the file extension
    // so that `dq query '.spec.containers[].image' deployment.yaml -F json`
    // does the obvious thing (parse YAML, render JSON).
    //
    // The single exception is stdin (`file == "-"`): there is no extension to
    // fall back on, so we honour `cli.format` as a last resort. Without this,
    // `dq query '.foo' - -F yaml` would hit the "stdin requires -F" guard in
    // `pick_format` even though the user *did* supply `-F`.
    //
    // The dispatcher in `lib.rs` still passes `input_format` for symmetry with
    // the other handler signatures, but we deliberately ignore it (note the
    // leading underscore in the parameter name) so the override only ever
    // applies to stdin.
    let input_format = if args.file == "-" {
        cli.format.as_input_format_name()
    } else {
        None
    };

    let (_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    let view = select_document(&doc, doc_arg)?;
    let serde_value = value_to_serde_json(&view);

    // Compile errors → dq_core::Error::Parse → exit 3 (PARSE_ERROR).
    let engine = dq_transform::JqEngine::compile(&args.expression)
        .map_err(|e| anyhow::Error::new(jq_compile_to_parse(e, &args.file, &args.expression)))?;

    // Runtime / Conversion / FeatureDisabled → plain anyhow → exit 1
    // (GENERIC). The file parsed and the expression compiled — only the
    // evaluation against this specific input failed, which doesn't fit any
    // of the structured exit-code categories.
    let outputs = engine
        .run(&serde_value)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let arr = serde_json::Value::Array(outputs);
    reporter.report(&arr, out)?;
    Ok(())
}

/// Convert a [`dq_transform::JqError::Compile`] into a [`dq_core::Error::Parse`].
///
/// Only the `Compile` variant maps to `Parse`; runtime / conversion /
/// feature-disabled errors are caller responsibility (they go through the
/// plain `anyhow::anyhow!` route to land on exit 1). This helper is
/// `pub(super)` so the `set --jq` handler can reuse the same mapping when
/// surfacing compile failures from its own engine call site.
///
/// The `_expression` parameter is currently unused — `JqError::Compile`
/// already carries a prepared `snippet` from the engine's own slicing pass —
/// but the parameter stays in the signature so a future change can re-derive
/// the snippet locally without touching every call site.
pub(super) fn jq_compile_to_parse(
    err: dq_transform::JqError,
    file: &Utf8Path,
    _expression: &str,
) -> dq_core::Error {
    match err {
        dq_transform::JqError::Compile {
            snippet,
            position,
            message,
        } => {
            // The expression is one line at the CLI; column maps directly to
            // the byte offset jaq reported. `u32::try_from` cannot overflow
            // in practice (jq expressions on the command line are short) but
            // we saturate just in case so a giant expression doesn't panic.
            let col = u32::try_from(position).unwrap_or(u32::MAX);
            dq_core::Error::Parse {
                file: Some(file.to_path_buf()),
                line: 1,
                col,
                span: position..position,
                snippet,
                message: format!("jq: {message}"),
            }
        }
        // Caller should not invoke this for non-Compile variants — fall
        // through with a generic Format error so we never silently mis-map a
        // runtime failure to PARSE_ERROR.
        other => dq_core::Error::Format {
            format: "jq",
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
    use crate::output::JsonReporter;
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

    fn cli_for(extra: &[&str], file: &str) -> Cli {
        let mut argv = vec!["dq"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&["query", ".", file]);
        Cli::try_parse_from(argv).expect("clap parse")
    }

    #[test]
    fn query_identity_filter_round_trips_yaml() {
        let tmp = write_yaml("foo: 1\nbar: 2\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_for(&[], path.as_str());
        let args = QueryArgs {
            expression: ".".to_owned(),
            file: path,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).expect("identity should succeed");
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // Output is wrapped in a single-element array (the jq stream model).
        assert_eq!(parsed, serde_json::json!([{"foo": 1, "bar": 2}]));
    }

    #[test]
    fn query_field_selector_returns_value() {
        let tmp = write_yaml("foo: 42\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_for(&[], path.as_str());
        let args = QueryArgs {
            expression: ".foo".to_owned(),
            file: path,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).expect("field selector");
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!([42]));
    }

    #[test]
    fn query_rejects_in_place_with_invalid_input_marker() {
        // The write-flag gate must reject `-i` before any I/O happens — the
        // file at the path is intentionally non-existent because the gate
        // should short-circuit first.
        let cli = Cli::try_parse_from(["dq", "-i", "query", ".", "/nope.yaml"]).unwrap();
        let args = QueryArgs {
            expression: ".".to_owned(),
            file: camino::Utf8PathBuf::from("/nope.yaml"),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out)
            .expect_err("--in-place must be rejected");
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "rejection must carry InvalidInput so exit code is 6, got: {err:?}",
        );
        assert!(
            err.to_string().contains("--in-place"),
            "error should name the offending flag, got: {err}",
        );
    }

    #[test]
    fn query_compile_error_maps_to_parse_error() {
        // A malformed jq expression must surface as `dq_core::Error::Parse`
        // so the exit-code mapper picks `PARSE_ERROR` (3) — same as a
        // file-parse failure.
        let tmp = write_yaml("x: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_for(&[], path.as_str());
        let args = QueryArgs {
            expression: ".foo |=".to_owned(),
            file: path,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err =
            run(&cli, &args, None, None, &reporter, &mut out).expect_err("malformed jq must error");
        let domain = err
            .downcast_ref::<dq_core::Error>()
            .expect("compile failures map to dq_core::Error::Parse");
        assert_eq!(domain.kind_name(), "parse", "got: {domain:?}");
    }

    #[test]
    fn query_with_format_override_still_uses_extension_for_input_parsing() {
        // Regression: passing `-F json` is an OUTPUT-format hint for `query`.
        // It must NOT be forwarded to the input parser — otherwise a YAML file
        // gets parsed as JSON and fails with PARSE_ERROR (exit 3).
        //
        // Before the fix, the dispatcher in `lib.rs` forwarded
        // `cli.format.as_input_format_name()` (Some("json") under `-F json`)
        // straight into `load_document_with_path`, overriding the `.yaml`
        // extension. The smoke test
        //   dq query '.spec.containers[].image' deployment.yaml -F json
        // would error instead of returning ["a","b"].
        //
        // This test pins the new contract: even if the dispatcher passes
        // `Some("json")` (we mimic that here by hand), the handler must
        // ignore it for non-stdin inputs and detect the format from the
        // file extension. Only the stdin branch consults `cli.format`.
        let tmp = write_yaml("spec:\n  containers:\n    - image: a\n    - image: b\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = Cli::try_parse_from([
            "dq",
            "-F",
            "json",
            "query",
            ".spec.containers[].image",
            path.as_str(),
        ])
        .unwrap();
        let args = QueryArgs {
            expression: ".spec.containers[].image".to_owned(),
            file: path,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        // Pass `Some("json")` deliberately — the handler must ignore it.
        run(&cli, &args, Some("json"), None, &reporter, &mut out)
            .expect("YAML file with -F json should parse via extension, render JSON");
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!(["a", "b"]));
    }

    #[test]
    fn query_runtime_error_does_not_map_to_parse() {
        // Type-mismatched arithmetic at evaluation time must NOT become a
        // `Parse` error. The file parsed fine, the expression compiled fine
        // — only the evaluation against this specific data failed, which is
        // GENERIC (1) territory.
        let tmp = write_yaml("\"hello\"\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_for(&[], path.as_str());
        let args = QueryArgs {
            expression: ". + 1".to_owned(),
            file: path,
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out)
            .expect_err("runtime type error must surface");
        // Must NOT downcast to dq_core::Error::Parse — that would mis-map to
        // exit 3 instead of exit 1.
        assert!(
            err.downcast_ref::<dq_core::Error>().is_none(),
            "runtime errors must stay as plain anyhow so exit code is 1, got: {err:?}",
        );
    }
}
