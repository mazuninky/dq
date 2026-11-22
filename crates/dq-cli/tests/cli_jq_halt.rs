//! `halt` and `halt_error` in an expression dq runs: a jq runtime error, not
//! the end of the process. jaq's own would end it with exit 0 (`halt`, or
//! `halt_error(0)`) before the report of a lint, and in the middle of a bulk
//! write.

mod scratch;

use pretty_assertions::assert_eq;
use scratch::{Run, Scratch};

const THING: &str = "kind: Thing\n";

/// Reports every thing; its fix sets `fixed`.
const FIRES: &str = r#"id: t.fires
description: d
severity: error
match:
  format: yaml
check:
  jq: 'select(.kind == "Thing")'
  message: is a thing
fix:
  ops: 'if .fixed then [] else [{"op":"add","path":"/fixed","value":true}] end'
"#;

const HALTS: [&str; 6] = [
    "halt",
    "halt(0)",
    "\"x\" | halt_error",
    "\"x\" | halt_error(0)",
    "try halt catch empty",
    "def halt_error: halt(0); halt_error",
];

const ERROR: &str =
    "jq runtime error: halt is not available: dq does not let an expression end the process";

/// A rule whose `match.filter` is `filter`, whose `check.jq` is `check` and
/// whose `fix` has `fix`.
fn rule(filter: &str, check: &str, fix: &str) -> String {
    let quoted = |text: &str| text.replace('\'', "''");
    format!(
        "id: t.halts\ndescription: d\nseverity: error\nmatch:\n  format: yaml\n  filter: '{}'\n\
         check:\n  jq: '{}'\n  message: m\nfix:\n  {fix}\n",
        quoted(filter),
        quoted(check),
    )
}

const FIX: &str = "ops: '[]'";

fn scratch(halts: &str) -> Scratch {
    Scratch::new(&[
        ("thing.yaml", THING),
        ("fires.yml", FIRES),
        ("halts.yml", halts),
    ])
}

/// stderr of a run in which t.halts did not run on thing.yaml.
fn did_not_run(field: &str) -> String {
    format!(
        "ERROR rule did not run rule_id=t.halts file=thing.yaml field={field} error={ERROR}\n\
         1 rule error(s): findings may be missing (--allow-rule-errors to accept)\n"
    )
}

/// The ids `run` reports as JSON.
fn ids(run: &Run) -> Vec<String> {
    let report: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON stdout");
    report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| d["rule_id"].as_str().expect("rule_id").to_owned())
        .collect()
}

/// `text` with the paths in it as they are spelled on Unix.
fn slashes(text: &str) -> String {
    text.replace('\\', "/")
}

#[test]
fn a_rule_that_halts_did_not_run_and_the_others_report() {
    for halt in HALTS {
        for (field, rule) in [
            ("check.jq", rule("true", halt, FIX)),
            ("match.filter", rule(halt, ".", FIX)),
        ] {
            let scratch = scratch(&rule);
            for order in [["fires.yml", "halts.yml"], ["halts.yml", "fires.yml"]] {
                let args = ["-F", "json", "lint", "--rules", order[0], "--rules"];
                let run = scratch.run(&[&args[..], &[order[1], "thing.yaml"]].concat());
                assert_eq!((run.code, run.stderr.as_str()), (3, &*did_not_run(field)));
                let mut ids = ids(&run);
                ids.sort();
                assert_eq!(ids, ["lint.rule_error", "t.fires"], "{halt}");
            }
            let args = ["-F", "json", "check", "--rule", "halts.yml", "thing.yaml"];
            let run = scratch.run(&args);
            assert_eq!((run.code, run.stderr.as_str()), (3, &*did_not_run(field)));
            assert_eq!(ids(&run), ["lint.rule_error"], "{halt}");
        }
    }
}

#[test]
fn fix_makes_the_other_fixes_and_fails() {
    let fixed = "kind: Thing\nfixed: true\n";
    for halt in HALTS {
        let scratch = scratch(&rule("true", halt, FIX));
        for (flags, text) in [(&["--check"][..], THING), (&["-i"], fixed)] {
            let args = ["fix", "--rules", "halts.yml", "--rules", "fires.yml"];
            let run = scratch.run(&[flags, &args, &["thing.yaml"]].concat());
            // Not applied yet is exit 1 of its own under `--check`, which a
            // rule error outranks.
            assert_eq!(run.code, 3, "{halt}: {run:?}");
            assert!(run.stderr.ends_with(&did_not_run("check.jq")), "{run:?}");
            let now = std::fs::read_to_string(scratch.path("thing.yaml")).expect("read");
            assert_eq!(now, text, "{halt}");
        }
    }
    // The fix itself halts: the file fails, and is left as it was.
    for fix in ["ops: halt", "jq: 'halt_error(0)'"] {
        let scratch = scratch(&rule("true", ".", fix));
        for flag in ["--check", "-i"] {
            let run = scratch.run(&[flag, "fix", "--rules", "halts.yml", "thing.yaml"]);
            assert_eq!(run.code, 3, "{fix}: {run:?}");
            assert!(run.stderr.contains(ERROR), "{run:?}");
            let now = std::fs::read_to_string(scratch.path("thing.yaml")).expect("read");
            assert_eq!(now, THING, "{fix}");
        }
    }
}

#[test]
fn a_rule_that_halts_fails_its_fixture() {
    let scratch = scratch(&rule("true", "halt", FIX));
    scratch.write(
        "halts.test.yml",
        "tests:\n  - name: clean\n    input: 'kind: Thing'\n    expected:\n      violations: []\n",
    );
    let run = scratch.run(&["test", "."]);
    assert_eq!(
        (run.code, slashes(&run.stdout)),
        (
            4,
            format!(
                "FAIL ./halts.test.yml: clean\n  raised: rule=t.halts check.jq: {ERROR}\n\
                 summary: 0 passed, 1 failed, 0 errored, 1 total\n"
            )
        )
    );
}

#[test]
fn a_place_or_an_extract_that_halts_keeps_the_finding() {
    let loc = rule("true", ".", FIX).replace("fix:", "loc:\n  line: halt\nfix:");
    let extract = "id: t.halts\ndescription: d\nseverity: error\nmatch:\n  format: yaml\ncheck:\n  \
                   extract: 'halt'\n  message: m\n  nested:\n    id: t.in\n    description: d\n    \
                   severity: error\n    match:\n      format: yaml\n    check:\n      jq: '.'\n      \
                   message: m\n";
    for (rule, id) in [
        (loc.as_str(), "t.halts"),
        (extract, "t.halts.composite-error"),
    ] {
        let args = ["-F", "json", "lint", "--rules", "halts.yml", "thing.yaml"];
        let run = scratch(rule).run(&args);
        assert_eq!(run.code, 4, "{run:?}");
        assert_eq!(ids(&run), [id]);
        assert!(run.stderr.contains(ERROR), "{run:?}");
    }
}

#[test]
fn query_fails_and_prints_no_log_line() {
    let scratch = scratch("");
    for halt in HALTS {
        let run = scratch.run(&["query", halt, "thing.yaml"]);
        let failed = Run {
            code: 1,
            stdout: String::new(),
            stderr: format!("{ERROR}\n"),
        };
        assert_eq!(run, failed, "{halt}");
    }
}

#[test]
fn set_jq_fails_the_file_the_expression_halts_on() {
    let files = [
        ("n/a.json", "{\"n\":1}\n"),
        ("n/b.json", "{\"n\":2,\"stop\":true}\n"),
        ("n/c.json", "{\"n\":3}\n"),
    ];
    let expression = "if .stop then halt else .n += 1 end";
    let counts = |scratch: &Scratch| {
        files.map(|(path, _)| {
            let text = std::fs::read_to_string(scratch.path(path)).expect("read");
            let value: serde_json::Value = serde_json::from_str(&text).expect("a whole file");
            value["n"].as_i64().expect("n")
        })
    };
    let set = |flags: &[&str]| {
        let scratch = Scratch::new(&files);
        let run = scratch.run(&[flags, &["set", "--jq", expression, "n/*.json"]].concat());
        (run.code, slashes(&run.stdout), run.stderr, counts(&scratch))
    };
    let ended = |counts| (1, String::new(), format!("{ERROR}\n"), counts);
    // One file: nothing is written.
    let scratch = Scratch::new(&files);
    let run = scratch.run(&["-i", "set", "--jq", expression, "n/b.json"]);
    assert_eq!((run.code, run.stderr), (1, format!("{ERROR}\n")));
    assert_eq!(counts(&scratch), [1, 2, 3]);
    // Several: as any file that fails, it ends the run.
    for flags in [&["-i"][..], &["-i", "--parallel", "3"]] {
        assert_eq!(set(flags), ended([2, 2, 3]));
    }
    // Or is counted, with the others written.
    let summary = format!("Modified: 2, Skipped: 0, Failed: 1\n  - n/b.json: {ERROR}\n");
    let failed = "1 file(s) failed in bulk run\n";
    for flags in [
        &["-i", "--continue-on-error"][..],
        &["-i", "--continue-on-error", "--parallel", "3"],
    ] {
        let counted = (7, summary.clone(), failed.to_owned(), [2, 2, 4]);
        assert_eq!(set(flags), counted);
    }
    assert_eq!(set(&["--check"]), ended([1, 2, 3]));
}
