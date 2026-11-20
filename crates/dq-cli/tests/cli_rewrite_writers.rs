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
