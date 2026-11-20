//! The byte layout of JSON output, command by command.
//!
//! The rule: JSON a command writes — a re-emit to stdout or to a file
//! (`convert`, `set --jq`, `fmt --indent` / `--sort-keys`) or a read
//! command's output (`get`, `query`, `diff`, `lint`) — ends with exactly
//! one line break; JSONL ends each record with one. Indentation is two
//! spaces unless `--indent` says otherwise on a re-emit path. A textual
//! edit of a `.json` file (`set`, `del`, `patch`, `merge` with a pointer)
//! keeps the file's own layout, its final line break or its absence
//! included, and writes what it inserts the same way whatever the op:
//! `patch` `move` lays a value out as `add` and `copy` do.

use assert_cmd::Command;
use tempfile::TempDir;

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

/// Stdout of a successful `dq --no-color <args>` on `files`.
fn stdout_of(files: &[(&str, &str)], args: &[&str]) -> String {
    let dir = dir_with(files);
    let out = dq()
        .current_dir(dir.path())
        .arg("--no-color")
        .args(args)
        .output()
        .expect("dq runs");
    assert!(
        out.status.success(),
        "dq {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("UTF-8 stdout")
}

/// The content `name` has after a successful `dq --no-color -i <args>`.
fn file_after(name: &str, src: &str, args: &[&str]) -> String {
    let dir = dir_with(&[(name, src)]);
    dq().current_dir(dir.path())
        .args(["--no-color", "-i"])
        .args(args)
        .assert()
        .success()
        .stdout("");
    read(&dir, name)
}

#[test]
fn convert_of_a_json_file_ends_with_exactly_one_line_break() {
    // The source bytes are copied; what follows the value is not.
    for (src, want) in [
        ("{\"a\": 1}", "{\"a\": 1}\n"),
        ("{\"a\": 1}\n", "{\"a\": 1}\n"),
        ("{\"a\": 1}\n\n", "{\"a\": 1}\n"),
        ("{\"a\": 1}\n  \n\n", "{\"a\": 1}\n"),
        ("{\"a\": 1}\r\n", "{\"a\": 1}\r\n"),
        ("{\"a\": 1}\r\n\r\n", "{\"a\": 1}\r\n"),
    ] {
        assert_eq!(
            stdout_of(&[("f.json", src)], &["convert", "f.json", "-F", "json"]),
            want,
            "{src:?}"
        );
    }
}

#[test]
fn json_re_emits_end_with_one_line_break_and_follow_indent() {
    let yaml = [("y.yaml", "a: 1\n")];
    assert_eq!(
        stdout_of(&yaml, &["convert", "y.yaml", "-F", "json"]),
        "{\n  \"a\": 1\n}\n"
    );
    assert_eq!(
        stdout_of(&yaml, &["convert", "y.yaml", "-F", "json", "--indent", "0"]),
        "{\"a\":1}\n"
    );
    assert_eq!(
        stdout_of(&yaml, &["convert", "y.yaml", "-F", "json", "--indent", "4"]),
        "{\n    \"a\": 1\n}\n"
    );
    for src in ["{\"a\": 1}", "{\"a\": 1}\n", "{\"a\": 1}\n\n"] {
        assert_eq!(
            stdout_of(&[("f.json", src)], &["set", "f.json", "--jq", ".a = 2"]),
            "{\n  \"a\": 2\n}\n",
            "set --jq on {src:?}"
        );
        assert_eq!(
            file_after("f.json", src, &["set", "f.json", "--jq", ".a = 2"]),
            "{\n  \"a\": 2\n}\n",
            "set --jq -i on {src:?}"
        );
        assert_eq!(
            file_after("f.json", src, &["fmt", "f.json", "--indent", "2"]),
            "{\n  \"a\": 1\n}\n",
            "fmt --indent 2 -i on {src:?}"
        );
        assert_eq!(
            file_after("f.json", src, &["fmt", "f.json", "--sort-keys"]),
            "{\n  \"a\": 1\n}\n",
            "fmt --sort-keys -i on {src:?}"
        );
    }
    assert_eq!(
        file_after(
            "f.json",
            "{\"a\": 1}\n",
            &["set", "f.json", "--jq", ".a = 2", "--indent", "4"]
        ),
        "{\n    \"a\": 2\n}\n"
    );
}

#[test]
fn jsonl_output_ends_each_record_with_one_line_break() {
    let jsonl = [("l.jsonl", "{\"a\":1}\n{\"a\":2}\n")];
    assert_eq!(
        stdout_of(&jsonl, &["convert", "l.jsonl", "-F", "jsonl"]),
        "{\"a\":1}\n{\"a\":2}\n"
    );
    assert_eq!(
        stdout_of(&jsonl, &["convert", "l.jsonl", "-F", "json"]),
        "[\n  {\n    \"a\": 1\n  },\n  {\n    \"a\": 2\n  }\n]\n"
    );
    assert_eq!(
        stdout_of(
            &[("f.json", "{\"a\": 1}\n\n")],
            &["convert", "f.json", "-F", "jsonl"]
        ),
        "{\"a\":1}\n"
    );
    assert_eq!(
        stdout_of(
            &[("f.json", "{\"a\": 1, \"x\": [1, 2]}\n")],
            &["query", ".a, .x", "f.json", "-F", "jsonl"]
        ),
        "1\n[1,2]\n"
    );
}

#[test]
fn read_commands_print_json_with_one_final_line_break() {
    let files = [
        ("f.json", "{\"a\": 1, \"n\": {\"x\": [1, 2]}}\n"),
        ("g.json", "{\"a\": 2}\n"),
    ];
    assert_eq!(
        stdout_of(&files, &["get", "f.json", "/n", "-F", "json"]),
        "{\n  \"x\": [\n    1,\n    2\n  ]\n}\n"
    );
    assert_eq!(
        stdout_of(&files, &["query", ".n.x", "f.json", "-F", "json"]),
        "[\n  [\n    1,\n    2\n  ]\n]\n"
    );
    assert_eq!(
        stdout_of(&files, &["diff", "f.json", "g.json", "-F", "json"]),
        "[\n  {\n    \"op\": \"remove\",\n    \"path\": \"/n\"\n  },\n  {\n    \"op\": \"replace\",\n    \"path\": \"/a\",\n    \"value\": 2\n  }\n]\n"
    );
    assert_eq!(
        stdout_of(&files, &["lint", "f.json", "-F", "json"]),
        "{\n  \"diagnostics\": []\n}\n"
    );
}

#[test]
fn textual_edits_keep_the_file_s_final_line_break_or_its_absence() {
    for (src, set, del) in [
        ("{\"a\": 1, \"b\": 2}", "{\"a\": 3, \"b\": 2}", "{\"b\": 2}"),
        (
            "{\"a\": 1, \"b\": 2}\n",
            "{\"a\": 3, \"b\": 2}\n",
            "{\"b\": 2}\n",
        ),
        (
            "{\"a\": 1, \"b\": 2}\n\n",
            "{\"a\": 3, \"b\": 2}\n\n",
            "{\"b\": 2}\n\n",
        ),
        (
            "{\"a\": 1, \"b\": 2}\r\n",
            "{\"a\": 3, \"b\": 2}\r\n",
            "{\"b\": 2}\r\n",
        ),
    ] {
        let files = [("f.json", src)];
        assert_eq!(
            stdout_of(&files, &["set", "f.json", "/a", "3"]),
            set,
            "{src:?}"
        );
        assert_eq!(
            file_after("f.json", src, &["set", "f.json", "/a", "3"]),
            set,
            "{src:?}"
        );
        assert_eq!(stdout_of(&files, &["del", "f.json", "/a"]), del, "{src:?}");
        assert_eq!(
            stdout_of(
                &files,
                &["patch", "f.json", r#"[{"op":"remove","path":"/a"}]"#]
            ),
            del,
            "{src:?}"
        );
    }
}

#[test]
fn patch_move_lays_the_value_out_as_add_and_copy_do() {
    // One-line file, multi-line file; into an empty object, a non-empty
    // one and an array. `move` gives `copy`'s result without the source.
    let one_line = "{\"ab\": 1, \"a\": {}, \"q\": {\"k\": [1, 2]}}\n";
    let multi =
        "{\n  \"ab\": 1,\n  \"a\": {},\n  \"q\": {\"k\": [1, 2]},\n  \"l\": [\n    1\n  ]\n}\n";
    for (src, ops, want) in [
        (
            one_line,
            r#"[{"op":"move","from":"/ab","path":"/a/b"}]"#,
            "{\"a\": {\"b\": 1}, \"q\": {\"k\": [1, 2]}}\n",
        ),
        (
            one_line,
            r#"[{"op":"add","path":"/a/b","value":1},{"op":"remove","path":"/ab"}]"#,
            "{\"a\": {\"b\": 1}, \"q\": {\"k\": [1, 2]}}\n",
        ),
        (
            one_line,
            r#"[{"op":"move","from":"/q","path":"/a/q"}]"#,
            "{\"ab\": 1, \"a\": {\"q\": {\"k\":[1,2]}}}\n",
        ),
        (
            one_line,
            r#"[{"op":"copy","from":"/q","path":"/a/q"},{"op":"remove","path":"/q"}]"#,
            "{\"ab\": 1, \"a\": {\"q\": {\"k\":[1,2]}}}\n",
        ),
        (
            multi,
            r#"[{"op":"move","from":"/q","path":"/a/q"}]"#,
            "{\n  \"ab\": 1,\n  \"a\": {\n    \"q\": {\"k\":[1,2]}\n  },\n  \"l\": [\n    1\n  ]\n}\n",
        ),
        (
            multi,
            r#"[{"op":"add","path":"/a/q","value":{"k":[1,2]}},{"op":"remove","path":"/q"}]"#,
            "{\n  \"ab\": 1,\n  \"a\": {\n    \"q\": {\"k\":[1,2]}\n  },\n  \"l\": [\n    1\n  ]\n}\n",
        ),
        (
            multi,
            r#"[{"op":"move","from":"/q","path":"/l/0"}]"#,
            "{\n  \"ab\": 1,\n  \"a\": {},\n  \"l\": [\n    {\"k\":[1,2]},\n    1\n  ]\n}\n",
        ),
        (
            multi,
            r#"[{"op":"copy","from":"/q","path":"/l/0"},{"op":"remove","path":"/q"}]"#,
            "{\n  \"ab\": 1,\n  \"a\": {},\n  \"l\": [\n    {\"k\":[1,2]},\n    1\n  ]\n}\n",
        ),
    ] {
        assert_eq!(
            stdout_of(&[("f.json", src)], &["patch", "f.json", ops]),
            want,
            "{ops}"
        );
        assert_eq!(
            file_after("f.json", src, &["patch", "f.json", ops]),
            want,
            "-i {ops}"
        );
    }
}
