//! Unit tests for `dq keys` driven through `dq::run`.
//!
//! Spec contract: lists keys of an object, returns TypeMismatch when the
//! pointer addresses a non-object node.

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
    // Pointer addresses a scalar — keys must report a Path/TypeMismatch error
    // so the CLI can show "kind=type" diagnostic.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "keys", path, "/a", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("keys on scalar must error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("expected dq_core::Error");
    assert_eq!(domain.kind_name(), "path");
    match domain {
        dq_core::Error::Path { kind, .. } => assert!(
            matches!(kind, dq_core::PathErrorKind::TypeMismatch { .. }),
            "expected TypeMismatch kind, got {kind:?}",
        ),
        other => panic!("unexpected variant: {other:?}"),
    }
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
fn keys_against_array_returns_type_mismatch_error() {
    // Arrays are not objects — `keys` must reject them too. The error kind is
    // `TypeMismatch { expected: "object", found: "array" }`.
    let tmp = write_yaml("- a\n- b\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "keys", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("keys on array must error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("expected dq_core::Error");
    match domain {
        dq_core::Error::Path {
            kind: dq_core::PathErrorKind::TypeMismatch { expected, found },
            ..
        } => {
            assert_eq!(*expected, "object");
            assert_eq!(*found, "array");
        }
        other => panic!("expected TypeMismatch on array, got {other:?}"),
    }
}
