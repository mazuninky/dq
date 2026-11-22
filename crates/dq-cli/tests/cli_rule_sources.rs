//! Where the rules of a run come from: a source that can check nothing is an
//! error, and what a rule directory holds under links.
//!
//! `cli_rule_errors.rs` has the basics.

mod scratch;

use pretty_assertions::assert_eq;
use scratch::{Run, Scratch};

/// Reports every document whose `kind` is `Thing`, in YAML; its fix sets
/// `fixed`.
const FIRES: &str = r#"id: t.fires
description: fires on every thing
severity: error
match:
  format: yaml
check:
  jq: 'select(.kind == "Thing")'
  message: is a thing
fix:
  ops: 'if .fixed then [] else [{"op":"add","path":"/fixed","value":true}] end'
"#;

/// [`FIRES`] under another id.
fn fires(id: &str) -> String {
    FIRES.replace("t.fires", id)
}

/// [`FIRES`] with another `match.format`.
fn with_format(format: &str) -> String {
    FIRES.replace("format: yaml", &format!("format: {format}"))
}

/// A composite with `nested` in it, itself a composite with a rule of
/// `match.format: FORMAT`.
fn nested(format: &str) -> String {
    let wrap = |id: &str, inner: &str| {
        let inner: String = inner.lines().map(|l| format!("    {l}\n")).collect();
        format!(
            "id: {id}\ndescription: d\nseverity: error\nmatch:\n  format: yaml\ncheck:\n  \
             extract: '[]'\n  message: m\n  nested:\n{inner}"
        )
    };
    let innermost = with_format(format);
    let innermost = innermost.split("fix:").next().expect("a rule");
    wrap("t.outer", &wrap("t.middle", innermost))
}

const THING: &str = "kind: Thing\n";

/// The ids `dq -F json ARGS` reports.
fn found(scratch: &Scratch, args: &[&str]) -> Vec<String> {
    let run = scratch.run(&[&["-F", "json"], args].concat());
    assert_eq!((run.code, run.stderr.lines().count()), (4, 1), "{run:?}");
    let report: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON stdout");
    report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| d["rule_id"].as_str().expect("rule_id").to_owned())
        .collect()
}

/// A run that failed before it printed anything.
fn failed(code: i32, stderr: &str) -> Run {
    Run {
        code,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// match.format
// ---------------------------------------------------------------------------

const UNKNOWN: &str =
    "rule t.fires has unknown format 'ymal' in match.format (did you mean: xml, yaml)\n";
const EMPTY: &str = "rule t.fires: match.format lists no format\n";

#[test]
fn a_rule_that_can_never_apply_is_refused_from_every_source() {
    for (format, message) in [("ymal", UNKNOWN), ("[yaml, ymal]", UNKNOWN), ("[]", EMPTY)] {
        let bad = with_format(format);
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("bad.yml", &bad),
            ("nested.yml", &nested(format)),
            ("stream.yml", &format!("{}---\n{bad}", fires("t.good"))),
            ("dir/a.yml", &fires("t.good")),
            ("dir/sub/b.yaml", &bad),
        ]);
        for source in ["bad.yml", "nested.yml", "stream.yml", "dir", "./dir/sub"] {
            for command in [["lint", "--rules"], ["check", "--rule"], ["fix", "--rules"]] {
                let args = [command[0], command[1], source, "thing.yaml"];
                assert_eq!(scratch.run(&args), failed(3, message), "{args:?}");
                // Not a rule that raised: nothing to accept.
                let args = [command[0], "--allow-rule-errors", command[1], source];
                let run = scratch.run(&[&args[..], &["thing.yaml"]].concat());
                assert_eq!(run, failed(3, message), "{args:?}");
            }
        }
        let inline = ["check", "--inline", &bad, "thing.yaml"];
        assert_eq!(scratch.run(&inline), failed(3, message));
        // One good source beside it does not make up for it.
        let args = ["lint", "--rules", "dir/a.yml", "--rules", "bad.yml"];
        let run = scratch.run(&[&args[..], &["thing.yaml"]].concat());
        assert_eq!(run, failed(3, message));
        // Nor does the format of the file, which other rules check.
        scratch.write("doc.json", "{}\n");
        assert_eq!(
            scratch.run(&["lint", "--rules", "bad.yml", "doc.json"]),
            failed(3, message)
        );
        // `rules add DIR` takes no such rule over.
        let args = ["rules", "add", "dir"];
        assert_eq!(scratch.run(&args), failed(3, message));
        assert!(!scratch.path(".dq").exists());
        // Found in `.dq/rules`, where `@std` is loaded too.
        scratch.write(".dq/rules/bad.yml", &bad);
        for command in ["lint", "fix"] {
            let run = scratch.run(&[command, "thing.yaml"]);
            assert_eq!(run, failed(3, message), "{command}");
        }
    }
}

#[test]
fn a_fixture_of_a_rule_that_can_never_apply_errors() {
    let fixture = "tests:\n  - name: thing\n    input: 'kind: Thing'\n    expected:\n      violations: []\n  - name: other\n    input: 'kind: X'\n    expected:\n      violations: []\n";
    let scratch = Scratch::new(&[
        ("set/empty.yml", &with_format("[]")),
        ("set/empty.test.yml", fixture),
        ("set/unknown.yml", &with_format("ymal")),
        ("set/unknown.test.yml", fixture),
    ]);
    let run = scratch.run(&["test", "set"]);
    assert_eq!(
        run.stdout,
        format!(
            "ERROR set/empty.test.yml: <file> (rule load error: {})\n\
             ERROR set/unknown.test.yml: <file> (rule load error: {})\n\
             summary: 0 passed, 0 failed, 2 errored, 2 total\n",
            EMPTY.trim_end(),
            UNKNOWN.trim_end()
        )
    );
    assert_eq!(run.code, 4);
}

#[test]
fn match_format_takes_every_format_dq_reads_by_its_own_name() {
    let names = [
        "json",
        "yaml",
        "toml",
        "jsonl",
        "hcl",
        "ini",
        "dotenv",
        "csv",
        "tsv",
        "dockerfile",
        "ignore-list",
        "markdown",
        "frontmatter",
        "xml",
    ];
    let registered: Vec<&str> = dq_core::parsers::registry()
        .iter()
        .map(|format| format.name())
        .collect();
    assert_eq!(registered, names);
    let scratch = Scratch::new(&[("thing.yaml", THING)]);
    for name in names {
        scratch.write("rule.yml", &with_format(name));
        let run = scratch.run(&["lint", "--rules", "rule.yml", "thing.yaml"]);
        // It loads; it applies to this file only as `yaml`.
        assert_eq!(run.code, if name == "yaml" { 4 } else { 0 }, "{name}");
    }
    let all = format!("[{}]", names.join(", "));
    scratch.write("rule.yml", &with_format(&all));
    assert_eq!(
        found(&scratch, &["lint", "--rules", "rule.yml", "thing.yaml"]),
        ["t.fires"]
    );
}

#[test]
fn an_unknown_format_comes_with_close_names_or_every_name() {
    let every = "formats: json, yaml, toml, jsonl, hcl, ini, dotenv, csv, tsv, dockerfile, \
         ignore-list, markdown, frontmatter, xml";
    let scratch = Scratch::new(&[("thing.yaml", THING)]);
    for (name, hint) in [
        ("ymal", "did you mean: xml, yaml"),
        ("yml", "did you mean: xml, yaml, hcl"),
        ("Json", "did you mean: json, jsonl"),
        ("ignore_list", "did you mean: ignore-list"),
        ("toon", "did you mean: json, toml"),
        // Names are lowercase, and no alias or extension stands for one.
        ("YAML", every),
        ("terraform", every),
        ("", every),
    ] {
        scratch.write("rule.yml", &with_format(&format!("'{name}'")));
        assert_eq!(
            scratch.run(&["lint", "--rules", "rule.yml", "thing.yaml"]),
            failed(
                3,
                &format!("rule t.fires has unknown format '{name}' in match.format ({hint})\n")
            )
        );
    }
}

// ---------------------------------------------------------------------------
// Directories without a rule
// ---------------------------------------------------------------------------

#[test]
fn an_explicit_rule_directory_without_a_rule_file_is_an_error() {
    let scratch = Scratch::new(&[
        ("thing.yaml", THING),
        ("docs/README.md", "no rules here"),
        ("docs/rule.yml.bak", FIRES),
        ("tests/a.test.yml", "tests: []\n"),
        ("tests/deep/b.test.YAML", "tests: []\n"),
        ("set/deep/a.Yml", FIRES),
    ]);
    std::fs::create_dir(scratch.path("empty")).expect("mkdir");
    let absolute = scratch.path("empty");
    let absolute = absolute.to_str().expect("UTF-8 path");
    for dir in ["empty", "./empty", "empty/", "docs", "tests", absolute] {
        for command in [["lint", "--rules"], ["check", "--rule"], ["fix", "--rules"]] {
            let args = [command[0], command[1], dir, "thing.yaml"];
            let named = dir.replace(absolute, "ROOT/empty");
            assert_eq!(
                scratch.run(&args),
                failed(6, &format!("no rules in '{named}'\n")),
                "{args:?}"
            );
        }
        // Beside a source that has rules: `dir` still checks nothing.
        let args = ["lint", "--rules", "set", "--rules", dir, "thing.yaml"];
        assert_eq!(scratch.run(&args).code, 6);
    }
    // The positive control, a rule some levels down.
    for command in [["lint", "--rules"], ["check", "--rule"]] {
        let args = [command[0], command[1], "set", "thing.yaml"];
        assert_eq!(found(&scratch, &args), ["t.fires"]);
    }
}

#[test]
fn a_rule_file_without_a_rule_is_an_error() {
    let scratch = Scratch::new(&[
        ("thing.yaml", THING),
        ("dir/a.yml", FIRES),
        ("dir/blank.yml", ""),
    ]);
    for text in ["", "# a comment\n", "---\n"] {
        scratch.write("blank.yml", text);
        for source in ["blank.yml", "dir"] {
            let run = scratch.run(&["lint", "--rules", source, "thing.yaml"]);
            assert_eq!((run.code, run.stdout.as_str()), (3, ""), "{text:?}");
        }
        let inline = format!("--inline={text}");
        let run = scratch.run(&["check", &inline, "thing.yaml"]);
        assert_eq!((run.code, run.stdout.as_str()), (3, ""), "{text:?}");
    }
}

#[test]
fn the_rules_found_automatically_may_be_none() {
    let scratch = Scratch::new(&[("thing.yaml", THING)]);
    let clean = |why: &str| {
        for command in ["lint", "fix"] {
            let run = scratch.run(&[command, "thing.yaml"]);
            assert_eq!((run.code, run.stderr.as_str()), (0, ""), "{why}: {command}");
        }
    };
    clean("no .dq");
    std::fs::create_dir_all(scratch.path(".dq/rules")).expect("mkdir");
    clean("an empty .dq/rules");
    scratch.write(".dq/rules/a.test.yml", "tests: []\n");
    scratch.write(".dq/rules/notes.txt", "none yet");
    clean("no rule in .dq/rules");
    // Named, the same directory is an error.
    assert_eq!(
        scratch.run(&["lint", "--rules", ".dq/rules", "thing.yaml"]),
        failed(6, "no rules in '.dq/rules'\n")
    );
    scratch.write(".dq/rules/a.yml", FIRES);
    assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.fires"]);
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod links {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::{PermissionsExt, symlink};

    impl Scratch {
        /// Make `link` (and the directories above it) a link to `target`.
        fn link(&self, target: &str, link: &str) {
            let link = self.path(link);
            std::fs::create_dir_all(link.parent().expect("a parent")).expect("mkdir");
            symlink(target, link).expect("link");
        }
    }

    const FIXTURE: &str = "tests:\n  - name: thing\n    input: 'kind: Thing'\n    expected:\n      violations:\n        - rule: t.fires\n";

    const LINT_SET: [&str; 4] = ["lint", "--rules", "set", "thing.yaml"];

    /// The error of a run stopped by `path`, which could not be read.
    fn stopped(path: &str, why: &str) -> Run {
        failed(5, &format!("io error reading {path}: {why}\n"))
    }

    fn dangling(path: &str, target: &str) -> Run {
        stopped(path, &format!("a link to '{target}', where nothing is"))
    }

    #[test]
    fn chains_of_links_are_followed_to_the_end() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("real/rules.txt", FIRES),
            ("real/dir/b.yml", &fires("t.dir")),
        ]);
        // The name of the first link tells what it is, not the last one's.
        scratch.link("rules.txt", "real/2.txt");
        scratch.link("2.txt", "real/1.txt");
        scratch.link("../real/1.txt", "set/a.yml");
        scratch.link("dir", "real/dir2");
        scratch.link("../real/dir2", "set/z");
        assert_eq!(found(&scratch, &LINT_SET), ["t.fires", "t.dir"]);
        // Broken at its end: the link in the directory is named.
        std::fs::remove_file(scratch.path("real/rules.txt")).expect("rm");
        assert_eq!(
            scratch.run(&LINT_SET),
            dangling("ROOT/set/a.yml", "../real/1.txt")
        );
        scratch.write("real/rules.txt", FIRES);
        std::fs::remove_file(scratch.path("real/dir2")).expect("rm");
        assert_eq!(
            scratch.run(&LINT_SET),
            dangling("ROOT/set/z", "../real/dir2")
        );
    }

    #[test]
    fn a_link_to_itself_stops_the_run() {
        let scratch = Scratch::new(&[("thing.yaml", THING), ("set/a.yml", FIRES)]);
        scratch.write("set/a.test.yml", FIXTURE);
        scratch.link("b.yml", "set/b.yml");
        for args in [
            &LINT_SET[..],
            &["check", "--rule", "set", "thing.yaml"],
            &["fix", "--rules", "set", "thing.yaml"],
            &["test", "set"],
            &["rules", "add", "set"],
        ] {
            let run = scratch.run(args);
            assert_eq!((run.code, run.stdout.as_str()), (5, ""), "{args:?}");
            assert!(run.stderr.contains("set/b.yml: "), "{}", run.stderr);
        }
    }

    #[test]
    fn a_link_to_a_directory_above_is_walked_once() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("rules/a.yml", FIRES),
            ("rules/a.test.yml", FIXTURE),
            ("rules/set/b.yml", &fires("t.set")),
        ]);
        scratch.link("..", "rules/set/up");
        scratch.link(".", "rules/set/here");
        // From below: `up` is the way to `a.yml`, and not to `set` again.
        assert_eq!(
            found(&scratch, &["lint", "--rules", "rules/set", "thing.yaml"]),
            ["t.set", "t.fires"]
        );
        // From above: both are there already.
        assert_eq!(
            found(&scratch, &["lint", "--rules", "rules", "thing.yaml"]),
            ["t.fires", "t.set"]
        );
        let run = scratch.run(&["test", "rules/set"]);
        assert_eq!(
            (run.code, run.stdout.as_str()),
            (
                0,
                "ok rules/set/up/a.test.yml: thing\nsummary: 1 passed, 0 failed, 0 errored, 1 total\n"
            )
        );
    }

    #[test]
    fn rules_run_in_the_order_of_the_names_they_are_found_under() {
        let scratch = Scratch::new(&[("thing.yaml", THING), ("set/5.yml", &fires("t.c"))]);
        for id in ["a", "b", "d"] {
            scratch.write(&format!("real/{id}.yml"), &fires(&format!("t.{id}")));
        }
        scratch.link("../real/a.yml", "set/9.yml");
        scratch.link("../real/b.yml", "set/3.yml");
        scratch.link("../real/d.yml", "set/0.yml");
        // Links to rules already found, one of them in the directory itself.
        scratch.link("../real/d.yml", "set/7.yml");
        scratch.link("5.yml", "set/8.yml");
        scratch.link("5.yml", "set/1.yml");
        assert_eq!(found(&scratch, &LINT_SET), ["t.d", "t.c", "t.b", "t.a"]);
    }

    #[test]
    fn one_fixture_linked_beside_two_rules_tests_both() {
        let scratch = Scratch::new(&[
            ("shared.test.yml", FIXTURE),
            ("set/a/r.yml", FIRES),
            ("set/b/r.yml", &fires("t.other")),
        ]);
        scratch.link("../../shared.test.yml", "set/a/r.test.yml");
        scratch.link("../../shared.test.yml", "set/b/r.test.yml");
        let run = scratch.run(&["test", "set"]);
        assert_eq!(
            run.stdout,
            "ok set/a/r.test.yml: thing\n\
             FAIL set/b/r.test.yml: thing\n  \
               missing: rule=t.fires\n  \
               extra: rule=t.other message=\"is a thing\" line=1\n\
             summary: 1 passed, 1 failed, 0 errored, 2 total\n"
        );
        assert_eq!(run.code, 4);
        // A linked rule beside its linked fixture.
        scratch.write("set/b/r.yml", FIRES);
        scratch.link("../a/r.yml", "set/c/r.yml");
        scratch.link("../a/r.test.yml", "set/c/r.test.yml");
        let run = scratch.run(&["test", "set"]);
        assert_eq!(
            (run.code, run.stdout.lines().last()),
            (0, Some("summary: 3 passed, 0 failed, 0 errored, 3 total"))
        );
    }

    #[test]
    fn a_directory_that_cannot_be_read_stops_the_run() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("set/a.yml", FIRES),
            ("set/a.test.yml", FIXTURE),
            ("set/sub/b.yml", FIRES),
        ]);
        scratch.link("../set", ".dq/rules");
        let mode = |path: &str, mode| {
            let permissions = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(scratch.path(path), permissions).expect("chmod");
        };
        mode("set/sub", 0o000);
        let root = std::fs::read_dir(scratch.path("set/sub")).is_ok();
        let runs: Vec<(&[&str], Run)> = [
            &LINT_SET[..],
            &["check", "--rule", "set", "thing.yaml"],
            &["fix", "--rules", "set", "thing.yaml"],
            &["lint", "thing.yaml"],
            &["test", "set"],
            &["rules", "add", "set"],
        ]
        .map(|args| (args, scratch.run(args)))
        .to_vec();
        mode("set/sub", 0o755);
        mode("set/sub/b.yml", 0o000);
        let file = scratch.run(&LINT_SET);
        mode("set/sub/b.yml", 0o644);
        if root {
            // Root reads everything.
            return;
        }
        for (args, run) in runs {
            assert_eq!((run.code, run.stdout.as_str()), (5, ""), "{args:?}");
            assert!(
                run.stderr.contains("/sub: Permission denied"),
                "{}",
                run.stderr
            );
        }
        assert_eq!((file.code, file.stdout.as_str()), (5, ""));
        assert!(file.stderr.contains("set/sub/b.yml: Permission denied"));
        assert_eq!(found(&scratch, &LINT_SET), ["t.fires", "t.fires"]);
    }

    #[test]
    fn a_dangling_link_shows_its_name_and_target_escaped() {
        let scratch = Scratch::new(&[("thing.yaml", THING), ("set/a.yml", FIRES)]);
        scratch.link(
            "go\u{1b}[31mne\u{202e}\nERROR forged\r\u{9b}",
            "set/b\u{1b}[32m\nad\u{202e}.yml",
        );
        let why = r"a link to 'go\x1b[31mne\u202e\x0aERROR forged\x0d\x9b', where nothing is";
        let name = r"b\x1b[32m\x0aad\u202e.yml";
        for (args, root) in [
            (&LINT_SET[..], "ROOT/"),
            (&["fix", "--rules", "set", "thing.yaml"], "ROOT/"),
            (&["check", "--rule", "set", "thing.yaml"], ""),
            (&["test", "set"], ""),
            (&["rules", "add", "set"], "ROOT/"),
        ] {
            assert_eq!(
                scratch.run(args),
                stopped(&format!("{root}set/{name}"), why),
                "{args:?}"
            );
        }
    }

    #[test]
    fn a_dangling_directory_link_stops_a_lint_and_no_test_run() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            (".dq/rules/a.yml", FIRES),
            (".dq/rules/a.test.yml", FIXTURE),
        ]);
        // A submodule that is not checked out may have held rules.
        scratch.link("../../company-rules", ".dq/rules/company");
        for args in [
            &["lint", "thing.yaml"][..],
            &["fix", "--check", "thing.yaml"],
            &["lint", "--rules", ".dq/rules", "thing.yaml"],
        ] {
            assert_eq!(
                scratch.run(args),
                dangling("ROOT/.dq/rules/company", "../../company-rules"),
                "{args:?}"
            );
        }
        // Any directory of a repository may hold such a link.
        let passed =
            "ok ./.dq/rules/a.test.yml: thing\nsummary: 1 passed, 0 failed, 0 errored, 1 total\n";
        let run = scratch.run(&["test", "."]);
        assert_eq!((run.code, run.stdout.as_str()), (0, passed));
        let why = "a link to '../../company-rules', where nothing is";
        let skipped = format!("WARN skipped ./.dq/rules/company: {why}\n");
        assert_eq!(run.stderr, skipped);
        // One named as a rule or a fixture is a test that does not run.
        for name in ["b.yml", "b.test.yaml", "b.TEST.YML"] {
            scratch.link("../gone", &format!(".dq/rules/{name}"));
            assert_eq!(
                scratch.run(&["test", "."]),
                dangling(&format!("./.dq/rules/{name}"), "../gone")
            );
            std::fs::remove_file(scratch.path(&format!(".dq/rules/{name}"))).expect("rm");
        }
        assert_eq!(scratch.run(&["test", "."]).code, 0);
    }

    #[test]
    fn an_editor_lock_is_no_rule_and_a_file_named_like_one_is() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            (".dq/rules/a.yml", FIRES),
            (".dq/rules/a.test.yml", FIXTURE),
        ]);
        for name in [".#a.yml", ".#a.test.yml"] {
            scratch.link("me@host.4711:1", &format!(".dq/rules/{name}"));
        }
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.fires"]);
        assert_eq!(
            found(&scratch, &["lint", "--rules", ".dq/rules", "thing.yaml"]),
            ["t.fires"]
        );
        let run = scratch.run(&["test", "."]);
        assert_eq!(run.code, 0, "{run:?}");
        // With something in it it is a rule like any other.
        scratch.write(".dq/rules/.#b.yml", &fires("t.file"));
        scratch.write("real.yml", &fires("t.link"));
        scratch.link("../../real.yml", ".dq/rules/.#c.yml");
        assert_eq!(
            found(&scratch, &["lint", "thing.yaml"]),
            ["t.file", "t.link", "t.fires"]
        );
    }

    #[test]
    fn rules_add_symlink_force_replaces_the_link_and_not_what_it_leads_to() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("one/set/a.yml", &fires("t.one")),
            ("two/set/a.yml", &fires("t.two")),
            ("two/set/a.test.yml", "tests: []\n"),
        ]);
        let add = |args: &[&str]| scratch.run(&[&["rules", "add"], args].concat());
        assert_eq!(add(&["one/set", "--symlink"]).code, 0);
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.one"]);

        // Not over what is there, unless told to.
        let run = add(&["two/set", "--symlink"]);
        assert_eq!(
            run,
            failed(
                6,
                "destination already exists: ROOT/.dq/rules/set/a.yml (pass --force to overwrite)\n"
            )
        );
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.one"]);
        assert_eq!(add(&["two/set", "--symlink", "--force"]).code, 0);
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.two"]);
        for name in ["a.yml", "a.test.yml"] {
            let link = scratch.path(&format!(".dq/rules/set/{name}"));
            assert!(link.is_symlink(), "{name}");
        }
        // A link follows its file; a copy over it leaves the file alone.
        scratch.write("two/set/a.yml", &fires("t.edited"));
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.edited"]);
        assert_eq!(add(&["one/set", "--force"]).code, 0);
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.one"]);
        assert!(!scratch.path(".dq/rules/set/a.yml").is_symlink());
        let read = |path| std::fs::read_to_string(scratch.path(path)).expect("read");
        assert_eq!(read("one/set/a.yml"), fires("t.one"));
        assert_eq!(read("two/set/a.yml"), fires("t.edited"));

        // The file moved away: no rule, so no clean run.
        assert_eq!(add(&["two/set", "--symlink", "--force"]).code, 0);
        std::fs::remove_file(scratch.path("two/set/a.yml")).expect("rm");
        assert_eq!(
            scratch.run(&["lint", "thing.yaml"]),
            dangling("ROOT/.dq/rules/set/a.yml", "ROOT/two/set/a.yml")
        );
    }

    #[test]
    fn a_dangling_rule_directory_found_automatically_stops_the_run() {
        let scratch = Scratch::new(&[("thing.yaml", THING)]);
        let stops = |path: &str| {
            for command in ["lint", "fix"] {
                let run = scratch.run(&[command, "thing.yaml"]);
                assert_eq!(run, dangling(path, "gone"), "{command}");
            }
            // Not asked for, it is not looked at.
            let run = scratch.run(&["lint", "--rules", "@std/npm", "thing.yaml"]);
            assert_eq!((run.code, run.stderr.as_str()), (0, ""));
        };
        scratch.link("gone", ".dq");
        stops("ROOT/.dq");
        std::fs::remove_file(scratch.path(".dq")).expect("rm");
        scratch.link("gone", ".dq/rules");
        stops("ROOT/.dq/rules");
        // Where the link leads to rules, they run.
        scratch.write(".dq/gone/a.yml", FIRES);
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.fires"]);
    }

    /// `lint` and `fix` without `--rules`, which `stderr` stops.
    fn stops(scratch: &Scratch, code: i32, stderr: &str) {
        for args in [&["lint"][..], &["fix", "--check"], &["fix", "-i"]] {
            let run = scratch.run(&[args, &["thing.yaml"]].concat());
            assert_eq!((run.code, run.stdout.as_str()), (code, ""), "{args:?}");
            assert!(run.stderr.starts_with(stderr), "{args:?}: {run:?}");
            assert_eq!(read(scratch, "thing.yaml"), THING);
        }
        // Not asked for, it is not looked at.
        let run = scratch.run(&["lint", "--rules", "real", "thing.yaml"]);
        assert_eq!(run.code, 4, "{run:?}");
    }

    fn read(scratch: &Scratch, path: &str) -> String {
        std::fs::read_to_string(scratch.path(path)).expect("read")
    }

    #[test]
    fn a_rule_directory_that_links_to_itself_stops_the_run() {
        for (target, link) in [(".dq", ".dq"), ("rules", ".dq/rules")] {
            let scratch = Scratch::new(&[("thing.yaml", THING), ("real/a.yml", FIRES)]);
            scratch.link(target, link);
            stops(&scratch, 5, &format!("io error reading ROOT/{link}: "));
        }
    }

    #[test]
    fn a_rule_directory_that_may_not_be_read_stops_the_run() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("real/a.yml", FIRES),
            (".dq/rules/a.yml", FIRES),
        ]);
        let mode = |mode| {
            let permissions = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(scratch.path(".dq"), permissions).expect("chmod");
        };
        mode(0o000);
        // Root reads everything.
        if std::fs::read_dir(scratch.path(".dq")).is_err() {
            let why = "io error reading ROOT/.dq/rules: Permission denied";
            stops(&scratch, 5, why);
        }
        mode(0o755);
    }

    #[test]
    fn a_file_in_place_of_the_rule_directory_stops_the_run() {
        for name in [".dq", ".dq/rules"] {
            let error = format!("./{name} is not a directory\n");
            // What a checkout without symbolic links leaves for one.
            let scratch = Scratch::new(&[
                ("thing.yaml", THING),
                ("real/a.yml", FIRES),
                (name, "../real"),
            ]);
            stops(&scratch, 6, &error);
            assert_eq!(scratch.run(&["rules", "add", "real"]), failed(6, &error));

            let scratch = Scratch::new(&[("thing.yaml", THING), ("real/a.yml", FIRES)]);
            let target = scratch.path("real/a.yml");
            scratch.link(target.to_str().expect("UTF-8 path"), name);
            stops(&scratch, 6, &error);
        }
    }

    #[test]
    fn a_link_to_itself_under_another_name_stops_a_lint_and_no_test_run() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            (".dq/rules/a.yml", FIRES),
            (".dq/rules/a.test.yml", FIXTURE),
        ]);
        scratch.link("company", ".dq/rules/company");
        let run = scratch.run(&["lint", "thing.yaml"]);
        assert_eq!((run.code, run.stdout.as_str()), (5, ""));
        let why = "io error reading ROOT/.dq/rules/company: ";
        assert!(run.stderr.starts_with(why), "{run:?}");
        let run = scratch.run(&["test", "."]);
        assert_eq!(
            (run.code, run.stdout.as_str()),
            (
                0,
                "ok ./.dq/rules/a.test.yml: thing\nsummary: 1 passed, 0 failed, 0 errored, 1 total\n"
            )
        );
        // Named as a fixture it is a test that does not run.
        scratch.link("b.test.yml", ".dq/rules/b.test.yml");
        let run = scratch.run(&["test", "."]);
        assert_eq!((run.code, run.stdout.as_str()), (5, ""));
        let why = "io error reading ./.dq/rules/b.test.yml: ";
        assert!(run.stderr.starts_with(why), "{run:?}");
    }

    #[test]
    fn a_fifo_named_as_a_rule_or_a_fixture_stops_the_run() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("set/a.yml", FIRES),
            ("set/a.test.yml", FIXTURE),
        ]);
        let fifo = |path: &str| {
            let made = std::process::Command::new("mkfifo")
                .arg(scratch.path(path))
                .status()
                .expect("mkfifo");
            assert!(made.success());
        };
        fifo("set/pipe");
        assert_eq!(found(&scratch, &LINT_SET), ["t.fires"]);
        assert_eq!(scratch.run(&["test", "set"]).code, 0);
        for name in ["b.yml", "b.test.yml"] {
            let path = format!("set/{name}");
            fifo(&path);
            for (args, root) in [
                (&LINT_SET[..], "ROOT/"),
                (&["fix", "--rules", "set", "thing.yaml"], "ROOT/"),
                (&["check", "--rule", "set", "thing.yaml"], ""),
                (&["test", "set"], ""),
                (&["rules", "add", "set"], "ROOT/"),
            ] {
                assert_eq!(
                    scratch.run(args),
                    stopped(&format!("{root}{path}"), "not a regular file"),
                    "{args:?}"
                );
            }
            std::fs::remove_file(scratch.path(&path)).expect("rm");
        }
        // Named itself, as a rule file.
        fifo("b.yml");
        let run = scratch.run(&["lint", "--rules", "b.yml", "thing.yaml"]);
        assert_eq!(run, stopped("ROOT/b.yml", "not a regular file"));
    }

    // -----------------------------------------------------------------------
    // `rules add` and the links it finds
    // -----------------------------------------------------------------------

    /// Everything in the directory, no link followed: a directory's path, a
    /// link's path and target, a file's path and text.
    fn tree(scratch: &Scratch) -> Vec<String> {
        fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("read dir") {
                let path = entry.expect("entry").path();
                let name = path.strip_prefix(root).expect("under root").display();
                if path.is_symlink() {
                    let target = std::fs::read_link(&path).expect("read link");
                    out.push(format!("{name} -> {}", target.display()));
                } else if path.is_dir() {
                    out.push(format!("{name}/"));
                    walk(root, &path, out);
                } else {
                    let text = std::fs::read_to_string(&path).expect("read");
                    out.push(format!("{name}: {text:?}"));
                }
            }
        }
        let mut out = Vec::new();
        walk(&scratch.path(""), &scratch.path(""), &mut out);
        out.sort();
        out
    }

    /// `rules add SOURCE`, as a copy and as a link, with and without
    /// `--force`.
    fn adds(source: &str) -> Vec<Vec<&str>> {
        [
            &[][..],
            &["--symlink"],
            &["--force"],
            &["--symlink", "--force"],
        ]
        .map(|flags| [&["rules", "add", source], flags].concat())
        .to_vec()
    }

    /// Every way to add `source` is refused for `dest`, and nothing changes.
    fn refuses(scratch: &Scratch, source: &str, dest: &str) {
        let before = tree(scratch);
        for args in adds(source) {
            let message = format!(
                "cannot add ROOT/{source} to ROOT/{dest}: \
                 one is inside the other, or they are the same\n"
            );
            assert_eq!(scratch.run(&args), failed(6, &message), "{args:?}");
            assert_eq!(tree(scratch), before, "{args:?}");
        }
    }

    #[test]
    fn rules_add_refuses_a_source_that_is_the_destination_or_in_it_or_around_it() {
        // The link leads to the source, above it, or into it.
        for target in ["../../src", "../..", "../../src/sub"] {
            let scratch = Scratch::new(&[("src/a.yml", FIRES), ("src/sub/b.yml", FIRES)]);
            scratch.link(target, ".dq/rules/src");
            refuses(&scratch, "src", ".dq/rules/src");
        }
        // The same for a rule file.
        let scratch = Scratch::new(&[("src/a.yml", FIRES)]);
        scratch.link("../../src", ".dq/rules/a");
        refuses(&scratch, "src/a.yml", ".dq/rules/a");
        // With nothing in it to add.
        let scratch = Scratch::new(&[(".dq/rules/ns/ns/notes.txt", "none yet")]);
        refuses(&scratch, ".dq/rules/ns/ns", ".dq/rules/ns");
        // No link at all.
        let scratch = Scratch::new(&[(".dq/rules/ns/a.yml", FIRES)]);
        refuses(&scratch, ".dq/rules/ns", ".dq/rules/ns");
        refuses(&scratch, ".dq/rules", ".dq/rules/rules");
        let scratch = Scratch::new(&[(".dq/rules/ns/ns/a.yml", FIRES)]);
        refuses(&scratch, ".dq/rules/ns/ns", ".dq/rules/ns");
        // `.dq/rules` is in the source.
        let scratch = Scratch::new(&[("src/a.yml", FIRES)]);
        scratch.link("../src", ".dq/rules");
        refuses(&scratch, "src", ".dq/rules/src");
    }

    #[test]
    fn rules_add_refuses_a_source_that_a_link_takes_into_the_destination() {
        // A directory in the destination is one of the source.
        let scratch = Scratch::new(&[("src/sub/a.yml", FIRES)]);
        scratch.link("../../../src/sub", ".dq/rules/src/sub");
        refuses(&scratch, "src", ".dq/rules/src");
        // A file of the source is one of the destination, or leads through it.
        for real in ["real.yml", ".dq/rules/src/real.yml"] {
            let scratch = Scratch::new(&[(real, FIRES)]);
            let real = scratch.path(real);
            scratch.link(real.to_str().expect("UTF-8 path"), ".dq/rules/src/a.yml");
            scratch.link("../.dq/rules/src/a.yml", "src/a.yml");
            refuses(&scratch, "src", ".dq/rules/src");
        }
    }

    #[test]
    fn rules_add_takes_a_link_for_what_is_there_and_replaces_it_when_forced() {
        // At the ruleset's directory, at a directory in it, at a file: to
        // something outside, to nothing, to itself.
        for (link, name) in [
            (".dq/rules/src", "src"),
            (".dq/rules/src/sub", "sub"),
            (".dq/rules/src/sub/a.yml", "a.yml"),
        ] {
            for target in ["outside", "gone", name] {
                for symlinks in [false, true] {
                    let scratch = Scratch::new(&[
                        ("src/sub/a.yml", FIRES),
                        ("outside/sub/a.yml", "not mine"),
                        ("outside/a.yml", "not mine"),
                    ]);
                    let outside = scratch.path(target);
                    let target = if target == name {
                        target
                    } else {
                        outside.to_str().expect("UTF-8 path")
                    };
                    scratch.link(target, link);
                    let before = tree(&scratch);
                    let flags: &[&str] = if symlinks { &["--symlink"] } else { &[] };
                    let add = [&["rules", "add", "src"], flags].concat();
                    assert_eq!(
                        scratch.run(&add),
                        failed(
                            6,
                            &format!(
                                "destination already exists: ROOT/{link} \
                                 (pass --force to overwrite)\n"
                            )
                        ),
                        "{target}"
                    );
                    assert_eq!(tree(&scratch), before, "{link} -> {target}");

                    let run = scratch.run(&[&add[..], &["--force"]].concat());
                    assert_eq!((run.code, run.stderr.as_str()), (0, ""), "{target}");
                    let mut after: Vec<String> = before
                        .into_iter()
                        .filter(|entry| !entry.starts_with(".dq/rules/src"))
                        .collect();
                    let source = scratch.path("src/sub/a.yml");
                    after.push(".dq/rules/src/".to_owned());
                    after.push(".dq/rules/src/sub/".to_owned());
                    after.push(if symlinks {
                        format!(".dq/rules/src/sub/a.yml -> {}", real(&source).display())
                    } else {
                        format!(".dq/rules/src/sub/a.yml: {FIRES:?}")
                    });
                    after.sort();
                    let now: Vec<String> = tree(&scratch);
                    assert_eq!(now, after, "{link} -> {target}");
                }
            }
        }
    }

    /// `path` as the working directory of `dq` spells it.
    fn real(path: &std::path::Path) -> std::path::PathBuf {
        std::fs::canonicalize(path).expect("canonicalize")
    }

    #[test]
    fn rules_add_std_does_not_write_through_a_link_either() {
        for target in ["outside", "gone"] {
            let scratch = Scratch::new(&[("outside/latest.yml", "not mine")]);
            scratch.link(&format!("../../{target}"), ".dq/rules/npm");
            let before = tree(&scratch);
            let message =
                "destination already exists: ROOT/.dq/rules/npm (pass --force to overwrite)\n";
            for flags in [&[][..], &["--symlink"]] {
                let run = scratch.run(&[&["rules", "add", "@std/npm"], flags].concat());
                assert_eq!(run, failed(6, message), "{flags:?}");
                assert_eq!(tree(&scratch), before, "{flags:?}");
            }
            assert_eq!(
                scratch.run(&["rules", "add", "@std/npm", "--force"]).code,
                0
            );
            assert!(!scratch.path(".dq/rules/npm").is_symlink());
            assert!(!scratch.path("gone").exists());
            assert_eq!(read(&scratch, "outside/latest.yml"), "not mine");
            let outside = std::fs::read_dir(scratch.path("outside")).expect("read dir");
            assert_eq!(outside.count(), 1);
        }
    }

    #[test]
    fn rules_add_again_replaces_its_own_links() {
        let scratch = Scratch::new(&[("thing.yaml", THING), ("src/a.yml", FIRES)]);
        for source in ["src", "src/a.yml"] {
            let first = scratch.run(&["rules", "add", source, "--symlink"]);
            assert_eq!(first.code, 0, "{first:?}");
            let before = tree(&scratch);
            let again = scratch.run(&["rules", "add", source, "--symlink", "--force"]);
            assert_eq!(again.code, 0, "{again:?}");
            assert_eq!(tree(&scratch), before);
            let copy = scratch.run(&["rules", "add", source, "--force"]);
            assert_eq!(copy.code, 0, "{copy:?}");
            assert_eq!(read(&scratch, "src/a.yml"), FIRES);
        }
        for copy in [".dq/rules/src/a.yml", ".dq/rules/a/a.yml"] {
            assert!(!scratch.path(copy).is_symlink());
            assert_eq!(read(&scratch, copy), FIRES);
        }
        // A link to the rule directory is where it is, not in the way.
        let scratch = Scratch::new(&[("src/a.yml", FIRES), ("shared/b.yml", FIRES)]);
        scratch.link("../shared", ".dq/rules");
        assert_eq!(scratch.run(&["rules", "add", "src"]).code, 0);
        assert_eq!(read(&scratch, "shared/src/a.yml"), FIRES);
    }

    #[test]
    fn rules_add_looks_at_everything_before_it_writes() {
        let scratch = Scratch::new(&[
            ("src/a.yml", FIRES),
            ("src/b.yml", FIRES),
            ("src/z/c.yml", FIRES),
            (".dq/rules/src/b.yml", "mine"),
        ]);
        let before = tree(&scratch);
        let message =
            "destination already exists: ROOT/.dq/rules/src/b.yml (pass --force to overwrite)\n";
        assert_eq!(scratch.run(&["rules", "add", "src"]), failed(6, message));
        assert_eq!(tree(&scratch), before);
        // Not for `--force` to replace.
        scratch.write(".dq/rules/src/a.yml/x", "mine");
        scratch.write(".dq/rules/src/z", "mine");
        let before = tree(&scratch);
        let add = ["rules", "add", "src", "--force"];
        let message = "destination is a directory: ROOT/.dq/rules/src/a.yml\n";
        assert_eq!(scratch.run(&add), failed(6, message));
        assert_eq!(tree(&scratch), before);
        std::fs::remove_file(scratch.path(".dq/rules/src/a.yml/x")).expect("rm");
        std::fs::remove_dir(scratch.path(".dq/rules/src/a.yml")).expect("rmdir");
        let before = tree(&scratch);
        let message = "destination is not a directory: ROOT/.dq/rules/src/z\n";
        assert_eq!(scratch.run(&add), failed(6, message));
        assert_eq!(tree(&scratch), before);
    }

    #[test]
    fn rules_add_refuses_a_name_that_leads_out_of_the_rules() {
        // The stem of `...yml` is `..`, of `..yml` it is `.`.
        for name in ["...yml", "..yml", "....yml"] {
            for rules in [false, true] {
                let scratch = Scratch::new(&[
                    ("victim", "not mine"),
                    (&format!("src/{name}"), FIRES),
                    (".dq/keep", ""),
                ]);
                scratch.link("../victim", &format!(".dq/{name}"));
                if rules {
                    scratch.write(".dq/rules/a.yml", FIRES);
                    scratch.link("../../victim", &format!(".dq/rules/{name}"));
                }
                let before = tree(&scratch);
                let source = format!("src/{name}");
                let stem = name.strip_suffix(".yml").expect("a stem");
                let message = format!(
                    "cannot add ROOT/{source}: '{stem}' does not name a directory for it in \
                     .dq/rules\n"
                );
                for args in adds(&source) {
                    assert_eq!(scratch.run(&args), failed(6, &message), "{args:?}");
                    assert_eq!(tree(&scratch), before, "{args:?}");
                }
            }
        }
        // `@std` is a list of names, none of them a path.
        for name in ["..", ".", "", "../npm", "npm/..", "npm/", "/npm", "./npm"] {
            let scratch = Scratch::new(&[(".dq/keep", "")]);
            let before = tree(&scratch);
            let source = format!("@std/{name}");
            for args in adds(&source) {
                let message = format!("unknown @std namespace: {name}\n");
                assert_eq!(scratch.run(&args), failed(6, &message), "{args:?}");
                assert_eq!(tree(&scratch), before, "{args:?}");
            }
        }
        // A directory is added under its own name, however it is reached.
        for source in ["src/ns", "src/ns/", "src/./ns/.", "src/../src/ns"] {
            let scratch = Scratch::new(&[("src/ns/a.yml", FIRES)]);
            assert_eq!(scratch.run(&["rules", "add", source]).code, 0, "{source}");
            assert_eq!(read(&scratch, ".dq/rules/ns/a.yml"), FIRES, "{source}");
        }
        // Odd names that stay inside.
        for (name, dest) in [
            (".yml", ".yml/.yml"),
            ("..a.yml", "..a/..a.yml"),
            ("a\\b.yml", "a\\b/a\\b.yml"),
        ] {
            let source = format!("src/{name}");
            let scratch = Scratch::new(&[(&source, FIRES)]);
            assert_eq!(scratch.run(&["rules", "add", &source]).code, 0, "{name}");
            let dest = format!(".dq/rules/{dest}");
            assert_eq!(read(&scratch, &dest), FIRES, "{name}");
        }
    }

    #[test]
    fn rules_add_may_replace_a_link_on_the_way_to_its_source() {
        // Where the source is, as typed, once the link is gone: nowhere.
        for (source, link, file) in [
            ("src/../x/src", ".dq/rules/src", "x/src/a.yml"),
            ("src/../x/src", ".dq/rules/src", "x/src/sub/a.yml"),
            ("src/l/../x/src", ".dq/rules/src/l", "x/src/a.yml"),
            ("a/../x/a.yml", ".dq/rules/a", "x/a.yml"),
        ] {
            let source = format!(".dq/rules/{source}");
            for symlinks in [false, true] {
                let scratch = Scratch::new(&[("ext/keep", ""), (file, FIRES)]);
                let target = scratch.path("ext");
                scratch.link(target.to_str().expect("UTF-8 path"), link);
                let name = if link.ends_with("/l") { "l" } else { "" };
                if !name.is_empty() {
                    // In the way: the source has a directory of that name.
                    scratch.write("x/src/l/b.yml", FIRES);
                }
                let before = tree(&scratch);
                let flags: &[&str] = if symlinks { &["--symlink"] } else { &[] };
                let add = [&["rules", "add", &source], flags].concat();
                let run = scratch.run(&add);
                assert_eq!((run.code, run.stdout.as_str()), (6, ""), "{run:?}");
                assert_eq!(tree(&scratch), before, "{add:?}");

                let run = scratch.run(&[&add[..], &["--force"]].concat());
                assert_eq!((run.code, run.stderr.as_str()), (0, ""), "{add:?}");
                assert!(!scratch.path(link).is_symlink());
                let added = file.strip_prefix("x/").expect("under x");
                let added = if link.ends_with("/a") {
                    ".dq/rules/a/a.yml".to_owned()
                } else {
                    format!(".dq/rules/{added}")
                };
                assert_eq!(read(&scratch, &added), FIRES, "{add:?}");
                let at = scratch.path(&added);
                assert_eq!(at.is_symlink(), symlinks, "{add:?}");
                if symlinks {
                    // To the file itself, by an absolute path.
                    let target = std::fs::read_link(&at).expect("read link");
                    assert_eq!(target, real(&scratch.path(file)));
                }
                assert_eq!(read(&scratch, file), FIRES);
            }
        }
        // The links of the source go the same way.
        let scratch = Scratch::new(&[("ext/keep", ""), ("x/a.yml", FIRES), ("src/b.yml", FIRES)]);
        scratch.link("../../../ext", ".dq/rules/src/sub");
        scratch.link("../.dq/rules/src/sub/../x/a.yml", "src/a.yml");
        scratch.write("src/sub/c.yml", FIRES);
        let run = scratch.run(&["rules", "add", "src", "--symlink", "--force"]);
        assert_eq!((run.code, run.stderr.as_str()), (0, ""));
        for name in ["a.yml", "b.yml", "sub/c.yml"] {
            assert_eq!(read(&scratch, &format!(".dq/rules/src/{name}")), FIRES);
        }
        let target = std::fs::read_link(scratch.path(".dq/rules/src/a.yml")).expect("read link");
        assert_eq!(target, real(&scratch.path("x/a.yml")));
    }

    #[test]
    fn rules_add_reads_every_file_before_it_removes_one() {
        let scratch = Scratch::new(&[
            ("src/a.yml", FIRES),
            ("src/a.test.yml", FIXTURE),
            (".dq/rules/src/a.yml", "mine"),
        ]);
        let mode = |mode| {
            let permissions = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(scratch.path("src/a.test.yml"), permissions).expect("chmod");
        };
        mode(0o000);
        let root = std::fs::read(scratch.path("src/a.test.yml")).is_ok();
        let run = scratch.run(&["rules", "add", "src", "--force"]);
        mode(0o644);
        // Root reads everything.
        if !root {
            assert_eq!((run.code, run.stdout.as_str()), (5, ""));
            let why = "src/a.test.yml': Permission denied";
            assert!(run.stderr.contains(why), "{run:?}");
            assert_eq!(read(&scratch, ".dq/rules/src/a.yml"), "mine");
        }
    }

    #[test]
    fn rules_add_refuses_a_destination_that_is_the_source_by_one_link_alone() {
        // A directory of the destination is one the source is around.
        let scratch = Scratch::new(&[("src/sub/a.yml", FIRES), ("src/other/b.yml", FIRES)]);
        scratch.link("../../../src/other", ".dq/rules/src/sub");
        refuses(&scratch, "src", ".dq/rules/src");
        // It is where a file of the source really is, outside the source.
        let scratch = Scratch::new(&[("out/a.yml", FIRES), ("src/b.yml", FIRES)]);
        scratch.link("../../../out", ".dq/rules/src/sub");
        scratch.link("../../out/a.yml", "src/sub/a.yml");
        refuses(&scratch, "src", ".dq/rules/src");
        // The destination is a link that leads out of the source it is in.
        let scratch = Scratch::new(&[("out/keep", ""), ("p/b.yml", FIRES)]);
        scratch.link("../p/rules", ".dq/rules");
        scratch.link("../../out", "p/rules/p");
        refuses(&scratch, "p", ".dq/rules/p");
        // A file of the source leads through it.
        let scratch = Scratch::new(&[("real.yml", FIRES), ("src/b.yml", FIRES)]);
        scratch.link("../../real.yml", ".dq/rules/src");
        scratch.link("../.dq/rules/src", "src/a.yml");
        refuses(&scratch, "src", ".dq/rules/src");
    }

    // -----------------------------------------------------------------------
    // Links on the way, and what may not be read behind them
    // -----------------------------------------------------------------------

    /// `DIR/00` to `DIR/59`, each with a rule, its fixture and a link `next`
    /// to the one after it; and the ids of the rules in that order.
    fn chain(scratch: &Scratch, dir: &str) -> Vec<String> {
        for n in 0..60 {
            let rule = format!(
                "id: t.r{n:02}\ndescription: d\nseverity: error\nmatch:\n  format: yaml\n\
                 check:\n  schema_file: s.json\n"
            );
            scratch.write(&format!("{dir}/{n:02}/r.yml"), &rule);
            scratch.write(&format!("{dir}/{n:02}/s.json"), "{\"type\": \"string\"}");
            let fixture = FIXTURE.replace("t.fires", &format!("t.r{n:02}"));
            scratch.write(&format!("{dir}/{n:02}/r.test.yml"), &fixture);
            if n < 59 {
                scratch.link(&format!("../{:02}", n + 1), &format!("{dir}/{n:02}/next"));
            }
        }
        (0..60).map(|n| format!("t.r{n:02}")).collect()
    }

    #[test]
    fn more_links_than_a_system_follows_lead_to_every_rule() {
        for (dir, reversed) in [("set", false), ("out", true)] {
            let scratch = Scratch::new(&[("thing.yaml", THING), ("s.json", "{}")]);
            // In the set, each directory is there without a link. Outside it
            // by way of the links alone, and named after them.
            let mut ids = chain(&scratch, dir);
            if reversed {
                scratch.link("../out/00", "set/next");
                ids.reverse();
            }
            // The base of a `schema_file` is the directory of the run.
            scratch.write("set/s.json", "{\"type\": \"string\"}");
            let run = scratch.run(&["test", "set"]);
            assert_eq!(
                (run.code, run.stdout.lines().last(), run.stderr.as_str()),
                (
                    0,
                    Some("summary: 60 passed, 0 failed, 0 errored, 60 total"),
                    ""
                ),
                "{dir}"
            );
            assert_eq!(found(&scratch, &LINT_SET), ids, "{dir}");
            for flags in [&[][..], &["--symlink", "--force"]] {
                let run = scratch.run(&[&["rules", "add", "set"], flags].concat());
                assert_eq!((run.code, run.stderr.as_str()), (0, ""), "{dir}");
                scratch.write(".dq/rules/s.json", "{\"type\": \"string\"}");
                assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ids, "{dir}");
            }
        }
    }

    #[test]
    fn a_directory_that_cannot_be_read_behind_a_link_stops_no_test_run() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("set/a.yml", FIRES),
            ("set/a.test.yml", FIXTURE),
            ("out/b.yml", FIRES),
            ("out/b.test.yml", FIXTURE),
            ("out/closed/dir/c.yml", FIRES),
            ("real/closed/c.yml", FIRES),
        ]);
        scratch.link("out", "linked");
        let mode = |path: &str, mode| {
            let permissions = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(scratch.path(path), permissions).expect("chmod");
        };
        mode("out/closed", 0o000);
        mode("real/closed", 0o000);
        let root = std::fs::read_dir(scratch.path("out/closed")).is_ok();
        // Below a link, at a link, and past what a link may look at.
        let mut runs: Vec<(&str, &str, Run, Run, Run)> = Vec::new();
        for (target, name) in [
            ("../out", "result/closed"),
            ("../out/closed", "closed"),
            ("../out/closed/dir", "dir"),
        ] {
            let link = format!("set/{}", name.split('/').next().expect("a name"));
            scratch.link(target, &link);
            runs.push((
                target,
                name,
                scratch.run(&["test", "set"]),
                scratch.run(&LINT_SET),
                scratch.run(&["rules", "add", "set"]),
            ));
            std::fs::remove_file(scratch.path(&link)).expect("rm");
        }
        // Named as a fixture, it may be one that does not run.
        scratch.link("../out/closed/dir/c.yml", "set/c.test.yml");
        let fixture = scratch.run(&["test", "set"]);
        std::fs::remove_file(scratch.path("set/c.test.yml")).expect("rm");
        // Without a link, and the directory of the run itself by one.
        let real = scratch.run(&["test", "real"]);
        scratch.link("../out/closed", "real/itself");
        let itself = scratch.run(&["test", "real/itself"]);
        let linked = scratch.run(&["test", "linked"]);
        mode("out/closed", 0o755);
        mode("real/closed", 0o755);
        if root {
            // Root reads everything.
            return;
        }
        for (target, name, test, lint, add) in runs {
            assert_eq!(test.code, 0, "{target}: {test:?}");
            let passed = if target == "../out" { 2 } else { 1 };
            let summary = format!("summary: {passed} passed, 0 failed, 0 errored, {passed} total");
            assert_eq!(test.stdout.lines().last(), Some(summary.as_str()));
            assert_eq!(
                test.stderr,
                format!("WARN skipped set/{name}: Permission denied (os error 13)\n")
            );
            for run in [lint, add] {
                let why = "Permission denied (os error 13)";
                assert_eq!(run, stopped(&format!("ROOT/set/{name}"), why), "{target}");
            }
            assert!(!scratch.path(".dq").exists(), "{target}");
        }
        for (run, path) in [
            (fixture, "set/c.test.yml"),
            (real, "real/closed"),
            (itself, "real/itself"),
            (linked, "linked/closed"),
        ] {
            assert_eq!(run, stopped(path, "Permission denied (os error 13)"));
        }
    }

    #[test]
    fn a_link_that_cannot_be_followed_stops_a_lint_and_no_test_run() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("set/a.yml", FIRES),
            ("set/a.test.yml", FIXTURE),
            ("out/c00", FIRES),
        ]);
        // More than any system follows in one go.
        for n in 1..80 {
            scratch.link(&format!("c{:02}", n - 1), &format!("out/c{n:02}"));
        }
        let long = "x".repeat(300);
        // Through a file, to a name no file can have, by too many links, to
        // itself.
        for target in ["a.yml/x", &long, "../out/c79", "itself"] {
            let link = if target == "itself" { target } else { "notes" };
            scratch.link(target, &format!("set/{link}"));
            let run = scratch.run(&["test", "set"]);
            let passed =
                "ok set/a.test.yml: thing\nsummary: 1 passed, 0 failed, 0 errored, 1 total\n";
            assert_eq!((run.code, run.stdout.as_str()), (0, passed), "{target}");
            let skipped = format!("WARN skipped set/{link}: ");
            assert!(run.stderr.starts_with(&skipped), "{run:?}");
            assert_eq!(run.stderr.lines().count(), 1, "{run:?}");
            // Any of them may have been a directory of rules.
            for args in [&LINT_SET[..], &["rules", "add", "set"]] {
                let run = scratch.run(args);
                assert_eq!((run.code, run.stdout.as_str()), (5, ""), "{target}");
                let why = format!("io error reading ROOT/set/{link}: ");
                assert!(run.stderr.starts_with(&why), "{run:?}");
            }
            std::fs::remove_file(scratch.path(&format!("set/{link}"))).expect("rm");
            for name in ["b.yml", "b.test.YAML"] {
                scratch.link(target, &format!("set/{name}"));
                let run = scratch.run(&["test", "set"]);
                assert_eq!((run.code, run.stdout.as_str()), (5, ""), "{target}");
                let why = format!("io error reading set/{name}: ");
                assert!(run.stderr.starts_with(&why), "{run:?}");
                std::fs::remove_file(scratch.path(&format!("set/{name}"))).expect("rm");
            }
        }
        assert_eq!(found(&scratch, &LINT_SET), ["t.fires"]);
    }

    // -----------------------------------------------------------------------
    // `rules add` that fails, and what it leaves
    // -----------------------------------------------------------------------

    /// The mode of a file, followed.
    fn mode_of(path: &std::path::Path) -> u32 {
        let metadata = std::fs::metadata(path).expect("metadata");
        metadata.permissions().mode() & 0o7777
    }

    fn set_mode(scratch: &Scratch, path: &str, mode: u32) {
        let permissions = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(scratch.path(path), permissions).expect("chmod");
    }

    /// `src`, and an older `src` added in `.dq/rules`, with a rule more.
    fn added() -> Scratch {
        Scratch::new(&[
            ("thing.yaml", THING),
            ("src/a.yml", &fires("t.new-a")),
            ("src/sub/b.yml", &fires("t.new-b")),
            (".dq/rules/src/a.yml", &fires("t.old-a")),
            (".dq/rules/src/sub/b.yml", &fires("t.old-b")),
            (".dq/rules/src/z.yml", &fires("t.old-z")),
        ])
    }

    const OLD: [&str; 3] = ["t.old-a", "t.old-b", "t.old-z"];
    const NEW: [&str; 3] = ["t.new-a", "t.new-b", "t.old-z"];

    #[test]
    fn rules_add_that_may_not_write_leaves_every_rule_as_it_was() {
        for flags in [&["--force"][..], &["--symlink", "--force"]] {
            let scratch = added();
            let add = [&["rules", "add", "src"], flags].concat();
            let before = tree(&scratch);
            set_mode(&scratch, ".dq/rules/src/sub", 0o555);
            let root = std::fs::write(scratch.path(".dq/rules/src/sub/probe"), "").is_ok();
            let run = scratch.run(&add);
            let after = tree(&scratch);
            set_mode(&scratch, ".dq/rules/src/sub", 0o755);
            if root {
                // Root writes everywhere.
                return;
            }
            let failed_at = |path: &str| {
                failed(
                    7,
                    &format!(
                        "I/O error writing 'ROOT/{path}': Permission denied (os error 13); \
                         nothing was replaced\n"
                    ),
                )
            };
            assert_eq!(run, failed_at(".dq/rules/src/sub/b.yml"), "{flags:?}");
            assert_eq!(after, before, "{flags:?}");
            assert_eq!(found(&scratch, &["lint", "thing.yaml"]), OLD);

            // The same where the directory is one to make.
            scratch.write("src/0/c.yml", &fires("t.new-c"));
            set_mode(&scratch, ".dq/rules/src", 0o555);
            let run = scratch.run(&add);
            let after = tree(&scratch);
            set_mode(&scratch, ".dq/rules/src", 0o755);
            assert_eq!(run, failed_at(".dq/rules/src/0/c.yml"), "{flags:?}");
            assert_eq!(after, before_with(&before, &scratch), "{flags:?}");
            std::fs::remove_file(scratch.path("src/0/c.yml")).expect("rm");

            // With the mode back, what the new set does not have stays.
            assert_eq!(scratch.run(&add).code, 0);
            assert_eq!(found(&scratch, &["lint", "thing.yaml"]), NEW);
            let hidden = |entry: &&String| entry.contains("/.dq-");
            let rules: Vec<String> = tree(&scratch)
                .into_iter()
                .filter_map(|entry| Some(entry.strip_prefix(".dq/")?.to_owned()))
                .collect();
            assert_eq!(rules.iter().find(hidden), None, "{flags:?}");
        }
    }

    /// `before`, and `src/0` in it.
    fn before_with(before: &[String], scratch: &Scratch) -> Vec<String> {
        let mut entries = before.to_vec();
        entries.push("src/0/".to_owned());
        let text = read(scratch, "src/0/c.yml");
        entries.push(format!("src/0/c.yml: {text:?}"));
        entries.sort();
        entries
    }

    /// [`Scratch::run`] with no file it writes allowed past some KiB, as on a
    /// disk that is full.
    fn run_on_a_full_disk(scratch: &Scratch, args: &[&str]) -> Run {
        let out = assert_cmd::Command::new("sh")
            .args(["-c", "trap '' XFSZ; ulimit -f 16; exec \"$@\"", "sh"])
            .arg(env!("CARGO_BIN_EXE_dq"))
            .arg("--no-color")
            .args(args)
            .current_dir(scratch.path(""))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", scratch.path(""))
            .timeout(scratch::DEADLINE)
            .output()
            .expect("run sh");
        let root = real(&scratch.path(""));
        let stderr = String::from_utf8(out.stderr).expect("UTF-8 stderr");
        Run {
            code: out.status.code().expect("an exit code"),
            stdout: String::from_utf8(out.stdout).expect("UTF-8 stdout"),
            stderr: stderr.replace(root.to_str().expect("UTF-8 path"), "ROOT"),
        }
    }

    #[test]
    fn rules_add_that_cannot_write_it_all_leaves_every_rule_as_it_was() {
        let full = |path: &str| {
            failed(
                7,
                &format!(
                    "I/O error writing 'ROOT/{path}': File too large (os error 27); \
                     nothing was replaced\n"
                ),
            )
        };
        let add = ["rules", "add", "src", "--force"];
        let big = format!("{}# {}\n", fires("t.new-b"), "x".repeat(200_000));
        // The old files, replaced one by one.
        let scratch = added();
        scratch.write("src/sub/b.yml", &big);
        let before = tree(&scratch);
        let run = run_on_a_full_disk(&scratch, &add);
        assert_eq!(run, full(".dq/rules/src/sub/b.yml"));
        assert_eq!(tree(&scratch), before);
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), OLD);
        assert_eq!(scratch.run(&add).code, 0);
        assert_eq!(found(&scratch, &["lint", "thing.yaml"]), NEW);

        // Old rules behind a link that is replaced as a whole.
        for (target, link) in [
            ("../../old/src", ".dq/rules/src"),
            ("../../../old/src/sub", ".dq/rules/src/sub"),
        ] {
            let scratch = Scratch::new(&[
                ("thing.yaml", THING),
                ("src/a.yml", &fires("t.new-a")),
                ("src/sub/b.yml", &big),
                ("old/src/a.yml", &fires("t.old-a")),
                ("old/src/sub/b.yml", &fires("t.old-b")),
            ]);
            scratch.link(target, link);
            let before = tree(&scratch);
            let old = found(&scratch, &["lint", "thing.yaml"]);
            let run = run_on_a_full_disk(&scratch, &add);
            assert_eq!(run, full(".dq/rules/src/sub/b.yml"), "{link}");
            assert_eq!(tree(&scratch), before, "{link}");
            assert_eq!(found(&scratch, &["lint", "thing.yaml"]), old);
            assert_eq!(scratch.run(&add).code, 0);
            assert_eq!(
                found(&scratch, &["lint", "thing.yaml"]),
                ["t.new-a", "t.new-b"]
            );
        }

        // With nothing there yet, no rule is added.
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            ("src/a.yml", FIRES),
            ("src/b.yml", &big),
        ]);
        let run = run_on_a_full_disk(&scratch, &add);
        assert_eq!(run, full(".dq/rules/src/b.yml"));
        assert_eq!(scratch.run(&["lint", "thing.yaml"]).code, 0);
    }

    #[test]
    fn rules_add_makes_files_with_the_mode_they_had_with_a_plain_copy() {
        let scratch = Scratch::new(&[("src/a.yml", FIRES), ("src/sub/b.yml", FIRES)]);
        set_mode(&scratch, "src/a.yml", 0o640);
        set_mode(&scratch, "src/sub/b.yml", 0o444);
        for flags in [&[][..], &["--force"]] {
            let run = scratch.run(&[&["rules", "add", "src"], flags].concat());
            assert_eq!((run.code, run.stderr.as_str()), (0, ""));
            for (path, mode) in [("a.yml", 0o640), ("sub/b.yml", 0o444)] {
                let added = scratch.path(&format!(".dq/rules/src/{path}"));
                assert_eq!(mode_of(&added), mode, "{path}");
            }
        }
        // An embedded file is as any the user makes.
        assert_eq!(scratch.run(&["rules", "add", "@std/npm"]).code, 0);
        scratch.write("plain", "");
        for entry in std::fs::read_dir(scratch.path(".dq/rules/npm")).expect("read dir") {
            let path = entry.expect("entry").path();
            assert_eq!(mode_of(&path), mode_of(&scratch.path("plain")), "{path:?}");
        }
        // A directory in place of a link, too.
        let scratch = Scratch::new(&[("src/a.yml", FIRES)]);
        scratch.link("../../gone", ".dq/rules/src");
        assert_eq!(scratch.run(&["rules", "add", "src", "--force"]).code, 0);
        let mode = |path| mode_of(&scratch.path(path));
        assert_eq!(mode(".dq/rules/src"), mode(".dq/rules"));
    }

    // -----------------------------------------------------------------------
    // `rules add` of a rule with several names
    // -----------------------------------------------------------------------

    #[test]
    fn rules_add_takes_a_rule_with_several_names_once() {
        let files = |scratch: &Scratch| -> Vec<String> {
            let entries = tree(scratch).into_iter();
            let added = entries.filter_map(|entry| {
                let name = entry.strip_prefix(".dq/rules/set/")?;
                let end = name.find([':', ' ']).unwrap_or(name.len());
                Some(name[..end].to_owned())
            });
            added.filter(|name| !name.is_empty()).collect()
        };
        for flags in [&[][..], &["--symlink"]] {
            let scratch = Scratch::new(&[
                ("thing.yaml", THING),
                ("set/a.yml", FIRES),
                ("set/a.test.yml", FIXTURE),
                ("set/c.yml", &fires("t.c")),
                ("set/only.test.yml", FIXTURE),
            ]);
            // A second name with the same fixture, one without, one below.
            scratch.link("a.yml", "set/b.yml");
            scratch.link("a.test.yml", "set/b.test.yml");
            scratch.link("b.yml", "set/d.yaml");
            scratch.link("../a.yml", "set/z/a.yml");
            // A fixture of two rules goes with both.
            scratch.link("a.test.yml", "set/c.test.yml");
            let loaded = found(&scratch, &LINT_SET);
            assert_eq!(loaded, ["t.fires", "t.c"]);
            let add = [&["rules", "add", "set"], flags].concat();
            let run = scratch.run(&add);
            assert_eq!((run.code, run.stderr.as_str()), (0, ""), "{flags:?}");
            assert_eq!(found(&scratch, &["lint", "thing.yaml"]), loaded);
            assert_eq!(
                files(&scratch),
                [
                    "a.test.yml",
                    "a.yml",
                    "c.test.yml",
                    "c.yml",
                    "only.test.yml"
                ]
            );
        }
        // The name the loader uses is the first, with its fixture; another's
        // is left out, which is said where it is another file.
        for flags in [&[][..], &["--symlink"]] {
            let scratch = Scratch::new(&[
                ("thing.yaml", THING),
                ("set/b.yml", FIRES),
                ("set/b.test.yml", FIXTURE),
                ("set/a.test.yml", FIXTURE),
            ]);
            scratch.link("b.yml", "set/a.yml");
            let run = scratch.run(&[&["rules", "add", "set"], flags].concat());
            let left = "WARN left out ROOT/set/b.test.yml: its rule is added as \
                        ROOT/set/a.yml, the first of its names\n";
            assert_eq!((run.code, run.stderr.as_str()), (0, left), "{flags:?}");
            assert_eq!(files(&scratch), ["a.test.yml", "a.yml"]);
            assert_eq!(found(&scratch, &["lint", "thing.yaml"]), ["t.fires"]);
            let run = scratch.run(&["test", ".dq/rules"]);
            let summary = "summary: 1 passed, 0 failed, 0 errored, 1 total";
            assert_eq!((run.code, run.stdout.lines().last()), (0, Some(summary)));
            assert_eq!(read(&scratch, "set/b.yml"), FIRES);
        }
        // The same fixture beside another rule does not make up for it.
        let scratch = Scratch::new(&[
            ("set/a.yml", FIRES),
            ("set/b.test.yml", FIXTURE),
            ("set/c.yml", &fires("t.c")),
        ]);
        scratch.link("a.yml", "set/b.yml");
        scratch.link("b.test.yml", "set/c.test.yml");
        let run = scratch.run(&["rules", "add", "set"]);
        assert_eq!(run.code, 0, "{run:?}");
        assert!(run.stderr.contains("left out ROOT/set/b.test.yml: "));
        assert_eq!(files(&scratch), ["a.yml", "c.test.yml", "c.yml"]);
    }

    // -----------------------------------------------------------------------
    // What a `rules add` that was killed leaves
    // -----------------------------------------------------------------------

    /// The warning about the staging directory at `path`.
    fn unfinished(path: &str) -> String {
        format!(
            "WARN skipped {path}: left by a `dq rules add` that did not finish; \
             it may be deleted\n"
        )
    }

    #[test]
    fn what_a_killed_rules_add_left_is_no_rule() {
        let scratch = Scratch::new(&[
            ("thing.yaml", THING),
            (".dq/rules/src/a.yml", FIRES),
            (".dq/rules/src/a.test.yml", FIXTURE),
            (".dq/rules/src/.dq-aB3dE6/b.yml", &fires("t.left")),
            (".dq/rules/src/.dq-aB3dE6/sub/b.test.yml", "no fixture"),
            (".dq/rules/src/.dq-aB3dE7", "half a ru"),
        ]);
        // One that was staged as a link, to a source that is gone by now.
        scratch.link("../../../gone.yml", ".dq/rules/src/.dq-000000");
        let lint = |args: &[&str]| scratch.run(&[&["-F", "json", "lint"], args].concat());
        for (args, root) in [
            (&["thing.yaml"][..], "ROOT/"),
            (&["--rules", ".dq/rules", "thing.yaml"], "ROOT/"),
            (&["--rules", ".dq/rules/src", "thing.yaml"], "ROOT/"),
        ] {
            let run = lint(args);
            assert_eq!(run.code, 4, "{run:?}");
            let report: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON");
            let found = report["diagnostics"].as_array().expect("diagnostics");
            assert_eq!(found.len(), 1, "{run:?}");
            assert_eq!(found[0]["rule_id"], "t.fires");
            let left = unfinished(&format!("{root}.dq/rules/src/.dq-aB3dE6"));
            assert!(run.stderr.starts_with(&left), "{run:?}");
            assert_eq!(run.stderr.lines().count(), 2, "{run:?}");
        }
        let run = scratch.run(&["test", ".dq/rules"]);
        let summary = "summary: 1 passed, 0 failed, 0 errored, 1 total";
        assert_eq!((run.code, run.stdout.lines().last()), (0, Some(summary)));
        assert_eq!(run.stderr, unfinished(".dq/rules/src/.dq-aB3dE6"));
        // Nor is it one of a source: said once, and not copied.
        scratch.write("set/a.yml", FIRES);
        scratch.write("set/.dq-aB3dE6/b.yml", &fires("t.left"));
        let run = scratch.run(&["rules", "add", "set"]);
        let left = unfinished("ROOT/set/.dq-aB3dE6");
        assert_eq!((run.code, run.stderr), (0, left));
        let added = std::fs::read_dir(scratch.path(".dq/rules/set")).expect("read dir");
        assert_eq!(added.count(), 1);
        // It may not be read: nor has it to be.
        set_mode(&scratch, ".dq/rules/src/.dq-aB3dE6", 0o000);
        let run = lint(&["thing.yaml"]);
        set_mode(&scratch, ".dq/rules/src/.dq-aB3dE6", 0o755);
        assert_eq!(run.code, 4, "{run:?}");
    }

    #[test]
    fn a_name_that_only_looks_like_a_staging_name_is_a_rule() {
        let names = [
            ".dq-abcdef.yml",
            ".dq-rules/r.yml",
            ".dq-old-abcdef/r.yml",
            "x.dq-abcdef/r.yml",
            "a/.dq-abc/r.yml",
        ];
        let scratch = Scratch::new(&[("thing.yaml", THING)]);
        for name in names {
            scratch.write(&format!("set/{name}"), &fires(&format!("t.{name}")));
        }
        let found_ids = |args: &[&str]| {
            let mut ids = found(&scratch, args);
            ids.sort();
            ids
        };
        let mut expected = names.map(|name| format!("t.{name}"));
        expected.sort();
        assert_eq!(found_ids(&LINT_SET), expected);
        let run = scratch.run(&["rules", "add", "set"]);
        assert_eq!((run.code, run.stderr.as_str()), (0, ""));
        assert_eq!(found_ids(&["lint", "thing.yaml"]), expected);
        // One the user names, on the way to the rules or by a link.
        scratch.write("out/.dq-abcdef/set/r.yml", &fires("t.named"));
        scratch.link("../out/.dq-abcdef", "linked/x");
        for dir in ["out/.dq-abcdef", "out/.dq-abcdef/set", "linked"] {
            let args = ["lint", "--rules", dir, "thing.yaml"];
            assert_eq!(found(&scratch, &args), ["t.named"], "{dir}");
        }
        // None is added where it would never be loaded.
        for source in ["out/.dq-abcdef", "out/.dq-abcdef.yml"] {
            scratch.write("out/.dq-abcdef.yml", FIRES);
            let message = format!(
                "cannot add ROOT/{source}: '.dq-abcdef' does not name a directory for it in \
                 .dq/rules\n"
            );
            let run = scratch.run(&["rules", "add", source]);
            assert_eq!(run, failed(6, &message));
        }
    }

    #[test]
    fn rules_add_that_may_not_replace_a_link_names_it_alone() {
        let scratch = Scratch::new(&[("src/a.yml", FIRES), ("old/a.yml", FIRES)]);
        scratch.link("../../old", ".dq/rules/src");
        set_mode(&scratch, ".dq/rules", 0o555);
        let root = std::fs::write(scratch.path(".dq/rules/probe"), "").is_ok();
        let run = scratch.run(&["rules", "add", "src", "--force"]);
        set_mode(&scratch, ".dq/rules", 0o755);
        if root {
            // Root writes everywhere.
            return;
        }
        let message = "I/O error writing 'ROOT/.dq/rules/src': Permission denied (os error 13); \
                       nothing was replaced\n";
        assert_eq!(run, failed(7, message));
    }

    #[test]
    fn rules_add_keeps_the_mode_of_a_source_whatever_the_umask() {
        let scratch = Scratch::new(&[("src/a.yml", FIRES), ("src/b.yml", FIRES)]);
        set_mode(&scratch, "src/a.yml", 0o666);
        set_mode(&scratch, "src/b.yml", 0o600);
        for add in [&["src"][..], &["@std/npm"]] {
            let out = assert_cmd::Command::new("sh")
                .args(["-c", "umask 077; exec \"$@\"", "sh"])
                .arg(env!("CARGO_BIN_EXE_dq"))
                .args(["rules", "add"])
                .args(add)
                .current_dir(scratch.path(""))
                .env_clear()
                .env("HOME", scratch.path(""))
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .timeout(scratch::DEADLINE)
                .output()
                .expect("run sh");
            assert!(out.status.success(), "{out:?}");
        }
        let mode = |path| mode_of(&scratch.path(path));
        assert_eq!(mode(".dq/rules/src/a.yml"), 0o666);
        assert_eq!(mode(".dq/rules/src/b.yml"), 0o600);
        assert_eq!(mode(".dq/rules/npm/has-license.yml"), 0o600);
    }
}
