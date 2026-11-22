//! Below the position of a parse error the terminal gets the source line and
//! a caret under the character at fault, on every command: escaped as any
//! other terminal text, coloured as the log is. The layout rules are unit
//! tests of `output::error`.

use std::path::Path;

use assert_cmd::Command;
use pretty_assertions::assert_eq;

fn dq(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd.current_dir(dir);
    cmd
}

/// The exit code, stdout and stderr of `cmd ARGS`.
fn run(cmd: &mut Command, args: &[&str]) -> (i32, String, String) {
    let out = cmd.args(args).output().expect("run dq");
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        String::from_utf8(out.stderr).expect("UTF-8 stderr"),
    )
}

fn dir_with(name: &str, bytes: &[u8]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(name), bytes).expect("seed");
    dir
}

const BAD_JSON: &[u8] = b"{\"a\": 1,\n  \"b\": [1, 2,, 3]\n}\n";
const BAD_JSON_ERROR: &str = "parse error in bad.json: expected JSON value (object, array, scalar)
  --> bad.json:2:14
   |
 2 |   \"b\": [1, 2,, 3]
   |              ^
";

#[test]
fn every_command_prints_the_source_line_and_a_caret() {
    let dir = dir_with("bad.json", BAD_JSON);
    for (args, code) in [
        (&["get", "bad.json", "/a"][..], 3),
        (&["query", ".a", "bad.json"], 3),
        (&["lint", "bad.json"], 3),
        (&["fmt", "bad.json"], 3),
        (&["set", "bad.json", "/a", "1"], 3),
        (&["-i", "set", "bad.json", "--jq", ".a"], 3),
        (&["diff", "bad.json", "bad.json"], 3),
        // Once, though the error is `validate`'s result too.
        (&["validate", "bad.json"], 4),
        (&["validate", "--check", "bad.json"], 4),
    ] {
        // A pipe has no colour, with the flag or without.
        for flags in [&["--no-color"][..], &[]] {
            assert_eq!(
                run(&mut dq(dir.path()), &[flags, args].concat()),
                (code, String::new(), BAD_JSON_ERROR.to_owned()),
                "{flags:?} {args:?}"
            );
        }
    }
}

#[test]
fn stdin_has_the_block_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (args, code) in [(&["get", "-", "/a"][..], 3), (&["validate", "-"], 4)] {
        let out = dq(dir.path())
            .args(args)
            .args(["--input-format", "json"])
            .write_stdin("{\"a\" 1}")
            .output()
            .expect("run dq");
        assert_eq!(out.status.code(), Some(code), "{args:?}");
        assert_eq!(
            String::from_utf8(out.stderr).expect("UTF-8"),
            "parse error: expected ':' after object key\n  --> <stdin>:1:6\n   |\n \
             1 | {\"a\" 1}\n   |      ^\n",
            "{args:?}"
        );
    }
}

/// Only styles are added: `CLICOLOR_FORCE` turns them on where there is no
/// terminal, and `--no-color` and `NO_COLOR` win over it. That they follow
/// stderr, not stdout, is in `cli_stream_colors.rs`: the variable is for both.
#[test]
fn colour_is_forced_by_the_variable_and_refused_by_the_flag_and_no_color() {
    let dir = dir_with("bad.json", BAD_JSON);
    let coloured = "parse error in bad.json: expected JSON value (object, array, scalar)
\u{1b}[2m  --> bad.json:2:14\u{1b}[0m
\u{1b}[2m   |\u{1b}[0m
\u{1b}[2m 2 |\u{1b}[0m   \"b\": [1, 2,, 3]
\u{1b}[2m   |\u{1b}[0m              \u{1b}[1;31m^\u{1b}[0m
";
    for args in [&["get", "bad.json", "/a"][..], &["validate", "bad.json"]] {
        let force = |extra: &[&str], no_color: bool| {
            let mut cmd = dq(dir.path());
            cmd.env("CLICOLOR_FORCE", "1");
            if no_color {
                cmd.env("NO_COLOR", "1");
            }
            run(&mut cmd, &[extra, args].concat()).2
        };
        assert_eq!(force(&[], false), coloured, "{args:?}");
        assert_eq!(force(&["--no-color"], false), BAD_JSON_ERROR, "{args:?}");
        assert_eq!(force(&[], true), BAD_JSON_ERROR, "{args:?}");
    }
}

/// ESC and U+202E on the line at fault print as four and six characters, and
/// the caret moves by them; in colour too, no control of the file is raw.
#[test]
fn controls_of_the_line_are_escaped_and_the_caret_moves() {
    let dir = dir_with("bad.tf", "a = 1\nb = \"\u{1b}[2J\u{202e}\" x\n".as_bytes());
    let (code, _, stderr) = run(&mut dq(dir.path()), &["--no-color", "get", "bad.tf", "/a"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.ends_with(
            "\n  --> bad.tf:2:15\n   |\n 2 | b = \"\\x1b[2J\\u202e\" x\n   \
             |                     ^\n"
        ),
        "{stderr}"
    );
    let dir = dir_with("bad.json", b"[\"\x7f\xe2\x80\xae\", \x1b]\n");
    let (code, _, stderr) = run(&mut dq(dir.path()), &["get", "bad.json", "/0"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.ends_with(
            "\n  --> bad.json:1:10\n   |\n 1 | [\"\\x7f\\u202e\", \\x1b]\n   |                ^\n"
        ),
        "{stderr}"
    );
    let mut forced = dq(dir.path());
    forced.env("CLICOLOR_FORCE", "1");
    let (_, _, stderr) = run(&mut forced, &["get", "bad.json", "/0"]);
    let plain = ["\u{1b}[0m", "\u{1b}[2m", "\u{1b}[1;31m"]
        .iter()
        .fold(stderr.clone(), |text, style| text.replace(style, ""));
    assert!(
        !plain.contains(['\u{1b}', '\u{7f}', '\u{202e}']) && stderr != plain,
        "{stderr:?}"
    );
}

/// The last line may be empty, the line number wide, the line long: what is
/// printed stays a block.
#[test]
fn the_end_of_a_file_and_a_long_line_are_shown() {
    let dir = dir_with("bad.yaml", b"a: [1, 2\n");
    let (code, _, stderr) = run(&mut dq(dir.path()), &["get", "bad.yaml", "/a"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.ends_with("\n  --> bad.yaml:2:1\n   |\n 2 |\n   | ^\n"),
        "{stderr}"
    );

    let mut text = "a: 1\n".repeat(1233).replace('a', "#");
    text.push_str("b: @\n");
    let dir = dir_with("bad.yaml", text.as_bytes());
    let (code, _, stderr) = run(&mut dq(dir.path()), &["get", "bad.yaml", "/b"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.ends_with("\n  --> bad.yaml:1234:4\n      |\n 1234 | b: @\n      |    ^\n"),
        "{stderr}"
    );

    let items = "1, ".repeat(500);
    let dir = dir_with("bad.json", format!("[{items}x, {items}1]").as_bytes());
    let (code, _, stderr) = run(&mut dq(dir.path()), &["get", "bad.json", "/0"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(
        stderr.ends_with(&format!(
            "\n  --> bad.json:1:1502\n   |\n 1 | …{}x{},…\n   | {}^\n",
            &"1, ".repeat(20)[1..],
            ", 1".repeat(19),
            " ".repeat(60)
        )),
        "{stderr}"
    );
}

/// An error whose cause says nothing new prints no `Caused by:`.
#[test]
fn an_io_error_is_printed_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (code, stdout, stderr) = run(&mut dq(dir.path()), &["get", "missing.json", "/a"]);
    assert_eq!((code, stdout.as_str()), (5, ""), "{stderr}");
    assert!(
        stderr.starts_with("I/O error reading 'missing.json': "),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}

/// A refused template and a jq expression have no block (yet).
#[test]
fn other_errors_have_no_block() {
    let dir = dir_with("t.yaml", b"a: {{ .x }}\n");
    let (code, _, stderr) = run(&mut dq(dir.path()), &["get", "t.yaml", "/a"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(!stderr.contains("-->") && !stderr.contains('^'), "{stderr}");
    let dir = dir_with("ok.json", b"{}");
    let (code, _, stderr) = run(&mut dq(dir.path()), &["query", ".a.", "ok.json"]);
    assert_eq!((code, stderr.lines().count()), (3, 1), "{stderr}");
}

/// `get` of a file that does not parse prints exactly `want`.
fn assert_get(name: &str, bytes: &[u8], want: &str) {
    let dir = dir_with(name, bytes);
    assert_eq!(
        run(&mut dq(dir.path()), &["--no-color", "get", name, "/a"]),
        (3, String::new(), want.to_owned()),
        "{name}: {bytes:?}"
    );
}

/// `text` in UTF-16, `big` endian or not, behind the BOM of that order.
fn utf16(text: &str, big: bool) -> Vec<u8> {
    std::iter::once(0xFEFF)
        .chain(text.encode_utf16())
        .flat_map(|unit| {
            if big {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            }
        })
        .collect()
}

/// The formats the other tests here do not show. Front matter is placed in
/// the file around it.
#[test]
fn every_format_shows_its_source_line() {
    for (name, bytes, want) in [
        (
            "bad.ini",
            &b"[s]\nk = 1\n  = 2\n"[..],
            "parse error in bad.ini: missing key
  --> bad.ini:3:3
   |
 3 |   = 2
   |   ^
",
        ),
        (
            "bad.env",
            b"A=1\nB=\"abc\nC=2\n",
            "parse error in bad.env: unterminated double-quoted value
  --> bad.env:2:3
   |
 2 | B=\"abc
   |   ^~~~
",
        ),
        (
            "Dockerfile",
            b"FROM a\nFRUM b\n",
            "parse error in Dockerfile: unknown instruction `FRUM`
  --> Dockerfile:2:1
   |
 2 | FRUM b
   | ^~~~~~
",
        ),
        (
            "bad.jsonl",
            b"{\"a\": 1}\n\n{\"b\": x}\n{\"c\": 2}\n",
            "parse error in bad.jsonl: expected JSON value (object, array, scalar)
  --> bad.jsonl:3:7
   |
 3 | {\"b\": x}
   |       ^
",
        ),
        (
            "bad.tsv",
            "a\tb\n1\tя\t2\n".as_bytes(),
            "parse error in bad.tsv: TSV record 0: CSV error: found record with 3 fields, but the previous record has 2 fields
  --> bad.tsv:2:1
   |
 2 | 1    я    2
   | ^
",
        ),
        (
            ".gitignore",
            b"a\nb\xFF!\n",
            "parse error in .gitignore: invalid UTF-8 in ignore-list input: invalid utf-8 sequence of 1 bytes
  --> .gitignore:2:2
   |
 2 | b\u{fffd}!
   |  ^
",
        ),
        (
            "yaml.md",
            "---\nt: я\nb: @\n---\n# T\n".as_bytes(),
            "parse error in yaml.md: found character that cannot start any token, while scanning for the next token
  --> yaml.md:3:4
   |
 3 | b: @
   |    ^
",
        ),
        (
            "toml.md",
            "+++\nt = 1\nb = \"я\" @\n+++\n# T\n".as_bytes(),
            "parse error in toml.md: unexpected key or value, expected newline, `#`
  --> toml.md:3:10
   |
 3 | b = \"я\" @
   |         ^
",
        ),
        (
            "bad.xml",
            b"<?xml version=\"1.0\" encoding=\"windows-1251\"?>\n<a>\n <b>\xFF\xFE</c>\n</a>\n",
            "parse error in bad.xml: XML parse error: closing tag </c> does not match <b>
  --> bad.xml:3:9
   |
 3 |  <b>яю</c>
   |       ^
",
        ),
    ] {
        assert_get(name, bytes, want);
    }
}

/// The line of a UTF-16 file is its text, and the column is in the bytes of
/// that text. The span is in the file's bytes, but every error of such a
/// file has an empty one, so what is under the line is a caret alone. (A
/// span longer than the line ends with it: a property of `output::error`.)
#[test]
fn a_utf16_file_is_shown_as_its_text_under_one_caret() {
    for (big, text, want) in [
        (
            true,
            "<a>\n я😀</c>\n</a>\n",
            "parse error in u.xml: XML parse error: closing tag </c> does not match <a>
  --> u.xml:2:8
   |
 2 |  я😀</c>
   |     ^
",
        ),
        // One construct of several characters at fault, the last thing on
        // the line: the closing tag, an entity, a name in the declaration.
        (
            false,
            "<a>\r\n\t<b>я</cc>\r\n</a>",
            "parse error in u.xml: XML parse error: closing tag </cc> does not match <b>
  --> u.xml:2:7
   |
 2 |     <b>я</cc>
   |         ^
",
        ),
        (
            false,
            "<a>\n&я;",
            "parse error in u.xml: XML parse error: undeclared entity reference '&я;'
  --> u.xml:2:1
   |
 2 | &я;
   | ^
",
        ),
        (
            true,
            "<?xml version=\"1.0\" encoding=\"nope\"?><a/>",
            "parse error in u.xml: XML parse error: the XML declaration's encoding \"nope\" is one dq does not know
  --> u.xml:1:31
   |
 1 | <?xml version=\"1.0\" encoding=\"nope\"?><a/>
   |                               ^
",
        ),
    ] {
        assert_get("u.xml", &utf16(text, big), want);
    }
    // A malformed sequence: half a surrogate pair, half a unit at the end.
    let mut lone = utf16("<a>\n я", false);
    lone.extend(b"\x00\xD8<\0");
    assert_get(
        "u.xml",
        &lone,
        "parse error in u.xml: invalid UTF-16LE in XML input
  --> u.xml:2:4
   |
 2 |  я\u{fffd}<
   |   ^
",
    );
    let mut half = utf16("<a/>", false);
    half.push(b'z');
    assert_get(
        "u.xml",
        &half,
        "parse error in u.xml: invalid UTF-16LE in XML input
  --> u.xml:1:5
   |
 1 | <a/>\u{fffd}
   |     ^
",
    );
}

/// The line break is no part of the line, whichever it is: a CR of one would
/// print as `\x0d`.
#[test]
fn a_line_break_of_any_kind_is_not_shown() {
    for line_break in ["\n", "\r\n", "\r"] {
        let text = String::from_utf8_lossy(BAD_JSON).replace('\n', line_break);
        assert_get("bad.json", text.as_bytes(), BAD_JSON_ERROR);
        // An underline ends with the line, not with the span.
        let text = "B=\"x\nC=2\n".replace('\n', line_break);
        assert_get(
            "bad.env",
            text.as_bytes(),
            "parse error in bad.env: unterminated double-quoted value
  --> bad.env:1:3
   |
 1 | B=\"x
   |   ^~
",
        );
    }
    // At the line break itself, the second byte of two: one past the line.
    assert_get(
        "bad.toml",
        b"a = \"x\r\nb = 1\r\n",
        "parse error in bad.toml: invalid basic string, expected `\"`
  --> bad.toml:1:8
   |
 1 | a = \"x
   |       ^
",
    );
}

#[test]
fn an_empty_file_has_an_empty_row() {
    assert_get(
        "e.json",
        b"",
        "parse error in e.json: EOF while parsing a value
  --> e.json:1:1
   |
 1 |
   | ^
",
    );
    assert_get(
        "e.xml",
        b"",
        "parse error in e.xml: XML parse error: a document must have exactly one root element; found 0
  --> e.xml:1:1
   |
 1 |
   | ^
",
    );
}

/// A byte order mark is neither shown nor counted.
#[test]
fn a_byte_order_mark_moves_nothing() {
    for (name, text) in [
        ("b.json", "{\"a\" 1}\n"),
        ("b.yaml", "я: @\n"),
        ("b.toml", "a = \"日\" x\n"),
        ("b.xml", "<日>x</c>"),
    ] {
        let without = dir_with(name, text.as_bytes());
        let with = dir_with(name, format!("\u{feff}{text}").as_bytes());
        let args = ["get", name, "/a"];
        let (code, _, want) = run(&mut dq(without.path()), &args);
        assert_eq!(code, 3, "{want}");
        assert!(want.contains("\n 1 | ") && want.ends_with("^\n"), "{want}");
        let got = run(&mut dq(with.path()), &args);
        assert_eq!(got, (3, String::new(), want), "{name}");
    }
}

/// At the end of a file the caret is past the last character, or on the
/// empty line after the last line break.
#[test]
fn an_error_at_the_end_of_a_file_is_past_its_text() {
    let message = "parse error in e.json: expected ',' or '}' in object";
    assert_get(
        "e.json",
        "{\"日\": 1".as_bytes(),
        &format!("{message}\n  --> e.json:1:10\n   |\n 1 | {{\"日\": 1\n   |         ^\n"),
    );
    for line_break in ["\n", "\r\n", "\r"] {
        assert_get(
            "e.json",
            format!("{{\"日\": 1{line_break}").as_bytes(),
            &format!("{message}\n  --> e.json:2:1\n   |\n 2 |\n   | ^\n"),
        );
    }
    let message = "parse error in e.yaml: did not find expected ',' or ']', while parsing a flow \
                   sequence at line 1 column 4";
    assert_get(
        "e.yaml",
        b"a: [1",
        &format!("{message}\n  --> e.yaml:1:6\n   |\n 1 | a: [1\n   |      ^\n"),
    );
    assert_get(
        "e.yaml",
        b"a: [1\n",
        &format!("{message}\n  --> e.yaml:2:1\n   |\n 2 |\n   | ^\n"),
    );
}

/// The line shown is the file's, with its templates, though the parser read
/// another text in their place; behind a byte order mark too.
#[test]
fn a_line_with_a_template_before_the_error_is_the_one_of_the_file() {
    for bom in ["", "\u{feff}"] {
        let text = format!("{bom}я: \"{{{{ .Values.long.path }}}}\" x: {{{{ .y }}}}\n");
        let dir = dir_with("t.yaml", text.as_bytes());
        assert_eq!(
            run(
                &mut dq(dir.path()),
                &["--raw-template-strings", "get", "t.yaml", "/a"]
            ),
            (
                3,
                String::new(),
                "parse error in t.yaml: did not find expected key, while parsing a block mapping
  --> t.yaml:1:31
   |
 1 | я: \"{{ .Values.long.path }}\" x: {{ .y }}
   |                              ^
"
                .to_owned()
            )
        );
    }
}

/// The bytes of XML that is not UTF-8 are not its text: the block is the
/// text's, as it is without the flag.
#[test]
fn raw_template_strings_show_the_text_of_xml_that_is_not_utf8() {
    let windows_1251 =
        |body: &[u8]| [b"<?xml version=\"1.0\" encoding=\"windows-1251\"?>\n", body].concat();
    for (bytes, want) in [
        (
            utf16("<a>\n <b>x</c>\n</a>\n", false),
            "parse error in u.xml: XML parse error: closing tag </c> does not match <b>
  --> u.xml:2:6
   |
 2 |  <b>x</c>
   |      ^
",
        ),
        (
            windows_1251(b"<a>\xff\xfe</c>\n"),
            "parse error in u.xml: XML parse error: closing tag </c> does not match <a>
  --> u.xml:2:8
   |
 2 | <a>яю</c>
   |      ^
",
        ),
        // A template before the error, on an earlier line.
        (
            windows_1251(b"<a k=\"{{ .Values.long.path }}\">\n\xff\xfe</c>\n"),
            "parse error in u.xml: XML parse error: closing tag </c> does not match <a>
  --> u.xml:3:5
   |
 3 | яю</c>
   |   ^
",
        ),
    ] {
        let dir = dir_with("u.xml", &bytes);
        for args in [
            &["get", "u.xml", "/a"][..],
            &["set", "u.xml", "/a", "1"],
            &["lint", "u.xml"],
        ] {
            let flags: [&[&str]; 2] = [&["--raw-template-strings"], &["--allow-templates"]];
            for flags in flags {
                assert_eq!(
                    run(&mut dq(dir.path()), &[flags, args].concat()),
                    (3, String::new(), want.to_owned()),
                    "{flags:?} {args:?}"
                );
            }
        }
    }
}

/// Tabs, wide characters and combining marks before the error.
#[test]
fn the_caret_counts_columns_not_bytes() {
    assert_get(
        "bad.tf",
        "\ta = \"日本\t😀e\u{301}\" x\n".as_bytes(),
        "parse error in bad.tf: expected newline or eof
  --> bad.tf:1:23
   |
 1 |     a = \"日本    😀e\u{301}\" x
   |                       ^
",
    );
    // And where the line is cut: 59 columns on either side are 29 wide
    // characters, and one more column to fill up with.
    let wide = "日".repeat(100);
    assert_get(
        "bad.json",
        format!("[\"{wide}\",x,\"{wide}\"]").as_bytes(),
        &format!(
            "parse error in bad.json: expected JSON value (object, array, scalar)
  --> bad.json:1:305
   |
 1 | …{}\",x,\"{}…
   | {}^
",
            "日".repeat(28),
            "日".repeat(28),
            " ".repeat(1 + 2 * 28 + 2),
        ),
    );
}

/// The other commands that read a file, each printing the error of `get`
/// once and nothing on stdout, and leaving the file as it is.
#[test]
fn every_other_command_prints_the_block_once() {
    const BAD: &str = "a: 1\nb: 2\nitems: [1, 2\nc: 3\n";
    const BAD_ERROR: &str = "parse error in bad.yaml: did not find expected ',' or ']', while parsing a flow sequence at line 3 column 8
  --> bad.yaml:4:2
   |
 4 | c: 3
   |  ^
";
    let rule = "{id: t.none, description: d, severity: error, match: {format: yaml}, \
                check: {jq: empty, message: m}}";
    let patch = r#"[{"op": "remove", "path": "/a"}]"#;
    let dir = dir_with("bad.yaml", BAD.as_bytes());
    std::fs::write(dir.path().join("good.yaml"), "a: 1\n").expect("seed");
    for args in [
        &["exists", "bad.yaml", "/a"][..],
        &["keys", "bad.yaml", ""],
        &["values", "bad.yaml", ""],
        &["len", "bad.yaml", ""],
        &["type", "bad.yaml", "/a"],
        &["paths", "bad.yaml"],
        &["select", "bad.yaml", "$.a"],
        &["convert", "-F", "json", "bad.yaml"],
        &["del", "bad.yaml", "/a"],
        &["-i", "del", "bad.yaml", "/a"],
        &["patch", "bad.yaml", patch],
        &["-i", "patch", "bad.yaml", patch],
        &["merge", "bad.yaml", "{\"a\": 2}"],
        &["-i", "merge", "bad.yaml", "{\"a\": 2}"],
        &["diff", "good.yaml", "bad.yaml"],
        &["check", "--inline", rule, "bad.yaml"],
        &["fix", "bad.yaml"],
        &["-i", "fix", "bad.yaml"],
        &["--check", "fmt", "bad.yaml"],
        &["--check", "set", "bad.yaml", "/a", "2"],
        &["--diff", "set", "bad.yaml", "/a", "2"],
    ] {
        assert_eq!(
            run(&mut dq(dir.path()), args),
            (3, String::new(), BAD_ERROR.to_owned()),
            "{args:?}"
        );
        let now = std::fs::read_to_string(dir.path().join("bad.yaml")).expect("read");
        assert_eq!(now, BAD, "{args:?}");
    }
}

/// Files of a bulk run in the order the run has them, three malformed, and
/// the one block of the first.
const BULK: [(&str, &str); 4] = [
    ("a.json", "{\"a\": 1,\n \"b\": x}\n"),
    ("b.yaml", "a:   1\n"),
    ("c.yaml", "a: 1\nя: @\n"),
    ("d.toml", "a = 1\nb = @\n"),
];
const BULK_FIRST: &str = "parse error in a.json: expected JSON value (object, array, scalar)\n  --> a.json:2:7\n   \
     |\n 2 |  \"b\": x}\n   |       ^\n";

fn bulk_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in BULK {
        std::fs::write(dir.path().join(name), text).expect("seed");
    }
    dir
}

/// The edits of a bulk run, and what the summary counts of its one good file.
const EDITS: [(&[&str], &str); 7] = [
    (&["set", "[a-d].*", "/a", "5"], "Modified: 1, Skipped: 0"),
    (
        &["set", "[a-d].*", "--jq", ".a = 5"],
        "Modified: 1, Skipped: 0",
    ),
    (&["del", "[a-d].*", "/a"], "Modified: 1, Skipped: 0"),
    (
        &[
            "patch",
            "[a-d].*",
            r#"[{"op": "replace", "path": "/a", "value": 5}]"#,
        ],
        "Modified: 1, Skipped: 0",
    ),
    (
        &["merge", "[a-d].*", "{\"a\": 5}"],
        "Modified: 1, Skipped: 0",
    ),
    (&["fmt", "[a-d].*"], "Modified: 1, Skipped: 0"),
    (&["fix", "[a-d].*"], "Modified: 0, Skipped: 1"),
];

/// The summary has a line per file, and the error of the run no block.
#[test]
fn a_bulk_run_that_continues_prints_no_block() {
    for (edit, good) in EDITS {
        for parallel in [&[][..], &["--parallel", "4"]] {
            let dir = bulk_dir();
            let args = [&["-i", "--continue-on-error"], parallel, edit].concat();
            assert_eq!(
                run(&mut dq(dir.path()), &args),
                (
                    7,
                    format!(
                        "{good}, Failed: 3\n  \
                         - a.json: parse error in a.json: expected JSON value (object, array, \
                         scalar) (line 2, column 7)\n  \
                         - c.yaml: parse error in c.yaml: found character that cannot start any \
                         token, while scanning for the next token (line 2, column 5)\n  \
                         - d.toml: parse error in d.toml: string values must be quoted, expected \
                         literal string (line 2, column 5)\n"
                    ),
                    "3 file(s) failed in bulk run\n".to_owned()
                ),
                "{args:?}"
            );
        }
    }
}

/// Without the flag the first malformed file stops the run: its block, once.
#[test]
fn a_bulk_run_that_stops_prints_the_block_of_the_first_file() {
    for (edit, _) in EDITS {
        for parallel in [&[][..], &["--parallel", "4"]] {
            for write in [&["-i"][..], &[]] {
                let dir = bulk_dir();
                let args = [write, parallel, edit].concat();
                assert_eq!(
                    run(&mut dq(dir.path()), &args),
                    (3, String::new(), BULK_FIRST.to_owned()),
                    "{args:?}"
                );
            }
        }
    }
}

/// `lint` and `check` warn of the other files, one line each, and end with
/// the block of the first. `-q` leaves the block.
#[test]
fn a_lint_of_several_files_ends_with_one_block() {
    let dir = bulk_dir();
    let rule = "{id: t.none, description: d, severity: error, match: {format: yaml}, \
                check: {jq: empty, message: m}}";
    for command in [&["lint"][..], &["check", "--inline", rule]] {
        let args = [command, &["a.json", "b.yaml", "c.yaml", "d.toml"]].concat();
        let (code, stdout, stderr) = run(&mut dq(dir.path()), &args);
        assert_eq!((code, stdout.as_str()), (3, ""), "{args:?}: {stderr}");
        let (warnings, rest): (Vec<_>, Vec<_>) = stderr
            .split_inclusive('\n')
            .partition(|line| line.contains(" WARN "));
        assert_eq!(rest.concat(), BULK_FIRST, "{args:?}");
        let warnings: Vec<_> = warnings
            .iter()
            .filter_map(|line| line.split_once(" WARN ").map(|(_, text)| text))
            .collect();
        assert_eq!(
            warnings,
            [
                "parse error in c.yaml: found character that cannot start any token, while \
                 scanning for the next token (line 2, column 5)\n",
                "parse error in d.toml: string values must be quoted, expected literal string \
                 (line 2, column 5)\n",
            ],
            "{args:?}"
        );
        assert!(stderr.ends_with(BULK_FIRST), "{args:?}: {stderr}");

        let quiet = [&["-q"][..], &args].concat();
        assert_eq!(
            run(&mut dq(dir.path()), &quiet),
            (3, String::new(), BULK_FIRST.to_owned()),
            "{quiet:?}"
        );
    }
}

/// The first line of `stderr`, which must be its only one, with no block.
fn only_line(stderr: &str) -> &str {
    let line = stderr.strip_suffix('\n').unwrap_or_default();
    assert!(
        !line.is_empty() && !line.contains('\n') && !line.contains("-->"),
        "{stderr:?}"
    );
    line
}

/// Errors whose message holds the text of their cause: an I/O error of a
/// data file, of a rule file and of a schema it names, a write error, a rule
/// that does not read or compile.
#[test]
fn a_cause_the_message_names_is_not_repeated() {
    let dir = dir_with("ok.yaml", b"a: 1\n");
    std::fs::create_dir(dir.path().join("dir.json")).expect("mkdir");
    let rule = |extra: &str, check: &str| {
        format!(
            "{{id: t.r, description: d, severity: error, match: {{format: yaml{extra}}}, \
             check: {{{check}, message: m}}}}"
        )
    };
    let jq = rule("", "jq: .a.");
    let glob = rule(", glob: '[z'", "jq: empty");
    let schema = rule("", "schema_file: nope.json");
    std::fs::write(dir.path().join("schema.yml"), &schema).expect("seed");
    for (args, code, want) in [
        (
            &["get", "dir.json", "/a"][..],
            5,
            "I/O error reading 'dir.json': ",
        ),
        (
            &["-i", "set", "*.toml", "/a", "1"],
            5,
            "I/O error reading '*.toml': glob *.toml matched zero files",
        ),
        (
            &["check", "--inline", "zz: 1", "ok.yaml"],
            3,
            "rule parse error: rule #1 failed to parse: unknown field `zz`",
        ),
        (
            &["check", "--inline", &jq, "ok.yaml"],
            3,
            "rule t.r failed to compile: jq compile error at byte offset 3: ",
        ),
        (
            &["check", "--inline", &glob, "ok.yaml"],
            3,
            "rule t.r has invalid glob pattern: error parsing glob '[z': ",
        ),
        (
            &["check", "--rule", "./schema.yml", "ok.yaml"],
            5,
            "io error reading ",
        ),
    ] {
        let (got, stdout, stderr) = run(&mut dq(dir.path()), args);
        assert_eq!((got, stdout.as_str()), (code, ""), "{args:?}: {stderr}");
        let line = only_line(&stderr);
        assert!(line.starts_with(want), "{args:?}: {stderr}");
        if want == "io error reading " {
            assert!(line.contains("nope.json: "), "{stderr}");
        }
    }
}

/// A write that fails, as `-i` into a directory that cannot be written to.
#[cfg(unix)]
#[test]
fn a_write_error_is_printed_once() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = dir_with("f.yaml", b"a: 1\n");
    let mode = |mode| {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    mode(0o500);
    let refused = std::fs::write(dir.path().join("probe"), "").is_err();
    let (code, stdout, stderr) = run(&mut dq(dir.path()), &["-i", "set", "f.yaml", "/a", "2"]);
    mode(0o700);
    if !refused {
        eprintln!("skipped: a read-only directory can be written to (root)");
        return;
    }
    assert_eq!((code, stdout.as_str()), (7, ""), "{stderr}");
    let line = only_line(&stderr);
    assert!(
        line.starts_with("I/O error writing 'f.yaml': Permission denied"),
        "{stderr}"
    );
    assert_eq!(line.matches("Permission denied").count(), 1, "{stderr}");
}

/// The summary of a bulk run names the cause of an error once too.
#[cfg(unix)]
#[test]
fn a_bulk_summary_names_an_io_cause_once() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = dir_with("a.yaml", b"a: 1\n");
    for (name, text) in [("bad.yaml", "a: 1\nb: @\n"), ("x.yaml", "a: 1\n")] {
        std::fs::write(dir.path().join(name), text).expect("seed");
    }
    let unreadable = dir.path().join("x.yaml");
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read(&unreadable).is_ok() {
        eprintln!("skipped: a file of mode 000 can be read (root)");
        return;
    }
    let (code, stdout, stderr) = run(
        &mut dq(dir.path()),
        &["-i", "--continue-on-error", "set", "*.yaml", "/a", "2"],
    );
    assert_eq!(
        (code, stderr.as_str()),
        (7, "2 file(s) failed in bulk run\n")
    );
    assert_eq!(
        stdout,
        "Modified: 1, Skipped: 0, Failed: 2\n  \
         - bad.yaml: parse error in bad.yaml: found character that cannot start any token, \
         while scanning for the next token (line 2, column 4)\n  \
         - x.yaml: I/O error reading 'x.yaml': Permission denied (os error 13)\n"
    );
}

/// A cause that says more than the message above it stays.
#[test]
fn a_cause_that_says_more_is_printed() {
    let dir = dir_with("ok.yaml", b"a: 1\n");
    let (code, stdout, stderr) = run(&mut dq(dir.path()), &["-i", "set", "[z", "/a", "1"]);
    assert_eq!((code, stdout.as_str()), (1, ""), "{stderr}");
    assert!(
        stderr.starts_with(
            "invalid glob pattern: [z\n\nCaused by:\n    error parsing glob '[z': unclosed"
        ),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 4, "{stderr}");
}

/// Nothing is at fault in a file: no block.
#[test]
fn an_error_that_is_about_no_place_has_no_block() {
    let dir = dir_with("ok.yaml", b"a: 1\n");
    let (code, _, stderr) = run(&mut dq(dir.path()), &["get", "ok.yaml", "/zz"]);
    assert_eq!(
        (code, stderr.as_str()),
        (
            2,
            "path '/zz' not found (matched up to '')\n  did_you_mean: a\n  pointer: /zz\n"
        )
    );
    for (args, code, want) in [
        (
            &["get", "ok.zzz", "/a"][..],
            6,
            "unsupported format 'zzz'\n",
        ),
        (
            &["merge", "ok.yaml", "{"],
            6,
            "invalid JSON merge patch: EOF while parsing an object at line 1 column 1\n",
        ),
        (
            &["--doc", "3", "get", "ok.yaml", "/a"],
            2,
            "--doc 3 is out of range: ok.yaml holds 1 document(s) (--doc 0)\n",
        ),
    ] {
        let (got, _, stderr) = run(&mut dq(dir.path()), args);
        assert_eq!((got, stderr.as_str()), (code, want), "{args:?}");
    }
}

/// `validate` in a format other than the console's is the report it was:
/// no block, no colour, stdout empty.
#[test]
fn a_machine_readable_validate_has_no_block() {
    let dir = dir_with("bad.json", BAD_JSON);
    for (format, want) in [
        (
            "json",
            r#"{
  "diagnostics": [
    {
      "kind": "parse",
      "file": "bad.json",
      "path": "bad.json",
      "line": 2,
      "col": 14,
      "severity": "error",
      "message": "expected JSON value (object, array, scalar)",
      "snippet": "  \"b\": [1, 2,, 3]",
      "rule_id": "validate.parse"
    }
  ]
}
"#,
        ),
        (
            "jsonl",
            r#"{"diagnostics":[{"kind":"parse","file":"bad.json","path":"bad.json","line":2,"col":14,"severity":"error","message":"expected JSON value (object, array, scalar)","snippet":"  \"b\": [1, 2,, 3]","rule_id":"validate.parse"}]}
"#,
        ),
        (
            "yaml",
            r#"diagnostics:
- kind: parse
  file: bad.json
  path: bad.json
  line: 2
  col: 14
  severity: error
  message: expected JSON value (object, array, scalar)
  snippet: '  "b": [1, 2,, 3]'
  rule_id: validate.parse
"#,
        ),
        (
            "toml",
            r#"[[diagnostics]]
kind = "parse"
file = "bad.json"
path = "bad.json"
line = 2
col = 14
severity = "error"
message = "expected JSON value (object, array, scalar)"
snippet = '  "b": [1, 2,, 3]'
rule_id = "validate.parse"
"#,
        ),
        (
            "toon",
            r#"diagnostics[1]{kind,file,path,line,col,severity,message,snippet,rule_id}:
  parse,bad.json,bad.json,2,14,error,"expected JSON value (object, array, scalar)","  \"b\": [1, 2,, 3]",validate.parse
"#,
        ),
        (
            "junit",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="dq-lint" tests="1" failures="1" errors="0" skipped="0">
    <testcase classname="bad.json" name="validate.parse">
      <failure type="error" message="expected JSON value (object, array, scalar)">bad.json:2:14
expected JSON value (object, array, scalar)</failure>
    </testcase>
  </testsuite>
</testsuites>
"#,
        ),
        (
            "tap",
            r#"TAP version 13
1..1
not ok 1 - validate.parse: expected JSON value (object, array, scalar)
  ---
  severity: error
  file: bad.json
  line: 2
  col: 14
  message: "expected JSON value (object, array, scalar)"
  ...
# 1 test, 1 failure
"#,
        ),
    ] {
        for color in ["0", "1"] {
            let mut cmd = dq(dir.path());
            cmd.env("CLICOLOR_FORCE", color);
            assert_eq!(
                run(&mut cmd, &["validate", "-F", format, "bad.json"]),
                (4, String::new(), want.to_owned()),
                "{format}, CLICOLOR_FORCE={color}"
            );
        }
    }
    // SARIF names the version of dq: the report is one JSON document.
    let (code, stdout, stderr) = run(
        &mut dq(dir.path()),
        &["validate", "-F", "sarif", "bad.json"],
    );
    assert_eq!((code, stdout.as_str()), (4, ""), "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stderr).expect("one JSON document");
    let result = &report["runs"][0]["results"][0];
    assert_eq!(
        (
            &result["message"]["text"],
            &result["locations"][0]["physicalLocation"]["region"],
        ),
        (
            &serde_json::json!("expected JSON value (object, array, scalar)"),
            &serde_json::json!({"startLine": 2, "startColumn": 14}),
        )
    );
}
