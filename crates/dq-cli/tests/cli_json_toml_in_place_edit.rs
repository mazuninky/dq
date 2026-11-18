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
                "a = {}\n[x]\ny = 1\n",
            ),
        ],
    );
}
