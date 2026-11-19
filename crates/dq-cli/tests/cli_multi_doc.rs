//! Multi-document YAML streams: `lint` / `fix` treat every document as a
//! document of its own, and `--doc` is honoured — or refused with a usage
//! error — by every command.
//!
//! The first tests drive `dq::run` in-process, like `cli_lint_smoke.rs`.
//! The rest spawn the binary through `assert_cmd` (see [`Scratch`]): they
//! pin the user-facing contract — exit codes, the stdout / stderr split,
//! what lands on disk — and can be run against another `dq` build by
//! pointing `CARGO_BIN_EXE_dq` at it.

use std::io::Write as _;

use clap::Parser;
use tempfile::{NamedTempFile, TempDir, TempPath};

/// Two Pods; only the second one is named `b`.
const STREAM: &str = "\
apiVersion: v1
kind: Pod
metadata: {name: a}
---
apiVersion: v1
kind: Pod
metadata: {name: b}
";

/// Fires on every Pod not named `fixed`, at `/metadata/name`, and fixes it
/// with a `fix.ops` replace. `match.filter` only matches a single document
/// — an array of documents is not `kind: Pod`.
const RULE: &str = r#"
id: test.pod-name
description: every Pod is named 'fixed'
severity: error
match:
  format: yaml
  filter: '.kind == "Pod"'
check:
  jq: 'select(.metadata.name != "fixed") | {name: .metadata.name, pointer: "/metadata/name"}'
  message: "Pod '{{ .name }}' is not named 'fixed'"
loc:
  pointer: '.pointer'
fix:
  ops: |
    if .metadata.name != "fixed"
    then [{"op":"replace","path":"/metadata/name","value":"fixed"}]
    else [] end
"#;

fn temp(contents: &str, suffix: &str) -> TempPath {
    let mut file = NamedTempFile::with_suffix(suffix).expect("tempfile");
    file.write_all(contents.as_bytes()).expect("write");
    file.into_temp_path()
}

fn path(p: &TempPath) -> &str {
    p.to_str().expect("UTF-8 path")
}

/// Run `dq` in-process; `Ok(stdout)` or `Err((exit code, message))`.
fn run(argv: &[&str]) -> Result<String, (i32, String)> {
    let cli = dq::Cli::try_parse_from(argv).map_err(|e| (e.exit_code(), e.to_string()))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    match dq::run(&cli, false, &mut out, &mut err) {
        Ok(()) => Ok(String::from_utf8(out).expect("UTF-8 stdout")),
        Err(e) => Err((dq::exit_code::exit_code_for_error(&e), e.to_string())),
    }
}

/// Run `dq ... lint` with `-F json`; the exit code and the lines and
/// messages of the diagnostics.
fn lint(argv: &[&str]) -> (i32, Vec<(u64, String)>) {
    let cli = dq::Cli::try_parse_from(argv).expect("clap parse");
    let mut out = Vec::new();
    let code = match dq::run(&cli, false, &mut out, &mut Vec::new()) {
        Ok(()) => 0,
        Err(e) => dq::exit_code::exit_code_for_error(&e),
    };
    let body: serde_json::Value = serde_json::from_slice(&out).expect("JSON body");
    let found = body["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["line"].as_u64().expect("line"),
                d["message"].as_str().expect("message").to_owned(),
            )
        })
        .collect();
    (code, found)
}

#[test]
fn lint_evaluates_every_document_of_a_stream_on_its_own() {
    let rule = temp(RULE, ".yml");
    let doc = temp(STREAM, ".yaml");
    let (code, found) = lint(&[
        "dq",
        "-F",
        "json",
        "lint",
        "--rules",
        path(&rule),
        path(&doc),
    ]);
    assert_eq!(code, 4, "error-severity diagnostics fail the lint");
    assert_eq!(
        found,
        vec![
            (3, "Pod 'a' is not named 'fixed'".to_owned()),
            (7, "Pod 'b' is not named 'fixed'".to_owned()),
        ],
        "one diagnostic per document, at its line in the file"
    );
}

#[test]
fn lint_doc_limits_the_stream_to_one_document() {
    let rule = temp(RULE, ".yml");
    let doc = temp(STREAM, ".yaml");
    let (_, found) = lint(&[
        "dq",
        "-F",
        "json",
        "--doc",
        "1",
        "lint",
        "--rules",
        path(&rule),
        path(&doc),
    ]);
    let lines: Vec<u64> = found.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, vec![7]);
}

#[test]
fn fix_fixes_every_document_and_keeps_the_stream() {
    let rule = temp(RULE, ".yml");
    let doc = temp(STREAM, ".yaml");
    run(&["dq", "-i", "fix", "--rules", path(&rule), path(&doc)]).expect("fix");
    let written = std::fs::read_to_string(&doc).unwrap();
    assert_eq!(
        written,
        STREAM
            .replace("{name: a}", "{name: fixed}")
            .replace("{name: b}", "{name: fixed}")
    );
}

#[test]
fn set_doc_edits_that_document_and_keeps_the_others() {
    let doc = temp(STREAM, ".yaml");
    let out = run(&["dq", "--doc", "1", "set", path(&doc), "/metadata/name", "z"]).expect("set");
    assert_eq!(out, STREAM.replace("{name: b}", "{name: z}"));
}

#[test]
fn set_jq_over_a_stream_writes_a_stream() {
    let doc = temp(STREAM, ".yaml");
    let out = run(&[
        "dq",
        "set",
        path(&doc),
        "--jq",
        ".[1].metadata.name = \"z\"",
    ])
    .expect("set");
    assert_eq!(out.matches("---").count(), 1, "still two documents:\n{out}");
    assert!(out.contains("name: z"), "{out}");
}

#[test]
fn get_doc_out_of_range_is_not_found() {
    let doc = temp(STREAM, ".yaml");
    let (code, message) = run(&["dq", "--doc", "2", "get", path(&doc), "/kind"]).unwrap_err();
    assert_eq!(code, 2);
    assert!(message.contains("--doc 2 is out of range"), "{message}");
    assert!(message.contains("holds 2 document(s)"), "{message}");
}

#[test]
fn doc_on_a_command_that_cannot_honour_it_is_a_usage_error() {
    let doc = temp(STREAM, ".yaml");
    let (code, message) = run(&["dq", "--doc", "1", "fmt", path(&doc)]).unwrap_err();
    assert_eq!(code, 2);
    assert!(message.contains("not supported by `dq fmt`"), "{message}");

    let (code, _) = run(&["dq", "--doc", "all", "del", path(&doc), "/kind"]).unwrap_err();
    assert_eq!(code, 2);
}

#[test]
fn doc_value_that_is_neither_index_nor_all_is_a_usage_error() {
    let (code, _) = run(&["dq", "--doc", "x", "get", "f.yaml", "/a"]).unwrap_err();
    assert_eq!(code, 2);
}

// ---------------------------------------------------------------------------
// The binary, through assert_cmd.
// ---------------------------------------------------------------------------

/// A single-document file: the same shape as one document of [`STREAM`],
/// already named `fixed`, so [`RULE`] finds nothing in it.
const SINGLE: &str = "\
apiVersion: v1
kind: Pod
metadata: {name: fixed}
";

/// The stdout of `dq ...` on success, or the exit code and the stderr.
type Outcome = Result<String, (i32, String)>;

/// A scratch directory every command runs in, so a file is named by its
/// bare name and messages that quote it are deterministic.
struct Scratch {
    dir: TempDir,
}

impl Scratch {
    /// A directory holding `s.yaml` ([`STREAM`]), `one.yaml` ([`SINGLE`])
    /// and `rule.yml` ([`RULE`]).
    fn new() -> Self {
        let scratch = Self {
            dir: TempDir::new().expect("tempdir"),
        };
        scratch.write("s.yaml", STREAM);
        scratch.write("one.yaml", SINGLE);
        scratch.write("rule.yml", RULE);
        scratch
    }

    fn write(&self, name: &str, contents: &str) {
        let path = self.dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, contents).expect("write fixture");
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).expect("read back")
    }

    fn exists(&self, name: &str) -> bool {
        self.dir.path().join(name).exists()
    }

    /// `dq --no-color` in this directory, with a clean environment: no
    /// `NO_COLOR` / `RUST_LOG` from the developer's shell, and a proxy that
    /// refuses every connection, so nothing a test runs reaches the network.
    fn dq(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("dq").expect("dq binary built");
        cmd.env_clear();
        if let Ok(p) = std::env::var("PATH") {
            cmd.env("PATH", p);
        }
        cmd.env("HOME", self.dir.path())
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .current_dir(self.dir.path())
            .arg("--no-color");
        cmd
    }

    /// Run `dq --no-color <args>`. A success must leave stderr free of
    /// errors; a failure must leave stdout empty.
    fn run(&self, args: &[&str]) -> Outcome {
        self.run_with_stdin(args, "")
    }

    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Outcome {
        let output = self
            .dq()
            .args(args)
            .write_stdin(stdin)
            .output()
            .expect("spawn dq");
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
        match output.status.code() {
            Some(0) => Ok(stdout),
            Some(code) => {
                assert_eq!(stdout, "", "dq {args:?} failed with exit {code}: no stdout");
                Err((code, stderr))
            }
            None => panic!("dq {args:?} was killed by a signal"),
        }
    }

    /// `dq -F json <args>` for a lint-family command: the exit code and
    /// `(rule_id, line, col)` of every diagnostic, in output order.
    fn diagnostics(&self, args: &[&str]) -> (i32, Vec<(String, u64, u64)>) {
        let output = self
            .dq()
            .args(["-F", "json"])
            .args(args)
            .output()
            .expect("spawn dq");
        let code = output.status.code().expect("exit code");
        let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "dq {args:?}: stdout is not one JSON document ({e}); exit {code}, stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        let found = body["diagnostics"]
            .as_array()
            .expect("diagnostics array")
            .iter()
            .map(|d| {
                (
                    d["rule_id"].as_str().expect("rule_id").to_owned(),
                    d["line"].as_u64().expect("line"),
                    d["col"].as_u64().expect("col"),
                )
            })
            .collect();
        (code, found)
    }
}

fn diag(rule_id: &str, line: u64, col: u64) -> (String, u64, u64) {
    (rule_id.to_owned(), line, col)
}

/// The usage error `--doc <shown>` on `dq <command>` must give: exit 2 and
/// clap's rendering — `error: …`, the usage line, the `--help` hint.
fn assert_doc_usage_error(outcome: Outcome, shown: &str, command: &str) {
    let (code, stderr) = outcome.expect_err("--doc must be refused");
    assert_eq!(code, 2, "a usage error exits 2: {stderr}");
    let head = format!("error: `--doc {shown}` is not supported by `dq {command}`: ");
    assert!(stderr.starts_with(&head), "{stderr}");
    assert!(
        stderr.ends_with(
            "\n\nUsage: dq [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.\n"
        ),
        "rendered like clap's own usage errors: {stderr:?}"
    );
}

/// Every command that cannot honour `--doc` refuses it before it does
/// anything, with the same clap-rendered usage error.
#[test]
fn doc_is_refused_by_every_command_that_cannot_honour_it() {
    let scratch = Scratch::new();
    scratch.write("rules/.keep", "");
    let refused: &[(&str, &[&str])] = &[
        ("fmt", &["fmt", "s.yaml"]),
        ("validate", &["validate", "s.yaml"]),
        ("diff", &["diff", "s.yaml", "one.yaml"]),
        ("patch", &["patch", "one.yaml", "[]"]),
        ("merge", &["merge", "one.yaml", "{}"]),
        ("test", &["test", "rules"]),
        ("explain", &["explain", "k8s.no-latest-tag"]),
        ("rules", &["rules", "list"]),
        ("completions", &["completions", "bash"]),
        ("man", &["man"]),
        ("self", &["self", "check"]),
        ("generate-docs", &["generate-docs", "--output-dir", "out"]),
    ];
    for (command, args) in refused {
        let argv: Vec<&str> = ["--doc", "1"].iter().chain(args.iter()).copied().collect();
        assert_doc_usage_error(scratch.run(&argv), "1", command);
    }
    assert!(!scratch.exists("out"), "generate-docs wrote nothing");
    assert_eq!(scratch.read("s.yaml"), STREAM);
}

#[test]
fn doc_all_is_refused_by_set_and_del() {
    let scratch = Scratch::new();
    assert_doc_usage_error(
        scratch.run(&["--doc", "all", "set", "s.yaml", "/kind", "X"]),
        "all",
        "set",
    );
    assert_doc_usage_error(
        scratch.run(&["--doc", "all", "del", "s.yaml", "/kind"]),
        "all",
        "del",
    );
    assert_doc_usage_error(
        scratch.run(&["--doc", "all", "-i", "set", "s.yaml", "--jq", "."]),
        "all",
        "set",
    );
    assert_eq!(scratch.read("s.yaml"), STREAM, "-i wrote nothing");
}

/// `convert -i --doc N` would write one document and remove the source,
/// losing the others: refused, and the source stays where it was.
#[test]
fn convert_in_place_refuses_doc_and_keeps_the_source() {
    let scratch = Scratch::new();
    assert_doc_usage_error(
        scratch.run(&["--doc", "0", "-F", "json", "-i", "convert", "s.yaml"]),
        "0",
        "convert -i",
    );
    assert_eq!(scratch.read("s.yaml"), STREAM);
    assert!(!scratch.exists("s.json"), "no converted sibling");
}

#[test]
fn doc_value_that_is_neither_index_nor_all_is_rendered_by_clap() {
    let scratch = Scratch::new();
    for bad in ["x", "-1", "ALL"] {
        let (code, stderr) = scratch
            .run(&[&format!("--doc={bad}"), "get", "s.yaml", "/kind"])
            .unwrap_err();
        assert_eq!(code, 2, "{bad}: {stderr}");
        assert!(
            stderr.starts_with(&format!(
                "error: invalid value '{bad}' for '--doc <N|all>': \
                 expected a document index (0, 1, ...) or 'all'"
            )),
            "{bad}: {stderr}"
        );
    }
}

/// Every command that honours `--doc`, with `F` standing for the file.
const HONOURED: &[&[&str]] = &[
    &["get", "F", "/kind"],
    &["exists", "F", "/kind"],
    &["keys", "F", "/metadata"],
    &["values", "F", "/metadata"],
    &["len", "F", "/metadata"],
    &["type", "F", "/kind"],
    &["paths", "F"],
    &["select", "F", "$.kind"],
    &["query", ".kind", "F"],
    &["convert", "F"],
    &["set", "F", "/kind", "X"],
    &["del", "F", "/kind"],
    &["lint", "--rules", "rule.yml", "F"],
    &["check", "--rule", "rule.yml", "F"],
    &["fix", "--rules", "rule.yml", "F"],
];

fn honoured_argv<'a>(doc: &'a str, template: &[&'a str], file: &'a str) -> Vec<&'a str> {
    let mut argv = vec!["--doc", doc];
    argv.extend(template.iter().map(|a| if *a == "F" { file } else { a }));
    argv
}

/// An index past the last document is `DocOutOfRange`: exit 2, the same
/// message from every command, on a stream and on a file that is not one
/// (which is a stream of one document).
#[test]
fn doc_out_of_range_exits_2_from_every_command_that_honours_doc() {
    let scratch = Scratch::new();
    let cases = [
        (
            "s.yaml",
            "2",
            "--doc 2 is out of range: s.yaml holds 2 document(s) (--doc 0 to 1)\n",
        ),
        (
            "one.yaml",
            "1",
            "--doc 1 is out of range: one.yaml holds 1 document(s) (--doc 0)\n",
        ),
    ];
    for template in HONOURED {
        for (file, doc, message) in cases {
            let argv = honoured_argv(doc, template, file);
            assert_eq!(
                scratch.run(&argv),
                Err((2, message.to_owned())),
                "dq {argv:?}"
            );
        }
    }
    assert_eq!(scratch.read("s.yaml"), STREAM);
    assert_eq!(scratch.read("one.yaml"), SINGLE);
}

/// The other half of the pair above: `--doc 0` is accepted by the same
/// commands on a file that is not a stream.
#[test]
fn doc_zero_on_a_single_document_file_is_accepted_by_every_command() {
    let scratch = Scratch::new();
    for template in HONOURED {
        let argv = honoured_argv("0", template, "one.yaml");
        assert!(scratch.run(&argv).is_ok(), "dq {argv:?}");
    }
}

/// `--doc all` on a file that is not a stream is the array of its one
/// document, so `/0/...` addresses it the way it would in a stream.
#[test]
fn doc_all_on_a_single_document_file_is_an_array_of_one() {
    let scratch = Scratch::new();
    let all = |args: &[&str]| {
        let argv: Vec<&str> = ["--doc", "all"].iter().chain(args).copied().collect();
        scratch.run(&argv)
    };
    assert_eq!(all(&["get", "one.yaml", "/0/kind"]), Ok("Pod\n".to_owned()));
    assert_eq!(all(&["len", "one.yaml", ""]), Ok("1\n".to_owned()));
    assert_eq!(all(&["type", "one.yaml", ""]), Ok("array\n".to_owned()));
    assert_eq!(
        all(&["query", "map(.kind)", "one.yaml"]),
        Ok("[Pod]\n".to_owned())
    );
    let paths = all(&["paths", "one.yaml"]).expect("paths");
    assert!(paths.starts_with(": array\n/0: object\n"), "{paths}");

    // Without `--doc` the file is its document: no `/0` in it.
    let (code, _) = scratch.run(&["get", "one.yaml", "/0/kind"]).unwrap_err();
    assert_eq!(code, 2);
    assert_eq!(scratch.run(&["len", "one.yaml", ""]), Ok("3\n".to_owned()));
}

#[test]
fn paths_and_select_honour_doc_on_a_stream() {
    let scratch = Scratch::new();
    assert_eq!(
        scratch.run(&["--doc", "1", "paths", "s.yaml"]),
        Ok(": object\n/apiVersion: string\n/kind: string\n/metadata: object\n/metadata/name: string\n"
            .to_owned())
    );
    assert_eq!(
        scratch.run(&["--doc", "1", "select", "s.yaml", "$.metadata.name"]),
        Ok("b\n".to_owned())
    );
    // Without `--doc`, paths lists every document under its `/N`.
    let every = scratch.run(&["paths", "s.yaml"]).expect("paths");
    assert!(
        every.contains("/0/metadata/name: string\n")
            && every.contains("/1/metadata/name: string\n"),
        "{every}"
    );
}

#[test]
fn convert_doc_writes_one_document() {
    let scratch = Scratch::new();
    let one = scratch
        .run(&["--doc", "1", "-F", "json", "convert", "s.yaml"])
        .expect("convert");
    let value: serde_json::Value = serde_json::from_str(&one).expect("JSON");
    assert_eq!(
        value,
        serde_json::json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "b"}})
    );
    // Without `--doc` the stream is the array of its documents.
    let every = scratch
        .run(&["-F", "json", "convert", "s.yaml"])
        .expect("convert");
    let value: serde_json::Value = serde_json::from_str(&every).expect("JSON");
    assert_eq!(value.as_array().map(Vec::len), Some(2), "{every}");
}

/// `--doc N` makes the pointer of `set` / `del` relative to document `N`;
/// without `--doc`, `/N/...` makes the very same edit.
#[test]
fn set_and_del_doc_address_one_document_like_an_index_prefix() {
    let scratch = Scratch::new();
    let set_doc = scratch.run(&["--doc", "1", "set", "s.yaml", "/metadata/name", "z"]);
    let set_prefix = scratch.run(&["set", "s.yaml", "/1/metadata/name", "z"]);
    assert_eq!(set_doc, Ok(STREAM.replace("{name: b}", "{name: z}")));
    assert_eq!(set_doc, set_prefix);

    let del_doc = scratch.run(&["--doc", "0", "del", "s.yaml", "/apiVersion"]);
    let del_prefix = scratch.run(&["del", "s.yaml", "/0/apiVersion"]);
    assert_eq!(del_doc, Ok(STREAM.replacen("apiVersion: v1\n", "", 1)));
    assert_eq!(del_doc, del_prefix);

    // On a file that is not a stream `--doc 0` is the file itself.
    assert_eq!(
        scratch.run(&["--doc", "0", "set", "one.yaml", "/kind", "X"]),
        Ok(SINGLE.replace("kind: Pod", "kind: X"))
    );
}

#[test]
fn set_jq_doc_transforms_that_document_and_keeps_the_stream() {
    let scratch = Scratch::new();
    let expected = "\
apiVersion: v1
kind: Pod
metadata:
  name: a
---
apiVersion: v1
kind: Pod
metadata:
  name: z
";
    assert_eq!(
        scratch.run(&[
            "--doc",
            "1",
            "set",
            "s.yaml",
            "--jq",
            ".metadata.name = \"z\""
        ]),
        Ok(expected.to_owned())
    );
    scratch
        .run(&[
            "--doc",
            "1",
            "-i",
            "set",
            "s.yaml",
            "--jq",
            ".metadata.name = \"z\"",
        ])
        .expect("set -i");
    assert_eq!(scratch.read("s.yaml"), expected);
}

/// Without `--doc` the filter sees the array of the documents and must
/// return one; anything else is exit 6 and `-i` leaves the file alone.
#[test]
fn set_jq_over_a_stream_must_return_an_array_of_documents() {
    let scratch = Scratch::new();
    let message = "s.yaml is a stream of 2 documents: --jq sees them as an array and must \
                   return an array of documents (got object); pass --doc N to transform one \
                   document\n";
    assert_eq!(
        scratch.run(&["set", "s.yaml", "--jq", ".[0]"]),
        Err((6, message.to_owned()))
    );
    assert_eq!(
        scratch.run(&["-i", "set", "s.yaml", "--jq", ".[0]"]),
        Err((6, message.to_owned()))
    );
    assert_eq!(scratch.read("s.yaml"), STREAM, "untouched on failure");

    // An out-of-range `--doc` with `-i` leaves it alone too.
    let (code, _) = scratch
        .run(&["--doc", "5", "-i", "set", "s.yaml", "--jq", "."])
        .unwrap_err();
    assert_eq!(code, 2);
    assert_eq!(scratch.read("s.yaml"), STREAM);

    // An array of documents is a stream of that many documents.
    let out = scratch
        .run(&["set", "s.yaml", "--jq", ". + [{kind: \"Extra\"}]"])
        .expect("set --jq");
    assert_eq!(out.matches("\n---\n").count(), 2, "three documents:\n{out}");
    assert!(out.ends_with("---\nkind: Extra\n"), "{out}");

    // A single-document file takes any result, as before.
    assert_eq!(
        scratch.run(&["set", "one.yaml", "--jq", ".kind"]),
        Ok("Pod\n".to_owned())
    );
}

/// The first Pod is fine, the second one uses `nginx:latest`: the
/// `@std/k8s` rule fires in the second document, at its line in the file.
#[test]
fn std_k8s_rules_fire_per_document_of_a_bundle() {
    let scratch = Scratch::new();
    scratch.write(
        "bundle.yaml",
        "\
apiVersion: v1
kind: Pod
metadata: {name: web}
spec:
  containers:
    - {name: web, image: \"nginx:1.25\"}
---
apiVersion: v1
kind: Pod
metadata: {name: api}
spec:
  containers:
    - {name: api, image: \"nginx:latest\"}
",
    );
    let (code, found) = scratch.diagnostics(&["lint", "--rules", "@std/k8s", "bundle.yaml"]);
    assert_eq!(code, 4, "{found:?}");
    let latest: Vec<_> = found
        .iter()
        .filter(|(id, _, _)| id == "k8s.no-latest-tag")
        .collect();
    assert_eq!(latest, vec![&diag("k8s.no-latest-tag", 13, 26)]);
    // Rules that fire on both Pods do so once per document.
    let probes: Vec<u64> = found
        .iter()
        .filter(|(id, _, _)| id == "k8s.missing-liveness-probe")
        .map(|(_, line, _)| *line)
        .collect();
    assert_eq!(probes, vec![6, 13]);
}

/// A diagnostic without a position lands on the first line of its
/// document that holds a value — not on line 1 of the file.
#[test]
fn a_position_less_diagnostic_lands_at_the_start_of_its_document() {
    let scratch = Scratch::new();
    scratch.write(
        "no-loc.yml",
        "\
id: test.no-loc
description: Pod b
severity: warn
match:
  format: yaml
  filter: '.kind == \"Pod\"'
check:
  jq: 'select(.metadata.name == \"b\")'
  message: \"Pod b\"
",
    );
    scratch.write(
        "s2.yaml",
        "# head\n---\n# c\nkind: Pod\nmetadata: {name: a}\n---\n\n# lead\nkind: Pod\nmetadata: {name: b}\n",
    );
    assert_eq!(
        scratch.diagnostics(&["lint", "--rules", "no-loc.yml", "s2.yaml"]),
        (0, vec![diag("test.no-loc", 9, 1)])
    );
    // A file that is not a stream still reports at line 1.
    scratch.write("b.yaml", "\n# lead\nkind: Pod\nmetadata: {name: b}\n");
    assert_eq!(
        scratch.diagnostics(&["lint", "--rules", "no-loc.yml", "b.yaml"]),
        (0, vec![diag("test.no-loc", 1, 1)])
    );
}

/// A composite rule: each ConfigMap carries a JSON config inline. Document
/// 0 is clean, document 1 enables debug, document 2 does not parse.
const COMPOSITE_RULE: &str = r#"
id: test.inline-config
description: inline JSON config must not enable debug
severity: error
match:
  format: yaml
  filter: '.kind == "ConfigMap"'
check:
  extract: '[{value: .data.config, format: "json", anchor: "/data/config"}]'
  nested:
    id: test.inline-config.inner
    description: debug off
    severity: error
    match:
      format: json
    check:
      jq: 'select(.debug == true) | {pointer: "/debug"}'
      message: "debug is on"
    loc:
      pointer: '.pointer'
  message: "inline config"
"#;

const CONFIG_MAPS: &str = r#"kind: ConfigMap
data:
  config: '{"debug": false}'
---
kind: ConfigMap
data:
  config: '{"debug": true}'
---
kind: ConfigMap
data:
  config: '{"debug": '
"#;

#[test]
fn composite_rule_reports_inner_hits_and_parse_failures_in_their_document() {
    let scratch = Scratch::new();
    scratch.write("composite.yml", COMPOSITE_RULE);
    scratch.write("cm.yaml", CONFIG_MAPS);
    // The same text as document 1, in a file of its own: the column of the
    // inner hit must not depend on where the document sits.
    scratch.write(
        "cm1.yaml",
        "kind: ConfigMap\ndata:\n  config: '{\"debug\": true}'\n",
    );
    let (_, alone) = scratch.diagnostics(&["lint", "--rules", "composite.yml", "cm1.yaml"]);
    let [(_, 3, inner_col)] = alone.as_slice() else {
        panic!("one inner hit on line 3: {alone:?}");
    };

    assert_eq!(
        scratch.diagnostics(&["lint", "--rules", "composite.yml", "cm.yaml"]),
        (
            4,
            vec![
                diag("test.inline-config.inner", 7, *inner_col),
                diag("test.inline-config.parse-failed", 11, 11),
            ]
        )
    );
    // Document 0 alone is clean.
    assert_eq!(
        scratch.diagnostics(&["--doc", "0", "lint", "--rules", "composite.yml", "cm.yaml"]),
        (0, vec![])
    );
}

/// The diagnostics of [`RULE`] on [`STREAM`] — one per document, each at
/// its line in the file — through every reporter.
#[test]
fn reporters_place_stream_diagnostics_at_their_lines_in_the_file() {
    let scratch = Scratch::new();
    let lint = |format: &str| {
        let output = scratch
            .dq()
            .args(["-F", format, "lint", "--rules", "rule.yml", "s.yaml"])
            .output()
            .expect("spawn dq");
        assert_eq!(output.status.code(), Some(4), "{format}");
        String::from_utf8(output.stdout).expect("UTF-8")
    };

    let (_, found) = scratch.diagnostics(&["lint", "--rules", "rule.yml", "s.yaml"]);
    assert_eq!(
        found,
        vec![diag("test.pod-name", 3, 18), diag("test.pod-name", 7, 18)]
    );

    let sarif: serde_json::Value = serde_json::from_str(&lint("sarif")).expect("SARIF JSON");
    let regions: Vec<(u64, u64)> = sarif["runs"][0]["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| {
            let region = &r["locations"][0]["physicalLocation"]["region"];
            (
                region["startLine"].as_u64().expect("startLine"),
                region["startColumn"].as_u64().expect("startColumn"),
            )
        })
        .collect();
    assert_eq!(regions, vec![(3, 18), (7, 18)]);

    let junit = lint("junit");
    assert!(junit.contains(r#"tests="2" failures="2""#), "{junit}");
    assert!(
        junit.contains(">s.yaml:3:18\n") && junit.contains(">s.yaml:7:18\n"),
        "{junit}"
    );

    let tap = lint("tap");
    assert!(tap.contains("1..2\n"), "{tap}");
    let lines: Vec<&str> = tap
        .lines()
        .filter(|l| l.trim_start().starts_with("line: "))
        .map(str::trim)
        .collect();
    assert_eq!(lines, vec!["line: 3", "line: 7"], "{tap}");
}

/// A stream with comments, flow style, a document [`RULE`] does not
/// match, and a block-style Pod.
const COMMENTED: &str = "\
# leading comment
kind: Pod
metadata: {name: a}  # keep me
---
kind: Service
metadata: {name: svc}
---
# third
kind: Pod
metadata:
  name: b
  labels: {team: x}
";

#[test]
fn fix_in_place_with_fix_ops_keeps_comments_flow_style_and_every_separator() {
    let scratch = Scratch::new();
    scratch.write("c.yaml", COMMENTED);
    scratch
        .run(&["-i", "fix", "--rules", "rule.yml", "c.yaml"])
        .expect("fix -i");
    assert_eq!(
        scratch.read("c.yaml"),
        COMMENTED
            .replace("{name: a}", "{name: fixed}")
            .replace("name: b\n", "name: fixed\n")
    );
}

#[test]
fn fix_doc_fixes_that_document_alone() {
    let scratch = Scratch::new();
    scratch.write("c.yaml", COMMENTED);
    scratch
        .run(&["--doc", "2", "-i", "fix", "--rules", "rule.yml", "c.yaml"])
        .expect("fix -i");
    assert_eq!(
        scratch.read("c.yaml"),
        COMMENTED.replace("name: b\n", "name: fixed\n")
    );
    // Document 1 is a Service: nothing to fix there.
    scratch
        .run(&["--doc", "1", "-i", "fix", "--rules", "rule.yml", "c.yaml"])
        .expect("fix -i");
    assert_eq!(
        scratch.read("c.yaml"),
        COMMENTED.replace("name: b\n", "name: fixed\n")
    );
}

#[test]
fn fix_check_and_diff_see_every_document_of_a_stream() {
    let scratch = Scratch::new();
    scratch
        .dq()
        .args(["--check", "fix", "--rules", "rule.yml", "s.yaml"])
        .assert()
        .code(1)
        .stdout("would modify: s.yaml\n")
        .stderr("1 file(s) would be modified\n");
    assert_eq!(
        scratch.run(&["--diff", "fix", "--rules", "rule.yml", "s.yaml"]),
        Ok("\
--- a/s.yaml
+++ b/s.yaml
@@ -1,7 +1,7 @@
 apiVersion: v1
 kind: Pod
-metadata: {name: a}
+metadata: {name: fixed}
 ---
 apiVersion: v1
 kind: Pod
-metadata: {name: b}
+metadata: {name: fixed}
"
        .to_owned())
    );
    assert_eq!(
        scratch.read("s.yaml"),
        STREAM,
        "--check / --diff write nothing"
    );

    // A stream every document of which is clean has nothing pending.
    scratch.write(
        "clean.yaml",
        &STREAM
            .replace("{name: a}", "{name: fixed}")
            .replace("{name: b}", "{name: fixed}"),
    );
    assert_eq!(
        scratch.run(&["--check", "fix", "--rules", "rule.yml", "clean.yaml"]),
        Ok(String::new())
    );
}

/// A legacy `fix.jq` sees one document and its output replaces that
/// document alone; the file stays a stream of three.
#[test]
fn fix_jq_replaces_only_its_document() {
    let scratch = Scratch::new();
    scratch.write(
        "jq-rule.yml",
        "\
id: test.pod-jq-name
description: every Pod is named 'fixed'
severity: error
match:
  format: yaml
  filter: '.kind == \"Pod\"'
check:
  jq: 'select(.metadata.name != \"fixed\")'
  message: \"not fixed\"
fix:
  jq: '.metadata.name = \"fixed\"'
",
    );
    scratch.write("c.yaml", COMMENTED);
    scratch
        .run(&["-i", "fix", "--rules", "jq-rule.yml", "c.yaml"])
        .expect("fix -i");
    let documents = scratch
        .run(&["-F", "json", "convert", "c.yaml"])
        .expect("convert");
    let documents: serde_json::Value = serde_json::from_str(&documents).expect("JSON");
    assert_eq!(
        documents,
        serde_json::json!([
            {"kind": "Pod", "metadata": {"name": "fixed"}},
            {"kind": "Service", "metadata": {"name": "svc"}},
            {"kind": "Pod", "metadata": {"name": "fixed", "labels": {"team": "x"}}},
        ])
    );
}

/// A glob over streams and single-document files: each file is one entry
/// of the `Modified / Skipped / Failed` accounting, whatever it holds.
#[test]
fn bulk_fix_counts_a_stream_as_one_file() {
    let scratch = Scratch::new();
    scratch.write("bk/a-stream.yaml", STREAM);
    scratch.write("bk/b-clean.yaml", SINGLE);
    scratch.write("bk/c-dirty.yaml", &SINGLE.replace("fixed", "q"));
    scratch.write("bk/d-services.yaml", "kind: Service\n---\nkind: Service\n");
    assert_eq!(
        scratch.run(&["-i", "fix", "--rules", "rule.yml", "bk/*.yaml"]),
        Ok("Modified: 2, Skipped: 2, Failed: 0\n".to_owned())
    );
    assert_eq!(
        scratch.read("bk/a-stream.yaml"),
        STREAM
            .replace("{name: a}", "{name: fixed}")
            .replace("{name: b}", "{name: fixed}")
    );
    assert_eq!(scratch.read("bk/c-dirty.yaml"), SINGLE);
    assert_eq!(
        scratch.read("bk/d-services.yaml"),
        "kind: Service\n---\nkind: Service\n"
    );
}

/// `--doc 1` over a glob: the streams are edited; the single-document
/// file has no document 1, so it is skipped with a note on stderr and
/// left untouched, and the run goes on (exit 0) — no
/// `--continue-on-error` needed.
#[test]
fn bulk_set_doc_skips_the_file_without_that_document() {
    let scratch = Scratch::new();
    scratch.write("bk/a.yaml", STREAM);
    scratch.write("bk/b.yaml", SINGLE);
    scratch.write("bk/c.yaml", STREAM);
    let output = scratch
        .dq()
        .args([
            "-i",
            "--doc",
            "1",
            "set",
            "bk/*.yaml",
            "/metadata/name",
            "z",
        ])
        .output()
        .expect("spawn dq");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8");
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    assert_eq!(stdout, "Modified: 2, Skipped: 1, Failed: 0\n");
    assert!(
        stderr
            .contains("skipped bk/b.yaml: --doc 1 is out of range: bk/b.yaml holds 1 document(s)"),
        "{stderr}"
    );
    let edited = STREAM.replace("{name: b}", "{name: z}");
    assert_eq!(scratch.read("bk/a.yaml"), edited);
    assert_eq!(scratch.read("bk/b.yaml"), SINGLE);
    assert_eq!(scratch.read("bk/c.yaml"), edited);
}

/// `--doc` reads stdin (`-` with `-F yaml`) the way it reads a file.
#[test]
fn doc_on_stdin() {
    let scratch = Scratch::new();
    let stdin = |args: &[&str]| scratch.run_with_stdin(args, STREAM);
    assert_eq!(
        stdin(&["--doc", "1", "-F", "yaml", "get", "-", "/metadata/name"]),
        Ok("b\n".to_owned())
    );
    assert_eq!(
        stdin(&["--doc", "all", "-F", "yaml", "get", "-", "/1/kind"]),
        Ok("Pod\n".to_owned())
    );
    assert_eq!(
        stdin(&["--doc", "3", "-F", "yaml", "get", "-", "/kind"]),
        Err((
            2,
            "--doc 3 is out of range: - holds 2 document(s) (--doc 0 to 1)\n".to_owned()
        ))
    );
    assert_eq!(
        stdin(&["--doc", "1", "-F", "yaml", "paths", "-"]),
        Ok("'': object\n/apiVersion: string\n/kind: string\n/metadata: object\n/metadata/name: string\n"
            .to_owned())
    );
    assert_eq!(
        stdin(&[
            "--doc",
            "1",
            "-F",
            "yaml",
            "set",
            "-",
            "/metadata/name",
            "z"
        ]),
        Ok(STREAM.replace("{name: b}", "{name: z}"))
    );
}

/// Files that are not YAML streams lint exactly as they did before
/// streams were linted per document — the diagnostics below were taken
/// from the build before that change.
#[test]
fn front_matter_json_and_single_document_yaml_lint_as_before() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let lint = |file: &str| {
        let output = assert_cmd::Command::cargo_bin("dq")
            .expect("dq binary built")
            .env_remove("RUST_LOG")
            .current_dir(&fixtures)
            .args(["--no-color", "-F", "json", "lint", file])
            .output()
            .expect("spawn dq");
        let body: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
        let found: Vec<(String, u64, u64)> = body["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|d| {
                (
                    d["rule_id"].as_str().expect("rule_id").to_owned(),
                    d["line"].as_u64().expect("line"),
                    d["col"].as_u64().expect("col"),
                )
            })
            .collect();
        (output.status.code().expect("exit code"), found)
    };
    let front_matter = (4, vec![diag("markdown.frontmatter-required-fields", 1, 1)]);
    assert_eq!(lint("hugo_post.md"), front_matter);
    assert_eq!(lint("json_frontmatter_post.md"), front_matter);
    assert_eq!(
        lint("package.json"),
        (
            0,
            vec![
                diag("npm.has-engines", 1, 1),
                diag("npm.has-license", 1, 1),
                diag("npm.has-repository", 1, 1),
            ]
        )
    );
    let mut k8s = Vec::new();
    for rule in [
        "k8s.allow-privilege-escalation",
        "k8s.missing-liveness-probe",
        "k8s.missing-readiness-probe",
        "k8s.missing-resources-limits",
    ] {
        for line in [22, 26, 28] {
            k8s.push(diag(rule, line, 17));
        }
    }
    k8s.push(diag("k8s.run-as-root", 22, 17));
    assert_eq!(lint("k8s_deployment.yaml"), (0, k8s));
}

/// The reference plugin (`examples/plugin-rust`) reports one
/// position-less diagnostic per document it is handed. A stream hands it
/// each document: one diagnostic per document, at the document's start.
///
/// Needs the prebuilt fixture `crates/dq-plugin/tests/fixtures/
/// example_noop.wasm` (not committed; see the README there). Without it
/// the test skips with a notice, like the `dq-plugin` integration tests.
#[cfg(feature = "plugins")]
#[test]
fn a_plugin_lints_every_document_of_a_stream() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dq-plugin/tests/fixtures/example_noop.wasm");
    if !fixture.exists() {
        eprintln!("SKIPPING: {} not built", fixture.display());
        return;
    }
    let scratch = Scratch::new();
    std::fs::create_dir(scratch.dir.path().join("plugins")).expect("mkdir");
    std::fs::copy(&fixture, scratch.dir.path().join("plugins/example.wasm")).expect("copy");
    let (code, found) = scratch.diagnostics(&[
        "--plugins",
        "plugins",
        "lint",
        "--rules",
        "rule.yml",
        "s.yaml",
    ]);
    assert_eq!(code, 4, "{found:?}");
    let plugin: Vec<_> = found
        .into_iter()
        .filter(|(id, _, _)| id == "example.demo-lint")
        .collect();
    assert_eq!(
        plugin,
        vec![
            diag("example.demo-lint", 1, 1),
            diag("example.demo-lint", 5, 1)
        ]
    );
}

/// Fires on every document without a `name` key — an empty document
/// (`null`) included.
const NO_NAME_RULE: &str = r#"
id: test.no-name
description: every document has a name
severity: warn
match:
  format: yaml
check:
  jq: 'select(has("name") | not)'
  message: "no name"
"#;

/// An empty document reports at its own `---` line (line 1 for an implicit
/// first document) — never past the end of the file, never on the
/// separator of the document after it.
#[test]
fn an_empty_document_reports_at_its_own_marker_line() {
    let scratch = Scratch::new();
    scratch.write("no-name.yml", NO_NAME_RULE);
    let cases: [(&str, &[u64]); 4] = [
        ("name: a\n---\n", &[2]),
        ("---\n---\n", &[1, 2]),
        ("name: a\n--- # c\n# tail\n", &[2]),
        ("name: a\n---\n---\nname: c\n", &[2]),
    ];
    for (source, lines) in cases {
        scratch.write("e.yaml", source);
        let (code, found) = scratch.diagnostics(&["lint", "--rules", "no-name.yml", "e.yaml"]);
        assert_eq!(code, 0, "{source:?}");
        let expected: Vec<_> = lines
            .iter()
            .map(|&line| diag("test.no-name", line, 1))
            .collect();
        assert_eq!(found, expected, "{source:?}");
    }
}

/// A `fix.jq` in one document re-emits that document only: every other
/// document keeps its bytes — `---` lines and comments included — and a
/// `fix.ops` in a later document still applies in the same run.
#[test]
fn fix_jq_in_one_document_keeps_every_other_byte_and_later_ops_apply() {
    let scratch = Scratch::new();
    scratch.write(
        "jq.yml",
        r#"
id: test.jq-name
description: documents with x get a name
severity: warn
match: {format: yaml}
check:
  jq: 'select(type == "object" and has("x") and .name != "fixed")'
  message: no name
fix:
  jq: '.name = "fixed"'
"#,
    );
    scratch.write(
        "ops.yml",
        r#"
id: test.ops-name
description: z becomes zz
severity: warn
match: {format: yaml}
check:
  jq: 'select(type == "object" and .name == "z")'
  message: z
fix:
  ops: 'if .name == "z" then [{"op":"replace","path":"/name","value":"zz"}] else [] end'
"#,
    );
    scratch.write(
        "m.yaml",
        "# head\nname: a # keep\n--- # one\nx: 1 # c\n---\n- 1\n---\nname: z # tail\n",
    );
    scratch
        .run(&[
            "-i", "fix", "--rules", "jq.yml", "--rules", "ops.yml", "m.yaml",
        ])
        .expect("fix -i");
    assert_eq!(
        scratch.read("m.yaml"),
        "# head\nname: a # keep\n--- # one\nx: 1\nname: fixed\n---\n- 1\n---\nname: zz # tail\n"
    );
}

/// A `fix.ops` script that replaces or removes the whole document is the
/// same error in a single file and in a stream: exit 3, naming the rule
/// and the document, the file untouched.
#[test]
fn fix_ops_on_a_whole_document_is_one_error_in_a_file_and_in_a_stream() {
    let scratch = Scratch::new();
    for (op, value) in [("replace", r#","value":{"ok":true}"#), ("remove", "")] {
        scratch.write(
            "whole.yml",
            &format!(
                r#"
id: test.whole
description: whole-document fix
severity: warn
match: {{format: yaml}}
check:
  jq: 'select(.bad == true)'
  message: bad
fix:
  ops: '[{{"op":"{op}","path":""{value}}}]'
"#
            ),
        );
        for (file, source, document) in [
            ("single.yaml", "bad: true\n", "the document"),
            (
                "stream.yaml",
                "ok: 1\n---\nbad: true\n",
                "document 1 of the stream",
            ),
        ] {
            scratch.write(file, source);
            let (code, stderr) = scratch
                .run(&["-i", "fix", "--rules", "whole.yml", file])
                .expect_err("a whole-document fix.ops is refused");
            assert_eq!(code, 3, "{op} {file}: {stderr}");
            assert!(
                stderr.starts_with(&format!(
                    "rule test.whole fix failed: fix.ops would {op} {document} as a whole"
                )),
                "{op} {file}: {stderr}"
            );
            assert_eq!(scratch.read(file), source, "{op} {file}: untouched");
        }
    }
}

/// `--doc N` over several files: lint and fix skip a file without
/// document N with a note on stderr and go on with the rest.
#[test]
fn bulk_lint_and_fix_doc_skip_the_file_without_that_document() {
    let scratch = Scratch::new();
    scratch.write("bk/a.yaml", STREAM);
    scratch.write("bk/b.yaml", SINGLE);
    scratch.write("bk/c.yaml", STREAM);

    let output = scratch
        .dq()
        .args(["-F", "json", "--doc", "1", "lint", "--rules", "rule.yml"])
        .args(["bk/a.yaml", "bk/b.yaml", "bk/c.yaml"])
        .output()
        .expect("spawn dq");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8");
    assert_eq!(output.status.code(), Some(4), "{stderr}");
    assert!(
        stderr.contains("skipped bk/b.yaml: --doc 1 is out of range"),
        "{stderr}"
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    let paths: Vec<_> = body["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| d["path"].as_str().expect("path").to_owned())
        .collect();
    assert_eq!(paths, vec!["bk/a.yaml", "bk/c.yaml"]);

    let output = scratch
        .dq()
        .args(["-i", "--doc", "1", "fix", "--rules", "rule.yml"])
        .args(["bk/a.yaml", "bk/b.yaml", "bk/c.yaml"])
        .output()
        .expect("spawn dq");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8");
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8"),
        "Modified: 2, Skipped: 1, Failed: 0\n"
    );
    let fixed = STREAM.replace("{name: b}", "{name: fixed}");
    assert_eq!(scratch.read("bk/a.yaml"), fixed);
    assert_eq!(scratch.read("bk/b.yaml"), SINGLE);
    assert_eq!(scratch.read("bk/c.yaml"), fixed);

    // One file lacking document 1 stays an exit-2 error.
    let (code, _) = scratch
        .run(&["--doc", "1", "lint", "--rules", "rule.yml", "bk/b.yaml"])
        .unwrap_err();
    assert_eq!(code, 2);
}

/// `set --jq` returning an empty array for a whole stream would write a
/// file with no document: refused (exit 6), the file untouched. With
/// `--doc N` an empty array is that document's value.
#[test]
fn set_jq_empty_array_over_a_stream_is_refused() {
    let scratch = Scratch::new();
    let (code, stderr) = scratch
        .run(&["-i", "set", "s.yaml", "--jq", "[]"])
        .unwrap_err();
    assert_eq!(code, 6);
    assert!(stderr.contains("--jq returned an empty array"), "{stderr}");
    assert_eq!(scratch.read("s.yaml"), STREAM);

    let out = scratch
        .run(&["--doc", "0", "set", "s.yaml", "--jq", "[]"])
        .expect("a per-document value");
    assert!(out.starts_with("[]\n---\n"), "{out}");
}
