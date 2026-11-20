//! `dq test @std/<ns>` runs the fixtures embedded for a standard
//! namespace, and `dq test @std` those of every namespace. `dq test`
//! used to take only a directory: `dq test @std/k8s` exited 6 with
//! "rules directory does not exist or is not a directory: @std/k8s".

use assert_cmd::Command;
use predicates::prelude::*;

/// Build a `dq` command with a clean environment (same discipline as
/// `cli_smoke.rs::dq`), run from a directory with no rules in it.
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd.current_dir(std::env::temp_dir());
    cmd
}

/// The summary line of a run with no failure and no error.
fn clean_summary() -> impl Predicate<str> {
    predicate::str::is_match(r"\nsummary: [1-9][0-9]* passed, 0 failed, 0 errored, [0-9]+ total\n$")
        .expect("regex")
}

#[test]
fn test_runs_every_std_namespace() {
    for ns in dq_lint::list_std_rulesets() {
        dq().args(["--no-color", "test", &format!("@std/{ns}")])
            .assert()
            .success()
            .stdout(predicate::str::starts_with(format!("ok @std/{ns}/")))
            .stdout(clean_summary())
            .stderr("");
    }
}

#[test]
fn test_runs_all_std_namespaces_at_once() {
    dq().args(["--no-color", "test", "@std"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ok @std/dockerfile/"))
        .stdout(predicate::str::contains("\nok @std/terraform/"))
        .stdout(clean_summary())
        .stderr("");
}

#[test]
fn test_names_an_unknown_std_namespace() {
    dq().args(["--no-color", "test", "@std/k8z"])
        .assert()
        .code(6)
        .stdout("")
        .stderr("unknown rule @std/k8z\n");
}
