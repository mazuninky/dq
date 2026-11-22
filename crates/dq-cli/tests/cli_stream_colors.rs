//! Each of the two streams has a colour of its own: an error and the log are
//! coloured when stderr is a terminal, the output when stdout is — neither
//! follows the other. `CLICOLOR_FORCE` colours both, so it cannot tell them
//! apart: the tests here set one stream only, through `dq::run` and, for what
//! `main` wires, on a pseudo-terminal.

use std::path::Path;

use dq::output::StreamColors;
use pretty_assertions::assert_eq;

const ESC: char = '\u{1b}';

/// The exit code, stdout and stderr of a command.
type Outcome = (i32, String, String);

/// Which of the streams of a command has colour when both may.
#[derive(Clone, Copy)]
struct Coloured {
    stdout: bool,
    stderr: bool,
}

const STDOUT_ONLY: Coloured = Coloured {
    stdout: true,
    stderr: false,
};
const STDERR_ONLY: Coloured = Coloured {
    stdout: false,
    stderr: true,
};
const NEITHER: Coloured = Coloured {
    stdout: false,
    stderr: false,
};

fn dir_with_files() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in [
        ("good.json", "{\"a\": {\"b\": 1}}\n"),
        ("other.json", "{\"a\": {\"b\": 2}}\n"),
        ("bad.json", "{\"a\": 1,\n  \"b\": x}\n"),
        ("mixed.xml", "<a>text<b/>tail</a>\n"),
    ] {
        std::fs::write(dir.path().join(name), text).expect("seed");
    }
    dir
}

/// `plain` has no colour, `coloured` has it on the streams `on` names, and
/// the same on the others.
fn assert_colour_is_on(on: Coloured, plain: &Outcome, coloured: &Outcome, args: &[&str]) {
    assert_eq!(plain.0, coloured.0, "{args:?}");
    for (on, plain, coloured) in [
        (on.stdout, &plain.1, &coloured.1),
        (on.stderr, &plain.2, &coloured.2),
    ] {
        assert!(!plain.contains(ESC), "{args:?}: {plain:?}");
        assert_eq!(coloured.contains(ESC), on, "{args:?}: {coloured:?}");
        assert!(on || plain == coloured, "{args:?}: {coloured:?}");
    }
}

/// `dq ARGS`, its streams coloured as `colors`, in this process. The file
/// names of `args` are those in `dir`.
fn run_in_process(colors: impl Into<StreamColors>, dir: &Path, args: &[&str]) -> Outcome {
    use clap::Parser;
    let args = args.iter().map(|arg| {
        if arg.ends_with(".json") {
            dir.join(arg).to_str().expect("UTF-8 path").to_owned()
        } else {
            (*arg).to_owned()
        }
    });
    let cli = dq::Cli::try_parse_from(std::iter::once("dq".to_owned()).chain(args)).expect("args");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = dq::run(&cli, colors, &mut out, &mut err);
    (
        result.map_or_else(|err| dq::exit_code::exit_code_for_error(&err), |()| 0),
        String::from_utf8(out).expect("UTF-8 stdout"),
        String::from_utf8(err).expect("UTF-8 stderr"),
    )
}

#[test]
fn a_stream_has_the_colour_run_is_given_for_it() {
    let dir = dir_with_files();
    for (args, on, code) in [
        (&["validate", "bad.json"][..], STDERR_ONLY, 4),
        (&["validate", "--check", "bad.json"], STDERR_ONLY, 4),
        (&["get", "good.json", "/a"], STDOUT_ONLY, 0),
        (&["convert", "good.json"], STDOUT_ONLY, 0),
        (
            &["diff", "--unified", "good.json", "other.json"],
            STDOUT_ONLY,
            0,
        ),
        (&["--diff", "set", "good.json", "/a/b", "2"], STDOUT_ONLY, 0),
        // A machine-readable format has no colour on either.
        (&["-F", "json", "validate", "bad.json"], NEITHER, 4),
        (&["-F", "json", "get", "good.json", "/a"], NEITHER, 0),
    ] {
        let run = |colors: StreamColors| run_in_process(colors, dir.path(), args);
        let plain = run(false.into());
        let coloured = run(true.into());
        assert_eq!(plain.0, code, "{args:?}: {plain:?}");
        assert_colour_is_on(on, &plain, &coloured, args);
        assert_eq!(
            run(StreamColors {
                stdout: true,
                stderr: false
            }),
            (code, coloured.1.clone(), plain.2.clone()),
            "{args:?}"
        );
        assert_eq!(
            run(StreamColors {
                stdout: false,
                stderr: true
            }),
            (code, plain.1, coloured.2),
            "{args:?}"
        );
    }
}

#[cfg(unix)]
mod terminal {
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::ptr::null_mut;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use pretty_assertions::assert_eq;

    use super::{
        Coloured, ESC, NEITHER, Outcome, STDERR_ONLY, STDOUT_ONLY, assert_colour_is_on,
        dir_with_files,
    };

    /// Only a hang takes this long.
    const LIMIT: Duration = Duration::from_secs(60);
    /// What ends the output on the terminal: dq prints no NUL.
    const END: &[u8] = b"\0end\0";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Stream {
        Stdout,
        Stderr,
    }

    fn dq(dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dq"));
        // No `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, `RUST_LOG`.
        cmd.env_clear()
            .env("HOME", "/tmp")
            .env("TERM", "xterm-256color")
            .current_dir(dir)
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    /// `dq ARGS` with both streams on pipes, coloured when `force`.
    fn on_pipes(dir: &Path, args: &[&str], force: bool) -> Outcome {
        let mut cmd = dq(dir, args);
        if force {
            cmd.env("CLICOLOR_FORCE", "1");
        }
        let out = cmd.output().expect("run dq");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("UTF-8 stdout"),
            String::from_utf8(out.stderr).expect("UTF-8 stderr"),
        )
    }

    /// The two ends of a new pseudo-terminal: the one a test reads, and the
    /// terminal.
    fn open_pty() -> io::Result<(File, File)> {
        let (mut reader, mut terminal) = (-1, -1);
        // SAFETY: both file descriptors are written to live integers; the
        // null pointers ask for no name, and the default settings and size.
        let opened = unsafe {
            libc::openpty(
                &raw mut reader,
                &raw mut terminal,
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if opened != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openpty` opened both, and nothing else owns them.
        let (reader, terminal) =
            unsafe { (File::from_raw_fd(reader), File::from_raw_fd(terminal)) };
        for end in [&reader, &terminal] {
            // SAFETY: the file descriptor is open for as long as `end` is.
            if unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok((reader, terminal))
    }

    /// Reads on a thread of its own up to the end of `from`, or to [`END`].
    fn read_to_end(mut from: impl Read + Send + 'static) -> mpsc::Receiver<io::Result<Vec<u8>>> {
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            let mut all = Vec::new();
            let mut chunk = [0; 4096];
            let result = loop {
                match from.read(&mut chunk) {
                    Ok(0) => break Ok(all),
                    Ok(n) => all.extend_from_slice(&chunk[..n]),
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => break Err(err),
                }
                if let Some(output) = all.strip_suffix(END) {
                    break Ok(output.to_vec());
                }
            };
            // The test has given up when there is nobody to receive it.
            let _ = send.send(result);
        });
        receive
    }

    /// `dq ARGS` with `stream` on a terminal and the other on a pipe; `None`
    /// when no pseudo-terminal can be opened.
    ///
    /// The terminal stays open here until all that was written to it is read:
    /// a system may drop what is left once the last writer closes it.
    fn on_a_terminal(stream: Stream, dir: &Path, args: &[&str]) -> Option<Outcome> {
        let (reader, mut terminal) = match open_pty() {
            Ok(pty) => pty,
            Err(err) => {
                eprintln!("skipped: no pseudo-terminal: {err}");
                return None;
            }
        };
        let shown = read_to_end(reader);
        let end = Stdio::from(terminal.try_clone().expect("dup"));
        let mut cmd = dq(dir, args);
        match stream {
            Stream::Stdout => cmd.stdout(end).stderr(Stdio::piped()),
            Stream::Stderr => cmd.stdout(Stdio::piped()).stderr(end),
        };
        let mut child = cmd.spawn().expect("spawn dq");
        let piped = match stream {
            Stream::Stdout => read_to_end(child.stderr.take().expect("a pipe")),
            Stream::Stderr => read_to_end(child.stdout.take().expect("a pipe")),
        };
        // The pipe ends when dq exits.
        let Ok(piped) = piped.recv_timeout(LIMIT) else {
            child.kill().expect("kill dq");
            child.wait().expect("wait for dq");
            panic!("{args:?} did not end");
        };
        let code = child.wait().expect("wait for dq").code();
        terminal.write_all(END).expect("write to the terminal");
        let shown = shown
            .recv_timeout(LIMIT)
            .unwrap_or_else(|_| panic!("no end of the output of {args:?}"))
            .expect("read the terminal");
        drop(terminal);

        let piped = String::from_utf8(piped.expect("read the pipe")).expect("UTF-8");
        // A terminal ends a line with both.
        let shown = String::from_utf8(shown)
            .expect("UTF-8")
            .replace("\r\n", "\n");
        let (stdout, stderr) = match stream {
            Stream::Stdout => (shown, piped),
            Stream::Stderr => (piped, shown),
        };
        Some((code.expect("exit code"), stdout, stderr))
    }

    const CASES: [(&[&str], Coloured, i32); 8] = [
        (&["get", "bad.json", "/a"], STDERR_ONLY, 3),
        (&["validate", "bad.json"], STDERR_ONLY, 4),
        (&["--diff", "set", "bad.json", "/a", "2"], STDERR_ONLY, 3),
        (&["get", "good.json", "/a"], STDOUT_ONLY, 0),
        (
            &["diff", "--unified", "good.json", "other.json"],
            STDOUT_ONLY,
            0,
        ),
        (&["--diff", "set", "good.json", "/a/b", "2"], STDOUT_ONLY, 0),
        (&["-F", "json", "validate", "bad.json"], NEITHER, 4),
        (&["-F", "json", "get", "good.json", "/a"], NEITHER, 0),
    ];

    /// On a terminal `stream` is what `CLICOLOR_FORCE` makes it, the other
    /// stream what it is on a pipe.
    fn assert_only_coloured(stream: Stream) {
        let dir = dir_with_files();
        for (args, on, code) in CASES {
            let plain = on_pipes(dir.path(), args, false);
            let forced = on_pipes(dir.path(), args, true);
            assert_eq!(plain.0, code, "{args:?}: {plain:?}");
            assert_colour_is_on(on, &plain, &forced, args);
            let Some(shown) = on_a_terminal(stream, dir.path(), args) else {
                return;
            };
            let expected = match stream {
                Stream::Stdout => (code, forced.1, plain.2),
                Stream::Stderr => (code, plain.1, forced.2),
            };
            assert_eq!(shown, expected, "{stream:?} {args:?}");
        }
    }

    #[test]
    fn an_error_is_coloured_on_a_terminal_and_the_output_on_a_pipe_is_not() {
        assert_only_coloured(Stream::Stderr);
    }

    #[test]
    fn the_output_is_coloured_on_a_terminal_and_an_error_on_a_pipe_is_not() {
        assert_only_coloured(Stream::Stdout);
    }

    /// The log is on stderr as the error is. Here one command writes both
    /// streams, and each would have colour.
    #[test]
    fn the_log_has_the_colour_of_stderr() {
        let dir = dir_with_files();
        let args = &["get", "mixed.xml", ""];
        for stream in [Stream::Stdout, Stream::Stderr] {
            let Some((code, stdout, stderr)) = on_a_terminal(stream, dir.path(), args) else {
                return;
            };
            assert_eq!(code, 0, "{stderr:?}");
            assert!(stderr.contains("WARN"), "{stderr:?}");
            assert!(stdout.contains("texttail"), "{stdout:?}");
            assert_eq!(
                (stdout.contains(ESC), stderr.contains(ESC)),
                (stream == Stream::Stdout, stream == Stream::Stderr),
                "{stream:?}: {stdout:?} {stderr:?}"
            );
        }
    }
}
