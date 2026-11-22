//! Repeated HCL blocks through the CLI: every block of a type repeated with
//! the same labels is read, as a list of its bodies in source order, and is
//! written back as a block by `convert -F hcl`, `set --jq` and `fix`.
//!
//! The HCL reader used to merge such blocks into one body, the last
//! attribute winning, so the earlier `ingress` blocks below were invisible
//! to `get` / `query` / `lint` and gone from any file dq wrote back.

use assert_cmd::Command;
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

/// A security group with three `ingress` blocks, the public one first,
/// and two `provider "aws"` blocks.
const TF: &str = r#"resource "aws_security_group" "app" {
  name = "app"
  ingress {
    from_port   = 80
    cidr_blocks = ["0.0.0.0/0"]
  }
  ingress {
    from_port   = 443
    cidr_blocks = ["10.0.0.0/8"]
  }
  ingress {
    from_port = 22
  }
  lifecycle {
    create_before_destroy = true
  }
}

provider "aws" {
  region = "us-east-1"
}

provider "aws" {
  alias  = "west"
  region = "us-west-2"
}
"#;

/// `TF` as dq writes it.
const TF_WRITTEN: &str = r#"resource "aws_security_group" "app" {
  name = "app"
  ingress {
    from_port = 80
    cidr_blocks = [
      "0.0.0.0/0"
    ]
  }
  ingress {
    from_port = 443
    cidr_blocks = [
      "10.0.0.0/8"
    ]
  }
  ingress {
    from_port = 22
  }
  lifecycle {
    create_before_destroy = true
  }
}
provider "aws" {
  region = "us-east-1"
}
provider "aws" {
  alias = "west"
  region = "us-west-2"
}
"#;

/// A temp dir holding `main.tf` with `TF`.
fn dir_with_tf() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("main.tf"), TF).expect("write main.tf");
    dir
}

/// Stdout of a successful `dq --no-color <args>` in `dir`, with a silent
/// stderr.
fn stdout_of(dir: &TempDir, args: &[&str]) -> String {
    let out = dq()
        .current_dir(dir.path())
        .arg("--no-color")
        .args(args)
        .assert()
        .success()
        .stderr("");
    String::from_utf8(out.get_output().stdout.clone()).expect("UTF-8 stdout")
}

#[test]
fn get_addresses_each_repeated_block_by_its_index() {
    let dir = dir_with_tf();
    let ingress = "/resource/aws_security_group/app/ingress";
    for (index, port) in [(0, "80"), (1, "443"), (2, "22")] {
        assert_eq!(
            stdout_of(
                &dir,
                &["get", "main.tf", &format!("{ingress}/{index}/from_port")]
            ),
            format!("{port}\n"),
        );
    }
    assert_eq!(
        stdout_of(&dir, &["get", "main.tf", "/provider/aws/0/region"]),
        "us-east-1\n"
    );
    // A block that occurs once keeps its documented shape.
    assert_eq!(
        stdout_of(
            &dir,
            &[
                "get",
                "main.tf",
                "/resource/aws_security_group/app/lifecycle/create_before_destroy"
            ]
        ),
        "true\n"
    );
}

#[test]
fn query_sees_every_repeated_block_in_source_order() {
    let dir = dir_with_tf();
    let out = stdout_of(
        &dir,
        &[
            "-F",
            "json",
            "query",
            "[.resource.aws_security_group.app.ingress[].from_port], .provider.aws",
            "main.tf",
        ],
    );
    let parsed: Value = serde_json::from_str(&out).expect("JSON");
    assert_eq!(
        parsed,
        json!([
            [80, 443, 22],
            [
                {"region": "us-east-1"},
                {"alias": "west", "region": "us-west-2"}
            ]
        ])
    );
}

#[test]
fn convert_to_hcl_writes_every_block_back_as_a_block() {
    let dir = dir_with_tf();
    assert_eq!(
        stdout_of(&dir, &["-F", "hcl", "convert", "main.tf"]),
        TF_WRITTEN
    );
}

#[test]
fn set_jq_keeps_every_repeated_block_and_can_add_one() {
    let dir = dir_with_tf();
    dq().current_dir(dir.path())
        .args([
            "--no-color",
            "set",
            "--jq",
            r#".resource.aws_security_group.app.name = "renamed""#,
            "-i",
            "main.tf",
        ])
        .assert()
        .success();
    let on_disk = std::fs::read_to_string(dir.path().join("main.tf")).expect("read main.tf");
    assert_eq!(
        on_disk,
        TF_WRITTEN.replace(r#"name = "app""#, r#"name = "renamed""#)
    );
    // A body appended to the list is one more block.
    dq().current_dir(dir.path())
        .args([
            "--no-color",
            "set",
            "--jq",
            ".resource.aws_security_group.app.ingress += [{from_port: 8080}]",
            "-i",
            "main.tf",
        ])
        .assert()
        .success();
    let on_disk = std::fs::read_to_string(dir.path().join("main.tf")).expect("read main.tf");
    assert!(
        on_disk.contains(
            "  ingress {\n    from_port = 22\n  }\n  ingress {\n    from_port = 8080\n  }\n"
        ),
        "{on_disk}"
    );
    assert_eq!(on_disk.matches("ingress {").count(), 4, "{on_disk}");
}

#[test]
fn fix_jq_keeps_every_repeated_block() {
    const RULE: &str = r#"id: sg-named
description: the security group is named
severity: warn
match:
  format: [hcl]
check:
  jq: 'select(.resource.aws_security_group.app.name != "fixed") | .'
  message: 'not named'
fix:
  jq: '.resource.aws_security_group.app.name = "fixed"'
"#;
    let dir = dir_with_tf();
    std::fs::create_dir_all(dir.path().join(".dq/rules")).expect("mkdir");
    std::fs::write(dir.path().join(".dq/rules/sg-named.yml"), RULE).expect("write rule");
    dq().current_dir(dir.path())
        .args(["--no-color", "-i", "fix", "main.tf"])
        .assert()
        .success();
    let on_disk = std::fs::read_to_string(dir.path().join("main.tf")).expect("read main.tf");
    assert_eq!(
        on_disk,
        TF_WRITTEN.replace(r#"name = "app""#, r#"name = "fixed""#)
    );
}

#[test]
fn a_block_with_the_name_of_an_attribute_exits_3_naming_it() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("x.tf"), "x = 1\nx {\n  b = 2\n}\n").expect("write x.tf");
    dq().current_dir(dir.path())
        .args(["--no-color", "-q", "-F", "json", "convert", "x.tf"])
        .assert()
        .code(3)
        .stdout("")
        .stderr(
            "parse error in x.tf: HCL error: block \"x\" has the name of an \
             attribute of the same body; dq reads both as the key \"x\" and would lose one\n\
             \x20 --> x.tf:2:1\n   |\n 2 | x {\n   | ^~~\n",
        );
}
