//! The template guard on templates whose text the parser reads otherwise
//! than it is written, and on files whose reading carries positions.
//!
//! The guard compared the file read as it is with the file read with each
//! template block swapped for a placeholder, the block's text put back in
//! place of each placeholder. A quoted string whose template holds an
//! escape — `''` in a YAML single-quoted string, `\"` in a YAML, JSON or
//! TOML double-quoted one — reads without the escape, so the two never
//! matched, and a valid GitHub Actions workflow was refused (exit 3) by
//! every command. Markdown keeps a line and a column in every node, and a
//! placeholder of another length moved them: any Markdown file with a
//! `{{ … }}` in its text was refused. The guard now matches each
//! placeholder with one template block of the string as read, and leaves
//! the Markdown positions out of the comparison.
//!
//! The write side had the same blind spot: a textual edit that targeted a
//! template with an escape was refused, because the block was looked up by
//! the text as read. It now tells blocks apart by where they stand, and a
//! comment holding a template goes with the node a `del` removes. `patch`
//! carries each op's path back through the index shifts of the ops before
//! it, and a refusal names the line of the template it would lose.

use std::path::Path;

use assert_cmd::Command;

fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

fn write(dir: &Path, name: &str, contents: &str) -> String {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    std::fs::write(&path, contents).expect("write fixture");
    path.to_str().expect("UTF-8 path").to_owned()
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).expect("read back")
}

fn stdout_of(args: &[&str]) -> String {
    let out = dq().arg("--no-color").args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).expect("UTF-8 stdout")
}

/// A GitHub Actions workflow with a template in a single-quoted string
/// (`''` inside it) and one in a double-quoted string (`\"` inside it).
const WORKFLOW: &str = r#"on: push
jobs:
  build:
    if: '${{ github.ref == ''refs/heads/main'' }}'
    env:
      HAS_Y: "${{ contains(x, \"y\") }}"
    runs-on: ubuntu-latest
"#;

const JSON: &str = r#"{"a": "${{ contains(x, \"y\") }}", "n": 1}
"#;

const TOML: &str = r#"a = "${{ contains(x, \"y\") }}"
b = '${{ format("{0}", x) }}'
n = 1
"#;

/// Every reading command passes the file and reads the templates as the
/// parser reads them, escapes taken out.
#[test]
fn reading_commands_pass_templates_with_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = write(dir.path(), "ci.yml", WORKFLOW);
    let json = write(dir.path(), "a.json", JSON);
    let toml = write(dir.path(), "a.toml", TOML);
    assert_eq!(
        stdout_of(&["get", &yaml, "/jobs/build/if"]),
        "${{ github.ref == 'refs/heads/main' }}\n"
    );
    assert_eq!(
        stdout_of(&["get", &yaml, "/jobs/build/env/HAS_Y"]),
        "${{ contains(x, \"y\") }}\n"
    );
    assert_eq!(
        stdout_of(&["get", &json, "/a"]),
        "${{ contains(x, \"y\") }}\n"
    );
    assert_eq!(
        stdout_of(&["get", &toml, "/b"]),
        "${{ format(\"{0}\", x) }}\n"
    );
    for file in [&yaml, &json, &toml] {
        dq().args(["--no-color", "lint", file.as_str()])
            .assert()
            .success();
    }
}

/// A point edit elsewhere keeps every template byte for byte; one that
/// targets a template with an escape replaces it.
#[test]
fn set_edits_files_with_escaped_templates() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = write(dir.path(), "ci.yml", WORKFLOW);
    stdout_of(&["set", &yaml, "/jobs/build/runs-on", "ubuntu-24.04", "-i"]);
    assert_eq!(
        read(&yaml),
        WORKFLOW.replace("ubuntu-latest", "ubuntu-24.04")
    );
    stdout_of(&["set", &yaml, "/jobs/build/if", "always()", "-i"]);
    assert_eq!(
        read(&yaml),
        WORKFLOW
            .replace("ubuntu-latest", "ubuntu-24.04")
            .replace("'${{ github.ref == ''refs/heads/main'' }}'", "'always()'")
    );
    stdout_of(&["set", &yaml, "/jobs/build/env/HAS_Y", "no", "-i"]);
    assert!(!read(&yaml).contains("contains(x"), "{}", read(&yaml));

    let json = write(dir.path(), "a.json", JSON);
    stdout_of(&["set", &json, "/n", "2", "-i"]);
    assert_eq!(read(&json), JSON.replace("\"n\": 1", "\"n\": 2"));
    stdout_of(&["set", &json, "/a", "2", "-i"]);
    assert_eq!(read(&json), "{\"a\": 2, \"n\": 2}\n");

    let toml = write(dir.path(), "a.toml", TOML);
    stdout_of(&["set", &toml, "/n", "2", "-i"]);
    assert_eq!(read(&toml), TOML.replace("n = 1", "n = 2"));
    stdout_of(&["set", &toml, "/a", "2", "-i"]);
    stdout_of(&["set", &toml, "/b", "3", "-i"]);
    assert_eq!(read(&toml), "a = 2\nb = 3\nn = 2\n");
}

/// `fmt` and `fix` refuse both escape hatches; the guard now lets the
/// file through, so they run. The rewrite keeps each template's data.
#[test]
fn fmt_and_fix_run_on_escaped_templates() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = write(dir.path(), "ci.yml", WORKFLOW);
    let json = write(dir.path(), "a.json", JSON);
    let toml = write(dir.path(), "a.toml", TOML);
    for file in [&yaml, &json, &toml] {
        stdout_of(&["fmt", file, "-i"]);
        stdout_of(&["fix", file]);
    }
    assert_eq!(
        stdout_of(&["get", &yaml, "/jobs/build/if"]),
        "${{ github.ref == 'refs/heads/main' }}\n"
    );
    assert_eq!(
        stdout_of(&["get", &json, "/a"]),
        "${{ contains(x, \"y\") }}\n"
    );
}

/// A template whose escape-free text would change the reading is still
/// refused: the comparison is per block, not a blanket pass.
#[test]
fn a_helm_template_beside_an_escaped_one_is_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "v.yaml",
        "a: '${{ f(''x'') }}'\nport: {{ .Values.port }}\n",
    );
    dq().args(["--no-color", "get", &file, "/a"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("Go template syntax"));
}

/// A skill's `SKILL.md` and `evals.json` mention `{{ }}` in prose, in code
/// spans, in fenced code and inside JSON strings with escaped quotes: the
/// templates sit in text, so `lint` reads both files.
#[test]
fn a_skill_with_templates_in_its_text_lints() {
    let dir = tempfile::tempdir().unwrap();
    let skill = write(
        dir.path(),
        "SKILL.md",
        "---\n\
         name: helm-edit\n\
         description: Edit Helm values and workflows without breaking `{{ }}` blocks.\n\
         ---\n\
         \n\
         # Helm edit\n\
         \n\
         Use `--raw-template-strings` for `{{ .Values.x }}`; a workflow's ${{ github.ref }} is text.\n\
         \n\
         __Note__: keep {{ .Release.Name }} as it is.\n\
         \n\
         ```yaml\n\
         image:\n  tag: {{ .Values.image.tag }}\n\
         ```\n",
    );
    let evals = write(
        dir.path(),
        "evals/evals.json",
        r#"{
  "skill_name": "helm-edit",
  "evals": [
    {
      "id": 1,
      "prompt": "values.yaml has '{{ .Values.image.repository }}:{{ .Values.image.tag }}' and repo: \"{{ .Values.image.repo | default \\\"nginx\\\" }}\"",
      "expected_output": "Uses --raw-template-strings; keeps {{ ... }} blocks.",
      "assertions": [{"text": "keeps {{ .Values.image.tag }}", "type": "output"}]
    }
  ]
}
"#,
    );
    dq().args([
        "--no-color",
        "lint",
        "--rules",
        "@std/skills",
        &skill,
        &evals,
    ])
    .assert()
    .success();
    assert_eq!(
        stdout_of(&["get", &skill, "/frontmatter/value/name"]),
        "helm-edit\n"
    );
}

/// A template that changes what the Markdown frontmatter's YAML reads is
/// refused, as in a YAML file.
#[test]
fn a_template_that_changes_the_frontmatter_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "SKILL.md",
        "---\nname: {{ .n }}\ndescription: d\n---\n# x\n",
    );
    dq().args(["--no-color", "get", &file, "/frontmatter"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("line 2"));
}

/// `del` of a key whose trailing comment holds a template removes the
/// comment with it; so does `del` of a mapping whose key line holds one.
#[test]
fn del_removes_the_templates_in_the_comments_of_its_node() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = write(dir.path(), "fl.yml", "k: 1 # ${{ c }}\nj: 2 # ${{ d }}\n");
    stdout_of(&["del", &yaml, "/k", "-i"]);
    assert_eq!(read(&yaml), "j: 2 # ${{ d }}\n");

    let toml = write(dir.path(), "fl.toml", "k = 1 # ${{ c }}\nj = 2\n");
    stdout_of(&["del", &toml, "/k", "-i"]);
    assert_eq!(read(&toml), "j = 2\n");

    let nested = write(
        dir.path(),
        "kl.yml",
        "x: ${{ m }}\na: # ${{ m }}\n  b: 1\nc: 2\n",
    );
    stdout_of(&["del", &nested, "/a", "-i"]);
    assert_eq!(read(&nested), "x: ${{ m }}\nc: 2\n");
}

const STEPS: &str = "\
name: ci
on: push
jobs:
  build:
    strategy: {matrix: {os: [a, b]}}
    runs-on: ${{ matrix.os }}
    steps:
      - name: checkout
        uses: actions/checkout@v4
      - name: setup
        run: echo hi
      - name: ${{ matrix.os }} build
        run: make ${{ matrix.os }}
";

/// After an `add` inserts an item, a later op's index names the shifted
/// item: replacing the name of what was `steps/2` (now `steps/3`) is the
/// edit asked for, not a template lost.
#[test]
fn patch_sequences_its_ops_like_rfc_6902() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(dir.path(), "ci.yml", STEPS);
    stdout_of(&[
        "patch",
        &file,
        r#"[{"op":"add","path":"/jobs/build/steps/0","value":{"name":"first"}},
            {"op":"replace","path":"/jobs/build/steps/3/name","value":"x"}]"#,
        "-i",
    ]);
    let written = read(&file);
    assert!(
        written.contains("      - name: x\n        run: make ${{ matrix.os }}\n"),
        "{written}"
    );
    assert!(written.contains("runs-on: ${{ matrix.os }}\n"), "{written}");

    // `setup`, at `steps/1`, is at `steps/2` once an item is inserted
    // before it: removing it there keeps every template.
    let file = write(dir.path(), "ci2.yml", STEPS);
    stdout_of(&[
        "patch",
        &file,
        r#"[{"op":"add","path":"/jobs/build/steps/1","value":{"name":"x"}},
            {"op":"remove","path":"/jobs/build/steps/2"}]"#,
        "-i",
    ]);
    let written = read(&file);
    assert!(!written.contains("echo hi"), "{written}");
    assert!(written.contains("${{ matrix.os }} build"), "{written}");
}
