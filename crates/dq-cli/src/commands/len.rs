//! `dq len FILE POINTER` — print the length of the addressed array, string, or object.
//!
//! Length semantics:
//! - `Array` → number of elements.
//! - `Map`   → number of keys.
//! - `String` → number of `char`s. **Note**: the spec calls for UTF-8 grapheme
//!   clusters; M1 simplifies this to `chars().count()` which counts Unicode
//!   scalar values rather than user-perceived characters. Real grapheme
//!   counting requires `unicode-segmentation` and is deferred — see the
//!   workspace-level `dq-plan.md` M2/M9 notes.
//! - Anything else (null, bool, int, float, big-int, big-float) is a
//!   `TypeMismatch` `Path` error.

use std::io::Write;

use dq_core::{Pointer, Value};

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, LenArgs};
use crate::output::Reporter;

/// Run the `len` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `len` is a read subcommand.
/// - `dq_core::Error::Path { kind: TypeMismatch, .. }` when the pointer
///   addresses an unsized value (null/bool/int/float).
/// - `dq_core::Error::*` for the usual I/O, parse, and unsupported-format
///   failures.
pub fn run(
    cli: &Cli,
    args: &LenArgs,
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
    let len: i64 = match value {
        Value::Array(items) => items.len() as i64,
        Value::Map(map) => map.len() as i64,
        Value::String(s) => s.chars().count() as i64,
        other => {
            return Err(anyhow::Error::new(dq_core::Error::Path {
                pointer: pointer.as_canonical(),
                matched_prefix: pointer.as_canonical(),
                kind: dq_core::PathErrorKind::TypeMismatch {
                    expected: "array, string, or object",
                    found: other.type_name(),
                },
                did_you_mean: Vec::new(),
            }));
        }
    };
    let json = serde_json::Value::Number(len.into());
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

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "len", file, ""]).expect("clap parse")
    }

    #[test]
    fn len_reports_array_length() {
        let tmp = write_yaml("- a\n- b\n- c\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = LenArgs {
            file: path,
            pointer: String::new(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "3\n");
    }

    #[test]
    fn len_reports_string_char_count() {
        // 'café' has 4 chars but 5 bytes — chars().count() is the right unit.
        let tmp = write_yaml("greeting: café\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = LenArgs {
            file: path,
            pointer: "/greeting".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "4\n");
    }

    #[test]
    fn len_rejects_scalar() {
        let tmp = write_yaml("flag: true\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = LenArgs {
            file: path,
            pointer: "/flag".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "path");
    }

    #[test]
    fn len_rejects_backup_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "--backup", "len", "/nope.yaml", ""]).unwrap();
        let args = LenArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            pointer: String::new(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
