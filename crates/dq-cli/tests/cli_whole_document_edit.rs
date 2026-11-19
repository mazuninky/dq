//! `dq set` / `dq del` / `dq fix` through the real binary on what a file
//! holds as a whole: one document of a YAML stream (`/N`, `--doc N`), the
//! root of a YAML / JSON / TOML file (`''`), an empty or comment-only
//! file. Also the edits dq used to refuse inside such files — lone-`\r`
//! line breaks, a `%TAG` rebinding of `!!`, implicit-null entries and
//! empty tagged nodes of YAML flow collections — the YAML flow spelling
//! of a collection `set` writes, and the mappings `set` creates on the way
//! to a missing key.
//!
//! Every edit is checked twice: the bytes on disk, and the value dq reads
//! back from them. Each refusal leaves the file as it was.

use std::path::Path;

use assert_cmd::Command;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

/// What one `dq` run left behind.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    /// The file the run was given, read back after it.
    file: String,
}

/// `dq --no-color` in `dir`, with a clean environment and a proxy that
/// refuses every connection, so nothing a test runs reaches the network.
fn dq(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", dir)
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .current_dir(dir)
        .arg("--no-color");
    cmd
}

/// Write `src` to `name` in a fresh directory and run `dq <args>` there;
/// `args` name the file by its bare `name`.
fn run(name: &str, src: &str, args: &[&str]) -> Run {
    let dir = TempDir::new().expect("tempdir");
    run_in(&dir, name, src, args)
}

fn run_in(dir: &TempDir, name: &str, src: &str, args: &[&str]) -> Run {
    std::fs::write(dir.path().join(name), src).expect("write fixture");
    let out = dq(dir.path()).args(args).output().expect("spawn dq");
    Run {
        code: out.status.code().expect("dq exited, not killed"),
        stdout: String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(out.stderr).expect("UTF-8 stderr"),
        file: String::from_utf8(std::fs::read(dir.path().join(name)).expect("read back"))
            .expect("UTF-8 file"),
    }
}

/// `dq -i <args>` on `src`: exit 0, nothing on stdout or stderr; the new
/// bytes of the file.
fn edit(name: &str, src: &str, args: &[&str]) -> String {
    let argv: Vec<&str> = std::iter::once("-i").chain(args.iter().copied()).collect();
    let out = run(name, src, &argv);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", ""),
        "dq {argv:?} on {src:?}"
    );
    out.file
}

/// `dq <args>` on `src` exits `code` with an empty stdout and leaves the
/// file as it was; the stderr.
fn refused(name: &str, src: &str, args: &[&str], code: i32) -> String {
    let out = run(name, src, args);
    assert_eq!(
        (out.code, out.stdout.as_str()),
        (code, ""),
        "dq {args:?} on {src:?}: {}",
        out.stderr
    );
    assert_eq!(out.file, src, "dq {args:?}: the file is untouched");
    out.stderr
}

/// `bytes`, saved as `name`, as `dq -F json convert` reads them.
fn read(name: &str, bytes: &str) -> Value {
    let out = run(name, bytes, &["-q", "-F", "json", "convert", name]);
    assert_eq!(out.code, 0, "{bytes:?} reads back: {}", out.stderr);
    serde_json::from_str(&out.stdout).expect("stdout is JSON")
}

/// One `dq -i <args>` edit: the file it starts as, the bytes it must end
/// as, and the value dq must read back from them.
struct Case<'a> {
    name: &'a str,
    src: &'a str,
    args: &'a [&'a str],
    want: &'a str,
    reads: Value,
}

fn check(cases: &[Case<'_>]) {
    for case in cases {
        let got = edit(case.name, case.src, case.args);
        assert_eq!(got, case.want, "dq -i {:?} on {:?}", case.args, case.src);
        assert_eq!(
            read(case.name, &got),
            case.reads,
            "{got:?} reads back as the edit asked"
        );
    }
}

// ---- one document of a stream -----------------------------------------

/// `del /N` removes document N with its `---` line and the comment lines
/// just above that line, which lead into it; its own trailing comment
/// lines lead into the next document and stay. Every other byte stays,
/// line breaks (`\r\n`, lone `\r`) and byte order mark included.
#[test]
fn del_of_a_stream_document_removes_it_with_its_separator() {
    check(&[
        Case {
            name: "s.yaml",
            src: "# head\na: 1\n# about b\n---\nb: 2 # mine\n# trailing b\n---\nc: 3\n",
            args: &["del", "s.yaml", "/1"],
            want: "# head\na: 1\n# trailing b\n---\nc: 3\n",
            reads: json!([{"a": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\r\n---\r\nb: 2\r\n---\r\nc: 3\r\n",
            args: &["del", "s.yaml", "/1"],
            want: "a: 1\r\n---\r\nc: 3\r\n",
            reads: json!([{"a": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\r---\rb: 2\r---\rc: 3\r",
            args: &["del", "s.yaml", "/1"],
            want: "a: 1\r---\rc: 3\r",
            reads: json!([{"a": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "\u{feff}a: 1\n---\nb: 2\n---\nc: 3\n",
            args: &["del", "s.yaml", "/0"],
            want: "\u{feff}---\nb: 2\n---\nc: 3\n",
            reads: json!([{"b": 2}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "\u{feff}  a: 1\n---\n  b: 2\n",
            args: &["del", "s.yaml", "/0"],
            want: "\u{feff}---\n  b: 2\n",
            reads: json!({"b": 2}),
        },
        // A `...` line that ends the document before stays to end it.
        Case {
            name: "s.yaml",
            src: "a: 1\n...\n---\nb: 2\n...\n---\nc: 3\n...\n",
            args: &["del", "s.yaml", "/1"],
            want: "a: 1\n...\n---\nc: 3\n...\n",
            reads: json!([{"a": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n...\n---\nb: 2\n...\n",
            args: &["del", "s.yaml", "/1"],
            want: "a: 1\n...\n",
            reads: json!({"a": 1}),
        },
        // An anchor no other document can name goes with its document.
        Case {
            name: "s.yaml",
            src: "a: &x 1\n---\nb: 2\n",
            args: &["del", "s.yaml", "/0"],
            want: "---\nb: 2\n",
            reads: json!({"b": 2}),
        },
        // Down to one document: the file reads as that document, not as
        // a stream of one.
        Case {
            name: "s.yaml",
            src: "a: 1\n---\nb: 2\n",
            args: &["del", "s.yaml", "/0"],
            want: "---\nb: 2\n",
            reads: json!({"b": 2}),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n---\nb: 2\n",
            args: &["del", "s.yaml", "/1"],
            want: "a: 1\n",
            reads: json!({"a": 1}),
        },
        // `--doc N` with the root: the same as `/N`.
        Case {
            name: "s.yaml",
            src: "a: 1\n---\nb: 2\n---\nc: 3\n",
            args: &["--doc", "1", "del", "s.yaml", ""],
            want: "a: 1\n---\nc: 3\n",
            reads: json!([{"a": 1}, {"c": 3}]),
        },
    ]);
}

/// Without `-i` the stream without document N goes to stdout, with
/// `--diff` the change does; the file stays as it was either way.
#[test]
fn del_of_a_stream_document_prints_or_diffs_and_leaves_the_file() {
    let src = "a: 1\n---\nb: 2\n---\nc: 3\n";
    for (args, stdout) in [
        (&["del", "s.yaml", "/1"][..], "a: 1\n---\nc: 3\n"),
        (
            &["--doc", "1", "del", "s.yaml", ""][..],
            "a: 1\n---\nc: 3\n",
        ),
        (
            &["del", "--diff", "s.yaml", "/1"][..],
            "--- a/s.yaml\n+++ b/s.yaml\n@@ -1,5 +1,3 @@\n a: 1\n ---\n-b: 2\n----\n c: 3\n",
        ),
    ] {
        let out = run("s.yaml", src, args);
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (0, stdout, ""),
            "dq {args:?}"
        );
        assert_eq!(out.file, src, "dq {args:?}: the file is untouched");
    }
}

/// A document past the end of the stream is not found (exit 2), for
/// `del` and `set` alike.
#[test]
fn a_stream_document_past_the_end_is_not_found() {
    for args in [
        &["-i", "del", "s.yaml", "/2"][..],
        &["-i", "set", "s.yaml", "/2", "{\"x\": 1}"][..],
    ] {
        let err = refused("s.yaml", "a: 1\n---\nb: 2\n", args, 2);
        assert!(err.starts_with("path '/2' not found"), "dq {args:?}: {err}");
    }
}

/// `set /N` with a collection (or over a null or empty document) writes
/// document N anew through the YAML writer; the `---` line with its
/// comment, the comment lines after the content, a `...` line and every
/// other document stay byte for byte, and the new lines break as the file
/// does. A scalar over a scalar document stays a textual edit.
#[test]
fn set_of_a_stream_document_rewrites_it_and_keeps_every_other_byte() {
    check(&[
        Case {
            name: "s.yaml",
            src: "a: 1\r\n---\r\nb: 2 # c\r\n---\r\nc: 3\r\n",
            args: &["set", "s.yaml", "/1", "{\"x\": [1, 2]}"],
            want: "a: 1\r\n---\r\nx:\r\n- 1\r\n- 2\r\n---\r\nc: 3\r\n",
            reads: json!([{"a": 1}, {"x": [1, 2]}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\r---\rb: 2\r",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\r---\rx: 1\r",
            reads: json!([{"a": 1}, {"x": 1}]),
        },
        Case {
            name: "s.yaml",
            src: "\u{feff}a: 1\n---\nb: 2\n",
            args: &["set", "s.yaml", "/0", "{\"x\": 1}"],
            want: "\u{feff}x: 1\n---\nb: 2\n",
            reads: json!([{"x": 1}, {"b": 2}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n...\n---\nb: 2\n...\n---\nc: 3\n",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\n...\n---\nx: 1\n...\n---\nc: 3\n",
            reads: json!([{"a": 1}, {"x": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n--- # c\nb: 2\n",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\n--- # c\nx: 1\n",
            reads: json!([{"a": 1}, {"x": 1}]),
        },
        // The comment lines after the content stay; so do the ones that
        // open the file, before the first document.
        Case {
            name: "s.yaml",
            src: "# lead\nb: 2\n# tail\n---\nc: 3\n",
            args: &["set", "s.yaml", "/0", "{\"x\": 1}"],
            want: "# lead\nx: 1\n# tail\n---\nc: 3\n",
            reads: json!([{"x": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n# about\n---\nb: 2\n# tail\n---\nc: 3\n",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\n# about\n---\nx: 1\n# tail\n---\nc: 3\n",
            reads: json!([{"a": 1}, {"x": 1}, {"c": 3}]),
        },
        // A null document, and an empty one.
        Case {
            name: "s.yaml",
            src: "a: 1\n---\n~\n---\nc: 3\n",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\n---\nx: 1\n---\nc: 3\n",
            reads: json!([{"a": 1}, {"x": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n---\n---\nc: 3\n",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\n---\nx: 1\n---\nc: 3\n",
            reads: json!([{"a": 1}, {"x": 1}, {"c": 3}]),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n---\nb: 2\n",
            args: &["set", "s.yaml", "/1", "[]"],
            want: "a: 1\n---\n[]\n",
            reads: json!([{"a": 1}, []]),
        },
        // `--doc N` with the root: the same as `/N`.
        Case {
            name: "s.yaml",
            src: "# h\na: 1\n---\nb: 2\n",
            args: &["--doc", "1", "set", "s.yaml", "", "{\"x\": 1}"],
            want: "# h\na: 1\n---\nx: 1\n",
            reads: json!([{"a": 1}, {"x": 1}]),
        },
        // A scalar over a scalar document: its comment and `...` stay.
        Case {
            name: "s.yaml",
            src: "a: 1\n---\n5 # c\n...\n",
            args: &["set", "s.yaml", "/1", "7"],
            want: "a: 1\n---\n7 # c\n...\n",
            reads: json!([{"a": 1}, 7]),
        },
    ]);
}

/// `set /N --diff` shows the rewritten document and leaves the file.
#[test]
fn set_of_a_stream_document_diffs_and_leaves_the_file() {
    let src = "a: 1\n---\nb: 2\n";
    let out = run(
        "s.yaml",
        src,
        &["set", "--diff", "s.yaml", "/1", "{\"x\": 1}"],
    );
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            0,
            "--- a/s.yaml\n+++ b/s.yaml\n@@ -1,3 +1,3 @@\n a: 1\n ---\n-b: 2\n+x: 1\n",
            ""
        )
    );
    assert_eq!(out.file, src);
}

#[test]
fn set_of_a_whole_document_keeps_the_comment_lines_after_its_marker() {
    check(&[
        Case {
            name: "f.yaml",
            src: "---\n# lead\nb: 2\n",
            args: &["set", "f.yaml", "", "{\"x\": 1}"],
            want: "---\n# lead\nx: 1\n",
            reads: json!({"x": 1}),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n---\n# lead\nb: 2\n",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\n---\n# lead\nx: 1\n",
            reads: json!([{"a": 1}, {"x": 1}]),
        },
    ]);
}

/// Blank lines, indented comments and CRLF line breaks between the marker
/// and the content stay too, under a kept `!!map` and after `--- # m`.
#[test]
fn set_of_a_whole_document_keeps_its_lead_in_lines_of_every_shape() {
    check(&[
        Case {
            name: "s.yaml",
            src: "a: 1\n---\n# lead\n\n  # two\nb: 2\n# tail\n",
            args: &["set", "s.yaml", "/1", "{\"x\": 1}"],
            want: "a: 1\n---\n# lead\n\n  # two\nx: 1\n# tail\n",
            reads: json!([{"a": 1}, {"x": 1}]),
        },
        Case {
            name: "f.yaml",
            src: "--- !!map\n# lead\nb: 2\n",
            args: &["set", "f.yaml", "", "{\"x\": 1}"],
            want: "--- !!map\n# lead\nx: 1\n",
            reads: json!({"x": 1}),
        },
        Case {
            name: "f.yaml",
            src: "--- # m\n# lead\n- 1\n",
            args: &["set", "f.yaml", "", "[2]"],
            want: "--- # m\n# lead\n- 2\n",
            reads: json!([2]),
        },
        Case {
            name: "f.yaml",
            src: "---\r\n# lead\r\n\r\nb: 2\r\n",
            args: &["set", "f.yaml", "", "{\"x\": 1}"],
            want: "---\r\n# lead\r\n\r\nx: 1\r\n",
            reads: json!({"x": 1}),
        },
    ]);
}

// ---- the root of a file -----------------------------------------------

/// `set ''` replaces the whole document. YAML keeps the comment and blank
/// lines it starts with, directives with their `---` line, a `!!map` on
/// that line while the value is still a mapping, and the comment lines
/// after the content. JSON keeps the blanks around the value; TOML the
/// comment and blank lines that open the file.
#[test]
fn set_of_the_root_replaces_the_document_and_keeps_its_head() {
    check(&[
        Case {
            name: "f.yaml",
            src: "# head\n\na: 1 # c\nb: [1]\n# tail\n",
            args: &["set", "f.yaml", "", "{\"x\": {\"y\": 1}}"],
            want: "# head\n\nx:\n  'y': 1\n# tail\n",
            reads: json!({"x": {"y": 1}}),
        },
        Case {
            name: "f.yaml",
            src: "%YAML 1.2\n%TAG !e! tag:e.com,2000:\n---\na: 1\n",
            args: &["set", "f.yaml", "", "{\"x\": 1}"],
            want: "%YAML 1.2\n%TAG !e! tag:e.com,2000:\n---\nx: 1\n",
            reads: json!({"x": 1}),
        },
        Case {
            name: "f.yaml",
            src: "--- !!map\na: 1\n",
            args: &["set", "f.yaml", "", "{\"x\": 1}"],
            want: "--- !!map\nx: 1\n",
            reads: json!({"x": 1}),
        },
        Case {
            name: "f.yaml",
            src: "--- !!map\na: 1\n",
            args: &["set", "f.yaml", "", "[1]"],
            want: "---\n- 1\n",
            reads: json!([1]),
        },
        Case {
            name: "f.yaml",
            src: "# one\n# two\n",
            args: &["set", "f.yaml", "", "{\"x\": 1}"],
            want: "# one\n# two\nx: 1\n",
            reads: json!({"x": 1}),
        },
        Case {
            name: "f.yaml",
            src: "# h\r\na: 1\r\n",
            args: &["set", "f.yaml", "", "{\"x\": [1]}"],
            want: "# h\r\nx:\r\n- 1\r\n",
            reads: json!({"x": [1]}),
        },
        Case {
            name: "f.json",
            src: "  {\"a\": 1}\n",
            args: &["set", "f.json", "", "{\"x\": [1]}"],
            want: "  {\"x\":[1]}\n",
            reads: json!({"x": [1]}),
        },
        Case {
            name: "f.json",
            src: "{\"a\": 1}\n",
            args: &["set", "f.json", "", "5"],
            want: "5\n",
            reads: json!(5),
        },
        Case {
            name: "f.toml",
            src: "# head\n# two\n\na = 1\n[t]\nx = 1\n",
            args: &["set", "f.toml", "", "{\"x\": {\"y\": 1}}"],
            want: "# head\n# two\n\n[x]\ny = 1\n",
            reads: json!({"x": {"y": 1}}),
        },
    ]);
}

#[test]
fn set_of_the_root_diffs_and_leaves_the_file() {
    let src = "# head\na = 1\n";
    let out = run(
        "f.toml",
        src,
        &["set", "--diff", "f.toml", "", "{\"b\": 2}"],
    );
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            0,
            "--- a/f.toml\n+++ b/f.toml\n@@ -1,2 +1,2 @@\n # head\n-a = 1\n+b = 2\n",
            ""
        )
    );
    assert_eq!(out.file, src);
}

/// A TOML document is a table. A root that is no table is refused by the
/// textual `set ''` (exit 7) and by every TOML writer — `set --jq`,
/// `--sort-keys`, `convert` — (exit 1), named by its kind; the file stays.
#[test]
fn a_toml_root_that_is_no_table_is_refused_by_every_writer() {
    let table = "a TOML document is a table, so its root is one, not";
    for (value, kind) in [
        ("null", "null"),
        ("true", "a boolean"),
        ("5", "an integer"),
        ("1.5", "a float"),
        ("s", "a string"),
        ("[1]", "an array"),
    ] {
        assert_eq!(
            refused("f.toml", "a = 1\n", &["-i", "set", "f.toml", "", value], 7),
            format!("cannot set '': {table} {kind}\n"),
            "set '' {value}"
        );
        let jq = if value == "s" { "\"s\"" } else { value };
        assert_eq!(
            refused("f.toml", "a = 1\n", &["-i", "set", "f.toml", "--jq", jq], 1),
            format!("toml format error: {table} {kind}\n"),
            "set --jq {jq}"
        );
    }
    assert_eq!(
        refused(
            "f.toml",
            "a = 1\n",
            &["--sort-keys", "set", "f.toml", "--jq", "5"],
            1
        ),
        format!("toml format error: {table} an integer\n")
    );
    for (src, kind) in [("5\n", "an integer"), ("[1]\n", "an array")] {
        assert_eq!(
            refused("f.json", src, &["-F", "toml", "convert", "f.json"], 1),
            format!("toml format error: {table} {kind}\n"),
            "convert {src:?}"
        );
    }
}

/// `del ''` cannot delete the root of any format, nor of a stream; `set ''`
/// cannot set the root of a stream. Both are exit 2 and say what to
/// address instead; the file stays.
#[test]
fn the_root_cannot_be_deleted_nor_a_stream_root_set() {
    let del = "cannot delete the root '': it is the whole document; delete a value inside it, \
               or one document of a stream as /N\n  pointer: \n";
    for (name, src) in [
        ("f.yaml", "a: 1\n"),
        ("f.json", "{\"a\": 1}\n"),
        ("f.toml", "a = 1\n"),
        ("s.yaml", "a: 1\n---\nb: 2\n"),
    ] {
        assert_eq!(
            refused(name, src, &["-i", "del", name, ""], 2),
            del,
            "{name}"
        );
    }
    assert_eq!(
        refused(
            "s.yaml",
            "a: 1\n---\nb: 2\n",
            &["-i", "set", "s.yaml", "", "{\"x\": 1}"],
            2
        ),
        "cannot set the root '' of a stream of documents: set one document as /N, or pass \
         --doc N\n  pointer: \n"
    );
}

// ---- fix.ops on a whole document --------------------------------------

/// A rule whose `fix.ops` is `ops` (a jq program), firing on `bad: true`.
fn whole_rule(ops: &str) -> String {
    format!(
        r#"
id: test.whole
description: whole-document fix
severity: warn
match: {{format: yaml}}
check:
  jq: 'select(.bad == true)'
  message: bad
fix:
  ops: '{ops}'
"#
    )
}

/// `dq -i fix --rules rule.yml s.yaml` with `rule` on `src`.
fn fix(rule: &str, src: &str) -> Run {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("rule.yml"), rule).expect("write rule");
    run_in(
        &dir,
        "s.yaml",
        src,
        &["-i", "fix", "--rules", "rule.yml", "s.yaml"],
    )
}

/// `remove ""` takes each bad document out of its stream, consecutive
/// ones too, with the comment lines leading into it; the ones after move
/// up and are still checked.
#[test]
fn fix_ops_remove_takes_every_bad_document_out_of_the_stream() {
    let rule = whole_rule(r#"[{"op":"remove","path":""}]"#);
    for (src, want) in [
        (
            "ok: 1\n---\nbad: true\n---\nbad: true\n---\nok: 2\n",
            "ok: 1\n---\nok: 2\n",
        ),
        ("bad: true\n---\nok: 2\n", "---\nok: 2\n"),
        (
            "# h\r\nok: 1\r\n# about bad\r\n---\r\nbad: true\r\n# tail\r\n---\r\nok: 2\r\n",
            "# h\r\nok: 1\r\n# tail\r\n---\r\nok: 2\r\n",
        ),
    ] {
        let out = fix(&rule, src);
        assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{src:?}");
        assert_eq!(out.file, want, "{src:?}");
    }
}

/// `remove ""` cannot take the last document a file has: a stream whose
/// every document is bad is exit 3 once the first goes, and nothing is
/// written.
#[test]
fn fix_ops_remove_cannot_take_every_document_of_a_stream() {
    let src = "bad: true\n---\nbad: true\n";
    let out = fix(&whole_rule(r#"[{"op":"remove","path":""}]"#), src);
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert_eq!(
        out.stderr,
        "rule test.whole fix failed: fix.ops would remove the document as a whole \
         (remove \"\"): it is the whole file; only a document of a stream can be removed\n"
    );
    assert_eq!(out.file, src, "untouched");
}

/// `remove ""` followed by more ops would edit the document after the
/// removed one: exit 3, naming the rule and the document; nothing is
/// written.
#[test]
fn fix_ops_remove_of_a_whole_document_must_be_the_last_op() {
    let src = "ok: 1\n---\nbad: true\n---\nok: 2\n";
    let out = fix(
        &whole_rule(r#"[{"op":"remove","path":""},{"op":"add","path":"/x","value":1}]"#),
        src,
    );
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert_eq!(
        out.stderr,
        "rule test.whole fix failed: fix.ops removes document 1 of the stream (remove \"\") \
         and then runs more ops, which would edit the document after it; remove \"\" must be \
         the last op\n"
    );
    assert_eq!(out.file, src, "untouched");
}

/// `replace ""` in a stream writes that document anew; the other
/// documents, the markers and the comments around it stay byte for byte,
/// `\r\n` line breaks included.
#[test]
fn fix_ops_replace_of_a_stream_document_keeps_the_other_bytes() {
    let src = "# head\r\nok: 1 # keep\r\n...\r\n--- # marker\r\nbad: true\r\n# tail\r\n---\r\nok:   \"2\"\r\n";
    let out = fix(
        &whole_rule(
            r#"if .bad then [{"op":"replace","path":"","value":{"ok":true,"list":[1,2]}}] else [] end"#,
        ),
        src,
    );
    assert_eq!((out.code, out.stderr.as_str()), (0, ""));
    assert_eq!(
        out.file,
        "# head\r\nok: 1 # keep\r\n...\r\n--- # marker\r\nok: true\r\nlist:\r\n- 1\r\n- 2\r\n\
         # tail\r\n---\r\nok:   \"2\"\r\n"
    );
    assert_eq!(
        read("s.yaml", &out.file),
        json!([{"ok": 1}, {"ok": true, "list": [1, 2]}, {"ok": "2"}])
    );
}

// ---- `%TAG` rebinding `!!` --------------------------------------------

/// Under `%TAG !! tag:example.com,2000:` a `!!str` is an application's
/// tag: a plain scalar under it reads untagged (`1` is a number), a quoted
/// one stays a string, and the core tag written verbatim still makes one.
/// The binding holds for its own document only.
#[test]
fn a_rebound_secondary_handle_reads_plain_scalars_untagged() {
    assert_eq!(
        read(
            "f.yaml",
            "%TAG !! tag:example.com,2000:\n---\na: !!str 1\nb: !<tag:yaml.org,2002:str> 1\n\
             c: !!str \"1\"\nd: !!int x\n"
        ),
        json!({"a": 1, "b": "1", "c": "1", "d": "x"})
    );
    assert_eq!(
        read(
            "s.yaml",
            "%TAG !! tag:example.com,2000:\n---\na: !!str 1\n---\nb: !!str 1\n"
        ),
        json!([{"a": 1}, {"b": "1"}])
    );
    // Without the directive the core `!!str` makes a string.
    assert_eq!(read("f.yaml", "a: !!str 1\n"), json!({"a": "1"}));
}

/// A `set` keeps a rebound `!!str` on the new number, which still reads
/// as the number.
#[test]
fn set_under_a_rebound_secondary_handle_keeps_the_tag() {
    check(&[
        Case {
            name: "f.yaml",
            src: "%TAG !! tag:example.com,2000:\n---\na: !!str 1\nb: !<tag:yaml.org,2002:str> 1\n",
            args: &["set", "f.yaml", "/a", "2"],
            want: "%TAG !! tag:example.com,2000:\n---\na: !!str 2\nb: !<tag:yaml.org,2002:str> 1\n",
            reads: json!({"a": 2, "b": "1"}),
        },
        Case {
            name: "f.yaml",
            src: "%TAG !! tag:example.com,2000:\n---\na: !!str \"1\"\n",
            args: &["set", "f.yaml", "/a", "2"],
            want: "%TAG !! tag:example.com,2000:\n---\na: !!str 2\n",
            reads: json!({"a": 2}),
        },
        Case {
            name: "s.yaml",
            src: "%TAG !! tag:example.com,2000:\n---\na: !!str 1\n---\nb: !!str 1\n",
            args: &["set", "s.yaml", "/0/a", "2"],
            want: "%TAG !! tag:example.com,2000:\n---\na: !!str 2\n---\nb: !!str 1\n",
            reads: json!([{"a": 2}, {"b": "1"}]),
        },
    ]);
}

// ---- lone-`\r` line breaks --------------------------------------------

/// A file whose lines end in a lone `\r` takes deletes, nested keys and
/// new keys, and the lines dq writes end in `\r` too.
#[test]
fn lone_carriage_returns_stay_the_line_breaks() {
    check(&[
        Case {
            name: "f.yaml",
            src: "a: 1\rb: 2\r",
            args: &["del", "f.yaml", "/a"],
            want: "b: 2\r",
            reads: json!({"b": 2}),
        },
        Case {
            name: "f.yaml",
            src: "a:\r  x: 1\rb: 2\r",
            args: &["set", "f.yaml", "/a/y", "2"],
            want: "a:\r  x: 1\r  \"y\": 2\rb: 2\r",
            reads: json!({"a": {"x": 1, "y": 2}, "b": 2}),
        },
        Case {
            name: "f.yaml",
            src: "a: |\r  one\r  two\rb: 2\r",
            args: &["set", "f.yaml", "/c", "3"],
            want: "a: |\r  one\r  two\rb: 2\rc: 3\r",
            reads: json!({"a": "one\ntwo\n", "b": 2, "c": 3}),
        },
        Case {
            name: "f.json",
            src: "{\r  \"a\": 1\r}\r",
            args: &["set", "f.json", "/b", "3"],
            want: "{\r  \"a\": 1,\r  \"b\": 3\r}\r",
            reads: json!({"a": 1, "b": 3}),
        },
    ]);
}

/// TOML has no lone-`\r` line break: such a file does not parse (exit 3).
#[test]
fn a_toml_file_with_lone_carriage_returns_does_not_parse() {
    assert_eq!(
        refused(
            "f.toml",
            "a = 1\rb = 2\r",
            &["-i", "set", "f.toml", "/a", "5"],
            3
        ),
        "parse error in f.toml: carriage return must be followed by newline, expected newline\n"
    );
}

// ---- YAML flow collections --------------------------------------------

/// An entry with no value (`{n}`, `{? n}`, `{n: }`, `{'n' }`, `[a: ]`)
/// takes one, goes, or gets a sibling.
#[test]
fn an_implicit_null_flow_entry_takes_a_value_and_goes() {
    let case = |src, args, want, reads| Case {
        name: "f.yaml",
        src,
        args,
        want,
        reads,
    };
    check(&[
        case(
            "z: {n}\n",
            &["set", "f.yaml", "/z/n", "1"],
            "z: {n: 1}\n",
            json!({"z": {"n": 1}}),
        ),
        case(
            "z: {n}\n",
            &["del", "f.yaml", "/z/n"],
            "z: {}\n",
            json!({"z": {}}),
        ),
        case(
            "z: {n}\n",
            &["set", "f.yaml", "/z/m", "1"],
            "z: {n, m: 1}\n",
            json!({"z": {"n": null, "m": 1}}),
        ),
        case(
            "z: [{n}]\n",
            &["del", "f.yaml", "/z/0/n"],
            "z: [{}]\n",
            json!({"z": [{}]}),
        ),
        case(
            "z: {? n}\n",
            &["del", "f.yaml", "/z/n"],
            "z: {}\n",
            json!({"z": {}}),
        ),
        case(
            "z: {n: }\n",
            &["del", "f.yaml", "/z/n"],
            "z: {}\n",
            json!({"z": {}}),
        ),
        case(
            "z: {'n' }\n",
            &["set", "f.yaml", "/z/n", "1"],
            "z: {'n': 1 }\n",
            json!({"z": {"n": 1}}),
        ),
        case(
            "z: {'n' }\n",
            &["del", "f.yaml", "/z/n"],
            "z: {}\n",
            json!({"z": {}}),
        ),
        case(
            "z: {&x n}\n",
            &["del", "f.yaml", "/z/n"],
            "z: {}\n",
            json!({"z": {}}),
        ),
        // An anchored empty value takes a value after its anchor.
        case(
            "z: {n: &a , b: *a}\n",
            &["set", "f.yaml", "/z/n", "1"],
            "z: {n: &a 1 , b: *a}\n",
            json!({"z": {"n": 1, "b": 1}}),
        ),
    ]);
}

/// An entry whose empty value holds an anchor an alias names stays: the
/// refusal says which line holds the alias.
#[test]
fn an_implicit_null_entry_an_alias_names_is_not_deleted() {
    assert_eq!(
        refused(
            "f.yaml",
            "z: {n: &a , b: *a}\n",
            &["-i", "del", "f.yaml", "/z/n"],
            7
        ),
        "cannot delete '/z/n': it holds an anchor that the alias on line 1 refers to; \
         `dq set --jq` rewrites the whole file through its writer instead, dropping comments\n"
    );
    // The alias goes, and with it no blank before the brace.
    assert_eq!(
        edit("f.yaml", "z: {n: &a , b: *a}\n", &["del", "f.yaml", "/z/b"]),
        "z: {n: &a}\n"
    );
}

/// A mapping written without braces inside a flow sequence (`[a: 1]`,
/// `[? a]`) is left empty by a delete, but takes no new key: that edit
/// stays refused (exit 7).
#[test]
fn a_flow_sequence_mapping_without_braces_empties_but_takes_no_key() {
    check(&[
        Case {
            name: "f.yaml",
            src: "z: [? a]\n",
            args: &["del", "f.yaml", "/z/0/a"],
            want: "z: [{}]\n",
            reads: json!({"z": [{}]}),
        },
        Case {
            name: "f.yaml",
            src: "[x, a: 1, y]\n",
            args: &["del", "f.yaml", "/1/a"],
            want: "[x, {}, y]\n",
            reads: json!(["x", {}, "y"]),
        },
    ]);
    for src in ["z: [a: 1]\n", "z: [a: ]\n", "z: [? a]\n"] {
        let err = refused("f.yaml", src, &["-i", "set", "f.yaml", "/z/0/b", "1"], 7);
        assert!(
            err.starts_with("cannot set '/z/0/b': dq cannot write this value here"),
            "{src:?}: {err}"
        );
    }
}

/// Deletes in a flow mapping leave no blank before `,` or `}`; a comment
/// after the comma before the deleted entry stays with that comma.
#[test]
fn a_flow_delete_leaves_no_blank_and_keeps_the_comment() {
    check(&[
        Case {
            name: "f.yaml",
            src: "z: {a: 1 , b: 2}\n",
            args: &["del", "f.yaml", "/z/b"],
            want: "z: {a: 1}\n",
            reads: json!({"z": {"a": 1}}),
        },
        Case {
            name: "f.yaml",
            src: "z: {[a]: 1, # c\n  b: 2}\n",
            args: &["del", "f.yaml", "/z/b"],
            want: "z: {[a]: 1, # c\n  }\n",
            reads: json!({"z": {"[a]": 1}}),
        },
        Case {
            name: "f.yaml",
            src: "z: {[a]: 1, # c\n  b: 2}\n",
            args: &["del", "f.yaml", "/z/[a]"],
            want: "z: { # c\n  b: 2}\n",
            reads: json!({"z": {"b": 2}}),
        },
    ]);
}

/// An empty node tagged `!!null` reads as null and takes a value; an
/// empty tagged item of a flow sequence takes one and goes. A tag that
/// ends the entry before a new one keeps a blank before the comma (YAML
/// 1.1 readers take `!!null,` for one tag).
#[test]
fn an_empty_tagged_flow_node_reads_takes_a_value_and_goes() {
    check(&[
        Case {
            name: "f.yaml",
            src: "{n: !!null }\n",
            args: &["set", "f.yaml", "/n", "1"],
            want: "{n: 1 }\n",
            reads: json!({"n": 1}),
        },
        Case {
            name: "f.yaml",
            src: "{n: !!null }\n",
            args: &["set", "f.yaml", "/new", "1"],
            want: "{n: !!null , new: 1 }\n",
            reads: json!({"n": null, "new": 1}),
        },
        Case {
            name: "f.yaml",
            src: "{n: !!null\n}\n",
            args: &["set", "f.yaml", "/new", "1"],
            want: "{n: !!null , new: 1\n}\n",
            reads: json!({"n": null, "new": 1}),
        },
        Case {
            name: "f.yaml",
            src: "{n: !foo }\n",
            args: &["set", "f.yaml", "/new", "1"],
            want: "{n: !foo , new: 1 }\n",
            reads: json!({"n": null, "new": 1}),
        },
        Case {
            name: "f.yaml",
            src: "[!!str , 1]\n",
            args: &["set", "f.yaml", "/0", "2"],
            want: "[2 , 1]\n",
            reads: json!([2, 1]),
        },
        Case {
            name: "f.yaml",
            src: "[!!str , 1]\n",
            args: &["del", "f.yaml", "/0"],
            want: "[1]\n",
            reads: json!([1]),
        },
    ]);
}

// ---- empty files ------------------------------------------------------

/// A YAML file with no content — empty, blank lines, a byte order mark,
/// comments, a bare `---` — takes a first key after what it holds, with
/// its line breaks; so does an empty document of a stream. An empty TOML
/// file takes one too.
#[test]
fn set_in_an_empty_file_writes_the_first_key() {
    let case = |name, src, args, want, reads| Case {
        name,
        src,
        args,
        want,
        reads,
    };
    let a = &["set", "f.yaml", "/a", "1"][..];
    check(&[
        case("f.yaml", "", a, "a: 1\n", json!({"a": 1})),
        case("f.yaml", "\r\n", a, "\r\na: 1\r\n", json!({"a": 1})),
        case("f.yaml", "\n\n", a, "\n\na: 1\n", json!({"a": 1})),
        case("f.yaml", "\u{feff}", a, "\u{feff}a: 1\n", json!({"a": 1})),
        case(
            "f.yaml",
            "\u{feff}\r\n",
            a,
            "\u{feff}\r\na: 1\r\n",
            json!({"a": 1}),
        ),
        case("f.yaml", "# c\r\n", a, "# c\r\na: 1\r\n", json!({"a": 1})),
        case("f.yaml", "---\r\n", a, "---\r\na: 1\r\n", json!({"a": 1})),
        case(
            "f.yaml",
            "",
            &["set", "f.yaml", "", "{\"a\": 1}"],
            "a: 1\n",
            json!({"a": 1}),
        ),
        case(
            "s.yaml",
            "a: 1\n---\n# c\n---\nb: 2\n",
            &["set", "s.yaml", "/1/x", "1"],
            "a: 1\n---\nx: 1\n# c\n---\nb: 2\n",
            json!([{"a": 1}, {"x": 1}, {"b": 2}]),
        ),
        case(
            "f.toml",
            "",
            &["set", "f.toml", "/a", "1"],
            "a = 1\n",
            json!({"a": 1}),
        ),
    ]);
}

/// Without `-i` the edit of an empty file goes to stdout, or as a diff
/// with `--diff`; the file stays empty.
#[test]
fn set_in_an_empty_file_prints_or_diffs_and_leaves_the_file() {
    for (src, args, stdout) in [
        (
            "",
            &["set", "--diff", "f.yaml", "/a", "1"][..],
            "--- a/f.yaml\n+++ b/f.yaml\n@@ -0,0 +1 @@\n+a: 1\n",
        ),
        ("# c\n", &["set", "f.yaml", "/a", "1"][..], "# c\na: 1\n"),
    ] {
        let out = run("f.yaml", src, args);
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (0, stdout, ""),
            "dq {args:?} on {src:?}"
        );
        assert_eq!(out.file, src);
    }
}

/// An empty document has nothing to delete (exit 2). A null or tagged
/// empty document is a node, so no key goes below it (exit 2); an empty
/// JSON file is no JSON (exit 3).
#[test]
fn an_empty_file_has_nothing_to_delete_and_a_null_document_takes_no_key() {
    for (name, src, args, code) in [
        ("f.yaml", "", &["-i", "del", "f.yaml", "/a"][..], 2),
        ("f.yaml", "# c\n", &["-i", "del", "f.yaml", "/a"][..], 2),
        (
            "s.yaml",
            "a: 1\n---\n---\nb: 2\n",
            &["-i", "del", "s.yaml", "/1/x"][..],
            2,
        ),
        ("f.yaml", "~\n", &["-i", "set", "f.yaml", "/a", "1"][..], 2),
        (
            "f.yaml",
            "--- ~\n",
            &["-i", "set", "f.yaml", "/a", "1"][..],
            2,
        ),
        (
            "f.yaml",
            "--- !!map\n",
            &["-i", "set", "f.yaml", "/a", "1"][..],
            2,
        ),
        ("f.json", "", &["-i", "set", "f.json", "/a", "1"][..], 3),
    ] {
        let err = refused(name, src, args, code);
        let pointer = args.get(3).copied().unwrap_or_default();
        let want = if code == 2 {
            format!("path '{pointer}' not found")
        } else {
            "parse error in f.json: EOF while parsing a value".to_owned()
        };
        assert!(err.starts_with(&want), "dq {args:?} on {src:?}: {err}");
    }
}

// ---- YAML flow spelling -----------------------------------------------

/// The strings of [`FLOW_SPELLINGS`] in one JSON array.
fn flow_strings() -> String {
    serde_json::to_string(&FLOW_SPELLINGS.iter().map(|(s, _)| s).collect::<Vec<_>>()).expect("JSON")
}

/// Strings as `set` spells them in a YAML flow collection: plain where a
/// YAML 1.2 and a YAML 1.1 reader both take the text for that string,
/// double-quoted where it would read as a number, a boolean (`on`, `y`),
/// null, a timestamp, or would start or end the entry early.
const FLOW_SPELLINGS: &[(&str, &str)] = &[
    ("x y", "x y"),
    ("a:b", "a:b"),
    ("a#b", "a#b"),
    ("a'b", "a'b"),
    ("a\"b", "a\"b"),
    ("a|b", "a|b"),
    ("é", "é"),
    ("1", "\"1\""),
    ("1.5", "\"1.5\""),
    ("+1", "\"+1\""),
    ("0x1F", "\"0x1F\""),
    ("0o7", "\"0o7\""),
    ("1_000", "\"1_000\""),
    ("12:30", "\"12:30\""),
    (".inf", "\".inf\""),
    ("2001-12-14", "\"2001-12-14\""),
    ("on", "\"on\""),
    ("No", "\"No\""),
    ("y", "\"y\""),
    ("null", "\"null\""),
    ("~", "\"~\""),
    ("", "\"\""),
    ("a: b", "\"a: b\""),
    ("a:", "\"a:\""),
    (":a", "\":a\""),
    ("#c", "\"#c\""),
    ("a #b", "\"a #b\""),
    ("- x", "\"- x\""),
    ("-", "\"-\""),
    ("?", "\"?\""),
    ("? x", "\"? x\""),
    ("[a", "\"[a\""),
    ("a]", "\"a]\""),
    ("{a", "\"{a\""),
    ("a}", "\"a}\""),
    ("a,b", "\"a,b\""),
    ("*a", "\"*a\""),
    ("&a", "\"&a\""),
    ("!a", "\"!a\""),
    ("%a", "\"%a\""),
    ("@a", "\"@a\""),
    ("`a", "\"`a\""),
    ("'a", "\"'a\""),
    ("\"a", "\"\\\"a\""),
    (" lead", "\" lead\""),
    ("trail ", "\"trail \""),
    ("<<", "\"<<\""),
    ("=", "\"=\""),
    ("a\nb", "\"a\\nb\""),
    ("t\tb", "\"t\\tb\""),
    ("---", "\"---\""),
    ("...", "\"...\""),
    (">", "\">\""),
    ("|", "\"|\""),
];

/// The values `set` writes into a YAML flow context and their sources:
/// `[(json, yaml)]`, each a list and a mapping of [`FLOW_SPELLINGS`].
fn flow_cases() -> Vec<(Value, String)> {
    let list: Vec<&str> = FLOW_SPELLINGS.iter().map(|(_, y)| *y).collect();
    let map: Vec<String> = FLOW_SPELLINGS
        .iter()
        .map(|(_, y)| format!("{y}: 0"))
        .collect();
    let strings: Value = serde_json::from_str(&flow_strings()).expect("JSON");
    let keys: serde_json::Map<String, Value> = FLOW_SPELLINGS
        .iter()
        .map(|(s, _)| ((*s).to_owned(), json!(0)))
        .collect();
    vec![
        (
            json!({"k": strings}),
            format!("a: {{k: [{}]}}\n", list.join(", ")),
        ),
        (Value::Object(keys), format!("a: {{{}}}\n", map.join(", "))),
    ]
}

/// A collection `set` writes into a flow context is YAML flow style, not
/// JSON: strings quoted only when they need it, keys too; dq reads every
/// string back as it was.
#[test]
fn a_collection_is_written_in_yaml_flow_style() {
    for (value, want) in flow_cases() {
        let arg = serde_json::to_string(&value).expect("JSON");
        let got = edit("f.yaml", "a: 1\n", &["set", "f.yaml", "/a", &arg]);
        assert_eq!(got, want);
        assert_eq!(read("f.yaml", &got), json!({"a": value}));
    }
    check(&[
        Case {
            name: "f.yaml",
            src: "{}\n",
            args: &["set", "f.yaml", "", "{\"b\": 2}"],
            want: "{b: 2}\n",
            reads: json!({"b": 2}),
        },
        Case {
            name: "f.yaml",
            src: "a: {x: 1}\n",
            args: &["set", "f.yaml", "/a/z", "{\"q\": [1, \"on\"]}"],
            want: "a: {x: 1, z: {q: [1, \"on\"]}}\n",
            reads: json!({"a": {"x": 1, "z": {"q": [1, "on"]}}}),
        },
    ]);
}

/// The same flow text read by PyYAML, a YAML 1.1 reader (as go-yaml in
/// Kubernetes and Helm is): every string and key reads back as the string
/// it was. A one-off oracle, skipped with a notice where `python3` with
/// PyYAML is missing.
#[test]
fn pyyaml_reads_the_yaml_flow_spelling_back_as_the_same_strings() {
    let probe = std::process::Command::new("python3")
        .args(["-I", "-c", "import yaml"])
        .output();
    if !probe.is_ok_and(|out| out.status.success()) {
        eprintln!("skipped: python3 with PyYAML is not available");
        return;
    }
    for (value, yaml) in flow_cases() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("f.yaml");
        std::fs::write(&path, &yaml).expect("write YAML");
        let out = std::process::Command::new("python3")
            .args([
                "-I",
                "-c",
                "import json, sys, yaml\n\
                 print(json.dumps(yaml.safe_load(open(sys.argv[1], encoding='utf-8'))))",
            ])
            .arg(&path)
            .output()
            .expect("python3 runs");
        assert!(
            out.status.success(),
            "{yaml}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let read: Value = serde_json::from_slice(&out.stdout).expect("JSON from python3");
        assert_eq!(read, json!({"a": value}), "{yaml}");
    }
}

#[test]
fn a_scalar_whose_last_word_starts_with_a_bang_is_no_tag() {
    check(&[
        Case {
            name: "f.yaml",
            src: "{a: x !y}\n",
            args: &["set", "f.yaml", "/new", "1"],
            want: "{a: x !y, new: 1}\n",
            reads: json!({"a": "x !y", "new": 1}),
        },
        Case {
            name: "f.yaml",
            src: "{a: 'x !y'}\n",
            args: &["set", "f.yaml", "/new", "1"],
            want: "{a: 'x !y', new: 1}\n",
            reads: json!({"a": "x !y", "new": 1}),
        },
        Case {
            name: "f.yaml",
            src: "[x !y , 1]\n",
            args: &["del", "f.yaml", "/1"],
            want: "[x !y]\n",
            reads: json!(["x !y"]),
        },
    ]);
}

/// The empty value of a brace-less pair goes with its blanks.
#[test]
fn del_in_a_brace_less_pair_leaves_no_blank_of_its_empty_value() {
    check(&[
        Case {
            name: "f.yaml",
            src: "z: [a: ]\n",
            args: &["del", "f.yaml", "/z/0/a"],
            want: "z: [{}]\n",
            reads: json!({"z": [{}]}),
        },
        Case {
            name: "f.yaml",
            src: "z: [a: , b]\n",
            args: &["del", "f.yaml", "/z/0/a"],
            want: "z: [{}, b]\n",
            reads: json!({"z": [{}, "b"]}),
        },
    ]);
}

// ---- mappings created on the way --------------------------------------

/// `set` creates the mappings missing above the key it sets, in YAML,
/// JSON and TOML, inside one document of a stream too. A numeric segment
/// is a mapping key; comments stay.
#[test]
fn set_creates_the_missing_mappings_on_the_way() {
    check(&[
        Case {
            name: "f.yaml",
            src: "a: 1 # c\n",
            args: &["set", "f.yaml", "/b/c", "2"],
            want: "a: 1 # c\nb:\n  c: 2\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
        Case {
            name: "f.yaml",
            src: "{a: 1}\n",
            args: &["set", "f.yaml", "/b/c", "2"],
            want: "{a: 1, b: {c: 2}}\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
        Case {
            name: "f.yaml",
            src: "a: 1\n",
            args: &["set", "f.yaml", "/b/0/c", "2"],
            want: "a: 1\nb:\n  \"0\": {c: 2}\n",
            reads: json!({"a": 1, "b": {"0": {"c": 2}}}),
        },
        Case {
            name: "s.yaml",
            src: "a: 1\n---\nb: 2\n",
            args: &["set", "s.yaml", "/1/c/d", "3"],
            want: "a: 1\n---\nb: 2\nc:\n  d: 3\n",
            reads: json!([{"a": 1}, {"b": 2, "c": {"d": 3}}]),
        },
        Case {
            name: "f.json",
            src: "{\"a\": 1}\n",
            args: &["set", "f.json", "/b/c", "2"],
            want: "{\"a\": 1, \"b\": {\"c\":2}}\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
    ]);
}

/// A TOML chain of tables is a dotted key in a file without headers or
/// under a table with none, a `[header]` in a file that writes its
/// tables under headers, and a nested inline table inside an inline one.
#[test]
fn set_creates_missing_toml_tables_in_the_style_of_the_file() {
    check(&[
        Case {
            name: "f.toml",
            src: "a = 1 # c\n",
            args: &["set", "f.toml", "/b/c/d", "2"],
            want: "a = 1 # c\nb.c.d = 2\n",
            reads: json!({"a": 1, "b": {"c": {"d": 2}}}),
        },
        Case {
            name: "f.toml",
            src: "a = 1\n\n[t]\nx = 1\n",
            args: &["set", "f.toml", "/b/c", "2"],
            want: "a = 1\n\n[t]\nx = 1\n\n[b]\nc = 2\n",
            reads: json!({"a": 1, "t": {"x": 1}, "b": {"c": 2}}),
        },
        Case {
            name: "f.toml",
            src: "a = 1\n\n[t]\nx = 1\n\n[u]\ny = 1\n",
            args: &["set", "f.toml", "/t/y/z", "2"],
            want: "a = 1\n\n[t]\nx = 1\n\n[t.y]\nz = 2\n\n[u]\ny = 1\n",
            reads: json!({"a": 1, "t": {"x": 1, "y": {"z": 2}}, "u": {"y": 1}}),
        },
        Case {
            name: "f.toml",
            src: "t.x = 1\n",
            args: &["set", "f.toml", "/t/y/z", "2"],
            want: "t.x = 1\nt.y.z = 2\n",
            reads: json!({"t": {"x": 1, "y": {"z": 2}}}),
        },
        Case {
            name: "f.toml",
            src: "t = { x = 1 }\n",
            args: &["set", "f.toml", "/t/y/z", "2"],
            want: "t = { x = 1, y = { z = 2 } }\n",
            reads: json!({"t": {"x": 1, "y": {"z": 2}}}),
        },
        Case {
            name: "f.toml",
            src: "[[arr]]\nx = 1\n",
            args: &["set", "f.toml", "/arr/0/y/z", "2"],
            want: "[[arr]]\nx = 1\ny.z = 2\n",
            reads: json!({"arr": [{"x": 1, "y": {"z": 2}}]}),
        },
        Case {
            name: "f.toml",
            src: "a = 1\r\n\r\n[t]\r\nx = 1\r\n",
            args: &["set", "f.toml", "/b/c", "2"],
            want: "a = 1\r\n\r\n[t]\r\nx = 1\r\n\r\n[b]\r\nc = 2\r\n",
            reads: json!({"a": 1, "t": {"x": 1}, "b": {"c": 2}}),
        },
    ]);
}

/// `--no-create` refuses the chain (exit 2), and so does a scalar or the
/// array-append marker on the way, `--no-create` or not.
#[test]
fn set_refuses_a_chain_with_no_create_or_under_a_scalar() {
    for (name, src) in [
        ("f.yaml", "a: 1\n"),
        ("f.json", "{\"a\": 1}\n"),
        ("f.toml", "a = 1\n"),
    ] {
        refused(name, src, &["set", "--no-create", name, "/b/c", "2"], 2);
        refused(name, src, &["set", name, "/a/c/d", "2"], 2);
        refused(name, src, &["set", name, "/b/-/c", "2"], 2);
    }
}

/// `dq patch` follows RFC 6902: `add`, `copy` and `move` into a missing
/// parent are refused (exit 2, naming the op's own path), `--no-create`
/// or not; a parent an earlier op made is there.
#[test]
fn patch_never_creates_a_missing_parent() {
    for ops in [
        r#"[{"op":"add","path":"/b/c","value":2}]"#,
        r#"[{"op":"copy","from":"/a","path":"/b/c"}]"#,
        r#"[{"op":"move","from":"/a","path":"/b/c"}]"#,
    ] {
        for args in [
            &["-i", "patch", "f.yaml", ops][..],
            &["-i", "patch", "--no-create", "f.yaml", ops][..],
        ] {
            let stderr = refused("f.yaml", "a: 1\n", args, 2);
            assert!(stderr.starts_with("path '/b/c' not found"), "{stderr}");
        }
    }
    let stepwise = r#"[{"op":"add","path":"/b","value":{}},{"op":"add","path":"/b/c","value":2}]"#;
    check(&[Case {
        name: "f.yaml",
        src: "a: 1\n",
        args: &["patch", "f.yaml", stepwise],
        want: "a: 1\nb: {c: 2}\n",
        reads: json!({"a": 1, "b": {"c": 2}}),
    }]);
}

/// The last `[[array]]` item of a CRLF file goes, and the `key = []`
/// written in its place ends in `\r\n` too.
#[test]
fn del_of_the_last_toml_array_item_keeps_the_crlf_line_break() {
    check(&[Case {
        name: "f.toml",
        src: "[[arr]]\r\nx = 1\r\n",
        args: &["del", "f.toml", "/arr/0"],
        want: "arr = []\r\n",
        reads: json!({"arr": []}),
    }]);
}

/// An `[[array]]` under a dotted name empties to `t.arr = []`, in the
/// file's line breaks; a dotted or implicit table emptied by a delete
/// keeps CRLF too.
#[test]
fn del_that_empties_a_toml_table_keeps_the_crlf_line_breaks() {
    let case = |src, args, want, reads| Case {
        name: "f.toml",
        src,
        args,
        want,
        reads,
    };
    check(&[
        case(
            "a = 1\r\n[[t.arr]]\r\nx = 1\r\n",
            &["del", "f.toml", "/t/arr/0"],
            "a = 1\r\nt.arr = []\r\n",
            json!({"a": 1, "t": {"arr": []}}),
        ),
        case(
            "a = 1\n[[t.arr]]\nx = 1\n",
            &["del", "f.toml", "/t/arr/0"],
            "a = 1\nt.arr = []\n",
            json!({"a": 1, "t": {"arr": []}}),
        ),
        case(
            "a = 1\r\nt.x = 1\r\n",
            &["del", "f.toml", "/t/x"],
            "a = 1\r\nt = {}\r\n",
            json!({"a": 1, "t": {}}),
        ),
        case(
            "a = 1\r\nt.u.x = 1\r\n",
            &["del", "f.toml", "/t/u/x"],
            "a = 1\r\nt.u = {}\r\n",
            json!({"a": 1, "t": {"u": {}}}),
        ),
        case(
            "a = 1\r\n[t.u]\r\nx = 1\r\n",
            &["del", "f.toml", "/t/u/x"],
            "a = 1\r\n[t.u]\r\n",
            json!({"a": 1, "t": {"u": {}}}),
        ),
        case(
            "a = 1\r\n[t.u]\r\nx = 1\r\n",
            &["del", "f.toml", "/t/u"],
            "a = 1\r\n[t]\r\n",
            json!({"a": 1, "t": {}}),
        ),
    ]);
}

// ---- the lead-in through `dq fix` -------------------------------------

/// A `fix.ops` `replace ""` keeps the comment and blank lines between a
/// document's `---` line and its content, in a file and in one document
/// of a CRLF stream.
#[test]
fn fix_ops_replace_keeps_the_lead_in_after_the_marker() {
    let rule =
        whole_rule(r#"if .bad then [{"op":"replace","path":"","value":{"ok":true}}] else [] end"#);
    for (src, want, reads) in [
        (
            "---\n# lead\n\nbad: true\n",
            "---\n# lead\n\nok: true\n",
            json!({"ok": true}),
        ),
        (
            "a: 1\r\n---\r\n# lead\r\n\r\nbad: true\r\n---\r\nc: 3\r\n",
            "a: 1\r\n---\r\n# lead\r\n\r\nok: true\r\n---\r\nc: 3\r\n",
            json!([{"a": 1}, {"ok": true}, {"c": 3}]),
        ),
    ] {
        let out = fix(&rule, src);
        assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{src:?}");
        assert_eq!(out.file, want, "{src:?}");
        assert_eq!(read("s.yaml", &out.file), reads);
    }
}

/// A `fix.jq` re-emit of one document of a stream keeps its lead-in
/// lines, under a kept `--- !!map` too.
#[test]
fn fix_jq_reemit_of_a_stream_document_keeps_its_lead_in() {
    let rule = "
id: test.jq
description: re-emit one document
severity: warn
match: {format: yaml}
check:
  jq: 'select(.bad == true)'
  message: bad
fix:
  jq: 'del(.bad) | .ok = true'
";
    for (src, want, reads) in [
        (
            "a: 1\r\n---\r\n# lead\r\n\r\nbad: true\r\n---\r\nc: 3\r\n",
            "a: 1\r\n---\r\n# lead\r\n\r\nok: true\r\n---\r\nc: 3\r\n",
            json!([{"a": 1}, {"ok": true}, {"c": 3}]),
        ),
        (
            "a: 1\n--- !!map\n  # lead\nbad: true\n",
            "a: 1\n--- !!map\n  # lead\nok: true\n",
            json!([{"a": 1}, {"ok": true}]),
        ),
    ] {
        let out = fix(rule, src);
        assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{src:?}");
        assert_eq!(out.file, want, "{src:?}");
        assert_eq!(read("s.yaml", &out.file), reads);
    }
}

// ---- chains of created mappings, further -------------------------------

/// A chain whose leaf value is a collection: the collection is written
/// where the chain ends, in each format's style.
#[test]
fn set_creates_a_chain_whose_leaf_is_a_collection() {
    let value = "{\"x\": [1, 2]}";
    let reads = || json!({"a": 1, "b": {"c": {"x": [1, 2]}}});
    check(&[
        Case {
            name: "f.yaml",
            src: "a: 1\n",
            args: &["set", "f.yaml", "/b/c", value],
            want: "a: 1\nb:\n  c: {x: [1, 2]}\n",
            reads: reads(),
        },
        Case {
            name: "f.yaml",
            src: "{a: 1}\n",
            args: &["set", "f.yaml", "/b/c", value],
            want: "{a: 1, b: {c: {x: [1, 2]}}}\n",
            reads: reads(),
        },
        Case {
            name: "f.json",
            src: "{\"a\": 1}\n",
            args: &["set", "f.json", "/b/c", value],
            want: "{\"a\": 1, \"b\": {\"c\":{\"x\":[1,2]}}}\n",
            reads: reads(),
        },
        Case {
            name: "f.toml",
            src: "a = 1\n",
            args: &["set", "f.toml", "/b/c", value],
            want: "a = 1\nb.c = { x = [1, 2] }\n",
            reads: reads(),
        },
        Case {
            name: "f.toml",
            src: "a = 1\n\n[t]\nx = 1\n",
            args: &["set", "f.toml", "/b/c", value],
            want: "a = 1\n\n[t]\nx = 1\n\n[b]\nc = { x = [1, 2] }\n",
            reads: json!({"a": 1, "t": {"x": 1}, "b": {"c": {"x": [1, 2]}}}),
        },
    ]);
}

/// Under a table that exists only through a sub-table header (`[a.d]`)
/// the chain is a dotted key under a new `[a]` header before it. In a
/// file whose last section is an `[[array]]`, a new table goes after it,
/// a sub-table of an earlier table before it, and a chain inside the
/// array's item stays in that item.
#[test]
fn set_creates_a_toml_chain_under_an_implicit_table_and_around_an_array() {
    let arr = "[t]\nx = 1\n\n[[arr]]\ny = 1\n";
    check(&[
        Case {
            name: "f.toml",
            src: "[a.d]\nx = 1\n",
            args: &["set", "f.toml", "/a/q/r", "2"],
            want: "[a]\nq.r = 2\n\n[a.d]\nx = 1\n",
            reads: json!({"a": {"q": {"r": 2}, "d": {"x": 1}}}),
        },
        Case {
            name: "f.toml",
            src: arr,
            args: &["set", "f.toml", "/b/c", "2"],
            want: "[t]\nx = 1\n\n[[arr]]\ny = 1\n\n[b]\nc = 2\n",
            reads: json!({"t": {"x": 1}, "arr": [{"y": 1}], "b": {"c": 2}}),
        },
        Case {
            name: "f.toml",
            src: arr,
            args: &["set", "f.toml", "/t/q/r", "2"],
            want: "[t]\nx = 1\n\n[t.q]\nr = 2\n\n[[arr]]\ny = 1\n",
            reads: json!({"t": {"x": 1, "q": {"r": 2}}, "arr": [{"y": 1}]}),
        },
        Case {
            name: "f.toml",
            src: arr,
            args: &["set", "f.toml", "/arr/0/q/r", "2"],
            want: "[t]\nx = 1\n\n[[arr]]\ny = 1\nq.r = 2\n",
            reads: json!({"t": {"x": 1}, "arr": [{"y": 1, "q": {"r": 2}}]}),
        },
        Case {
            name: "f.toml",
            src: "[t]\r\nx = 1\r\n\r\n[[arr]]\r\ny = 1\r\n",
            args: &["set", "f.toml", "/t/q/r", "2"],
            want: "[t]\r\nx = 1\r\n\r\n[t.q]\r\nr = 2\r\n\r\n[[arr]]\r\ny = 1\r\n",
            reads: json!({"t": {"x": 1, "q": {"r": 2}}, "arr": [{"y": 1}]}),
        },
    ]);
}

/// A chain in one document of a stream through `--doc N`, and in files
/// with a byte order mark or CRLF line breaks: the new lines break as the
/// file's do, the mark stays.
#[test]
fn set_creates_a_chain_with_doc_n_and_in_bom_and_crlf_files() {
    check(&[
        Case {
            name: "s.yaml",
            src: "a: 1\n---\nb: 2\n",
            args: &["--doc", "1", "set", "s.yaml", "/c/d", "3"],
            want: "a: 1\n---\nb: 2\nc:\n  d: 3\n",
            reads: json!([{"a": 1}, {"b": 2, "c": {"d": 3}}]),
        },
        Case {
            name: "f.yaml",
            src: "\u{feff}a: 1\n",
            args: &["set", "f.yaml", "/b/c", "2"],
            want: "\u{feff}a: 1\nb:\n  c: 2\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
        Case {
            name: "f.yaml",
            src: "a: 1\r\n",
            args: &["set", "f.yaml", "/b/c", "2"],
            want: "a: 1\r\nb:\r\n  c: 2\r\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
        Case {
            name: "f.json",
            src: "{\r\n  \"a\": 1\r\n}\r\n",
            args: &["set", "f.json", "/b/c", "2"],
            want: "{\r\n  \"a\": 1,\r\n  \"b\": {\"c\":2}\r\n}\r\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
        Case {
            name: "f.toml",
            src: "a = 1\r\n",
            args: &["set", "f.toml", "/b/c", "2"],
            want: "a = 1\r\nb.c = 2\r\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
        Case {
            name: "f.toml",
            src: "\u{feff}a = 1\n",
            args: &["set", "f.toml", "/b/c", "2"],
            want: "\u{feff}a = 1\nb.c = 2\n",
            reads: json!({"a": 1, "b": {"c": 2}}),
        },
    ]);
}

/// `set --diff` shows the created chain and leaves the file.
#[test]
fn set_diff_shows_a_created_chain_and_leaves_the_file() {
    let out = run(
        "f.yaml",
        "a: 1\n",
        &["set", "--diff", "f.yaml", "/b/c", "2"],
    );
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            0,
            "--- a/f.yaml\n+++ b/f.yaml\n@@ -1 +1,3 @@\n a: 1\n+b:\n+  c: 2\n",
            ""
        )
    );
    assert_eq!(out.file, "a: 1\n");
}

/// A refused chain names the whole pointer and how far it resolved:
/// under a scalar, through `-`, past the end of a sequence, and every
/// chain with `--no-create`. Exit 2, the file untouched.
#[test]
fn a_refused_chain_names_the_whole_pointer() {
    for (name, src) in [
        ("f.yaml", "a: 1\nl: [1]\n"),
        ("f.json", "{\"a\": 1, \"l\": [1]}\n"),
        ("f.toml", "a = 1\nl = [1]\n"),
    ] {
        for (args, stderr) in [
            (
                &["-i", "set", name, "/a/c/d", "2"][..],
                "path '/a/c/d' not found (matched up to '/a')\n  matched prefix: /a\n  \
                 pointer: /a/c/d\n",
            ),
            (
                &["-i", "set", name, "/b/-/c", "2"][..],
                "path '/b/-/c' not found (matched up to '')\n  pointer: /b/-/c\n",
            ),
            (
                &["-i", "set", name, "/l/-/c", "2"][..],
                "path '/l/-/c' not found (matched up to '/l')\n  matched prefix: /l\n  \
                 pointer: /l/-/c\n",
            ),
            (
                &["-i", "set", name, "/l/1/c", "2"][..],
                "path '/l/1/c' not found (matched up to '/l')\n  matched prefix: /l\n  \
                 pointer: /l/1/c\n",
            ),
            (
                &["-i", "set", "--no-create", name, "/b/c/d", "2"][..],
                "path '/b/c/d' not found (matched up to '')\n  did_you_mean: a, l\n  \
                 pointer: /b/c/d\n",
            ),
        ] {
            assert_eq!(refused(name, src, args, 2), stderr, "dq {args:?}");
        }
    }
}

// ---- `dq patch` and parents -------------------------------------------

/// A patch on a stream addresses its documents as `/N/…`; an `add` into
/// a missing parent there is refused like anywhere (exit 2, its own
/// path), and `--no-create` changes nothing.
#[test]
fn patch_on_a_stream_addresses_documents_and_creates_no_parent() {
    let ops = r#"[{"op":"add","path":"/1/c","value":3},{"op":"replace","path":"/0/a","value":9}]"#;
    for args in [
        &["patch", "s.yaml", ops][..],
        &["patch", "--no-create", "s.yaml", ops][..],
    ] {
        check(&[Case {
            name: "s.yaml",
            src: "a: 1\n---\nb: 2\n",
            args,
            want: "a: 9\n---\nb: 2\nc: 3\n",
            reads: json!([{"a": 9}, {"b": 2, "c": 3}]),
        }]);
    }
    let missing = r#"[{"op":"add","path":"/1/c/d","value":3}]"#;
    assert_eq!(
        refused(
            "s.yaml",
            "a: 1\n---\nb: 2\n",
            &["-i", "patch", "s.yaml", missing],
            2
        ),
        "path '/1/c/d' not found (matched up to '/1')\n  matched prefix: /1\n  pointer: /1/c/d\n"
    );
}

/// A `move` whose target's parent stays after `from` is removed lands
/// there.
#[test]
fn patch_move_lands_only_where_the_parent_survives_the_removal() {
    check(&[Case {
        name: "f.yaml",
        src: "a: {x: 1}\nb: 2\n",
        args: &[
            "patch",
            "f.yaml",
            r#"[{"op":"move","from":"/a/x","path":"/a/y"}]"#,
        ],
        want: "a: {\"y\": 1}\nb: 2\n",
        reads: json!({"a": {"y": 1}, "b": 2}),
    }]);
}
