//! `dq keys FILE POINTER` — list the keys of the addressed object.
//!
//! Output shape:
//! - Console: one key per line (handled by [`crate::output::ConsoleReporter`]
//!   when given a JSON array of strings).
//! - JSON: a JSON array of strings — also produced by passing the same array
//!   to the reporter; [`crate::output::JsonReporter`] pretty-prints it.
//!
//! When the pointer addresses a non-object value, the handler returns
//! `dq_core::Error::Path { kind: TypeMismatch, .. }` so the exit-code mapper
//! can route it.

use std::io::Write;

use dq_core::{Pointer, Value};

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, KeysArgs};
use crate::output::Reporter;

/// Run the `keys` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `keys` is a read subcommand.
/// - `dq_core::Error::Path` when the pointer is invalid, missing, or the
///   resolved node is not an object — the latter via `kind = TypeMismatch`.
/// - `dq_core::Error::Io` / `Parse` / `UnsupportedFormat` for the usual
///   I/O and parse failures.
pub fn run(
    cli: &Cli,
    args: &KeysArgs,
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
    let array: Vec<serde_json::Value> = map
        .keys()
        .map(|k| serde_json::Value::String(k.clone()))
        .collect();
    reporter.report(&serde_json::Value::Array(array), out)?;
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
        Cli::try_parse_from(["dq", "keys", file, ""]).expect("clap parse")
    }

    #[test]
    fn keys_lists_object_keys_in_source_order() {
        let tmp = write_yaml("z: 1\na: 2\nm: 3\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = KeysArgs {
            file: path,
            pointer: String::new(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, None, &reporter, &mut out).unwrap();
        // ConsoleReporter prints one element per line for arrays.
        assert_eq!(String::from_utf8(out).unwrap(), "z\na\nm\n");
    }

    #[test]
    fn keys_rejects_non_object_pointer() {
        let tmp = write_yaml("a: 1\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = KeysArgs {
            file: path,
            pointer: "/a".to_owned(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "path");
        match domain {
            dq_core::Error::Path { kind, .. } => {
                assert!(matches!(kind, dq_core::PathErrorKind::TypeMismatch { .. }))
            }
            _ => panic!("expected Path"),
        }
    }

    #[test]
    fn keys_rejects_in_place_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "-i", "keys", "/nope.yaml", ""]).unwrap();
        let args = KeysArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            pointer: String::new(),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, None, &reporter, &mut out).unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker, got: {err:?}",
        );
    }
}
