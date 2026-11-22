//! `dq` binary entry point.
//!
//! Exactly six things happen here: SIGPIPE restoration on Unix, clap parse,
//! tracing init, dependency wiring (color resolution + locked stdio),
//! dispatch to `dq::run` with exit-code mapping, and — once the output is
//! out and the exit code is settled — the update of an installed skill
//! (`dq::skill::FirstRun`). All command logic lives in `dq::commands::*`.
//!
//! A process that ends before that — clap's own exit (`--help`, `--version`,
//! a usage error it raises while parsing), a death by SIGPIPE, a panic —
//! does without: the next run does it.

use std::io::Write;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use dq::Cli;

#[cfg(unix)]
fn restore_sigpipe() {
    // Without this, piping output into `head` (or any consumer that closes
    // stdin early) panics with "failed printing to stdout: Broken pipe".
    // SAFETY: `signal(2)` is async-signal-safe; we install the default
    // handler before any output happens.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_sigpipe() {}

/// Install the `tracing` subscriber. Logs go to stderr — stdout carries the
/// command's output — with ANSI escapes only when `ansi` is set (see
/// `dq::output::resolve_log_color`).
fn init_tracing(verbose: u8, quiet: bool, ansi: bool) {
    let default_directive = if quiet {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_directive));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_target(false)
        .fmt_fields(dq::output::log_fields())
        .try_init();
}

fn main() {
    restore_sigpipe();
    let cli = Cli::parse();
    let colors = dq::output::StreamColors {
        stdout: dq::output::resolve_color(cli.no_color),
        stderr: dq::output::resolve_log_color(cli.no_color),
    };
    init_tracing(cli.verbose, cli.quiet, colors.stderr);
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    // Not locked for the run, unlike stdout: the subscriber logs to stderr
    // from the `--parallel` workers too, which would wait for this thread
    // while it waits for them.
    let mut stderr = std::io::stderr();
    let result = dq::run(&cli, colors, &mut stdout, &mut stderr);
    let code = match result {
        Ok(()) => dq::exit_code::SUCCESS,
        Err(err) => {
            if err.downcast_ref::<dq::SilentError>().is_none() {
                let _ = dq::render_error(&err, colors.stderr, &mut stderr);
            }
            dq::exit_code::exit_code_for_error(&err)
        }
    };
    // As at a process exit, where nobody is left to be told of a failure.
    let _ = stdout.flush();
    let first_run = dq::skill::FirstRun::after(&cli.command, !cfg!(debug_assertions));
    first_run.update_skill(&dq::skill::Edition::embedded());
    std::process::exit(code);
}
