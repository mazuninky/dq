//! A textual edit that dq cannot make without changing other parts of the
//! file is refused: exit 7 (`edit_rejected`), nothing on stdout, the reason
//! and the `dq set --jq` alternative on stderr, and the file left exactly
//! as it was — with `-i` too.

use assert_cmd::Command;
use predicates::prelude::*;
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

/// What the refusal says after the reason.
const JQ_HINT: &str =
    "`dq set --jq` rewrites the whole file through its writer instead, dropping comments";

/// A YAML file whose `a` is anchored and named again by the alias key
/// `*x`: a new value for `a` renames that key too.
const ALIAS_KEY_YAML: &str = "a: &x k\n*x : 1\n";

/// One refused edit: the file, the arguments after it, and what stderr says.
struct Refusal {
    name: &'static str,
    src: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    message: &'static str,
}

const REFUSALS: &[Refusal] = &[
    Refusal {
        name: "doc.yaml",
        src: ALIAS_KEY_YAML,
        command: "set",
        args: &["/a", "z"],
        message: "cannot set '/a': the edit would change other parts of the document; ",
    },
    Refusal {
        // The alias key names a collection: the set changes it too.
        name: "doc.yaml",
        src: "a: &k [1, 2]\n*k : x\n",
        command: "set",
        args: &["/a/0", "9"],
        message: "cannot set '/a/0': the edit would change other parts of the document; ",
    },
    Refusal {
        // A comment between the entry and `}`: removing the entry would
        // leave the comment swallowing the bracket.
        name: "doc.yaml",
        src: "a: {x: 1, y: 2 # c\n}\n",
        command: "del",
        args: &["/a/y"],
        message: "cannot delete '/a/y': dq cannot remove it as a textual edit; ",
    },
    Refusal {
        // A duplicate member: removing one leaves the other, so `a` is
        // still there.
        name: "doc.json",
        src: "{\"a\": 1, \"a\": 2, \"b\": 3}",
        command: "del",
        args: &["/a"],
        message: "cannot delete '/a': dq cannot remove it as a textual edit; ",
    },
    // Below an alias the node is the anchored one: the refusal names the
    // alias's line and the pointer of the anchored node.
    Refusal {
        name: "doc.yaml",
        src: UNDER_ALIAS_YAML,
        command: "set",
        args: &["/b/x", "5"],
        message: "cannot set '/b/x': it lies under the alias on line 3, a copy of /a; \
                  an edit at the anchored node changes every copy; ",
    },
    Refusal {
        name: "doc.yaml",
        src: UNDER_ALIAS_YAML,
        command: "set",
        args: &["/b/new", "5"],
        message: "cannot set '/b/new': it lies under the alias on line 3, a copy of /a; ",
    },
    Refusal {
        name: "doc.yaml",
        src: UNDER_ALIAS_YAML,
        command: "del",
        args: &["/b/x"],
        message: "cannot delete '/b/x': it lies under the alias on line 3, a copy of /a; ",
    },
    Refusal {
        // A merge key is an alias like any other.
        name: "doc.yaml",
        src: MERGE_YAML,
        command: "set",
        args: &["/svc/<</x", "5"],
        message: "cannot set '/svc/<</x': it lies under the alias on line 4, a copy of /base; ",
    },
    Refusal {
        name: "doc.yaml",
        src: MERGE_YAML,
        command: "del",
        args: &["/svc/<</x"],
        message: "cannot delete '/svc/<</x': it lies under the alias on line 4, a copy of /base; ",
    },
    Refusal {
        // The nearest alias on the way down is the one named.
        name: "doc.yaml",
        src: NESTED_ALIAS_YAML,
        command: "set",
        args: &["/c/k", "5"],
        message: "cannot set '/c/k': it lies under the alias on line 5, a copy of /a/in; ",
    },
    Refusal {
        name: "doc.yaml",
        src: NESTED_ALIAS_YAML,
        command: "set",
        args: &["/b/in/k", "5"],
        message: "cannot set '/b/in/k': it lies under the alias on line 4, a copy of /a; ",
    },
    Refusal {
        // A blank CRLF line counts as a line.
        name: "doc.yaml",
        src: "a: &m\r\n  x: 1\r\n\r\nb: *m\r\n",
        command: "set",
        args: &["/b/x", "5"],
        message: "cannot set '/b/x': it lies under the alias on line 4, a copy of /a; ",
    },
    Refusal {
        name: "doc.yaml",
        src: ROOT_SEQUENCE_ALIAS_YAML,
        command: "del",
        args: &["/1/x"],
        message: "cannot delete '/1/x': it lies under the alias on line 2, a copy of /0; ",
    },
    Refusal {
        // The second alias of an anchor is named at its own line.
        name: "doc.yaml",
        src: "a: &m\n  x: 1\nb: *m\nzz: *m\n",
        command: "set",
        args: &["/zz/x", "5"],
        message: "cannot set '/zz/x': it lies under the alias on line 4, a copy of /a; ",
    },
    Refusal {
        // Two aliases on one flow line: the line is theirs, not the
        // anchor's.
        name: "doc.yaml",
        src: "a: &m\n  x: 1\nl: [*m, *m]\n",
        command: "set",
        args: &["/l/1/x", "5"],
        message: "cannot set '/l/1/x': it lies under the alias on line 3, a copy of /a; ",
    },
    Refusal {
        // `bc` after `b`, whose name is a prefix of it.
        name: "doc.yaml",
        src: "a: &m\n  x: 1\np: &n\n  y: 1\nb: *m\nbc: *n\n",
        command: "set",
        args: &["/bc/y", "5"],
        message: "cannot set '/bc/y': it lies under the alias on line 6, a copy of /p; ",
    },
];

/// `b` is an alias of the anchored mapping `a`.
const UNDER_ALIAS_YAML: &str = "a: &m\n  x: 1\nb: *m\n";

/// `svc` merges `base` in through `<<`, which dq keeps as a key.
const MERGE_YAML: &str = "base: &b\n  x: 1\nsvc:\n  <<: *b\n  y: 2\n";

/// An anchor inside an anchor: `b` copies all of `a`, `c` only `a/in`.
const NESTED_ALIAS_YAML: &str = "a: &m\n  in: &n\n    k: 1\nb: *m\nc: *n\n";

/// A root sequence whose second item is an alias of the first.
const ROOT_SEQUENCE_ALIAS_YAML: &str = "- &m {x: 1}\n- *m\n";

/// JSON has no NaN: no writer of dq's can put one there, so the refusal
/// points to no way around it.
#[test]
fn a_json_set_of_nan_exits_7_with_no_way_around() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("nan.yaml"), "x: .nan\n").expect("write value file");
    let src = "{\"j\": 1}\n";
    std::fs::write(dir.path().join("doc.json"), src).expect("write fixture");
    for in_place in [false, true] {
        let mut cmd = dq();
        cmd.current_dir(dir.path());
        if in_place {
            cmd.arg("-i");
        }
        cmd.args([
            "--no-color",
            "set",
            "doc.json",
            "/j",
            "--value-from",
            "nan.yaml",
        ])
        .assert()
        .code(7)
        .stdout("")
        .stderr(
            predicate::str::contains("cannot set '/j': JSON has no NaN or Infinity")
                .and(predicate::str::contains(JQ_HINT).not())
                .and(predicate::str::contains("panicked").not()),
        );
        assert_eq!(
            std::fs::read(dir.path().join("doc.json")).expect("read back"),
            src.as_bytes(),
            "-i={in_place}: the file is untouched"
        );
    }
}

#[test]
fn a_refused_edit_exits_7_and_leaves_the_file_alone() {
    for case in REFUSALS {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join(case.name);
        std::fs::write(&path, case.src).expect("write fixture");
        for in_place in [false, true] {
            let mut cmd = dq();
            cmd.current_dir(dir.path());
            if in_place {
                cmd.arg("-i");
            }
            cmd.args(["--no-color", case.command, case.name])
                .args(case.args)
                .assert()
                .code(7)
                .stdout("")
                .stderr(
                    predicate::str::contains(case.message)
                        .and(predicate::str::contains(JQ_HINT))
                        .and(predicate::str::contains("panicked").not()),
                );
            assert_eq!(
                std::fs::read(&path).expect("read back"),
                case.src.as_bytes(),
                "{} {:?} on {:?}, -i={in_place}: the file is untouched",
                case.command,
                case.args,
                case.src,
            );
        }
    }
}

#[test]
fn the_same_edits_are_made_where_they_read_back() {
    // The positive side of each refusal above: one change to the input
    // and the edit goes through.
    let cases: &[(&str, &str, &str, &[&str], &str)] = &[
        (
            "doc.yaml",
            "a: k\nk: 1\n",
            "set",
            &["/a", "z"],
            "a: z\nk: 1\n",
        ),
        (
            "doc.yaml",
            "a: {x: 1, y: 2, # c\n}\n",
            "del",
            &["/a/y"],
            "a: {x: 1,  # c\n}\n",
        ),
        (
            "doc.json",
            "{\"a\": 1, \"b\": 3}",
            "del",
            &["/a"],
            "{\"b\": 3}",
        ),
        (
            "doc.json",
            "{\"j\": 1}\n",
            "set",
            &["/j", "--value-from", "one.yaml"],
            "{\"j\": {\"x\":1.5}}\n",
        ),
    ];
    for (name, src, command, args, want) in cases {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("one.yaml"), "x: 1.5\n").expect("write value file");
        let path = dir.path().join(name);
        std::fs::write(&path, src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["-i", "--no-color", command, name])
            .args(*args)
            .assert()
            .success()
            .stdout("");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            *want,
            "{command} {args:?} on {src:?}",
        );
    }
}

#[test]
fn edits_at_an_anchored_node_or_of_an_alias_go_through() {
    // The positive side of the refusals below an alias: the anchored node
    // itself, a key beside a merge, and the alias replaced as a whole.
    let cases: &[(&str, &str, &[&str], &str, serde_json::Value)] = &[
        (
            NESTED_ALIAS_YAML,
            "set",
            &["/a/in/k", "5"],
            "a: &m\n  in: &n\n    k: 5\nb: *m\nc: *n\n",
            serde_json::json!({"a": {"in": {"k": 5}}, "b": {"in": {"k": 5}}, "c": {"k": 5}}),
        ),
        (
            MERGE_YAML,
            "set",
            &["/base/x", "5"],
            "base: &b\n  x: 5\nsvc:\n  <<: *b\n  y: 2\n",
            serde_json::json!({"base": {"x": 5}, "svc": {"<<": {"x": 5}, "y": 2}}),
        ),
        (
            MERGE_YAML,
            "set",
            &["/svc/y", "5"],
            "base: &b\n  x: 1\nsvc:\n  <<: *b\n  y: 5\n",
            serde_json::json!({"base": {"x": 1}, "svc": {"<<": {"x": 1}, "y": 5}}),
        ),
        (
            ROOT_SEQUENCE_ALIAS_YAML,
            "set",
            &["/0/x", "2"],
            "- &m {x: 2}\n- *m\n",
            serde_json::json!([{"x": 2}, {"x": 2}]),
        ),
        (
            UNDER_ALIAS_YAML,
            "set",
            &["/b", "5"],
            "a: &m\n  x: 1\nb: 5\n",
            serde_json::json!({"a": {"x": 1}, "b": 5}),
        ),
        (
            UNDER_ALIAS_YAML,
            "del",
            &["/b"],
            "a: &m\n  x: 1\n",
            serde_json::json!({"a": {"x": 1}}),
        ),
    ];
    for (src, command, args, want, value) in cases {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("doc.yaml");
        std::fs::write(&path, src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["-i", "--no-color", command, "doc.yaml"])
            .args(*args)
            .assert()
            .success()
            .stdout("");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            *want,
            "{command} {args:?} on {src:?}",
        );
        let out = dq()
            .current_dir(dir.path())
            .args(["--no-color", "-F", "json", "convert", "doc.yaml"])
            .output()
            .expect("convert runs");
        assert!(out.status.success(), "{command} {args:?}: convert");
        let got: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(got, *value, "{command} {args:?}: aliases resolve");
    }
}

#[test]
fn the_refusal_names_the_alias_the_pointer_runs_through() {
    // `/bc/y` runs through `bc`, not through `b`, whose name is a prefix
    // of it.
    let src = "a: &m\n  x: 1\np: &n\n  y: 1\nb: *m\nbc: *n\n";
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("doc.yaml"), src).expect("write fixture");
    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "doc.yaml", "/bc/y", "5"])
        .assert()
        .code(7)
        .stdout("")
        .stderr(
            predicate::str::contains("cannot set '/bc/y': it lies under the alias on line ")
                .and(predicate::str::contains(", a copy of /p; ")),
        );
    assert_eq!(
        std::fs::read(dir.path().join("doc.yaml")).expect("read back"),
        src.as_bytes()
    );
}

#[test]
fn del_of_a_missing_key_below_an_alias_is_not_found() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("doc.yaml"), UNDER_ALIAS_YAML).expect("write fixture");
    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "del", "doc.yaml", "/b/nope"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("path '/b/nope' not found"));
    assert_eq!(
        std::fs::read(dir.path().join("doc.yaml")).expect("read back"),
        UNDER_ALIAS_YAML.as_bytes()
    );
}

#[test]
fn bulk_set_counts_a_refused_file_as_failed() {
    let dir = TempDir::new().expect("tempdir");
    let refused = dir.path().join("1.yaml");
    let edited = dir.path().join("2.yaml");
    std::fs::write(&refused, ALIAS_KEY_YAML).expect("write fixture");
    std::fs::write(&edited, "a: k # c\n").expect("write fixture");

    dq().current_dir(dir.path())
        .args([
            "-i",
            "--no-color",
            "--continue-on-error",
            "set",
            "*.yaml",
            "/a",
            "z",
        ])
        .assert()
        .code(7)
        .stdout(
            predicate::str::contains("Modified: 1, Skipped: 0, Failed: 1").and(
                predicate::str::contains(
                    "1.yaml: cannot set '/a': the edit would change other parts",
                ),
            ),
        );
    assert_eq!(
        std::fs::read_to_string(&refused).expect("read"),
        ALIAS_KEY_YAML
    );
    assert_eq!(
        std::fs::read_to_string(&edited).expect("read"),
        "a: z # c\n"
    );

    // Without `--continue-on-error` the run stops at the refused file,
    // which sorts first, before it touches the other.
    std::fs::write(&edited, "a: k # c\n").expect("write fixture");
    dq().current_dir(dir.path())
        .args(["-i", "--no-color", "set", "*.yaml", "/a", "z"])
        .assert()
        .code(7)
        .stderr(predicate::str::contains(
            "cannot set '/a': the edit would change other parts",
        ));
    assert_eq!(
        std::fs::read_to_string(&refused).expect("read"),
        ALIAS_KEY_YAML
    );
    assert_eq!(
        std::fs::read_to_string(&edited).expect("read"),
        "a: k # c\n"
    );
}

/// A `fix.ops` rule that sets `/a` to `z` in any YAML file.
const FIX_A_RULE: &str = r#"
id: test.a-is-z
description: a must be z
severity: warn
match:
  format: yaml
check:
  jq: 'select(.a != "z")'
  message: "a must be z"
fix:
  ops: |
    if .a != "z" then [{"op":"replace","path":"/a","value":"z"}] else [] end
"#;

#[test]
fn fix_in_place_skips_a_refused_fix_ops_edit() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("rule.yml"), FIX_A_RULE).expect("write rule");
    let refused = dir.path().join("1.yaml");
    let edited = dir.path().join("2.yaml");
    std::fs::write(&refused, ALIAS_KEY_YAML).expect("write fixture");
    std::fs::write(&edited, "a: k # c\n").expect("write fixture");

    dq().current_dir(dir.path())
        .args([
            "-i",
            "--no-color",
            "fix",
            "--rules",
            "rule.yml",
            "1.yaml",
            "2.yaml",
        ])
        .assert()
        .code(0)
        .stdout("Modified: 1, Skipped: 1, Failed: 0\n")
        .stderr(
            predicate::str::contains(
                "fix.ops cannot be applied without changing other parts of the file",
            )
            .and(predicate::str::contains("rule_id=test.a-is-z")),
        );
    assert_eq!(
        std::fs::read_to_string(&refused).expect("read"),
        ALIAS_KEY_YAML
    );
    // `fix.ops` edits in place of the text, so the comment stays.
    assert_eq!(
        std::fs::read_to_string(&edited).expect("read"),
        "a: z # c\n"
    );
}
