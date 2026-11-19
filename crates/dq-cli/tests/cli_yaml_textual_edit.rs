//! `dq set` / `dq del` / `dq lint` through the real binary, for YAML
//! textual-edit bugs that predate the saphyr-parser 0.3 bump.

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

/// Write `src` to a temp file named `*<suffix>`; the handle is closed
/// before the binary touches the path.
fn write_with_suffix(src: &str, suffix: &str) -> TempPath {
    let mut tmp = NamedTempFile::with_suffix(suffix).expect("tempfile");
    tmp.write_all(src.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

fn write_yaml(src: &str) -> TempPath {
    write_with_suffix(src, ".yaml")
}

/// Run `dq <args[0]> <file> <args[1..]> --no-color` on `src`, assert exit
/// 0, return stdout.
fn edit(src: &str, args: &[&str]) -> String {
    let file = write_yaml(src);
    let out = dq()
        .arg(args[0])
        .arg(file.to_str().expect("UTF-8 path"))
        .args(&args[1..])
        .arg("--no-color")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).expect("UTF-8 stdout")
}

#[test]
fn null_key_with_an_anchor_does_not_panic() {
    let file = write_yaml("f: {? &a}\n");
    dq().arg("lint")
        .arg(file.to_str().expect("UTF-8 path"))
        .arg("--no-color")
        .assert()
        .code(0);
    // Whether dq can add the key is another matter; it must not panic.
    let status = dq()
        .arg("set")
        .arg(file.to_str().expect("UTF-8 path"))
        .args(["/g", "1", "--no-color"])
        .output()
        .expect("dq runs")
        .status;
    assert_ne!(status.code(), Some(101), "dq panicked: {status:?}");
}

#[test]
fn set_next_to_an_empty_key_does_not_panic_in_json_or_toml() {
    for (src, suffix, want) in [
        ("{\"\": 1, \"b\": 2}", ".json", "{\"\": 1, \"b\": 3}"),
        ("\"\" = 1\nb = 2\n", ".toml", "\"\" = 1\nb = 3\n"),
    ] {
        let file = write_with_suffix(src, suffix);
        let out = dq()
            .arg("set")
            .arg(file.to_str().expect("UTF-8 path"))
            .args(["/b", "3", "--no-color"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(
            String::from_utf8(out).expect("UTF-8 stdout").trim_end(),
            want.trim_end()
        );
    }
}

#[test]
fn set_new_key_after_a_block_scalar_keeps_the_scalar() {
    assert_eq!(
        edit("a: |\n  x\n", &["set", "/zz", "1"]),
        "a: |\n  x\nzz: 1\n"
    );
    assert_eq!(
        edit("o:\n  a: |\n    x\n", &["set", "/o/zz", "1"]),
        "o:\n  a: |\n    x\n  zz: 1\n"
    );
}

#[test]
fn set_after_an_alias_edits_the_addressed_node() {
    let src = "a: &x 1\nb: *x\nc: 2\n";
    assert_eq!(edit(src, &["set", "/b", "9"]), "a: &x 1\nb: 9\nc: 2\n");
    assert_eq!(edit(src, &["set", "/c", "7"]), "a: &x 1\nb: *x\nc: 7\n");
}

#[test]
fn del_of_the_last_child_keeps_the_collection_type() {
    assert_eq!(
        edit("a:\n  x: 1\nb: 2\n", &["del", "/a/x"]),
        "a: {}\nb: 2\n"
    );
    assert_eq!(edit("a:\n  - 1\nb: 2\n", &["del", "/a/0"]), "a: []\nb: 2\n");
}

#[test]
fn del_of_a_key_holding_a_block_collection_removes_the_entry() {
    assert_eq!(
        edit("k: v\na:\n  x: 1\nb: 2\n", &["del", "/a"]),
        "k: v\nb: 2\n"
    );
    assert_eq!(edit("a:\n  - 1\nb: 2\n", &["del", "/a"]), "b: 2\n");
}

#[test]
fn del_of_an_anchor_an_alias_refers_to_is_refused_with_exit_7() {
    let src = "a: &m\n  x: 1\nb: *m\n";
    let file = write_yaml(src);
    let path = file.to_str().expect("UTF-8 path");
    dq().args(["del", path, "/a", "-i", "--no-color"])
        .assert()
        .code(7)
        .stdout("")
        .stderr(predicates::str::contains(
            "cannot delete '/a': it holds an anchor that the alias on line 3 refers to",
        ));
    assert_eq!(std::fs::read_to_string(path).expect("read back"), src);
}

#[test]
fn an_edit_that_does_not_read_back_is_refused_with_exit_7() {
    // TOML has no null; the renderer writes `""` for it: the read-back
    // check refuses it, prints nothing and leaves the file alone.
    let src = "a = 1\nb = 2\n";
    let file = write_with_suffix(src, ".toml");
    let path = file.to_str().expect("UTF-8 path");
    for extra in [&[][..], &["-i"][..]] {
        dq().args(["set", path, "/a", r#"{"x": null}"#, "--no-color"])
            .args(extra)
            .assert()
            .code(7)
            .stdout("")
            .stderr(predicates::str::contains(
                "cannot set '/a': dq cannot write this value here as a textual edit; `dq set --jq`",
            ));
        assert_eq!(std::fs::read_to_string(path).expect("read back"), src);
    }
}

#[test]
fn del_in_place_on_single_line_json_keeps_the_other_members() {
    let file = write_with_suffix(r#"{"a": 1, "b": 2}"#, ".json");
    let path = file.to_str().expect("UTF-8 path");
    dq().args(["del", "-i", path, "/a", "--no-color"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(path).expect("read back"),
        r#"{"b": 2}"#
    );
}

/// `set` writes a string holding LS, PS or NEL (U+2028, U+2029, U+0085) —
/// line breaks to a YAML 1.1 reader, content to a YAML 1.2 one —
/// double-quoted with `\L` / `\P` / `\N`, over a plain, a single-quoted,
/// a double-quoted or a block scalar, in a flow collection, and as a new
/// key. Before, every such `set` was refused: the value was written raw,
/// and the edited document did not read back as the value set.
#[test]
fn set_writes_line_and_paragraph_separators_escaped() {
    let src = "a: x\nb: 'y'\nc: \"z\"\nd: |\n  w\ne: [x, 'y']\nf: {g: h}\n";
    let value = "l\u{2028}m\u{2029}n\u{85}o";
    let quoted = "\"l\\Lm\\Pn\\No\"";
    for (pointer, expected) in [
        (
            "/a",
            format!("a: {quoted}\nb: 'y'\nc: \"z\"\nd: |\n  w\ne: [x, 'y']\nf: {{g: h}}\n"),
        ),
        (
            "/b",
            format!("a: x\nb: {quoted}\nc: \"z\"\nd: |\n  w\ne: [x, 'y']\nf: {{g: h}}\n"),
        ),
        (
            "/c",
            format!("a: x\nb: 'y'\nc: {quoted}\nd: |\n  w\ne: [x, 'y']\nf: {{g: h}}\n"),
        ),
        (
            "/d",
            format!("a: x\nb: 'y'\nc: \"z\"\nd: {quoted}\ne: [x, 'y']\nf: {{g: h}}\n"),
        ),
        (
            "/e/1",
            format!("a: x\nb: 'y'\nc: \"z\"\nd: |\n  w\ne: [x, {quoted}]\nf: {{g: h}}\n"),
        ),
        (
            "/f/g",
            format!("a: x\nb: 'y'\nc: \"z\"\nd: |\n  w\ne: [x, 'y']\nf: {{g: {quoted}}}\n"),
        ),
    ] {
        assert_eq!(edit(src, &["set", pointer, value]), expected, "{pointer}");
    }
    assert_eq!(
        edit("a: x\n", &["set", "/k\u{2028}", "1"]),
        "a: x\n\"k\\L\": 1\n"
    );
    // A multi-line value over a single-quoted scalar, which would fold.
    assert_eq!(edit("b: 'y'\n", &["set", "/b", "p\nq"]), "b: \"p\\nq\"\n");
}
