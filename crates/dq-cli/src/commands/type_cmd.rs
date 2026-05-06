//! `dq type FILE POINTER` — print the type name of the addressed node.
//!
//! Module name is `type_cmd` (not `type`) because `type` is a Rust keyword
//! and using it as a module name forces awkward `r#type` everywhere. The
//! type-name catalogue lives on [`dq_core::Value::type_name`] and returns
//! one of `null`, `bool`, `int`, `float`, `string`, `array`, `object`.

use std::io::Write;

use dq_core::Pointer;

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, TypeArgs};
use crate::output::Reporter;

/// Run the `type` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `type` is a read subcommand.
/// - The usual `dq_core::Error` family on I/O, parse, or pointer-resolve
///   failure.
pub fn run(
    cli: &Cli,
    args: &TypeArgs,
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
    let json = serde_json::Value::String(value.type_name().to_owned());
    reporter.report(&json, out)?;
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
        Cli::try_parse_from(["dq", "type", file, ""]).expect("clap parse")
    }

    #[test]
    fn type_reports_int_for_integer_scalar() {
        let tmp = write_yaml("port: 8080\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = TypeArgs {
            file: path,
            pointer: "/port".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "int\n");
    }

    #[test]
    fn type_reports_object_for_map() {
        let tmp = write_yaml("server:\n  host: x\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = TypeArgs {
            file: path,
            pointer: "/server".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "object\n");
    }

    #[test]
    fn type_reports_null() {
        let tmp = write_yaml("a: ~\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = TypeArgs {
            file: path,
            pointer: "/a".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "null\n");
    }

    #[test]
    fn type_rejects_in_place_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "-i", "type", "/nope.yaml", ""]).unwrap();
        let args = TypeArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            pointer: String::new(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
