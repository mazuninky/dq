//! `dq skill` — install, update, inspect and remove the agent skill.
//!
//! `install`, `update` and `remove` print a line per thing they did on
//! stdout, and nothing under `--quiet`; `status` reports an object through
//! the reporter, `--quiet` or not, because the object is what was asked for.
//!
//! # Exit codes
//!
//! 0 — done, or nothing installed to work on; 6 — refused
//! ([`crate::skill::Refusal`], nothing written); 5 / 7 — a file could not be
//! read / written.

use std::io::Write;

use crate::cli::{SkillArgs, SkillCommand};
use crate::output::{Reporter, terminal_text};
use crate::skill::{
    self, Edition, Environment, LinkState, Made, Refusal, RemoveReport, Status, Target, WriteReport,
};

/// How many of the files and directories that keep a directory from being
/// empty are named.
const SHOWN_OTHERS: usize = 3;

/// Run the `skill` command in `env`.
///
/// # Errors
///
/// [`skill::SkillError`] as the module documentation says; any error of the
/// reporter or of `out`.
pub fn run(
    args: &SkillArgs,
    quiet: bool,
    env: &Environment,
    reporter: &dyn Reporter,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let edition = Edition::embedded();
    let mut sink = std::io::sink();
    let lines: &mut dyn Write = if quiet { &mut sink } else { &mut *out };
    match &args.command {
        SkillCommand::Install(args) => {
            let report = skill::install(&target(env, args.project)?, &edition, args.force)?;
            write_install(&report, lines)?;
        }
        SkillCommand::Update(args) => {
            let target = target(env, args.project)?;
            match skill::update(&target, &edition, args.force)? {
                Some(report) => write_install(&report, lines)?,
                None => write_not_installed(&target, lines)?,
            }
        }
        SkillCommand::Remove(args) => {
            let target = target(env, args.project)?;
            let report = skill::remove(&target, &edition, args.force)?;
            if report.version.is_none() {
                write_not_installed(&target, lines)?;
            }
            write_remove(&report, lines)?;
        }
        SkillCommand::Status(args) => {
            let status = skill::status(&target(env, args.project)?, &edition)?;
            reporter.report(&status_value(&status, &edition), out)?;
        }
    }
    Ok(())
}

/// The skill the command is about.
fn target(env: &Environment, project: bool) -> Result<Target, skill::SkillError> {
    let target: Result<Target, Refusal> = if project { env.project() } else { env.global() };
    Ok(target?)
}

fn write_not_installed(target: &Target, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "dq has installed no skill in {}",
        terminal_text(target.dir.as_str())
    )
}

fn write_install(report: &WriteReport, out: &mut dyn Write) -> std::io::Result<()> {
    let dir = terminal_text(report.dir.as_str());
    let version = &report.version;
    match &report.previous {
        _ if !report.changed => writeln!(out, "dq skill v{version} in {dir} is up to date")?,
        None => writeln!(out, "installed dq skill v{version} in {dir}")?,
        Some(previous) if previous == version => {
            writeln!(out, "restored dq skill v{version} in {dir}")?;
        }
        Some(previous) => writeln!(out, "updated dq skill v{previous} to v{version} in {dir}")?,
    }
    for path in &report.kept {
        writeln!(
            out,
            "kept {}: it was edited, and v{version} has no such file",
            terminal_text(path)
        )?;
    }
    if let Some((place, made)) = &report.link {
        let place = terminal_text(place.as_str());
        match made {
            Made::Nothing => {}
            Made::Created => writeln!(out, "linked {place}")?,
            Made::Replaced => writeln!(out, "replaced the link {place}")?,
        }
    }
    Ok(())
}

fn write_remove(report: &RemoveReport, out: &mut dyn Write) -> std::io::Result<()> {
    let dir = terminal_text(report.dir.as_str());
    if let Some(version) = &report.version {
        writeln!(out, "removed dq skill v{version} from {dir}")?;
    }
    if let Some(place) = &report.link {
        writeln!(out, "removed the link {}", terminal_text(place.as_str()))?;
    }
    if report.dir_kept && report.left.is_empty() {
        // What nobody counts as a file of theirs: what a write that was cut
        // short left, what a system puts there, empty directories.
        writeln!(
            out,
            "kept {dir}: it is not empty ({})",
            terminal_text(&skill::listed_up_to(&report.other, SHOWN_OTHERS))
        )?;
    } else if report.dir_kept {
        writeln!(
            out,
            "kept {dir}: not dq's are {}",
            terminal_text(&skill::listed(&report.left))
        )?;
    }
    Ok(())
}

/// The object `dq skill status` reports. Every key is always there and none
/// is null, so that each output format can hold it: what does not apply is
/// an empty string or list.
fn status_value(status: &Status, edition: &Edition) -> serde_json::Value {
    let (place, link) = match &status.link {
        Some((place, state)) => (place.as_str(), link_name(state)),
        None => ("", "not_wanted"),
    };
    serde_json::json!({
        "path": status.dir.as_str(),
        "installed": status.state.is_installed(),
        "state": status.state.name(),
        "version": status.version.as_deref().unwrap_or_default(),
        "dq_version": edition.version,
        "edited": status.edited,
        "missing": status.missing,
        "colliding": status.colliding,
        "in_the_way": status.in_the_way,
        "unlisted": status.unlisted,
        "link": place,
        "link_state": link,
    })
}

/// The name of a link's state in the output of `dq skill status`.
fn link_name(state: &LinkState) -> &'static str {
    match state {
        LinkState::Ours | LinkState::Alike | LinkState::Shared => "ok",
        LinkState::Missing => "missing",
        LinkState::Elsewhere(_) => "elsewhere",
        LinkState::Taken | LinkState::Blocked(_) => "taken",
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use crate::exit_code::{INVALID_INPUT, exit_code_for_error};
    use crate::output::JsonReporter;
    use clap::Parser;
    use pretty_assertions::assert_eq;

    /// `dq skill ARGS` in `env`: what it prints.
    fn dq(args: &[&str], env: &Environment) -> anyhow::Result<String> {
        let argv = ["dq", "skill"].iter().chain(args);
        let cli = Cli::try_parse_from(argv).expect("arguments");
        let Command::Skill(args) = &cli.command else {
            panic!("not `dq skill`");
        };
        let mut out = Vec::new();
        run(args, cli.quiet, env, &JsonReporter, &mut out)?;
        Ok(String::from_utf8(out).expect("UTF-8"))
    }

    /// As root, in a home directory of another user's: nothing is written,
    /// `--force` or not, and `status` works.
    #[test]
    fn root_is_refused_in_another_users_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let owner = skill::testing::not_roots(tmp.path());
        let home = camino::Utf8Path::from_path(tmp.path()).expect("UTF-8");
        let mut env = Environment {
            home: Some(home.as_str().into()),
            ..Environment::default()
        };
        // Installed by whoever runs the tests, so that there is something to
        // refuse.
        dq(&["install"], &env).expect("install");
        env.euid = Some(0);
        for command in ["install", "update", "remove"] {
            for command in [&[command][..], &[command, "--force"]] {
                let err = dq(command, &env).expect_err("refused");
                assert_eq!(exit_code_for_error(&err), INVALID_INPUT, "{command:?}");
                assert_eq!(
                    err.to_string(),
                    format!(
                        "{home} belongs to the user with id {owner}, and dq runs as root: run it \
                         without sudo, or what it writes there would be root's"
                    )
                );
            }
        }
        assert!(home.join(".agents/skills/dq/SKILL.md").is_file());
        let status = dq(&["status"], &env).expect("status");
        assert!(status.contains(r#""state": "up_to_date""#), "{status}");
    }

    /// Root in a checkout of another user's is what a container or a CI job
    /// is; the home directory, foreign too, is not looked at.
    #[test]
    fn root_works_in_another_users_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        skill::testing::not_roots(tmp.path());
        let dir = camino::Utf8Path::from_path(tmp.path()).expect("UTF-8");
        let env = Environment {
            home: Some(dir.as_str().into()),
            current_dir: Some(dir.to_path_buf()),
            euid: Some(0),
            ..Environment::default()
        };
        for command in ["install", "update", "status", "remove"] {
            dq(&[command, "--project"], &env).expect(command);
        }
        assert!(!dir.join(".agents/skills/dq").exists());
    }
}
