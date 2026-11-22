//! YAML mapping keys that are sequences or mappings, through the real
//! binary: such a key is named by compact flow YAML of its value (`[1, 2]`,
//! `{k: v}`), never by Rust debug text (`Sequence [`, `Mapping {`,
//! `TaggedValue`) or a stand-in tag; two keys with the same text are a
//! parse error at the repeated one; textual `set` / `del` work on such an
//! entry and next to it, in every document of a stream.
//!
//! Each test spawns `dq` through `assert_cmd`, so the file can be run
//! against another build by pointing `CARGO_BIN_EXE_dq` at it.

use std::process::Output;

use assert_cmd::Command;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

/// `dq` with a clean environment.
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

fn dir_with(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src).expect("write fixture");
    }
    dir
}

fn read(dir: &TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name)).expect("read back")
}

fn run(dir: &TempDir, args: &[&str]) -> Output {
    dq().current_dir(dir.path())
        .arg("--no-color")
        .args(args)
        .output()
        .expect("dq runs")
}

/// Stdout of a successful `dq --no-color <args>` in `dir`.
fn stdout_of(dir: &TempDir, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "{args:?}: exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("UTF-8 stdout")
}

/// `(exit code, stdout, stderr)` of a failing `dq --no-color <args>`.
fn failure_of(dir: &TempDir, args: &[&str]) -> (Option<i32>, String, String) {
    let out = run(dir, args);
    assert!(!out.status.success(), "{args:?} succeeded");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        String::from_utf8(out.stderr).expect("UTF-8 stderr"),
    )
}

/// `text`, `<message> at line L column C`, in its two parts: the message on
/// stderr, and the lines `validate` reports for the place.
fn message_and_place(text: &str) -> (&str, String) {
    let (message, place) = text.split_once(" at line ").expect("a place");
    let (line, col) = place.split_once(" column ").expect("a column");
    (
        message,
        format!("\nline: {line}\ncol: {col}\nmessage: {message}\n"),
    )
}

/// The stderr of a command that cannot parse `file`, for the same text.
fn parse_error(file: &str, text: &str) -> String {
    let (message, place) = text.split_once(" at line ").expect("a place");
    let (line, col) = place.split_once(" column ").expect("a column");
    format!("parse error in {file}: {message}\n  --> {file}:{line}:{col}\n")
}

// ---- duplicates -------------------------------------------------------

/// Collection keys whose text is the same — tags dropped, map order and
/// quoting aside, `-0.0` and `0.0` equal — fail with exit 3 at the
/// repeated key, through the plain reader (`get`) and the span-aware one
/// (`set` / `del`), the file untouched. Keys with different text are two
/// keys.
#[test]
fn collection_keys_with_the_same_text_fail_at_the_repeated_key() {
    for (src, text) in [
        (
            "[!a 1]: x\n[!b 1]: y\n",
            "duplicate entry in YAML map at line 2 column 1",
        ),
        (
            "!foo a: x\na: y\n",
            "duplicate entry with key \"a\" at line 2 column 1",
        ),
        (
            "{1: a}: x\n{\"1\": a}: y\n",
            "duplicate entry in YAML map at line 2 column 1",
        ),
        (
            "{a: 1, b: 2}: x\n{b: 2, a: 1}: y\n",
            "duplicate entry in YAML map at line 2 column 1",
        ),
        (
            "[-0.0]: x\n[0.0]: y\n",
            "duplicate entry in YAML map at line 2 column 1",
        ),
        (
            "[1e400]: x\n[1e400]: y\n",
            "duplicate entry in YAML map at line 2 column 1",
        ),
        (
            "[1]: x\n\"[1]\": y\n",
            "duplicate entry in YAML map at line 2 column 1",
        ),
        (
            "x:\n  - [1, 2]: a\n    [1, 2]: b\n",
            "x[0]: duplicate entry in YAML map at line 3 column 5",
        ),
    ] {
        let dir = dir_with(&[("k.yaml", src)]);
        let (_, place) = message_and_place(text);
        let (code, _, stderr) = failure_of(&dir, &["validate", "k.yaml"]);
        assert_eq!(code, Some(4), "{src:?}");
        assert!(stderr.contains(&place), "{src:?}: {stderr}");
        for args in [
            &["get", "k.yaml", ""][..],
            &["set", "k.yaml", "/zz", "1"],
            &["del", "-i", "k.yaml", "/zz"],
        ] {
            let (code, stdout, stderr) = failure_of(&dir, args);
            assert_eq!(code, Some(3), "{src:?} {args:?}");
            assert_eq!(stdout, "", "{src:?} {args:?}");
            assert_eq!(stderr, parse_error("k.yaml", text), "{src:?} {args:?}");
        }
        assert_eq!(read(&dir, "k.yaml"), src, "file untouched");
    }
    let dir = dir_with(&[
        ("two.yaml", "[1e400]: x\n[2e400]: y\n"),
        ("spelled.yaml", "[1e400]: x\n[1.0e+400]: y\n"),
    ]);
    assert_eq!(
        stdout_of(&dir, &["keys", "two.yaml", ""]),
        "[1e400]\n[2e400]\n"
    );
    assert_eq!(
        stdout_of(&dir, &["keys", "spelled.yaml", ""]),
        "[1e400]\n[1.0e+400]\n"
    );
}

// ---- no debug text ----------------------------------------------------

/// A tagged big number inside a mapping inside a key: every command names
/// the key by flow text, never by debug text or a stand-in tag.
#[test]
fn a_nested_tagged_collection_key_never_shows_debug_text() {
    let dir = dir_with(&[("n.yaml", "{[!!float 1e400]: a}: b\nz: 1\n")]);
    let key = "{'[1e400]': a}";
    let commands: [&[&str]; 10] = [
        &["get", "n.yaml", ""],
        &["keys", "n.yaml", ""],
        &["paths", "n.yaml"],
        &["-F", "json", "convert", "n.yaml"],
        &["-F", "yaml", "convert", "n.yaml"],
        &["-F", "toml", "convert", "n.yaml"],
        &["-F", "toon", "convert", "n.yaml"],
        &["query", ".", "n.yaml"],
        &["query", "keys", "n.yaml"],
        &["lint", "n.yaml"],
    ];
    for args in commands {
        let out = run(&dir, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let both = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        for debug in [
            "!xxx",
            "Sequence [",
            "Mapping {",
            "TaggedValue",
            "String(",
            "Number(",
        ] {
            assert!(!both.contains(debug), "{args:?} shows {debug}: {both}");
        }
    }
    assert_eq!(
        stdout_of(&dir, &["keys", "n.yaml", ""]),
        format!("{key}\nz\n")
    );
    assert_eq!(
        stdout_of(&dir, &["-q", "-F", "json", "convert", "n.yaml"]),
        format!("{{\n  \"{key}\": \"b\",\n  \"z\": 1\n}}\n")
    );
    // The YAML writer writes the key as the string it is; it reads back as
    // the same key.
    let yaml = stdout_of(&dir, &["-q", "-F", "yaml", "convert", "n.yaml"]);
    assert_eq!(yaml, "'{''[1e400]'': a}': b\nz: 1\n");
    let back = dir_with(&[("b.yaml", &yaml)]);
    assert_eq!(
        stdout_of(&back, &["keys", "b.yaml", ""]),
        format!("{key}\nz\n")
    );
}

/// Strings in a key are quoted where flow YAML needs it: a YAML 1.1 word,
/// a flow indicator, a leading indicator, a line break (double quotes).
#[test]
fn key_text_quotes_the_strings_flow_yaml_needs_quoted() {
    let dir = dir_with(&[(
        "q.yaml",
        "? [\"a\\nb\", \"on\", \"a: b\", \"a,b\", \"[x]\", \"*a\", \"#c\", \"\", \"-1\", \"x y\", \"it's\", \" lead\"]\n: v\n",
    )]);
    let text = "[\"a\\nb\", 'on', 'a: b', 'a,b', '[x]', '*a', '#c', '', '-1', x y, it's, ' lead']";
    assert_eq!(
        stdout_of(&dir, &["keys", "q.yaml", ""]),
        format!("{text}\n")
    );
    // The text, read as YAML, is the key's value again.
    let again = dir_with(&[("t.yaml", text)]);
    assert_eq!(
        stdout_of(&again, &["-q", "-F", "json", "convert", "t.yaml"]),
        "[\n  \"a\\nb\",\n  \"on\",\n  \"a: b\",\n  \"a,b\",\n  \"[x]\",\n  \"*a\",\n  \"#c\",\n  \"\",\n  \"-1\",\n  \"x y\",\n  \"it's\",\n  \" lead\"\n]\n"
    );
    // And through the YAML writer and back.
    let yaml = stdout_of(&dir, &["-q", "-F", "yaml", "convert", "q.yaml"]);
    let back = dir_with(&[("b.yaml", &yaml)]);
    assert_eq!(
        stdout_of(&back, &["keys", "b.yaml", ""]),
        format!("{text}\n")
    );
}

// ---- edits ------------------------------------------------------------

const STREAM: &str = "---\n[1, 2]: a  # one\nz: 1\n---\n? {k: v}\n: b\ny: 2  # two\n";

#[test]
fn a_stream_with_collection_keys_reads_and_edits_per_document() {
    let dir = dir_with(&[("s.yaml", STREAM)]);
    assert_eq!(
        stdout_of(&dir, &["--doc", "0", "keys", "s.yaml", ""]),
        "[1, 2]\nz\n"
    );
    assert_eq!(
        stdout_of(&dir, &["--doc", "1", "keys", "s.yaml", ""]),
        "{k: v}\ny\n"
    );
    assert_eq!(
        stdout_of(&dir, &["--doc", "0", "get", "s.yaml", "/[1, 2]"]),
        "a\n"
    );
    assert_eq!(
        stdout_of(&dir, &["--doc", "1", "get", "s.yaml", "/{k: v}"]),
        "b\n"
    );
    assert_eq!(
        stdout_of(
            &dir,
            &["-q", "--doc", "all", "-F", "json", "convert", "s.yaml"]
        ),
        "[\n  {\n    \"[1, 2]\": \"a\",\n    \"z\": 1\n  },\n  {\n    \"{k: v}\": \"b\",\n    \"y\": 2\n  }\n]\n"
    );
    for (args, want) in [
        (
            &["set", "s.yaml", "/0/z", "5"][..],
            STREAM.replace("z: 1", "z: 5"),
        ),
        (
            &["set", "s.yaml", "/1/{k: v}", "c"],
            STREAM.replace(": b", ": c"),
        ),
        (
            &["--doc", "1", "del", "s.yaml", "/y"],
            STREAM.replace("y: 2  # two\n", ""),
        ),
        (
            &["del", "s.yaml", "/0/[1, 2]"],
            STREAM.replace("[1, 2]: a  # one\n", ""),
        ),
        (
            &["--doc", "1", "set", "s.yaml", "/w", "3"],
            format!("{STREAM}w: 3\n"),
        ),
    ] {
        assert_eq!(stdout_of(&dir, args), want, "{args:?}");
    }
}

/// A block-style sequence as a key: its siblings and its own entry are
/// edited in place, comments kept.
#[test]
fn a_block_sequence_key_and_its_siblings_are_edited_in_place() {
    let src = "a: 0\n? - 1\n  - 2\n: x  # k\nz: 1\n";
    let dir = dir_with(&[("b.yaml", src)]);
    assert_eq!(stdout_of(&dir, &["keys", "b.yaml", ""]), "a\n[1, 2]\nz\n");
    assert_eq!(stdout_of(&dir, &["get", "b.yaml", "/[1, 2]"]), "x\n");
    for (args, want) in [
        (
            &["set", "b.yaml", "/z", "2"][..],
            src.replace("z: 1", "z: 2"),
        ),
        (&["del", "b.yaml", "/z"], src.replace("z: 1\n", "")),
        (&["del", "b.yaml", "/a"], src.replace("a: 0\n", "")),
        (
            &["set", "b.yaml", "/[1, 2]", "y"],
            src.replace(": x", ": \"y\""),
        ),
        (&["del", "b.yaml", "/[1, 2]"], "a: 0\nz: 1\n".to_owned()),
    ] {
        assert_eq!(stdout_of(&dir, args), want, "{args:?}");
    }
}

/// `del` of a collection-key entry takes its trailing comment with it.
#[test]
fn deleting_a_collection_key_entry_takes_its_trailing_comment() {
    let dir = dir_with(&[
        ("d.yaml", "[1, 2]: a\nz: 1  # keep\n[3]: b  # gone\n"),
        ("m.yaml", "z: 1\n{k: v}: b  # gone\n"),
    ]);
    assert_eq!(
        stdout_of(&dir, &["del", "d.yaml", "/[3]"]),
        "[1, 2]: a\nz: 1  # keep\n"
    );
    assert_eq!(
        stdout_of(&dir, &["del", "d.yaml", "/[1, 2]"]),
        "z: 1  # keep\n[3]: b  # gone\n"
    );
    dq().current_dir(dir.path())
        .args(["--no-color", "del", "-i", "m.yaml", "/{k: v}"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(read(&dir, "m.yaml"), "z: 1\n");
}

/// An anchor defined inside a key and aliased later as a value: the value
/// is the anchored node; a sibling edits in place; the key holding the
/// anchor is not deleted from under its alias.
#[test]
fn an_anchor_inside_a_key_is_aliased_as_a_value() {
    let src = "? &k [1, {a: 2}]\n: x\nv: *k\nw: 1\n";
    let dir = dir_with(&[("a.yaml", src), ("s.yaml", "[1, &s 2]: x\nv: *s\n")]);
    assert_eq!(
        stdout_of(&dir, &["-q", "-F", "json", "convert", "a.yaml"]),
        "{\n  \"[1, {a: 2}]\": \"x\",\n  \"v\": [\n    1,\n    {\n      \"a\": 2\n    }\n  ],\n  \"w\": 1\n}\n"
    );
    assert_eq!(stdout_of(&dir, &["get", "a.yaml", "/v/1/a"]), "2\n");
    assert_eq!(
        stdout_of(&dir, &["set", "a.yaml", "/w", "2"]),
        src.replace("w: 1", "w: 2")
    );
    let (code, stdout, stderr) = failure_of(&dir, &["del", "a.yaml", "/[1, {a: 2}]"]);
    assert_eq!((code, stdout.as_str()), (Some(7), ""));
    assert!(
        stderr.starts_with(
            "cannot delete '/[1, {a: 2}]': it holds an anchor that the alias on line 3 refers to"
        ),
        "{stderr}"
    );
    assert_eq!(
        stdout_of(&dir, &["-q", "-F", "json", "convert", "s.yaml"]),
        "{\n  \"[1, 2]\": \"x\",\n  \"v\": 2\n}\n"
    );
}
