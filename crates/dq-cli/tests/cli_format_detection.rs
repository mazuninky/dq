//! Format detection through the CLI for the names README's formats table
//! lists beyond the plain extensions: `.pom` and `.xsd` are XML, and a
//! `.env.<anything>` file (`.env.local`, `.env.production`, `.env.example`)
//! is dotenv. Each used to fail as an unknown extension, exit 6.
//!
//! A telling extension still wins over the `.env.` prefix, and a name
//! that only looks like one (`.envrc`) stays an error: dq never guesses.

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

/// Run `dq -q convert <name> -F json` (`-q`: a YAML source warns on stderr) on a file `name` holding `src`, in a
/// fresh directory; return (exit code, stdout, stderr).
fn convert(name: &str, src: &str) -> (i32, String, String) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join(name), src).expect("seed");
    let out = dq()
        .current_dir(dir.path())
        .args(["-q", "convert", name, "-F", "json"])
        .output()
        .expect("run dq");
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        String::from_utf8(out.stderr).expect("utf-8 stderr"),
    )
}

#[test]
fn pom_and_xsd_files_read_as_xml() {
    let pom = "<project><version>1.0</version></project>\n";
    let xsd = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"/>\n";
    for (name, src, want) in [
        (
            "pom.pom",
            pom,
            "{\n  \"project\": [\n    {\n      \"version\": [\n        {\n          \"#text\": \"1.0\"\n        }\n      ]\n    }\n  ]\n}\n",
        ),
        (
            "Library.POM",
            pom,
            "{\n  \"project\": [\n    {\n      \"version\": [\n        {\n          \"#text\": \"1.0\"\n        }\n      ]\n    }\n  ]\n}\n",
        ),
        (
            "schema.xsd",
            xsd,
            "{\n  \"xs:schema\": [\n    {\n      \"@attrs\": {\n        \"xmlns:xs\": \"http://www.w3.org/2001/XMLSchema\"\n      }\n    }\n  ]\n}\n",
        ),
    ] {
        let (code, stdout, stderr) = convert(name, src);
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (0, want, ""),
            "{name}"
        );
    }
}

#[test]
fn env_dot_anything_files_read_as_dotenv() {
    for name in [
        ".env.local",
        ".env.production",
        ".env.example",
        ".ENV.Staging",
    ] {
        let (code, stdout, stderr) = convert(name, "# c\nA=1\nB=\"x y\"\n");
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (0, "{\n  \"A\": \"1\",\n  \"B\": \"x y\"\n}\n", ""),
            "{name}"
        );
    }
}

#[test]
fn a_telling_extension_wins_over_the_env_prefix() {
    let (code, stdout, stderr) = convert(".env.yaml", "A: 1\n");
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (0, "{\n  \"A\": 1\n}\n", "")
    );
}

#[test]
fn names_that_only_look_like_dotenv_stay_unknown() {
    for (name, ext) in [
        (".envrc", ".envrc"),
        ("app.env.local", "local"),
        ("env.local", "local"),
    ] {
        let (code, stdout, stderr) = convert(name, "A=1\n");
        assert_eq!(
            (code, stdout.as_str(), stderr.as_str()),
            (6, "", format!("unsupported format '{ext}'\n").as_str()),
            "{name}"
        );
    }
}
