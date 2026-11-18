//! `@std/dockerfile` rules match YAML only so their fixtures can spell the
//! instruction list; their `match.filter` keeps them off any other YAML.
//! `dq lint` of an ordinary YAML file used to print a `check.jq raised a
//! runtime error` warning per rule.

use assert_cmd::Command;
use predicates::prelude::*;
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

#[test]
fn dockerfile_rules_stay_quiet_on_ordinary_yaml() {
    for src in ["name: web\nreplicas: 2\n", "- a\n- b\n", "[]\n"] {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("x.yaml"), src).expect("write fixture");
        dq().current_dir(dir.path())
            .args(["--no-color", "lint", "x.yaml"])
            .assert()
            .stdout(predicate::str::contains("dockerfile").not())
            .stderr(predicate::str::contains("dockerfile").not());
    }
}

#[test]
fn dockerfile_rules_still_run_on_a_dockerfile() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM alpine:3.20\nADD ./app /app\n",
    )
    .expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "Dockerfile"])
        .assert()
        .stdout(
            predicate::str::contains("dockerfile.no-add-use-copy")
                .and(predicate::str::contains("dockerfile.has-healthcheck")),
        )
        .stderr(predicate::str::contains("WARN").not());
}
