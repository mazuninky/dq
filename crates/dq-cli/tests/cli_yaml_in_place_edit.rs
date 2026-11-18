//! `dq -i set` / `dq -i del` on YAML through the real binary: the bytes the
//! textual edit writes back to the file, for the shapes real files have —
//! empty values, compact sequence items (k8s `containers`, GitHub Actions
//! `steps`), block scalars, tags, aliases, collections an edit empties.

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

/// Write `src` to `doc.yaml`, run `dq -i <command> doc.yaml <args…>`,
/// assert exit 0 and an empty stdout, and return the file's new bytes.
fn in_place(src: &str, command: &str, args: &[&str]) -> String {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.yaml");
    std::fs::write(&path, src).expect("write fixture");
    dq().current_dir(dir.path())
        .args(["-i", "--no-color", command, "doc.yaml"])
        .args(args)
        .assert()
        .success()
        .stdout("");
    String::from_utf8(std::fs::read(&path).expect("read back")).expect("UTF-8 file")
}

/// `bytes` as `dq -F json convert` reads them.
fn read_back(bytes: &str) -> serde_json::Value {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("doc.yaml"), bytes).expect("write");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "-F", "json", "convert", "doc.yaml"])
        .assert()
        .success();
    serde_json::from_slice(&out.get_output().stdout).expect("stdout is JSON")
}

/// Run each `(src, command, args, want)` case and compare the file.
fn check(cases: &[(&str, &str, &[&str], &str)]) {
    for (src, command, args, want) in cases {
        assert_eq!(
            in_place(src, command, args),
            *want,
            "{command} {args:?} on {src:?}"
        );
    }
}

#[test]
fn set_on_an_empty_value_writes_it_after_the_indicator() {
    check(&[
        ("a:\nb: 1\n", "set", &["/a", "5"], "a: 5\nb: 1\n"),
        ("a: # c\nb: 1\n", "set", &["/a", "5"], "a: 5 # c\nb: 1\n"),
        ("a: &x\nb: *x\n", "set", &["/a", "5"], "a: &x 5\nb: *x\n"),
        ("a: !!str\nb: 1\n", "set", &["/a", "5"], "a: 5\nb: 1\n"),
        (
            "l:\n  -\n  - 2\n",
            "set",
            &["/l/0", "5"],
            "l:\n  - 5\n  - 2\n",
        ),
        ("a:\r\nb: 1\r\n", "set", &["/a", "5"], "a: 5\r\nb: 1\r\n"),
        ("b: 1\na:", "set", &["/a", "5"], "b: 1\na: 5"),
        ("? v\na: 1\n", "set", &["/v", "42"], "? v\n: 42\na: 1\n"),
        (
            "? v\r\na: 1\r\n",
            "set",
            &["/v", "42"],
            "? v\r\n: 42\r\na: 1\r\n",
        ),
    ]);
}

const K8S: &str = "\
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      containers:
        - name: web
          image: nginx:1.25
          ports:
            - containerPort: 80
        - name: side
          image: busybox
";

#[test]
fn compact_items_of_a_k8s_deployment() {
    let containers = "/spec/template/spec/containers";
    let first_name = format!("{containers}/0/name");
    let ports = format!("{containers}/0/ports");
    let args = format!("{containers}/1/args");
    let policy = format!("{containers}/0/imagePullPolicy");
    check(&[
        (
            K8S,
            "del",
            &[&first_name],
            &K8S.replace("- name: web\n          image", "- image"),
        ),
        (
            K8S,
            "del",
            &[&ports],
            &K8S.replace("          ports:\n            - containerPort: 80\n", ""),
        ),
        (
            K8S,
            "set",
            &[&args, "[\"sh\"]"],
            &format!("{K8S}          args:\n            - sh\n"),
        ),
        (
            K8S,
            "set",
            &[&policy, "Always"],
            &K8S.replace(
                "containerPort: 80\n",
                "containerPort: 80\n          imagePullPolicy: Always\n",
            ),
        ),
        // A new key after the collection that ends its parent.
        (
            K8S,
            "set",
            &["/spec/replicas", "3"],
            &format!("{K8S}  replicas: 3\n"),
        ),
    ]);
}

const GHA: &str = "\
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Test
        run: cargo test
";

#[test]
fn compact_items_of_github_actions_steps() {
    check(&[
        (
            GHA,
            "del",
            &["/jobs/build/steps/1/name"],
            &GHA.replace("- name: Test\n        run", "- run"),
        ),
        // The only key of an item: the item stays, as an empty mapping.
        (
            GHA,
            "del",
            &["/jobs/build/steps/0/uses"],
            &GHA.replace("- uses: actions/checkout@v4", "- {}"),
        ),
        (
            GHA,
            "set",
            &["/jobs/build/steps/0/with", "{\"fetch-depth\": 0}"],
            &GHA.replace(
                "checkout@v4\n",
                "checkout@v4\n        with:\n          fetch-depth: 0\n",
            ),
        ),
        (
            GHA,
            "set",
            &["/jobs/build/steps/1/env", "{\"RUST_LOG\": \"debug\"}"],
            &format!("{GHA}        env:\n          RUST_LOG: debug\n"),
        ),
    ]);
}

#[test]
fn compact_items_in_a_crlf_file() {
    check(&[
        (
            "l:\r\n  - x: 1\r\n    y: 2\r\n",
            "del",
            &["/l/0/x"],
            "l:\r\n  - y: 2\r\n",
        ),
        ("l:\r\n  - x: 1\r\n", "del", &["/l/0/x"], "l:\r\n  - {}\r\n"),
        (
            "l:\r\n  - x: 1\r\n",
            "set",
            &["/l/0/z", "3"],
            "l:\r\n  - x: 1\r\n    z: 3\r\n",
        ),
    ]);
}

#[test]
fn a_string_the_reader_would_type_otherwise_is_written_quoted() {
    // `--value-string`: the argument is the string, whatever it looks like.
    for text in [
        ".nan", ".NaN", ".inf", "0x10", "0o17", "~", "null", "Null", "yes", "on", "true", "1e3",
    ] {
        // A replaced value, a new key, an entry of a flow mapping.
        let src = "j: 1\nm: {x: 1}\n";
        let out = in_place(src, "set", &["/j", text, "--value-string"]);
        assert_eq!(out, format!("j: \"{text}\"\nm: {{x: 1}}\n"), "{text}");
        let out = in_place(&out, "set", &["/n", text, "--value-string"]);
        let out = in_place(&out, "set", &["/m/y", text, "--value-string"]);
        assert_eq!(
            read_back(&out),
            serde_json::json!({"j": text, "m": {"x": 1, "y": text}, "n": text}),
            "{text}: {out:?}"
        );
    }
    // A date is quoted too: dq reads it back as a string, a YAML 1.1
    // reader (go-yaml, PyYAML) as a timestamp.
    let out = in_place("j: 1\n", "set", &["/j", "2024-01-01", "--value-string"]);
    assert_eq!(out, "j: \"2024-01-01\"\n");
    assert_eq!(read_back(&out), serde_json::json!({"j": "2024-01-01"}));
}

#[test]
fn a_string_a_yaml_1_1_reader_would_type_is_quoted_wherever_it_lands() {
    // dq reads these back as strings; go-yaml and PyYAML read ints,
    // sexagesimals, timestamps, bools, merge and value keys, and
    // infinity. A value, a flow mapping entry, a flow sequence item and a
    // new key — each typed as a plain argument, no `--value-string`.
    let src = "j: 1\nm: {x: 1}\nl: [1]\n";
    for (text, quoted) in [
        ("1_000", true),
        ("0b101", true),
        ("1:20", true),
        ("12:30:45", true),
        ("2024-01-01", true),
        ("2001-12-14T21:59:43Z", true),
        ("<<", true),
        ("=", true),
        (".Inf", true),
        ("y", true),
        ("N", true),
        ("1.2.3", false),
        ("v1", false),
        ("hello", false),
        ("a-b", false),
    ] {
        let mut out = in_place(src, "set", &["/j", text]);
        out = in_place(&out, "set", &["/m/z", text]);
        out = in_place(&out, "set", &["/l/0", text]);
        out = in_place(&out, "set", &[&format!("/{text}"), "1"]);
        let t = if quoted {
            format!("\"{text}\"")
        } else {
            text.to_owned()
        };
        assert_eq!(
            out,
            format!("j: {t}\nm: {{x: 1, z: {t}}}\nl: [{t}]\n{t}: 1\n")
        );
        assert_eq!(
            read_back(&out),
            serde_json::json!({"j": text, "m": {"x": 1, "z": text}, "l": [text], text: 1}),
            "{text}: {out:?}"
        );
    }
}

#[test]
fn set_drops_a_core_tag_and_keeps_the_anchor_and_a_local_tag() {
    check(&[
        (
            "b: &a !!binary |\n  aGVsbG8=\nc: *a\n",
            "set",
            &["/b", "hi"],
            "b: &a hi\nc: *a\n",
        ),
        (
            "b: !local |\n  x\nc: 1\n",
            "set",
            &["/b", "hi"],
            "b: !local hi\nc: 1\n",
        ),
        (
            "b: &a !local x\nc: *a\n",
            "set",
            &["/b", "hi"],
            "b: &a !local hi\nc: *a\n",
        ),
    ]);
    assert_eq!(
        read_back("b: &a hi\nc: *a\n"),
        serde_json::json!({"b": "hi", "c": "hi"})
    );
}

#[test]
fn edits_next_to_aliases() {
    check(&[
        // An alias key naming a collection keeps later pointers in place.
        (
            "a: &k {p: 1}\n? *k\n: x\nz: 1\n",
            "set",
            &["/z", "2"],
            "a: &k {p: 1}\n? *k\n: x\nz: 2\n",
        ),
        // Anchor and tag on the anchored line, the alias after it.
        (
            "a: &x !!str 1\nb: *x\nc: 2\n",
            "set",
            &["/c", "3"],
            "a: &x !!str 1\nb: *x\nc: 3\n",
        ),
        (
            "a: &x !!str 1\nb: *x\nc: 2\n",
            "set",
            &["/b", "3"],
            "a: &x !!str 1\nb: 3\nc: 2\n",
        ),
        // Deleting the alias itself is fine.
        ("a: &x 1\nb: *x\nc: 2\n", "del", &["/b"], "a: &x 1\nc: 2\n"),
        (
            "a: &x 1\nl:\n  - *x\n  - 2\n",
            "del",
            &["/l/0"],
            "a: &x 1\nl:\n  - 2\n",
        ),
        ("a: &x {k: 1}\nb: *x\n", "del", &["/b"], "a: &x {k: 1}\n"),
    ]);
}

#[test]
fn set_on_a_block_scalar_rewrites_it_inline() {
    check(&[
        // Keep chomping: the trailing blank lines stay as blank lines.
        (
            "k: |+\n  x\n\n\nr: 1\n",
            "set",
            &["/k", "5"],
            "k: 5\n\n\nr: 1\n",
        ),
        ("k: |2\n   x\nr: 1\n", "set", &["/k", "5"], "k: 5\nr: 1\n"),
        ("k: |2-\n   x\nr: 1\n", "set", &["/k", "5"], "k: 5\nr: 1\n"),
        // A `|` inside the header's comment is not the indicator.
        (
            "k: | # a | b\n  x\nr: 1\n",
            "set",
            &["/k", "5"],
            "k: 5 # a | b\nr: 1\n",
        ),
        // CRLF, last in the file, no final line break.
        ("r: 1\nk: |\r\n  x", "set", &["/k", "5"], "r: 1\nk: 5"),
        (
            "o:\n  p:\n    k: |\n      x\n    q: 1\n",
            "set",
            &["/o/p/k", "5"],
            "o:\n  p:\n    k: 5\n    q: 1\n",
        ),
        // A sequence item with node properties on the `-` line.
        (
            "l:\n  - &a !local |\n    x\n  - 2\n",
            "set",
            &["/l/0", "5"],
            "l:\n  - &a !local 5\n  - 2\n",
        ),
        // The header on a line of its own.
        (
            "k:\n  |\n  x\nr: 1\n",
            "set",
            &["/k", "5"],
            "k:\n  5\nr: 1\n",
        ),
        ("k:\n  |\n  x\nr: 1\n", "del", &["/k"], "r: 1\n"),
    ]);
}

#[test]
fn a_new_key_after_a_block_scalar_keeps_its_value() {
    for (src, want, value) in [
        (
            "k: |+\n  x\n\n\n",
            "k: |+\n  x\n\n\nz: 5\n",
            serde_json::json!({"k": "x\n\n\n", "z": 5}),
        ),
        (
            "k: |2-\n   x\n",
            "k: |2-\n   x\nz: 5\n",
            serde_json::json!({"k": " x", "z": 5}),
        ),
        (
            "k: | # a | b\n  x\n",
            "k: | # a | b\n  x\nz: 5\n",
            serde_json::json!({"k": "x\n", "z": 5}),
        ),
        // The scalar without a final line break turns to strip chomping.
        (
            "r: 1\r\nk: |\r\n  x",
            "r: 1\r\nk: |-\r\n  x\r\nz: 5\r\n",
            serde_json::json!({"r": 1, "k": "x", "z": 5}),
        ),
        (
            "k:\n  |\n  x\n",
            "k:\n  |\n  x\nz: 5\n",
            serde_json::json!({"k": "x\n", "z": 5}),
        ),
    ] {
        let out = in_place(src, "set", &["/z", "5"]);
        assert_eq!(out, want, "{src:?}");
        assert_eq!(read_back(&out), value, "{src:?}");
    }
}

#[test]
fn del_of_the_last_child_leaves_an_empty_collection() {
    check(&[
        // The marker goes after the node properties, before the comment.
        (
            "a: &m # c\n  x: 1\nb: 2\n",
            "del",
            &["/a/x"],
            "a: &m {} # c\nb: 2\n",
        ),
        (
            "l:\n  -\n    x: 1\n  - 2\n",
            "del",
            &["/l/0/x"],
            "l:\n  - {}\n  - 2\n",
        ),
        (
            "a:\n  x: 1\n---\nb:\n  y: 2\n",
            "del",
            &["/1/b/y"],
            "a:\n  x: 1\n---\nb: {}\n",
        ),
        (
            "a:\n  x: 1\n---\nb:\n  y: 2\n",
            "del",
            &["/0/a/x"],
            "a: {}\n---\nb:\n  y: 2\n",
        ),
        // Inside a flow collection.
        ("a: {x: {y: 1}}\n", "del", &["/a/x/y"], "a: {x: {}}\n"),
        ("a: [{x: 1}]\n", "del", &["/a/0/x"], "a: [{}]\n"),
        ("a: {x: [1]}\n", "del", &["/a/x/0"], "a: {x: []}\n"),
    ]);
}

#[test]
fn set_over_a_block_collection_replaces_it_after_its_key() {
    let cases: &[(&str, &[&str], &str, serde_json::Value)] = &[
        // An anchored collection: the aliases follow the new value.
        (
            "a: &m\n  x: 1\nb: *m\nc: 3\n",
            &["/a", "5"],
            "a: &m 5\nb: *m\nc: 3\n",
            serde_json::json!({"a": 5, "b": 5, "c": 3}),
        ),
        (
            "a: &m\n  x: 1\nb: *m\n",
            &["/a", "[1, 2]"],
            "a: &m [1, 2]\nb: *m\n",
            serde_json::json!({"a": [1, 2], "b": [1, 2]}),
        ),
        // Comments inside the collection go with it, the comment lines
        // before its first entry stay; the comment after it stays too.
        (
            "a:\n  # lead\n  x: 1 # one\n  # mid\n  y: 2\n# after\nb: 2\n",
            &["/a", "5"],
            "a: 5\n  # lead\n# after\nb: 2\n",
            serde_json::json!({"a": 5, "b": 2}),
        ),
        (
            "a:\n  x: 1 # one\n  y: [1, 2]\nb: 2\n",
            &["/a", "{\"q\": 1}"],
            "a: {\"q\": 1}\nb: 2\n",
            serde_json::json!({"a": {"q": 1}, "b": 2}),
        ),
        (
            "l:\n  - a\n  # c\n  - b\nz: 1\n",
            &["/l", "[]"],
            "l: []\nz: 1\n",
            serde_json::json!({"l": [], "z": 1}),
        ),
    ];
    for (src, args, want, value) in cases {
        let out = in_place(src, "set", args);
        assert_eq!(out, *want, "set {args:?} on {src:?}");
        assert_eq!(read_back(&out), *value, "set {args:?} on {src:?}");
    }
}

#[test]
fn set_over_a_collection_takes_an_alias_inside_it_along() {
    // The alias goes away with its anchor: nothing is left to dangle.
    check(&[
        (
            "a:\n  x: &q 1\n  y: *q\nb: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nb: 2\n",
        ),
        ("a: {x: &q 1, y: *q}\n", "set", &["/a", "5"], "a: 5\n"),
        (
            "l:\n  - x: &q 1\n    y: *q\n  - 2\n",
            "set",
            &["/l/0", "5"],
            "l:\n  - 5\n  - 2\n",
        ),
    ]);
}

#[test]
fn del_of_a_compact_first_key_hands_its_comments_to_the_dash() {
    check(&[
        (
            "l:\n  - a: 1 # t\n    # m1\n    # m2\n    b: 2\n",
            "del",
            &["/l/0/a"],
            "l:\n  - # m1\n    # m2\n    b: 2\n",
        ),
        // Blank lines between the comments stay; a blank line alone goes
        // with the entry.
        (
            "l:\n  - a: 1\n    # mid\n\n    # m2\n    b: 2\n  - z: 1\n",
            "del",
            &["/l/0/a"],
            "l:\n  - # mid\n\n    # m2\n    b: 2\n  - z: 1\n",
        ),
        (
            "l:\n  - a: 1\n\n    b: 2\n",
            "del",
            &["/l/0/a"],
            "l:\n  - b: 2\n",
        ),
        (
            "l:\r\n  - a: 1\r\n    # m1\r\n\r\n    # m2\r\n    b: 2\r\n",
            "del",
            &["/l/0/a"],
            "l:\r\n  - # m1\r\n\r\n    # m2\r\n    b: 2\r\n",
        ),
        // A comment at the dash's column.
        (
            "- a: 1\n# top\n  b: 2\n",
            "del",
            &["/0/a"],
            "- # top\n  b: 2\n",
        ),
    ]);
}

#[test]
fn yaml_writer_quotes_keys_and_values_that_would_read_as_null() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("doc.json"),
        r#"{"": 1, "null": 2, "~": 3, "a": "", "b": "null", "c": "~"}"#,
    )
    .expect("write");
    dq().current_dir(dir.path())
        .args(["--no-color", "-F", "yaml", "convert", "doc.json"])
        .assert()
        .success()
        .stdout("'': 1\n'null': 2\n'~': 3\na: ''\nb: 'null'\nc: '~'\n");
}

#[test]
fn lint_reports_the_real_node_after_aliases_in_a_k8s_file() {
    // Aliases before each reported node must not shift the positions.
    let src = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels: &labels
    app: web
spec:
  selector:
    matchLabels: *labels
  template:
    metadata:
      labels: *labels
    spec:
      containers:
        - name: web
          resources: &res {limits: {cpu: \"1\"}}
          image: nginx:latest
        - resources: *res
          name: side
          image: busybox:latest
";
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("deploy.yaml"), src).expect("write");
    let out = dq()
        .current_dir(dir.path())
        .args([
            "--no-color",
            "-F",
            "json",
            "lint",
            "--rules",
            "@std/k8s",
            "deploy.yaml",
        ])
        .assert()
        .code(4);
    let report: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("stdout is JSON");
    let at = |rule: &str| -> Vec<(u64, u64)> {
        report["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .filter(|d| d["rule_id"] == rule)
            .map(|d| {
                (
                    d["line"].as_u64().expect("line"),
                    d["col"].as_u64().expect("col"),
                )
            })
            .collect()
    };
    // The `image` values, and the `name` of each container.
    assert_eq!(at("k8s.no-latest-tag"), [(17, 18), (20, 18)]);
    assert_eq!(at("k8s.missing-liveness-probe"), [(15, 17), (19, 17)]);
}
