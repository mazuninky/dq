//! `dq` binary entry point.
//!
//! Exactly five things happen here: SIGPIPE restoration on Unix, clap parse,
//! tracing init, dependency wiring (color resolution + locked stdio), and
//! dispatch to `dq::run` with exit-code mapping. All command logic lives in
//! `dq::commands::*`.

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

fn init_tracing(verbose: u8, quiet: bool) {
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
        .with_target(false)
        .try_init();
}

fn main() {
    restore_sigpipe();
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet);
    let use_color = dq::output::resolve_color(cli.no_color);
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    let result = dq::run(&cli, use_color, &mut stdout, &mut stderr);
    if let Err(err) = result {
        if err.downcast_ref::<dq::SilentError>().is_none() {
            let _ = dq::render_error(&err, &mut stderr);
        }
        std::process::exit(dq::exit_code::exit_code_for_error(&err));
    }
    std::process::exit(dq::exit_code::SUCCESS);
}
