//! Where a parse error is: the message has no position, so the terminal
//! gets it as a second line, `  --> FILE:LINE:COL`, and the bulk summary,
//! one line per file, as ` (line LINE, column COL)`. A jq compile error is
//! about the expression, not a place in the file, and gets neither. What
//! stands below the second line is the source line with a caret:
//! `cli_parse_error_caret.rs`.

mod common;

use std::path::Path;

use assert_cmd::Command;

/// Line 3, column 8 of this is the unclosed bracket, line 4, column 2 the
/// colon that cannot follow its items.
const BAD_YAML: &str = "a: 1\nb: 2\nitems: [1, 2\nc: 3\n";
const BAD_YAML_MESSAGE: &str =
    "did not find expected ',' or ']', while parsing a flow sequence at line 3 column 8";

fn dq(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd.current_dir(dir).arg("--no-color");
    cmd
}

/// Run `dq --no-color ARGS` in `dir` with `stdin`; the exit code, stdout
/// and stderr, without the source line of a parse error.
fn run(dir: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let out = dq(dir)
        .args(args)
        .write_stdin(stdin)
        .output()
        .expect("run dq");
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        common::without_source_block(&String::from_utf8(out.stderr).expect("UTF-8 stderr")),
    )
}

/// A directory holding `files`.
fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("seed");
    }
    dir
}

#[test]
fn a_parse_error_names_its_position_on_a_second_line() {
    let dir = dir_with(&[("bad.yaml", BAD_YAML)]);
    let want = format!("parse error in bad.yaml: {BAD_YAML_MESSAGE}\n  --> bad.yaml:4:2\n");
    for args in [
        &["get", "bad.yaml", "/a"][..],
        &["query", ".a", "bad.yaml"],
        &["lint", "bad.yaml"],
        &["fmt", "bad.yaml"],
        &["set", "bad.yaml", "/a", "1"],
        // Was: `parse error: …`, naming no file.
        &["set", "bad.yaml", "--jq", ".a"],
        &["-i", "set", "bad.yaml", "--jq", ".a"],
    ] {
        assert_eq!(
            run(dir.path(), args, ""),
            (3, String::new(), want.clone()),
            "{args:?}"
        );
    }
}

#[test]
fn a_parse_error_of_stdin_is_placed_in_stdin() {
    let dir = dir_with(&[]);
    for (args, named) in [
        (&["get", "-", "/a"][..], ""),
        (&["set", "-", "--jq", ".a"], ""),
        // An edit names stdin `-`.
        (&["set", "-", "/a", "1"], " in -"),
    ] {
        let args = [args, &["--input-format", "yaml"]].concat();
        assert_eq!(
            run(dir.path(), &args, BAD_YAML),
            (
                3,
                String::new(),
                format!("parse error{named}: {BAD_YAML_MESSAGE}\n  --> <stdin>:4:2\n"),
            ),
            "{args:?}"
        );
    }
}

#[test]
fn the_bulk_summary_ends_a_parse_error_with_its_position() {
    for edit in [&["/a", "5"][..], &["--jq", ".a = 5"]] {
        let dir = dir_with(&[("bad.yaml", BAD_YAML), ("good.yaml", "a: 1\n")]);
        let args = [&["-i", "--continue-on-error", "set", "*.yaml"], edit].concat();
        let (code, stdout, _) = run(dir.path(), &args, "");
        assert_eq!(code, 7, "{args:?}");
        assert_eq!(
            stdout,
            format!(
                "Modified: 1, Skipped: 0, Failed: 1\n  - bad.yaml: parse error in bad.yaml: \
                 {BAD_YAML_MESSAGE} (line 4, column 2)\n"
            ),
            "{args:?}"
        );
    }
}

#[test]
fn a_jq_compile_error_has_no_position_in_the_file() {
    let dir = dir_with(&[("ok.json", "{\"a\": 1}\n")]);
    for args in [
        &["query", ".a.", "ok.json"][..],
        // An undefined function: past the start of the expression.
        &["query", ".a | nope", "ok.json"],
        &["set", "ok.json", "--jq", ".a."],
    ] {
        let (code, stdout, stderr) = run(dir.path(), args, "");
        assert_eq!((code, stdout.as_str()), (3, ""), "{args:?}");
        assert!(
            stderr.starts_with("parse error in ok.json: jq: ") && stderr.lines().count() == 1,
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("-->"), "{args:?}: {stderr}");
    }
}

/// A line break in the file's name cannot start a line of its own.
#[test]
fn the_position_line_holds_the_file_name_on_one_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = "n\n\u{1b}l.json";
    if std::fs::write(dir.path().join(name), "{").is_err() {
        eprintln!("skipped: the file system refuses control characters in names");
        return;
    }
    let (code, _, stderr) = run(dir.path(), &["get", name, "/a"], "");
    assert_eq!(code, 3, "{stderr}");
    assert_eq!(
        stderr.lines().nth(1),
        Some("  --> n\\x0a\\x1bl.json:1:2"),
        "{stderr}"
    );
}

/// A line break in what the file holds, quoted in the message, cannot start
/// a line of its own, so the position is the line after the message.
#[test]
fn a_message_quoting_a_line_break_of_the_file_stays_on_one_line() {
    let dir = dir_with(&[("b.xml", "<r>\n </b\n  <c/>\n")]);
    let (code, stdout, stderr) = run(dir.path(), &["get", "b.xml", "/a"], "");
    assert_eq!((code, stdout.as_str()), (3, ""), "{stderr}");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            "parse error in b.xml: XML parse error: closing tag </b\\x0a  <c/> does not match <r>",
            "  --> b.xml:2:2",
        ]
    );
}

/// The position stays the second line, the source line and the caret below
/// it, when a backtrace follows.
#[test]
fn the_position_comes_before_a_backtrace() {
    let dir = dir_with(&[("bad.yaml", BAD_YAML)]);
    let out = dq(dir.path())
        .env("RUST_BACKTRACE", "1")
        .args(["get", "bad.yaml", "/a"])
        .output()
        .expect("run dq");
    let stderr = String::from_utf8(out.stderr).expect("UTF-8 stderr");
    assert!(
        stderr.starts_with(&format!(
            "parse error in bad.yaml: {BAD_YAML_MESSAGE}\n  --> bad.yaml:4:2\n   |\n 4 | c: 3\n   \
             |  ^\n\nStack backtrace:\n"
        )),
        "{stderr}"
    );
}

/// The line and the column of the `  --> FILE:LINE:COL` line of `stderr`,
/// which must be its second, for `file`.
fn arrow(stderr: &str, file: &str) -> (u64, u64) {
    let line = stderr.lines().nth(1).unwrap_or_default();
    let place = line
        .strip_prefix(&format!("  --> {file}:"))
        .unwrap_or_else(|| panic!("no position of {file} on the second line: {stderr}"));
    let (line, col) = place.split_once(':').expect("LINE:COL");
    (
        line.parse().expect("a line number"),
        col.parse().expect("a column number"),
    )
}

/// `(line, col, message)` of every `rule_id` diagnostic of the report
/// `stdout`.
fn diagnostics(stdout: &str, rule_id: &str) -> Vec<(u64, u64, String)> {
    let report: serde_json::Value =
        serde_json::from_str(stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"));
    report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["rule_id"] == rule_id)
        .map(|d| {
            (
                d["line"].as_u64().expect("line"),
                d["col"].as_u64().expect("col"),
                d["message"].as_str().expect("message").to_owned(),
            )
        })
        .collect()
}

/// A rule of no finding, for `check`.
const RULE: &str = "id: t.none\ndescription: d\nseverity: error\nmatch:\n  format: yaml\n\
                    check:\n  jq: 'empty'\n  message: m\n";

/// A template block is longer, or shorter, than the placeholder the parser
/// reads in its place: the position is the file's all the same.
#[test]
fn a_parse_error_read_with_raw_template_strings_is_placed_in_the_file() {
    let text = "name: {{ .Release.Name }}-{{ .x }}\nя: \"{{ .Values.image }}\" x: 1\n";
    let dir = dir_with(&[("t.yaml", text)]);
    let want = "parse error in t.yaml: did not find expected key, while parsing a block mapping\n  \
                --> t.yaml:2:27\n";
    for args in [
        &["get", "t.yaml", "/name"][..],
        &["query", ".name", "t.yaml"],
        &["convert", "-F", "json", "t.yaml"],
        &["lint", "t.yaml"],
        &["set", "t.yaml", "/name", "x"],
        &["del", "t.yaml", "/name"],
        &["merge", "t.yaml", "{\"name\": 1}"],
    ] {
        let args = [&["--raw-template-strings"], args].concat();
        assert_eq!(
            run(dir.path(), &args, ""),
            (3, String::new(), want.to_owned()),
            "{args:?}"
        );
    }
    let (code, _, stderr) = run(
        dir.path(),
        &["--raw-template-strings", "validate", "-F", "json", "t.yaml"],
        "",
    );
    assert_eq!(code, 4, "{stderr}");
    assert_eq!(
        diagnostics(&stderr, "validate.parse"),
        [(
            2,
            27,
            "did not find expected key, while parsing a block mapping".to_owned()
        )]
    );
    // Without the flag the templates are what is wrong, not the syntax.
    let (code, _, stderr) = run(dir.path(), &["get", "t.yaml", "/name"], "");
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.starts_with("file contains Go template syntax (line 1)") && !stderr.contains("-->"),
        "{stderr}"
    );
}

/// Every bulk edit: the good files are written, the malformed one is not,
/// and its line of the summary ends with the position.
#[test]
fn every_bulk_edit_ends_a_parse_error_with_its_position() {
    for (edit, good) in [
        (&["del", "*.yaml", "/a"][..], "b:   2\n"),
        (
            &[
                "patch",
                "*.yaml",
                r#"[{"op": "replace", "path": "/a", "value": 9}]"#,
            ],
            "a:   9\nb:   2\n",
        ),
        (&["merge", "*.yaml", r#"{"a": 9}"#], "a:   9\nb:   2\n"),
        (&["fmt", "*.yaml"], "a: 1\nb: 2\n"),
    ] {
        let dir = dir_with(&[
            ("a-good.yaml", "a:   1\nb:   2\n"),
            ("bad.yaml", BAD_YAML),
            ("c-good.yaml", "a:   1\nb:   2\n"),
        ]);
        let args = [&["-i", "--continue-on-error"], edit].concat();
        let (code, stdout, stderr) = run(dir.path(), &args, "");
        assert_eq!(
            (code, stdout),
            (
                7,
                format!(
                    "Modified: 2, Skipped: 0, Failed: 1\n  - bad.yaml: parse error in bad.yaml: \
                     {BAD_YAML_MESSAGE} (line 4, column 2)\n"
                )
            ),
            "{args:?}: {stderr}"
        );
        for (name, want) in [
            ("a-good.yaml", good),
            ("bad.yaml", BAD_YAML),
            ("c-good.yaml", good),
        ] {
            let now = std::fs::read_to_string(dir.path().join(name)).expect("read");
            assert_eq!(now, want, "{args:?}: {name}");
        }
    }
}

/// The summary of a parallel run is the one of a serial run, each file with
/// its own position.
#[test]
fn a_parallel_bulk_run_has_the_same_summary() {
    let summary = |parallel: &[&str]| {
        let dir = dir_with(&[
            ("a.yaml", "a: 1\nя: @\n"),
            ("b.json", "{\"a\": 1,\n \"b\": x}\n"),
            ("c.yaml", "a:   1\n"),
            ("d.toml", "a = 1\nb = @\n"),
            ("e.yaml", BAD_YAML),
        ]);
        let args = [
            &["-i", "--continue-on-error"],
            parallel,
            &["set", "[a-e].*", "/a", "5"],
        ]
        .concat();
        let (code, stdout, stderr) = run(dir.path(), &args, "");
        assert_eq!(code, 7, "{args:?}: {stderr}");
        stdout
    };
    let serial = summary(&[]);
    assert_eq!(
        serial,
        format!(
            "Modified: 1, Skipped: 0, Failed: 4\n  \
             - a.yaml: parse error in a.yaml: found character that cannot start any token, while \
             scanning for the next token (line 2, column 5)\n  \
             - b.json: parse error in b.json: expected JSON value (object, array, scalar) \
             (line 2, column 7)\n  \
             - d.toml: parse error in d.toml: string values must be quoted, expected literal \
             string (line 2, column 5)\n  \
             - e.yaml: parse error in e.yaml: {BAD_YAML_MESSAGE} (line 4, column 2)\n"
        )
    );
    for jobs in ["1", "2", "4"] {
        assert_eq!(summary(&["--parallel", jobs]), serial, "{jobs} jobs");
    }
}

/// `lint` and `check` report the file they cannot parse as `lint.parse`, at
/// the place the error on stderr has, for every format.
#[test]
fn the_lint_parse_diagnostic_is_at_the_position_of_the_error() {
    let utf16 = |text: &str| -> Vec<u8> {
        std::iter::once(0xFEFF)
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    for (name, bytes, place) in [
        ("x.yaml", BAD_YAML.into(), (4, 2)),
        ("x.json", "{\"я\": 1, x}\n".into(), (1, 11)),
        ("x.jsonl", "{\"a\": 1}\n{\"я\":,}\n".into(), (2, 7)),
        ("x.toml", "a = 1\n\"я\" = @\n".into(), (2, 8)),
        ("x.xml", "<a>\r\n я</c>\r\n</a>\r\n".into(), (2, 4)),
        ("y.xml", utf16("<a>\n я</c>\n</a>\n"), (2, 4)),
        ("x.tf", "a = 1\nb = \"я\" x\n".into(), (2, 10)),
        ("x.csv", "a,b\r\n1,2\r\n\r\n3\r\n".into(), (4, 1)),
        ("x.tsv", "a\tb\n1\tя\t2\n".into(), (2, 1)),
        ("x.env", "A=1\nB=\"я\n".into(), (2, 3)),
        ("x.ini", "[s]\n  = 2\n".into(), (2, 3)),
        ("Dockerfile", "FROM a\nFRUM b\n".into(), (2, 1)),
        ("x.md", "---\nt: я\nb: @\n---\n# T\n".into(), (3, 4)),
        (".gitignore", b"a\nb\xFF\n".to_vec(), (2, 2)),
        ("z.json", b"{\"a\":\n\"\xFF\"}".to_vec(), (2, 2)),
    ] {
        let bytes: Vec<u8> = bytes;
        let dir = dir_with(&[]);
        std::fs::write(dir.path().join(name), bytes).expect("seed");
        for command in [&["lint"][..], &["check", "--inline", RULE]] {
            let args = [command, &["-F", "json", name]].concat();
            let (code, stdout, stderr) = run(dir.path(), &args, "");
            assert_eq!(code, 3, "{args:?}: {stderr}");
            assert_eq!(arrow(&stderr, name), place, "{args:?}: {stderr}");
            let found = diagnostics(&stdout, "lint.parse");
            let [(line, col, message)] = found.as_slice() else {
                panic!("{args:?}: one lint.parse diagnostic: {stdout}");
            };
            assert_eq!((*line, *col), place, "{args:?}: {stdout}");
            assert_eq!(
                stderr.lines().next(),
                Some(format!("parse error in {name}: {message}").as_str()),
                "{args:?}"
            );
        }
    }
    // A file that parses has none.
    let dir = dir_with(&[("ok.yaml", "a: 1\n")]);
    let args = ["check", "--inline", RULE, "-F", "json", "ok.yaml"];
    let (code, stdout, stderr) = run(dir.path(), &args, "");
    assert_eq!((code, diagnostics(&stdout, "lint.parse")), (0, vec![]));
    assert!(!stderr.contains("-->"), "{stderr}");
}

/// Several files that do not parse: one `lint.parse` diagnostic each, at its
/// own place; the error is the first one's.
#[test]
fn every_file_that_does_not_parse_has_its_own_position_in_the_report() {
    let dir = dir_with(&[
        ("a.json", "{\"a\": 1,\n \"b\": x}\n"),
        ("b.yaml", "a: 1\n"),
        ("c.yaml", BAD_YAML),
    ]);
    let args = [
        "check", "--inline", RULE, "-F", "json", "a.json", "b.yaml", "c.yaml",
    ];
    let (code, stdout, stderr) = run(dir.path(), &args, "");
    assert_eq!(code, 3, "{stderr}");
    let places: Vec<_> = diagnostics(&stdout, "lint.parse")
        .into_iter()
        .map(|(line, col, _)| (line, col))
        .collect();
    assert_eq!(places, [(2, 7), (4, 2)], "{stdout}");
    assert!(stderr.contains("  --> a.json:2:7\n"), "{stderr}");
}

/// The file of `diff` that does not parse is the one named, first or second.
#[test]
fn diff_names_the_file_that_does_not_parse() {
    let dir = dir_with(&[
        ("good.yaml", "a: 1\n"),
        ("bad.yaml", BAD_YAML),
        ("bad.json", "{\"я\": 1, x}\n"),
    ]);
    let yaml = format!("parse error in bad.yaml: {BAD_YAML_MESSAGE}\n  --> bad.yaml:4:2\n");
    let json = "parse error in bad.json: expected string key in object\n  --> bad.json:1:11\n";
    for (files, want) in [
        (["good.yaml", "bad.yaml"], yaml.as_str()),
        (["bad.yaml", "good.yaml"], &yaml),
        (["good.yaml", "bad.json"], json),
    ] {
        let args = ["diff", files[0], files[1]];
        assert_eq!(
            run(dir.path(), &args, ""),
            (3, String::new(), want.to_owned()),
            "{args:?}"
        );
    }
}

/// A `--value-from` file that does not parse is the one named, not the file
/// to edit, which stays as it is.
#[test]
fn set_names_the_value_file_that_does_not_parse() {
    let dir = dir_with(&[
        ("good.yaml", "a: 1\n"),
        ("other.yaml", "a: 2\n"),
        ("value.json", "{\"я\": 1,\n \"b\": x}\n"),
    ]);
    let want = "parse error in value.json: expected JSON value (object, array, scalar)\n  \
                --> value.json:2:7\n";
    for args in [
        &["set", "good.yaml", "/a", "--value-from", "value.json"][..],
        &["-i", "set", "good.yaml", "/a", "--value-from", "value.json"],
        &[
            "-i",
            "--continue-on-error",
            "set",
            "*.yaml",
            "/a",
            "--value-from",
            "value.json",
        ],
    ] {
        assert_eq!(
            run(dir.path(), args, ""),
            (3, String::new(), want.to_owned()),
            "{args:?}"
        );
        for (name, text) in [("good.yaml", "a: 1\n"), ("other.yaml", "a: 2\n")] {
            let now = std::fs::read_to_string(dir.path().join(name)).expect("read");
            assert_eq!(now, text, "{args:?}: {name}");
        }
    }
    // One that parses is used.
    std::fs::write(dir.path().join("value.json"), "[1]").expect("seed");
    let (code, stdout, stderr) = run(
        dir.path(),
        &["set", "good.yaml", "/a", "--value-from", "value.json"],
        "",
    );
    assert_eq!((code, stdout.as_str()), (0, "a: [1]\n"), "{stderr}");
}

/// A composite rule parses a part of the file: the finding is at the place
/// in the file, its column in bytes, past front matter and a list's indent.
#[test]
fn a_parse_error_inside_an_inner_document_is_placed_in_the_outer_file() {
    let rule = "markdown.code-blocks-yaml-valid.parse-failed";
    for line_break in ["\n", "\r\n"] {
        let text = "---\ntitle: я\n---\n# Я\n\n- я\n\n  ```yaml\n  я: ok\n  б: \"я\" x: 1\n  ```\n"
            .replace('\n', line_break);
        let dir = dir_with(&[("m.md", text.as_str())]);
        let (code, stdout, stderr) = run(dir.path(), &["lint", "-F", "json", "m.md"], "");
        assert_eq!(code, 4, "{stderr}");
        let found = diagnostics(&stdout, rule);
        let [(line, col, message)] = found.as_slice() else {
            panic!("one finding: {stdout}");
        };
        // The `x` of `  б: "я" x: 1`.
        assert_eq!((*line, *col), (10, 12), "{stdout}");
        assert!(
            message.ends_with(
                "yaml parse failed: parse error: did not find expected key, while parsing a \
                 block mapping"
            ),
            "{message}"
        );
        // The file itself parses.
        assert!(!stderr.contains("-->"), "{stderr}");
    }
}

/// The console prints the first file that does not parse as the error, and
/// the others as warnings, one line each: every one with its position.
#[test]
fn a_console_lint_of_several_files_names_the_position_of_each() {
    let dir = dir_with(&[
        ("a.json", "{\"a\": 1,\n \"b\": x}\n"),
        ("b.yaml", "a: 1\n"),
        ("c.yaml", BAD_YAML),
        ("d.toml", "a = 1\n\"я\" = @\n"),
    ]);
    let files = ["a.json", "b.yaml", "c.yaml", "d.toml"];
    for command in [&["lint"][..], &["check", "--inline", RULE]] {
        let args = [command, &files].concat();
        let (code, _, stderr) = run(dir.path(), &args, "");
        assert_eq!(code, 3, "{args:?}: {stderr}");
        let warnings: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.split_once(" WARN ").map(|(_, text)| text.trim_start()))
            .collect();
        assert_eq!(
            warnings,
            [
                format!("parse error in c.yaml: {BAD_YAML_MESSAGE} (line 4, column 2)"),
                "parse error in d.toml: string values must be quoted, expected literal string \
                 (line 2, column 8)"
                    .to_owned(),
            ],
            "{args:?}"
        );
        assert!(
            stderr.ends_with(
                "parse error in a.json: expected JSON value (object, array, scalar)\n  \
                 --> a.json:2:7\n"
            ),
            "{args:?}: {stderr}"
        );
    }
}

/// A fixture input that does not parse: the case names the position in it.
#[test]
fn a_rule_test_input_that_does_not_parse_names_its_position() {
    let cases = "tests:\n  - name: bad\n    input: |\n      a: 1\n      я: [1, 2\n      \
                 c: 3\n    expected:\n      violations: []\n";
    let dir = dir_with(&[("none.yml", RULE), ("none.test.yml", cases)]);
    let (code, stdout, stderr) = run(dir.path(), &["test", "."], "");
    assert_eq!(code, 4, "{stderr}");
    assert!(
        stdout.contains(
            "none.test.yml: bad (yaml parse error: parse error: did not find expected ',' or \
             ']', while parsing a flow sequence at line 2 column 5 (line 3, column 2))\n"
        ),
        "{stdout}"
    );
}

/// The place a YAML message names for its context is one in the file, not in
/// the text a template block was replaced in.
#[test]
fn a_context_read_with_raw_template_strings_is_placed_in_the_file() {
    let dir = dir_with(&[("t.yaml", "\"{{ .Values.long.path }}\": [1,\nb: 2\n")]);
    let message =
        "did not find expected ',' or ']', while parsing a flow sequence at line 1 column 28";
    for args in [&["get", "t.yaml", "/b"][..], &["lint", "t.yaml"]] {
        let args = [&["--raw-template-strings"], args].concat();
        let (code, _, stderr) = run(dir.path(), &args, "");
        assert_eq!(code, 3, "{args:?}: {stderr}");
        assert_eq!(
            stderr,
            format!("parse error in t.yaml: {message}\n  --> t.yaml:3:1\n"),
            "{args:?}"
        );
    }
    let args = ["--raw-template-strings", "lint", "-F", "json", "t.yaml"];
    let (_, stdout, _) = run(dir.path(), &args, "");
    assert_eq!(
        diagnostics(&stdout, "lint.parse"),
        [(3, 1, message.to_owned())]
    );
}

/// A composite rule's finding names the context of a YAML error at its place
/// in the outer file, or without a place where the inner text does not stand
/// in the file as it is.
#[test]
fn a_context_in_an_inner_document_is_placed_in_the_outer_file_or_nowhere() {
    let context = "did not find expected ',' or ']', while parsing a flow sequence";
    let message = |stdout: &str, rule: &str| {
        let found = diagnostics(stdout, rule);
        let [(_, _, message)] = found.as_slice() else {
            panic!("one finding: {stdout}");
        };
        message.clone()
    };
    for (text, place) in [
        (
            "# T\n\ntext\n\n```yaml\na: 1\nb: [1, 2\nc: 3\n```\n",
            " at line 7 column 4",
        ),
        (
            "---\nt: я\n---\n- я\n\n  ```yaml\n  a: 1\n  я: [1, 2\n  c: 3\n  ```\n",
            " at line 8 column 7",
        ),
        // In a block quote: dq does not know the columns.
        ("> ```yaml\n> a: 1\n> b: [1, 2\n> c: 3\n> ```\n", ""),
    ] {
        for text in [text.to_owned(), text.replace('\n', "\r\n")] {
            let dir = dir_with(&[("m.md", &text)]);
            let (code, stdout, stderr) = run(dir.path(), &["lint", "-F", "json", "m.md"], "");
            assert_eq!(code, 4, "{stderr}");
            let message = message(&stdout, "markdown.code-blocks-yaml-valid.parse-failed");
            assert!(
                message.ends_with(&format!("parse error: {context}{place}")),
                "{text:?}: {message}"
            );
        }
    }
    let rule = "id: t.c\ndescription: d\nseverity: error\nmatch:\n  format: [yaml, json]\n\
                check:\n  extract: '[{value: .s, format: \"yaml\", anchor: \"/s\"}]'\n  nested:\n    \
                id: t.n\n    description: d\n    severity: error\n    match:\n      \
                format: yaml\n    check:\n      jq: 'empty'\n      message: m\n  message: bad\n";
    for (name, text) in [
        ("b.yaml", "s: |\n    a: 1\n    b: [1, 2\n    c: 3\n"),
        ("q.yaml", "s: \"a: 1\\nb: [1, 2\\nc: 3\\n\"\n"),
        ("q.json", "{\"s\": \"a: 1\\nb: [1, 2\\nc: 3\\n\"}\n"),
    ] {
        let dir = dir_with(&[(name, text)]);
        let args = ["check", "--inline", rule, "-F", "json", name];
        let (code, stdout, stderr) = run(dir.path(), &args, "");
        assert_eq!(code, 4, "{name}: {stderr}");
        let message = message(&stdout, "t.c.parse-failed");
        assert!(
            message.ends_with(&format!("parse error: {context}")),
            "{name}: {message}"
        );
    }
}
