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
        // before its first entry too; the comment after it, shallower
        // than its entries, stays.
        (
            "a:\n  # lead\n  x: 1 # one\n  # mid\n  y: 2\n# after\nb: 2\n",
            &["/a", "5"],
            "a: 5\n# after\nb: 2\n",
            serde_json::json!({"a": 5, "b": 2}),
        ),
        (
            "a:\n  x: 1 # one\n  y: [1, 2]\nb: 2\n",
            &["/a", "{\"q\": 1}"],
            "a: {q: 1}\nb: 2\n",
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

/// The stdout of `dq get doc.yaml <pointer>` with `bytes` as the file.
fn get_back(bytes: &str, pointer: &str) -> String {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("doc.yaml"), bytes).expect("write");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "get", "doc.yaml", pointer])
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).expect("UTF-8 stdout")
}

/// Run each `(src, command, args, want, value)` case: the file becomes
/// `want`, and reads back as `value`.
fn check_read(cases: &[(&str, &str, &[&str], &str, serde_json::Value)]) {
    for (src, command, args, want, value) in cases {
        let out = in_place(src, command, args);
        assert_eq!(out, *want, "{command} {args:?} on {src:?}");
        assert_eq!(read_back(&out), *value, "{command} {args:?} on {src:?}");
    }
}

#[test]
fn del_of_an_explicit_key_without_a_value_keeps_the_keys_after_it() {
    check_read(&[
        (
            "? k\n? j\n",
            "del",
            &["/k"],
            "? j\n",
            serde_json::json!({"j": null}),
        ),
        (
            "? k\n? j\n? l\n",
            "del",
            &["/j"],
            "? k\n? l\n",
            serde_json::json!({"k": null, "l": null}),
        ),
        (
            "? k\r\n? j\r\n",
            "del",
            &["/k"],
            "? j\r\n",
            serde_json::json!({"j": null}),
        ),
        (
            "? k # c\n? j\n",
            "del",
            &["/k"],
            "? j\n",
            serde_json::json!({"j": null}),
        ),
        // The last one has no key after it to lose.
        (
            "? k\n? j\n",
            "del",
            &["/j"],
            "? k\n",
            serde_json::json!({"k": null}),
        ),
    ]);
}

#[test]
fn set_over_a_tagged_collection_drops_a_core_tag_and_keeps_the_rest() {
    let five = || serde_json::json!({"a": 5, "c": 2});
    check_read(&[
        (
            "a: !!map\n  b: 1\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nc: 2\n",
            five(),
        ),
        (
            "a: !!map {b: 1}\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nc: 2\n",
            five(),
        ),
        // The tag on a line of its own: the line goes.
        (
            "a:\n  !!map\n  b: 1\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nc: 2\n",
            five(),
        ),
        (
            "a: !!map # c\n  b: 1\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: 5 # c\nc: 2\n",
            five(),
        ),
        (
            "a: !!map\r\n  b: 1\r\nc: 2\r\n",
            "set",
            &["/a", "5"],
            "a: 5\r\nc: 2\r\n",
            five(),
        ),
        (
            "a: !!map\n  b: 1\nc: 2\n",
            "set",
            &["/a", "[1]"],
            "a: [1]\nc: 2\n",
            serde_json::json!({"a": [1], "c": 2}),
        ),
        (
            "a: !!seq\n  - 1\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nc: 2\n",
            five(),
        ),
        (
            "a: !!seq\n- 1\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nc: 2\n",
            five(),
        ),
        (
            "a: !!seq [1]\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nc: 2\n",
            five(),
        ),
        (
            "l:\n  - !!map\n    b: 1\n  - 2\n",
            "set",
            &["/l/0", "5"],
            "l:\n  - 5\n  - 2\n",
            serde_json::json!({"l": [5, 2]}),
        ),
        (
            "l:\n  - !!seq [1]\n  - 2\n",
            "set",
            &["/l/0", "5"],
            "l:\n  - 5\n  - 2\n",
            serde_json::json!({"l": [5, 2]}),
        ),
        // The anchor stays, before the value, whichever came first; the
        // aliases follow the new value.
        (
            "a: &x !!map\n  b: 1\n",
            "set",
            &["/a", "5"],
            "a: &x 5\n",
            serde_json::json!({"a": 5}),
        ),
        (
            "a: !!map &x\n  b: 1\n",
            "set",
            &["/a", "5"],
            "a: &x 5\n",
            serde_json::json!({"a": 5}),
        ),
        (
            "a: &x !!map\n  b: 1\nc: *x\n",
            "set",
            &["/a", "5"],
            "a: &x 5\nc: *x\n",
            serde_json::json!({"a": 5, "c": 5}),
        ),
        (
            "a: &x !!seq [1]\nb: *x\n",
            "set",
            &["/a", "5"],
            "a: &x 5\nb: *x\n",
            serde_json::json!({"a": 5, "b": 5}),
        ),
        // A local tag is not the core schema's: it stays on the new value.
        (
            "a: !Foo\n  b: 1\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: !Foo 5\nc: 2\n",
            five(),
        ),
        (
            "a: !Foo {b: 1}\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: !Foo 5\nc: 2\n",
            five(),
        ),
        (
            "a: !Foo\n  - 1\nc: 2\n",
            "set",
            &["/a", "5"],
            "a: !Foo 5\nc: 2\n",
            five(),
        ),
        // An edit inside the collection keeps its tag.
        (
            "a: !!map\n  b: 1\nc: 2\n",
            "del",
            &["/a/b"],
            "a: !!map {}\nc: 2\n",
            serde_json::json!({"a": {}, "c": 2}),
        ),
    ]);
    assert_eq!(get_back("a: &x 5\nc: *x\n", "/c"), "5\n");
}

#[test]
fn set_over_a_block_collection_takes_the_comment_lines_inside_it() {
    check_read(&[
        (
            "a:\n  # note\n  b: 1\n  c: 2\nd: 3\n",
            "set",
            &["/a", "5"],
            "a: 5\nd: 3\n",
            serde_json::json!({"a": 5, "d": 3}),
        ),
        (
            "a:\n  # note\n  b: 1\n  c: 2\nd: 3\n",
            "set",
            &["/a", "{\"z\":3}"],
            "a: {z: 3}\nd: 3\n",
            serde_json::json!({"a": {"z": 3}, "d": 3}),
        ),
        // After its last entry, as deep as its entries: inside it.
        (
            "a:\n  b: 1\n  # tail\n# keep\nd: 3\n",
            "set",
            &["/a", "5"],
            "a: 5\n# keep\nd: 3\n",
            serde_json::json!({"a": 5, "d": 3}),
        ),
        // Blank lines between such comments go; the ones after them stay.
        (
            "a:\n  b: 1\n\n  # tail\nd: 3\n",
            "set",
            &["/a", "5"],
            "a: 5\nd: 3\n",
            serde_json::json!({"a": 5, "d": 3}),
        ),
        (
            "a:\n  b: 1\n  # tail\n\nd: 3\n",
            "set",
            &["/a", "5"],
            "a: 5\n\nd: 3\n",
            serde_json::json!({"a": 5, "d": 3}),
        ),
        // A shallower comment is the parent's: it stays.
        (
            "a:\n  b: 1\n# keep\nd: 3\n",
            "set",
            &["/a", "5"],
            "a: 5\n# keep\nd: 3\n",
            serde_json::json!({"a": 5, "d": 3}),
        ),
        (
            "a:\n  b:\n    c: 1\n    # deep\n  # mid\n# top\nd: 3\n",
            "set",
            &["/a/b", "5"],
            "a:\n  b: 5\n  # mid\n# top\nd: 3\n",
            serde_json::json!({"a": {"b": 5}, "d": 3}),
        ),
        (
            "l:\n  - a: 1\n    # in\n  # tail\nz: 1\n",
            "set",
            &["/l", "5"],
            "l: 5\nz: 1\n",
            serde_json::json!({"l": 5, "z": 1}),
        ),
        // An indentless sequence has its `-` at its key's column: a comment
        // there may as well be the parent's and stays; a deeper one goes.
        (
            "a:\n- 1\n# c\nb: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\n# c\nb: 2\n",
            serde_json::json!({"a": 5, "b": 2}),
        ),
        (
            "a:\n- 1\n  # c\nb: 2\n",
            "set",
            &["/a", "5"],
            "a: 5\nb: 2\n",
            serde_json::json!({"a": 5, "b": 2}),
        ),
    ]);
    assert_eq!(get_back("a: 5\n# keep\nd: 3\n", "/a"), "5\n");
}

#[test]
fn del_of_a_block_collection_takes_the_comment_lines_inside_it() {
    check_read(&[
        (
            "a:\n  # note\n  b: 1\n  c: 2\nd: 3\n",
            "del",
            &["/a"],
            "d: 3\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "a:\n  b: 1\n  # tail\n\n  # tail2\n# keep\nd: 3\n",
            "del",
            &["/a"],
            "# keep\nd: 3\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "x: 0\na:\n  b: 1\n  # tail\n",
            "del",
            &["/a"],
            "x: 0\n",
            serde_json::json!({"x": 0}),
        ),
        // A shallower comment stays.
        (
            "a:\n  b: 1\n# keep\nd: 3\n",
            "del",
            &["/a"],
            "# keep\nd: 3\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "a:\n- 1\n# c\nb: 2\n",
            "del",
            &["/a"],
            "# c\nb: 2\n",
            serde_json::json!({"b": 2}),
        ),
    ]);
}

#[test]
fn del_of_a_compact_first_key_before_an_empty_key() {
    check_read(&[
        (
            "- a: 1\n  \"\": 2\n- b: 3\n",
            "del",
            &["/0/a"],
            "- \"\": 2\n- b: 3\n",
            serde_json::json!([{"": 2}, {"b": 3}]),
        ),
        (
            "- a: 1\n  \"\": 2\n  c: 4\n",
            "del",
            &["/0/a"],
            "- \"\": 2\n  c: 4\n",
            serde_json::json!([{"": 2, "c": 4}]),
        ),
        (
            "- a: 1\n  '': 2\n- b: 3\n",
            "del",
            &["/0/a"],
            "- '': 2\n- b: 3\n",
            serde_json::json!([{"": 2}, {"b": 3}]),
        ),
        (
            "l:\n  - a: 1\n    \"\": 2\n  - b: 3\n",
            "del",
            &["/l/0/a"],
            "l:\n  - \"\": 2\n  - b: 3\n",
            serde_json::json!({"l": [{"": 2}, {"b": 3}]}),
        ),
        // A named key after it, as before.
        (
            "- a: 1\n  c: 4\n- b: 3\n",
            "del",
            &["/0/a"],
            "- c: 4\n- b: 3\n",
            serde_json::json!([{"c": 4}, {"b": 3}]),
        ),
    ]);
}

#[test]
fn set_of_negative_zero_keeps_its_sign() {
    let out = in_place("x: 0.0\n", "set", &["/x", "--", "-0.0"]);
    assert_eq!(out, "x: -0.0\n");
    assert_eq!(get_back(&out, "/x"), "-0.0\n");
    let out = in_place("x: 1\n", "set", &["/y", "--", "-0.0"]);
    assert_eq!(out, "x: 1\n\"y\": -0.0\n");
    assert_eq!(get_back(&out, "/y"), "-0.0\n");
}

#[test]
fn set_drops_a_core_tag_however_it_is_spelled() {
    let hello = || serde_json::json!({"a": "hello", "c": 2});
    check_read(&[
        // Verbatim.
        (
            "a: !<tag:yaml.org,2002:map> {b: 1}\nc: 2\n",
            "set",
            &["/a", "hello"],
            "a: hello\nc: 2\n",
            hello(),
        ),
        (
            "a: !<tag:yaml.org,2002:map>\n  b: 1\nc: 2\n",
            "set",
            &["/a", "hello"],
            "a: hello\nc: 2\n",
            hello(),
        ),
        (
            "a: !<tag:yaml.org,2002:seq> [1]\nc: 2\n",
            "set",
            &["/a", "hello"],
            "a: hello\nc: 2\n",
            hello(),
        ),
        // A `%TAG` handle bound to the core prefix.
        (
            "%TAG !e! tag:yaml.org,2002:\n---\na: !e!map {b: 1}\n",
            "set",
            &["/a", "hello"],
            "%TAG !e! tag:yaml.org,2002:\n---\na: hello\n",
            serde_json::json!({"a": "hello"}),
        ),
        // The non-specific `!` makes a scalar a string, which `7` is not.
        (
            "a: ! 5\nc: 2\n",
            "set",
            &["/a", "7"],
            "a: 7\nc: 2\n",
            serde_json::json!({"a": 7, "c": 2}),
        ),
        (
            "a: &x ! 5\nc: *x\n",
            "set",
            &["/a", "7"],
            "a: &x 7\nc: *x\n",
            serde_json::json!({"a": 7, "c": 7}),
        ),
        (
            "a: !\nc: 2\n",
            "set",
            &["/a", "7"],
            "a: 7\nc: 2\n",
            serde_json::json!({"a": 7, "c": 2}),
        ),
        // Tags outside the core schema stay.
        (
            "a: !<tag:example.com,2000:x> {b: 1}\nc: 2\n",
            "set",
            &["/a", "hello"],
            "a: !<tag:example.com,2000:x> hello\nc: 2\n",
            hello(),
        ),
        (
            "%TAG !e! tag:example.com,2000:\n---\na: !e!x {b: 1}\n",
            "set",
            &["/a", "hello"],
            "%TAG !e! tag:example.com,2000:\n---\na: !e!x hello\n",
            serde_json::json!({"a": "hello"}),
        ),
        // The `%TAG` of the document the node is in binds the handle.
        (
            "%TAG !e! tag:example.com,2000:\n---\na: 1\n...\n\
             %TAG !e! tag:yaml.org,2002:\n---\nb: !e!map {c: 1}\n",
            "set",
            &["/1/b", "hello"],
            "%TAG !e! tag:example.com,2000:\n---\na: 1\n...\n\
             %TAG !e! tag:yaml.org,2002:\n---\nb: hello\n",
            serde_json::json!([{"a": 1}, {"b": "hello"}]),
        ),
    ]);
    assert_eq!(get_back("a: hello\nc: 2\n", "/a"), "hello\n");
}

#[test]
fn set_keeps_a_tag_whose_handle_another_document_binds_to_the_core_prefix() {
    check_read(&[(
        "%TAG !e! tag:yaml.org,2002:\n---\na: 1\n...\n\
         %TAG !e! tag:example.com,2000:\n---\nb: !e!x {c: 1}\n",
        "set",
        &["/1/b", "hello"],
        "%TAG !e! tag:yaml.org,2002:\n---\na: 1\n...\n\
         %TAG !e! tag:example.com,2000:\n---\nb: !e!x hello\n",
        serde_json::json!([{"a": 1}, {"b": "hello"}]),
    )]);
}

#[test]
fn del_of_a_compact_first_key_takes_the_comment_lines_inside_its_value() {
    check_read(&[
        (
            "- a:\n    b: 1\n    # c\n  d: 2\n",
            "del",
            &["/0/a"],
            "- d: 2\n",
            serde_json::json!([{"d": 2}]),
        ),
        (
            "- a:\n    # in\n    b: 1\n  d: 2\n",
            "del",
            &["/0/a"],
            "- d: 2\n",
            serde_json::json!([{"d": 2}]),
        ),
        (
            "l:\n  - a:\n      b: 1\n      # c\n    d: 2\n",
            "del",
            &["/l/0/a"],
            "l:\n  - d: 2\n",
            serde_json::json!({"l": [{"d": 2}]}),
        ),
        // The only key: the item is left empty.
        (
            "- a:\n    b: 1\n    # c\n- z: 1\n",
            "del",
            &["/0/a"],
            "- {}\n- z: 1\n",
            serde_json::json!([{}, {"z": 1}]),
        ),
        // A comment at the column of the item's keys is the item's: it
        // stays, after the `-`.
        (
            "- a:\n    b: 1\n  # c\n  d: 2\n",
            "del",
            &["/0/a"],
            "- # c\n  d: 2\n",
            serde_json::json!([{"d": 2}]),
        ),
    ]);
}

#[test]
fn del_at_the_top_of_the_file_leaves_no_blank_line_first() {
    check_read(&[
        (
            "a:\n  b: 1\n  # tail\n\nd: 3\n",
            "del",
            &["/a"],
            "d: 3\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "a: 1\n\nd: 3\n",
            "del",
            &["/a"],
            "d: 3\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "a: 1\n\n\nd: 3\n",
            "del",
            &["/a"],
            "d: 3\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "a: 1\r\n\r\nd: 3\r\n",
            "del",
            &["/a"],
            "d: 3\r\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "\u{feff}a: 1\n\nd: 3\n",
            "del",
            &["/a"],
            "\u{feff}d: 3\n",
            serde_json::json!({"d": 3}),
        ),
        (
            "c: !!str\n\nd: 1\n",
            "del",
            &["/c"],
            "d: 1\n",
            serde_json::json!({"d": 1}),
        ),
        // Not at the top: the blank line stays between what is left.
        (
            "x: 0\na: 1\n\nd: 3\n",
            "del",
            &["/a"],
            "x: 0\n\nd: 3\n",
            serde_json::json!({"x": 0, "d": 3}),
        ),
        (
            "# top\na: 1\n\nd: 3\n",
            "del",
            &["/a"],
            "# top\n\nd: 3\n",
            serde_json::json!({"d": 3}),
        ),
        // Nothing after the blank lines: they stay after the `{}`.
        ("a: 1\n\n", "del", &["/a"], "{}\n\n", serde_json::json!({})),
        (
            "c: !!str\n\n",
            "del",
            &["/c"],
            "{}\n\n",
            serde_json::json!({}),
        ),
    ]);
}

#[test]
fn comment_lines_count_their_columns_past_a_byte_order_mark() {
    check_read(&[
        (
            "\u{feff}a:\n  b: 1\n  # c\nd: 2\n",
            "del",
            &["/a"],
            "\u{feff}d: 2\n",
            serde_json::json!({"d": 2}),
        ),
        (
            "\u{feff}a:\n  b: 1\n  # c\nd: 2\n",
            "set",
            &["/a", "5"],
            "\u{feff}a: 5\nd: 2\n",
            serde_json::json!({"a": 5, "d": 2}),
        ),
        (
            "\u{feff}- a:\n    b: 1\n    # c\n  d: 2\n",
            "del",
            &["/0/a"],
            "\u{feff}- d: 2\n",
            serde_json::json!([{"d": 2}]),
        ),
        (
            "\u{feff}a:\r\n  b: 1\r\n  # c\r\nd: 2\r\n",
            "del",
            &["/a"],
            "\u{feff}d: 2\r\n",
            serde_json::json!({"d": 2}),
        ),
        (
            "\u{feff}a:\r\n  b: 1\r\n  # c\r\nd: 2\r\n",
            "set",
            &["/a", "5"],
            "\u{feff}a: 5\r\nd: 2\r\n",
            serde_json::json!({"a": 5, "d": 2}),
        ),
        (
            "\u{feff}- a:\r\n    b: 1\r\n    # c\r\n  d: 2\r\n",
            "del",
            &["/0/a"],
            "\u{feff}- d: 2\r\n",
            serde_json::json!([{"d": 2}]),
        ),
        // A shallower comment stays, as without the mark.
        (
            "\u{feff}a:\n  b: 1\n# c\nd: 2\n",
            "set",
            &["/a", "5"],
            "\u{feff}a: 5\n# c\nd: 2\n",
            serde_json::json!({"a": 5, "d": 2}),
        ),
        (
            "\u{feff}a:\n- 1\n# c\nb: 2\n",
            "set",
            &["/a", "5"],
            "\u{feff}a: 5\n# c\nb: 2\n",
            serde_json::json!({"a": 5, "b": 2}),
        ),
    ]);
}

#[test]
fn a_tag_directive_binds_its_handle_for_its_own_document() {
    check_read(&[
        // `!e!` is the core prefix in the second document only.
        (
            "%TAG !e! tag:example.com,2000:\n---\na: !e!map {c: 1}\n...\n\
             %TAG !e! tag:yaml.org,2002:\n---\nb: 1\n",
            "set",
            &["/0/a", "hello"],
            "%TAG !e! tag:example.com,2000:\n---\na: !e!map hello\n...\n\
             %TAG !e! tag:yaml.org,2002:\n---\nb: 1\n",
            serde_json::json!([{"a": "hello"}, {"b": 1}]),
        ),
        // The primary handle bound to the core prefix.
        (
            "%TAG ! tag:yaml.org,2002:\n---\na: !map {c: 1}\n",
            "set",
            &["/a", "hello"],
            "%TAG ! tag:yaml.org,2002:\n---\na: hello\n",
            serde_json::json!({"a": "hello"}),
        ),
        // `!!` bound elsewhere: `!!str` is not the core `str`, and stays.
        (
            "%TAG !! tag:x,1:\n---\na: !!str {c: 1}\nb: 2\n",
            "set",
            &["/a", "hello"],
            "%TAG !! tag:x,1:\n---\na: !!str hello\nb: 2\n",
            serde_json::json!({"a": "hello", "b": 2}),
        ),
        (
            "%TAG !! tag:x,1:\r\n---\r\na: !!str {c: 1}\r\n",
            "set",
            &["/a", "hello"],
            "%TAG !! tag:x,1:\r\n---\r\na: !!str hello\r\n",
            serde_json::json!({"a": "hello"}),
        ),
        // ...for the one document after it: in the next, `!!` is core again.
        (
            "%TAG !! tag:x,1:\n---\na: 1\n---\nb: !!map {c: 1}\n",
            "set",
            &["/1/b", "7"],
            "%TAG !! tag:x,1:\n---\na: 1\n---\nb: 7\n",
            serde_json::json!([{"a": 1}, {"b": 7}]),
        ),
        // A rebound `!!str` is an application's tag: dq reads the scalar
        // under it as untagged, as under `!foo`, so a kept tag over a
        // number reads back as the number.
        (
            "%TAG !! tag:x,1:\n---\na: !!str 5\nb: 2\n",
            "set",
            &["/a", "7"],
            "%TAG !! tag:x,1:\n---\na: !!str 7\nb: 2\n",
            serde_json::json!({"a": 7, "b": 2}),
        ),
        (
            "%TAG !! tag:x,1:\n---\na: !!map\n  c: 1\nb: 2\n",
            "set",
            &["/a", "7"],
            "%TAG !! tag:x,1:\n---\na: !!map 7\nb: 2\n",
            serde_json::json!({"a": 7, "b": 2}),
        ),
    ]);
}

#[test]
fn a_tag_on_the_document_start_line() {
    check_read(&[
        // Edits inside the root collection keep its tag: it stays a map.
        (
            "--- !!map {a: 1}\n",
            "set",
            &["/a", "5"],
            "--- !!map {a: 5}\n",
            serde_json::json!({"a": 5}),
        ),
        (
            "--- !!map {a: 1}\n",
            "del",
            &["/a"],
            "--- !!map {}\n",
            serde_json::json!({}),
        ),
        (
            "--- !!map\na: 1\n",
            "set",
            &["/b", "2"],
            "--- !!map\na: 1\nb: 2\n",
            serde_json::json!({"a": 1, "b": 2}),
        ),
        // A document of a stream is written anew as a whole: a tag that
        // no longer fits its value goes, one that does stays.
        (
            "--- !!map {a: 1}\n--- !!map {b: 1}\n",
            "set",
            &["/1", "5"],
            "--- !!map {a: 1}\n---\n5\n",
            serde_json::json!([{"a": 1}, 5]),
        ),
        (
            "--- !!map\na: 1\n--- !!map\nb: 1\n",
            "set",
            &["/1", "{\"c\": 2}"],
            "--- !!map\na: 1\n--- !!map\nc: 2\n",
            serde_json::json!([{"a": 1}, {"c": 2}]),
        ),
        (
            "--- {a: 1}\n--- {b: 1}\n",
            "set",
            &["/1", "5"],
            "--- {a: 1}\n---\n5\n",
            serde_json::json!([{"a": 1}, 5]),
        ),
    ]);
}

#[test]
fn set_in_an_empty_file_writes_the_first_key() {
    for (src, args, want) in [
        ("", &["/a", "1"][..], "a: 1\n"),
        ("", &["", "{\"a\": 1}"][..], "a: 1\n"),
        (
            "# only a comment\n",
            &["/a", "1"][..],
            "# only a comment\na: 1\n",
        ),
        ("---\n", &["/a", "1"][..], "---\na: 1\n"),
    ] {
        let out = in_place(src, "set", args);
        assert_eq!(out, want, "set {args:?} on {src:?}");
        assert_eq!(read_back(&out), serde_json::json!({"a": 1}));
    }
}
