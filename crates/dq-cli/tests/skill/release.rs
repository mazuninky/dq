//! The automatic update, by the real binary. Only a release build does it, so
//! in a debug build these tests are ignored. Run them with either of
//!
//! ```sh
//! cargo test --release -p dq-cli --test cli_skill
//! # 20 s to build instead of two minutes, in a directory of its own
//! CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=false CARGO_TARGET_DIR=target/no-debug \
//!     cargo test -p dq-cli --test cli_skill
//! ```

use std::process::Stdio;

use pretty_assertions::assert_eq;

use super::*;

/// A home directory with an older skill, and data to work on.
fn home() -> Home {
    let home = Home::new();
    home.install_edition(&OLD);
    fs::write(home.root.join("a.json"), "{\"a\": 1}\n").expect("write");
    fs::write(home.root.join("bad.json"), "{").expect("write");
    home
}

/// Every file of the skill, and the manifest.
fn skill(home: &Home) -> Vec<(String, String)> {
    tree(&home.skill())
}

/// The version that is installed, with every file as that version wrote it.
fn version(home: &Home) -> String {
    let status = home.status(&[]);
    assert_eq!(status["edited"], json!([]), "{status:#}");
    assert_eq!(status["missing"], json!([]), "{status:#}");
    assert_eq!(status["unlisted"], json!([]), "{status:#}");
    status["version"].as_str().expect("version").to_owned()
}

/// Ordinary commands of every kind of outcome.
const COMMANDS: [&[&str]; 9] = [
    &["get", "a.json", "/a"],
    &["get", "-F", "json", "a.json", ""],
    &["get", "-q", "a.json", "/a"],
    &["get", "a.json", "/b"],
    &["get", "bad.json", ""],
    &["get", "none.json", ""],
    &["get", "-i", "a.json", ""],
    &["validate", "bad.json"],
    &["completions", "bash"],
];

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn an_ordinary_command_updates_an_older_skill_and_is_not_changed_by_it() {
    for command in COMMANDS {
        let run = |home: &Home| home.finish(home.dq().args(command));
        let without = home();
        without.ok(&["remove"]);
        let expected = run(&without);
        assert!(!without.skill().exists(), "{command:?} made a skill");

        let home = home();
        assert_eq!(run(&home), expected, "{command:?}");
        assert_eq!(version(&home), VERSION, "{command:?}");
        assert!(!home.skill().join("references/gone.md").exists());
        assert!(!home.link().exists(), "only `install` makes the link");
        // And the second run, which finds nothing to do.
        let before = skill(&home);
        assert_eq!(run(&home), expected, "{command:?}");
        assert_eq!(skill(&home), before);
    }
}

/// clap ends the process itself; the next run does it.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn help_and_version_update_nothing() {
    for args in [
        &["--help"][..],
        &["--version"],
        &["get"],
        &["--no-such-flag"],
    ] {
        let home = home();
        home.finish(home.dq().args(args));
        assert_eq!(version(&home), OLD.version, "{args:?}");
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn the_environment_turns_it_off() {
    for (name, value, expected) in [
        ("CI", "true", OLD.version),
        ("CI", "", OLD.version),
        ("CI", "false", OLD.version),
        ("DQ_NO_SKILL_UPDATE", "1", OLD.version),
        ("DQ_NO_SKILL_UPDATE", "", OLD.version),
        ("DQ_NO_SKILL_UPDATE", "0", VERSION),
        ("HOME", "", OLD.version),
        ("HOME", "rel", OLD.version),
        ("HOME", "/no/such/home", OLD.version),
    ] {
        let home = home();
        let said = home.finish(home.dq().env(name, value).args(["get", "a.json", "/a"]));
        assert_eq!(said, (0, "1\n".to_owned(), String::new()), "{name}={value}");
        assert_eq!(version(&home), expected, "{name}={value}");
    }
    let home = home();
    home.dq()
        .env_remove("HOME")
        .args(["get", "a.json", "/a"])
        .assert()
        .success()
        .stdout("1\n")
        .stderr("");
    assert_eq!(version(&home), OLD.version);
}

/// `dq skill` and `dq self` see to the skill themselves.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn the_commands_about_the_skill_and_dq_itself_update_nothing() {
    for args in [
        &["skill", "status"][..],
        &["skill", "status", "--project"],
        &["skill", "install", "--check"],
        // Refused before they would reach the network.
        &["self", "check", "--check"],
        &["self", "update", "--check"],
    ] {
        let home = home();
        home.finish(home.dq().args(args));
        assert_eq!(version(&home), OLD.version, "{args:?}");
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn a_skill_that_is_not_dqs_to_update_is_left_alone() {
    let changes: [fn(&Home); 8] = [
        |home| fs::write(home.skill().join("SKILL.md"), "mine").expect("write"),
        |home| fs::remove_file(home.skill().join("SKILL.md")).expect("rm"),
        |home| fs::write(home.skill().join("references/linting.md"), "mine").expect("write"),
        |home| home.change_manifest(|value| value["dq_version"] = json!("9999.1.0")),
        |home| home.change_manifest(|value| value["dq_version"] = json!(VERSION)),
        |home| home.write_manifest("{}"),
        |home| fs::remove_file(home.skill().join(MANIFEST)).expect("rm"),
        |home| {
            let aside = home.root.join("aside");
            fs::rename(home.skill(), &aside).expect("mv");
            symlink(aside, home.skill()).expect("symlink");
        },
    ];
    for (n, change) in changes.into_iter().enumerate() {
        let home = home();
        change(&home);
        let before = tree(&home.root);
        let said = home.finish(home.dq().args(["get", "a.json", "/a"]));
        assert_eq!(said, (0, "1\n".to_owned(), String::new()), "case {n}");
        assert_eq!(tree(&home.root), before, "case {n}");
    }
    // A file of the user's is in no one's way, and stays.
    let home = home();
    fs::write(home.skill().join("notes.md"), "mine").expect("write");
    home.dq().args(["get", "a.json", "/a"]).assert().success();
    assert_eq!(home.status(&[])["state"], json!("up_to_date"));
    assert_eq!(home.status(&[])["unlisted"], json!(["notes.md"]));
}

/// The project's skill is under version control: the project's decision.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn the_skill_of_a_project_is_left_alone() {
    let home = home();
    let project = home.root.join("project");
    let env = Environment {
        current_dir: camino::Utf8PathBuf::from_path_buf(project.clone()).ok(),
        ..Environment::default()
    };
    fs::create_dir(&project).expect("mkdir");
    dq::skill::install(&env.project().expect("target"), &OLD, false).expect("install");
    fs::write(project.join("a.json"), "{\"a\": 1}\n").expect("write");
    let before = tree(&project);
    home.dq()
        .current_dir(&project)
        .args(["get", "a.json", "/a"])
        .assert()
        .success();
    assert_eq!(tree(&project), before);
    assert_eq!(version(&home), VERSION);
}

/// A failure is a debug log line; the next run that can, does.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn a_skill_that_cannot_be_written_changes_nothing_of_the_command() {
    if !modes_bind() {
        return;
    }
    let home = home();
    let before = skill(&home);
    let get = |log: &str| {
        chmod(&home.skill(), 0o500);
        let said = home.finish(home.dq().env("RUST_LOG", log).args(["get", "a.json", "/a"]));
        chmod(&home.skill(), 0o700);
        said
    };
    assert_eq!(get("info"), (0, "1\n".to_owned(), String::new()));
    let (code, stdout, stderr) = get("debug");
    assert_eq!((code, stdout.as_str()), (0, "1\n"));
    assert!(
        stderr.contains("DEBUG") && stderr.contains("the dq skill was not updated: "),
        "{stderr}"
    );
    assert_eq!(skill(&home), before);
    home.dq().args(["get", "a.json", "/a"]).assert().success();
    assert_eq!(version(&home), VERSION);
}

/// A reader that is gone: the process may die of SIGPIPE before it comes to
/// the skill, or after; the skill is of one version.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn a_reader_that_is_gone_leaves_a_whole_skill() {
    let dq = assert_cmd::cargo::cargo_bin("dq");
    for size in [1, 100_000] {
        let home = home();
        let list = vec!["some text"; size];
        fs::write(home.root.join("l.json"), json!(list).to_string()).expect("write");
        let mut sh = Command::new("/bin/sh");
        sh.env_clear()
            .env("HOME", &home.root)
            .env("PATH", "/usr/bin:/bin")
            .env("DQ", &dq)
            .current_dir(&home.root)
            .args([
                "-c",
                "{ \"$DQ\" get -F json l.json ''; echo $? >code; } | :",
            ]);
        let (code, stdout, stderr) = home.finish(&mut sh);
        assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "", ""));
        let code = fs::read_to_string(home.root.join("code")).expect("read");
        assert!(["0\n", "141\n"].contains(&code.as_str()), "{code}");
        assert!([OLD.version, VERSION].contains(&version(&home).as_str()));
        home.dq().args(["get", "a.json", "/a"]).assert().success();
        assert_eq!(version(&home), VERSION);
    }
}

/// What is read by another program is out before the skill is looked at,
/// and all of it.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn the_output_is_whole_in_a_pipe_and_in_a_file() {
    let home = home();
    let text = json!(vec!["some text"; 50_000]).to_string();
    fs::write(home.root.join("l.json"), &text).expect("write");
    let out = fs::File::create(home.root.join("out.json")).expect("create");
    let mut dq = std::process::Command::new(assert_cmd::cargo::cargo_bin("dq"));
    dq.env_clear()
        .env("HOME", &home.root)
        .current_dir(&home.root)
        .args(["get", "-F", "jsonl", "l.json", ""])
        .stdin(Stdio::null());
    let status = dq.stdout(out).status().expect("run dq");
    assert!(status.success());
    assert_eq!(version(&home), VERSION);
    let in_file = fs::read_to_string(home.root.join("out.json")).expect("read");
    let in_pipe = home
        .dq()
        .args(["get", "-F", "jsonl", "l.json", ""])
        .output()
        .expect("run dq")
        .stdout;
    assert_eq!(in_file.len(), in_pipe.len());
    assert!(in_file.as_bytes() == in_pipe.as_slice());
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "cargo test --release -p dq-cli --test cli_skill"
)]
fn many_ordinary_commands_at_once() {
    let home = home();
    let children: Vec<_> = (0..16)
        .map(|_| {
            std::process::Command::new(assert_cmd::cargo::cargo_bin("dq"))
                .env_clear()
                .env("HOME", &home.root)
                .current_dir(&home.root)
                .args(["get", "a.json", "/a"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn dq")
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().expect("wait");
        assert!(output.status.success());
        assert_eq!(
            (output.stdout.as_slice(), output.stderr.len()),
            (&b"1\n"[..], 0)
        );
    }
    home.dq().args(["get", "a.json", "/a"]).assert().success();
    assert_eq!(version(&home), VERSION);
    assert!(!skill(&home).iter().any(|(path, _)| path.contains(".tmp")));
}
