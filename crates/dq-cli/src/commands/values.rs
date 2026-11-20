//! `dq values FILE POINTER` — list the values of the addressed object, or
//! the items of the addressed array.
//!
//! Output shape mirrors `keys` but with the values themselves: console emits
//! one element per line, JSON emits a JSON array. The values are printed as
//! `get` prints them, so a NaN, an infinity or a big number reads the same
//! in every output format. The handler returns [`WrongType`] (exit 1) when
//! the pointer addresses a scalar.

use std::io::Write;

use dq_core::{Pointer, Value};

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, ValuesArgs};
use crate::error::WrongType;
use crate::output::Reporter;

/// Run the `values` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `values` is a read subcommand.
/// - [`WrongType`] (exit 1) when the pointer addresses a scalar.
/// - Same shape as `keys` otherwise: path errors map to exit 2, I/O and
///   parse errors map to their respective codes.
pub fn run(
    cli: &Cli,
    args: &ValuesArgs,
    input_format: Option<&str>,
    doc_arg: Option<crate::cli::DocSelector>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;
    let (_fmt, doc) = load_document_with_path(&args.file, input_format, cli.template_mode())?;
    let view = select_document(&doc, doc_arg, &args.file)?;
    let pointer = Pointer::parse(&args.pointer).map_err(anyhow::Error::new)?;
    let value = pointer.resolve(view.as_ref()).map_err(anyhow::Error::new)?;
    let items = match value {
        Value::Map(map) => map.values().cloned().collect(),
        Value::Array(items) => items.clone(),
        scalar => {
            return Err(anyhow::Error::new(WrongType {
                action: "list the values of",
                pointer: pointer.as_canonical(),
                found: scalar.type_name(),
                expected: "an object or an array",
            }));
        }
    };
    reporter.report_value(&Value::Array(items), out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
    use crate::output::ConsoleReporter;
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
        Cli::try_parse_from(["dq", "values", file, ""]).expect("clap parse")
    }

    #[test]
    fn values_lists_object_values_in_source_order() {
        let tmp = write_yaml("z: 1\na: 2\nm: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
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
    fn values_lists_array_items() {
        let tmp = write_yaml("a:\n  - 1\n  - x\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ValuesArgs {
            file: path,
            pointer: "/a".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "1\nx\n");
    }

    #[test]
    fn values_rejects_a_scalar() {
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ValuesArgs {
            file: path,
            pointer: "/a".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        let wrong = err.downcast_ref::<WrongType>().expect("WrongType");
        assert_eq!((wrong.pointer.as_str(), wrong.found), ("/a", "int"));
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
