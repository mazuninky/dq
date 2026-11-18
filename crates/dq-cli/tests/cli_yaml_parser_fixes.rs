//! YAML edge inputs of the span parser after the saphyr-parser 0.3 bump:
//! a single-key file with a UTF-8 BOM, and a nested multi-line plain scalar
//! with no newline at EOF.
//!
//! The write commands route YAML through `parse_yaml_with_spans`, so these
//! tests drive `set` / `del` through the real binary and compare the bytes
//! it writes.

use std::io::Write as _;

use assert_cmd::Command;
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

/// Write raw `bytes` to a `.yaml` temp file; the handle is closed before the
/// binary touches the path.
fn write_yaml(bytes: &[u8]) -> TempPath {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    tmp.write_all(bytes).expect("write tempfile");
    tmp.into_temp_path()
}

/// Run `dq <args…> <file> <rest…> --no-color`, assert exit 0, return stdout.
fn stdout_of(args: &[&str], file: &TempPath, rest: &[&str]) -> Vec<u8> {
    dq().args(args)
        .arg(file.to_str().unwrap())
        .args(rest)
        .arg("--no-color")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
}

const BOM_ONE_KEY: &[u8] = b"\xEF\xBB\xBFkey: x\n";

#[test]
fn set_on_single_key_file_with_bom_rewrites_the_value_and_keeps_the_bom() {
    // The 0.0.6 build failed with exit 2 "path '/key' not found".
    let file = write_yaml(BOM_ONE_KEY);
    assert_eq!(
        stdout_of(&["set"], &file, &["/key", "y"]),
        b"\xEF\xBB\xBFkey: y\n"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        BOM_ONE_KEY,
        "no -i, no write"
    );
}

#[test]
fn set_in_place_on_single_key_file_with_bom_writes_the_file() {
    let file = write_yaml(BOM_ONE_KEY);
    stdout_of(&["-i", "set"], &file, &["/key", "y"]);
    assert_eq!(std::fs::read(&file).unwrap(), b"\xEF\xBB\xBFkey: y\n");
}

/// A nested plain scalar spread over two lines, with no newline at EOF.
const MULTILINE_PLAIN: &[u8] = b"root:\n  inner:\n    x\n    y";

#[test]
fn multiline_plain_scalar_without_final_newline_reads_as_folded_string() {
    // The 0.0.6 build read this file the same way; pinned so the parser
    // bump cannot regress it to "unexpected end of plain scalar".
    let file = write_yaml(MULTILINE_PLAIN);
    assert_eq!(stdout_of(&["get"], &file, &["/root/inner"]), b"x y\n");
}

#[test]
fn set_replaces_multiline_plain_scalar_without_final_newline() {
    // The span covers both scalar lines: the replacement drops `y` and adds
    // no newline the source did not have.
    let file = write_yaml(MULTILINE_PLAIN);
    assert_eq!(
        stdout_of(&["set"], &file, &["/root/inner", "z"]),
        b"root:\n  inner:\n    z"
    );
}

/// `bytes` read back through `dq -F json convert`, as JSON. `-q` silences
/// the lossy-conversion warning, which the binary logs to stdout.
fn read_back(bytes: &[u8]) -> serde_json::Value {
    let file = write_yaml(bytes);
    let out = stdout_of(&["-q", "-F", "json", "convert"], &file, &[]);
    serde_json::from_slice(&out).expect("stdout is JSON")
}

#[test]
fn del_removes_key_whose_multiline_plain_value_starts_on_the_next_line() {
    // The 0.0.6 build, and this one before the fix, left `root:\n  inner:\n`:
    // the value was gone, the key stayed and read back as `null`. Now the
    // whole entry goes, and the emptied `root` is written as `{}`.
    let file = write_yaml(MULTILINE_PLAIN);
    let out = stdout_of(&["del"], &file, &["/root/inner"]);
    assert_eq!(read_back(&out), serde_json::json!({"root": {}}));
}

#[test]
fn set_new_key_after_next_line_value_lands_at_the_mapping_indent() {
    // The 0.0.6 build, and this one before the fix:
    // - `/root/other` gave `root:\n  inner:\n    x\n    y\n    other: z`,
    //   which does not parse ("mapping values are not allowed");
    // - `/zz` after a literal block (`a: |\n  x\n`) gave `a: |\n  x\n  zz: 1`,
    //   which parses, with `zz: 1` folded into the string `a`.
    let file = write_yaml(MULTILINE_PLAIN);
    let out = stdout_of(&["set"], &file, &["/root/other", "z"]);
    assert_eq!(
        read_back(&out),
        serde_json::json!({"root": {"inner": "x y", "other": "z"}})
    );
    let literal = write_yaml(b"a: |\n  x\n");
    let out = stdout_of(&["set"], &literal, &["/zz", "1"]);
    assert_eq!(read_back(&out), serde_json::json!({"a": "x\n", "zz": 1}));
}

#[test]
fn set_new_root_key_next_to_a_collection_sibling() {
    // The root's only child `root` is a collection; the insertion used to
    // anchor on a scalar sibling only and failed with exit 2.
    let file = write_yaml(MULTILINE_PLAIN);
    let out = stdout_of(&["set"], &file, &["/zz", "1"]);
    assert_eq!(
        read_back(&out),
        serde_json::json!({"root": {"inner": "x y"}, "zz": 1})
    );
}
