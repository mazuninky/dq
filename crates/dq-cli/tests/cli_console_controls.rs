//! Console output escapes every control character a terminal acts on.
//!
//! C0 controls and DEL have been escaped as `\xNN` since the console
//! reporter was written; C1 controls (U+0080–U+009F) went through raw, and
//! U+009B (CSI) starts a terminal control sequence on its own, the way
//! `ESC [` does. They are now escaped the same way, `\xNN` with the code
//! point (`\x9b`, as dq's YAML writer spells it), in values, in keys and in
//! the snippet `validate` prints. The invisible direction controls of
//! Trojan Source and the line / paragraph separators went through raw as
//! well; they print as `\uNNNN`, as a YAML double-quoted string reads them.
//! Other non-ASCII text stays raw.

use std::path::Path;

use assert_cmd::Command;

fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

fn write(dir: &Path, name: &str, contents: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write fixture");
    path.to_str().expect("UTF-8 path").to_owned()
}

fn stdout_of(args: &[&str]) -> String {
    let out = dq().args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).expect("stdout is UTF-8")
}

/// A value holding C1 controls — CSI, NEL, the first and the last of the
/// block — prints them as `\xNN`; é, NBSP and an emoji stay raw.
#[test]
fn get_escapes_c1_controls_in_a_value() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "c1.json",
        "{\"v\": \"a\\u009b31mb\\u0085c\\u0080\\u009f é\\u00a0🦀\\u001b\"}",
    );
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, "/v"]),
        "a\\x9b31mb\\x85c\\x80\\x9f é\u{a0}🦀\\x1b\n"
    );
}

/// A key holding a C1 control is escaped too — both as a top-level row
/// and inside an inline object, where it is a double-quoted YAML scalar
/// the escape reads back from.
#[test]
fn get_escapes_c1_controls_in_keys() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "keys.json",
        "{\"k\\u009bx\": {\"i\\u0085j\": \"v\"}}",
    );
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, ""]),
        "k\\x9bx: {\"i\\x85j\": v}\n"
    );
}

/// `validate` prints the offending line as a snippet; a C1 control in it
/// is escaped like any other value.
#[test]
fn validate_escapes_c1_controls_in_the_snippet() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "bad.json", "{\"a\": \"x\u{9b}y\" oops}\n");
    let out = dq()
        .args(["--no-color", "validate", &file])
        .assert()
        .code(4);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("UTF-8 stderr");
    assert!(
        !stderr.contains('\u{9b}'),
        "raw U+009B reached the terminal: {stderr:?}"
    );
    assert!(
        stderr.contains("snippet: {\"a\": \"x\\x9by\" oops}"),
        "expected the escaped snippet, got: {stderr:?}"
    );
}

/// The invisible direction controls of Trojan Source — LRM, RLM, the
/// embeddings and overrides U+202A–U+202E, the isolates U+2066–U+2069 —
/// and the line and paragraph separators print as `\uNNNN`, lowercase hex,
/// as a YAML double-quoted string reads them: they reorder or break the
/// line a terminal shows without being visible themselves.
#[test]
fn get_escapes_direction_controls_and_separators_in_values_and_keys() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "bidi.json",
        "{\"k\\u202ex\": \"a\\u200eb\\u200fc\\u202ad\\u202be\\u202cf\\u202dg\\u2066h\\u2067i\\u2068j\\u2069k\\u2028l\\u2029m\"}",
    );
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, "/k\u{202e}x"]),
        "a\\u200eb\\u200fc\\u202ad\\u202be\\u202cf\\u202dg\\u2066h\\u2067i\\u2068j\\u2069k\\u2028l\\u2029m\n"
    );
    let whole = stdout_of(&["--no-color", "get", &file, ""]);
    assert!(
        whole.starts_with("k\\u202ex: a\\u200eb"),
        "a key is escaped too, got: {whole:?}"
    );
    // Neighbours stay raw: ZWJ (U+200D) in an emoji sequence, U+2065
    // (unassigned) and U+206A are not in the set.
    let file = write(
        dir.path(),
        "near.json",
        "{\"v\": \"\\u200d\\u2065\\u206a\"}",
    );
    assert_eq!(
        stdout_of(&["--no-color", "get", &file, "/v"]),
        "\u{200d}\u{2065}\u{206a}\n"
    );
}

/// The `validate` snippet escapes them as well.
#[test]
fn validate_escapes_direction_controls_in_the_snippet() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "bad.json", "{\"a\": \"x\u{202e}y\" oops}\n");
    let out = dq()
        .args(["--no-color", "validate", &file])
        .assert()
        .code(4);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).expect("UTF-8 stderr");
    assert!(
        !stderr.contains('\u{202e}'),
        "raw U+202E reached the terminal: {stderr:?}"
    );
    assert!(
        stderr.contains("snippet: {\"a\": \"x\\u202ey\" oops}"),
        "expected the escaped snippet, got: {stderr:?}"
    );
}

fn stderr_of(args: &[&str]) -> String {
    let out = dq().args(args).assert().failure();
    String::from_utf8(out.get_output().stderr.clone()).expect("stderr is UTF-8")
}

/// The error on stderr escapes what the user or the file put in it, as the
/// console reporter does: the `did_you_mean:` key from the file (a U+202E
/// override), the pointer echoed back (ESC, a C1 CSI, a tab). The message's
/// own line breaks stay.
#[test]
fn errors_escape_controls_in_did_you_mean_and_the_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "k.json", "{\"ab\u{202e}c\": 1}\n");
    let err = stderr_of(&["--no-color", "get", &file, "/abc"]);
    assert!(err.contains("did_you_mean: ab\\u202ec\n"), "{err:?}");
    assert!(!err.contains('\u{202e}'), "{err:?}");

    let err = stderr_of(&["--no-color", "get", &file, "/x\u{1b}[31m\u{9b}2J\ty"]);
    assert!(
        err.contains("pointer: /x\\x1b[31m\\x9b2J\\x09y\n"),
        "{err:?}"
    );
    assert!(
        !err.contains(['\u{1b}', '\u{9b}', '\t']),
        "no raw control on stderr: {err:?}"
    );
    assert!(err.lines().count() >= 2, "line breaks kept: {err:?}");
}

/// Any other error the anyhow chain renders — here the I/O error naming a
/// missing file — escapes the same way.
#[test]
fn errors_escape_controls_in_any_message() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no\u{1b}]0;t\u{7}.json");
    let err = stderr_of(&["--no-color", "get", missing.to_str().unwrap(), "/a"]);
    assert!(err.contains("no\\x1b]0;t\\x07.json"), "{err:?}");
    assert!(!err.contains(['\u{1b}', '\u{7}']), "{err:?}");
}

/// A line break in what the user or the file put in an error — a pointer,
/// a key, a file name — prints as `\x0a` (`\x0d` for a carriage return),
/// so it cannot start a line of its own that passes for one of dq's; the
/// message's own line breaks stay. Was: `dq get x.json $'/a\nY'` printed
/// `Y' not found …` as a line of its own.
#[test]
fn errors_escape_line_breaks_in_user_data() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "x.json", "{\"a\\nb\": 1, \"a\": {\"x\": 1}}\n");

    let err = stderr_of(&["--no-color", "get", &file, "/a\nY"]);
    assert_eq!(
        err,
        "path '/a\\x0aY' not found (matched up to '')\n  \
         did_you_mean: a\\x0ab, a\n  pointer: /a\\x0aY\n"
    );
    let err = stderr_of(&["--no-color", "get", &file, "/a/y\r\nZ"]);
    assert_eq!(
        err,
        "path '/a/y\\x0d\\x0aZ' not found (matched up to '/a')\n  \
         matched prefix: /a\n  pointer: /a/y\\x0d\\x0aZ\n"
    );
    let err = stderr_of(&["--no-color", "keys", &file, "/a\nb"]);
    assert_eq!(
        err,
        "cannot list the keys of '/a\\x0ab': it is int, not an object or an array\n"
    );
    let missing = format!("{}/n\new.json", dir.path().to_str().unwrap());
    let err = stderr_of(&["--no-color", "get", &missing, "/a"]);
    assert!(
        err.starts_with(&format!(
            "I/O error reading '{}': ",
            missing.replace('\n', "\\x0a")
        )),
        "{err:?}"
    );
    assert!(!err.contains("/n\new"), "{err:?}");
}
