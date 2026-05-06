//! Unit tests for `dq exists` driven through `dq::run`.
//!
//! Spec contract: silent success (exit 0, empty stdout/stderr) and silent
//! failure (`SilentError`, exit 1, empty stdout/stderr) — main.rs maps the
//! marker error to exit 1 without rendering the chain.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use dq::SilentError;
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
fn exists_existing_pointer_succeeds_silently() {
    let tmp = write_yaml("server:\n  port: 8080\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "exists", path, "/server/port", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("must succeed when pointer exists");
    assert!(out.is_empty(), "exists must not write to stdout");
    assert!(err.is_empty(), "exists must not write to stderr");
}

#[test]
fn exists_missing_pointer_returns_silent_error() {
    // Missing pointer must surface as `SilentError` so main.rs suppresses any
    // diagnostic on stderr — the spec demands the empty stderr pipe-friendly
    // shell idiom (`dq exists … && echo ok`).
    let tmp = write_yaml("server:\n  port: 8080\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "exists", path, "/server/missing", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("missing pointer must error");
    assert!(
        e.downcast_ref::<SilentError>().is_some(),
        "exists/miss must yield SilentError so main.rs suppresses stderr; got {e:?}",
    );
    assert!(out.is_empty(), "exists must not write to stdout on miss");
    assert!(err.is_empty(), "stderr was not empty: {err:?}");
}

#[test]
fn exists_propagates_io_error_for_missing_file() {
    // I/O errors are NOT silenced — a missing file is a genuine error worth
    // reporting through the standard chain (and exit code 5 / IO_ERROR).
    let cli = Cli::parse_from(["dq", "exists", "/no/such/file.yaml", "/x", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("missing file must error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("missing file must surface as dq_core::Error::Io");
    assert_eq!(domain.kind_name(), "io");
    // Importantly: not a SilentError — we want stderr to render the I/O chain.
    assert!(
        e.downcast_ref::<SilentError>().is_none(),
        "I/O errors must NOT be silenced",
    );
}

#[test]
fn exists_invalid_pointer_returns_path_error_not_silent() {
    // A malformed pointer (no leading slash) is a different failure category
    // than "node missing" — it's a Path parse error, not SilentError.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "exists", path, "no-leading-slash", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("malformed pointer must error");
    // Pointer::parse error is a dq_core::Error::Path — not silenced. The CLI
    // surfaces the parse-time error verbatim so users see the cause.
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("expected dq_core::Error");
    assert_eq!(domain.kind_name(), "path");
}
