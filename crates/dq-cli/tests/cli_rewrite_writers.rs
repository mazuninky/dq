//! What the writers of the formats dq rewrites whole — XML, HCL, INI,
//! dotenv, CSV/TSV and JSON Lines — keep of a file through a pointer edit
//! (`set`, `del`, `patch`, `merge`) or a `set --jq`: the rewrite drops
//! comments and layout, but must not change what the file means.

use assert_cmd::Command;
use pretty_assertions::assert_eq;
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

/// What one run did: exit code, stdout, stderr and the file afterwards.
#[derive(Debug, PartialEq)]
struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
    file: String,
}

/// Run `dq <args>` in a fresh directory holding `name` with `src`.
fn run(name: &str, src: &str, args: &[&str]) -> Outcome {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, src).expect("seed");
    let out = dq()
        .current_dir(dir.path())
        .args(args)
        .output()
        .expect("run dq");
    Outcome {
        code: out.status.code().expect("exit code"),
        stdout: String::from_utf8(out.stdout).expect("utf-8 stdout"),
        stderr: String::from_utf8(out.stderr).expect("utf-8 stderr"),
        file: std::fs::read_to_string(&path).expect("read back"),
    }
}

/// `dq -i <args>` on `name` must succeed silently and leave `want`.
fn edits_to(name: &str, src: &str, args: &[&str], want: &str) {
    let mut argv = vec!["-i"];
    argv.extend_from_slice(args);
    assert_eq!(
        run(name, src, &argv),
        Outcome {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
            file: want.to_owned(),
        },
        "{name}: {args:?}"
    );
}

/// `dq -i <args>` on `name` must fail with `code` and `stderr`, leaving the
/// file as it was.
fn refused(name: &str, src: &str, args: &[&str], code: i32, stderr: &str) {
    let mut argv = vec!["-i"];
    argv.extend_from_slice(args);
    assert_eq!(
        run(name, src, &argv),
        Outcome {
            code,
            stdout: String::new(),
            stderr: stderr.to_owned(),
            file: src.to_owned(),
        },
        "{name}: {args:?}"
    );
}

// ---------------------------------------------------------------------------
// HCL expressions
// ---------------------------------------------------------------------------

/// A Terraform file with every kind of expression a value cannot spell.
const TF_EXPRESSIONS: &str = r#"variable "ami" {
  default = "a"
}
resource "aws_instance" "web" {
  ami   = var.ami
  name  = "${var.p}-web"
  count = length(var.l)
  tags = {
    Name = "x-${var.p}"
    L    = [var.a, 1]
  }
  user_data = <<EOT
hello ${var.x}
EOT
  esc  = "$${lit}"
  c    = var.on ? 1 : 2
  f    = [for s in var.l : upper(s)]
  m    = { for k, v in var.m : k => v }
  d    = "%{ if var.on }on%{ endif }"
  n    = -var.n
  p    = (var.a + 1)
  s    = var.l[*].id
  keys = { (var.k) = 1 }
}
"#;

/// [`TF_EXPRESSIONS`] as the HCL writer lays it out, with
/// `variable.ami.default` set to `default`: every expression is its source.
fn tf_expressions_written(default: &str) -> String {
    format!(
        r#"variable "ami" {{
  default = {default}
}}
resource "aws_instance" "web" {{
  ami = var.ami
  name = "${{var.p}}-web"
  count = length(var.l)
  tags = {{
    "Name" = "x-${{var.p}}"
    "L" = [
      var.a,
      1
    ]
  }}
  user_data = <<EOT
hello ${{var.x}}
EOT
  esc = "$${{lit}}"
  c = var.on ? 1 : 2
  f = [for s in var.l : upper(s)]
  m = {{ for k, v in var.m : k => v }}
  d = "%{{ if var.on }}on%{{ endif }}"
  n = -var.n
  p = (var.a + 1)
  s = var.l[*].id
  keys = {{ (var.k) = 1 }}
}}
"#
    )
}

#[test]
fn hcl_point_edits_write_every_expression_back_as_its_source() {
    // Was: `ami = "$${var.ami}"`, `count = "$${length(var.l)}"`, … — every
    // expression became a string literal, exit 0.
    edits_to(
        "main.tf",
        TF_EXPRESSIONS,
        &["set", "main.tf", "/variable/ami/default", "x"],
        &tf_expressions_written("\"x\""),
    );
    // A string the user sets stays a string, `${` escaped.
    edits_to(
        "main.tf",
        TF_EXPRESSIONS,
        &["set", "main.tf", "/variable/ami/default", "${x}"],
        &tf_expressions_written("\"$${x}\""),
    );
    // Replacing an expression whole writes the value set.
    let want = tf_expressions_written("\"a\"").replace("  ami = var.ami\n", "  ami = \"ami-1\"\n");
    edits_to(
        "main.tf",
        TF_EXPRESSIONS,
        &["set", "main.tf", "/resource/aws_instance/web/ami", "ami-1"],
        &want,
    );
    // Deleting, patching and merging keep them too.
    let want = tf_expressions_written("\"a\"").replace("  ami = var.ami\n", "");
    edits_to(
        "main.tf",
        TF_EXPRESSIONS,
        &["del", "main.tf", "/resource/aws_instance/web/ami"],
        &want,
    );
    edits_to(
        "main.tf",
        TF_EXPRESSIONS,
        &[
            "patch",
            "main.tf",
            r#"[{"op": "replace", "path": "/variable/ami/default", "value": "x"}]"#,
        ],
        &tf_expressions_written("\"x\""),
    );
    edits_to(
        "main.tf",
        TF_EXPRESSIONS,
        &[
            "merge",
            "main.tf",
            r#"{"variable": {"ami": {"default": "x"}}}"#,
        ],
        &tf_expressions_written("\"x\""),
    );
}

#[test]
fn hcl_items_after_one_deleted_or_inserted_keep_their_expressions() {
    let src = "l = [1, var.a, \"${var.b}-x\", <<EOT\nh ${var.c}\nEOT\n]\n";
    edits_to(
        "a.tf",
        src,
        &["del", "a.tf", "/l/0"],
        "l = [\n  var.a,\n  \"${var.b}-x\",\n  <<EOT\nh ${var.c}\nEOT\n]\n",
    );
    edits_to(
        "a.tf",
        src,
        &[
            "patch",
            "a.tf",
            r#"[{"op": "add", "path": "/l/1", "value": 0}]"#,
        ],
        // A heredoc's closing marker stays on a line of its own.
        "l = [\n  1,\n  0,\n  var.a,\n  \"${var.b}-x\",\n  <<EOT\nh ${var.c}\nEOT\n]\n",
    );
    edits_to(
        "a.tf",
        "l = [<<EOT\nh\nEOT\n, var.a]\n",
        &["set", "a.tf", "/x", "1"],
        "l = [\n  <<EOT\nh\nEOT\n  ,\n  var.a\n]\nx = 1\n",
    );
}

#[test]
fn hcl_jq_rewrites_write_expressions_back_where_they_moved() {
    // `set --jq` did the same before point edits rewrote HCL.
    edits_to(
        "main.tf",
        TF_EXPRESSIONS,
        &["set", "main.tf", "--jq", ".variable.ami.default = \"x\""],
        &tf_expressions_written("\"x\""),
    );
    edits_to(
        "a.tf",
        "l = [1, var.a, length(var.b)]\n",
        &["set", "a.tf", "--jq", ".l |= reverse | .m = .l[0]"],
        "l = [\n  length(var.b),\n  var.a,\n  1\n]\nm = length(var.b)\n",
    );
}

#[test]
fn hcl_an_edit_no_writer_can_keep_an_expression_through_is_refused() {
    let src = "t = merge(var.a, { k = 1 })\nn = \"${var.p}-web\"\n";
    // Into an expression: its value is HCL's to compute.
    refused(
        "a.tf",
        src,
        &["set", "a.tf", "/t/k", "2"],
        1,
        "hcl format error: cannot set '/t/k': '/t' is the expression `merge(var.a, { k = 1 })`, \
         whose value HCL computes; dq can replace the whole expression, not edit inside it\n",
    );
    refused(
        "a.tf",
        src,
        &["del", "a.tf", "/t/k"],
        1,
        "hcl format error: cannot delete '/t/k': '/t' is the expression `merge(var.a, { k = 1 })`, \
         whose value HCL computes; dq can replace the whole expression, not edit inside it\n",
    );
    // A filter that edits the string a template reads as.
    refused(
        "a.tf",
        src,
        &["set", "a.tf", "--jq", ".n |= sub(\"web\"; \"api\")"],
        1,
        "hcl format error: '/n': the edit turned the expression `\"${var.p}-web\"` into the string \
         \"${var.p}-api\"; dq would write that as a string, no longer an expression, so it \
         refuses: edit the expression in the file itself, or set a value that holds no `${` or \
         `%{`\n",
    );
    // Two expressions of different source read as one moved string.
    refused(
        "a.tf",
        "l = [1, var.a, \"${var.a}\"]\n",
        &["set", "a.tf", "--jq", ".l |= .[1:]"],
        1,
        "hcl format error: '/l/0': the string \"${var.a}\" is what more than one expression of \
         the file reads as, and the edit moved it; dq cannot tell which expression to write \
         there\n",
    );
}

#[test]
fn hcl_an_object_with_expression_keys_keeps_them_through_a_jq_edit() {
    // Was: `"(var.k)" = 1`, a literal key, exit 0.
    edits_to(
        "a.tf",
        "tags = { (var.k) = 1, b = 2 }\n",
        &["set", "a.tf", "--jq", ".tags.b = 3"],
        "tags = {\n  (var.k) = 1\n  \"b\" = 3\n}\n",
    );
    // A member read from an expression keeps it too, and an object moved
    // whole keeps its one expression key.
    edits_to(
        "a.tf",
        "tags = { (var.k) = var.v, b = 2 }\n",
        &["set", "a.tf", "--jq", ".t = .tags | .tags.b = 3"],
        "tags = {\n  (var.k) = var.v\n  \"b\" = 3\n}\nt = {\n  (var.k) = var.v\n  \"b\" = 2\n}\n",
    );
    // Two keys of different source that read as one string: dq cannot tell
    // which to write where the filter moved it.
    refused(
        "a.tf",
        "a = { (var.k) = 1 }\nb = { (var.k ) = 2 }\n",
        &["set", "a.tf", "--jq", ".c = .a | .a.x = 1"],
        1,
        "hcl format error: '/c': the key \"(var.k)\" is what more than one expression key of \
         the file reads as, and the edit moved it; dq cannot tell which expression to write \
         there\n",
    );
}

#[test]
fn hcl_a_string_set_over_an_expression_that_reads_as_it_is_written_as_a_string() {
    // Was: the file left as it was (`x = var.a`), exit 0: the string set
    // reads as what the expression reads as.
    for args in [
        &["set", "s.tf", "/x", "${var.a}"][..],
        &["set", "s.tf", "/x", "--value-string", "${var.a}"],
        &[
            "patch",
            "s.tf",
            r#"[{"op": "replace", "path": "/x", "value": "${var.a}"}]"#,
        ],
    ] {
        edits_to("s.tf", "x = var.a\n", args, "x = \"$${var.a}\"\n");
    }
}

#[test]
fn hcl_a_moved_string_two_sources_read_as_is_refused_even_at_its_own_path() {
    let src = "a = \"$${var.x}\"\nb = var.x\n";
    // Was: `a = "$${var.x}"`, exit 0 — the expression moved into `a` came
    // out as the literal read there.
    refused(
        "m.tf",
        src,
        &["set", "m.tf", "--jq", ".a = .b | .b = \"z\""],
        1,
        "hcl format error: '/a': the string \"${var.x}\" is what more than one expression of \
         the file reads as, and the edit moved it; dq cannot tell which expression to write \
         there\n",
    );
    // Nothing moved: each stays what it was.
    edits_to(
        "m.tf",
        src,
        &["set", "m.tf", "--jq", ".c = 1"],
        "a = \"$${var.x}\"\nb = var.x\nc = 1\n",
    );
}

#[test]
fn hcl_a_patch_on_repeated_blocks_sees_every_op_on_the_tree_before_it() {
    let src = "ingress {\n  p = 1\n}\ningress {\n  p = 2\n}\ningress {\n  p = 3\n}\n";
    // Was: exit 2, "path '/ingress/0' not found": each op re-read the file,
    // and the one block left read back as a map.
    edits_to(
        "r.tf",
        src,
        &[
            "patch",
            "r.tf",
            r#"[{"op":"remove","path":"/ingress/2"},{"op":"remove","path":"/ingress/1"},
                {"op":"replace","path":"/ingress/0/p","value":9}]"#,
        ],
        "ingress {\n  p = 9\n}\n",
    );
    edits_to(
        "r.tf",
        "ingress {\n  p = 1\n}\ningress {\n  p = 2\n}\nx = 1\n",
        &[
            "patch",
            "r.tf",
            r#"[{"op":"remove","path":"/ingress/1"},{"op":"remove","path":"/ingress/0"}]"#,
        ],
        "x = 1\n",
    );
    // A failing op leaves the file as it was, the ops before it too.
    refused(
        "r.tf",
        src,
        &[
            "patch",
            "r.tf",
            r#"[{"op":"remove","path":"/ingress/2"},{"op":"remove","path":"/ingress/5"}]"#,
        ],
        2,
        "path '/ingress/5' not found (matched up to '/ingress')\n  matched prefix: /ingress\n  \
         pointer: /ingress/5\n",
    );
}

// ---------------------------------------------------------------------------
// XML: what stands outside the root keeps its place
// ---------------------------------------------------------------------------

#[test]
fn xml_comments_and_pis_outside_the_root_keep_their_order_and_lines() {
    let src = "<?xml version=\"1.0\"?>\n<!-- top -->\n<?style a?>\n<r>\n  <a>1</a>\n</r>\n\
               <!-- after -->\n<?tail b?>\n";
    // Was: `<?xml version="1.0"?><?style a?><?tail b?><!-- top --><!-- after --><r>…`:
    // no line break between the items, and those after the root before it.
    edits_to(
        "a.xml",
        src,
        &["set", "a.xml", "/r/0/a/0/#text", "2"],
        &src.replace("<a>1</a>", "<a>2</a>"),
    );
    edits_to("a.xml", src, &["fmt", "a.xml"], src);
    // One added by a filter goes after the last of its kind.
    edits_to(
        "a.xml",
        src,
        &["set", "a.xml", "--jq", ".\"#comments\" += [\"new\"]"],
        &src.replace("<!-- after -->\n", "<!-- after -->\n<!--new-->\n"),
    );
}

// ---------------------------------------------------------------------------
// An edit that changes nothing leaves the bytes alone
// ---------------------------------------------------------------------------

/// Run `dq <args>` in a fresh directory holding `files`; the outcome's
/// `file` is the first of them afterwards.
fn run_in(files: &[(&str, &str)], args: &[&str]) -> Outcome {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, src).expect("seed");
    }
    let out = dq()
        .current_dir(dir.path())
        .args(args)
        .output()
        .expect("run dq");
    Outcome {
        code: out.status.code().expect("exit code"),
        stdout: String::from_utf8(out.stdout).expect("utf-8 stdout"),
        stderr: String::from_utf8(out.stderr).expect("utf-8 stderr"),
        file: std::fs::read_to_string(dir.path().join(files[0].0)).expect("read back"),
    }
}

#[test]
fn a_rewrite_edit_that_changes_nothing_leaves_the_file_as_it_was() {
    let ini = "; keep me\n[s]\nk = v\nn = 80\n";
    // Was: exit 1, "would modify: a.ini".
    assert_eq!(
        run("a.ini", ini, &["--check", "set", "a.ini", "/s/k", "v"]),
        Outcome {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
            file: ini.to_owned(),
        }
    );
    // Was: the comment stripped. `80` reads back from INI as the string
    // the file holds already.
    for args in [
        &["set", "a.ini", "/s/k", "v"][..],
        &["set", "a.ini", "/s/n", "80"],
        &["merge", "a.ini", r#"{"s": {"k": "v"}}"#],
        &[
            "patch",
            "a.ini",
            r#"[{"op": "replace", "path": "/s/k", "value": "v"}]"#,
        ],
    ] {
        edits_to("a.ini", ini, args, ini);
    }
    // Printed without `-i`, the file is printed as it is.
    assert_eq!(
        run("a.ini", ini, &["set", "a.ini", "/s/k", "v"]).stdout,
        ini
    );
    let env = "# keep me\nexport A=1\n";
    edits_to(".env", env, &["set", ".env", "/A", "1"], env);
    let tf = "# keep me\nami = var.ami\nn = 1\n";
    edits_to("a.tf", tf, &["set", "a.tf", "/n", "1"], tf);
    let csv = "n,a\nx,1\n";
    edits_to("t.csv", csv, &["set", "t.csv", "/0/a", "1"], csv);
}

#[test]
fn a_bulk_rewrite_counts_a_file_it_does_not_change_as_skipped() {
    let out = run_in(
        &[("a.ini", "; keep\n[s]\nk = v\n"), ("b.ini", "[s]\nk = w\n")],
        &["-i", "set", "*.ini", "/s/k", "v"],
    );
    // Was: Modified: 2.
    assert_eq!(
        (out.code, out.stdout.as_str(), out.file.as_str()),
        (
            0,
            "Modified: 1, Skipped: 1, Failed: 0\n",
            "; keep\n[s]\nk = v\n"
        )
    );
}

#[test]
fn a_fix_op_that_sets_a_value_already_there_leaves_the_file_alone() {
    let rule = "id: t.port\ndescription: port is 80\nseverity: warn\nmatch:\n  format: ini\n\
                check:\n  jq: 'select(.s.port != 80)'\n  message: port\nfix:\n  ops: |\n    \
                [{\"op\":\"replace\",\"path\":\"/s/port\",\"value\":80}]\n";
    let ini = "; keep\n[s]\nport = 80\n";
    let out = run_in(
        &[("a.ini", ini), ("rules/port.yml", rule)],
        &["-i", "fix", "--rules", "rules/port.yml", "a.ini"],
    );
    // The fixer's idempotency check already kept this file: a second run
    // of the ops after a change that changed nothing asks for the same
    // change again. Pinned so the rewrite never takes it over.
    assert_eq!((out.code, out.file.as_str()), (0, ini), "{out:?}");
}

#[test]
fn a_textual_edit_that_changes_nothing_already_left_the_file_alone() {
    for (name, src, pointer, value) in [
        ("a.yaml", "# c\nk: 'v'\nn: 1\n", "/k", "v"),
        ("a.yaml", "# c\nk: 'v'\nn: 1\n", "/n", "1"),
        ("a.toml", "# c\nk = 'v'\nn = 1\n", "/k", "v"),
        ("a.toml", "# c\nk = 'v'\nn = 1\n", "/n", "1"),
        ("a.json", "{\"k\":  \"v\"}\n", "/k", "v"),
    ] {
        assert_eq!(
            run(name, src, &["--check", "set", name, pointer, value]),
            Outcome {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
                file: src.to_owned(),
            },
            "{name} {pointer}"
        );
    }
}

// ---------------------------------------------------------------------------
// JSON Lines point edits
// ---------------------------------------------------------------------------

#[test]
fn jsonl_point_edits_address_records_by_index_and_rewrite_the_file() {
    let src = "{\"a\":1,\"b\":[1, 2]}\n{\"a\":2}\n";
    // `get` addresses a record by its index; the edits do the same. Was:
    // exit 7, "jsonl has no textual edits".
    assert_eq!(
        run("a.jsonl", src, &["get", "a.jsonl", "/1/a"]).stdout,
        "2\n"
    );
    edits_to(
        "a.jsonl",
        src,
        &["set", "a.jsonl", "/0/a", "5"],
        "{\"a\":5,\"b\":[1,2]}\n{\"a\":2}\n",
    );
    edits_to(
        "a.jsonl",
        src,
        &["set", "a.jsonl", "/-", r#"{"c": true}"#],
        "{\"a\":1,\"b\":[1,2]}\n{\"a\":2}\n{\"c\":true}\n",
    );
    edits_to("a.jsonl", src, &["del", "a.jsonl", "/0"], "{\"a\":2}\n");
    edits_to(
        "a.jsonl",
        src,
        &[
            "patch",
            "a.jsonl",
            r#"[{"op": "add", "path": "/1/c", "value": null}]"#,
        ],
        "{\"a\":1,\"b\":[1,2]}\n{\"a\":2,\"c\":null}\n",
    );
    // A value already there leaves the file as it is.
    edits_to("a.jsonl", src, &["set", "a.jsonl", "/1/a", "2"], src);
}

#[test]
fn a_jsonl_root_that_is_no_array_is_refused() {
    let src = "{\"a\":1}\n{\"a\":2}\n";
    // Was: every record dropped and one line written that reads back as
    // `[{"x":1}]` (or `[5]`), exit 0.
    let refusal = |kind: &str| {
        format!(
            "jsonl format error: a JSON Lines file is a list of records, one per line, so its \
             root is an array, not {kind}; set a record at /0 or append one at /-\n"
        )
    };
    for (args, kind) in [
        (&["set", "l.jsonl", "", r#"{"x":1}"#][..], "a map"),
        (&["set", "l.jsonl", "", "5"], "an integer"),
        (&["merge", "l.jsonl", r#"{"x":1}"#], "a map"),
    ] {
        refused("l.jsonl", src, args, 1, &refusal(kind));
    }
    // An array is a list of records.
    edits_to(
        "l.jsonl",
        src,
        &["set", "l.jsonl", "", r#"[{"x":1}]"#],
        "{\"x\":1}\n",
    );
}

#[test]
fn a_rewrite_that_would_not_read_back_as_the_edit_is_refused() {
    let ini = "; c\n[s]\nk = v\n";
    // Was: exit 0, the file left as it was — the value set reads back
    // trimmed, as what was there.
    refused(
        "a.ini",
        ini,
        &["set", "a.ini", "/s/k", " v "],
        7,
        "cannot set '/s/k': the ini dq would write reads back as another value at '/s/k'\n",
    );
    // Was: exit 0, `k=v` and `k=x=y`, which read back as `k = x=y`: the
    // key set clobbered another.
    refused(
        "a.ini",
        ini,
        &["set", "a.ini", "/s/k=x", "y"],
        7,
        "cannot set '/s/k=x': the ini dq would write reads back as another value at '/s/k'\n",
    );
    // What a format reads every value back as is not a change: a number
    // set in INI, dotenv or CSV reads back as its text.
    edits_to(
        "a.ini",
        ini,
        &["set", "a.ini", "/s/n", "1.50"],
        "[s]\nk=v\nn=1.5\n",
    );
    edits_to(
        "t.csv",
        "a,b\n1,2\n",
        &["set", "t.csv", "/-", r#"{"c": null}"#],
        "a,b,c\n1,2,\n,,\n",
    );
}
