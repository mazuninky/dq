//! Block `position` spans of the Markdown IR on top of comrak 1.0.
//!
//! comrak 1.0 changed where some blocks end: an indented code block and a
//! list nested in a container now end at their last character / last child
//! instead of at the start of the next line. Each test spawns the real
//! binary (`dq -F json convert x.md`) and compares an outline of the IR —
//! one line per block node, `<type> <start>-<end>` as `line:column` — so a
//! change in any span shows up as a one-line diff.

use std::io::Write as _;

use assert_cmd::Command;
use tempfile::NamedTempFile;

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

/// The IR of `markdown` as `dq -F json convert` prints it.
fn ir(markdown: &str) -> serde_json::Value {
    let mut tmp = NamedTempFile::with_suffix(".md").expect("tempfile");
    tmp.write_all(markdown.as_bytes()).expect("write tempfile");
    let path = tmp.into_temp_path();
    let out = dq()
        .args([
            "-F",
            "json",
            "convert",
            path.to_str().unwrap(),
            "--no-color",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("stdout is JSON")
}

/// One line per node that carries a `position`, indented by depth.
fn outline(markdown: &str) -> String {
    fn point(position: &serde_json::Value, key: &str) -> String {
        format!("{}:{}", position[key]["line"], position[key]["column"])
    }
    fn walk(node: &serde_json::Value, depth: usize, out: &mut String) {
        let position = &node["position"];
        if position.is_object() {
            let kind = node["type"].as_str().expect("node type");
            let indent = "  ".repeat(depth);
            let (start, end) = (point(position, "start"), point(position, "end"));
            out.push_str(&format!("{indent}{kind} {start}-{end}\n"));
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(&ir(markdown), 0, &mut out);
    out
}

#[test]
fn list_inside_blockquote_ends_at_its_last_child() {
    // comrak 0.52 ended the list and its item at 2:1, the start of the
    // `>` line after them.
    assert_eq!(
        outline("> - a\n>\n> b\n"),
        "\
document 1:1-3:3
  block_quote 1:1-3:3
    list 1:3-1:5
      list_item 1:3-1:5
        paragraph 1:5-1:5
    paragraph 3:3-3:3
",
    );
}

#[test]
fn indented_code_block_inside_list_item_ends_at_its_last_character() {
    // comrak 0.52 ended the code block and its item at 3:8, the column the
    // code starts at.
    assert_eq!(
        outline("1. x\n\n       code\n\n2. y\n"),
        "\
document 1:1-5:4
  list 1:1-5:4
    list_item 1:1-3:11
      paragraph 1:4-1:4
      code_block 3:8-3:11
    list_item 5:1-5:4
      paragraph 5:4-5:4
",
    );
}

#[test]
fn top_level_indented_code_block_ends_before_trailing_blank_lines() {
    // comrak 0.52 ended the code block at 4:0, past the blank lines.
    assert_eq!(
        outline("    code\n    more\n\n\n"),
        "\
document 1:1-4:0
  code_block 1:5-2:8
",
    );
}

#[test]
fn simple_list_followed_by_blank_lines_is_unchanged() {
    // The pair of the cases above: comrak 0.52 gave the same spans, so the
    // bump must not move them.
    assert_eq!(
        outline("- a\n- b\n\n\n"),
        "\
document 1:1-4:0
  list 1:1-2:3
    list_item 1:1-1:3
      paragraph 1:3-1:3
    list_item 2:1-2:3
      paragraph 2:3-2:3
",
    );
}
