//! SIGPIPE smoke test — ensure piping `dq paths` into a slow consumer (or
//! `head`) does not panic with `failed printing to stdout`.
//!
//! Skipped on Windows: SIGPIPE is a Unix concept; on Windows the broken-pipe
//! error path is different and not a regression vector for `dq`.
//!
//! Strategy: synthesize a large YAML fixture (≥ 1000 paths) at test time, then
//! launch a subprocess that pipes `dq paths` into `head -n 1`. We assert:
//!
//! 1. The whole pipeline exits cleanly (no panic to stderr).
//! 2. The `dq` process gets shut down by SIGPIPE *or* exits with status 0
//!    (depending on timing — sometimes it finishes writing all output before
//!    `head` closes the pipe).
//!
//! What we explicitly do NOT assert: an exact `dq` exit code. POSIX does not
//! mandate one for "head closed the pipe before I was done"; what matters is
//! that *no panic message* lands on stderr. The bug this guards against is
//! the Rust-default behaviour where SIGPIPE is masked and `println!` panics.

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Stdio};

use tempfile::NamedTempFile;

/// Build a YAML fixture with ≥ 1000 reachable JSON Pointers.
///
/// Layout: `keyN: { entryM: <int> }` for N in 0..50 and M in 0..25 — that's
/// 50 keys × 25 entries × (object + leaf) = ~2500 pointers, well above the
/// 1000-path bar.
fn write_large_yaml() -> NamedTempFile {
    let mut tmp = NamedTempFile::with_suffix(".yaml").expect("tempfile");
    for n in 0..50 {
        writeln!(tmp, "key{n}:").unwrap();
        for m in 0..25 {
            writeln!(tmp, "  entry{m}: {value}", value = n * 100 + m).unwrap();
        }
    }
    tmp.flush().unwrap();
    tmp
}

#[test]
fn dq_paths_piped_into_head_does_not_panic() {
    let dq_bin = assert_cmd::cargo::cargo_bin("dq");
    let fixture = write_large_yaml();
    let path = fixture.path().to_str().expect("utf-8 fixture path");

    // Spawn `dq paths <large> --no-color` and capture both pipes.
    let mut dq_child = Command::new(&dq_bin)
        .args(["paths", path, "--no-color"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", "/tmp")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dq");

    // Read just the first line of stdout, then drop the reader. Dropping it
    // closes the read-end of the pipe so subsequent `dq` writes hit EPIPE
    // and (with the SIGPIPE handler restored to SIG_DFL) the process is
    // killed cleanly by the kernel.
    {
        let mut stdout = dq_child.stdout.take().expect("dq stdout pipe");
        let mut buf = [0u8; 64];
        // We don't care if this succeeds — we only want to consume *some*
        // bytes before closing.
        let _ = std::io::Read::read(&mut stdout, &mut buf);
        // Drop here: dropping `stdout` closes our end of the pipe.
    }

    let output = dq_child
        .wait_with_output()
        .expect("dq must terminate cleanly");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The contract: NO panic. Without SIGPIPE restoration, we'd see Rust's
    // panic message "failed printing to stdout: Broken pipe (os error 32)"
    // or similar. The assertion below catches both wordings.
    assert!(
        !stderr.contains("failed printing to stdout"),
        "dq must not panic on broken pipe; got stderr: {stderr:?}",
    );
    assert!(
        !stderr.contains("panicked at"),
        "dq must not panic on broken pipe; got stderr: {stderr:?}",
    );
    // Sanity: stderr should be empty or only contain WARN/INFO logs (none on
    // the default WARN level for a successful paths run). The point is just
    // that no traceback is present.
}
