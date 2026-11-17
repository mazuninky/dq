//! Unit tests for `dq type` driven through `dq::run`.
//!
//! Spec contract: returns one of `null`, `bool`, `int`, `float`, `string`,
//! `array`, `object`. Errors only on path resolution failures.

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
fn type_returns_int_for_integer_scalar() {
    let tmp = write_yaml("port: 8080\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "type", path, "/port", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("type must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "int\n");
}

#[test]
fn type_returns_object_for_map() {
    let tmp = write_yaml("server:\n  host: x\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "type", path, "/server", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("type must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "object\n");
}

#[test]
fn type_returns_array_for_sequence() {
    let tmp = write_yaml("items:\n  - 1\n  - 2\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "type", path, "/items", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("type must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "array\n");
}

#[test]
fn type_missing_pointer_returns_path_error() {
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "type", path, "/missing", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("missing pointer must error");
    let domain = e.downcast_ref::<dq_core::Error>().unwrap();
    assert_eq!(domain.kind_name(), "path");
}

#[test]
fn type_invalid_format_returns_unsupported_format_error() {
    // Confirm the same I/O / format-detection error path also applies to
    // `type` — the handler shares `load_document_with_path`.
    let mut tmp = NamedTempFile::with_suffix(".unknown").expect("tempfile");
    tmp.write_all(b"hello\n").unwrap();
    let tmp = tmp.into_temp_path();
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "type", path, "/x", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("unknown format must error");
    let domain = e.downcast_ref::<dq_core::Error>().unwrap();
    assert_eq!(domain.kind_name(), "unsupported_format");
}
