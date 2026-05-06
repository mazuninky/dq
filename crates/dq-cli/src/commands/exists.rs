//! `dq exists FILE POINTER` — exit 0 when the pointer addresses a node, exit 1 otherwise.
//!
//! On a miss the handler returns [`SilentError`] so `main.rs` keeps stderr
//! empty: per spec, `exists` reports presence purely via exit code. I/O,
//! parse, and unsupported-format failures still surface their structured
//! error chains as usual.

use dq_core::Pointer;

use super::io_helpers::{load_document_with_path, select_document};
use crate::cli::{Cli, ExistsArgs};
use crate::error::SilentError;

/// Run the `exists` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `exists` is a read subcommand.
/// - [`SilentError`] when the pointer does not address an existing node —
///   `main.rs` recognises it and exits 1 without writing to stderr.
/// - `dq_core::Error::*` for I/O, parse, or unsupported-format failures.
pub fn run(
    cli: &Cli,
    args: &ExistsArgs,
    input_format: Option<&str>,
    doc_arg: Option<&str>,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;
    let (_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    let view = select_document(&doc, doc_arg)?;
    let pointer = Pointer::parse(&args.pointer).map_err(anyhow::Error::new)?;
    match pointer.resolve(view.as_ref()) {
        Ok(_) => Ok(()),
        Err(_) => Err(anyhow::Error::new(SilentError)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
    use clap::Parser;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "exists", file, "/dummy"]).expect("clap parse")
    }

    #[test]
    fn exists_returns_ok_on_match() {
        let tmp = write_yaml("server:\n  port: 8080\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ExistsArgs {
            file: path,
            pointer: "/server/port".to_owned(),
        };
        run(&cli, &args, None, None).expect("pointer should exist");
    }

    #[test]
    fn exists_returns_silent_error_on_miss() {
        let tmp = write_yaml("server:\n  port: 8080\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = ExistsArgs {
            file: path,
            pointer: "/server/missing".to_owned(),
        };
        let err = run(&cli, &args, None, None).unwrap_err();
        assert!(
            err.downcast_ref::<SilentError>().is_some(),
            "expected SilentError so main.rs suppresses stderr; got: {err:?}",
        );
    }

    #[test]
    fn exists_propagates_io_error_for_missing_file() {
        let cli = cli_no_flags("/no/such/file.yaml");
        let args = ExistsArgs {
            file: camino::Utf8PathBuf::from("/no/such/file.yaml"),
            pointer: "/x".to_owned(),
        };
        let err = run(&cli, &args, None, None).unwrap_err();
        let domain = err.downcast_ref::<dq_core::Error>().unwrap();
        assert_eq!(domain.kind_name(), "io");
    }

    #[test]
    fn exists_rejects_in_place_flag_before_io() {
        // Construct a `Cli` with `-i` so the gate fires before the handler
        // attempts to read the (non-existent) file.
        let cli = Cli::try_parse_from(["dq", "-i", "exists", "/nope.yaml", "/foo"]).unwrap();
        let args = ExistsArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
            pointer: "/foo".to_owned(),
        };
        let err = run(&cli, &args, None, None).unwrap_err();
        assert!(
            err.downcast_ref::<InvalidInput>().is_some(),
            "expected InvalidInput marker so exit-code mapper picks 6, got: {err:?}",
        );
    }
}
