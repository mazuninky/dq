//! Unit tests for `dq validate` driven through `dq::run`.
//!
//! Spec contract: silent success (exit 0) for valid files; structured Parse
//! error rendered to stderr (exit 4 / `VALIDATE_FAIL`) for malformed files.
//! The handler wraps parse errors in `ValidateFail` to differentiate from
//! generic parse-time errors that map to exit 3.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use dq::ValidateFail;
use tempfile::NamedTempFile;

#[test]
fn validate_succeeds_silently_for_valid_yaml() {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(b"a: 1\nb: 2\n").unwrap();
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "validate", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("valid YAML must validate");
    assert!(out.is_empty(), "validate must not write to stdout");
    assert!(err.is_empty(), "valid input must not write to stderr");
}

#[test]
fn validate_malformed_json_returns_validate_fail_with_structured_stderr() {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(b"{ \"x\": 1, }").unwrap();
    let path = tmp.path().to_str().unwrap();
    // Use `-F json` to also pick the JSON reporter for stderr; .json
    // extension already selects the input parser.
    let cli = Cli::parse_from(["dq", "-F", "json", "validate", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e =
        dq::run(&cli, false, &mut out, &mut err).expect_err("malformed JSON must fail validate");
    // ValidateFail wrapper makes the exit-code mapper pick 4 (VALIDATE_FAIL)
    // instead of 3 (PARSE_ERROR). This is the spec's primary differentiator.
    assert!(
        e.downcast_ref::<ValidateFail>().is_some(),
        "validate must wrap parse errors in ValidateFail; got {e:?}",
    );
    let s = String::from_utf8(err).expect("utf-8 stderr");
    assert!(
        s.contains("\"kind\": \"parse\""),
        "stderr must contain a structured parse-error JSON object; got {s:?}",
    );
}

#[test]
fn validate_unsupported_extension_returns_unsupported_format() {
    // Validate also goes through pick_format → an unknown extension is an
    // UnsupportedFormat error (mapped to INVALID_INPUT / exit 6).
    let mut tmp = NamedTempFile::with_suffix(".unknown").expect("tempfile");
    tmp.write_all(b"hi\n").unwrap();
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "validate", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("unknown extension must error");
    let domain = e.downcast_ref::<dq_core::Error>().unwrap();
    assert_eq!(domain.kind_name(), "unsupported_format");
}

#[test]
fn validate_succeeds_silently_for_valid_json() {
    // Mirrors the YAML success case for the JSON parser path — exercises a
    // different `Format::parse` in the registry.
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(br#"{"x": 1, "y": [1, 2, 3]}"#).unwrap();
    let path = tmp.path().to_str().unwrap();
    let cli = Cli::parse_from(["dq", "validate", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("valid JSON must validate");
    assert!(out.is_empty());
    assert!(err.is_empty());
}
