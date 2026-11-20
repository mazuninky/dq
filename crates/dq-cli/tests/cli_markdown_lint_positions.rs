//! Where `@std/markdown` findings land in the file.
//!
//! The Markdown body is parsed apart from its frontmatter, and its block
//! positions used to count from the end of the frontmatter: every finding
//! in a file with a three-line header was three lines too high. A fenced
//! code block had no byte span, so `markdown.code-blocks-yaml-valid`
//! reported at 1:1 whatever the block. Positions now count from the start
//! of the file, and the YAML finding lands on the YAML error inside the
//! block.

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

/// `(rule_id, line, col)` of every diagnostic `dq lint` reports for a
/// Markdown file holding `markdown`, sorted.
fn findings(markdown: &str) -> Vec<(String, u64, u64)> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("post.md");
    std::fs::write(&path, markdown).unwrap();
    let out = dq()
        .args(["--no-color", "lint", "-F", "json", path.to_str().unwrap()])
        .output()
        .expect("run dq lint");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stderr)));
    let mut all: Vec<_> = report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["rule_id"].as_str().unwrap().to_owned(),
                d["line"].as_u64().unwrap(),
                d["col"].as_u64().unwrap(),
            )
        })
        .collect();
    all.sort();
    all
}

/// A body that trips most `@std/markdown` rules, each on a line of its own.
const BODY: &str = "\
# Title

### Skipped a level

# Second title

![](img.png)

[](https://example.test)

See https://example.test now.

<div>inline html</div>

## Trailing spaces

```
no language
```

    indented code

```yaml
a: 1
b: : x
```

| a | b |
|---|---|
| 1 | 2 |

## Trailing spaces
";

/// Every finding about the body lands on the same line and column, counted
/// from the start of the file, with or without a frontmatter header — the
/// header only moves it down by its own lines.
#[test]
fn frontmatter_moves_findings_by_its_own_lines() {
    let header = "---\ntitle: t\ndate: 2026-01-01\n---\n";
    let lines = header.lines().count() as u64;
    let bare = findings(BODY);
    let fronted = findings(&format!("{header}{BODY}"));
    let body_only = |all: Vec<(String, u64, u64)>, shift: u64| -> Vec<(String, u64, u64)> {
        all.into_iter()
            .filter(|(id, _, _)| !id.starts_with("markdown.frontmatter-"))
            .map(|(id, line, col)| (id, line - shift, col))
            .collect()
    };
    assert!(bare.len() > 8, "the body should trip many rules: {bare:#?}");
    assert_eq!(body_only(fronted, lines), body_only(bare, 0));
}

/// The YAML finding lands on the error inside the block: the block's
/// second content line, column 4, as `dq validate` reports it for the
/// block's text on its own (`line 2, col 4`).
#[test]
fn yaml_code_block_finding_lands_on_the_yaml_error() {
    let markdown = "---\ntitle: t\n---\n# T\n\n```yaml\na: 1\nb: : x\n```\n";
    let found: Vec<_> = findings(markdown)
        .into_iter()
        .filter(|(id, _, _)| id == "markdown.code-blocks-yaml-valid.parse-failed")
        .collect();
    assert_eq!(
        found,
        [(
            "markdown.code-blocks-yaml-valid.parse-failed".to_owned(),
            8,
            4
        )]
    );
}

/// A block inside a list item: the fence and its content are indented, and
/// the finding still lands on the error line, at the error's column in
/// the file.
#[test]
fn yaml_code_block_finding_in_a_list_item() {
    let markdown = "# T\n\n- item\n\n  ```yaml\n  b: : x\n  ```\n";
    let found: Vec<_> = findings(markdown)
        .into_iter()
        .filter(|(id, _, _)| id == "markdown.code-blocks-yaml-valid.parse-failed")
        .collect();
    assert_eq!(
        found,
        [(
            "markdown.code-blocks-yaml-valid.parse-failed".to_owned(),
            6,
            6
        )]
    );
}

/// `convert` shows the same positions: the first heading after a
/// three-line header starts on line 4.
#[test]
fn convert_positions_count_from_the_start_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("post.md");
    std::fs::write(&path, "---\ntitle: t\n---\n# T\n").unwrap();
    let out = dq()
        .args([
            "--no-color",
            "-F",
            "json",
            "convert",
            path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let ir: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let heading = &ir["children"][0];
    assert_eq!(heading["type"], "heading");
    assert_eq!(heading["position"]["start"]["line"], 4);
    assert_eq!(heading["position"]["start"]["column"], 1);
    assert_eq!(heading["position"]["end"]["line"], 4);
}

/// A fence indented by two blanks takes at most two from each content
/// line: a line indented less starts at its own first non-blank, and the
/// finding lands at the error's column in the file there too — not two
/// columns to the right.
#[test]
fn yaml_code_block_finding_on_a_line_indented_less_than_the_fence() {
    let id = "markdown.code-blocks-yaml-valid.parse-failed";
    let only = |markdown: &str| -> Vec<(String, u64, u64)> {
        findings(markdown)
            .into_iter()
            .filter(|(rule, _, _)| rule == id)
            .collect()
    };
    // Not indented at all: `a: b: c`, the error at its second `:`.
    assert_eq!(
        only("# T\n\n  ```yaml\na: b: c\n  ```\n"),
        [(id.to_owned(), 4, 5)]
    );
    // Indented by one blank, under a line indented by two.
    assert_eq!(
        only("# T\n\n  ```yaml\n  a: 1\n b: c: d\n  ```\n"),
        [(id.to_owned(), 5, 6)]
    );
}

/// A UTF-8 byte order mark before the frontmatter: the header is read as
/// the frontmatter, not as a thematic break and a paragraph, and the first
/// heading after it starts on line 4, column 1.
#[test]
fn frontmatter_after_a_byte_order_mark() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("post.md");
    std::fs::write(&path, "\u{feff}---\ntitle: t\n---\n# T\n").unwrap();
    let out = dq()
        .args([
            "--no-color",
            "-F",
            "json",
            "convert",
            path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ir: serde_json::Value = serde_json::from_slice(&out).expect("JSON");
    assert_eq!(
        ir["frontmatter"],
        serde_json::json!({"kind": "yaml", "value": {"title": "t"}})
    );
    assert_eq!(ir["children"][0]["type"], "heading");
    assert_eq!(
        ir["children"][0]["position"]["start"],
        serde_json::json!({"line": 4, "column": 1})
    );
}
