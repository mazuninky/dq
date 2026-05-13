//! `dq self check` and `dq self update` — operations on the running binary.
//!
//! Mirrors `rustup self check` / `rustup self update`. The check path makes
//! a single GitHub API call via [`ureq`] and compares the released
//! `tag_name` with `env!("CARGO_PKG_VERSION")`; the update path delegates
//! the download → verify SHA256 → atomic rename dance to the `self_update`
//! crate's GitHub backend.
//!
//! Module file is named `self_cmd` because `self` is a Rust keyword. The
//! subcommand still surfaces as `dq self` — see `cli/args/self_cmd.rs`.
//!
//! # Exit codes
//!
//! - `dq self check` exits 0 in all three "what version are we on" cases
//!   (up-to-date, newer available, pre-release). Network failures map to
//!   exit 5 (`IO_ERROR`) via a [`dq_core::Error::Io`] wrapper. GitHub API
//!   rate-limit hits (`403` with `X-RateLimit-Remaining: 0`) emit a hint
//!   pointing to the `GITHUB_TOKEN` workaround before bubbling the error.
//! - `dq self update` exits 0 on success, 5 on network errors, 6 when the
//!   target binary path is read-only by the current user (with a
//!   `sudo dq self update` hint), 7 on atomic-replace failure.

use std::io::Write;

use camino::Utf8PathBuf;
use tracing::{info, warn};

use crate::cli::SelfUpdateArgs;
use crate::error::InvalidInput;

const GITHUB_REPO_OWNER: &str = "mazuninky";
const GITHUB_REPO_NAME: &str = "dq";
const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/mazuninky/dq/releases/latest";
const USER_AGENT: &str = "dq-self-check";

/// Outcome of a version comparison between the running binary and the
/// latest published GitHub release.
#[derive(Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Local version equals the latest published release.
    UpToDate { version: String },
    /// A newer release is available than the running binary.
    NewerAvailable { local: String, latest: String },
    /// The running binary is newer than the latest published release.
    PreRelease { local: String, latest: String },
}

/// Run `dq self check` — query GitHub for the latest release and render the
/// outcome to `out`.
///
/// Thin wrapper around [`run_check_with_url`] that hardcodes the production
/// GitHub Releases endpoint. The inner function takes the URL as a parameter
/// so unit tests can stand up a local mock server and exercise the full
/// HTTP-response → error-mapping path without touching the network.
///
/// # Errors
///
/// - Network errors and HTTP 4xx/5xx responses are wrapped in
///   [`dq_core::Error::Io`] so the exit-code mapper picks 5 (`IO_ERROR`).
/// - HTTP 403 + `X-RateLimit-Remaining: 0` produces a [`dq_core::Error::Io`]
///   carrying a `GITHUB_TOKEN` hint — same exit code, more helpful message.
pub fn run_check(out: &mut dyn Write) -> anyhow::Result<()> {
    run_check_with_url(out, GITHUB_RELEASES_LATEST_URL)
}

/// Inner implementation of [`run_check`] with the releases URL injected.
///
/// Exposed at crate visibility so the unit test in this module can point it
/// at a mock HTTP server. Production callers should go through [`run_check`].
///
/// # Errors
///
/// Same as [`run_check`].
pub(crate) fn run_check_with_url(out: &mut dyn Write, releases_url: &str) -> anyhow::Result<()> {
    let response = ureq::get(releases_url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call();

    let body = match response {
        Ok(resp) => resp.into_string().map_err(|source| {
            anyhow::Error::new(dq_core::Error::Io {
                path: Utf8PathBuf::from(":self-check"),
                source,
            })
        })?,
        Err(ureq::Error::Status(code, resp)) if code == 403 => {
            let rate_limited =
                matches!(resp.header("x-ratelimit-remaining"), Some(remaining) if remaining == "0");
            if rate_limited {
                warn!("GitHub API rate limit hit; set the GITHUB_TOKEN env var to raise the limit",);
            }
            return Err(anyhow::Error::new(dq_core::Error::Io {
                path: Utf8PathBuf::from(":self-check"),
                source: std::io::Error::other(format!(
                    "GitHub API returned HTTP {code}{}",
                    if rate_limited {
                        " (rate limit exhausted; set GITHUB_TOKEN)"
                    } else {
                        ""
                    },
                )),
            }));
        }
        Err(other) => {
            return Err(anyhow::Error::new(dq_core::Error::Io {
                path: Utf8PathBuf::from(":self-check"),
                source: std::io::Error::other(format!("self-check request failed: {other}")),
            }));
        }
    };

    let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|source| {
        anyhow::Error::new(dq_core::Error::Io {
            path: Utf8PathBuf::from(":self-check"),
            source: std::io::Error::other(format!(
                "GitHub API response was not valid JSON: {source}"
            )),
        })
    })?;
    let remote_tag = parsed
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::Error::new(dq_core::Error::Io {
                path: Utf8PathBuf::from(":self-check"),
                source: std::io::Error::other(
                    "GitHub API response did not contain a `tag_name` field",
                ),
            })
        })?;

    let outcome = compare_versions(env!("CARGO_PKG_VERSION"), remote_tag);
    render_check_outcome(&outcome, out)?;
    Ok(())
}

/// Compare a local SemVer string with a remote tag (which conventionally
/// starts with a `v`).
///
/// Pulled out of [`run_check`] so unit tests can cover the three branches
/// without an HTTP fixture.
#[must_use]
pub fn compare_versions(local: &str, remote_tag: &str) -> CheckOutcome {
    let stripped = remote_tag.strip_prefix('v').unwrap_or(remote_tag);
    if local == stripped {
        CheckOutcome::UpToDate {
            version: stripped.to_owned(),
        }
    } else if version_is_newer(stripped, local) {
        CheckOutcome::NewerAvailable {
            local: local.to_owned(),
            latest: stripped.to_owned(),
        }
    } else {
        CheckOutcome::PreRelease {
            local: local.to_owned(),
            latest: stripped.to_owned(),
        }
    }
}

/// Render a [`CheckOutcome`] to `out`.
///
/// Lives next to [`compare_versions`] for symmetry; the `out` parameter
/// keeps the rendering testable without going through the binary.
///
/// # Errors
///
/// Any I/O error from `out`.
pub fn render_check_outcome(outcome: &CheckOutcome, out: &mut dyn Write) -> std::io::Result<()> {
    match outcome {
        CheckOutcome::UpToDate { version } => {
            writeln!(out, "dq v{version} is up to date")
        }
        CheckOutcome::NewerAvailable { local, latest } => writeln!(
            out,
            "newer version available: v{latest} (current: v{local}) — run `dq self update` to install",
        ),
        CheckOutcome::PreRelease { local, latest } => writeln!(
            out,
            "running pre-release version (local: v{local}, latest published: v{latest})",
        ),
    }
}

/// Best-effort SemVer-ish "is `a` newer than `b`?" comparison.
///
/// Splits each version on `.`, compares numeric components, and falls back
/// to a string compare if a component isn't numeric. Pre-release suffixes
/// (`1.2.3-rc1`) are stripped to the leading numeric core — good enough for
/// the M6 use case where both versions come from the same release pipeline.
fn version_is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|c| {
                c.split(|ch: char| !ch.is_ascii_digit())
                    .next()
                    .unwrap_or("")
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .collect()
    };
    let a_parts = parse(a);
    let b_parts = parse(b);
    let len = a_parts.len().max(b_parts.len());
    for i in 0..len {
        let av = a_parts.get(i).copied().unwrap_or(0);
        let bv = b_parts.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    false
}

/// Run `dq self update` — download the configured release artifact and
/// atomically replace the running binary.
///
/// # Errors
///
/// - [`InvalidInput`] (exit 6) when the running binary's parent directory
///   is not writable for the current user — surfaced before any download
///   attempt with a `sudo dq self update` hint.
/// - [`dq_core::Error::Io`] (exit 5) for network errors.
/// - [`dq_core::Error::WriteIo`] (exit 7) for atomic-replace failures.
pub fn run_update(args: &SelfUpdateArgs) -> anyhow::Result<()> {
    let current_exe = std::env::current_exe().map_err(|source| {
        anyhow::Error::new(dq_core::Error::Io {
            path: Utf8PathBuf::from(":self-update"),
            source,
        })
    })?;
    ensure_writable_install_dir(&current_exe)?;

    info!(
        "starting self-update; current binary: {}",
        current_exe.display()
    );

    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner(GITHUB_REPO_OWNER)
        .repo_name(GITHUB_REPO_NAME)
        .bin_name("dq")
        .current_version(env!("CARGO_PKG_VERSION"))
        .target(self_update::get_target())
        .show_download_progress(false)
        .show_output(false);
    if let Some(tag) = args.to.as_deref() {
        builder.target_version_tag(tag);
    }

    let updater = builder.build().map_err(map_self_update_error)?;
    let status = updater.update().map_err(map_self_update_error)?;
    if status.updated() {
        info!("dq updated to v{}", status.version());
    } else {
        info!("dq is already up to date (v{})", status.version());
    }
    Ok(())
}

/// Reject early if the directory containing `current_exe` is not writable
/// by the current user — saves the user from a half-finished download
/// followed by an opaque permissions error from `self_update`.
fn ensure_writable_install_dir(current_exe: &std::path::Path) -> anyhow::Result<()> {
    let parent = current_exe.parent().ok_or_else(|| {
        anyhow::Error::new(InvalidInput::new(format!(
            "cannot determine parent directory of running binary: {}",
            current_exe.display()
        )))
    })?;
    // Probe writability by attempting to create a temp file in `parent`.
    // Cheaper than re-implementing per-OS permission decoding (Unix mode
    // bits + Windows ACLs), and a temp-file probe is exactly what the
    // installer is about to do anyway.
    match tempfile::Builder::new()
        .prefix(".dq-self-update-probe-")
        .tempfile_in(parent)
    {
        Ok(_) => Ok(()),
        Err(_) => Err(anyhow::Error::new(InvalidInput::new(format!(
            "binary lives at {} which is not writable by the current user; \
             try `sudo dq self update`",
            current_exe.display()
        )))),
    }
}

fn map_self_update_error(err: self_update::errors::Error) -> anyhow::Error {
    use self_update::errors::Error as SuError;
    // Both `Network` and the active HTTP-backend variant (`Ureq` under the
    // 0.44 `ureq` feature) represent failed HTTP traffic; collapse them onto
    // the same `Io`-shape so `exit_code_for_error` produces 5 (`IO_ERROR`)
    // and the user sees the GITHUB_TOKEN hint. The `Reqwest` variant is
    // compiled out under our feature set so we don't enumerate it here.
    let network_msg = match &err {
        SuError::Network(msg) => Some(msg.clone()),
        SuError::Ureq(req) => Some(req.to_string()),
        _ => None,
    };
    if let Some(msg) = network_msg {
        return anyhow::Error::new(dq_core::Error::Io {
            path: Utf8PathBuf::from(":self-update"),
            source: std::io::Error::other(format!(
                "self-update network failure: {msg} \
                 (set GITHUB_TOKEN to raise the GitHub API rate limit)"
            )),
        });
    }
    match err {
        SuError::Io(io_err) => anyhow::Error::new(dq_core::Error::WriteIo {
            path: Utf8PathBuf::from(":self-update"),
            source: io_err,
        }),
        other => anyhow::Error::new(dq_core::Error::WriteIo {
            path: Utf8PathBuf::from(":self-update"),
            source: std::io::Error::other(format!("self-update failed: {other}")),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Render `outcome` to a fresh `Vec<u8>` and return the produced string.
    fn render(outcome: &CheckOutcome) -> String {
        let mut buf: Vec<u8> = Vec::new();
        render_check_outcome(outcome, &mut buf).expect("render must not fail to a Vec<u8>");
        String::from_utf8(buf).expect("output must be valid UTF-8")
    }

    #[test]
    fn compare_versions_up_to_date() {
        let outcome = compare_versions("0.1.0", "v0.1.0");
        assert_eq!(
            outcome,
            CheckOutcome::UpToDate {
                version: "0.1.0".to_owned(),
            }
        );
    }

    #[test]
    fn compare_versions_newer_available() {
        let outcome = compare_versions("0.1.0", "v0.2.0");
        assert_eq!(
            outcome,
            CheckOutcome::NewerAvailable {
                local: "0.1.0".to_owned(),
                latest: "0.2.0".to_owned(),
            }
        );
    }

    #[test]
    fn compare_versions_pre_release_when_local_newer() {
        let outcome = compare_versions("0.3.0", "v0.2.0");
        assert_eq!(
            outcome,
            CheckOutcome::PreRelease {
                local: "0.3.0".to_owned(),
                latest: "0.2.0".to_owned(),
            }
        );
    }

    #[test]
    fn compare_versions_strips_v_prefix_when_local_matches() {
        // The remote tag conventionally carries a `v` prefix — the prefix
        // strip must succeed so a release named `v0.1.0` against a local
        // `0.1.0` is recognised as up-to-date, not as a "different version".
        let outcome = compare_versions("0.1.0", "v0.1.0");
        assert_eq!(
            outcome,
            CheckOutcome::UpToDate {
                version: "0.1.0".to_owned(),
            }
        );
    }

    #[test]
    fn compare_versions_handles_remote_without_v_prefix() {
        // Defensive: GitHub release tags conventionally start with `v`, but
        // the comparator should also work if a project ships unprefixed
        // tags (`0.2.0`). Newer-available branch must trigger.
        let outcome = compare_versions("0.1.0", "0.2.0");
        assert_eq!(
            outcome,
            CheckOutcome::NewerAvailable {
                local: "0.1.0".to_owned(),
                latest: "0.2.0".to_owned(),
            }
        );
    }

    #[test]
    fn compare_versions_local_newer_is_pre_release() {
        // When the local version is ahead of the latest published release
        // (developer build / RC), the user must see the pre-release line so
        // they know the running binary is not a stable release.
        let outcome = compare_versions("0.2.0", "v0.1.0");
        assert_eq!(
            outcome,
            CheckOutcome::PreRelease {
                local: "0.2.0".to_owned(),
                latest: "0.1.0".to_owned(),
            }
        );
    }

    #[test]
    fn compare_versions_does_not_panic_on_malformed_remote_tag() {
        // The remote tag came from a third party (GitHub's API). The
        // comparator's `version_is_newer` parser returns 0 for non-numeric
        // segments — so a `"bogus"` tag should NOT panic, regardless of
        // which branch the implementation lands on. Guards against future
        // refactors that introduce a `.unwrap()` on the parsed components.
        let outcome = compare_versions("0.1.0", "bogus");
        // The implementation chose: 0.1.0 vs "bogus" → both parse to [0, …]
        // → equal numeric rank → falls through to PreRelease (local seen as
        // not-older). What matters is "no panic" + "some outcome returned".
        match outcome {
            CheckOutcome::UpToDate { .. }
            | CheckOutcome::NewerAvailable { .. }
            | CheckOutcome::PreRelease { .. } => {}
        }
    }

    #[test]
    fn render_check_outcome_writes_each_variant() {
        // Sanity: each variant produces a single non-empty line ending in
        // '\n'. Full wording assertions live in the per-variant tests below.
        for outcome in [
            CheckOutcome::UpToDate {
                version: "0.1.0".to_owned(),
            },
            CheckOutcome::NewerAvailable {
                local: "0.1.0".to_owned(),
                latest: "0.2.0".to_owned(),
            },
            CheckOutcome::PreRelease {
                local: "0.3.0".to_owned(),
                latest: "0.2.0".to_owned(),
            },
        ] {
            let line = render(&outcome);
            assert!(line.ends_with('\n'), "missing trailing newline: {line:?}");
            assert!(!line.trim().is_empty(), "empty output for {outcome:?}");
        }
    }

    #[test]
    fn render_check_outcome_newer_available_mentions_self_update_command() {
        // The "newer available" line is the user's call to action. It MUST
        // tell them how to install the new release — the literal string
        // `dq self update` so a user can copy-paste the suggestion.
        let line = render(&CheckOutcome::NewerAvailable {
            local: "0.1.0".to_owned(),
            latest: "0.2.0".to_owned(),
        });
        assert!(
            line.contains("dq self update"),
            "newer-available line must suggest `dq self update`; got: {line:?}",
        );
        // And both versions must appear so the user knows what they're
        // upgrading from / to.
        assert!(
            line.contains("0.1.0") && line.contains("0.2.0"),
            "newer-available line must name both versions; got: {line:?}",
        );
    }

    #[test]
    fn render_check_outcome_up_to_date_names_version() {
        let line = render(&CheckOutcome::UpToDate {
            version: "1.2.3".to_owned(),
        });
        assert!(
            line.contains("1.2.3"),
            "up-to-date line must name the current version; got: {line:?}",
        );
        assert!(
            line.contains("up to date"),
            "up-to-date line must say `up to date`; got: {line:?}",
        );
    }

    #[test]
    fn render_check_outcome_pre_release_names_both_versions() {
        let line = render(&CheckOutcome::PreRelease {
            local: "0.3.0".to_owned(),
            latest: "0.2.0".to_owned(),
        });
        assert!(
            line.contains("0.3.0") && line.contains("0.2.0"),
            "pre-release line must name both local and latest versions; got: {line:?}",
        );
        assert!(
            line.contains("pre-release"),
            "pre-release line must say `pre-release`; got: {line:?}",
        );
    }

    // ---------------------------------------------------------------------
    // `run_update` and the live-API smoke test for `run_check` are gated
    // behind `#[ignore]` because they hit the network (api.github.com and
    // the release artifact host). They are NOT part of the default test
    // run — opt in with `cargo test --package dq-cli -- --ignored self_cmd`.
    //
    // `run_check` itself now has a hermetic test (`run_check_returns_io_error_on_404`
    // above): the URL is injected via [`run_check_with_url`] so the test
    // can point it at a local `TcpListener`-backed mock returning a
    // specific status code. The live-API test is retained as a pre-release
    // smoke check that confirms the GitHub API contract hasn't drifted.
    //
    // Test gap: `run_update` still wraps `self_update::backends::github`
    // with no seam. The `ensure_writable_install_dir` helper IS unit-testable
    // (covered indirectly by the integration test below) but the
    // `Update::configure` invocation cannot be observed without launching a
    // real download. The trait-based seam asked for in spec §4.5 was not
    // implemented in the production-code pass — flagged as a follow-up.
    // ---------------------------------------------------------------------

    /// Minimal one-shot HTTP server backed by a `TcpListener`. Reads the
    /// inbound request bytes through the end of the request headers
    /// (`\r\n\r\n`) and writes a single fixed response back.
    ///
    /// Hand-rolled instead of pulling in `httpmock` / `mockito` / `wiremock`
    /// (each of which drags in a full async runtime plus an HTTP server
    /// stack) because all the regression test actually needs is a server
    /// that returns one specific status code so the error-mapping branch
    /// of `run_check_with_url` is reachable from a unit test.
    fn spawn_one_shot_http(response_bytes: &'static [u8]) -> String {
        use std::io::Read;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener
            .local_addr()
            .expect("local_addr after successful bind");
        std::thread::spawn(move || {
            // Accept exactly one connection — the unit test sends exactly
            // one request. Errors are swallowed: if the test client never
            // connects (e.g. it panicked first), there is nothing useful
            // the helper thread can do.
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            // Drain the request headers so the client doesn't see a
            // connection reset before it can read the response. A 1 KiB
            // scratch buffer is plenty for the one-line GET produced by
            // `ureq::get(url).call()` plus the User-Agent / Accept headers
            // set in `run_check_with_url`.
            let mut buf = [0u8; 1024];
            let mut total = Vec::new();
            loop {
                match socket.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        total.extend_from_slice(&buf[..n]);
                        if total.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = socket.write_all(response_bytes);
        });
        format!("http://{addr}/releases/latest")
    }

    #[test]
    fn run_check_returns_io_error_on_404() {
        // Regression for the "exit 0 on HTTP 4xx/5xx" bug: stand up a local
        // mock server that returns HTTP 404 (the symptom a fresh repo
        // without a published GitHub release exhibits) and confirm
        // `run_check_with_url` surfaces the failure as `dq_core::Error::Io`.
        //
        // The downstream contract this guards: `exit_code_for_error` maps
        // `Error::Io` to exit 5 (`IO_ERROR`), so as long as the error is
        // returned in the `Io` shape, the CLI binary exits 5 — which is
        // the behaviour documented in README.md and required by the bug
        // report. A regression that swallows the error (returns `Ok(())`)
        // would silently let the binary exit 0; this test fails fast in
        // that case.
        let url = spawn_one_shot_http(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let mut buf: Vec<u8> = Vec::new();
        let result = run_check_with_url(&mut buf, &url);
        let err = result.expect_err("HTTP 404 must produce an error, not a silent Ok");
        let domain = err
            .downcast_ref::<dq_core::Error>()
            .expect("error must downcast to dq_core::Error for exit-code mapping");
        assert_eq!(
            domain.kind_name(),
            "io",
            "404 must map to Error::Io (exit 5), got: {domain:?}",
        );
        // The mock server returns 0 bytes of body, so nothing should reach
        // the writer — but assert it explicitly so future refactors that
        // accidentally print partial output before erroring out get caught.
        assert!(
            buf.is_empty(),
            "no output should be written on 404; got: {buf:?}",
        );
    }

    #[test]
    #[ignore = "hits api.github.com — opt-in via `cargo test -- --ignored`"]
    fn run_check_against_real_github_api() {
        // Smoke test for the live network path. Asserts only "no error" —
        // we cannot pin the released version. Run manually before publishing
        // a new release to confirm the GitHub API contract still holds.
        let mut buf: Vec<u8> = Vec::new();
        run_check(&mut buf).expect("live GitHub API call must succeed");
        let out = String::from_utf8(buf).expect("output must be valid UTF-8");
        // One of three known status lines must appear.
        assert!(
            out.contains("up to date")
                || out.contains("newer version available")
                || out.contains("pre-release"),
            "unexpected check-outcome line: {out:?}",
        );
    }
}
