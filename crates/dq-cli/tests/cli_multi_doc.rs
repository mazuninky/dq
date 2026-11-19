//! Multi-document YAML streams: `lint` / `fix` treat every document as a
//! document of its own, and `--doc` is honoured — or refused with a usage
//! error — by every command.
//!
//! Driven through `dq::run` in-process, like `cli_lint_smoke.rs`.

use std::io::Write as _;

use clap::Parser;
use tempfile::{NamedTempFile, TempPath};

/// Two Pods; only the second one is named `b`.
const STREAM: &str = "\
apiVersion: v1
kind: Pod
metadata: {name: a}
---
apiVersion: v1
kind: Pod
metadata: {name: b}
";

/// Fires on every Pod not named `fixed`, at `/metadata/name`, and fixes it
/// with a `fix.ops` replace. `match.filter` only matches a single document
/// — an array of documents is not `kind: Pod`.
const RULE: &str = r#"
id: test.pod-name
description: every Pod is named 'fixed'
severity: error
match:
  format: yaml
  filter: '.kind == "Pod"'
check:
  jq: 'select(.metadata.name != "fixed") | {name: .metadata.name, pointer: "/metadata/name"}'
  message: "Pod '{{ .name }}' is not named 'fixed'"
loc:
  pointer: '.pointer'
fix:
  ops: |
    if .metadata.name != "fixed"
    then [{"op":"replace","path":"/metadata/name","value":"fixed"}]
    else [] end
"#;

fn temp(contents: &str, suffix: &str) -> TempPath {
    let mut file = NamedTempFile::with_suffix(suffix).expect("tempfile");
    file.write_all(contents.as_bytes()).expect("write");
    file.into_temp_path()
}

fn path(p: &TempPath) -> &str {
    p.to_str().expect("UTF-8 path")
}

/// Run `dq` in-process; `Ok(stdout)` or `Err((exit code, message))`.
fn run(argv: &[&str]) -> Result<String, (i32, String)> {
    let cli = dq::Cli::try_parse_from(argv).map_err(|e| (e.exit_code(), e.to_string()))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    match dq::run(&cli, false, &mut out, &mut err) {
        Ok(()) => Ok(String::from_utf8(out).expect("UTF-8 stdout")),
        Err(e) => Err((dq::exit_code::exit_code_for_error(&e), e.to_string())),
    }
}

/// Run `dq ... lint` with `-F json`; the exit code and the lines and
/// messages of the diagnostics.
fn lint(argv: &[&str]) -> (i32, Vec<(u64, String)>) {
    let cli = dq::Cli::try_parse_from(argv).expect("clap parse");
    let mut out = Vec::new();
    let code = match dq::run(&cli, false, &mut out, &mut Vec::new()) {
        Ok(()) => 0,
        Err(e) => dq::exit_code::exit_code_for_error(&e),
    };
    let body: serde_json::Value = serde_json::from_slice(&out).expect("JSON body");
    let found = body["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["line"].as_u64().expect("line"),
                d["message"].as_str().expect("message").to_owned(),
            )
        })
        .collect();
    (code, found)
}

#[test]
fn lint_evaluates_every_document_of_a_stream_on_its_own() {
    let rule = temp(RULE, ".yml");
    let doc = temp(STREAM, ".yaml");
    let (code, found) = lint(&[
        "dq",
        "-F",
        "json",
        "lint",
        "--rules",
        path(&rule),
        path(&doc),
    ]);
    assert_eq!(code, 4, "error-severity diagnostics fail the lint");
    assert_eq!(
        found,
        vec![
            (3, "Pod 'a' is not named 'fixed'".to_owned()),
            (7, "Pod 'b' is not named 'fixed'".to_owned()),
        ],
        "one diagnostic per document, at its line in the file"
    );
}

#[test]
fn lint_doc_limits_the_stream_to_one_document() {
    let rule = temp(RULE, ".yml");
    let doc = temp(STREAM, ".yaml");
    let (_, found) = lint(&[
        "dq",
        "-F",
        "json",
        "--doc",
        "1",
        "lint",
        "--rules",
        path(&rule),
        path(&doc),
    ]);
    let lines: Vec<u64> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, vec![7]);
}

#[test]
fn fix_fixes_every_document_and_keeps_the_stream() {
    let rule = temp(RULE, ".yml");
    let doc = temp(STREAM, ".yaml");
    run(&["dq", "-i", "fix", "--rules", path(&rule), path(&doc)]).expect("fix");
    let written = std::fs::read_to_string(&doc).unwrap();
    assert_eq!(
        written,
        STREAM
            .replace("{name: a}", "{name: fixed}")
            .replace("{name: b}", "{name: fixed}")
    );
}

#[test]
fn set_doc_edits_that_document_and_keeps_the_others() {
    let doc = temp(STREAM, ".yaml");
    let out = run(&["dq", "--doc", "1", "set", path(&doc), "/metadata/name", "z"]).expect("set");
    assert_eq!(out, STREAM.replace("{name: b}", "{name: z}"));
}

#[test]
fn set_jq_over_a_stream_writes_a_stream() {
    let doc = temp(STREAM, ".yaml");
    let out = run(&[
        "dq",
        "set",
        path(&doc),
        "--jq",
        ".[1].metadata.name = \"z\"",
    ])
    .expect("set");
    assert_eq!(out.matches("---").count(), 1, "still two documents:\n{out}");
    assert!(out.contains("name: z"), "{out}");
}

#[test]
fn get_doc_out_of_range_is_not_found() {
    let doc = temp(STREAM, ".yaml");
    let (code, message) = run(&["dq", "--doc", "2", "get", path(&doc), "/kind"]).unwrap_err();
    assert_eq!(code, 2);
    assert!(message.contains("--doc 2 is out of range"), "{message}");
    assert!(message.contains("holds 2 document(s)"), "{message}");
}

#[test]
fn doc_on_a_command_that_cannot_honour_it_is_a_usage_error() {
    let doc = temp(STREAM, ".yaml");
    let (code, message) = run(&["dq", "--doc", "1", "fmt", path(&doc)]).unwrap_err();
    assert_eq!(code, 2);
    assert!(message.contains("not supported by `dq fmt`"), "{message}");

    let (code, _) = run(&["dq", "--doc", "all", "del", path(&doc), "/kind"]).unwrap_err();
    assert_eq!(code, 2);
}

#[test]
fn doc_value_that_is_neither_index_nor_all_is_a_usage_error() {
    let (code, _) = run(&["dq", "--doc", "x", "get", "f.yaml", "/a"]).unwrap_err();
    assert_eq!(code, 2);
}
