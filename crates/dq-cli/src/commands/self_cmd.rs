//! `dq self check` and `dq self update` — operations on the running binary.
//!
//! Mirrors `rustup self check` / `rustup self update`. The check path makes
//! a single GitHub API call via [`ureq`] and compares the released
//! `tag_name` with `env!("CARGO_PKG_VERSION")`; the update path delegates
//! the download → extract → atomic replace dance to the `self_update`
//! crate's GitHub backend. No SHA256 verification runs: `self_update`'s
//! `checksums` feature is not enabled (see the workspace `Cargo.toml`).
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
use ureq::ResponseExt;

use crate::cli::SelfUpdateArgs;
use crate::error::InvalidInput;

const GITHUB_REPO_OWNER: &str = "mazuninky";
const GITHUB_REPO_NAME: &str = "dq";
const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/mazuninky/dq/releases/latest";
const USER_AGENT: &str = "dq-self-check";
/// User-Agent `dq self update` sends on every request (the API listing and
/// the asset download) — the value `self_update` 0.44 hard-coded for GitHub.
const SELF_UPDATE_USER_AGENT: &str = "rust/self-update";
/// Connect timeout of `dq self check` — ureq 2's default, kept explicit
/// because ureq 3 has no connect timeout by default.
const SELF_CHECK_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Redirect budget of `dq self check` — ureq 2's default (ureq 3: 10).
const SELF_CHECK_MAX_REDIRECTS: u32 = 5;

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
    let io_error = |source: std::io::Error| {
        anyhow::Error::new(dq_core::Error::Io {
            path: Utf8PathBuf::from(":self-check"),
            source,
        })
    };

    let mut response = self_check_agent()
        .get(releases_url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|err| {
            io_error(std::io::Error::other(format!(
                "self-check request failed: {releases_url}: {err}"
            )))
        })?;

    let code = response.status().as_u16();
    if code == 403 {
        let rate_limited = response
            .headers()
            .get("x-ratelimit-remaining")
            .is_some_and(|remaining| remaining.as_bytes() == b"0");
        if rate_limited {
            warn!("GitHub API rate limit hit; set the GITHUB_TOKEN env var to raise the limit",);
        }
        return Err(io_error(std::io::Error::other(format!(
            "GitHub API returned HTTP {code}{}",
            if rate_limited {
                " (rate limit exhausted; set GITHUB_TOKEN)"
            } else {
                ""
            },
        ))));
    }
    if code >= 400 {
        // Same text ureq 2's `Error::Status` rendered:
        // `<final url>: status code <n>[ (redirected from <original url>)]`.
        let redirected_from = response
            .get_redirect_history()
            .filter(|history| history.len() > 1)
            .and_then(<[_]>::first)
            .map(|original| format!(" (redirected from {original})"))
            .unwrap_or_default();
        return Err(io_error(std::io::Error::other(format!(
            "self-check request failed: {}: status code {code}{redirected_from}",
            response.get_uri(),
        ))));
    }

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|err| io_error(err.into_io()))?;

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

/// The `ureq` agent `dq self check` talks to GitHub with.
///
/// Pins the settings the ureq 2 client applied by default, so the ureq 3
/// bump keeps the observable behaviour: a 30 s connect timeout and no other
/// timeout, at most 5 redirects, no proxy picked up from `HTTP(S)_PROXY`
/// (ureq 3 reads it by default; ureq 2 had that behind an off-by-default
/// feature). HTTP 4xx/5xx come back as a response rather than an error so
/// the 403 branch can read `X-RateLimit-Remaining`; the redirect history
/// feeds the `(redirected from …)` suffix of status errors.
fn self_check_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(SELF_CHECK_CONNECT_TIMEOUT))
        .max_redirects(SELF_CHECK_MAX_REDIRECTS)
        .save_redirect_history(true)
        .proxy(None)
        .build();
    ureq::Agent::new_with_config(config)
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

    let updater = update_builder(args.to.as_deref())
        .build()
        .map_err(map_self_update_error)?;
    let status = updater.update().map_err(map_self_update_error)?;
    if status.is_updated() {
        info!("dq updated to v{}", status.version());
    } else {
        info!("dq is already up to date (v{})", status.version());
    }
    Ok(())
}

/// The GitHub-backend builder `dq self update` runs, pinned to `tag` when
/// `--to` was given. Split out of [`run_update`] so the request shape can be
/// checked against a local server without replacing the running binary.
fn update_builder(tag: Option<&str>) -> self_update::backends::github::UpdateBuilder {
    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner(GITHUB_REPO_OWNER)
        .repo_name(GITHUB_REPO_NAME)
        .bin_name("dq")
        .current_version(env!("CARGO_PKG_VERSION"))
        .target(self_update::get_target())
        .show_download_progress(false)
        .show_output(false)
        // 1.x defaults to `self-update/<version>`; keep the User-Agent the
        // 0.44 GitHub backend sent on both the API and the asset download.
        .request_header(
            self_update::http::header::USER_AGENT,
            SELF_UPDATE_USER_AGENT,
        );
    if let Some(tag) = tag {
        builder.release_tag(tag);
    }
    builder
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
    // Failed HTTP traffic — a completed request with a non-2xx status
    // (`NotFound` / `Unauthorized` / `RateLimited` / `HttpStatus`, all of
    // which report `http_status()`) or a request that could not complete
    // (`Transport`) — collapses onto the same `Io`-shape so
    // `exit_code_for_error` produces 5 (`IO_ERROR`) and the user sees the
    // GITHUB_TOKEN hint. The message keeps the text 0.44 produced over its
    // ureq 3 client: `http status: <n>` for a status, the transport error's
    // own text otherwise.
    let network_msg = match &err {
        SuError::Transport(source) => Some(source.to_string()),
        other => other
            .http_status()
            .map(|status| format!("http status: {status}")),
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
            source: std::io::Error::other(format!(
                "self-update failed: {}",
                legacy_failure_text(&other)
            )),
        }),
    }
}

/// The text `self_update` 0.44 rendered for the failures a user can reach
/// on a well-formed GitHub release — no asset for this platform, or `n` at
/// the confirmation prompt; every other error keeps its own `Display`.
fn legacy_failure_text(err: &self_update::errors::Error) -> String {
    use self_update::errors::Error as SuError;
    match err {
        SuError::NoReleaseFound {
            target: Some(target),
            ..
        } => format!("ReleaseError: No asset found for target: `{target}`"),
        SuError::Aborted => "UpdateError: Update aborted".to_owned(),
        other => other.to_string(),
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
    // `run_update` builds its updater through [`update_builder`], so the
    // request shape is observable: the `update_builder_*` tests below point
    // `api_base_url` at the local mock and check the User-Agent on both the
    // release listing and the asset download, failing with 404 before
    // anything is extracted or the binary is replaced. The
    // `ensure_writable_install_dir` helper is covered indirectly by the
    // integration test below. Still untested: `run_update` itself
    // (`current_exe`, the replacement step) — it needs a real release, so the
    // trait-based seam from spec §4.5 remains a follow-up.
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
        spawn_one_shot_http_at("/releases/latest", response_bytes.to_vec()).0
    }

    /// [`spawn_one_shot_http`] with a caller-chosen URL path and a handle
    /// that yields the raw request bytes (request line + headers) once the
    /// response has been written — lets a test assert what the client sent.
    fn spawn_one_shot_http_at(
        path: &str,
        response_bytes: Vec<u8>,
    ) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::Read;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener
            .local_addr()
            .expect("local_addr after successful bind");
        let handle = std::thread::spawn(move || {
            // Accept exactly one connection — the unit test sends exactly
            // one request. Errors are swallowed: if the test client never
            // connects (e.g. it panicked first), there is nothing useful
            // the helper thread can do.
            let Ok((mut socket, _)) = listener.accept() else {
                return Vec::new();
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
            let _ = socket.write_all(&response_bytes);
            total
        });
        (format!("http://{addr}{path}"), handle)
    }

    /// Build a complete `Connection: close` HTTP/1.1 response with a
    /// correct `Content-Length`.
    fn http_response(status_line: &str, extra_headers: &[&str], body: &str) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {status_line}\r\n");
        for header in extra_headers {
            out.push_str(header);
            out.push_str("\r\n");
        }
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        out.into_bytes()
    }

    /// The message of the `std::io::Error` inside a `dq_core::Error::Io`
    /// (the shape `exit_code_for_error` maps to exit 5). Panics on any
    /// other error shape so a test fails loudly when the mapping drifts.
    fn io_source_message(err: &anyhow::Error) -> String {
        match err.downcast_ref::<dq_core::Error>() {
            Some(dq_core::Error::Io { path, source }) => {
                assert_eq!(path.as_str(), ":self-check", "unexpected Io path");
                source.to_string()
            }
            other => panic!("expected dq_core::Error::Io, got: {other:?} ({err:#})"),
        }
    }

    #[test]
    fn run_check_404_message_names_url_and_status() {
        // Pins the user-visible text of a non-403 HTTP failure. The format
        // `<url>: status code <code>` is what `dq self check` printed since
        // the ureq 2 days; the ureq 3 client reproduces it by hand.
        let (url, _req) =
            spawn_one_shot_http_at("/releases/latest", http_response("404 Not Found", &[], ""));
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &url).expect_err("404 must fail");
        assert_eq!(
            io_source_message(&err),
            format!("self-check request failed: {url}: status code 404"),
        );
        assert_eq!(
            crate::exit_code::exit_code_for_error(&err),
            crate::exit_code::IO_ERROR
        );
    }

    #[test]
    fn run_check_500_maps_to_io_error_with_status() {
        let (url, _req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response("500 Internal Server Error", &[], "boom"),
        );
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &url).expect_err("500 must fail");
        assert_eq!(
            io_source_message(&err),
            format!("self-check request failed: {url}: status code 500"),
        );
        assert!(buf.is_empty(), "no output on 500; got: {buf:?}");
    }

    #[test]
    fn run_check_403_with_exhausted_rate_limit_carries_token_hint() {
        let (url, _req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response(
                "403 Forbidden",
                &["X-RateLimit-Remaining: 0"],
                r#"{"message":"API rate limit exceeded"}"#,
            ),
        );
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &url).expect_err("403 must fail");
        assert_eq!(
            io_source_message(&err),
            "GitHub API returned HTTP 403 (rate limit exhausted; set GITHUB_TOKEN)",
        );
        assert_eq!(
            crate::exit_code::exit_code_for_error(&err),
            crate::exit_code::IO_ERROR
        );
    }

    #[test]
    fn run_check_403_without_rate_limit_header_has_no_token_hint() {
        let (url, _req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response("403 Forbidden", &["X-RateLimit-Remaining: 12"], ""),
        );
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &url).expect_err("403 must fail");
        assert_eq!(io_source_message(&err), "GitHub API returned HTTP 403");
    }

    #[test]
    fn run_check_status_error_after_redirect_names_original_url() {
        // ureq 2 rendered a status error reached through a redirect as
        // `<final>: status code <n> (redirected from <original>)`.
        let (final_url, _final_req) =
            spawn_one_shot_http_at("/final", http_response("404 Not Found", &[], ""));
        let location = format!("Location: {final_url}");
        let (start_url, _start_req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response("302 Found", &[location.as_str()], ""),
        );
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &start_url).expect_err("404 must fail");
        assert_eq!(
            io_source_message(&err),
            format!(
                "self-check request failed: {final_url}: status code 404 (redirected from {start_url})"
            ),
        );
    }

    #[test]
    fn run_check_sends_user_agent_and_accept_headers() {
        let (url, req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response("200 OK", &[], r#"{"tag_name":"v0.0.0"}"#),
        );
        let mut buf: Vec<u8> = Vec::new();
        run_check_with_url(&mut buf, &url).expect("200 with a tag must succeed");
        let raw = String::from_utf8(req.join().expect("server thread")).expect("ascii request");
        let lower = raw.to_ascii_lowercase();
        assert!(
            lower.starts_with("get /releases/latest http/1.1\r\n"),
            "unexpected request line: {raw:?}",
        );
        let header_values = |name: &str| -> Vec<String> {
            lower
                .lines()
                .filter_map(|line| line.strip_prefix(name))
                .map(|v| v.trim().to_owned())
                .collect()
        };
        assert_eq!(header_values("user-agent:"), vec!["dq-self-check"]);
        assert_eq!(
            header_values("accept:"),
            vec!["application/vnd.github+json"]
        );
    }

    #[test]
    fn update_builder_sends_the_0_44_user_agent() {
        // The listing request is the first one `update()` makes; a 404 ends
        // the run before any download or binary replacement.
        let (url, req) = spawn_one_shot_http_at(
            "/repos/mazuninky/dq/releases/latest",
            http_response("404 Not Found", &[], "{}"),
        );
        let base = url
            .strip_suffix("/repos/mazuninky/dq/releases/latest")
            .expect("helper URL ends with the requested path");
        let mut builder = update_builder(None);
        builder.api_base_url(base);
        let err = builder
            .build()
            .expect("builder is valid")
            .update()
            .expect_err("404 listing must fail");
        let (code, _, _) = mapped(err);
        assert_eq!(
            code, 5,
            "HTTP status failures stay network failures (exit 5)"
        );
        let raw = String::from_utf8(req.join().expect("server thread")).expect("ascii request");
        let lower = raw.to_ascii_lowercase();
        let agents: Vec<&str> = lower
            .lines()
            .filter_map(|line| line.strip_prefix("user-agent:"))
            .map(str::trim)
            .collect();
        assert_eq!(agents, vec!["rust/self-update"], "request was: {raw:?}");
    }

    #[test]
    fn update_builder_sends_the_same_user_agent_on_the_asset_download() {
        // The listing advertises a newer release whose asset `url` points at
        // a second local server; that server answers the download with 404,
        // so the run fails before anything is extracted or replaced.
        let (asset_url, asset_req) =
            spawn_one_shot_http_at("/asset/1", http_response("404 Not Found", &[], ""));
        let listing = serde_json::json!([{
            "tag_name": "v99999.0.0",
            "created_at": "2026-01-01T00:00:00Z",
            "assets": [{
                "name": format!("dq-v99999.0.0-{}.tar.gz", self_update::get_target()),
                "url": asset_url,
            }],
        }])
        .to_string();
        let (list_url, list_req) = spawn_one_shot_http_at(
            "/repos/mazuninky/dq/releases",
            http_response(
                "200 OK",
                &["Content-Type: application/json"],
                listing.as_str(),
            ),
        );
        let base = list_url
            .strip_suffix("/repos/mazuninky/dq/releases")
            .expect("helper URL ends with the requested path");
        let mut builder = update_builder(None);
        // `dq self update` prompts before downloading; a test has no
        // terminal to answer it.
        builder.api_base_url(base).no_confirm(true);
        let err = builder
            .build()
            .expect("builder is valid")
            .update()
            .expect_err("404 asset download must fail");
        let (code, _, msg) = mapped(err);
        assert_eq!(code, 5, "a failed download is a network failure: {msg}");

        let header_values = |raw: &str, name: &str| -> Vec<String> {
            raw.to_ascii_lowercase()
                .lines()
                .filter_map(|line| line.strip_prefix(name).map(|v| v.trim().to_owned()))
                .collect()
        };
        let list_raw =
            String::from_utf8(list_req.join().expect("listing thread")).expect("ascii request");
        let asset_raw =
            String::from_utf8(asset_req.join().expect("asset thread")).expect("ascii request");
        assert!(
            asset_raw.to_ascii_lowercase().starts_with("get /asset/1 "),
            "the asset server must receive the download, got: {asset_raw:?}"
        );
        assert_eq!(
            header_values(&asset_raw, "user-agent:"),
            vec!["rust/self-update"],
            "download request was: {asset_raw:?}"
        );
        assert_eq!(
            header_values(&list_raw, "user-agent:"),
            header_values(&asset_raw, "user-agent:"),
            "listing request was: {list_raw:?}"
        );
        assert_eq!(
            header_values(&asset_raw, "accept:"),
            vec!["application/octet-stream"],
            "download request was: {asset_raw:?}"
        );
    }

    #[test]
    fn run_check_renders_outcome_from_tag_name() {
        let (url, _req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response("200 OK", &[], r#"{"tag_name":"v0.0.0","name":"x"}"#),
        );
        let mut buf: Vec<u8> = Vec::new();
        run_check_with_url(&mut buf, &url).expect("200 with a tag must succeed");
        assert_eq!(
            String::from_utf8(buf).expect("utf-8"),
            format!(
                "running pre-release version (local: v{}, latest published: v0.0.0)\n",
                env!("CARGO_PKG_VERSION")
            ),
        );
    }

    #[test]
    fn run_check_rejects_non_json_body() {
        let (url, _req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response("200 OK", &[], "<html>not json</html>"),
        );
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &url).expect_err("non-JSON must fail");
        let msg = io_source_message(&err);
        assert!(
            msg.starts_with("GitHub API response was not valid JSON: "),
            "unexpected message: {msg:?}",
        );
    }

    #[test]
    fn run_check_rejects_json_without_tag_name() {
        let (url, _req) = spawn_one_shot_http_at(
            "/releases/latest",
            http_response("200 OK", &[], r#"{"name":"no tag here"}"#),
        );
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &url).expect_err("missing tag must fail");
        assert_eq!(
            io_source_message(&err),
            "GitHub API response did not contain a `tag_name` field",
        );
    }

    #[test]
    fn run_check_connection_refused_maps_to_io_error_naming_url() {
        // "No network" stand-in: bind an ephemeral port, then drop the
        // listener so the connect is refused.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral port")
            .local_addr()
            .expect("local_addr");
        let url = format!("http://{addr}/releases/latest");
        let mut buf: Vec<u8> = Vec::new();
        let err = run_check_with_url(&mut buf, &url).expect_err("refused connect must fail");
        let msg = io_source_message(&err);
        assert!(
            msg.starts_with(&format!("self-check request failed: {url}: ")),
            "unexpected message: {msg:?}",
        );
        assert_eq!(
            crate::exit_code::exit_code_for_error(&err),
            crate::exit_code::IO_ERROR
        );
    }

    /// `(exit code, dq_core error kind, source message)` of a mapped
    /// `self_update` error.
    fn mapped(err: self_update::errors::Error) -> (i32, &'static str, String) {
        let mapped = map_self_update_error(err);
        let code = crate::exit_code::exit_code_for_error(&mapped);
        let domain = mapped
            .downcast_ref::<dq_core::Error>()
            .expect("mapped error must downcast to dq_core::Error");
        match domain {
            dq_core::Error::Io { path, source } | dq_core::Error::WriteIo { path, source } => {
                assert_eq!(path.as_str(), ":self-update", "unexpected path");
                (code, domain.kind_name(), source.to_string())
            }
            other => panic!("expected Io / WriteIo, got: {other:?}"),
        }
    }

    const TOKEN_HINT: &str = "(set GITHUB_TOKEN to raise the GitHub API rate limit)";

    #[test]
    fn map_self_update_error_http_status_is_network_failure() {
        for (status, url) in [
            (404, "https://api.github.com/repos/o/r/releases/latest"),
            (401, "https://api.github.com/x"),
            (403, "https://api.github.com/x"),
            (429, "https://api.github.com/x"),
            (502, "https://api.github.com/x"),
        ] {
            let (code, kind, msg) =
                mapped(self_update::errors::Error::http_status_error(status, url));
            assert_eq!(code, crate::exit_code::IO_ERROR, "status {status}");
            assert_eq!(kind, "io", "status {status}");
            assert_eq!(
                msg,
                format!("self-update network failure: http status: {status} {TOKEN_HINT}"),
            );
        }
    }

    #[test]
    fn map_self_update_error_transport_is_network_failure() {
        let (code, kind, msg) = mapped(self_update::errors::Error::transport(
            std::io::Error::other("connection refused"),
        ));
        assert_eq!(code, crate::exit_code::IO_ERROR);
        assert_eq!(kind, "io");
        assert_eq!(
            msg,
            format!("self-update network failure: connection refused {TOKEN_HINT}"),
        );
    }

    #[test]
    fn map_self_update_error_io_is_write_failure() {
        let (code, kind, msg) = mapped(self_update::errors::Error::Io(std::io::Error::other(
            "rename failed",
        )));
        assert_eq!(code, crate::exit_code::WRITE_FAILED);
        assert_eq!(kind, "write_io");
        assert_eq!(msg, "rename failed");
    }

    #[test]
    fn map_self_update_error_keeps_0_44_text_for_missing_target_asset() {
        let (code, kind, msg) = mapped(self_update::errors::Error::no_release_found_for_target(
            "x86_64-unknown-linux-gnu",
        ));
        assert_eq!((code, kind), (7, "write_io"));
        assert_eq!(
            msg,
            "self-update failed: ReleaseError: No asset found for target: `x86_64-unknown-linux-gnu`"
        );
    }

    #[test]
    fn map_self_update_error_keeps_0_44_text_for_declined_prompt() {
        let (code, kind, msg) = mapped(self_update::errors::Error::Aborted);
        assert_eq!((code, kind), (7, "write_io"));
        assert_eq!(msg, "self-update failed: UpdateError: Update aborted");
    }

    #[test]
    fn map_self_update_error_other_is_write_failure() {
        let (code, kind, msg) = mapped(self_update::errors::Error::no_release_found_for_target(
            "x86_64-unknown-linux-gnu",
        ));
        assert_eq!(code, crate::exit_code::WRITE_FAILED);
        assert_eq!(kind, "write_io");
        assert!(
            msg.starts_with("self-update failed: "),
            "unexpected: {msg:?}"
        );
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
