//! Integration-level CLI tests for `dq query` driven through `dq::run`.
//!
//! Each test parses a real CLI invocation via `Cli::try_parse_from(...)` (so
//! clap's `global = true` plumbing is exercised), then calls `dq::run` with
//! `Vec<u8>` writers and asserts on stdout / domain error variants / mapped
//! exit codes. The in-process style is dramatically faster than spawning the
//! binary and gives full debuggability when an assertion fails.
//!
//! Coverage tracks the `data-query-read` delta spec for `dq query`:
//! - happy paths (default console, JSON, multi-output, missing key)
//! - compile error → `PARSE_ERROR` (3)
//! - runtime error → `GENERIC` (1)
//! - write-flag rejection → `INVALID_INPUT` (6)
//! - multi-doc YAML via `--doc <idx>`
//! - SARIF reporter rejection (BannedReporter pattern)
//!
//! Stdin paths (`-` with / without `-F`) are intentionally driven through a
//! spawned binary in the small subprocess block at the bottom of the file:
//! `dq::run` reads stdin via `std::io::stdin().lock()`, which cannot be
//! redirected from inside the test process without process-wide tricks.

use std::io::Write as _;

use clap::Parser;
use dq::Cli;
use dq::exit_code;
use tempfile::NamedTempFile;

/// Write `content` to a YAML temp file and return a `TempPath`. We return
/// `TempPath` (not `NamedTempFile`) so the underlying handle is closed before
/// the binary touches the path. On Windows, holding the `NamedTempFile` open
/// blocks any in-place rewrite of the same path with "Access is denied. (os
/// error 5)". The `TempPath` still removes the file on drop, so cleanup is
/// preserved. Applied uniformly even on read-only sites for consistency.
fn write_yaml(content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

/// Write `content` to a JSON temp file. Tests that pass `-F json` must use
/// this — the `-F` flag overrides the *input* parser too (see the comment in
/// `unit_select.rs::select_returns_empty_array_for_no_match_under_json`),
/// so feeding a YAML file to a JSON-format invocation produces a parse error.
fn write_json(content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

#[test]
fn query_single_output_prints_scalar_to_console() {
    // Spec scenario "Single-output filter prints one value": stdout is the
    // raw value, exit 0. The default console reporter renders the
    // single-element JSON array `[3]` as `3\n`.
    let tmp = write_yaml("spec:\n  replicas: 3\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "query", ".spec.replicas", path, "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("query must succeed");
    assert_eq!(
        String::from_utf8(out).unwrap().trim_end(),
        "3",
        "single-output console form must be the raw scalar",
    );
}

#[test]
fn query_multi_output_with_json_format_returns_array() {
    // Spec scenario "Multi-output filter prints array": three matches → JSON
    // array of three image strings in document order (exit 0). Uses a JSON
    // fixture because `-F json` overrides the *input* parser too.
    let tmp = write_json(
        r#"{"spec": {"containers": [{"image": "img-a"}, {"image": "img-b"}, {"image": "img-c"}]}}"#,
    );
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "query",
        ".spec.containers[].image",
        path,
        "-F",
        "json",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("multi-output query must succeed");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("stdout must be valid JSON");
    assert_eq!(parsed, serde_json::json!(["img-a", "img-b", "img-c"]));
}

#[test]
fn query_missing_key_returns_array_with_null_under_json() {
    // Spec scenario "Empty result is not an error": jq's "missing key returns
    // null" semantics mean `.does.not.exist` produces a single `null` output,
    // wrapped as `[null]` by the reporter. NOT `[]` — that's a different
    // jq filter (e.g. `empty`). Uses a JSON fixture because `-F json` also
    // overrides the input parser.
    let tmp = write_json(r#"{"a": 1}"#);
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "query",
        ".does.not.exist",
        path,
        "-F",
        "json",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("missing key must NOT be an error");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("stdout must be valid JSON");
    assert_eq!(
        parsed,
        serde_json::json!([null]),
        "missing key should surface as `[null]`, got: {parsed}",
    );
}

#[test]
fn query_compile_error_maps_to_parse_error_exit_three() {
    // Spec scenario "Compile error maps to PARSE_ERROR": malformed jq
    // expression must produce a `dq_core::Error::Parse` so the exit-code
    // mapper picks 3 (same family as file-parse failures).
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "query", ".foo |=", path, "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e =
        dq::run(&cli, false, &mut out, &mut err).expect_err("malformed jq expression must error");
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("compile failures map to dq_core::Error::Parse");
    assert_eq!(
        domain.kind_name(),
        "parse",
        "expected `parse` kind, got {domain:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::PARSE_ERROR,
        "compile failures must map to exit 3 (PARSE_ERROR), got: {e:?}",
    );
}

#[test]
fn query_runtime_error_maps_to_generic_exit_one() {
    // Spec scenario "Runtime error maps to GENERIC": the file parsed fine and
    // the expression compiled fine — only the evaluation against this
    // specific data failed (string + number). That's GENERIC (exit 1)
    // territory, not PARSE_ERROR.
    //
    // Use a YAML file whose top-level value is the string "hello".
    let tmp = write_yaml("\"hello\"\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "query", ". + 1", path, "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("runtime type error in jq filter must surface");
    // The runtime error must NOT be a dq_core::Error — that would mis-map to
    // exit 3 instead of 1.
    assert!(
        e.downcast_ref::<dq_core::Error>().is_none(),
        "runtime errors must stay as plain anyhow so exit code is 1, got: {e:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::GENERIC,
        "runtime errors must map to exit 1 (GENERIC), got: {e:?}",
    );
}

#[test]
fn query_rejects_in_place_with_invalid_input_exit_six() {
    // Spec scenario "Read flag rejection": `-i` against a read subcommand
    // must surface an `InvalidInput` marker so the exit-code mapper picks 6.
    // The path doesn't have to exist — the gate short-circuits before any
    // I/O.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from(["dq", "-i", "query", ".a", path, "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("--in-place must be rejected on the read-only `query` verb");
    assert!(
        e.downcast_ref::<dq::error::InvalidInput>().is_some(),
        "rejection must carry the InvalidInput marker, got: {e:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::INVALID_INPUT,
        "write-flag rejection must map to exit 6 (INVALID_INPUT), got: {e:?}",
    );
    assert!(
        e.to_string().contains("--in-place"),
        "error should name the offending flag, got: {e}",
    );
}

#[test]
fn query_stdin_without_format_is_rejected_with_invalid_input() {
    // Spec scenario "Stdin read without format errors": passing `-` as the
    // file path with no `-F` override has no extension to dispatch from →
    // `InvalidInput` from `pick_format`, exit 6.
    //
    // This is in-process safe because the gate fires inside `pick_format`
    // BEFORE any stdin read attempt, so we never touch the parent's stdin.
    let cli = Cli::try_parse_from(["dq", "query", ".foo", "-", "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err).expect_err("stdin without -F must error");
    assert!(
        e.downcast_ref::<dq::error::InvalidInput>().is_some(),
        "rejection must carry InvalidInput marker, got: {e:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::INVALID_INPUT,
        "stdin-without-format must map to exit 6, got: {e:?}",
    );
    assert!(
        e.to_string().to_lowercase().contains("stdin"),
        "error message must name `stdin`, got: {e}",
    );
}

#[test]
fn query_multi_doc_yaml_with_doc_index_picks_second_document() {
    // Spec scenario "Multi-doc YAML uses --doc": with two documents, `--doc 1`
    // selects the second, exit 0. Use a multi-doc YAML stream — the closest
    // existing fixture is none (M1–M6 didn't ship one), so we build it
    // inline via `tempfile`.
    let tmp = write_yaml(concat!(
        "kind: Service\n",
        "name: svc\n",
        "---\n",
        "kind: Deployment\n",
        "name: web\n",
    ));
    let path = tmp.to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "--doc", "1", "query", ".kind", path, "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("multi-doc query must succeed");
    assert_eq!(
        String::from_utf8(out).unwrap().trim_end(),
        "Deployment",
        "second document's `.kind` must be `Deployment`",
    );
}

#[test]
fn query_with_sarif_format_is_rejected() {
    // Spec scenario "SARIF reporter rejected for query": query results are
    // arbitrary JSON, not SARIF-shaped; the SarifReporter raises
    // `InvalidInput` (no `diagnostics` array) → exit 6.
    let tmp = write_yaml("a: 1\n");
    let path = tmp.to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "-F", "sarif", "query", ".a", path, "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("`-F sarif` for query results must error");
    assert!(
        e.downcast_ref::<dq::error::InvalidInput>().is_some(),
        "rejection must carry InvalidInput marker (exit 6), got: {e:?}",
    );
    assert_eq!(
        exit_code::exit_code_for_error(&e),
        exit_code::INVALID_INPUT,
        "SARIF rejection must map to exit 6 (INVALID_INPUT), got: {e:?}",
    );
}

#[test]
fn query_update_assignment_does_not_modify_file_on_disk() {
    // Spec scenario "Update assignment is read-only at the query level":
    // `dq query` NEVER writes to disk regardless of the expression — even
    // an assignment-shaped filter (`|=`) just emits the transformed
    // document to stdout. Uses a JSON fixture because `-F json` also
    // overrides the input parser.
    let tmp = write_json(r#"{"spec": {"replicas": 3}}"#);
    let path = tmp.to_str().unwrap();
    let original = std::fs::read_to_string(&tmp).unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "query",
        ".spec.replicas |= . + 1",
        path,
        "-F",
        "json",
        "--no-color",
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("query with `|=` must succeed");

    // File on disk is untouched.
    let after = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(after, original, "query must NEVER touch the file on disk");

    // Stdout is the *transformed* document with `replicas: 4`.
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("stdout must be valid JSON");
    assert_eq!(
        parsed,
        serde_json::json!([{"spec": {"replicas": 4}}]),
        "stdout should carry the transformed document, got: {parsed}",
    );
}

#[test]
fn query_doc_all_returns_array_of_all_documents() {
    // Spec scenario "--doc all queries the entire stream": with three
    // documents, `--doc all` makes the entire stream visible to jq as an
    // array; `length` counts to 3.
    let tmp = write_yaml(concat!(
        "kind: A\n",
        "---\n",
        "kind: B\n",
        "---\n",
        "kind: C\n",
    ));
    let path = tmp.to_str().unwrap();
    let cli =
        Cli::try_parse_from(["dq", "--doc", "all", "query", "length", path, "--no-color"]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("--doc all length must succeed");
    assert_eq!(
        String::from_utf8(out).unwrap().trim_end(),
        "3",
        "length of three-doc stream must be 3",
    );
}

// ---------------------------------------------------------------------------
// Stdin-driven scenarios.
//
// `dq::run` reads stdin via `std::io::stdin().lock()`, so driving it from an
// in-process test is impractical (we'd be redirecting the test runner's stdin
// for the whole process, which breaks parallel tests). For the "stdin with
// `-F`" scenario specifically we spawn the real binary via `assert_cmd` —
// this is the minimum subprocess footprint the suite needs.
// ---------------------------------------------------------------------------

#[test]
fn query_stdin_with_explicit_format_round_trips_value() {
    // Spec scenario "Stdin read with explicit format": `cat config.yaml | dq
    // query '.foo' - -F yaml` reads stdin as YAML, evaluates the filter,
    // writes the result to stdout (exit 0). Driven through a real
    // subprocess because the in-process driver shares the test runner's
    // stdin handle.
    use assert_cmd::Command;
    use predicates::prelude::*;

    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    // Wipe inherited env so the developer's NO_COLOR / RUST_LOG do not leak.
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");

    cmd.args(["query", ".foo", "-", "-F", "yaml", "--no-color"])
        .write_stdin("foo: hello\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn query_with_format_override_does_not_misparse_input_yaml() {
    // Regression: `dq query '...' file.yaml -F json` previously caused the
    // YAML file to be parsed as JSON because the dispatcher forwarded -F to
    // the input parser. The handler now ignores its `_input_format`
    // parameter for file inputs and uses the extension. This test exercises
    // the full dq::run path so a future dispatcher-arm regression is caught.
    let tmp = write_yaml("spec:\n  containers:\n    - image: a\n    - image: b\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::try_parse_from([
        "dq",
        "-F",
        "json",
        "query",
        ".spec.containers[].image",
        path,
    ])
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("query should succeed");
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed, serde_json::json!(["a", "b"]));
}
