//! Unit tests for `dq get` driven through `dq::run`.
//!
//! These tests bypass `assert_cmd`: they call `dq::run` directly with `Vec<u8>`
//! writers so a failure can be diagnosed without spawning a subprocess. Each
//! test follows the dependency-injection pattern from `references/cli-testing.md`:
//! build `Cli` via `Cli::parse_from(...)` (so clap's `global = true` plumbing is
//! exercised) and pass a temp file as the input.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use tempfile::NamedTempFile;

/// Write a YAML file to a temp location and return both the handle (so it's not
/// dropped) and the path string. The caller passes the path to `Cli::parse_from`.
fn write_yaml(content: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp
}

fn write_json(content: &str) -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp
}

#[test]
fn get_existing_pointer_emits_value_to_stdout() {
    let tmp = write_yaml("server:\n  port: 8080\n");
    let path = tmp.path().to_str().expect("utf-8 path");
    let cli = Cli::parse_from(["dq", "get", path, "/server/port", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("get must succeed");
    let stdout = String::from_utf8(out).expect("utf-8 stdout");
    assert_eq!(stdout, "8080\n", "console reporter prints scalar + newline");
    assert!(err.is_empty(), "expected empty stderr, got {err:?}");
}

#[test]
fn get_missing_pointer_returns_path_error_for_exit_code_two() {
    // Spec: `dq get config.yaml /missing/path` → exit code 2 (NOT_FOUND).
    // We verify that by asserting on the error variant: the exit-code mapper
    // maps `Error::Path` → `NOT_FOUND` (already covered in exit_code.rs unit
    // tests). Here we only need to confirm the handler produces a `Path` error.
    let tmp = write_yaml("server:\n  port: 8080\n");
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "get", path, "/server/missing", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    let e = result.expect_err("missing pointer must be an error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("missing pointer must surface as dq_core::Error");
    assert_eq!(
        domain.kind_name(),
        "path",
        "expected path-kind error, got {domain:?}"
    );
    assert!(out.is_empty(), "no stdout on miss, got {out:?}");
}

#[test]
fn get_against_unsupported_format_extension_returns_format_error() {
    // `script.sh` does not match any known parser. Without a `-F` override the
    // command should fail with `UnsupportedFormat`, mapping to exit code 6
    // (INVALID_INPUT) at the binary level.
    let mut tmp = NamedTempFile::with_suffix(".sh").expect("tempfile");
    tmp.write_all(b"echo hi\n").unwrap();
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "get", path, "/x", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("must reject .sh extension");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("expected dq_core::Error variant for unsupported extension");
    assert_eq!(domain.kind_name(), "unsupported_format");
}

#[test]
fn get_with_format_override_parses_unknown_extension() {
    // Override the parser via `-F json`: the file extension is `.txt` (no parser
    // would match by extension), but `-F json` forces JSON. This proves the
    // override path works through the full `dq::run` pipeline.
    let mut tmp = NamedTempFile::with_suffix(".txt").expect("tempfile");
    tmp.write_all(br#"{"a": 1}"#).unwrap();
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "get", path, "/a", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    // NOTE: `-F` is overloaded — for `get` it sets the *input* parser, but it
    // also picks the output reporter. Json output for the scalar `1` is `1`.
    dq::run(&cli, false, &mut out, &mut err).expect("override should succeed");
    let stdout = String::from_utf8(out).unwrap();
    // JsonReporter pretty-prints: a single integer is just `1`.
    assert!(stdout.trim() == "1", "unexpected stdout {stdout:?}");
}

#[test]
fn get_rejects_jsonpath_input_with_helpful_message() {
    // The handler refuses `$.a` and points at `dq select` instead. This is the
    // user-friendly affordance test.
    let tmp = write_json(r#"{"a": 1}"#);
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "get", path, "$.a", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("jsonpath must be rejected");
    assert!(
        e.to_string().contains("dq select"),
        "rejection message must point at `dq select`, got: {e:?}",
    );
}
