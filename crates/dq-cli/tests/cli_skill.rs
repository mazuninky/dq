//! `dq skill`, driven through the real binary in a home directory of its own.
//!
//! The binary under `cargo test` is a debug build, which never updates a
//! skill by itself; that decision is tested in `dq::skill`. What is tested
//! here of it is that this is so, and that no test of this crate runs the
//! binary in the developer's home directory.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use dq::skill::{Edition, Environment, Manifest, SkillFile};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "skill/concurrent.rs"]
mod concurrent;
#[path = "skill/project.rs"]
mod project;
#[path = "skill/refusals.rs"]
mod refusals;
#[path = "skill/release.rs"]
mod release;
#[path = "skill/status.rs"]
mod status;
#[path = "skill/writes.rs"]
mod writes;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The skill of a dq older than any: other text in two files this one has,
/// and a file it has not.
const OLD: Edition = Edition {
    version: "2020.1.0",
    files: &[
        SkillFile {
            path: "SKILL.md",
            text: "old skill\n",
        },
        SkillFile {
            path: "references/formats.md",
            text: "old formats\n",
        },
        SkillFile {
            path: "references/gone.md",
            text: "old gone\n",
        },
    ],
};

/// The manifest, relative to the skill directory.
const MANIFEST: &str = ".dq-skill.json";

/// A home directory.
struct Home {
    /// Held for its `Drop`.
    _tmp: TempDir,
    root: PathBuf,
}

impl Home {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical tempdir");
        Self { _tmp: tmp, root }
    }

    /// With Claude Code in it.
    fn with_claude() -> Self {
        let home = Self::new();
        fs::create_dir(home.root.join(".claude")).expect("mkdir");
        home
    }

    fn dq(&self) -> Command {
        let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
        cmd.env_clear();
        cmd.env("HOME", &self.root);
        cmd.current_dir(&self.root);
        cmd.arg("--no-color");
        cmd
    }

    fn skill(&self) -> PathBuf {
        self.root.join(".agents/skills/dq")
    }

    fn link(&self) -> PathBuf {
        self.root.join(".claude/skills/dq")
    }

    /// Write what the dq `edition` belongs to would. Not by `dq::skill`: a
    /// process that another test forks has the locks of this one until it
    /// execs, and the automatic update would find the skill busy.
    fn install_edition(&self, edition: &Edition) {
        let manifest = Manifest::of(edition).to_bytes();
        let files = edition.files.iter();
        for (path, bytes) in files
            .map(|file| (file.path, file.text.as_bytes()))
            .chain([(MANIFEST, manifest.as_slice())])
        {
            let at = self.skill().join(path);
            fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
            fs::write(at, bytes).expect("write");
        }
    }

    /// Put `text` where the manifest is.
    fn write_manifest(&self, text: &str) {
        fs::write(self.skill().join(MANIFEST), text).expect("write manifest");
    }

    /// The manifest, changed by `change`.
    fn change_manifest(&self, change: impl FnOnce(&mut Value)) {
        let bytes = fs::read(self.skill().join(MANIFEST)).expect("read manifest");
        let mut value = serde_json::from_slice(&bytes).expect("JSON");
        change(&mut value);
        self.write_manifest(&value.to_string());
    }

    /// Exit code, stdout, stderr of `dq skill ARGS`; the home directory's
    /// path is `~` in what is returned.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.finish(self.dq().arg("skill").args(args))
    }

    /// The same of any command, which must end by itself: a minute is far
    /// more than any of them takes.
    fn finish(&self, cmd: &mut Command) -> (i32, String, String) {
        let output = cmd
            .timeout(std::time::Duration::from_secs(60))
            .output()
            .expect("run dq");
        let text = |bytes: Vec<u8>| {
            String::from_utf8(bytes)
                .expect("UTF-8")
                .replace(self.root.to_str().expect("UTF-8 tempdir"), "~")
        };
        (
            output.status.code().expect("exit code"),
            text(output.stdout),
            text(output.stderr),
        )
    }

    /// `dq skill ARGS`, which must succeed; what it prints.
    fn ok(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.run(args);
        assert_eq!((code, stderr.as_str()), (0, ""), "dq skill {args:?}");
        stdout
    }

    /// `dq skill ARGS`, which must be refused; what it says.
    fn refused(&self, args: &[&str]) -> String {
        let before = tree(&self.root);
        let (code, stdout, stderr) = self.run(args);
        assert_eq!(
            (code, stdout.as_str()),
            (6, ""),
            "dq skill {args:?}: {stderr}"
        );
        assert_eq!(
            tree(&self.root),
            before,
            "a refused command wrote something"
        );
        stderr.trim_end().to_owned()
    }

    fn status(&self, args: &[&str]) -> Value {
        let mut all = vec!["status", "-F", "json"];
        all.extend(args);
        serde_json::from_str(&self.ok(&all)).expect("JSON")
    }
}

/// Every file and link under `dir` with what is in it, sorted by path.
fn tree(dir: &Path) -> Vec<(String, String)> {
    let mut entries: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .map(|entry| entry.expect("entry"))
        .filter(|entry| !entry.file_type().is_dir())
        .map(|entry| {
            let holds = if entry.path_is_symlink() {
                let to = fs::read_link(entry.path()).expect("link");
                format!("-> {}", to.display())
            } else if entry.file_type().is_file() {
                fs::read_to_string(entry.path()).expect("text file")
            } else {
                "special".to_owned() // a FIFO: reading it would wait
            };
            let path = entry.path().strip_prefix(dir).expect("under dir");
            (path.to_str().expect("UTF-8").to_owned(), holds)
        })
        .collect();
    entries.sort();
    entries
}

/// Set the mode of `path`.
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Whether a mode binds whoever runs the tests; root's does not.
fn modes_bind() -> bool {
    let dir = tempfile::tempdir().expect("tempdir");
    chmod(dir.path(), 0o500);
    fs::write(dir.path().join("probe"), "").is_err()
}

/// The skill in this repository.
fn source(path: &str) -> String {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/dq")
        .join(path);
    fs::read_to_string(file).expect("skill file")
}

#[test]
fn install_writes_the_skill_its_manifest_and_the_link() {
    let home = Home::with_claude();
    assert_eq!(
        home.ok(&["install"]),
        format!(
            "installed dq skill v{VERSION} in ~/.agents/skills/dq\nlinked ~/.claude/skills/dq\n"
        )
    );
    let files: Vec<String> = tree(&home.skill())
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        files,
        [
            ".dq-skill.json",
            "SKILL.md",
            "references/formats.md",
            "references/linting.md",
            "references/pointers.md",
        ]
    );
    for path in &files[1..] {
        let written = fs::read_to_string(home.skill().join(path)).expect("read");
        assert_eq!(written, source(path), "{path}");
    }
    let manifest: Value =
        serde_json::from_slice(&fs::read(home.skill().join(".dq-skill.json")).expect("read"))
            .expect("JSON");
    assert_eq!(manifest["schema"], json!(1));
    assert_eq!(manifest["dq_version"], json!(VERSION));
    assert_eq!(manifest["files"].as_object().expect("object").len(), 4);
    assert_eq!(
        fs::read_link(home.link()).expect("a link"),
        Path::new("../../.agents/skills/dq")
    );
    assert!(
        home.link().join("SKILL.md").is_file(),
        "the link leads there"
    );
    assert_eq!(
        home.ok(&["install"]),
        format!("dq skill v{VERSION} in ~/.agents/skills/dq is up to date\n")
    );
}

#[test]
fn quiet_prints_nothing_but_errors_and_the_status() {
    let home = Home::with_claude();
    assert_eq!(home.ok(&["update", "-q"]), "");
    assert_eq!(home.ok(&["remove", "-q"]), "");
    assert_eq!(home.ok(&["install", "-q"]), "");
    assert!(home.ok(&["status", "-q"]).contains("state: up_to_date\n"));
    assert_eq!(home.ok(&["remove", "-q"]), "");
    fs::create_dir_all(home.link()).expect("mkdir");
    assert_eq!(
        home.refused(&["install", "-q"]),
        "~/.claude/skills/dq is not a link, and dq did not write it: move it away first"
    );
}

#[test]
fn status_reports_an_object_of_a_fixed_shape() {
    let home = Home::new();
    assert_eq!(
        home.status(&[]),
        json!({
            "path": "~/.agents/skills/dq",
            "installed": false,
            "state": "not_installed",
            "version": "",
            "dq_version": VERSION,
            "edited": [],
            "missing": [],
            "colliding": [],
            "in_the_way": [],
            "unlisted": [],
            "link": "",
            "link_state": "not_wanted",
        })
    );
    fs::create_dir(home.root.join(".claude")).expect("mkdir");
    home.ok(&["install"]);
    fs::write(home.skill().join("SKILL.md"), "mine").expect("write");
    fs::write(home.skill().join("notes.md"), "mine").expect("write");
    fs::remove_file(home.skill().join("references/formats.md")).expect("rm");
    assert_eq!(
        home.status(&[]),
        json!({
            "path": "~/.agents/skills/dq",
            "installed": true,
            "state": "up_to_date",
            "version": VERSION,
            "dq_version": VERSION,
            "edited": ["SKILL.md"],
            "missing": ["references/formats.md"],
            "colliding": [],
            "in_the_way": [],
            "unlisted": ["notes.md"],
            "link": "~/.claude/skills/dq",
            "link_state": "ok",
        })
    );
    for format in ["yaml", "toml", "toon", "jsonl", "console"] {
        assert!(
            home.ok(&["status", "-F", format]).contains("SKILL.md"),
            "{format}"
        );
    }
}

#[test]
fn update_and_remove_with_nothing_installed_say_so() {
    let home = Home::new();
    let note = "dq has installed no skill in ~/.agents/skills/dq\n";
    assert_eq!(home.ok(&["update"]), note);
    assert_eq!(home.ok(&["remove"]), note);
    assert_eq!(tree(&home.root), []);
}

#[test]
fn update_restores_a_missing_file_and_refuses_an_edited_one() {
    let home = Home::new();
    home.ok(&["install"]);
    let skill_md = home.skill().join("SKILL.md");
    fs::remove_file(&skill_md).expect("rm");
    assert_eq!(
        home.ok(&["update"]),
        format!("restored dq skill v{VERSION} in ~/.agents/skills/dq\n")
    );
    fs::write(&skill_md, "mine").expect("write");
    for command in ["install", "update"] {
        assert_eq!(
            home.refused(&[command]),
            "~/.agents/skills/dq was changed since dq wrote it: edited SKILL.md \
             (--force writes dq's files in their place)"
        );
    }
    assert_eq!(
        home.refused(&["remove"]),
        "~/.agents/skills/dq was changed since dq wrote it: edited SKILL.md \
         (--force removes the edited files too)"
    );
    home.ok(&["update", "--force"]);
    assert_eq!(
        fs::read_to_string(&skill_md).expect("read"),
        source("SKILL.md")
    );
}

#[test]
fn a_foreign_directory_is_taken_over_only_by_force_and_loses_only_dqs_files() {
    let home = Home::new();
    fs::create_dir_all(home.skill()).expect("mkdir");
    fs::write(home.skill().join("SKILL.md"), "theirs").expect("write");
    fs::write(home.skill().join("notes.md"), "theirs").expect("write");
    for command in ["install", "update"] {
        assert_eq!(
            home.refused(&[command]),
            "~/.agents/skills/dq was not installed by dq (SKILL.md, notes.md): \
             --force writes dq's files into it and keeps the rest"
        );
    }
    for args in [&["remove"][..], &["remove", "--force"]] {
        assert_eq!(
            home.refused(args),
            "~/.agents/skills/dq was not installed by dq (it has no .dq-skill.json): \
             remove it yourself"
        );
    }
    home.ok(&["install", "--force"]);
    assert_eq!(home.status(&[])["unlisted"], json!(["notes.md"]));
    // Dq's from here on: what is not dq's stops nothing.
    let up_to_date = format!("dq skill v{VERSION} in ~/.agents/skills/dq is up to date\n");
    assert_eq!(home.ok(&["update"]), up_to_date);
    assert_eq!(home.ok(&["install"]), up_to_date);
    assert_eq!(
        home.ok(&["remove"]),
        format!(
            "removed dq skill v{VERSION} from ~/.agents/skills/dq\n\
             kept ~/.agents/skills/dq: not dq's are notes.md\n"
        )
    );
    assert_eq!(
        tree(&home.skill()),
        [("notes.md".to_owned(), "theirs".to_owned())]
    );
}

/// A file dq did not write, where this version has one: the manifest is one
/// that does not list it, as an older dq's.
#[test]
fn a_file_in_the_place_of_a_new_one_is_replaced_only_by_force() {
    let home = Home::new();
    home.ok(&["install"]);
    let manifest = home.skill().join(".dq-skill.json");
    let mut value: Value =
        serde_json::from_slice(&fs::read(&manifest).expect("read")).expect("JSON");
    value["files"]
        .as_object_mut()
        .expect("object")
        .remove("references/formats.md");
    let older = value.to_string();
    let formats = home.skill().join("references/formats.md");
    let outside = home.root.join("outside.md");
    fs::write(&outside, "theirs").expect("write");
    for link in [false, true] {
        fs::write(&manifest, &older).expect("write");
        fs::remove_file(&formats).expect("rm");
        if link {
            symlink(&outside, &formats).expect("symlink");
        } else {
            fs::write(&formats, "theirs").expect("write");
        }
        for command in ["install", "update"] {
            assert_eq!(
                home.refused(&[command]),
                "~/.agents/skills/dq was changed since dq wrote it: \
                 not dq's: references/formats.md (--force writes dq's files in their place)"
            );
        }
        home.ok(&["update", "--force"]);
        assert!(!formats.is_symlink());
        assert_eq!(
            fs::read_to_string(&formats).expect("read"),
            source("references/formats.md")
        );
        assert_eq!(fs::read_to_string(&outside).expect("read"), "theirs");
    }

    fs::write(&manifest, &older).expect("write");
    fs::remove_file(&formats).expect("rm");
    fs::create_dir(&formats).expect("mkdir");
    for args in [&["update"][..], &["update", "--force"]] {
        assert_eq!(
            home.refused(args),
            "~/.agents/skills/dq cannot hold the files of the skill (references/formats.md): \
             dq neither goes through a link nor replaces a directory"
        );
    }
    // Removing writes nothing there, so nothing is in its way.
    assert_eq!(
        home.ok(&["remove"]),
        format!(
            "removed dq skill v{VERSION} from ~/.agents/skills/dq\n\
             kept ~/.agents/skills/dq: it is not empty (references/formats.md)\n"
        )
    );
}

#[test]
fn remove_takes_the_files_the_directory_and_the_link() {
    let home = Home::with_claude();
    home.ok(&["install"]);
    assert_eq!(
        home.ok(&["remove"]),
        format!(
            "removed dq skill v{VERSION} from ~/.agents/skills/dq\n\
             removed the link ~/.claude/skills/dq\n"
        )
    );
    assert_eq!(tree(&home.root), []);
    assert!(!home.skill().exists());
    assert!(
        home.root.join(".agents/skills").is_dir(),
        "not dq's to remove"
    );
}

#[test]
fn a_skill_directory_that_is_a_link_is_refused_always() {
    let home = Home::new();
    let checkout = home.root.join("checkout");
    fs::create_dir_all(&checkout).expect("mkdir");
    fs::create_dir_all(home.root.join(".agents/skills")).expect("mkdir");
    symlink(&checkout, home.skill()).expect("symlink");
    for command in ["install", "update", "remove"] {
        for args in [&[command][..], &[command, "--force"]] {
            assert_eq!(
                home.refused(args),
                "~/.agents/skills/dq is a symbolic link: \
                 dq neither writes through it nor replaces it"
            );
        }
    }
    assert_eq!(home.status(&[])["state"], json!("blocked"));
}

#[test]
fn a_link_inside_the_skill_is_never_written_or_deleted_through() {
    let home = Home::new();
    home.ok(&["install"]);
    let outside = home.root.join("outside");
    fs::rename(home.skill().join("references"), &outside).expect("mv");
    symlink(&outside, home.skill().join("references")).expect("symlink");
    for command in ["install", "update", "remove"] {
        assert_eq!(
            home.refused(&[command, "--force"]),
            "~/.agents/skills/dq cannot hold the files of the skill (references): \
             dq neither goes through a link nor replaces a directory"
        );
    }
}

/// A link in the place of a file of dq's, or of the manifest: what it leads
/// to is neither written nor deleted, and `--force` replaces the link itself.
#[test]
fn a_link_in_the_place_of_a_file_is_replaced_not_followed() {
    let home = Home::new();
    home.ok(&["install"]);
    let outside = home.root.join("outside");
    for (path, refusal) in [
        ("SKILL.md", "was changed since dq wrote it: edited SKILL.md"),
        (
            "references/formats.md",
            "was changed since dq wrote it: edited references/formats.md",
        ),
        (".dq-skill.json", "its .dq-skill.json cannot be used"),
    ] {
        let at = home.skill().join(path);
        for command in [&["update", "--force"][..], &["install", "--force"]] {
            // To a file that is there, then to one that is not.
            for text in [Some("theirs"), None] {
                let _ = fs::remove_file(&outside);
                if let Some(text) = text {
                    fs::write(&outside, text).expect("write");
                }
                fs::remove_file(&at).expect("rm");
                symlink(&outside, &at).expect("symlink");
                let said = home.refused(&["update"]);
                assert!(said.contains(refusal), "{said}");
                home.ok(command);
                assert!(at.is_file() && !at.is_symlink(), "{path}");
                assert_eq!(fs::read_to_string(&outside).ok().as_deref(), text, "{path}");
                assert_eq!(home.status(&[])["edited"], json!([]));
            }
        }
    }
    let skill_md = home.skill().join("SKILL.md");
    fs::write(&outside, "theirs").expect("write");
    fs::remove_file(&skill_md).expect("rm");
    symlink(&outside, &skill_md).expect("symlink");
    home.ok(&["remove", "--force"]);
    assert!(!home.skill().exists());
    assert_eq!(fs::read_to_string(&outside).expect("read"), "theirs");
}

#[test]
fn a_link_above_the_skill_directory_is_allowed() {
    let home = Home::with_claude();
    fs::create_dir_all(home.root.join("dotfiles/agents")).expect("mkdir");
    fs::create_dir_all(home.root.join("dotfiles/claude")).expect("mkdir");
    fs::remove_dir(home.root.join(".claude")).expect("rmdir");
    symlink("dotfiles/agents", home.root.join(".agents")).expect("symlink");
    symlink("dotfiles/claude", home.root.join(".claude")).expect("symlink");
    home.ok(&["install"]);
    assert!(
        home.root
            .join("dotfiles/agents/skills/dq/SKILL.md")
            .is_file()
    );
    assert!(
        home.link().join("SKILL.md").is_file(),
        "the link leads there"
    );
    assert_eq!(home.status(&[])["link_state"], json!("ok"));
}

#[test]
fn claude_code_reading_the_shared_directory_needs_no_link() {
    let home = Home::with_claude();
    symlink("../.agents/skills", home.root.join(".claude/skills")).expect("symlink");
    home.ok(&["install"]);
    assert_eq!(home.status(&[])["link_state"], json!("ok"));
    assert!(home.skill().join("SKILL.md").is_file());
    assert_eq!(
        home.ok(&["remove"]),
        format!("removed dq skill v{VERSION} from ~/.agents/skills/dq\n")
    );
}

#[test]
fn only_another_link_in_the_place_of_the_link_is_replaced_and_only_by_force() {
    let home = Home::with_claude();
    fs::create_dir(home.root.join(".claude/skills")).expect("mkdir");
    symlink("/nowhere", home.link()).expect("symlink");
    assert_eq!(
        home.refused(&["install"]),
        "~/.claude/skills/dq is a link to '/nowhere': --force replaces it"
    );
    assert_eq!(home.status(&[])["link_state"], json!("elsewhere"));
    assert_eq!(
        home.ok(&["install", "--force"]),
        format!(
            "installed dq skill v{VERSION} in ~/.agents/skills/dq\n\
             replaced the link ~/.claude/skills/dq\n"
        )
    );
    assert!(home.link().join("SKILL.md").is_file());

    let home = Home::with_claude();
    fs::create_dir_all(home.link()).expect("mkdir");
    fs::write(home.link().join("SKILL.md"), "theirs").expect("write");
    for args in [&["install"][..], &["install", "--force"]] {
        assert_eq!(
            home.refused(args),
            "~/.claude/skills/dq is not a link, and dq did not write it: move it away first"
        );
    }
}

#[test]
fn claude_config_dir_is_where_the_link_goes() {
    let home = Home::with_claude();
    let claude = home.root.join("config/claude");
    let dq = |args: &[&str]| {
        home.dq()
            .env("CLAUDE_CONFIG_DIR", &claude)
            .arg("skill")
            .args(args)
            .assert()
            .success();
    };
    dq(&["install"]);
    assert!(
        !home.link().exists(),
        "no ~/.claude when the variable is set"
    );
    fs::create_dir_all(&claude).expect("mkdir");
    dq(&["install"]);
    assert_eq!(
        fs::read_link(claude.join("skills/dq")).expect("a link"),
        Path::new("../../../.agents/skills/dq")
    );
}

#[test]
fn project_is_the_current_directory_and_needs_no_home() {
    let home = Home::new();
    let project = home.root.join("project");
    fs::create_dir_all(project.join(".claude")).expect("mkdir");
    let dq = |args: &[&str]| {
        home.dq()
            .env_remove("HOME")
            .current_dir(&project)
            .args(["skill"])
            .args(args)
            .assert()
            .success();
    };
    dq(&["install", "--project"]);
    assert!(project.join(".agents/skills/dq/SKILL.md").is_file());
    assert!(project.join(".claude/skills/dq/SKILL.md").is_file());
    dq(&["update", "--project"]);
    dq(&["status", "--project"]);
    dq(&["remove", "--project"]);
    assert_eq!(tree(&home.root), []);
}

#[test]
fn a_home_that_is_none_is_refused() {
    let home = Home::new();
    for (value, message) in [
        (
            None,
            "HOME is not set, so there is no home directory to look for the skill in \
             (--project works in the current directory)\n",
        ),
        (
            Some("rel"),
            "HOME is 'rel', which is no absolute path of a directory\n",
        ),
        (
            Some("/no/such/home"),
            "HOME is '/no/such/home', which is no absolute path of a directory\n",
        ),
    ] {
        for command in ["install", "update", "status", "remove"] {
            let mut dq = home.dq();
            match value {
                Some(value) => dq.env("HOME", value),
                None => dq.env_remove("HOME"),
            };
            dq.args(["skill", command]).assert().code(6).stderr(message);
        }
    }
}

#[test]
fn write_flags_are_refused() {
    let home = Home::new();
    for flag in ["-i", "--check", "--diff", "--backup", "--continue-on-error"] {
        for command in ["install", "update", "status", "remove"] {
            assert!(
                home.refused(&[command, flag])
                    .starts_with("write-mode flag --"),
                "{command} {flag}"
            );
        }
    }
}

/// An unwritable directory is a write failure, an unreadable file a read
/// failure.
#[test]
fn file_errors_have_their_exit_codes() {
    let home = Home::new();
    if !modes_bind() {
        return;
    }
    chmod(&home.root, 0o500);
    let (code, _, stderr) = home.run(&["install"]);
    chmod(&home.root, 0o700);
    assert_eq!(code, 7, "{stderr}");
    home.ok(&["install"]);
    let skill_md = home.skill().join("SKILL.md");
    chmod(&skill_md, 0o000);
    let (code, _, stderr) = home.run(&["update"]);
    chmod(&skill_md, 0o600);
    assert_eq!(code, 5, "{stderr}");
}

/// The binary tests run is a debug build: it leaves an older skill as it is.
/// A release build would update it, which this test cannot see; the
/// decision is tested in `dq::skill`.
#[test]
#[cfg(debug_assertions)]
fn a_debug_build_updates_no_skill_by_itself() {
    let home = Home::new();
    home.ok(&["install"]);
    let manifest = home.skill().join(".dq-skill.json");
    let older = fs::read_to_string(&manifest)
        .expect("read")
        .replace(VERSION, "2020.1.0");
    fs::write(&manifest, &older).expect("write");
    fs::write(home.root.join("a.json"), "{}").expect("write");
    home.dq().args(["get", "a.json", ""]).assert().success();
    assert_eq!(fs::read_to_string(&manifest).expect("read"), older);
    assert_eq!(home.status(&[])["state"], json!("older"));
    assert_eq!(
        home.ok(&["update"]),
        format!("updated dq skill v2020.1.0 to v{VERSION} in ~/.agents/skills/dq\n")
    );
}

/// What a line has that starts the binary. In two pieces, so that this file
/// does not find itself.
const STARTS: [&str; 3] = [
    concat!("cargo_", "bin("),
    concat!("cargo_", "bin!"),
    concat!("CARGO_", "BIN_EXE_"),
];

/// The lines of `text` that start the binary, with whether it is given a
/// `HOME` of the test's: an `.env("HOME", …)` whose value is not read from
/// the environment, within 16 lines and before the next start. Anything
/// else, `.env_remove("HOME")` and a comment among it, leaves the binary
/// with the `HOME` the tests run with, or is not seen to change it.
fn starts(text: &str) -> Vec<(usize, bool)> {
    let is_code = |line: &str| !line.trim_start().starts_with("//");
    let lines: Vec<&str> = text.lines().collect();
    let found: Vec<usize> = (0..lines.len())
        .filter(|n| is_code(lines[*n]) && STARTS.iter().any(|name| lines[*n].contains(name)))
        .collect();
    let sets_home = |line: &&str| {
        is_code(line)
            && line
                .split_once(".env(\"HOME\", ")
                .is_some_and(|(_, value)| !value.contains("var") && !value.contains("env!"))
    };
    found
        .iter()
        .enumerate()
        .map(|(i, start)| {
            let next = found.get(i + 1).copied().unwrap_or(lines.len());
            let end = next.min(start + 16);
            (start + 1, lines[*start..end].iter().any(sets_home))
        })
        .collect()
}

/// Every place in the tests of this crate that starts the binary gives it a
/// `HOME`: under `cargo test --release` the binary may update the skill of
/// the home directory it has.
#[test]
fn no_test_runs_the_binary_in_the_developers_home() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut seen = 0;
    for entry in walkdir::WalkDir::new(tests) {
        let entry = entry.expect("entry");
        if entry.path().extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = fs::read_to_string(entry.path()).expect("read");
        for (line, has_home) in starts(&text) {
            seen += 1;
            assert!(
                has_home,
                "{}:{line}: starts dq with the HOME it inherits",
                entry.path().display()
            );
        }
    }
    assert!(seen > 40, "found {seen}");
}

#[test]
fn a_start_that_inherits_home_is_found() {
    let start = STARTS[0];
    for (rest, has_home) in [
        ("cmd.env_clear();\ncmd.env(\"HOME\", \"/tmp\");", true),
        (".env(\"HOME\", dir.path())", true),
        (".args([\"lint\"])", false),
        (".env(\"HOME\", std::env::var_os(\"HOME\").unwrap())", false),
        (".env(\"HOME\", env!(\"HOME\"))", false),
        (".env_remove(\"HOME\")", false),
        ("// .env(\"HOME\", \"/tmp\")", false),
    ] {
        let text = format!("{start}\"dq\")\n{rest}\n");
        assert_eq!(starts(&text), [(1, has_home)], "{rest}");
    }
    // The HOME of the next start is not this one's.
    let text = format!("{start}\n{start}\n.env(\"HOME\", \"/tmp\")\n");
    assert_eq!(starts(&text), [(1, false), (2, true)]);
    assert_eq!(starts(&format!("// {start}\n")), []);
}
