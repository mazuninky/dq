//! Unit tests for `dq len` driven through `dq::run`.
//!
//! Spec contract: returns array length, string char count, or object key
//! count. Scalars (null/bool/int/float) trigger Path/TypeMismatch.

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
fn len_returns_array_length() {
    let tmp = write_yaml("- a\n- b\n- c\n- d\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "len", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("len must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "4\n");
}

#[test]
fn len_returns_object_key_count() {
    let tmp = write_yaml("a: 1\nb: 2\nc: 3\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "len", path, "", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("len must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "3\n");
}

#[test]
fn len_returns_string_char_count_not_byte_count() {
    // 'café' has 5 bytes but 4 chars. The handler counts chars (Unicode scalar
    // values), which is documented as the M1 behaviour even though the spec
    // mentions grapheme clusters as a future refinement.
    let tmp = write_yaml("name: café\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "len", path, "/name", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("len must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "4\n");
}

#[test]
fn len_against_scalar_bool_returns_type_mismatch_error() {
    // Per spec scenario "Length of scalar bool/number/null".
    let tmp = write_yaml("flag: true\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "len", path, "/flag", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("len on scalar must error");
    let domain = e.downcast_ref::<dq_core::Error>().unwrap();
    assert_eq!(domain.kind_name(), "path");
    match domain {
        dq_core::Error::Path { kind, .. } => {
            assert!(matches!(kind, dq_core::PathErrorKind::TypeMismatch { .. }))
        }
        _ => panic!("expected Path/TypeMismatch"),
    }
}
