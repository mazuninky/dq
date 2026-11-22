//! The scratch directory of the rule tests (`cli_rule_error_reports.rs`,
//! `cli_rule_error_fix.rs`, `cli_rule_sources.rs`, `cli_jq_halt.rs`): the
//! `dq` binary run in it, with what varies between runs taken out of its
//! stderr.

use std::path::PathBuf;
use std::time::Duration;

use assert_cmd::Command;
use tempfile::TempDir;

/// How long one `dq` may run before it is killed. A run takes milliseconds;
/// one that never ends must not hang the suite.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// What one `dq` run left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The exit code.
    pub code: i32,
    /// stdout, as it was written.
    pub stdout: String,
    /// stderr without the timestamps of its log lines (`ERROR rule did not
    /// run …`), the scratch directory spelled `ROOT`.
    pub stderr: String,
}

/// A temporary directory `dq` runs in.
pub struct Scratch {
    dir: TempDir,
}

impl Scratch {
    /// A directory that holds `files` (path and contents).
    pub fn new(files: &[(&str, &str)]) -> Self {
        let scratch = Self {
            dir: TempDir::new().expect("tempdir"),
        };
        for (path, text) in files {
            scratch.write(path, text);
        }
        scratch
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    /// Write a file, and the directories above it.
    pub fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    /// Run `dq --no-color ARGS` in the directory, with a clean environment.
    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with_stdin(args, "")
    }

    /// [`Scratch::run`] with `stdin`.
    pub fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Run {
        let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        let out = cmd
            .env("HOME", self.dir.path())
            .current_dir(self.dir.path())
            .timeout(DEADLINE)
            .arg("--no-color")
            .args(args)
            .write_stdin(stdin)
            .output()
            .expect("run dq");
        let code = out
            .status
            .code()
            .unwrap_or_else(|| panic!("dq {args:?} was killed: {:?}", out.status));
        Run {
            code,
            stdout: String::from_utf8(out.stdout).expect("UTF-8 stdout"),
            stderr: self.steady(&String::from_utf8(out.stderr).expect("UTF-8 stderr")),
        }
    }

    /// `stderr` as [`Run::stderr`] holds it.
    fn steady(&self, stderr: &str) -> String {
        let is_timestamp = |word: &str| word.starts_with("20") && word.ends_with('Z');
        let mut text: String = stderr
            .split_inclusive('\n')
            .map(|line| match line.split_once(' ') {
                Some((word, rest)) if is_timestamp(word) => rest.trim_start_matches(' '),
                _ => line,
            })
            .collect();
        // The canonical spelling first: the other may be its suffix.
        let real = std::fs::canonicalize(self.dir.path()).expect("canonicalize");
        for root in [real.as_path(), self.dir.path()] {
            text = text.replace(root.to_str().expect("UTF-8 path"), "ROOT");
        }
        text
    }
}
