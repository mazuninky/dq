//! CLI contract of `dq select` on top of `jsonpath-rust` 1.0 (RFC 9535).
//!
//! The bump from `jsonpath-rust` 0.7 changed what parses, what matches and
//! the order of descendant results. These tests spawn the real binary and
//! pin the behaviour the owner accepted from upstream: exit code, the
//! stdout/stderr split and the exact output.
//!
//! Every input file is JSON, so `-F json` (which also picks the input
//! parser) reads it unchanged.

use std::io::Write as _;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::{NamedTempFile, TempPath};

/// Build a `dq` command with a clean environment (same discipline as
/// `cli_smoke.rs::dq`).
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// Write `content` to a `.json` temp file; the handle is closed before the
/// binary reads the path.
fn write_json(content: &str) -> TempPath {
    let mut tmp = NamedTempFile::with_suffix(".json").expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

const ITEMS: &str =
    r#"{"items": [{"name": "x", "n": 1, "tags": []}, {"name": "y", "n": 2, "tags": ["a"]}]}"#;

/// `dq select <file> <expr> --no-color` with console output.
fn select(file: &TempPath, expr: &str) -> assert_cmd::assert::Assert {
    dq().args(["select", file.to_str().unwrap(), expr, "--no-color"])
        .assert()
}

/// `dq -F json select <file> <expr>`, stdout parsed as JSON.
fn select_json(file: &TempPath, expr: &str) -> serde_json::Value {
    let out = dq()
        .args(["-F", "json", "select", file.to_str().unwrap(), expr])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("stdout is JSON")
}

#[test]
fn select_rejects_non_rfc_9535_syntax_with_exit_1() {
    // `.length()` and `.['x']` were extensions of jsonpath-rust 0.7 (they
    // printed `2` and `[]`); `=~` was rejected before and still is; `$[` is
    // plain broken syntax. All of them are a JSONPath syntax error now.
    let file = write_json(ITEMS);
    for expr in [
        "$.items.length()",
        "$.['x']",
        r#"$[?(@.name =~ "x")]"#,
        "$[",
    ] {
        select(&file, expr)
            .code(1)
            .stdout("")
            .stderr(predicate::str::starts_with("invalid JSONPath expression:"));
    }
}

#[test]
fn select_evaluates_rfc_9535_filter_without_parentheses() {
    // 0.7: exit 1, "expected number, string_qt, unsigned, or filter".
    select(&write_json(ITEMS), "$.items[?@.n > 1].name")
        .success()
        .stdout("y\n");
}

#[test]
fn select_evaluates_rfc_9535_length_function_in_filter() {
    // 0.7: exit 1 (no `?@` filter, no function extensions).
    select(&write_json(ITEMS), "$.items[?length(@.tags) == 0].name")
        .success()
        .stdout("x\n");
}

#[test]
fn select_evaluates_negative_index_from_the_end() {
    // 0.7: exit 1, negative indices were not part of its grammar.
    select(&write_json(ITEMS), "$.items[-1].name")
        .success()
        .stdout("y\n");
}

#[test]
fn select_descendant_wildcard_lists_children_before_descending() {
    // RFC 9535 order as jsonpath-rust 1.0 implements it: a node's children
    // first, then each child's descendants. 0.7 went depth-first and gave
    // `[{b,c}, 1, [2, 3], 2, 3, 4]` — `4` came last.
    let file = write_json(r#"{"a": {"b": 1, "c": [2, 3]}, "d": 4}"#);
    assert_eq!(
        select_json(&file, "$..*"),
        serde_json::json!([{"b": 1, "c": [2, 3]}, 4, 1, [2, 3], 2, 3]),
    );
}

#[test]
fn select_shorthand_name_keeps_a_blank_as_part_of_the_key() {
    // Upstream behaviour of jsonpath-rust 1.0, accepted as is: a blank inside
    // a member-name shorthand is part of the name, so `$.s y` looks up the
    // key "s y" (0.7 rejected it with exit 1).
    select(&write_json(r#"{"s y": 7, "s": 1}"#), "$.s y")
        .success()
        .stdout("7\n");
    // The pair: without a "s y" key it matches nothing, it does not fall back
    // to the key "s".
    assert_eq!(
        select_json(&write_json(r#"{"s": 1}"#), "$.s y"),
        serde_json::json!([])
    );
}

#[test]
fn select_in_operator_silently_matches_nothing() {
    // Upstream behaviour of jsonpath-rust 1.0, accepted as is: `in` is not an
    // RFC 9535 operator, yet the expression parses and the filter matches
    // nothing, so the user gets `[]` and exit 0 instead of a syntax error.
    // 0.7 evaluated it and returned the `x` item.
    assert_eq!(
        select_json(&write_json(ITEMS), r#"$.items[?(@.name in ["x"])]"#),
        serde_json::json!([]),
    );
}
