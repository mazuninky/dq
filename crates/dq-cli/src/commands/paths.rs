//! `dq paths FILE` — list every JSON Pointer addressable in the document.
//!
//! Output shape:
//! - Console: one pointer per line (the reporter renders an object as
//!   `<pointer>: <type>` per line; here we emit a JSON object whose keys are
//!   pointers and values are leaf type names — see comment below).
//! - JSON: a JSON object whose keys are pointers and values are leaf type
//!   names. Per spec: "writes a JSON object whose keys are pointers (RFC 6901
//!   strings) and whose values are leaf type names".
//!
//! `serde_json::Map` preserves insertion order through the workspace's
//! `preserve_order` feature, so the JSON object retains the pre-order walk.

use std::io::Write;

use dq_core::enumerate_pointers;

use super::io_helpers::load_document_with_path;
use crate::cli::{Cli, PathsArgs};
use crate::output::Reporter;

/// Run the `paths` command.
///
/// # Errors
///
/// - [`crate::error::InvalidInput`] (exit 6) when any write-mode flag
///   (`-i`, `--diff`, `--backup`) is set — `paths` is a read subcommand.
/// - I/O / parse / unsupported-format errors as usual; enumerating pointers
///   itself never fails.
pub fn run(
    cli: &Cli,
    args: &PathsArgs,
    input_format: Option<&str>,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    cli.ensure_no_write_flags()?;
    let (_fmt, doc) = load_document_with_path(&args.file, input_format)?;
    let mut obj = serde_json::Map::new();
    for (pointer, type_name) in enumerate_pointers(&doc) {
        let key = pointer.as_canonical();
        obj.insert(key, serde_json::Value::String(type_name.to_owned()));
    }
    reporter.report(&serde_json::Value::Object(obj), out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidInput;
    use crate::output::{ConsoleReporter, JsonReporter};
    use clap::Parser;
    use tempfile::NamedTempFile;

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::with_suffix(".yaml").unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp
    }

    fn cli_no_flags(file: &str) -> Cli {
        Cli::try_parse_from(["dq", "paths", file]).expect("clap parse")
    }

    #[test]
    fn paths_emits_root_and_children_in_pre_order_console() {
        let tmp = write_yaml("server:\n  port: 8080\n  host: x\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = PathsArgs { file: path };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        // ConsoleReporter renders `key: value` per line for objects.
        // The empty pointer is `""` followed by `: object` and is the FIRST
        // line emitted (pre-order walk visits the root first). Compare against
        // the literal first line rather than `contains(": object\n")` — the
        // latter would also match `/server: object\n` and accept a broken
        // implementation that omits the root entry entirely.
        assert_eq!(
            s.lines().next(),
            Some(": object"),
            "first line must be the root entry: {s:?}"
        );
        assert!(s.contains("/server: object"), "missing /server: {s:?}");
        assert!(
            s.contains("/server/port: int"),
            "missing /server/port: {s:?}"
        );
        assert!(
            s.contains("/server/host: string"),
            "missing /server/host: {s:?}"
        );
    }

    #[test]
    fn paths_json_output_is_an_object() {
        let tmp = write_yaml("a: 1\nb: c\n");
        let path = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let cli = cli_no_flags(path.as_str());
        let args = PathsArgs { file: path };
        let reporter = JsonReporter;
        let mut out: Vec<u8> = Vec::new();
        run(&cli, &args, None, &reporter, &mut out).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let serde_json::Value::Object(map) = parsed else {
            panic!("expected object");
        };
        assert_eq!(map.get("/a"), Some(&serde_json::json!("int")));
        assert_eq!(map.get("/b"), Some(&serde_json::json!("string")));
        // Root entry uses the empty string as key (canonical RFC 6901 root form).
        assert_eq!(map.get(""), Some(&serde_json::json!("object")));
    }

    #[test]
    fn paths_rejects_in_place_flag_before_io() {
        let cli = Cli::try_parse_from(["dq", "-i", "paths", "/nope.yaml"]).unwrap();
        let args = PathsArgs {
            file: camino::Utf8PathBuf::from("/nope.yaml"),
        };
        let reporter = ConsoleReporter::new(false);
        let mut out: Vec<u8> = Vec::new();
        let err = run(&cli, &args, None, &reporter, &mut out).unwrap_err();
        assert!(err.downcast_ref::<InvalidInput>().is_some());
    }
}
