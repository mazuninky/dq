//! Unit tests for `dq keys` driven through `dq::run`.
//!
//! Spec contract: lists the keys of an object or the indices of an array,
//! returns `WrongType` (exit 1) when the pointer addresses a scalar.

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
fn keys_lists_object_keys_one_per_line_in_source_order() {
    // Console reporter prints one key per line. Source order matters: keys
    // come from `IndexMap`, so insertion order is preserved.
    let tmp = write_yaml("z: 1\na: 2\nm: 3\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "keys", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("keys must succeed");
    let stdout = String::from_utf8(out).unwrap();
    assert_eq!(stdout, "z\na\nm\n", "expected source order, got {stdout:?}");
    assert!(err.is_empty());
}

#[test]
fn keys_against_scalar_returns_type_mismatch_error() {
    // Pointer addresses a scalar — keys reports a wrong-type error, exit 1:
    // the node is there, so not "not found" (exit 2).
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "keys", path, "/a", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("keys on scalar must error");
    let wrong = e.downcast_ref::<dq::WrongType>().expect("WrongType");
    assert_eq!(wrong.found, "int");
    assert_eq!(
        e.to_string(),
        "cannot list the keys of '/a': it is int, not an object or an array"
    );
    assert_eq!(dq::exit_code::exit_code_for_error(&e), 1);
}

#[test]
fn keys_with_json_format_writes_json_array() {
    // Switching to `-F json` makes the reporter emit a JSON array instead of
    // one-per-line console text. Note: `-F` is overloaded — for non-convert
    // commands it also switches the *input* parser, so we use a `.json` file
    // here instead of YAML.
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    std::io::Write::write_all(&mut tmp, br#"{"a": 1, "b": 2}"#).unwrap();
    let tmp = tmp.into_temp_path();
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "keys", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("keys must succeed");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("JSON reporter must emit valid JSON");
    assert_eq!(parsed, serde_json::json!(["a", "b"]));
}

#[test]
fn keys_against_array_lists_its_indices() {
    // The keys of an array are the indices a pointer takes into it.
    let tmp = write_yaml("- a\n- b\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "keys", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("keys on an array lists its indices");
    assert_eq!(String::from_utf8(out).unwrap(), "0\n1\n");
}
