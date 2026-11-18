//! `dq -i set` / `dq -i del` on JSON and TOML through the real binary:
//! nested JSON collections, TOML inline tables and arrays, dotted keys,
//! keys before the first table, an empty file, and files with an empty key.

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

/// Run each `(src, command, args, want)` case on a `doc.<ext>` file with
/// `-i`: exit 0, empty stdout, the file's bytes become `want`.
fn check(ext: &str, cases: &[(&str, &str, &[&str], &str)]) {
    for (src, command, args, want) in cases {
        let dir = TempDir::new().expect("tempdir");
        let name = format!("doc.{ext}");
        let path = dir.path().join(&name);
        std::fs::write(&path, src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["-i", "--no-color", command, &name])
            .args(*args)
            .assert()
            .success()
            .stdout("");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read back"),
            *want,
            "{command} {args:?} on {src:?}"
        );
    }
}

const PRETTY: &str =
    "{\n  \"a\": {\n    \"x\": [\n      1\n    ]\n  },\n  \"b\": [\n    1,\n    2\n  ]\n}\n";

#[test]
fn json_del_of_a_nested_object_or_array() {
    check(
        "json",
        &[
            (
                r#"{"a": {"x": 1}, "b": [1, 2]}"#,
                "del",
                &["/a"],
                r#"{"b": [1, 2]}"#,
            ),
            // The last member takes the comma before it.
            (
                r#"{"a": {"x": 1}, "b": [1, 2]}"#,
                "del",
                &["/b"],
                r#"{"a": {"x": 1}}"#,
            ),
            (
                PRETTY,
                "del",
                &["/a"],
                "{\n  \"b\": [\n    1,\n    2\n  ]\n}\n",
            ),
            (
                PRETTY,
                "del",
                &["/b"],
                "{\n  \"a\": {\n    \"x\": [\n      1\n    ]\n  }\n}\n",
            ),
            (
                PRETTY,
                "del",
                &["/a/x"],
                "{\n  \"a\": {},\n  \"b\": [\n    1,\n    2\n  ]\n}\n",
            ),
            (
                "{\r\n  \"a\": 1,\r\n  \"b\": {\r\n    \"x\": 1\r\n  }\r\n}\r\n",
                "del",
                &["/b"],
                "{\r\n  \"a\": 1\r\n}\r\n",
            ),
            // Escaped quotes and backslashes in the key.
            (r#"{"q\"x": 1, "b": 2}"#, "del", &["/q\"x"], r#"{"b": 2}"#),
            (r#"{"b": 2, "q\\": 1}"#, "del", &["/q\\"], r#"{"b": 2}"#),
        ],
    );
}

#[test]
fn toml_inline_tables_and_arrays() {
    check(
        "toml",
        &[
            (
                "a = {x = 1, y = [1, 2]}\n",
                "del",
                &["/a/y"],
                "a = {x = 1}\n",
            ),
            (
                "a = {x = 1, y = [1, 2]}\n",
                "set",
                &["/a/y/0", "9"],
                "a = {x = 1, y = [9, 2]}\n",
            ),
            (
                "a = {x = 1}\n",
                "set",
                &["/a/z", "{\"q\": 1}"],
                "a = {x = 1, z = { q = 1 }}\n",
            ),
            ("a = {}\n", "set", &["/a/z", "1"], "a = { z = 1 }\n"),
            (
                "a = {x = 1}\r\n",
                "set",
                &["/a/z", "3"],
                "a = {x = 1, z = 3}\r\n",
            ),
            // A dotted key with a quoted, spaced segment inside the table.
            (
                "a = {b = 2, x . \"y z\" . w = 1}\n",
                "del",
                &["/a/x/y z/w"],
                "a = {b = 2, x . \"y z\" = {}}\n",
            ),
        ],
    );
}

#[test]
fn toml_dotted_keys_root_keys_and_an_empty_file() {
    check(
        "toml",
        &[
            (
                "p.q = 1\np.r = 2\n",
                "set",
                &["/p/s", "3"],
                "p.q = 1\np.r = 2\np.s = 3\n",
            ),
            ("p.q = 1\n", "del", &["/p/q"], "p = {}\n"),
            // A new root key goes before the first table header.
            (
                "[t]\nx = 1\n",
                "set",
                &["/top", "1"],
                "top = 1\n[t]\nx = 1\n",
            ),
            (
                "a = 1\r\n[t]\r\nx = 1\r\n",
                "set",
                &["/c", "3"],
                "a = 1\r\nc = 3\r\n[t]\r\nx = 1\r\n",
            ),
            ("", "set", &["/a", "1"], "a = 1\n"),
            (
                "",
                "set",
                &["/a", "{\"b\": [1, {\"c\": 2}]}"],
                "a = { b = [1, { c = 2 }] }\n",
            ),
        ],
    );
}

#[test]
fn an_empty_key_does_not_get_in_the_way() {
    check(
        "json",
        &[(r#"{"": 1, "b": 2}"#, "del", &["/b"], r#"{"": 1}"#)],
    );
    check(
        "toml",
        &[("\"\" = 1\nb = 2\n", "del", &["/b"], "\"\" = 1\n")],
    );
    for (name, src) in [
        ("doc.json", r#"{"": 1, "b": 2}"#),
        ("doc.toml", "\"\" = 1\nb = 2\n"),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["--no-color", "lint", name])
            .assert()
            .code(0);
        // dq pointers have no empty segment: `/` is not the empty key.
        dq().current_dir(dir.path())
            .args(["-i", "--no-color", "del", name, "/"])
            .assert()
            .code(2)
            .stdout("");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), src);
    }
}

#[test]
fn json_set_over_a_nested_object_replaces_it() {
    check(
        "json",
        &[
            (
                "{\n  \"a\": {\n    \"b\": {\"c\": [1, 2]},\n    \"d\": 1\n  },\n  \"e\": 2\n}\n",
                "set",
                &["/a", "5"],
                "{\n  \"a\": 5,\n  \"e\": 2\n}\n",
            ),
            (
                r#"{"a": {"b": {"c": 1}}}"#,
                "set",
                &["/a/b", "true"],
                r#"{"a": {"b": true}}"#,
            ),
        ],
    );
}

#[test]
fn toml_set_over_a_table_takes_its_subtables_with_it() {
    check(
        "toml",
        &[
            // The subtable comes after another table; the new key goes
            // before the first header, where it stays a root key.
            (
                "[a]\nb = 1\n[x]\ny = 1\n[a.c]\nd = 1\n",
                "set",
                &["/a", "5"],
                "a = 5\n[x]\ny = 1\n",
            ),
            (
                "[x]\ny = 1\n[a]\nb = 1\n",
                "set",
                &["/a", "{\"n\": 1}"],
                "a = { n = 1 }\n[x]\ny = 1\n",
            ),
            (
                "top = 1\n[x]\ny = 1\n[[arr]]\nx = 1\n[[arr]]\nx = 2\n",
                "set",
                &["/arr", "5"],
                "top = 1\narr = 5\n[x]\ny = 1\n",
            ),
        ],
    );
}

#[test]
fn toml_del_of_an_array_of_tables_item_with_a_subtable() {
    check(
        "toml",
        &[
            (
                "[[arr]]\nx = 1\n[arr.sub]\ny = 1\n[[arr]]\nx = 2\n",
                "del",
                &["/arr/0"],
                "[[arr]]\nx = 2\n",
            ),
            // The last item leaves an empty array, as a root key.
            (
                "[[arr]]\nx = 1\n[arr.sub]\ny = 1\n",
                "del",
                &["/arr/0"],
                "arr = []\n",
            ),
            (
                "[t]\nk = 1\n[[arr]]\nx = 1\n[arr.sub]\ny = 1\n",
                "del",
                &["/arr/0"],
                "arr = []\n[t]\nk = 1\n",
            ),
            (
                "[x]\ny = 1\n[a.b]\nk = 1\n",
                "del",
                &["/a/b"],
                // A header of its own where the removed section was.
                "[x]\ny = 1\n[a]\n",
            ),
        ],
    );
}

#[test]
fn toml_set_over_a_table_whose_parent_only_it_makes() {
    // The parent has no header of its own: the new value goes in as an
    // inline table for the parent, where the header was.
    check(
        "toml",
        &[
            (
                "x = 1\n[a.b]\nc = 1\n",
                "set",
                &["/a/b", "5"],
                "x = 1\na = { b = 5 }\n",
            ),
            (
                "x = 1\n[a.b]\nc = 1\n",
                "set",
                &["/a/b", "{\"z\":1}"],
                "x = 1\na = { b = { z = 1 } }\n",
            ),
            (
                "x = 1\r\n[a.b]\r\nc = 1\r\n",
                "set",
                &["/a/b", "5"],
                "x = 1\r\na = { b = 5 }\r\n",
            ),
            (
                "a.b.c = 1\ne = 1\n",
                "set",
                &["/a/b", "5"],
                // Where its dotted line was, under the key cut down to it.
                "a.b = 5\ne = 1\n",
            ),
            (
                "[q]\n[q.a.b]\nc = 1\n",
                "set",
                &["/q/a/b", "5"],
                "[q]\na = { b = 5 }\n",
            ),
            // An `[[arr]]` item whose only section is a subtable.
            (
                "[[arr]]\n[arr.s]\nk = 1\n",
                "set",
                &["/arr/0/s", "5"],
                "[[arr]]\ns = 5\n",
            ),
        ],
    );
}

#[test]
fn a_new_key_next_to_an_only_empty_key() {
    check(
        "json",
        &[
            (
                r#"{"a": {"": 2}}"#,
                "set",
                &["/a/zzq", "1"],
                r#"{"a": {"": 2, "zzq": 1}}"#,
            ),
            (r#"{"": 2}"#, "set", &["/zzq", "1"], r#"{"": 2, "zzq": 1}"#),
        ],
    );
    check(
        "toml",
        &[
            (
                "[a]\n\"\" = 2\n",
                "set",
                &["/a/zzq", "1"],
                "[a]\n\"\" = 2\nzzq = 1\n",
            ),
            ("\"\" = 2\n", "set", &["/zzq", "1"], "\"\" = 2\nzzq = 1\n"),
        ],
    );
    check(
        "yaml",
        &[
            (
                "a:\n  \"\": 2\n",
                "set",
                &["/a/zzq", "1"],
                "a:\n  \"\": 2\n  zzq: 1\n",
            ),
            ("\"\": 2\n", "set", &["/zzq", "1"], "\"\": 2\nzzq: 1\n"),
        ],
    );
}

/// `bytes` as `dq -F json convert doc.<ext>` reads them.
fn read_back(ext: &str, bytes: &str) -> serde_json::Value {
    let dir = TempDir::new().expect("tempdir");
    let name = format!("doc.{ext}");
    std::fs::write(dir.path().join(&name), bytes).expect("write");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "-F", "json", "convert", &name])
        .assert()
        .success();
    serde_json::from_slice(&out.get_output().stdout).expect("stdout is JSON")
}

/// The stdout of `dq get doc.<ext> <pointer>` with `bytes` as the file.
fn get_back(ext: &str, bytes: &str, pointer: &str) -> String {
    let dir = TempDir::new().expect("tempdir");
    let name = format!("doc.{ext}");
    std::fs::write(dir.path().join(&name), bytes).expect("write");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "get", &name, pointer])
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).expect("UTF-8 stdout")
}

/// [`check`], and each file then reads back as `value`.
fn check_read(ext: &str, cases: &[(&str, &str, &[&str], &str, serde_json::Value)]) {
    for (src, command, args, want, value) in cases {
        check(ext, &[(*src, *command, *args, *want)]);
        assert_eq!(
            read_back(ext, want),
            *value,
            "{command} {args:?} on {src:?}"
        );
    }
}

#[test]
fn toml_del_of_the_last_table_under_a_dotted_key_respells_the_parent_in_place() {
    check_read(
        "toml",
        &[
            (
                "a.b.c = 1\na.b.d = 2\ne = 1\n",
                "del",
                &["/a/b"],
                "a = {}\ne = 1\n",
                serde_json::json!({"a": {}, "e": 1}),
            ),
            (
                "a.b.c = 1\r\na.b.d = 2\r\ne = 1\r\n",
                "del",
                &["/a/b"],
                "a = {}\r\ne = 1\r\n",
                serde_json::json!({"a": {}, "e": 1}),
            ),
            // A quoted key with a dot in it is one part.
            (
                "\"a.b\".c.d = 1\ne = 1\n",
                "del",
                &["/a.b/c"],
                "\"a.b\" = {}\ne = 1\n",
                serde_json::json!({"a.b": {}, "e": 1}),
            ),
            // A removed section: a header of its own where it was.
            (
                "[x]\r\ny = 1\r\n[a.b]\r\nk = 1\r\n",
                "del",
                &["/a/b"],
                "[x]\r\ny = 1\r\n[a]\r\n",
                serde_json::json!({"x": {"y": 1}, "a": {}}),
            ),
            // A sibling left: nothing to respell.
            (
                "a.b.c = 1\na.x = 2\ne = 1\n",
                "del",
                &["/a/b"],
                "a.x = 2\ne = 1\n",
                serde_json::json!({"a": {"x": 2}, "e": 1}),
            ),
        ],
    );
}

#[test]
fn toml_set_over_a_dotted_table_writes_it_where_its_first_line_was() {
    check_read(
        "toml",
        &[
            (
                "a.b.c = 1\na.b.d = 2\ne = 1\n",
                "set",
                &["/a/b", "5"],
                "a.b = 5\ne = 1\n",
                serde_json::json!({"a": {"b": 5}, "e": 1}),
            ),
            (
                "a.b.c = 1\r\na.b.d = 2\r\ne = 1\r\n",
                "set",
                &["/a/b", "5"],
                "a.b = 5\r\ne = 1\r\n",
                serde_json::json!({"a": {"b": 5}, "e": 1}),
            ),
        ],
    );
}

#[test]
fn toml_new_key_in_a_table_with_no_header_of_its_own() {
    check_read(
        "toml",
        &[
            // Made by the headers of the tables under it: a header of its
            // own before the first of them.
            (
                "[a.d]\nx = 1\n",
                "set",
                &["/a/z", "3"],
                "[a]\nz = 3\n\n[a.d]\nx = 1\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}}}),
            ),
            (
                "[a.b]\nx=1\n[a.c]\ny=1\n",
                "set",
                &["/a/z", "3"],
                "[a]\nz = 3\n\n[a.b]\nx=1\n[a.c]\ny=1\n",
                serde_json::json!({"a": {"z": 3, "b": {"x": 1}, "c": {"y": 1}}}),
            ),
            (
                "q = 1\n[a.d]\nx = 1\n[b]\ny = 1\n",
                "set",
                &["/a/z", "3"],
                "q = 1\n[a]\nz = 3\n\n[a.d]\nx = 1\n[b]\ny = 1\n",
                serde_json::json!({"q": 1, "a": {"z": 3, "d": {"x": 1}}, "b": {"y": 1}}),
            ),
            (
                "[a.d]\nx = 1\n",
                "set",
                &["/a/z", "{\"k\":1}"],
                "[a]\nz = { k = 1 }\n\n[a.d]\nx = 1\n",
                serde_json::json!({"a": {"z": {"k": 1}, "d": {"x": 1}}}),
            ),
            (
                "[a.d]\r\nx = 1\r\n",
                "set",
                &["/a/z", "3"],
                "[a]\r\nz = 3\r\n\r\n[a.d]\r\nx = 1\r\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}}}),
            ),
            (
                "[a.b]\r\nx=1\r\n[a.c]\r\ny=1\r\n",
                "set",
                &["/a/z", "3"],
                "[a]\r\nz = 3\r\n\r\n[a.b]\r\nx=1\r\n[a.c]\r\ny=1\r\n",
                serde_json::json!({"a": {"z": 3, "b": {"x": 1}, "c": {"y": 1}}}),
            ),
            // Made by dotted keys: under the same key, after the last of
            // its lines.
            (
                "a.d.x = 1\n",
                "set",
                &["/a/z", "3"],
                "a.d.x = 1\na.z = 3\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}}}),
            ),
            (
                "a.d.x = 1\r\n",
                "set",
                &["/a/z", "3"],
                "a.d.x = 1\r\na.z = 3\r\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}}}),
            ),
            (
                "a.d.x = 1",
                "set",
                &["/a/z", "3"],
                "a.d.x = 1\na.z = 3\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}}}),
            ),
            (
                "a.d.x = 1 # c\n",
                "set",
                &["/a/z", "3"],
                "a.d.x = 1 # c\na.z = 3\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}}}),
            ),
            (
                "a.d.x = 1\na.e.y = 2\nq = 1\n",
                "set",
                &["/a/z", "3"],
                "a.d.x = 1\na.e.y = 2\na.z = 3\nq = 1\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}, "e": {"y": 2}}, "q": 1}),
            ),
            // The key's own spelling and indentation are kept.
            (
                "  a . d.x = 1\n",
                "set",
                &["/a/z", "3"],
                "  a . d.x = 1\n  a.z = 3\n",
                serde_json::json!({"a": {"z": 3, "d": {"x": 1}}}),
            ),
            (
                "[s]\na.d.x = 1\n[s.q]\nk = 1\n",
                "set",
                &["/s/a/z", "3"],
                "[s]\na.d.x = 1\na.z = 3\n[s.q]\nk = 1\n",
                serde_json::json!({"s": {"a": {"z": 3, "d": {"x": 1}}, "q": {"k": 1}}}),
            ),
            (
                "[[t]]\na.d.x = 1\n",
                "set",
                &["/t/0/a/z", "3"],
                "[[t]]\na.d.x = 1\na.z = 3\n",
                serde_json::json!({"t": [{"a": {"z": 3, "d": {"x": 1}}}]}),
            ),
            // A quoted key with a dot in it is one part.
            (
                "\"a.b\".c.d = 1\n",
                "set",
                &["/a.b/z", "3"],
                "\"a.b\".c.d = 1\n\"a.b\".z = 3\n",
                serde_json::json!({"a.b": {"z": 3, "c": {"d": 1}}}),
            ),
            (
                "'a.b'.c.d = 1\n",
                "set",
                &["/a.b/z", "3"],
                "'a.b'.c.d = 1\n'a.b'.z = 3\n",
                serde_json::json!({"a.b": {"z": 3, "c": {"d": 1}}}),
            ),
            (
                "\"a.b\".c = 1\n",
                "set",
                &["/a.b/z", "3"],
                "\"a.b\".c = 1\n\"a.b\".z = 3\n",
                serde_json::json!({"a.b": {"z": 3, "c": 1}}),
            ),
        ],
    );
    assert_eq!(
        get_back("toml", "[a]\nz = 3\n\n[a.d]\nx = 1\n", "/a/z"),
        "3\n"
    );
    assert_eq!(get_back("toml", "a.d.x = 1\na.z = 3\n", "/a/z"), "3\n");
}

#[test]
fn toml_set_over_a_table_under_a_parent_with_no_header() {
    // A header `[a]` of its own takes the new value.
    check_read(
        "toml",
        &[(
            "[a.b]\nc = 1\n[a.d]\ne = 1\n",
            "set",
            &["/a/b", "5"],
            "[a]\nb = 5\n\n[a.d]\ne = 1\n",
            serde_json::json!({"a": {"b": 5, "d": {"e": 1}}}),
        )],
    );
    // Under an `[[t]]` item, among that item's sections: a header `[t.a]`
    // names the item opened last above it, the first one here.
    check_read(
        "toml",
        &[(
            "[[t]]\n[t.a.b]\nc = 1\n[t.a.d]\ne = 1\n[[t]]\n",
            "set",
            &["/t/0/a/b", "5"],
            "[[t]]\n[t.a]\nb = 5\n\n[t.a.d]\ne = 1\n[[t]]\n",
            serde_json::json!({"t": [{"a": {"b": 5, "d": {"e": 1}}}, {}]}),
        )],
    );
}

#[test]
fn toml_new_key_under_an_array_of_tables_item_with_no_header() {
    // A `[t.a]` header names the `[[t]]` item opened last above it, so it
    // goes among the item's own sections — the first item's too.
    check_read(
        "toml",
        &[
            (
                "[[t]]\n[t.a.d]\nx = 1\n",
                "set",
                &["/t/0/a/z", "3"],
                "[[t]]\n[t.a]\nz = 3\n\n[t.a.d]\nx = 1\n",
                serde_json::json!({"t": [{"a": {"z": 3, "d": {"x": 1}}}]}),
            ),
            (
                "[[t]]\n[t.a.d]\nx = 1\n[[t]]\ny = 2\n",
                "set",
                &["/t/0/a/z", "3"],
                "[[t]]\n[t.a]\nz = 3\n\n[t.a.d]\nx = 1\n[[t]]\ny = 2\n",
                serde_json::json!({"t": [{"a": {"z": 3, "d": {"x": 1}}}, {"y": 2}]}),
            ),
        ],
    );
}

#[test]
fn set_of_negative_zero_keeps_its_sign() {
    for (ext, src, want) in [
        ("toml", "x = 0.0\n", "x = -0.0\n"),
        ("json", "{\"x\": 0.0}\n", "{\"x\": -0.0}\n"),
        ("toml", "a = {x = 1}\n", "a = {x = 1, y = -0.0}\n"),
        ("json", "{\"x\": [1]}\n", "{\"x\": -0.0}\n"),
    ] {
        let pointer = if src.starts_with("a =") { "/a/y" } else { "/x" };
        check(ext, &[(src, "set", &[pointer, "--", "-0.0"], want)]);
        assert_eq!(get_back(ext, want, pointer), "-0.0\n", "{ext}: {src:?}");
    }
}

#[test]
fn toml_new_key_in_a_headerless_table_whatever_its_keys_spell() {
    check_read(
        "toml",
        &[
            // A dotted key under `[a.d]` does not reach up to `a`: a header.
            (
                "[a.d]\ne.f = 1\n",
                "set",
                &["/a/z", "3"],
                "[a]\nz = 3\n\n[a.d]\ne.f = 1\n",
                serde_json::json!({"a": {"z": 3, "d": {"e": {"f": 1}}}}),
            ),
            (
                "[[a.d]]\nx.y = 1\n",
                "set",
                &["/a/z", "3"],
                "[a]\nz = 3\n\n[[a.d]]\nx.y = 1\n",
                serde_json::json!({"a": {"z": 3, "d": [{"x": {"y": 1}}]}}),
            ),
            // Escapes and an empty part in a quoted key.
            (
                "a.\"b\\u0063\".d = 1\n",
                "set",
                &["/a/z", "3"],
                "a.\"b\\u0063\".d = 1\na.z = 3\n",
                serde_json::json!({"a": {"z": 3, "bc": {"d": 1}}}),
            ),
            (
                "a.\"b\\\"c\".d = 1\n",
                "set",
                &["/a/z", "3"],
                "a.\"b\\\"c\".d = 1\na.z = 3\n",
                serde_json::json!({"a": {"z": 3, "b\"c": {"d": 1}}}),
            ),
            (
                "a.\"\".x = 1\n",
                "set",
                &["/a/z", "3"],
                "a.\"\".x = 1\na.z = 3\n",
                serde_json::json!({"a": {"z": 3, "": {"x": 1}}}),
            ),
        ],
    );
}

#[test]
fn toml_new_key_in_a_table_spelled_with_an_escape_is_refused() {
    // `/a/bc` is spelled `a."bc"`: refused, the file untouched.
    let src = "a.\"b\\u0063\".d = 1\n";
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.toml");
    std::fs::write(&path, src).expect("write fixture");
    let out = dq()
        .current_dir(dir.path())
        .args(["-i", "--no-color", "set", "doc.toml", "/a/bc/z", "3"])
        .assert()
        .code(7)
        .stdout("");
    let err = String::from_utf8_lossy(&out.get_output().stderr).into_owned();
    assert!(err.contains("cannot set '/a/bc/z'"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).expect("read back"), src);
}

#[test]
fn toml_new_key_after_a_byte_order_mark() {
    let tree = || serde_json::json!({"a": {"z": 3, "d": {"x": 1}}});
    check_read(
        "toml",
        &[
            (
                "\u{feff}[a.d]\nx=1\n",
                "set",
                &["/a/z", "3"],
                "\u{feff}[a]\nz = 3\n\n[a.d]\nx=1\n",
                tree(),
            ),
            (
                "\u{feff}a.d.x=1\n",
                "set",
                &["/a/z", "3"],
                "\u{feff}a.d.x=1\na.z = 3\n",
                tree(),
            ),
            // The comment line above `[a.d]` is its: the new header goes
            // before it, still after the mark.
            (
                "\u{feff}# c\n[a.d]\nx=1\n",
                "set",
                &["/a/z", "3"],
                "\u{feff}[a]\nz = 3\n\n# c\n[a.d]\nx=1\n",
                tree(),
            ),
            (
                "\u{feff}[a.d]\r\nx=1\r\n",
                "set",
                &["/a/z", "3"],
                "\u{feff}[a]\r\nz = 3\r\n\r\n[a.d]\r\nx=1\r\n",
                tree(),
            ),
            (
                "\u{feff}a.d.x=1\r\n",
                "set",
                &["/a/z", "3"],
                "\u{feff}a.d.x=1\r\na.z = 3\r\n",
                tree(),
            ),
            (
                "\u{feff}# c\r\n[a.d]\r\nx=1\r\n",
                "set",
                &["/a/z", "3"],
                "\u{feff}[a]\r\nz = 3\r\n\r\n# c\r\n[a.d]\r\nx=1\r\n",
                tree(),
            ),
        ],
    );
}

#[test]
fn toml_new_key_goes_to_the_array_of_tables_item_it_names() {
    const TWO: &str = "[[t]]\nk = 0\n[t.a.d]\nx = 1\n[[t]]\nk = 1\n[t.a.d]\nx = 2\n";
    check_read(
        "toml",
        &[
            (
                TWO,
                "set",
                &["/t/0/a/z", "3"],
                "[[t]]\nk = 0\n[t.a]\nz = 3\n\n[t.a.d]\nx = 1\n[[t]]\nk = 1\n[t.a.d]\nx = 2\n",
                serde_json::json!({"t": [
                    {"k": 0, "a": {"z": 3, "d": {"x": 1}}},
                    {"k": 1, "a": {"d": {"x": 2}}},
                ]}),
            ),
            (
                TWO,
                "set",
                &["/t/1/a/z", "3"],
                "[[t]]\nk = 0\n[t.a.d]\nx = 1\n[[t]]\nk = 1\n[t.a]\nz = 3\n\n[t.a.d]\nx = 2\n",
                serde_json::json!({"t": [
                    {"k": 0, "a": {"d": {"x": 1}}},
                    {"k": 1, "a": {"z": 3, "d": {"x": 2}}},
                ]}),
            ),
            // Only some items have `[t.a.*]`.
            (
                "[[t]]\nk = 0\n[[t]]\nk = 1\n[t.a.d]\nx = 2\n",
                "set",
                &["/t/1/a/z", "3"],
                "[[t]]\nk = 0\n[[t]]\nk = 1\n[t.a]\nz = 3\n\n[t.a.d]\nx = 2\n",
                serde_json::json!({"t": [{"k": 0}, {"k": 1, "a": {"z": 3, "d": {"x": 2}}}]}),
            ),
            (
                "[[t]]\nk = 0\n[t.a.d]\nx = 1\n[[t]]\nk = 1\n[[t]]\nk = 2\n[t.a.e]\ny = 1\n",
                "set",
                &["/t/2/a/z", "3"],
                "[[t]]\nk = 0\n[t.a.d]\nx = 1\n[[t]]\nk = 1\n[[t]]\nk = 2\n[t.a]\nz = 3\n\n\
                 [t.a.e]\ny = 1\n",
                serde_json::json!({"t": [
                    {"k": 0, "a": {"d": {"x": 1}}},
                    {"k": 1},
                    {"k": 2, "a": {"z": 3, "e": {"y": 1}}},
                ]}),
            ),
        ],
    );
}

#[test]
fn toml_del_keeps_the_byte_order_mark_and_the_line_endings() {
    check_read(
        "toml",
        &[
            (
                "\u{feff}a=1\r\n\r\nb=2\r\n",
                "del",
                &["/a"],
                "\u{feff}b=2\r\n",
                serde_json::json!({"b": 2}),
            ),
            (
                "\u{feff}a=1\nb=2\n",
                "del",
                &["/a"],
                "\u{feff}b=2\n",
                serde_json::json!({"b": 2}),
            ),
            (
                "\u{feff}a=1\n",
                "del",
                &["/a"],
                "\u{feff}",
                serde_json::json!({}),
            ),
            // The header written in place of the removed section.
            (
                "\u{feff}[a.b]\r\nc=1\r\n",
                "del",
                &["/a/b"],
                "\u{feff}[a]\r\n",
                serde_json::json!({"a": {}}),
            ),
            (
                "[a.b]\r\nc=1\r\n",
                "del",
                &["/a/b"],
                "[a]\r\n",
                serde_json::json!({"a": {}}),
            ),
            (
                "\u{feff}a.b.c=1\r\ne=1\r\n",
                "del",
                &["/a/b"],
                "\u{feff}a = {}\r\ne=1\r\n",
                serde_json::json!({"a": {}, "e": 1}),
            ),
        ],
    );
}
