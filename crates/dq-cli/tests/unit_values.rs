//! Unit tests for `dq values` driven through `dq::run`.
//!
//! Spec contract: lists the values of an object or the items of an array,
//! returns `WrongType` (exit 1) when the pointer addresses a scalar. JSON
//! output is a JSON array.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use tempfile::NamedTempFile;

/// Returns a `TempPath` so the underlying handle is closed before the binary
/// touches the path. Windows requires this for in-place rewrites — the same
/// pattern propagated from `cli_set_jq.rs`. Applied uniformly even on
/// read-only sites for consistency.
fn write_yaml(content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

#[test]
fn values_emits_one_per_line_in_source_order() {
    let tmp = write_yaml("z: 100\na: 200\nm: 300\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "values", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("values must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "100\n200\n300\n");
    assert!(err.is_empty());
}

#[test]
fn values_against_array_lists_its_items() {
    let tmp = write_yaml("- 1\n- [2, 3]\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "values", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("values on an array lists its items");
    assert_eq!(String::from_utf8(out).unwrap(), "1\n[2, 3]\n");
}

#[test]
fn values_against_scalar_is_a_wrong_type_error() {
    let tmp = write_yaml("a: hi\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "values", path, "/a", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("values on a scalar must error");
    let wrong = e.downcast_ref::<dq::WrongType>().expect("WrongType");
    assert_eq!(wrong.found, "string");
    assert_eq!(
        e.to_string(),
        "cannot list the values of '/a': it is string, not an object or an array"
    );
    assert_eq!(dq::exit_code::exit_code_for_error(&e), 1);
}

#[test]
fn values_with_json_format_emits_json_array_preserving_types() {
    // The values are heterogeneous — int, string, bool — and JSON output must
    // preserve their types (not stringify them). Note: `-F json` overrides
    // the input parser too, so we feed it a JSON file.
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    std::io::Write::write_all(&mut tmp, br#"{"a": 1, "b": "hello", "c": true}"#).unwrap();
    let tmp = tmp.into_temp_path();
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "values", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("values must succeed");
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed, serde_json::json!([1, "hello", true]));
}
