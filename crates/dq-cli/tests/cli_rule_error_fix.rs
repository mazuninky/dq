//! A rule that raises under `dq fix`: the fixes of the other rules are made
//! in every write mode, the run ends with exit 3 unless a file failed, and a
//! parallel run prints what a serial one does, and ends.
//!
//! `cli_rule_errors.rs` has the basics; `cli_rule_error_reports.rs` is
//! `dq lint` and `dq check`.

mod scratch;

use pretty_assertions::assert_eq;
use scratch::{Run, Scratch};

/// Raises in `check.jq` unless `.spec` is an object; its fix adds `x`.
const RAISES_IN_CHECK: &str = r#"id: t.check
description: adds x
severity: error
match:
  format: yaml
check:
  jq: 'select(.x == null) | .spec.template | select(. == null)'
  message: no template
fix:
  ops: 'if .x == null then [{"op":"add","path":"/x","value":1}] else [] end'
"#;

/// Raises in `match.filter` unless `.spec` is an object; its fix adds `z`.
const RAISES_IN_FILTER: &str = r#"id: t.filter
description: adds z
severity: error
match:
  format: yaml
  filter: '.spec.a == null'
check:
  jq: 'select(.z == null)'
  message: no z
fix:
  ops: 'if .z == null then [{"op":"add","path":"/z","value":2}] else [] end'
"#;

/// Never raises; its fix adds `fixed: true`.
const FIXES: &str = r#"id: t.fixes
description: adds fixed
severity: warn
match:
  format: yaml
check:
  jq: 'select(.fixed != true)'
  message: not fixed
fix:
  ops: 'if .fixed == true then [] else [{"op":"add","path":"/fixed","value":true}] end'
"#;

/// The fix itself raises, on a document with `boom`.
const FIX_FAILS: &str = r#"id: t.boom
description: raises in fix.ops
severity: warn
match:
  format: yaml
check:
  jq: 'select(.boom)'
  message: boom
fix:
  ops: 'error("boom")'
"#;

/// Adds an element each time it is applied, which `fix` refuses with a
/// warning: a rule that logs from the worker.
const NEVER_DONE: &str = r#"id: t.never-done
description: not idempotent
severity: warn
match:
  format: yaml
check:
  jq: '.'
  message: m
fix:
  ops: '[{"op":"add","path":"/n","value":((.n // 0) + 1)}]'
"#;

/// The rules above, each in a file of its own.
fn scratch(files: &[(&str, &str)]) -> Scratch {
    let scratch = Scratch::new(&[
        ("rules/check.yml", RAISES_IN_CHECK),
        ("rules/filter.yml", RAISES_IN_FILTER),
        ("rules/fixes.yml", FIXES),
        ("boom.yml", FIX_FAILS),
        ("never-done.yml", NEVER_DONE),
    ]);
    for (path, text) in files {
        scratch.write(path, text);
    }
    scratch
}

/// `dq FLAGS fix --rules rules ARGS`.
fn fix(scratch: &Scratch, flags: &[&str], args: &[&str]) -> Run {
    scratch.run(&[flags, &["fix", "--rules", "rules"], args].concat())
}

fn read(scratch: &Scratch, path: &str) -> String {
    std::fs::read_to_string(scratch.path(path)).expect("read")
}

/// The two rules that raise on `file`, where `.spec` is the string `spec`,
/// as stderr names them.
fn raised(file: &str, spec: &str) -> String {
    format!(
        "ERROR rule did not run rule_id=t.check file={file} field=check.jq \
         error=jq runtime error: cannot index \"{spec}\" with \"template\"\n\
         ERROR rule did not run rule_id=t.filter file={file} field=match.filter \
         error=jq runtime error: cannot index \"{spec}\" with \"a\"\n"
    )
}

/// The same as `--allow-rule-errors` warns of them, in the words dq had
/// before a rule error failed the run.
fn allowed(spec: &str) -> String {
    format!(
        "WARN check.jq raised a runtime error before fix; skipping rule rule_id=t.check \
         error=jq runtime error: cannot index \"{spec}\" with \"template\"\n\
         WARN match.filter raised a runtime error; skipping rule for fix rule_id=t.filter \
         error=jq runtime error: cannot index \"{spec}\" with \"a\"\n"
    )
}

fn summary(count: usize) -> String {
    format!("{count} rule error(s): findings may be missing (--allow-rule-errors to accept)\n")
}

const BROKEN: &str = "a: [\n";

const PARSE_ERROR: &str = concat!(
    "parse error in broken.yaml: did not find expected node content, while parsing a flow node\n",
    "  --> broken.yaml:2:1\n",
    "   |\n",
    " 2 |\n",
    "   | ^\n",
);

/// Two files the rules raise on (`t.fixes` still fixes them) around one they
/// fix.
const FILES: [(&str, &str); 3] = [
    ("a.yaml", "spec: x # kept\n"),
    ("b.yaml", "spec: {}\n"),
    ("c.yaml", "spec: y\n"),
];
const NAMES: [&str; 3] = ["a.yaml", "b.yaml", "c.yaml"];
const FIXED: [(&str, &str); 3] = [
    ("a.yaml", "spec: x # kept\nfixed: true\n"),
    ("b.yaml", "spec: {}\nx: 1\nz: 2\nfixed: true\n"),
    ("c.yaml", "spec: y\nfixed: true\n"),
];

const PRINTED: &str = "=== a.yaml ===
spec: x # kept
fixed: true
=== b.yaml ===
spec: {}
x: 1
z: 2
fixed: true
=== c.yaml ===
spec: y
fixed: true
Modified: 3, Skipped: 0, Failed: 0
";

const DIFFED: &str = "=== a.yaml ===
--- a/a.yaml
+++ b/a.yaml
@@ -1 +1,2 @@
 spec: x # kept
+fixed: true
=== b.yaml ===
--- a/b.yaml
+++ b/b.yaml
@@ -1 +1,4 @@
 spec: {}
+x: 1
+z: 2
+fixed: true
=== c.yaml ===
--- a/c.yaml
+++ b/c.yaml
@@ -1 +1,2 @@
 spec: y
+fixed: true
Modified: 3, Skipped: 0, Failed: 0
";

const CHECKED: &str = "would modify: a.yaml\nwould modify: b.yaml\nwould modify: c.yaml\n";

const WRITTEN: &str = "Modified: 3, Skipped: 0, Failed: 0\n";

/// Each write mode with what it prints for [`FILES`].
const MODES: [(&[&str], &str); 4] = [
    (&[], PRINTED),
    (&["--diff"], DIFFED),
    (&["--check"], CHECKED),
    (&["-i"], WRITTEN),
];

fn contents(scratch: &Scratch) -> Vec<(&'static str, String)> {
    NAMES
        .iter()
        .map(|name| (*name, read(scratch, name)))
        .collect()
}

fn owned(files: [(&'static str, &'static str); 3]) -> Vec<(&'static str, String)> {
    files.map(|(name, text)| (name, text.to_owned())).to_vec()
}

#[test]
fn every_mode_makes_the_other_fixes_and_exits_3_serially_and_in_parallel() {
    let stderr = raised("a.yaml", "x") + &raised("c.yaml", "y") + &summary(4);
    for (mode, stdout) in MODES {
        for jobs in ["1", "4"] {
            let scratch = scratch(&FILES);
            let flags = [mode, &["--parallel", jobs]].concat();
            assert_eq!(
                fix(&scratch, &flags, &NAMES),
                Run {
                    code: 3,
                    stdout: stdout.to_owned(),
                    stderr: stderr.clone(),
                },
                "{flags:?}"
            );
            let written = if mode == ["-i"] { FIXED } else { FILES };
            assert_eq!(contents(&scratch), owned(written), "{flags:?}");
        }
    }
}

#[test]
fn allow_rule_errors_is_the_output_from_before_in_every_mode() {
    let stderr = allowed("x") + &allowed("y");
    for (mode, stdout) in MODES {
        let scratch = scratch(&FILES);
        // `--check` has its own failure then: files to fix.
        let (code, last) = match mode {
            ["--check"] => (1, "3 file(s) would be modified\n"),
            _ => (0, ""),
        };
        assert_eq!(
            fix(&scratch, mode, &with_allow(&NAMES)),
            Run {
                code,
                stdout: stdout.to_owned(),
                stderr: format!("{stderr}{last}"),
            },
            "{mode:?}"
        );
        let written = if mode == ["-i"] { FIXED } else { FILES };
        assert_eq!(contents(&scratch), owned(written), "{mode:?}");
    }
}

fn with_allow<'a>(args: &[&'a str]) -> Vec<&'a str> {
    [&["--allow-rule-errors"], args].concat()
}

#[test]
fn one_file_prints_the_fixed_text_alone_and_exits_3() {
    let scratch = scratch(&FILES);
    let stderr = raised("a.yaml", "x") + &summary(2);
    for (mode, stdout) in [
        (&[][..], "spec: x # kept\nfixed: true\n"),
        (
            &["--diff"],
            "--- a/a.yaml\n+++ b/a.yaml\n@@ -1 +1,2 @@\n spec: x # kept\n+fixed: true\n",
        ),
        (&["--check"], "would modify: a.yaml\n"),
        (&["-q", "-i"], ""),
        // Nothing is left to fix, and the rules still did not run.
        (&["--check"], ""),
        (&["-i"], ""),
    ] {
        let run = fix(&scratch, mode, &["a.yaml"]);
        assert_eq!(
            run,
            Run {
                code: 3,
                stdout: stdout.to_owned(),
                stderr: stderr.clone(),
            },
            "{mode:?}"
        );
    }
    assert_eq!(contents(&scratch)[0], ("a.yaml", FIXED[0].1.to_owned()));
    // A file no rule raises on, the same way: clean.
    let run = fix(&scratch, &["-i"], &["b.yaml"]);
    assert_eq!((run.code, run.stderr.as_str()), (0, ""));
    assert_eq!(fix(&scratch, &["--check"], &["b.yaml"]).code, 0);
}

#[test]
fn stdin_is_fixed_and_its_rule_errors_name_the_dash() {
    let scratch = scratch(&[]);
    let args = ["fix", "--rules", "rules", "--input-format", "yaml", "-"];
    let run = scratch.run_with_stdin(&args, "spec: x\n");
    assert_eq!(
        run,
        Run {
            code: 3,
            stdout: "spec: x\nfixed: true\n".to_owned(),
            stderr: raised("-", "x") + &summary(2),
        }
    );
}

/// Documents 0 and 2 raise the same, 3 another text; 1 is fixed whole.
const STREAM: &str = "spec: x\n---\nspec: {}\n---\nspec: x\n---\nspec: w\n";

#[test]
fn a_stream_has_each_rule_error_once_and_every_other_fix() {
    let scratch = scratch(&[("s.yaml", STREAM)]);
    let fixed = "spec: x\nfixed: true\n---\nspec: {}\nx: 1\nz: 2\nfixed: true\n---\nspec: x\nfixed: true\n---\nspec: w\nfixed: true\n";
    let run = fix(&scratch, &[], &["s.yaml"]);
    let stderr = raised("s.yaml", "x") + &raised("s.yaml", "w") + &summary(4);
    assert_eq!((run.code, run.stdout.as_str()), (3, fixed));
    assert_eq!(run.stderr, stderr);

    // Document 1 alone raises nothing; document 3 alone has its own error.
    let run = fix(&scratch, &["--doc", "1"], &["s.yaml"]);
    assert_eq!((run.code, run.stderr.as_str()), (0, ""));
    let run = fix(&scratch, &["--doc", "3"], &["s.yaml"]);
    assert_eq!(
        run,
        Run {
            code: 3,
            stdout: STREAM.to_owned() + "fixed: true\n",
            stderr: raised("s.yaml", "w") + &summary(2),
        }
    );

    let run = fix(&scratch, &["-i"], &["s.yaml"]);
    assert_eq!((run.code, run.stderr), (3, stderr));
    assert_eq!(read(&scratch, "s.yaml"), fixed);
    // Allowed, every document warns, as before.
    let scratch = self::scratch(&[("s.yaml", STREAM)]);
    let run = fix(&scratch, &["-i"], &with_allow(&["s.yaml"]));
    let every = allowed("x") + &allowed("x") + &allowed("w");
    assert_eq!((run.code, run.stderr), (0, every));
    assert_eq!(read(&scratch, "s.yaml"), fixed);
}

#[test]
fn a_file_named_twice_is_fixed_once_and_has_its_rule_errors_once() {
    let scratch = scratch(&FILES);
    for jobs in ["1", "4"] {
        let run = fix(&scratch, &["--parallel", jobs], &["a.yaml", "a.yaml"]);
        assert_eq!(run.code, 3);
        assert_eq!(run.stderr, raised("a.yaml", "x") + &summary(2), "{jobs}");
    }
    let run = fix(&scratch, &["-i"], &["a.yaml", "a.yaml"]);
    assert_eq!(run.stdout, "Modified: 1, Skipped: 1, Failed: 0\n");
    assert_eq!(read(&scratch, "a.yaml"), FIXED[0].1);
}

#[test]
fn a_failed_file_is_the_runs_error_and_the_rule_errors_are_still_named() {
    let files = ["a.yaml", "broken.yaml", "c.yaml"];
    let stage = || {
        let scratch = scratch(&FILES);
        scratch.write("broken.yaml", BROKEN);
        scratch
    };
    for jobs in ["1", "4"] {
        // Stopped at the file that failed: what came before it is written.
        let scratch = stage();
        let run = fix(&scratch, &["-i", "--parallel", jobs], &files);
        let stderr = raised("a.yaml", "x") + &raised("c.yaml", "y") + PARSE_ERROR;
        assert_eq!(
            run,
            Run {
                code: 3,
                stdout: String::new(),
                stderr,
            }
        );
        assert_eq!(read(&scratch, "a.yaml"), FIXED[0].1);
        assert_eq!(read(&scratch, "c.yaml"), FILES[2].1);

        // Every other file is written, and the failed one counts as 7: a
        // rule error (3) does not hide it, nor the other way round.
        let scratch = stage();
        let flags = ["-i", "--continue-on-error", "--parallel", jobs];
        let run = fix(&scratch, &flags, &files);
        let failed = "Modified: 2, Skipped: 0, Failed: 1\n  - broken.yaml: parse error in \
             broken.yaml: did not find expected node content, while parsing a flow node \
             (line 2, column 1)\n";
        let stderr =
            raised("a.yaml", "x") + &raised("c.yaml", "y") + "1 file(s) failed in bulk run\n";
        assert_eq!(
            run,
            Run {
                code: 7,
                stdout: failed.to_owned(),
                stderr,
            }
        );
        assert_eq!(read(&scratch, "a.yaml"), FIXED[0].1);
        assert_eq!(read(&scratch, "broken.yaml"), BROKEN);
        assert_eq!(read(&scratch, "c.yaml"), FIXED[2].1);
    }
    // `--allow-rule-errors` accepts the rules, not the file.
    let scratch = stage();
    let flags = ["-i", "--continue-on-error"];
    let run = fix(&scratch, &flags, &with_allow(&files));
    let stderr = allowed("x") + &allowed("y") + "1 file(s) failed in bulk run\n";
    assert_eq!((run.code, run.stderr), (7, stderr));
}

#[cfg(unix)]
#[test]
fn a_failed_write_is_the_runs_error_with_and_without_continue_on_error() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = scratch(&FILES);
    scratch.write("locked/d.yaml", "spec: w\n");
    let mode = |mode| {
        let permissions = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(scratch.path("locked"), permissions).expect("chmod");
    };
    mode(0o555);
    let root = std::fs::write(scratch.path("locked/probe"), "").is_ok();
    let files = ["a.yaml", "locked/d.yaml", "c.yaml"];
    let stopped = fix(&scratch, &["-i"], &files);
    let after = [read(&scratch, "a.yaml"), read(&scratch, "c.yaml")];
    let continued = fix(&scratch, &["-i", "--continue-on-error"], &files);
    mode(0o755);
    if root {
        // Nothing is read-only to root.
        return;
    }
    let raised = raised("a.yaml", "x") + &raised("locked/d.yaml", "w") + &raised("c.yaml", "y");
    assert_eq!(stopped.code, 7);
    let rest = stopped.stderr.strip_prefix(&raised).expect("rule errors");
    assert!(
        rest.starts_with("I/O error writing 'locked/d.yaml'"),
        "{rest}"
    );
    assert_eq!(after, [FIXED[0].1, FILES[2].1]);
    assert_eq!(
        (continued.code, continued.stderr),
        (7, raised + "1 file(s) failed in bulk run\n")
    );
    assert_eq!(read(&scratch, "c.yaml"), FIXED[2].1);
    assert_eq!(read(&scratch, "locked/d.yaml"), "spec: w\n");
}

#[test]
fn a_fix_that_fails_after_a_rule_error_is_the_runs_error() {
    let stage = || {
        let scratch = scratch(&FILES);
        scratch.write("boom.yaml", "spec: z\nboom: true\n");
        scratch
    };
    let failure = "rule t.boom fix failed: fix.ops runtime error: jq runtime error: \"boom\"\n";
    let rules = ["--rules", "boom.yml"];
    let scratch = stage();
    let run = fix(&scratch, &["-i"], &[&rules[..], &["boom.yaml"]].concat());
    assert_eq!(
        run,
        Run {
            code: 3,
            stdout: String::new(),
            stderr: raised("boom.yaml", "z") + failure,
        }
    );
    // The file is left alone: its fixes were not all made.
    assert_eq!(read(&scratch, "boom.yaml"), "spec: z\nboom: true\n");

    let files = [&rules[..], &["a.yaml", "boom.yaml"]].concat();
    for jobs in ["1", "4"] {
        let scratch = stage();
        let flags = ["-i", "--continue-on-error", "--parallel", jobs];
        let run = fix(&scratch, &flags, &files);
        let stderr =
            raised("a.yaml", "x") + &raised("boom.yaml", "z") + "1 file(s) failed in bulk run\n";
        assert_eq!((run.code, run.stderr), (7, stderr));
        assert_eq!(
            run.stdout,
            format!("Modified: 1, Skipped: 0, Failed: 1\n  - boom.yaml: {failure}")
        );
        assert_eq!(read(&scratch, "a.yaml"), FIXED[0].1);
    }
}

#[test]
fn rule_errors_of_a_parallel_run_come_in_file_order() {
    let files: Vec<(String, String)> = (0..24)
        // Every third file raises nothing; the long ones finish late.
        .map(|n| {
            let pad = "# pad\n".repeat(if n % 5 == 0 { 4000 } else { 0 });
            let spec = if n % 3 == 0 {
                "{}".to_owned()
            } else {
                format!("s{n}")
            };
            (format!("f{n:02}.yaml"), format!("{pad}spec: {spec}\n"))
        })
        .collect();
    let scratch = scratch(&[]);
    for (path, text) in &files {
        scratch.write(path, text);
    }
    let names: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
    let expected: String = (0..24)
        .filter(|n| n % 3 != 0)
        .map(|n| raised(&format!("f{n:02}.yaml"), &format!("s{n}")))
        .chain([summary(32)])
        .collect();
    let serial = fix(&scratch, &["--parallel", "1"], &names);
    assert_eq!((serial.code, &serial.stderr), (3, &expected));
    for _ in 0..5 {
        assert_eq!(fix(&scratch, &["--parallel", "4"], &names), serial);
    }
}

/// The workers of a parallel run log to stderr, which the main thread must
/// not hold while it waits for them: it did, and such a run never ended
/// ([`scratch::DEADLINE`] kills it).
#[test]
fn a_parallel_run_whose_rules_log_ends() {
    let names: Vec<String> = (0..12).map(|n| format!("f{n:02}.yaml")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let scratch = scratch(&[]);
    for name in &names {
        scratch.write(name, "spec: x\n");
    }
    let warning = "WARN fix.ops is non-idempotent; skipping (second application produced \
         non-empty script) rule_id=t.never-done";
    for mode in ["--check", "--diff", "-i"] {
        // A warning of the fix engine, one per file, and no rule error.
        let args = ["fix", "--rules", "never-done.yml"];
        let args = [&["--parallel", "4", mode], &args[..], &names].concat();
        let run = scratch.run(&args);
        let warnings: Vec<&str> = run.stderr.lines().collect();
        assert_eq!((run.code, warnings), (0, vec![warning; 12]), "{mode}");
    }
    // The audit lines of `-v`, beside rules that raise.
    let run = fix(&scratch, &["--parallel", "4", "-v", "-i"], &names);
    assert_eq!(run.code, 3);
    let audited = |line: &&str| line.starts_with("INFO applied fix.ops rule_id=t.fixes");
    assert_eq!(run.stderr.lines().filter(audited).count(), 12);
    assert!(run.stderr.ends_with(&summary(24)), "{}", run.stderr);
    // The warnings `--allow-rule-errors` gives.
    let run = fix(&scratch, &["--parallel", "4", "-i"], &with_allow(&names));
    let warnings = run.stderr.lines().filter(|l| l.starts_with("WARN "));
    assert_eq!((run.code, warnings.count()), (0, 24));
}
