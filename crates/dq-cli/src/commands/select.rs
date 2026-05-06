//! `dq select FILE EXPR` — JSONPath query (RFC 9535 subset) over the document.
//!
//! Reaches for `jsonpath_rust = "0.7"` because that crate's API is what the
//! workspace pins. Notes on the API:
//! - `JsonPath::try_from(&str)` parses the expression.
//! - `path.find_slice_ptr(&value)` returns `Vec<JsonPtr<Value>>` — an empty
//!   `Vec` means "no matches" (in contrast with `find_slice`, which inserts a
//!   sentinel `NoValue` for "no matches").
//!
//! Per spec, an empty match list is NOT an error: stdout becomes `[]` and the
//! exit code is 0.

use std::io::Write;

use jsonpath_rust::{JsonPath, JsonPtr};

use super::io_helpers::{load_document_with_path, value_to_serde_json};
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
    // The `--doc` flag is meaningful for multi-doc YAML, but for `select` we
    // adopt the same M1 default as `get`: doc 0 for `Multi`. We don't honour
    // `--doc` here yet because the spec does not mandate it for `select`.
    let value: serde_json::Value = if let Some(values) = doc.values() {
        values
            .first()
            .map(value_to_serde_json)
            .unwrap_or(serde_json::Value::Null)
    } else {
        value_to_serde_json(doc.value())
    };

    let path = JsonPath::try_from(args.jsonpath.as_str())
        .map_err(|e| anyhow::anyhow!("invalid JSONPath expression: {e}"))?;

    let matched: Vec<serde_json::Value> = path
        .find_slice_ptr(&value)
        .into_iter()
        .map(|ptr| match ptr {
            JsonPtr::Slice(v) => v.clone(),
            JsonPtr::NewValue(v) => v,
        })
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

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "select", file, "$"]).expect("clap parse")
    }

    #[test]
    fn select_returns_array_of_one_for_single_match() {
        let tmp = write_yaml("spec:\n  replicas: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
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
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
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
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
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
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
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
