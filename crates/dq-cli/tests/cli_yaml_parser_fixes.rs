//! YAML edge inputs of the span parser after the saphyr-parser 0.3 bump:
//! a single-key file with a UTF-8 BOM, and a nested multi-line plain scalar
//! with no newline at EOF.
//!
//! The write commands route YAML through `parse_yaml_with_spans`, so these
//! tests drive `set` / `del` through the real binary and compare the bytes
//! it writes.

mod common;

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
        stdout_of(&["set"], &file, &["/key", "z"]),
        b"\xEF\xBB\xBFkey: z\n"
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
    stdout_of(&["-i", "set"], &file, &["/key", "z"]);
    assert_eq!(std::fs::read(&file).unwrap(), b"\xEF\xBB\xBFkey: z\n");
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

/// `bytes` read back through `dq -F json convert`, as JSON. The
/// lossy-conversion warning goes to stderr and leaves stdout alone.
fn read_back(bytes: &[u8]) -> serde_json::Value {
    let file = write_yaml(bytes);
    let out = stdout_of(&["-F", "json", "convert"], &file, &[]);
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

/// Run `dq --no-color <args…> <file> <rest…>`, return (exit code, stdout,
/// stderr).
fn outcome_of(args: &[&str], file: &TempPath, rest: &[&str]) -> (Option<i32>, String, String) {
    let out = dq()
        .arg("--no-color")
        .args(args)
        .arg(file.to_str().unwrap())
        .args(rest)
        .output()
        .expect("dq runs");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        common::without_source_block(&String::from_utf8(out.stderr).expect("UTF-8 stderr")),
    )
}

const COLLECTION_KEYS: &[u8] = b"# top\n[1, 2]: a  # one\n? {k: [on, 'a: b']}\n: b\nz: 1  # keep\n";

#[test]
fn a_collection_key_reads_as_its_flow_text() {
    // Before: `"Sequence [Number(1), Number(2)]"` and `"Mapping {…}"`, Rust
    // debug text.
    let file = write_yaml(COLLECTION_KEYS);
    assert_eq!(
        read_back(&stdout_of(&["-q", "-F", "json", "convert"], &file, &[])),
        serde_json::json!({"[1, 2]": "a", "{k: ['on', 'a: b']}": "b", "z": 1})
    );
    assert_eq!(
        String::from_utf8(stdout_of(&["keys"], &file, &[""])).unwrap(),
        "[1, 2]\n{k: ['on', 'a: b']}\nz\n"
    );
    assert_eq!(
        String::from_utf8(stdout_of(&["get"], &file, &["/[1, 2]"])).unwrap(),
        "a\n"
    );
    // The YAML writer quotes such a key: it is a string now, and reads back
    // as the same key.
    let yaml = stdout_of(&["-q", "-F", "yaml", "convert"], &file, &[]);
    assert!(
        yaml.starts_with(b"'[1, 2]': a\n"),
        "{}",
        String::from_utf8_lossy(&yaml)
    );
    let again = write_yaml(&yaml);
    assert_eq!(
        String::from_utf8(stdout_of(&["keys"], &again, &[""])).unwrap(),
        "[1, 2]\n{k: ['on', 'a: b']}\nz\n"
    );
}

#[test]
fn a_collection_key_repeats_by_its_text() {
    for (src, line) in [
        (&b"[1, 2]: a\n[1, 2]: b\n"[..], 2),
        (b"[1,2]: a\n[1, 2]: b\n", 2),
        (b"[1, 2]: a\nx: 1\n'[1, 2]': b\n", 3),
        (b"{a: 1, b: 2}: x\n{b: 2, a: 1}: y\n", 2),
    ] {
        let file = write_yaml(src);
        let (code, stdout, stderr) = outcome_of(&["get"], &file, &[""]);
        assert_eq!(code, Some(3), "{}", String::from_utf8_lossy(src));
        assert_eq!(stdout, "");
        assert!(
            stderr.contains(": duplicate entry in YAML map\n  --> ")
                && stderr.ends_with(&format!(":{line}:1\n")),
            "{stderr}"
        );
        // `validate` prints the same.
        let (code, _, validated) = outcome_of(&["validate"], &file, &[]);
        assert_eq!((code, validated), (Some(4), stderr));
    }
}

#[test]
fn textual_edits_next_to_a_collection_key_keep_the_file() {
    // Before: `path '/z' not found` (exit 2) for both.
    let file = write_yaml(COLLECTION_KEYS);
    assert_eq!(
        String::from_utf8(stdout_of(&["set"], &file, &["/z", "2"])).unwrap(),
        "# top\n[1, 2]: a  # one\n? {k: [on, 'a: b']}\n: b\nz: 2  # keep\n"
    );
    assert_eq!(
        String::from_utf8(stdout_of(&["del"], &file, &["/z"])).unwrap(),
        "# top\n[1, 2]: a  # one\n? {k: [on, 'a: b']}\n: b\n"
    );
    assert_eq!(
        String::from_utf8(stdout_of(&["set"], &file, &["/[1, 2]", "x"])).unwrap(),
        "# top\n[1, 2]: x  # one\n? {k: [on, 'a: b']}\n: b\nz: 1  # keep\n"
    );
}

#[test]
fn a_collection_key_quotes_strings_that_start_with_a_flow_indicator() {
    // Before: `[?a, :x]`, which reads back as `[{a: null}, …]` or fails.
    let file = write_yaml(b"? [\"?a\", \":x\", -x]\n: v\n");
    assert_eq!(
        String::from_utf8(stdout_of(&["keys"], &file, &[""])).unwrap(),
        "['?a', ':x', -x]\n"
    );
    let yaml = stdout_of(&["-q", "-F", "yaml", "convert"], &file, &[]);
    let again = write_yaml(&yaml);
    assert_eq!(
        String::from_utf8(stdout_of(&["keys"], &again, &[""])).unwrap(),
        "['?a', ':x', -x]\n"
    );
}

#[test]
fn del_of_a_collection_key_in_a_flow_mapping_takes_its_entry() {
    // Before: exit 7, "the edited document would not parse".
    for (src, pointer, left) in [
        (&b"{[1]: a, z: 1}\n"[..], "/[1]", "{z: 1}\n"),
        (b"{z: 1, [1]: a}\n", "/[1]", "{z: 1}\n"),
        (b"k: {[1]: a}\n", "/k/[1]", "k: {}\n"),
        (b"k: {? [1] : a}\n", "/k/[1]", "k: {}\n"),
        (b"{[1]: [2], z: 1}\n", "/[1]", "{z: 1}\n"),
    ] {
        let file = write_yaml(src);
        assert_eq!(
            String::from_utf8(stdout_of(&["del"], &file, &[pointer])).unwrap(),
            left,
            "{}",
            String::from_utf8_lossy(src)
        );
    }
}
