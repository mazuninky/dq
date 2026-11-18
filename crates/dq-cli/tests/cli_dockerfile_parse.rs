//! The Dockerfile IR as the user sees it through `dq query` / `dq get`.
//!
//! IR: `Array<Map { instruction, arguments, line }>`. `instruction` is the
//! upper-cased keyword; `arguments` is the argument text as written (string)
//! for shell form, an array of strings for JSON-array form, a map for
//! `ENV` / `LABEL` / `ARG`; `line` is the 1-based line where the instruction
//! starts.
//!
//! Every input here either exited 3 or came out wrong under the
//! `dockerfile-parser-rs` crate the dq-owned parser replaced. Inputs are
//! written as bytes so CRLF and a BOM reach the parser untouched.

use assert_cmd::Command;
use predicates::prelude::*;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

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

/// Write `src` to `<tmp>/Dockerfile`.
fn dockerfile(src: &[u8]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Dockerfile"), src).expect("write Dockerfile");
    dir
}

/// The parsed IR of `src`, read back through `dq query -F json .`. Asserts
/// exit 0 and a silent stderr.
fn ir(src: &[u8]) -> Value {
    let dir = dockerfile(src);
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "query", "-F", "json", ".", "Dockerfile"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let stream: Value = serde_json::from_slice(&out).expect("query -F json prints JSON");
    // `query -F json` prints the result stream as an array; `.` yields one.
    let Value::Array(mut results) = stream else {
        panic!("expected the result stream as an array, got {stream}");
    };
    assert_eq!(results.len(), 1, "`.` must yield exactly one result");
    results.remove(0)
}

#[test]
fn healthcheck_onbuild_and_maintainer_parse() {
    assert_eq!(
        ir(b"HEALTHCHECK CMD true\n\
             HEALTHCHECK NONE\n\
             HEALTHCHECK --interval=30s CMD curl -f http://localhost/ || exit 1\n\
             ONBUILD RUN make\n\
             onbuild run make\n\
             MAINTAINER x <y@z>\n"),
        json!([
            {"instruction": "HEALTHCHECK", "arguments": "CMD true", "line": 1},
            {"instruction": "HEALTHCHECK", "arguments": "NONE", "line": 2},
            {
                "instruction": "HEALTHCHECK",
                "arguments": "--interval=30s CMD curl -f http://localhost/ || exit 1",
                "line": 3
            },
            {"instruction": "ONBUILD", "arguments": "RUN make", "line": 4},
            {"instruction": "ONBUILD", "arguments": "run make", "line": 5},
            {"instruction": "MAINTAINER", "arguments": "x <y@z>", "line": 6},
        ])
    );
}

#[test]
fn one_character_arguments_and_lowercase_keywords_parse() {
    assert_eq!(
        ir(b"WORKDIR /\nUSER 0\nEXPOSE 8\nfrom alpine\n"),
        json!([
            {"instruction": "WORKDIR", "arguments": "/", "line": 1},
            {"instruction": "USER", "arguments": "0", "line": 2},
            {"instruction": "EXPOSE", "arguments": "8", "line": 3},
            {"instruction": "FROM", "arguments": "alpine", "line": 4},
        ])
    );
}

#[test]
fn heredoc_body_and_terminator_are_appended_to_the_arguments() {
    assert_eq!(
        ir(b"RUN <<EOT\n\
             echo hi\n\
             EOT\n\
             COPY <<\"EOF\" /x\n\
             # not a comment inside a heredoc\n\
             EOF\n\
             onbuild run <<EOT\n\
             echo on\n\
             EOT\n\
             USER 1\n"),
        json!([
            {"instruction": "RUN", "arguments": "<<EOT\necho hi\nEOT", "line": 1},
            {
                "instruction": "COPY",
                "arguments": "<<\"EOF\" /x\n# not a comment inside a heredoc\nEOF",
                "line": 4
            },
            {"instruction": "ONBUILD", "arguments": "run <<EOT\necho on\nEOT", "line": 7},
            {"instruction": "USER", "arguments": "1", "line": 10},
        ])
    );
}

#[test]
fn escape_directive_makes_backtick_the_continuation_and_backslash_literal() {
    // With `escape=`` a trailing backslash (a Windows path) ends the line.
    assert_eq!(
        ir(b"# escape=`\n\
             RUN echo a `\n  \
             b\n\
             WORKDIR c:\\\n\
             USER 1\n"),
        json!([
            {"instruction": "RUN", "arguments": "echo a   b", "line": 2},
            {"instruction": "WORKDIR", "arguments": "c:\\", "line": 4},
            {"instruction": "USER", "arguments": "1", "line": 5},
        ])
    );
}

#[test]
fn backslash_continuation_drops_comment_and_blank_lines() {
    assert_eq!(
        ir(b"RUN echo a \\\n# dropped\n\n  b\nUSER 1\n"),
        json!([
            {"instruction": "RUN", "arguments": "echo a   b", "line": 1},
            {"instruction": "USER", "arguments": "1", "line": 5},
        ])
    );
}

#[test]
fn crlf_line_endings_parse_like_lf() {
    // No `\r` leaks into an argument, a continuation still joins, and a
    // heredoc terminator still matches.
    assert_eq!(
        ir(b"FROM a\r\nRUN echo \\\r\n  b\r\nRUN <<EOT\r\necho hi\r\nEOT\r\nUSER 1\r\n"),
        json!([
            {"instruction": "FROM", "arguments": "a", "line": 1},
            {"instruction": "RUN", "arguments": "echo   b", "line": 2},
            {"instruction": "RUN", "arguments": "<<EOT\necho hi\nEOT", "line": 4},
            {"instruction": "USER", "arguments": "1", "line": 7},
        ])
    );
}

#[test]
fn a_utf8_bom_is_skipped_before_an_instruction_and_before_a_directive() {
    assert_eq!(
        ir(b"\xef\xbb\xbfFROM alpine\nUSER 1\n"),
        json!([
            {"instruction": "FROM", "arguments": "alpine", "line": 1},
            {"instruction": "USER", "arguments": "1", "line": 2},
        ])
    );
    assert_eq!(
        ir(b"\xef\xbb\xbf# escape=`\nRUN a `\n  b\n"),
        json!([{"instruction": "RUN", "arguments": "a   b", "line": 2}])
    );
}

#[test]
fn exec_form_is_an_array_and_flags_before_it_keep_the_shell_string() {
    assert_eq!(
        ir(b"RUN [\"sh\",\"-c\",\"echo hi\"]\n\
             RUN --mount=type=cache,target=/x [\"sh\",\"-c\",\"x\"]\n\
             CMD [\"a\", \\\n  \"b\"]\n\
             RUN [ -f x ] && y\n"),
        json!([
            {"instruction": "RUN", "arguments": ["sh", "-c", "echo hi"], "line": 1},
            {
                "instruction": "RUN",
                "arguments": "--mount=type=cache,target=/x [\"sh\",\"-c\",\"x\"]",
                "line": 2
            },
            {"instruction": "CMD", "arguments": ["a", "b"], "line": 3},
            {"instruction": "RUN", "arguments": "[ -f x ] && y", "line": 5},
        ])
    );
}

#[test]
fn env_and_arg_arguments_are_maps() {
    assert_eq!(
        ir(b"ENV A 1\nENV B one two\nENV A=1 B=\"x y\"\nARG V\nARG V=1\n"),
        json!([
            {"instruction": "ENV", "arguments": {"A": "1"}, "line": 1},
            {"instruction": "ENV", "arguments": {"B": "one two"}, "line": 2},
            {"instruction": "ENV", "arguments": {"A": "1", "B": "x y"}, "line": 3},
            {"instruction": "ARG", "arguments": {"V": null}, "line": 4},
            {"instruction": "ARG", "arguments": {"V": "1"}, "line": 5},
        ])
    );
}

#[test]
fn line_is_the_start_line_after_continuations_and_heredocs() {
    let got = ir(b"FROM a\n\
                   RUN a \\\n  b \\\n  c\n\
                   USER 1\n\
                   RUN <<EOT\nx\nEOT\n\
                   \n\
                   # comment\n\
                   USER 2\n");
    let lines: Vec<(&str, i64)> = got
        .as_array()
        .expect("array")
        .iter()
        .map(|row| {
            (
                row["instruction"].as_str().expect("instruction"),
                row["line"].as_i64().expect("line"),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            ("FROM", 1),
            ("RUN", 2),
            ("USER", 5),
            ("RUN", 6),
            ("USER", 11)
        ]
    );
}

#[test]
fn dq_get_addresses_the_ir_by_pointer() {
    let dir = dockerfile(b"FROM a\nHEALTHCHECK NONE\nONBUILD RUN make\n");
    dq().current_dir(dir.path())
        .args(["--no-color", "get", "Dockerfile", "/1/instruction"])
        .assert()
        .success()
        .stdout("HEALTHCHECK\n");
    dq().current_dir(dir.path())
        .args(["--no-color", "get", "Dockerfile", "/2/line"])
        .assert()
        .success()
        .stdout("3\n");
}

#[test]
fn a_bad_instruction_exits_3_naming_its_start_line() {
    let cases: &[(&[u8], &str)] = &[
        (b"FROM a\nFOO bar\n", "line 2: unknown instruction `FOO`"),
        (
            b"FROM a\nRUN <<EOF\necho\n",
            "line 2: unterminated heredoc `EOF`",
        ),
        (
            b"FROM a\nSHELL sh -c\n",
            "line 2: SHELL requires the arguments in JSON form",
        ),
        (b"FROM a\nENV A\n", "line 2: ENV must have two arguments"),
    ];
    for (src, needle) in cases {
        let dir = dockerfile(src);
        dq().current_dir(dir.path())
            .args(["--no-color", "query", "-F", "json", ".", "Dockerfile"])
            .assert()
            .code(3)
            .stdout("")
            .stderr(predicate::str::contains(*needle));
    }
}
