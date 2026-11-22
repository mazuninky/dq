//! Bringing an installed skill to the version of the binary the first time a
//! new version runs, after whatever command that was.
//!
//! A package manager that replaces the binary has no moment to update the
//! skill; the first run of the new binary is the moment there is. So this is
//! an exception to "one command, one operation", and a narrow one: it writes
//! only into the directory the user had dq install the skill to, only when
//! nobody touched it since, only when its version is behind, and it never
//! changes what the command printed or how it exits.

use camino::Utf8Path;

use super::content::Edition;
use super::error::SkillError;
use super::manifest::{MANIFEST, Manifest};
use super::ops::{Freshness, write_files};
use super::place::{Place, other_owner, place, try_lock};
use super::survey::{FileState, Installed, files, read_manifest};
use super::{Environment, SKILL_DIR};
use crate::cli::Command;

/// What decides whether the skill is looked at. `main` fills it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstRun {
    /// This is a release build. A debug build is someone working on dq, and
    /// must not touch that someone's skill.
    pub release_build: bool,
    /// The command was `dq skill` or `dq self`, which see to the skill
    /// themselves.
    pub own_command: bool,
    /// The process environment.
    pub env: Environment,
}

/// Why the skill was left as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skipped {
    /// Not a release build.
    NotARelease,
    /// The command sees to the skill itself.
    OwnCommand,
    /// `CI` is set.
    Ci,
    /// `DQ_NO_SKILL_UPDATE` is set.
    OptedOut,
    /// There is no home directory.
    NoHome,
    /// dq installed no skill.
    NotInstalled,
    /// The manifest cannot be used, or the directory is none.
    Unusable,
    /// The installed skill is of this version or a newer one.
    NotOlder,
    /// Root, in another user's home directory.
    ForeignHome,
    /// The skill directory is another user's.
    OtherOwner,
    /// Another dq is writing, or removing the skill.
    Busy,
    /// A file of dq's was edited or removed, or another one is where this
    /// dq has one to write. What else is in the directory does not matter.
    Changed,
}

/// What came of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The skill was left as it is.
    Skipped(Skipped),
    /// The skill is now of the version of the edition.
    Updated,
    /// Reading or writing failed, with the message.
    Failed(String),
}

impl FirstRun {
    /// After `command`, in the environment of this process.
    #[must_use]
    pub fn after(command: &Command, release_build: bool) -> Self {
        Self {
            release_build,
            own_command: matches!(command, Command::Skill(_) | Command::Self_(_)),
            env: Environment::from_process(),
        }
    }

    /// Update the skill of the home directory to `edition`, if all that has
    /// to hold for that does. Prints nothing, whatever happens; the result is
    /// for tests.
    pub fn update_skill(&self, edition: &Edition) -> Outcome {
        let outcome = self.attempt(edition).unwrap_or_else(|err| match err {
            SkillError::Refused(_) => Outcome::Skipped(Skipped::NoHome),
            SkillError::File(err) => Outcome::Failed(err.to_string()),
        });
        match &outcome {
            Outcome::Skipped(_) => {}
            Outcome::Updated => tracing::debug!("the dq skill is now v{}", edition.version),
            Outcome::Failed(err) => tracing::debug!("the dq skill was not updated: {err}"),
        }
        outcome
    }

    /// Why the skill is not even looked at, from what is known without
    /// touching the disk.
    fn closed(&self) -> Option<Skipped> {
        if !self.release_build {
            Some(Skipped::NotARelease)
        } else if self.own_command {
            Some(Skipped::OwnCommand)
        } else if self.env.ci.is_some() {
            Some(Skipped::Ci)
        } else if self.env.opted_out() {
            Some(Skipped::OptedOut)
        } else {
            None
        }
    }

    fn attempt(&self, edition: &Edition) -> Result<Outcome, SkillError> {
        if let Some(why) = self.closed() {
            return Ok(Outcome::Skipped(why));
        }
        let dir = self.env.home()?.join(SKILL_DIR);
        // The only thing most runs do: no skill is installed, and one `stat`
        // says so.
        if std::fs::symlink_metadata(dir.join(MANIFEST)).is_err() {
            return Ok(Outcome::Skipped(Skipped::NotInstalled));
        }
        match place(&dir)? {
            Place::Directory => {}
            Place::Absent => return Ok(Outcome::Skipped(Skipped::NotInstalled)),
            Place::Link | Place::Other(_) => return Ok(Outcome::Skipped(Skipped::Unusable)),
        }
        if self.env.is_root_in_foreign_home() {
            return Ok(Outcome::Skipped(Skipped::ForeignHome));
        }
        // What most runs with a skill do is this far: one small file is read.
        // Of a skill that is older and was changed, up to the change.
        let user = self.env.euid;
        if let Decision::Skip(why) = decide(&dir, user, edition)? {
            return Ok(Outcome::Skipped(why));
        }
        let Some(_lock) = try_lock(&dir)? else {
            return Ok(Outcome::Skipped(Skipped::Busy));
        };
        update_locked(&dir, user, edition)
    }
}

/// Update the skill in `dir`, which is locked, if it still is to be: another
/// may have been here before the lock was this run's.
fn update_locked(
    dir: &Utf8Path,
    user: Option<u32>,
    edition: &Edition,
) -> Result<Outcome, SkillError> {
    match decide(dir, user, edition)? {
        Decision::Skip(why) => Ok(Outcome::Skipped(why)),
        Decision::Write(manifest, files) => write_over(dir, edition, &manifest, &files),
    }
}

/// Write `edition` over `files`, what was seen of them.
fn write_over(
    dir: &Utf8Path,
    edition: &Edition,
    manifest: &Manifest,
    files: &[FileState],
) -> Result<Outcome, SkillError> {
    match write_files(dir, edition, Some(manifest), files, false) {
        Ok(_) => Ok(Outcome::Updated),
        // A user saved a file in between.
        Err(SkillError::Refused(_)) => Ok(Outcome::Skipped(Skipped::Changed)),
        Err(err) => Err(err),
    }
}

/// What [`decide`] came to.
enum Decision {
    /// The skill is left as it is.
    Skip(Skipped),
    /// It is to be updated: its manifest, and what was seen of every file.
    Write(Manifest, Vec<FileState>),
}

/// Whether the skill in `dir` is to be updated to `edition` by `user`. The files are
/// looked at one by one up to the first that was changed. What else is in the
/// directory does not matter, and is not looked at.
fn decide(
    dir: &Utf8Path,
    user: Option<u32>,
    edition: &Edition,
) -> Result<Decision, dq_core::Error> {
    // Before anything of it is read.
    if user.is_some_and(|user| other_owner(dir, user).is_some()) {
        return Ok(Decision::Skip(Skipped::OtherOwner));
    }
    let manifest = match read_manifest(dir)? {
        Installed::No => return Ok(Decision::Skip(Skipped::NotInstalled)),
        Installed::Unusable(_) => return Ok(Decision::Skip(Skipped::Unusable)),
        Installed::Yes(manifest) => manifest,
    };
    if Freshness::of(&manifest.dq_version, edition.version) != Freshness::Older {
        return Ok(Decision::Skip(Skipped::NotOlder));
    }
    let mut seen = Vec::new();
    for file in files(dir, Some(&manifest), edition)? {
        let file = file?;
        if file.is_change() {
            return Ok(Decision::Skip(Skipped::Changed));
        }
        seen.push(file);
    }
    Ok(Decision::Write(manifest, seen))
}

#[cfg(test)]
mod tests {
    use super::super::content::SkillFile;
    use super::super::{Target, install, status};
    use super::*;
    use crate::skill::State;
    use pretty_assertions::assert_eq;
    use std::fs;

    const V1: Edition = Edition {
        version: "2026.1.0",
        files: &[SkillFile {
            path: "SKILL.md",
            text: "one",
        }],
    };
    const V2: Edition = Edition {
        version: "2026.10.0",
        files: &[SkillFile {
            path: "SKILL.md",
            text: "two",
        }],
    };

    /// V2, and a file it has that V1 has not.
    const V3: Edition = Edition {
        version: "2026.11.0",
        files: &[
            SkillFile {
                path: "SKILL.md",
                text: "three",
            },
            SkillFile {
                path: "new.md",
                text: "new",
            },
        ],
    };

    /// What comes of `run`, asked until it is not that another is busy: a
    /// process that another test forks has this one's locks until it execs.
    fn update(run: &FirstRun, edition: &Edition) -> Outcome {
        let start = std::time::Instant::now();
        loop {
            let outcome = run.update_skill(edition);
            if outcome != Outcome::Skipped(Skipped::Busy) || start.elapsed().as_secs() > 30 {
                return outcome;
            }
            std::thread::yield_now();
        }
    }

    /// A run of a release build in a home directory of its own.
    fn run() -> (tempfile::TempDir, FirstRun) {
        let home = tempfile::tempdir().expect("tempdir");
        let run = FirstRun {
            release_build: true,
            own_command: false,
            env: Environment {
                home: Some(home.path().into()),
                ..Environment::default()
            },
        };
        (home, run)
    }

    /// [`run`], with V1 installed.
    fn run_with_v1() -> (tempfile::TempDir, FirstRun, Target) {
        let (home, run) = run();
        let target = run.env.global().expect("target");
        install(&target, &V1, false).expect("install");
        (home, run, target)
    }

    #[test]
    fn an_older_skill_is_updated_once() {
        let (_home, run, target) = run_with_v1();
        assert_eq!(update(&run, &V2), Outcome::Updated);
        assert_eq!(status(&target, &V2).expect("status").state, State::UpToDate);
        assert_eq!(
            update(&run, &V2),
            Outcome::Skipped(Skipped::NotOlder),
            "the second run"
        );
        assert_eq!(
            update(&run, &V1),
            Outcome::Skipped(Skipped::NotOlder),
            "an older dq on the same machine"
        );
    }

    #[test]
    fn where_nothing_is_installed_nothing_is_made() {
        let (home, run) = run();
        assert_eq!(update(&run, &V2), Outcome::Skipped(Skipped::NotInstalled));
        assert_eq!(fs::read_dir(home.path()).expect("home").count(), 0);
    }

    #[test]
    fn every_condition_keeps_the_skill_as_it_is() {
        let (_home, run, target) = run_with_v1();
        let some = |value: &str| Some(value.into());
        let with = |change: &dyn Fn(&mut FirstRun)| {
            let mut run = run.clone();
            change(&mut run);
            update(&run, &V2)
        };
        for (outcome, why) in [
            (with(&|r| r.release_build = false), Skipped::NotARelease),
            (with(&|r| r.own_command = true), Skipped::OwnCommand),
            (with(&|r| r.env.ci = some("")), Skipped::Ci),
            (
                with(&|r| r.env.no_skill_update = some("1")),
                Skipped::OptedOut,
            ),
            (
                with(&|r| r.env.no_skill_update = some("")),
                Skipped::OptedOut,
            ),
            (with(&|r| r.env.home = None), Skipped::NoHome),
            (with(&|r| r.env.home = some("rel")), Skipped::NoHome),
        ] {
            assert_eq!(outcome, Outcome::Skipped(why));
        }
        assert_eq!(status(&target, &V2).expect("status").state, State::Older);
        assert_eq!(
            with(&|r| r.env.no_skill_update = some("0")),
            Outcome::Updated
        );
    }

    #[test]
    #[cfg(unix)]
    fn root_leaves_another_users_home_alone() {
        let (home, mut run, _target) = run_with_v1();
        crate::skill::testing::not_roots(home.path());
        run.env.euid = Some(0);
        assert_eq!(update(&run, &V2), Outcome::Skipped(Skipped::ForeignHome));
    }

    #[test]
    fn a_changed_skill_is_left_alone() {
        for change in [
            (|file| fs::write(file, "mine")) as fn(std::path::PathBuf) -> std::io::Result<()>,
            |file| fs::remove_file(file),
            |file| fs::write(file.with_file_name("new.md"), "mine"),
            |file| fs::create_dir(file.with_file_name("new.md")),
        ] {
            let (_home, run, target) = run_with_v1();
            change(target.dir.join("SKILL.md").into()).expect("change");
            assert_eq!(update(&run, &V3), Outcome::Skipped(Skipped::Changed));
            assert_eq!(status(&target, &V3).expect("status").state, State::Older);
        }
    }

    #[test]
    fn a_file_that_is_not_dqs_does_not_matter() {
        let (_home, run, target) = run_with_v1();
        let notes = target.dir.join("notes.md");
        fs::write(&notes, "mine").expect("write");
        fs::write(target.dir.join("new.md"), "mine").expect("write");
        assert_eq!(update(&run, &V2), Outcome::Updated, "V2 has no new.md");
        assert_eq!(fs::read_to_string(&notes).expect("read"), "mine");
    }

    #[test]
    fn a_skill_dq_did_not_install_or_cannot_read_is_left_alone() {
        let (_home, run, target) = run_with_v1();
        let manifest = target.dir.join(MANIFEST);
        for text in ["{}", r#"{"schema": 2}"#] {
            fs::write(&manifest, text).expect("write");
            assert_eq!(update(&run, &V2), Outcome::Skipped(Skipped::Unusable));
        }
        fs::remove_file(&manifest).expect("rm");
        assert_eq!(update(&run, &V2), Outcome::Skipped(Skipped::NotInstalled));
    }

    #[test]
    #[cfg(unix)]
    fn a_skill_directory_that_is_a_link_is_left_alone() {
        let (home, run, target) = run_with_v1();
        let elsewhere = home.path().join("checkout");
        fs::rename(&target.dir, &elsewhere).expect("mv");
        std::os::unix::fs::symlink(&elsewhere, &target.dir).expect("symlink");
        assert_eq!(update(&run, &V2), Outcome::Skipped(Skipped::Unusable));
    }

    /// What `dq ARGS` parses to.
    fn command(args: &[&str]) -> Command {
        let argv = ["dq"].iter().chain(args);
        <crate::Cli as clap::Parser>::try_parse_from(argv)
            .expect("arguments")
            .command
    }

    /// What a release build decides after `dq ARGS`. Not to be run: its home
    /// directory is the developer's.
    fn after(args: &[&str]) -> FirstRun {
        FirstRun::after(&command(args), true)
    }

    #[test]
    fn dq_skill_and_dq_self_see_to_the_skill_themselves() {
        for args in [
            &["skill", "install"][..],
            &["skill", "update"],
            &["skill", "status"],
            &["skill", "remove"],
            &["self", "check"],
            &["self", "update"],
        ] {
            assert!(after(args).own_command, "{args:?}");
        }
        for args in [
            &["get", "a.json", ""][..],
            &["set", "a.json", "/a", "1"],
            &["lint", "a.json"],
            &["completions", "bash"],
            &["rules", "list"],
        ] {
            let run = after(args);
            assert!(!run.own_command && run.release_build, "{args:?}");
        }
        let debug = FirstRun::after(&command(&["man"]), false);
        assert!(!debug.release_build);
    }

    /// Any value of `CI` is one, and any of `DQ_NO_SKILL_UPDATE` but `0`.
    #[test]
    fn the_environment_turns_it_off_whatever_the_value() {
        for (ci, no_update, expected) in [
            (Some("0"), None, Some(Skipped::Ci)),
            (Some("false"), None, Some(Skipped::Ci)),
            (Some("1"), Some("0"), Some(Skipped::Ci)),
            (None, Some("false"), Some(Skipped::OptedOut)),
            (None, Some("00"), Some(Skipped::OptedOut)),
            (None, Some(" 0"), Some(Skipped::OptedOut)),
            (None, Some("0"), None),
            (None, None, None),
        ] {
            let (_home, mut run, _target) = run_with_v1();
            run.env.ci = ci.map(Into::into);
            run.env.no_skill_update = no_update.map(Into::into);
            assert_eq!(
                update(&run, &V2),
                expected.map_or(Outcome::Updated, Outcome::Skipped),
                "CI {ci:?}, DQ_NO_SKILL_UPDATE {no_update:?}"
            );
        }
    }

    /// The order of versions is that of numbers, component by component: in
    /// each of these the one that is not older stays.
    #[test]
    fn only_an_older_skill_is_updated() {
        let edition = |version: &'static str| Edition { version, ..V2 };
        for (installed, running, updated) in [
            ("2026.9.0", "2026.10.0", true),
            ("2026.10.0", "2026.9.0", false),
            ("2026.10.0", "2026.10.0", false),
            ("2026.10", "2026.10.0", false),
            ("2026.10.0", "2026.10", false),
            ("2026.10.1", "2026.11.0", true),
            ("2026.10.0-rc1", "2026.10.0", false),
            ("2025.52.9", "2026.1.0", true),
            ("2027.1.0", "2026.52.9", false),
        ] {
            let (_home, run) = run();
            let target = run.env.global().expect("target");
            install(
                &target,
                &Edition {
                    version: installed,
                    ..V1
                },
                false,
            )
            .expect("install");
            let expected = if updated {
                Outcome::Updated
            } else {
                Outcome::Skipped(Skipped::NotOlder)
            };
            assert_eq!(
                update(&run, &edition(running)),
                expected,
                "{installed} -> {running}"
            );
            let text = fs::read_to_string(target.dir.join("SKILL.md")).expect("read");
            assert_eq!(text == "two", updated, "{installed} -> {running}: {text}");
        }
    }

    /// A file whose name changed only in case is no change of the user's, on
    /// any file system, and has one name after it.
    #[test]
    fn a_name_that_differs_in_case_only_is_updated() {
        const OLD: Edition = Edition {
            version: "2026.1.0",
            files: &[
                SkillFile {
                    path: "References/Other.md",
                    text: "two",
                },
                SkillFile {
                    path: "Skill.md",
                    text: "one",
                },
            ],
        };
        const NEW: Edition = Edition {
            version: "2026.2.0",
            files: &[
                SkillFile {
                    path: "SKILL.md",
                    text: "two",
                },
                SkillFile {
                    path: "references/other.md",
                    text: "two",
                },
            ],
        };
        let (_home, run) = run();
        let target = run.env.global().expect("target");
        install(&target, &OLD, false).expect("install");
        assert_eq!(update(&run, &NEW), Outcome::Updated);
        let status = status(&target, &NEW).expect("status");
        assert_eq!(status.state, State::UpToDate);
        let mut names: Vec<_> = walkdir::WalkDir::new(&target.dir)
            .min_depth(1)
            .into_iter()
            .map(|entry| {
                let entry = entry.expect("entry");
                let path = entry.path().strip_prefix(&target.dir).expect("inside");
                path.to_str().expect("UTF-8").to_owned()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            [MANIFEST, "SKILL.md", "references", "references/other.md"]
        );
    }

    /// Root in its own home directory, and anyone else in any, as long as the
    /// skill directory is theirs.
    #[test]
    #[cfg(unix)]
    fn only_a_skill_directory_that_is_not_ones_own_stops() {
        for (user, updated) in [(None, true), (Some(0), true), (Some(1), false)] {
            let (home, mut run, target) = run_with_v1();
            crate::skill::testing::not_roots(home.path());
            let owner = crate::skill::testing::not_roots(target.dir.as_std_path());
            run.env.euid = user.map(|user| owner + user);
            let expected = if updated {
                Outcome::Updated
            } else {
                Outcome::Skipped(Skipped::OtherOwner)
            };
            assert_eq!(update(&run, &V2), expected, "{user:?}");
            // Of one that stops, nothing is read; and once more after the lock.
            fs::write(target.dir.join(MANIFEST), "{}").expect("write");
            let expected = Outcome::Skipped(if updated {
                Skipped::Unusable
            } else {
                Skipped::OtherOwner
            });
            assert_eq!(update(&run, &V2), expected, "{user:?}");
            let locked = update_locked(&target.dir, run.env.euid, &V2);
            assert_eq!(locked.expect("decided"), expected, "{user:?}");
        }
    }

    /// What is decided before the lock is decided once more after it.
    #[test]
    fn what_another_dq_did_before_the_lock_counts() {
        let (_home, _run, target) = run_with_v1();
        let dir = &target.dir;
        let locked = |edition| update_locked(dir, None, edition).expect("decided");
        assert_eq!(locked(&V2), Outcome::Updated);
        assert_eq!(locked(&V2), Outcome::Skipped(Skipped::NotOlder));
        assert_eq!(locked(&V1), Outcome::Skipped(Skipped::NotOlder));
        let text = fs::read_to_string(dir.join("SKILL.md")).expect("read");
        assert_eq!(text, "two", "no way back");
        fs::write(dir.join("SKILL.md"), "mine").expect("write");
        assert_eq!(locked(&V3), Outcome::Skipped(Skipped::Changed));
        fs::remove_file(dir.join(MANIFEST)).expect("rm");
        assert_eq!(locked(&V3), Outcome::Skipped(Skipped::NotInstalled));
    }

    /// A user saves a file after the decision: it stays, and is an edit from
    /// then on.
    #[test]
    fn what_is_saved_after_the_decision_is_not_lost() {
        for path in ["SKILL.md", "new.md"] {
            let (_home, run, target) = run_with_v1();
            let dir = &target.dir;
            let Decision::Write(manifest, files) = decide(dir, None, &V3).expect("decided") else {
                panic!("not to be updated");
            };
            fs::write(dir.join(path), "mine").expect("write");
            assert_eq!(
                write_over(dir, &V3, &manifest, &files).expect("stopped"),
                Outcome::Skipped(Skipped::Changed)
            );
            assert_eq!(fs::read_to_string(dir.join(path)).expect("read"), "mine");
            assert_eq!(update(&run, &V3), Outcome::Skipped(Skipped::Changed));
            fs::remove_file(dir.join(path)).expect("rm");
            let expected = if path == "new.md" {
                Outcome::Updated
            } else {
                Outcome::Skipped(Skipped::Changed) // removed by someone
            };
            assert_eq!(update(&run, &V3), expected, "{path}");
        }
    }

    /// An edited skill costs no more than finding the first edit: no lock, no
    /// further file, no look at what else is there.
    #[test]
    #[cfg(unix)]
    fn a_changed_skill_is_left_alone_at_the_first_change() {
        use std::os::unix::fs::PermissionsExt;
        const OLD: Edition = Edition {
            version: "2026.1.0",
            files: &[
                SkillFile {
                    path: "SKILL.md",
                    text: "one",
                },
                SkillFile {
                    path: "z.md",
                    text: "one",
                },
            ],
        };
        let (_home, run) = run();
        let target = run.env.global().expect("target");
        install(&target, &OLD, false).expect("install");
        let lock = fs::File::open(&target.dir).expect("open");
        lock.lock().expect("lock");
        assert_eq!(run.update_skill(&V3), Outcome::Skipped(Skipped::Busy));
        fs::write(target.dir.join("SKILL.md"), "mine").expect("write");
        let unreadable = target.dir.join("z.md");
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).expect("chmod");
        assert_eq!(run.update_skill(&V3), Outcome::Skipped(Skipped::Changed));
    }

    /// A directory of the user's that cannot be read is not looked into.
    #[test]
    #[cfg(unix)]
    fn a_directory_that_cannot_be_read_does_not_matter() {
        use std::os::unix::fs::PermissionsExt;
        let (_home, run, target) = run_with_v1();
        let mine = target.dir.join("mine");
        fs::create_dir(&mine).expect("mkdir");
        fs::set_permissions(&mine, fs::Permissions::from_mode(0o000)).expect("chmod");
        let outcome = update(&run, &V2);
        fs::set_permissions(&mine, fs::Permissions::from_mode(0o700)).expect("chmod");
        assert_eq!(outcome, Outcome::Updated);
    }

    /// A manifest that lists more files than dq ever has is none.
    #[test]
    fn a_manifest_with_many_files_is_left_alone() {
        let (_home, run, target) = run_with_v1();
        let mut manifest = Manifest::of(&V1);
        for n in 0..20_000 {
            manifest.files.insert(format!("f{n}"), "0".repeat(64));
        }
        fs::write(target.dir.join(MANIFEST), manifest.to_bytes()).expect("write");
        assert_eq!(update(&run, &V2), Outcome::Skipped(Skipped::Unusable));
        manifest.files.retain(|path, _| path.len() < 3);
        manifest.dq_version.push_str(&".0".repeat(70_000));
        fs::write(target.dir.join(MANIFEST), manifest.to_bytes()).expect("write");
        assert_eq!(update(&run, &V2), Outcome::Skipped(Skipped::Unusable));
    }

    #[test]
    fn another_dq_at_work_is_left_to_it() {
        let (_home, run, target) = run_with_v1();
        let lock = fs::File::open(&target.dir).expect("open");
        lock.lock().expect("lock");
        assert_eq!(run.update_skill(&V2), Outcome::Skipped(Skipped::Busy));
        assert_eq!(status(&target, &V2).expect("status").state, State::Older);
        drop(lock);
        assert_eq!(update(&run, &V2), Outcome::Updated);
    }

    /// A link is never dq's work: to the very text dq wrote, in the place of
    /// the directory above a file, or of the manifest.
    #[test]
    #[cfg(unix)]
    fn a_link_in_the_skill_is_left_alone() {
        use std::os::unix::fs::symlink;
        const DEEP: Edition = Edition {
            version: "2026.1.0",
            files: &[SkillFile {
                path: "references/a.md",
                text: "one",
            }],
        };
        let move_and_link = |home: &std::path::Path, at: &camino::Utf8Path| {
            let outside = home.join("outside");
            fs::rename(at, &outside).expect("mv");
            symlink(&outside, at).expect("symlink");
        };
        for (path, why) in [
            ("references/a.md", Skipped::Changed),
            ("references", Skipped::Changed),
            (MANIFEST, Skipped::Unusable),
        ] {
            let (home, run) = run();
            let target = run.env.global().expect("target");
            install(&target, &DEEP, false).expect("install");
            move_and_link(home.path(), &target.dir.join(path));
            assert_eq!(update(&run, &V2), Outcome::Skipped(why), "{path}");
            assert!(target.dir.join(path).is_symlink(), "{path}");
            assert!(!target.dir.join("SKILL.md").exists(), "{path}");
        }
        // A link above the skill directory is how dotfiles are kept.
        let (home, run, target) = run_with_v1();
        move_and_link(home.path(), target.dir.parent().expect("skills"));
        assert_eq!(update(&run, &V2), Outcome::Updated);
    }

    /// The manifest is the last to be written: what a run leaves that was
    /// cut short is updated, and one that fails says so and is repeated.
    #[test]
    fn a_run_that_was_cut_short_is_repeated() {
        let (_home, run, target) = run_with_v1();
        fs::write(target.dir.join("SKILL.md"), "three").expect("write");
        fs::write(target.dir.join(".tmpAb12Cd"), "half").expect("write");
        assert_eq!(update(&run, &V3), Outcome::Updated);
        assert_eq!(status(&target, &V3).expect("status").state, State::UpToDate);
        // A file V1 has not: gone, with the manifest of V3 still there.
        fs::remove_file(target.dir.join("new.md")).expect("rm");
        let older = Edition {
            version: "2026.12.0",
            ..V1
        };
        assert_eq!(update(&run, &older), Outcome::Updated);
    }

    #[test]
    #[cfg(unix)]
    fn a_failure_says_what_failed() {
        use std::os::unix::fs::PermissionsExt;
        let (_home, run, target) = run_with_v1();
        let chmod = |path: &camino::Utf8Path, mode: u32| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
        };
        chmod(&target.dir, 0o500);
        if fs::write(target.dir.join("probe"), "").is_ok() {
            return; // root: the mode does not bind
        }
        let outcome = update(&run, &V2);
        chmod(&target.dir, 0o700);
        assert!(
            matches!(&outcome, Outcome::Failed(message) if message.contains("Permission denied")),
            "{outcome:?}"
        );
        let status_now = status(&target, &V2).expect("status");
        assert_eq!(
            (status_now.state, status_now.edited),
            (State::Older, vec![])
        );
        chmod(&target.dir.join(MANIFEST), 0o000);
        let outcome = update(&run, &V2);
        chmod(&target.dir.join(MANIFEST), 0o600);
        assert!(
            matches!(&outcome, Outcome::Failed(message) if message.contains(MANIFEST)),
            "{outcome:?}"
        );
        assert_eq!(update(&run, &V2), Outcome::Updated);
    }

    /// Only the skill directory is written to: no link, nothing next to it.
    #[test]
    fn nothing_but_the_skill_directory_changes() {
        let (home, run, _target) = run_with_v1();
        fs::create_dir(home.path().join(".claude")).expect("mkdir");
        assert_eq!(update(&run, &V2), Outcome::Updated);
        assert_eq!(
            fs::read_dir(home.path().join(".claude"))
                .expect("dir")
                .count(),
            0
        );
        let skills = fs::read_dir(home.path().join(".agents/skills")).expect("dir");
        assert_eq!(skills.count(), 1);
    }

    /// Many at once: one updates, the others find it busy, done, or — while
    /// its files are ahead of its manifest — as their own edition has them.
    #[test]
    fn concurrent_runs_leave_one_whole_version() {
        let (_home, run, target) = run_with_v1();
        let outcomes: Vec<Outcome> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..16)
                .map(|n| {
                    let (run, edition) = (run.clone(), if n % 2 == 0 { V2 } else { V1 });
                    scope.spawn(move || update(&run, &edition))
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().expect("no panic"))
                .collect()
        });
        assert!(outcomes.contains(&Outcome::Updated), "{outcomes:?}");
        let status = status(&target, &V2).expect("status");
        assert_eq!(status.state, State::UpToDate);
        assert_eq!(status.edited, Vec::<String>::new());
    }
}
