//! A rule that raises, as `dq lint` and `dq check` report it: every output
//! format with every flag that could hide it, what else may go wrong in the
//! same run, and hostile text in what the error quotes.
//!
//! `cli_rule_errors.rs` has the basics; `cli_rule_error_fix.rs` is `dq fix`.

mod scratch;

use pretty_assertions::assert_eq;
use scratch::{Run, Scratch};

/// Indexes `.spec` as an object in `check.jq`.
const RAISES: &str = r#"id: t.raises
description: indexes spec as an object
severity: warn
match:
  format: yaml
check:
  jq: '.spec.template | select(. == null)'
  message: no template
"#;

/// Reports every Pod, as an error.
const FIRES: &str = r#"id: t.fires
description: fires on every pod
severity: error
match:
  format: yaml
check:
  jq: 'select(.kind == "Pod")'
  message: is a pod
"#;

/// Reports every Pod, as a warning.
const WARNS: &str = r#"id: t.warns
description: warns on every pod
severity: warn
match:
  format: yaml
check:
  jq: 'select(.kind == "Pod")'
  message: is a pod
"#;

/// A file [`RAISES`] raises on.
const POD: &str = "kind: Pod\nspec: x\n";

/// The rules above in files of their own, and [`POD`].
fn scratch() -> Scratch {
    Scratch::new(&[
        ("raises.yml", RAISES),
        ("fires.yml", FIRES),
        ("warns.yml", WARNS),
        ("pod.yaml", POD),
    ])
}

/// The line on stderr of [`RAISES`] on `file`, which has `spec: SPEC`.
fn raised(file: &str, spec: &str) -> String {
    format!(
        "ERROR rule did not run rule_id=t.raises file={file} field=check.jq \
         error=jq runtime error: cannot index \"{spec}\" with \"template\"\n"
    )
}

/// The last line on stderr of a run with `count` rule errors.
fn summary(count: usize) -> String {
    format!("{count} rule error(s): findings may be missing (--allow-rule-errors to accept)\n")
}

/// The warning `--allow-rule-errors` gives for [`RAISES`] on [`POD`].
const ALLOWED: &str = "WARN check.jq raised a runtime error; skipping rule for this file \
     rule_id=t.raises error=jq runtime error: cannot index \"x\" with \"template\"\n";

const LINT_FAILED: &str = "lint reported 1 error-severity diagnostic(s)\n";

/// Join two flag lists.
fn with<'a>(flags: &[&'a str], rest: &[&'a str]) -> Vec<&'a str> {
    [flags, rest].concat()
}

// ---------------------------------------------------------------------------
// Every output format
// ---------------------------------------------------------------------------

/// What one format prints for `pod.yaml`.
struct Printed {
    format: &'static str,
    /// `lint` with [`RAISES`] and [`FIRES`].
    both: String,
    /// `check` with [`RAISES`].
    error_only: String,
    /// `lint --allow-rule-errors` with [`RAISES`] and [`FIRES`].
    finding_only: String,
    /// `check --allow-rule-errors` with [`RAISES`].
    nothing: String,
}

const FINDING_JSON: &str = r#"    {
      "path": "pod.yaml",
      "line": 1,
      "col": 1,
      "message": "is a pod",
      "severity": "error",
      "rule_id": "t.fires",
      "references": []
    }"#;

const ERROR_JSON: &str = r#"    {
      "path": "pod.yaml",
      "line": 1,
      "col": 1,
      "message": "rule 't.raises' did not run: check.jq: jq runtime error: cannot index \"x\" with \"template\"",
      "severity": "error",
      "rule_id": "lint.rule_error",
      "references": []
    }"#;

const FINDING_JSONL: &str = r#"{"path":"pod.yaml","line":1,"col":1,"message":"is a pod","severity":"error","rule_id":"t.fires","references":[]}"#;

const ERROR_JSONL: &str = r#"{"path":"pod.yaml","line":1,"col":1,"message":"rule 't.raises' did not run: check.jq: jq runtime error: cannot index \"x\" with \"template\"","severity":"error","rule_id":"lint.rule_error","references":[]}"#;

const FINDING_YAML: &str = r#"- path: pod.yaml
  line: 1
  col: 1
  message: is a pod
  severity: error
  rule_id: t.fires
  references: []
"#;

const ERROR_YAML: &str = r#"- path: pod.yaml
  line: 1
  col: 1
  message: 'rule ''t.raises'' did not run: check.jq: jq runtime error: cannot index "x" with "template"'
  severity: error
  rule_id: lint.rule_error
  references: []
"#;

const FINDING_TOML: &str = r#"[[diagnostics]]
path = "pod.yaml"
line = 1
col = 1
message = "is a pod"
severity = "error"
rule_id = "t.fires"
references = []
"#;

const ERROR_TOML: &str = r#"[[diagnostics]]
path = "pod.yaml"
line = 1
col = 1
message = """rule 't.raises' did not run: check.jq: jq runtime error: cannot index "x" with "template""""
severity = "error"
rule_id = "lint.rule_error"
references = []
"#;

const FINDING_TOON: &str = r"  - path: pod.yaml
    line: 1
    col: 1
    message: is a pod
    severity: error
    rule_id: t.fires
    references: []
";

const ERROR_TOON: &str = r#"  - path: pod.yaml
    line: 1
    col: 1
    message: "rule 't.raises' did not run: check.jq: jq runtime error: cannot index \"x\" with \"template\""
    severity: error
    rule_id: lint.rule_error
    references: []
"#;

const FINDING_JUNIT: &str = r#"    <testcase classname="pod.yaml" name="t.fires">
      <failure type="error" message="is a pod">pod.yaml:1:1
is a pod</failure>
    </testcase>
"#;

const ERROR_JUNIT: &str = r#"    <testcase classname="pod.yaml" name="lint.rule_error">
      <failure type="error" message="rule &apos;t.raises&apos; did not run: check.jq: jq runtime error: cannot index &quot;x&quot; with &quot;template&quot;">pod.yaml:1:1
rule 't.raises' did not run: check.jq: jq runtime error: cannot index "x" with "template"</failure>
    </testcase>
"#;

const FINDING_TAP: &str = r#"not ok 1 - t.fires: is a pod
  ---
  severity: error
  file: pod.yaml
  line: 1
  col: 1
  message: "is a pod"
  ...
"#;

/// The TAP entry of the rule error, the `n`th.
fn error_tap(n: usize) -> String {
    format!(
        r#"not ok {n} - lint.rule_error: rule 't.raises' did not run: check.jq: jq runtime error: cannot index "x" with "template"
  ---
  severity: error
  file: pod.yaml
  line: 1
  col: 1
  message: "rule 't.raises' did not run: check.jq: jq runtime error: cannot index \"x\" with \"template\""
  ...
"#
    )
}

const FINDING_CONSOLE: &str = "diagnostics: [{path: pod.yaml, line: 1, col: 1, message: is a pod, \
     severity: error, rule_id: t.fires, references: []}]\n";

/// The SARIF log of a run whose results are `ids` (`t.fires`,
/// `lint.rule_error`), each with a descriptor of its own.
fn sarif(ids: &[&str]) -> String {
    let location = serde_json::json!([{
        "physicalLocation": {
            "artifactLocation": { "uri": "pod.yaml" },
            "region": { "startLine": 1, "startColumn": 1 },
        },
    }]);
    let text = |id: &str| match id {
        "t.fires" => ("fires on every pod", "is a pod"),
        "lint.rule_error" => (
            "A rule raised a jq error and did not run on the file: its findings may be missing.",
            "rule 't.raises' did not run: check.jq: jq runtime error: cannot index \"x\" with \"template\"",
        ),
        other => panic!("no such rule: {other}"),
    };
    let rules: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({ "id": id, "shortDescription": { "text": text(id).0 } }))
        .collect();
    let results: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            serde_json::json!({
                "ruleId": id,
                "ruleIndex": index,
                "level": "error",
                "message": { "text": text(id).1 },
                "locations": location,
            })
        })
        .collect();
    let log = serde_json::json!({
        "$schema": "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.5.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "dq",
                "version": env!("CARGO_PKG_VERSION"),
                "informationUri": "https://github.com/mazuninky/dq",
                "rules": rules,
            } },
            "results": results,
        }],
    });
    format!("{log:#}\n")
}

fn junit(cases: &[&str]) -> String {
    let count = cases.len();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n  <testsuite name=\"dq-lint\" \
         tests=\"{count}\" failures=\"{count}\" errors=\"0\" skipped=\"0\">\n{}  </testsuite>\n\
         </testsuites>\n",
        cases.concat()
    )
}

/// A JSON report that lists `items`.
fn json(items: &[&str]) -> String {
    format!("{{\n  \"diagnostics\": [\n{}\n  ]\n}}\n", items.join(",\n"))
}

fn printed() -> Vec<Printed> {
    vec![
        // The console lists findings only, and nothing when there are none:
        // `diagnostics: []` would read as "all clear".
        Printed {
            format: "console",
            both: FINDING_CONSOLE.to_owned(),
            error_only: String::new(),
            finding_only: FINDING_CONSOLE.to_owned(),
            nothing: "diagnostics: []\n".to_owned(),
        },
        Printed {
            format: "json",
            both: json(&[FINDING_JSON, ERROR_JSON]),
            error_only: json(&[ERROR_JSON]),
            finding_only: json(&[FINDING_JSON]),
            nothing: "{\n  \"diagnostics\": []\n}\n".to_owned(),
        },
        Printed {
            format: "jsonl",
            both: format!("{{\"diagnostics\":[{FINDING_JSONL},{ERROR_JSONL}]}}\n"),
            error_only: format!("{{\"diagnostics\":[{ERROR_JSONL}]}}\n"),
            finding_only: format!("{{\"diagnostics\":[{FINDING_JSONL}]}}\n"),
            nothing: "{\"diagnostics\":[]}\n".to_owned(),
        },
        Printed {
            format: "yaml",
            both: format!("diagnostics:\n{FINDING_YAML}{ERROR_YAML}"),
            error_only: format!("diagnostics:\n{ERROR_YAML}"),
            finding_only: format!("diagnostics:\n{FINDING_YAML}"),
            nothing: "diagnostics: []\n".to_owned(),
        },
        Printed {
            format: "toml",
            both: format!("{FINDING_TOML}\n{ERROR_TOML}"),
            error_only: ERROR_TOML.to_owned(),
            finding_only: FINDING_TOML.to_owned(),
            nothing: "diagnostics = []\n".to_owned(),
        },
        Printed {
            format: "toon",
            both: format!("diagnostics[2]:\n{FINDING_TOON}{ERROR_TOON}"),
            error_only: format!("diagnostics[1]:\n{ERROR_TOON}"),
            finding_only: format!("diagnostics[1]:\n{FINDING_TOON}"),
            nothing: "diagnostics: []\n".to_owned(),
        },
        Printed {
            format: "sarif",
            both: sarif(&["t.fires", "lint.rule_error"]),
            error_only: sarif(&["lint.rule_error"]),
            finding_only: sarif(&["t.fires"]),
            nothing: sarif(&[]),
        },
        Printed {
            format: "junit",
            both: junit(&[FINDING_JUNIT, ERROR_JUNIT]),
            error_only: junit(&[ERROR_JUNIT]),
            finding_only: junit(&[FINDING_JUNIT]),
            nothing: junit(&[]),
        },
        Printed {
            format: "tap",
            both: format!(
                "TAP version 13\n1..2\n{FINDING_TAP}{}# 2 tests, 2 failures\n",
                error_tap(2)
            ),
            error_only: format!(
                "TAP version 13\n1..1\n{}# 1 test, 1 failure\n",
                error_tap(1)
            ),
            finding_only: format!("TAP version 13\n1..1\n{FINDING_TAP}# 1 test, 1 failure\n"),
            nothing: "TAP version 13\n1..0\n# 0 tests, 0 failures\n".to_owned(),
        },
    ]
}

/// No flag hides a rule error or changes the exit code, in any format.
const FLAGS: [&[&str]; 4] = [&[], &["-q"], &["--strict"], &["-q", "--strict"]];

const LINT_BOTH: [&str; 5] = ["lint", "--rules", "raises.yml", "--rules", "fires.yml"];

#[test]
fn lint_reports_the_finding_and_the_rule_error_in_every_format() {
    let scratch = scratch();
    for printed in printed() {
        for flags in FLAGS {
            let format = printed.format;
            let mut args = with(flags, &["-F", format]);
            args.extend(LINT_BOTH);
            args.push("pod.yaml");
            assert_eq!(
                scratch.run(&args),
                Run {
                    code: 3,
                    stdout: printed.both.clone(),
                    stderr: raised("pod.yaml", "x") + &summary(1),
                },
                "{args:?}"
            );
        }
    }
}

#[test]
fn check_reports_the_rule_error_alone_in_every_format() {
    let scratch = scratch();
    for printed in printed() {
        for flags in FLAGS {
            let args = with(
                flags,
                &[
                    "-F",
                    printed.format,
                    "check",
                    "--rule",
                    "raises.yml",
                    "pod.yaml",
                ],
            );
            assert_eq!(
                scratch.run(&args),
                Run {
                    code: 3,
                    stdout: printed.error_only.clone(),
                    stderr: raised("pod.yaml", "x") + &summary(1),
                },
                "{args:?}"
            );
        }
    }
}

#[test]
fn allow_rule_errors_leaves_the_report_to_the_findings_in_every_format() {
    let scratch = scratch();
    for printed in printed() {
        for flags in FLAGS {
            let format = printed.format;
            let warning = if flags.contains(&"-q") { "" } else { ALLOWED };
            let mut args = with(flags, &["-F", format]);
            args.extend(LINT_BOTH);
            args.extend(["--allow-rule-errors", "pod.yaml"]);
            assert_eq!(
                scratch.run(&args),
                Run {
                    code: 4,
                    stdout: printed.finding_only.clone(),
                    stderr: format!("{warning}{LINT_FAILED}"),
                },
                "{args:?}"
            );
            let args = with(
                flags,
                &[
                    "-F",
                    format,
                    "check",
                    "--allow-rule-errors",
                    "--rule",
                    "raises.yml",
                    "pod.yaml",
                ],
            );
            assert_eq!(
                scratch.run(&args),
                Run {
                    code: 0,
                    stdout: printed.nothing.clone(),
                    stderr: warning.to_owned(),
                },
                "{args:?}"
            );
        }
    }
}

#[test]
fn a_rule_error_outranks_findings_and_strict_whatever_the_severities() {
    let scratch = scratch();
    let lint = |flags: &[&str], rules: &[&str]| {
        let mut args = with(flags, &["lint"]);
        for rule in rules {
            args.extend(["--rules", rule]);
        }
        args.push("pod.yaml");
        scratch.run(&args).code
    };
    // Without the rule that raises: 4 for an error, 1 for a warning under
    // `--strict`, 0 for one without it.
    for (rule, flags, code) in [
        ("fires.yml", &[][..], 4),
        ("fires.yml", &["--strict"], 4),
        ("warns.yml", &[], 0),
        ("warns.yml", &["--strict"], 1),
    ] {
        assert_eq!(lint(flags, &[rule]), code, "{rule} {flags:?}");
        // Before or after it, and a warning itself (RAISES is one).
        assert_eq!(lint(flags, &[rule, "raises.yml"]), 3, "{rule} {flags:?}");
        assert_eq!(lint(flags, &["raises.yml", rule]), 3, "{rule} {flags:?}");
        let allowed = with(flags, &["lint", "--allow-rule-errors"]);
        let allowed = with(
            &allowed,
            &["--rules", "raises.yml", "--rules", rule, "pod.yaml"],
        );
        assert_eq!(scratch.run(&allowed).code, code, "{allowed:?}");
    }
}

// ---------------------------------------------------------------------------
// Beside something else that goes wrong
// ---------------------------------------------------------------------------

/// `(rule_id, path, line, message)` of every diagnostic of `-F json` output.
fn diagnostics(run: &Run) -> Vec<(String, String, u64, String)> {
    let report: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON stdout");
    report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            let text = |key: &str| d[key].as_str().expect(key).to_owned();
            let line = d["line"].as_u64().expect("line");
            (text("rule_id"), text("path"), line, text("message"))
        })
        .collect()
}

/// `(rule_id, path, line)` of every diagnostic of `-F json` output.
fn places(run: &Run) -> Vec<(String, String, u64)> {
    diagnostics(run)
        .into_iter()
        .map(|(id, path, line, _)| (id, path, line))
        .collect()
}

fn place(id: &str, path: &str, line: u64) -> (String, String, u64) {
    (id.to_owned(), path.to_owned(), line)
}

#[test]
fn a_file_that_does_not_parse_gives_the_run_its_error_and_both_are_reported() {
    let scratch = scratch();
    scratch.write("broken.yaml", "a: [\n");
    scratch.write("broken2.yaml", "b: {\n");
    let parse_error = concat!(
        "parse error in broken.yaml: did not find expected node content, while parsing a flow node\n",
        "  --> broken.yaml:2:1\n",
        "   |\n",
        " 2 |\n",
        "   | ^\n",
    );
    for files in [["broken.yaml", "pod.yaml"], ["pod.yaml", "broken.yaml"]] {
        let console = scratch.run(&with(&LINT_BOTH, &files));
        assert_eq!(
            console,
            Run {
                code: 3,
                stdout: FINDING_CONSOLE.to_owned(),
                stderr: raised("pod.yaml", "x") + parse_error,
            },
            "{files:?}"
        );
        let structured = scratch.run(&with(&["-F", "json"], &with(&LINT_BOTH, &files)));
        assert_eq!((structured.code, &structured.stderr), (3, &console.stderr));
        let mut reported = places(&structured);
        reported.sort();
        assert_eq!(
            reported,
            [
                place("lint.parse", "broken.yaml", 2),
                place("lint.rule_error", "pod.yaml", 1),
                place("t.fires", "pod.yaml", 1),
            ]
        );
    }
    // The console has nothing but errors, so it prints them alone.
    let run = scratch.run(&[
        "check",
        "--rule",
        "raises.yml",
        "broken.yaml",
        "pod.yaml",
        "broken2.yaml",
    ]);
    assert_eq!((run.code, run.stdout.as_str()), (3, ""));
    assert!(
        run.stderr
            .starts_with(&(raised("pod.yaml", "x") + "WARN parse error in broken2.yaml: "))
            && run.stderr.ends_with(parse_error),
        "{}",
        run.stderr
    );
    // Accepting rule errors does not accept the file.
    let run = scratch.run(&[
        "check",
        "--allow-rule-errors",
        "--rule",
        "raises.yml",
        "broken.yaml",
        "pod.yaml",
    ]);
    assert_eq!(
        (run.code, run.stderr.as_str()),
        (3, [ALLOWED, parse_error].concat().as_str())
    );
}

/// Four documents: [`RAISES`] raises on `x` twice and on `z`, and reports
/// the last one.
const STREAM: &str = "kind: Pod\nspec: x\n---\nkind: Pod\nspec: x\n---\nkind: Pod\nspec: z\n---\nkind: Pod\nspec: {}\n";

/// [`RAISES`] under another id, raising in `match.filter`.
fn raises_in_filter() -> String {
    RAISES
        .replace("id: t.raises", "id: t.filter")
        .replace("format: yaml", "format: yaml\n  filter: '.spec.a == null'")
}

#[test]
fn one_result_per_rule_file_field_and_error_over_files_and_documents() {
    let scratch = scratch();
    scratch.write("filter.yml", &raises_in_filter());
    scratch.write("stream.yaml", STREAM);
    scratch.write("pod2.yaml", "kind: Pod\nspec: x\n");
    let run = scratch.run(&[
        "-F",
        "json",
        "lint",
        "--rules",
        "raises.yml",
        "--rules",
        "fires.yml",
        "--rules",
        "filter.yml",
        "stream.yaml",
        "pod.yaml",
        "pod2.yaml",
    ]);
    let message = |id: &str, field: &str, spec: &str, key: &str| {
        format!(
            "rule '{id}' did not run: {field}: jq runtime error: cannot index \"{spec}\" with \"{key}\""
        )
    };
    let check = |spec| message("t.raises", "check.jq", spec, "template");
    let filter = |spec| message("t.filter", "match.filter", spec, "a");
    let row = |id: &str, path: &str, line: u64, message: String| {
        (id.to_owned(), path.to_owned(), line, message)
    };
    let fires = |path: &str, line| row("t.fires", path, line, "is a pod".to_owned());
    let error = |path: &str, message| row("lint.rule_error", path, 1, message);
    let no_template = |id| row(id, "stream.yaml", 10, "no template".to_owned());
    assert_eq!(
        diagnostics(&run),
        [
            // Document 0, and 1, which raises the same again.
            fires("stream.yaml", 1),
            error("stream.yaml", check("x")),
            error("stream.yaml", filter("x")),
            fires("stream.yaml", 4),
            // Document 2 raises another text; 3 is fine for every rule.
            fires("stream.yaml", 7),
            error("stream.yaml", check("z")),
            error("stream.yaml", filter("z")),
            no_template("t.raises"),
            fires("stream.yaml", 10),
            no_template("t.filter"),
            // The same text in other files is theirs.
            fires("pod.yaml", 1),
            error("pod.yaml", check("x")),
            error("pod.yaml", filter("x")),
            fires("pod2.yaml", 1),
            error("pod2.yaml", check("x")),
            error("pod2.yaml", filter("x")),
        ]
    );
    let line = |id: &str, file: &str, field: &str, spec: &str, key: &str| {
        format!(
            "ERROR rule did not run rule_id={id} file={file} field={field} error=jq runtime error: \
             cannot index \"{spec}\" with \"{key}\"\n"
        )
    };
    let expected: String = [
        ("stream.yaml", "x"),
        ("stream.yaml", "z"),
        ("pod.yaml", "x"),
        ("pod2.yaml", "x"),
    ]
    .into_iter()
    .flat_map(|(file, spec)| {
        [
            line("t.raises", file, "check.jq", spec, "template"),
            line("t.filter", file, "match.filter", spec, "a"),
        ]
    })
    .collect();
    assert_eq!((run.code, run.stderr), (3, expected + &summary(8)));
}

#[test]
fn a_file_named_twice_has_its_rule_error_once() {
    let scratch = scratch();
    let run = scratch.run(&with(
        &["-F", "json"],
        &with(&LINT_BOTH, &["pod.yaml", "pod.yaml"]),
    ));
    assert_eq!(
        places(&run),
        [
            place("t.fires", "pod.yaml", 1),
            place("lint.rule_error", "pod.yaml", 1),
            place("t.fires", "pod.yaml", 1),
        ]
    );
    assert_eq!(
        (run.code, run.stderr),
        (3, raised("pod.yaml", "x") + &summary(1))
    );
    // Under another spelling it is another file of the run.
    let run = scratch.run(&["check", "--rule", "raises.yml", "pod.yaml", "./pod.yaml"]);
    let stderr = raised("pod.yaml", "x") + &raised("./pod.yaml", "x") + &summary(2);
    assert_eq!((run.code, run.stderr), (3, stderr));
}

#[test]
fn a_rule_error_on_stdin_names_the_dash() {
    let scratch = scratch();
    for command in [["lint", "--rules"], ["check", "--rule"]] {
        let args = [command[0], command[1], "raises.yml"];
        let args = with(&args, &["--input-format", "yaml", "-"]);
        let run = scratch.run_with_stdin(&args, POD);
        assert_eq!(
            run,
            Run {
                code: 3,
                stdout: String::new(),
                stderr: raised("-", "x") + &summary(1),
            }
        );
        let run = scratch.run_with_stdin(&with(&["-F", "json"], &args), POD);
        assert_eq!(places(&run), [place("lint.rule_error", "-", 1)]);
    }
}

#[test]
fn doc_reports_the_rule_errors_of_its_document_only() {
    let scratch = scratch();
    scratch.write("stream.yaml", STREAM);
    let run = |doc: &str, files: &[&str]| {
        let args = with(&["-F", "json", "--doc", doc], &LINT_BOTH);
        scratch.run(&with(&args, files))
    };
    // Document 3 raises nothing, whatever the others do.
    let out = run("3", &["stream.yaml"]);
    assert_eq!(
        places(&out),
        [
            place("t.raises", "stream.yaml", 10),
            place("t.fires", "stream.yaml", 10),
        ]
    );
    assert_eq!((out.code, out.stderr.as_str()), (4, LINT_FAILED));
    let out = run("2", &["stream.yaml"]);
    assert_eq!(
        places(&out),
        [
            place("t.fires", "stream.yaml", 7),
            place("lint.rule_error", "stream.yaml", 1),
        ]
    );
    let stderr = raised("stream.yaml", "z") + &summary(1);
    assert_eq!((out.code, &out.stderr), (3, &stderr));
    // Beside a file without the document, which no rule ran on.
    let out = run("2", &["pod.yaml", "stream.yaml"]);
    let skipped =
        "WARN skipped pod.yaml: --doc 2 is out of range: pod.yaml holds 1 document(s) (--doc 0)\n";
    assert_eq!((out.code, out.stderr), (3, skipped.to_owned() + &stderr));
}

/// A composite whose nested rule raises in `match.filter` for `spec: x`,
/// passes `spec: {}` on to its check, and is not run on what does not parse.
const COMPOSITE: &str = r#"id: t.outer
description: parts of a pod
severity: error
match:
  format: yaml
check:
  extract: '["spec: x", "spec: [", "spec: {}", "spec: x"] | map({value: ., format: "yaml", anchor: ""})'
  message: outer
  nested:
    id: t.inner
    description: inner
    severity: error
    match:
      format: yaml
      filter: '.spec.a == null'
    check:
      jq: '.spec.template | select(. == null)'
      message: no template
"#;

/// A composite whose `extract` raises.
const BAD_EXTRACT: &str = r#"id: t.extract
description: extract raises
severity: error
match:
  format: yaml
check:
  extract: '.kind.x'
  message: outer
  nested:
    id: t.never
    description: never runs
    severity: error
    match:
      format: yaml
    check:
      jq: '.'
      message: m
"#;

#[test]
fn a_nested_rule_that_raises_stands_beside_the_errors_of_its_composite() {
    let scratch = scratch();
    scratch.write("composite.yml", COMPOSITE);
    scratch.write("extract.yml", BAD_EXTRACT);
    let args = [
        "lint",
        "--rules",
        "composite.yml",
        "--rules",
        "extract.yml",
        "--rules",
        "raises.yml",
    ];
    let run = scratch.run(&with(&["-F", "json"], &with(&args, &["pod.yaml"])));
    let ids: Vec<String> = diagnostics(&run).into_iter().map(|d| d.0).collect();
    assert_eq!(
        ids,
        [
            "t.outer.parse-failed",
            "t.inner",
            "t.extract.composite-error",
            "lint.rule_error",
            "lint.rule_error",
        ]
    );
    let nested =
        "rule 't.inner' did not run: match.filter: jq runtime error: cannot index \"x\" with \"a\"";
    assert_eq!(diagnostics(&run)[3].3, nested);
    let stderr = [
        "WARN composite extract jq raised a runtime error; emitting composite-error diagnostic \
         rule_id=t.extract error=jq runtime error: cannot index \"Pod\" with \"x\"\n",
        "ERROR rule did not run rule_id=t.inner file=pod.yaml field=match.filter \
         error=jq runtime error: cannot index \"x\" with \"a\"\n",
        &raised("pod.yaml", "x"),
        &summary(2),
    ]
    .concat();
    assert_eq!((run.code, run.stderr), (3, stderr));
    // An `extract` that raises is a finding of its own, and fails the run
    // as one.
    let run = scratch.run(&["-F", "json", "check", "--rule", "extract.yml", "pod.yaml"]);
    assert_eq!(
        places(&run),
        [place("t.extract.composite-error", "pod.yaml", 1)]
    );
    assert_eq!(run.code, 4);
    // Allowed, it is what it was before rule errors failed a run: each
    // error the moment it is raised, and the findings decide.
    let args = with(&["-F", "json"], &with(&args, &["--allow-rule-errors"]));
    let run = scratch.run(&with(&args, &["pod.yaml"]));
    assert_eq!(diagnostics(&run).len(), 3);
    let nested = "WARN match.filter raised a runtime error; skipping rule for this file \
         rule_id=t.inner error=jq runtime error: cannot index \"x\" with \"a\"\n";
    let stderr = [
        nested,
        nested,
        "WARN composite extract jq raised a runtime error; emitting composite-error diagnostic \
         rule_id=t.extract error=jq runtime error: cannot index \"Pod\" with \"x\"\n",
        ALLOWED,
        "lint reported 3 error-severity diagnostic(s)\n",
    ]
    .concat();
    assert_eq!((run.code, run.stderr), (4, stderr));
}

#[test]
fn a_loc_expression_that_raises_is_a_warning_beside_a_rule_error() {
    let scratch = scratch();
    let rule = FIRES.replace("id: t.fires", "id: t.loc") + "loc:\n  line: '.kind.x'\n";
    scratch.write("loc.yml", &rule);
    let warning = "WARN loc.line raised a runtime error; defaulting to line 1 rule_id=t.loc \
         error=jq runtime error: cannot index \"Pod\" with \"x\"\n";
    // Alone: the finding is there, so nothing is missing.
    let run = scratch.run(&["-F", "json", "check", "--rule", "loc.yml", "pod.yaml"]);
    assert_eq!(places(&run), [place("t.loc", "pod.yaml", 1)]);
    let stderr = [warning, LINT_FAILED].concat();
    assert_eq!((run.code, run.stderr), (4, stderr));
    let args = [
        "-F",
        "json",
        "lint",
        "--rules",
        "raises.yml",
        "--rules",
        "loc.yml",
        "pod.yaml",
    ];
    let run = scratch.run(&args);
    assert_eq!(
        places(&run),
        [
            place("t.loc", "pod.yaml", 1),
            place("lint.rule_error", "pod.yaml", 1),
        ]
    );
    let stderr = [warning, &raised("pod.yaml", "x"), &summary(1)].concat();
    assert_eq!((run.code, run.stderr), (3, stderr));
}

#[test]
fn a_plugin_directory_without_a_plugin_changes_nothing() {
    let scratch = scratch();
    std::fs::create_dir(scratch.path("plugins")).expect("mkdir");
    let run = scratch.run(&with(
        &["--plugins", "plugins"],
        &with(&LINT_BOTH, &["pod.yaml"]),
    ));
    assert_eq!(run, scratch.run(&with(&LINT_BOTH, &["pod.yaml"])));
    assert_eq!(run.code, 3);
}

#[test]
fn a_check_that_raises_after_an_output_did_not_run() {
    let scratch = scratch();
    // `"a"` is an output before the error: half a check is no check.
    scratch.write("late.yml", &rule_with_check("t.late", r#""a", error("b")"#));
    // Raised, but caught: no error.
    scratch.write(
        "caught.yml",
        &rule_with_check("t.caught", r#"try error("b") catch empty"#),
    );
    let check = |rule| scratch.run(&["-F", "json", "check", "--rule", rule, "pod.yaml"]);
    let late = check("late.yml");
    assert_eq!(
        diagnostics(&late)
            .into_iter()
            .map(|(id, _, _, message)| (id, message))
            .collect::<Vec<_>>(),
        [(
            "lint.rule_error".to_owned(),
            "rule 't.late' did not run: check.jq: jq runtime error: \"b\"".to_owned()
        )]
    );
    assert_eq!(late.code, 3);
    let caught = check("caught.yml");
    assert_eq!((caught.code, caught.stderr.as_str()), (0, ""));
    assert_eq!(diagnostics(&caught), []);
}

/// A YAML rule with `check.jq: JQ`.
fn rule_with_check(id: &str, jq: &str) -> String {
    format!(
        "id: {id}\ndescription: d\nseverity: error\nmatch:\n  format: yaml\ncheck:\n  jq: '{jq}'\n  \
         message: m\n"
    )
}

// ---------------------------------------------------------------------------
// What an error quotes
// ---------------------------------------------------------------------------

/// Nothing on stderr acts on the terminal: no control character but the line
/// breaks, and `lines` of those.
fn assert_safe(stderr: &str, lines: usize) {
    let raw: Vec<char> = stderr
        .chars()
        .filter(|c| c.is_control() && *c != '\n')
        .chain(
            stderr
                .chars()
                .filter(|c| ('\u{2028}'..='\u{202e}').contains(c)),
        )
        .collect();
    assert_eq!(raw, Vec::<char>::new(), "{stderr:?}");
    assert_eq!(stderr.lines().count(), lines, "{stderr:?}");
}

/// A rule whose id and whose error hold ESC, U+009B, U+202E, U+2028 and line
/// breaks (`\u2028` etc. are YAML escapes in the id, jq escapes in the error).
const HOSTILE: &str = r#"id: "t.h\u001b[31m\u202e\nERROR forged\u2028"
description: hostile
severity: error
match:
  format: FORMAT
check:
  jq: 'error("\u001b[31mred\n0 rule error(s)\u202e\u009b0m\r\u2028x")'
  message: m
"#;

/// A file name with the same in it, where the file system takes it.
#[cfg(unix)]
const HOSTILE_FILE: &str = "p\u{1b}[32m\u{202e}\nod.yaml";
#[cfg(not(unix))]
const HOSTILE_FILE: &str = "p\u{202e}od.yaml";

#[cfg(unix)]
const HOSTILE_FILE_SHOWN: &str = r"p\x1b[32m\u202e\x0aod.yaml";
#[cfg(not(unix))]
const HOSTILE_FILE_SHOWN: &str = r"p\u202eod.yaml";

#[test]
fn rule_error_lines_show_a_hostile_id_error_and_file_name_escaped() {
    let rule = HOSTILE.replace("FORMAT", "yaml");
    let scratch = Scratch::new(&[("rule.yml", &rule), (HOSTILE_FILE, POD)]);
    let shown_id = r"t.h\x1b[31m\u202e\x0aERROR forged\u2028";
    let shown_error = r#"jq runtime error: "\u001b[31mred\n0 rule error(s)\u202e\x9b0m\r\u2028x""#;
    let stderr = format!(
        "ERROR rule did not run rule_id={shown_id} file={HOSTILE_FILE_SHOWN} field=check.jq \
         error={shown_error}\n{}",
        summary(1)
    );
    for args in [
        &["lint", "--rules", "rule.yml", HOSTILE_FILE][..],
        &["check", "--rule", "rule.yml", HOSTILE_FILE],
        &["-q", "lint", "--rules", "rule.yml", HOSTILE_FILE],
        &["-F", "json", "lint", "--rules", "rule.yml", HOSTILE_FILE],
    ] {
        let run = scratch.run(args);
        assert_safe(&run.stderr, 2);
        assert_eq!((run.code, &run.stderr), (3, &stderr), "{args:?}");
    }
    let run = scratch.run(&[
        "lint",
        "--allow-rule-errors",
        "--rules",
        "rule.yml",
        HOSTILE_FILE,
    ]);
    assert_safe(&run.stderr, 1);
    assert_eq!(run.code, 0);
    // A structured report holds the text itself.
    let run = scratch.run(&["-F", "json", "lint", "--rules", "rule.yml", HOSTILE_FILE]);
    assert_eq!(
        diagnostics(&run),
        [(
            "lint.rule_error".to_owned(),
            HOSTILE_FILE.to_owned(),
            1,
            // jq writes the error as JSON text, so its ESC, line break and
            // carriage return are escapes in the message already.
            "rule 't.h\u{1b}[31m\u{202e}\nERROR forged\u{2028}' did not run: check.jq: \
             jq runtime error: \"\\u001b[31mred\\n0 rule error(s)\u{202e}\u{9b}0m\\r\u{2028}x\""
                .to_owned()
        )]
    );
}

#[test]
fn rule_source_errors_show_a_hostile_id_format_and_directory_escaped() {
    let shown_id = r"t.h\x1b[31m\u202e\x0aERROR forged\u2028";
    let unknown = HOSTILE.replace("FORMAT", r#""yaml\u001b[32m\n\u202e\u009b""#);
    let empty = HOSTILE.replace("FORMAT", "[]");
    let close = HOSTILE.replace("FORMAT", r#""yaml\u001b""#);
    let scratch = Scratch::new(&[
        ("unknown.yml", &unknown),
        ("close.yml", &close),
        ("empty.yml", &empty),
        ("pod.yaml", POD),
    ]);
    let lint = |rules: &str| scratch.run(&["lint", "--rules", rules, "pod.yaml"]);
    for (rules, stderr) in [
        (
            "unknown.yml",
            format!(
                "rule {shown_id} has unknown format 'yaml\\x1b[32m\\x0a\\u202e\\x9b' in \
                 match.format (formats: json, yaml, toml, jsonl, hcl, ini, dotenv, csv, tsv, \
                 dockerfile, ignore-list, markdown, frontmatter, xml)\n"
            ),
        ),
        (
            "close.yml",
            format!(
                "rule {shown_id} has unknown format 'yaml\\x1b' in match.format \
                 (did you mean: yaml)\n"
            ),
        ),
        (
            "empty.yml",
            format!("rule {shown_id}: match.format lists no format\n"),
        ),
    ] {
        let run = lint(rules);
        assert_safe(&run.stderr, 1);
        assert_eq!((run.code, run.stderr), (3, stderr));
    }
}

#[cfg(unix)]
#[test]
fn a_rule_directory_with_a_hostile_name_and_no_rule_is_named_escaped() {
    let scratch = scratch();
    let name = "d\u{1b}[31m\n\u{202e}";
    std::fs::create_dir(scratch.path(name)).expect("mkdir");
    for args in [
        ["lint", "--rules", name, "pod.yaml"],
        ["check", "--rule", name, "pod.yaml"],
        ["fix", "--rules", name, "pod.yaml"],
    ] {
        let run = scratch.run(&args);
        assert_safe(&run.stderr, 1);
        assert_eq!(
            run,
            Run {
                code: 6,
                stdout: String::new(),
                stderr: "no rules in 'd\\x1b[31m\\x0a\\u202e'\n".to_owned(),
            }
        );
    }
}
