//! `dq values FILE POINTER` — list the values of the addressed object.
//!
//! Output shape mirrors `keys` but with the values themselves: console emits
//! one element per line, JSON emits a JSON array. The handler returns
//! `dq_core::Error::Path { kind: TypeMismatch, .. }` when the pointer
//! addresses a non-object node.

use std::io::Write;

use dq_core::{Pointer, Value};

use super::io_helpers::{load_document_with_path, select_document, value_to_serde_json};
use crate::cli::{Cli, ValuesArgs};
use crate::output::Reporter;

/// Run the `values` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `values` is a read subcommand.
/// - Same shape as `keys` otherwise: path/type errors map to exit 2/1 per
///   the exit-code mapper, I/O and parse errors map to their respective codes.
pub fn run(
    cli: &Cli,
    args: &ValuesArgs,
    input_format: Option<&str>,
    doc_arg: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;
    let (_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    let view = select_document(&doc, doc_arg)?;
    let pointer = Pointer::parse(&args.pointer).map_err(anyhow::Error::new)?;
    let value = pointer.resolve(view.as_ref()).map_err(anyhow::Error::new)?;
    let Value::Map(map) = value else {
        return Err(anyhow::Error::new(dq_core::Error::Path {
            pointer: pointer.as_canonical(),
            matched_prefix: pointer.as_canonical(),
            kind: dq_core::PathErrorKind::TypeMismatch {
                expected: "object",
                found: value.type_name(),
            },
            did_you_mean: Vec::new(),
        }));
    };
    let items: Vec<serde_json::Value> = map.values().map(value_to_serde_json).collect();
    reporter.report(&serde_json::Value::Array(items), out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
    use crate::output::ConsoleReporter;
    use clap::Parser;
    use tempfile::NamedTempFile;

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "values", file, ""]).expect("clap parse")
    }

    #[test]
    fn values_lists_object_values_in_source_order() {
        let tmp = write_yaml("z: 1\na: 2\nm: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ValuesArgs {
            file: path,
            pointer: String::new(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "1\n2\n3\n");
    }

    #[test]
    fn values_rejects_array_pointer() {
        let tmp = write_yaml("a:\n  - 1\n  - 2\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ValuesArgs {
            file: path,
            pointer: "/a".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "path");
    }

    #[test]
    fn values_rejects_diff_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "--diff", "values", "/nope.yaml", ""]).unwrap();
        let args = ValuesArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            pointer: String::new(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
