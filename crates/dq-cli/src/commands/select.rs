//! `dq select FILE EXPR` — JSONPath query (RFC 9535 subset) over the document.
//!
//! Reaches for `jsonpath_rust = "1.0"` (RFC 9535 grammar) because that
//! crate's API is what the workspace pins. Notes on the API:
//! - `JsonPath::query(&value, &str)` parses and evaluates in one call and
//!   returns `Vec<&Value>`; a parse failure is the `Err`. Descendant
//!   segments (`$..*`) list a node's children before descending into them.
//! - Blanks are allowed between segments and inside a member-name shorthand,
//!   where they stay part of the name: `$.a b` looks up the key `"a b"`
//!   instead of failing to parse.
//! - An empty `Vec` means "no matches" — there is no sentinel value.
//!
//! Per spec, an empty match list is NOT an error: stdout becomes `[]` and the
//! exit code is 0.

use std::io::Write;

use jsonpath_rust::JsonPath;

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, SelectArgs};
use crate::output::Reporter;

/// Run the `select` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `select` is a read subcommand.
/// - `dq_core::Error::Io` / `Parse` / `UnsupportedFormat` for the usual
///   I/O and parse failures.
/// - `anyhow::Error` (exit 1) when the JSONPath expression itself is
///   malformed.
pub fn run(
    cli: &Cli,
    args: &SelectArgs,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;
    let (_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    // Same view as `get`: document 0 of a stream unless `--doc` says
    // otherwise; `--doc all` queries the array of every document.
    let value = select_document(&doc, cli.doc, &args.file)?.to_serde_json();

    let matched: Vec<serde_json::Value> = value
        .query(args.jsonpath.as_str())
        .map_err(|e| anyhow::anyhow!("invalid JSONPath expression: {e}"))?
        .into_iter()
        .cloned()
        .collect();

    let arr = serde_json::Value::Array(matched);
    reporter.report(&arr, out)?;
    Ok(())
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

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "select", file, "$"]).expect("clap parse")
    }

    #[test]
    fn select_returns_array_of_one_for_single_match() {
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.spec.replicas".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!([3]));
    }

    #[test]
    fn select_returns_empty_array_for_no_match() {
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.does.not.exist".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        // Per spec: empty match list is NOT an error.
        run(&cli, &args, None, &reporter, &mut out).expect("empty match should not error");
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!([]));
    }

    #[test]
    fn select_returns_multi_match_in_order() {
        let tmp =
            write_yaml("spec:\n  containers:\n    - image: a\n    - image: b\n    - image: c\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.spec.containers[*].image".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, serde_json::json!(["a", "b", "c"]));
    }

    #[test]
    fn select_rejects_malformed_expression() {
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "this is not jsonpath".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out).unwrap_err();
        assert!(err.to_string().contains("invalid JSONPath"));
    }

    #[test]
    fn select_evaluates_rfc_9535_filters_and_negative_indices() {
        let tmp = write_yaml("items:\n  - {name: x, n: 1}\n  - {name: y, n: 2}\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        for expr in ["$.items[?@.n > 1].name", "$.items[-1].name"] {
            let args = SelectArgs {
                file: path.clone(),
                jsonpath: expr.to_owned(),
            };
            let mut out: Vec<u8> = Vec::new();
            run(&cli, &args, None, &JsonReporter, &mut out).unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(parsed, serde_json::json!(["y"]), "{expr}");
        }
    }

    #[test]
    fn select_rejects_non_rfc_9535_length_method() {
        let tmp = write_yaml("items: [1, 2]\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = SelectArgs {
            file: path,
            jsonpath: "$.items.length()".to_owned(),
        };
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &JsonReporter, &mut out).unwrap_err();
        assert!(err.to_string().contains("invalid JSONPath"), "{err}");
    }

    #[test]
    fn select_rejects_in_place_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "-i", "select", "/nope.yaml", "$"]).unwrap();
        let args = SelectArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            jsonpath: "$".to_owned(),
        };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out).unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
