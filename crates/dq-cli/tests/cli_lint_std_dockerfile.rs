//! `@std/dockerfile` rules run on Dockerfiles only. `dq lint` of an
//! ordinary YAML file used to print a `check.jq raised a runtime error`
//! warning per rule.

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
fn an_ordinary_yaml_mapping_lints_clean_without_a_warning() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("x.yaml"), "name: web\nreplicas: 2\n").expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "x.yaml"])
        .assert()
        .success()
        .stdout("diagnostics: []\n")
        .stderr("");
}

#[test]
fn each_dockerfile_rule_fires_on_a_dockerfile_and_not_on_a_clean_one() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM ubuntu:latest\nADD . /app\nRUN [\"sh\", \"-c\", \"curl -sSL https://x.sh | bash\"]\n",
    )
    .expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "Dockerfile"])
        .assert()
        .code(4)
        .stdout(
            predicate::str::contains("dockerfile.no-latest-base-image")
                .and(predicate::str::contains("dockerfile.no-add-use-copy"))
                .and(predicate::str::contains("dockerfile.no-curl-pipe-bash"))
                .and(predicate::str::contains("dockerfile.has-healthcheck")),
        )
        .stderr(predicate::str::contains("WARN").not());

    // Pinned tag, COPY, no pipe into a shell: only the missing HEALTHCHECK.
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("Dockerfile"),
        "FROM alpine:3.20\nCOPY . /app\nRUN echo hi\n",
    )
    .expect("write fixture");
    dq().current_dir(dir.path())
        .args(["--no-color", "lint", "Dockerfile"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("dockerfile.has-healthcheck")
                .and(predicate::str::contains("dockerfile.no-latest-base-image").not())
                .and(predicate::str::contains("dockerfile.no-add-use-copy").not())
                .and(predicate::str::contains("dockerfile.no-curl-pipe-bash").not()),
        )
        .stderr(predicate::str::contains("WARN").not());
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

// ---------------------------------------------------------------------------
// The rules on real Dockerfile text, through the CLI: a parser that rejects
// or mis-shapes an instruction shows up as a rule verdict here.
// ---------------------------------------------------------------------------

/// `dq lint -F json Dockerfile` over `src`: the exit code and the
/// `(rule_id, message)` of every diagnostic. Asserts that no rule raised a
/// runtime warning.
fn lint(src: &str) -> (i32, Vec<(String, String)>) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Dockerfile"), src).expect("write Dockerfile");
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "lint", "-F", "json", "Dockerfile"])
        .output()
        .expect("run dq lint");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("WARN"),
        "rule warning for {src:?}: {stderr}"
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("lint -F json for {src:?} is not JSON ({e}); stderr: {stderr}"));
    let diags = report["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .map(|d| {
            (
                d["rule_id"].as_str().expect("rule_id").to_owned(),
                d["message"].as_str().expect("message").to_owned(),
            )
        })
        .collect();
    (out.status.code().expect("exit code"), diags)
}

/// The messages `rule` produced for `src`.
fn messages_of(src: &str, rule: &str) -> Vec<String> {
    lint(src)
        .1
        .into_iter()
        .filter(|(id, _)| id == rule)
        .map(|(_, message)| message)
        .collect()
}

const HEALTHCHECK: &str = "dockerfile.has-healthcheck";
const CURL_PIPE: &str = "dockerfile.no-curl-pipe-bash";
const ADD: &str = "dockerfile.no-add-use-copy";
const LATEST: &str = "dockerfile.no-latest-base-image";

#[test]
fn has_healthcheck_passes_with_any_healthcheck_form() {
    for src in [
        "FROM alpine:3.20\nHEALTHCHECK CMD true\n",
        "FROM alpine:3.20\nHEALTHCHECK --interval=30s CMD curl -f http://localhost/ || exit 1\n",
        "FROM alpine:3.20\nHEALTHCHECK NONE\n",
    ] {
        assert_eq!(
            messages_of(src, HEALTHCHECK),
            Vec::<String>::new(),
            "{src:?}"
        );
    }
}

#[test]
fn has_healthcheck_fires_without_one_and_on_an_onbuild_trigger_only() {
    for src in [
        "FROM alpine:3.20\nRUN echo hi\n",
        "FROM alpine:3.20\nONBUILD HEALTHCHECK CMD true\n",
    ] {
        assert_eq!(
            messages_of(src, HEALTHCHECK),
            ["Dockerfile has no HEALTHCHECK directive"],
            "{src:?}"
        );
    }
}

#[test]
fn no_curl_pipe_bash_fires_on_a_pipe_and_across_a_continuation() {
    for src in [
        "FROM alpine:3.20\nHEALTHCHECK NONE\nRUN curl -fsSL https://x | bash\n",
        "FROM alpine:3.20\nHEALTHCHECK NONE\nRUN curl -fsSL https://x \\\n    | bash\nUSER 1\n",
        "FROM alpine:3.20\nHEALTHCHECK NONE\nRUN <<EOT\ncurl -fsSL https://x | sh\nEOT\n",
    ] {
        assert_eq!(
            messages_of(src, CURL_PIPE),
            ["RUN line 3 pipes curl/wget into a shell"],
            "{src:?}"
        );
    }
}

#[test]
fn no_curl_pipe_bash_allows_exec_form_text_and_separate_heredoc_lines() {
    for src in [
        "FROM alpine:3.20\nRUN [\"echo\",\"curl x | bash\"]\n",
        "FROM alpine:3.20\nRUN <<EOT\ncurl -o f https://x\necho done | sh\nEOT\n",
    ] {
        assert_eq!(messages_of(src, CURL_PIPE), Vec::<String>::new(), "{src:?}");
    }
}

#[test]
fn no_add_use_copy_allows_a_url_with_or_without_a_checksum() {
    for src in [
        "FROM alpine:3.20\nADD https://example.com/a.tgz /tmp/\n",
        "FROM alpine:3.20\nADD --checksum=sha256:24454f830cdb571e2c4ad15481119c43b3cafd48dd869a9b2945d1036d1dc68d https://example.com/a.tgz /tmp/\n",
    ] {
        assert_eq!(messages_of(src, ADD), Vec::<String>::new(), "{src:?}");
    }
}

#[test]
fn no_add_use_copy_fires_on_a_local_source() {
    assert_eq!(
        messages_of("FROM alpine:3.20\nRUN echo hi\nADD ./app /app\n", ADD),
        ["ADD used for local copy (line 3); prefer COPY"]
    );
}

#[test]
fn no_latest_base_image_allows_scratch_a_stage_name_and_a_digest() {
    let src = "FROM golang:1.22 AS Build\n\
               FROM build\n\
               FROM scratch\n\
               FROM alpine@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n\
               HEALTHCHECK NONE\n";
    assert_eq!(lint(src), (0, Vec::new()));
}

#[test]
fn no_latest_base_image_fires_on_an_untagged_image_behind_a_registry_port() {
    let (code, diags) = lint("FROM localhost:5000/img\nFROM alpine\nHEALTHCHECK NONE\n");
    assert_eq!(code, 4, "an error-severity diagnostic exits 4");
    assert_eq!(
        diags,
        [
            (
                LATEST.to_owned(),
                "FROM line uses :latest (or no tag): localhost:5000/img".to_owned()
            ),
            (
                LATEST.to_owned(),
                "FROM line uses :latest (or no tag): alpine".to_owned()
            ),
        ]
    );
}
