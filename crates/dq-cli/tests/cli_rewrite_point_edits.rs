//! Pointer edits (`set`, `del`, `patch`, `merge`) on XML, HCL, INI, dotenv
//! and CSV/TSV files. These formats have no textual edits, so the change is
//! made to the value tree and the whole file is written anew through the
//! format's writer — the way `set --jq` writes them: layout is not kept,
//! nor are comments but XML's.
//!
//! Every such edit used to be refused, exit 7 ("<format> has no textual
//! edits: dq can read a <format> file but cannot set a value in it"),
//! though README lists the formats as writable and `set --jq` changed the
//! same files. A tree the writer cannot hold is refused with the writer's
//! own error, exit 1, and the file is left alone.

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

const XML: &str = "<?xml version=\"1.0\"?>\n<!-- c -->\n<project>\n  <version>1.0</version>\n  <name>app</name>\n</project>\n";
const TF: &str = "# c\nregion = \"us\"\nresource \"r\" \"n\" {\n  a = 1\n}\nresource \"r\" \"m\" {\n  a = 2\n}\n";
const INI: &str = "; c\n[server]\nhost = a\nport = 80\n";
const ENV: &str = "# c\nA=1\nB=\"x y\"\n";
const CSV: &str = "n,a\nx,1\ny,2\n";
const TSV: &str = "n\ta\nx\t1\n";

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

#[test]
fn set_rewrites_each_format_through_its_writer() {
    edits_to(
        "pom.xml",
        XML,
        &["set", "pom.xml", "/project/0/version/0/#text", "2.0"],
        // The declaration and the comment are in the tree; the layout is the
        // writer's, ending with one line break.
        "<?xml version=\"1.0\"?>\n<!-- c -->\n<project>\n  <version>2.0</version>\n  <name>app</name>\n</project>\n",
    );
    edits_to(
        "main.tf",
        TF,
        &["set", "main.tf", "/resource/r/n/a", "5"],
        "region = \"us\"\nresource \"r\" \"n\" {\n  a = 5\n}\nresource \"r\" \"m\" {\n  a = 2\n}\n",
    );
    edits_to(
        "app.ini",
        INI,
        &["set", "app.ini", "/server/port", "8080"],
        "[server]\nhost=a\nport=8080\n",
    );
    edits_to(".env", ENV, &["set", ".env", "/A", "2"], "A=2\nB=\"x y\"\n");
    edits_to(
        "t.csv",
        CSV,
        &["set", "t.csv", "/1/a", "3"],
        "n,a\nx,1\ny,3\n",
    );
    edits_to("t.tsv", TSV, &["set", "t.tsv", "/0/a", "3"], "n\ta\nx\t3\n");
}

#[test]
fn set_creates_missing_keys_and_appends_as_on_textual_formats() {
    edits_to(
        "app.ini",
        INI,
        &["set", "app.ini", "/client/retries", "3"],
        "[server]\nhost=a\nport=80\n\n[client]\nretries=3\n",
    );
    edits_to(
        "t.csv",
        CSV,
        &["set", "t.csv", "/-", r#"{"n": "z", "a": 9}"#],
        "n,a\nx,1\ny,2\nz,9\n",
    );
    // A body appended to repeated HCL blocks is one more block.
    edits_to(
        "sg.tf",
        "ingress {\n  p = 1\n}\ningress {\n  p = 2\n}\n",
        &["set", "sg.tf", "/ingress/-", r#"{"p": 3}"#],
        "ingress {\n  p = 1\n}\ningress {\n  p = 2\n}\ningress {\n  p = 3\n}\n",
    );
    // `--value-string` and the JSON-literal heuristic are the same as for
    // YAML: `08` is a string either way.
    edits_to(
        ".env",
        ENV,
        &["set", ".env", "/C", "08"],
        "A=1\nB=\"x y\"\nC=08\n",
    );
}

#[test]
fn set_prints_the_rewritten_file_without_touching_it() {
    assert_eq!(
        run("app.ini", INI, &["set", "app.ini", "/server/port", "8080"]),
        Outcome {
            code: 0,
            stdout: "[server]\nhost=a\nport=8080\n".to_owned(),
            stderr: String::new(),
            file: INI.to_owned(),
        }
    );
}

#[test]
fn del_rewrites_each_format_through_its_writer() {
    edits_to(
        "pom.xml",
        XML,
        &["del", "pom.xml", "/project/0/name"],
        "<?xml version=\"1.0\"?>\n<!-- c -->\n<project>\n  <version>1.0</version>\n</project>\n",
    );
    edits_to(
        "main.tf",
        TF,
        &["del", "main.tf", "/resource/r/m"],
        "region = \"us\"\nresource \"r\" \"n\" {\n  a = 1\n}\n",
    );
    edits_to(
        "app.ini",
        INI,
        &["del", "app.ini", "/server/host"],
        "[server]\nport=80\n",
    );
    edits_to(".env", ENV, &["del", ".env", "/B"], "A=1\n");
    edits_to("t.csv", CSV, &["del", "t.csv", "/0"], "n,a\ny,2\n");
}

#[test]
fn a_csv_with_no_rows_left_keeps_its_header() {
    // Was: an empty file, the header gone.
    edits_to("t.csv", "n,a\nx,1\n", &["del", "t.csv", "/0"], "n,a\n");
    edits_to("t.tsv", "n\ta\nx\t1\n", &["del", "t.tsv", "/0"], "n\ta\n");
    edits_to(
        "t.csv",
        "n,a\nx,1\ny,2\n",
        &["set", "t.csv", "--jq", "map(select(false))"],
        "n,a\n",
    );
    // A file of its header alone is written back as it is.
    edits_to("t.csv", "n,a\n", &["fmt", "t.csv"], "n,a\n");
    // A row added to a header alone gets every column of it. Was: `a\n1\n`,
    // the column `n` gone.
    edits_to(
        "t.csv",
        "n,a\n",
        &["set", "t.csv", "/-", r#"{"a": "1"}"#],
        "n,a\n,1\n",
    );
    edits_to(
        "t.tsv",
        "n\ta\n",
        &["set", "t.tsv", "/-", r#"{"a": "1", "c": 2}"#],
        "n\ta\tc\n\t1\t2\n",
    );
}

/// The header the file was read with keeps its columns and their order
/// through a point edit; a new key is a new column after them, in the
/// order first seen. A cell a point edit deletes is left empty, whether
/// other rows hold its column or not.
#[test]
fn a_csv_point_edit_keeps_the_header_the_file_has() {
    // Was: `b,a\n9,8\n3,4\n`, the header of the whole file reordered.
    edits_to(
        "t.csv",
        "a,b\n1,2\n3,4\n",
        &["set", "t.csv", "/0", r#"{"b": "9", "a": "8"}"#],
        "a,b\n8,9\n3,4\n",
    );
    edits_to(
        "t.csv",
        "a,b\n1,2\n",
        &["set", "t.csv", "/0", r#"{"c": "5", "b": "9"}"#],
        "a,b,c\n,9,5\n",
    );
    // A deleted cell is empty, the column kept — as in a file of several
    // rows, where another row holds it. Was: `a\n1\n`.
    edits_to(
        "t.csv",
        "a,b\n1,2\n",
        &["del", "t.csv", "/0/b"],
        "a,b\n1,\n",
    );
    edits_to(
        "t.csv",
        "a,b\n1,2\n3,4\n",
        &["del", "t.csv", "/0/b"],
        "a,b\n1,\n3,4\n",
    );
    // A filter rebuilds the rows, which then decide the columns and their
    // order: one no row holds goes.
    edits_to(
        "t.csv",
        "a,b\n1,2\n3,4\n",
        &["set", "t.csv", "--jq", "map(del(.a))"],
        "b\n2\n4\n",
    );
    edits_to(
        "t.csv",
        "a,b\n1,2\n",
        &["set", "t.csv", "--jq", "map({b, c: 1, a})"],
        "b,c,a\n2,1,1\n",
    );
}

#[test]
fn a_value_the_writer_cannot_hold_is_refused_and_the_file_left_alone() {
    refused(
        "pom.xml",
        XML,
        &["set", "pom.xml", "/project/0/version", "2"],
        1,
        "xml format error: XML write: child 'version' under <project> must be an Array of element maps (got a number 2)\n",
    );
    refused(
        "main.tf",
        TF,
        &["set", "main.tf", "/region", "1e400"],
        1,
        "hcl format error: HCL cannot represent arbitrary-precision number 1e400\n",
    );
    refused(
        "app.ini",
        INI,
        &["set", "app.ini", "/server/port", r#"{"x": 1}"#],
        1,
        "ini format error: section 'server' key 'port': nested object cannot be serialized to INI\n",
    );
    refused(
        ".env",
        ENV,
        &["set", ".env", "/A", "[1]"],
        1,
        "dotenv format error: key 'A': nested array cannot be serialized to .env\n",
    );
    refused(
        "t.csv",
        CSV,
        &["set", "t.csv", "/0/a", r#"{"x": 1}"#],
        1,
        "csv format error: row 0 key 'a': nested object cannot be serialized to csv\n",
    );
}

#[test]
fn path_errors_are_the_same_as_on_textual_formats() {
    refused(
        "app.ini",
        INI,
        &["set", "app.ini", "/client/retries", "3", "--no-create"],
        2,
        "path '/client/retries' not found (matched up to '')\n  pointer: /client/retries\n",
    );
    refused(
        "t.csv",
        CSV,
        &["del", "t.csv", "/0/zz"],
        2,
        "path '/0/zz' not found (matched up to '/0')\n  matched prefix: /0\n  pointer: /0/zz\n",
    );
    refused(
        ".env",
        ENV,
        &["del", ".env", ""],
        2,
        "cannot delete the root '': it is the whole document; delete a value inside it, or one document of a stream as /N\n  pointer: \n",
    );
}

#[test]
fn patch_and_merge_rewrite_through_the_writer() {
    edits_to(
        "t.csv",
        CSV,
        &[
            "patch",
            "t.csv",
            r#"[{"op":"add","path":"/0","value":{"n":"w","a":"0"}},{"op":"test","path":"/1/n","value":"x"}]"#,
        ],
        "n,a\nw,0\nx,1\ny,2\n",
    );
    edits_to(
        "app.ini",
        INI,
        &[
            "merge",
            "app.ini",
            r#"{"server": {"host": null, "tls": "on"}}"#,
        ],
        "[server]\nport=80\ntls=on\n",
    );
    // A failing op leaves the file alone, as on any format.
    refused(
        "t.csv",
        CSV,
        &[
            "patch",
            "t.csv",
            r#"[{"op":"remove","path":"/0"},{"op":"test","path":"/0/n","value":"x"}]"#,
        ],
        1,
        "RFC 6902 test op failed at /0/n: expected \"x\", got \"y\"\n",
    );
}

#[test]
fn a_bulk_run_counts_rewritten_skipped_and_refused_files() {
    let dir = TempDir::new().expect("tempdir");
    let seed = |name: &str, src: &str| std::fs::write(dir.path().join(name), src).unwrap();
    seed("a.env", "# c\nA=1\n");
    // Already as the writer writes it: no byte would change.
    seed("b.env", "A=2\n");
    // An INI section must be a map: the writer refuses `A = 2` at the top.
    seed("c.ini", INI);
    let out = dq()
        .current_dir(dir.path())
        .args([
            "-i",
            "--continue-on-error",
            "--no-color",
            "set",
            "*",
            "/A",
            "2",
        ])
        .output()
        .expect("run dq");
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
    assert_eq!(
        (
            out.status.code(),
            String::from_utf8(out.stdout).unwrap(),
            read("a.env"),
            read("b.env"),
            read("c.ini"),
        ),
        (
            Some(7),
            "Modified: 1, Skipped: 1, Failed: 1\n  - c.ini: ini format error: section 'A' must be a map of string values, got int\n".to_owned(),
            "A=2\n".to_owned(),
            "A=2\n".to_owned(),
            INI.to_owned(),
        )
    );
}

#[test]
fn set_and_del_help_say_which_formats_lose_their_comments() {
    for command in ["set", "del"] {
        let out = dq().args([command, "--help"]).output().expect("run dq");
        let help = String::from_utf8(out.stdout).unwrap().replace('\n', " ");
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            help.contains("YAML, JSON and TOML files are edited in place: comments and formatting stay. XML, HCL, INI, dotenv, CSV/TSV and JSON Lines files are written whole anew through the format's writer"),
            "{command}: {help}"
        );
        assert!(
            help.contains("formatting is not kept, and neither are comments, except XML's."),
            "{command}: {help}"
        );
    }
}

/// A write that replaces the whole document builds the layout anew from
/// the value written, as a filter does: the header the file was read with
/// does not come back.
#[test]
fn a_csv_whose_root_is_replaced_takes_the_new_rows_columns() {
    let src = "a,b\n1,2\n3,4\n";
    // Was: `a,b,z\n,,1\n`, the old header back.
    for args in [
        &["set", "c.csv", "", r#"[{"z": 1}]"#][..],
        &[
            "patch",
            "c.csv",
            r#"[{"op":"replace","path":"","value":[{"z":1}]}]"#,
        ],
        &[
            "patch",
            "c.csv",
            r#"[{"op":"add","path":"","value":[{"z":1}]}]"#,
        ],
        &["merge", "c.csv", r#"[{"z": 1}]"#],
    ] {
        edits_to("c.csv", src, args, "z\n1\n");
    }
    // Not in HCL: a string set over an expression, at the root as below
    // it, is the string asked for, not the expression it reads as.
    edits_to(
        "m.tf",
        "a = var.x\nb = 1\n",
        &["set", "m.tf", "", r#"{"a": "${var.x}", "b": 2}"#],
        "a = \"$${var.x}\"\nb = 2\n",
    );
    // A point edit below the root keeps it.
    edits_to(
        "c.csv",
        src,
        &["set", "c.csv", "/0", r#"{"z": "1"}"#],
        "a,b,z\n,,1\n3,4,\n",
    );
}

/// `#xml` is the declaration at the top level, and nothing below it: an
/// edit there is written, one that sets it in an element, where the writer
/// has nowhere to put it, is refused.
#[test]
fn an_xml_declaration_edit_is_written_or_refused_never_dropped() {
    let src = "<?xml version=\"1.0\"?>\n<r>\n  <c>1</c>\n</r>\n";
    // Was: exit 0, the file unchanged.
    refused(
        "g.xml",
        src,
        &["set", "g.xml", "/r/0/c/0/#xml/v", "1"],
        7,
        "cannot set '/r/0/c/0/#xml/v': the xml dq would write reads back as another value \
         at '/r/0/c/0/#xml'\n",
    );
    refused(
        "g.xml",
        src,
        &["set", "g.xml", "/r/0/#xml", r#"{"version": "1.0"}"#],
        7,
        "cannot set '/r/0/#xml': the xml dq would write reads back as another value \
         at '/r/0/#xml'\n",
    );
    // At the top level: the declaration, written as asked.
    edits_to(
        "g.xml",
        src,
        &["set", "g.xml", "/#xml/encoding", "UTF-8"],
        &src.replace("?>", " encoding=\"UTF-8\"?>"),
    );
    edits_to(
        "g.xml",
        "<r/>\n",
        &["set", "g.xml", "/#xml", "{}"],
        "<?xml version=\"1.0\"?>\n<r/>\n",
    );
    edits_to(
        "g.xml",
        "<?xml version=\"1.0\" standalone=\"yes\"?>\n<r/>\n",
        &["set", "g.xml", "/#xml", "{}"],
        "<?xml version=\"1.0\"?>\n<r/>\n",
    );
    // `{}` asks for a declaration of the default version, which the file
    // has: nothing to write.
    edits_to("g.xml", src, &["set", "g.xml", "/#xml", "{}"], src);
    // A null below the root carries nothing, as anywhere in XML.
    edits_to("g.xml", src, &["set", "g.xml", "/r/0/#xml", "null"], src);
}
