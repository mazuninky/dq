//! Unit tests for `dq paths` driven through `dq::run`.
//!
//! Spec contract: emits every JSON Pointer in the document. Console output is
//! one pointer per line; JSON output is an object `{pointer: type_name}`.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use tempfile::NamedTempFile;

/// Returns a `TempPath` so the underlying handle is closed before the binary
/// touches the path. Windows requires this for the binary to overwrite the
/// path during in-place edits — the same pattern propagated from
/// `cli_set_jq.rs`. We apply it uniformly even for read-only sites.
fn write_yaml(content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

#[test]
fn paths_console_output_lists_pre_order_pointers() {
    let tmp = write_yaml("server:\n  port: 8080\n  host: x\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "paths", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("paths must succeed");
    let s = String::from_utf8(out).unwrap();
    // Console reporter renders the {pointer: type_name} object as `key: value`
    // per line; the empty pointer maps to the root entry which must be the
    // *first* line (otherwise a stray `: object` from somewhere else would let
    // this assertion pass). Pin the position explicitly.
    assert_eq!(
        s.lines().next(),
        Some(": object"),
        "missing root entry: {s:?}"
    );
    assert!(s.contains("/server: object"));
    assert!(s.contains("/server/port: int"));
    assert!(s.contains("/server/host: string"));
}

#[test]
fn paths_json_output_is_a_pointer_to_type_object() {
    // `-F json` switches both the parser and the reporter; we feed JSON in.
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    std::io::Write::write_all(&mut tmp, br#"{"a": 1, "b": "c"}"#).unwrap();
    let tmp = tmp.into_temp_path();
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "paths", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("paths must succeed");
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("must be valid JSON");
    let serde_json::Value::Object(obj) = parsed else {
        panic!("expected JSON object, got {parsed:?}");
    };
    // The empty-string key represents the document root per RFC 6901 canonical form.
    assert_eq!(obj.get(""), Some(&serde_json::json!("object")));
    assert_eq!(obj.get("/a"), Some(&serde_json::json!("int")));
    assert_eq!(obj.get("/b"), Some(&serde_json::json!("string")));
}

#[test]
fn paths_missing_file_propagates_io_error() {
    let cli = Cli::parse_from(["dq", "paths", "/no/such/file.yaml", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("missing file must error");
    let domain = e.downcast_ref::<dq_core::Error>().unwrap();
    assert_eq!(domain.kind_name(), "io");
}

#[test]
fn paths_handles_empty_document() {
    // A document containing nothing but a single null still has one pointer:
    // the root. The handler must not panic on degenerate inputs. We avoid
    // `-F json` here so the YAML parser handles the `~` token correctly; the
    // ConsoleReporter still writes the root entry as a single line.
    let tmp = write_yaml("~\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "paths", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("paths must succeed on null doc");
    let stdout = String::from_utf8(out).unwrap();
    // Root has empty pointer — console reporter prints `: null\n`.
    assert!(
        stdout.contains(": null"),
        "expected single root entry of type null, got {stdout:?}",
    );
}
