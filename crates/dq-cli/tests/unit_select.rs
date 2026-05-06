//! Unit tests for `dq select` driven through `dq::run`.
//!
//! Spec contract: returns a JSON array of matching values; an empty match list
//! is `[]` with exit 0 (NOT an error); malformed JSONPath is exit 1.

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
fn select_returns_array_for_single_match_default_console() {
    // Console output for an array prints one element per line.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "select", path, "$.spec.replicas", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("select must succeed");
    assert_eq!(String::from_utf8(out).unwrap(), "3\n");
}

#[test]
fn select_returns_empty_array_for_no_match_under_json() {
    // Spec scenario "No matches": stdout `[]`, exit 0.
    // `-F json` overrides input parser → use JSON file.
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    std::io::Write::write_all(&mut tmp, br#"{"spec": {"replicas": 3}}"#).unwrap();
    let tmp = tmp.into_temp_path();
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from([
        "dq",
        "-F",
        "json",
        "select",
        path,
        "$.does.not.exist",
        "--no-color",
    ]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err)
        .expect("empty select result must NOT be an error per spec");
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed, serde_json::json!([]));
}

#[test]
fn select_multi_match_returns_each_in_document_order() {
    // `-F json` overrides input parser → use JSON file.
    let json_input = serde_json::json!({
        "containers": [
            {"name": "a", "image": "img-a"},
            {"name": "b", "image": "img-b"},
            {"name": "c", "image": "img-c"},
        ]
    });
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    std::io::Write::write_all(&mut tmp, json_input.to_string().as_bytes()).unwrap();
    let tmp = tmp.into_temp_path();
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from([
        "dq",
        "-F",
        "json",
        "select",
        path,
        "$.containers[*].image",
        "--no-color",
    ]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("select must succeed");
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed, serde_json::json!(["img-a", "img-b", "img-c"]));
}

#[test]
fn select_malformed_expression_returns_generic_error() {
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "select", path, "this is not jsonpath", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("malformed jsonpath must error");
    // Per handler: jsonpath parse errors surface as plain `anyhow::Error` (not
    // a `dq_core::Error`), which the exit-code mapper routes to GENERIC.
    assert!(
        e.downcast_ref::<dq_core::Error>().is_none(),
        "jsonpath syntax errors should NOT be dq_core::Error variants",
    );
    assert!(
        e.to_string().contains("invalid JSONPath"),
        "error must explain JSONPath syntax failure: {e:?}",
    );
}
