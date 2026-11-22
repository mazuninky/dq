//! `--project`: the skill of the current directory, and nothing else.

use pretty_assertions::assert_eq;

use super::*;

/// A home directory with a skill and Claude Code, and a project in it.
fn project() -> (Home, PathBuf) {
    let home = Home::with_claude();
    home.ok(&["install"]);
    let dir = home.root.join("project");
    fs::create_dir(&dir).expect("mkdir");
    (home, dir)
}

/// `dq skill ARGS --project` in `dir`, which must succeed; what it prints.
fn ok(home: &Home, dir: &Path, args: &[&str]) -> String {
    let mut dq = home.dq();
    dq.current_dir(dir).arg("skill").args(args).arg("--project");
    let (code, stdout, stderr) = home.finish(&mut dq);
    assert_eq!((code, stderr.as_str()), (0, ""), "{args:?}");
    stdout
}

/// Every directory under `dir`, sorted.
fn directories(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.expect("entry"))
        .filter(|entry| entry.file_type().is_dir())
        .map(|entry| {
            let path = entry.path().strip_prefix(dir).expect("under dir");
            path.to_str().expect("UTF-8").to_owned()
        })
        .collect();
    found.sort();
    found
}

/// Claude Code is not there: no link, and no directory of its.
#[test]
fn a_project_without_claude_code_gets_no_link() {
    for taken in [false, true] {
        let (home, dir) = project();
        if taken {
            fs::write(dir.join(".claude"), "theirs").expect("write");
        }
        let before = tree(&dir);
        assert_eq!(
            ok(&home, &dir, &["install"]),
            format!("installed dq skill v{VERSION} in ~/project/.agents/skills/dq\n")
        );
        assert_eq!(directories(&dir)[0], ".agents", "taken: {taken}");
        assert!(!directories(&dir).contains(&".claude".to_owned()));
        let status: Value =
            serde_json::from_str(&ok(&home, &dir, &["status", "-F", "json"])).expect("JSON");
        assert_eq!(status["link_state"], json!("not_wanted"));
        assert_eq!(
            ok(&home, &dir, &["remove"]),
            format!("removed dq skill v{VERSION} from ~/project/.agents/skills/dq\n")
        );
        assert_eq!(tree(&dir), before, "taken: {taken}");
    }
}

/// Installing and removing leaves no file of dq's and changes none of the
/// project's; what stays is empty directories, which git does not see.
#[test]
fn removing_leaves_the_project_as_it_was() {
    let (home, dir) = project();
    for (path, text) in [
        (".git/HEAD", "ref: refs/heads/main\n"),
        (".git/index", "index"),
        (".claude/settings.json", "{}"),
        (".claude/skills/other/SKILL.md", "theirs"),
        (".agents/skills/other/SKILL.md", "theirs"),
        ("README.md", "# project\n"),
    ] {
        let at = dir.join(path);
        fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
        fs::write(at, text).expect("write");
    }
    let before = (tree(&dir), directories(&dir));
    assert_eq!(
        ok(&home, &dir, &["install"]),
        format!(
            "installed dq skill v{VERSION} in ~/project/.agents/skills/dq\n\
             linked ~/project/.claude/skills/dq\n"
        )
    );
    assert_eq!(
        fs::read_link(dir.join(".claude/skills/dq")).expect("a link"),
        Path::new("../../.agents/skills/dq"),
        "relative, so that it is the same in every checkout"
    );
    ok(&home, &dir, &["remove"]);
    assert_eq!((tree(&dir), directories(&dir)), before);
}

/// The project is the current directory, not the repository it is in.
#[test]
fn a_directory_inside_a_repository_is_a_project_of_its_own() {
    let (home, dir) = project();
    fs::create_dir_all(dir.join(".git")).expect("mkdir");
    fs::create_dir_all(dir.join(".claude")).expect("mkdir");
    let inside = dir.join("crates/one");
    fs::create_dir_all(&inside).expect("mkdir");
    ok(&home, &inside, &["install"]);
    assert!(inside.join(".agents/skills/dq/SKILL.md").is_file());
    assert_eq!(directories(&dir)[..2], [".claude", ".git"]);
    assert!(!dir.join(".agents").exists());
    ok(&home, &inside, &["remove"]);
    assert_eq!(tree(&dir), []);
}

/// The two skills do not know of each other, and neither has a link to the
/// other's.
#[test]
fn the_skill_of_the_home_directory_is_left_alone() {
    let (home, dir) = project();
    fs::create_dir(dir.join(".claude")).expect("mkdir");
    let outside = |home: &Home| {
        let mut entries = tree(&home.root);
        entries.retain(|(path, _)| !path.starts_with("project/"));
        entries
    };
    let before = outside(&home);
    let config = home.root.join("config");
    fs::create_dir(&config).expect("mkdir");
    for args in [
        &["install"][..],
        &["update"],
        &["install", "--force"],
        &["remove"],
    ] {
        home.dq()
            .current_dir(&dir)
            .env("CLAUDE_CONFIG_DIR", &config)
            .arg("skill")
            .args(args)
            .arg("--project")
            .assert()
            .success();
        assert_eq!(outside(&home), before, "{args:?}");
    }
    ok(&home, &dir, &["install"]);
    home.ok(&["remove"]);
    assert!(dir.join(".claude/skills/dq/SKILL.md").is_file());
}

/// A link to the skill directory of the home directory is another link: the
/// project's would lead to its own.
#[test]
fn a_link_to_the_skill_of_the_home_directory_is_another_link() {
    let (home, dir) = project();
    fs::create_dir_all(dir.join(".claude/skills")).expect("mkdir");
    symlink(home.skill(), dir.join(".claude/skills/dq")).expect("symlink");
    let mut dq = home.dq();
    dq.current_dir(&dir).args(["skill", "install", "--project"]);
    let (code, _, stderr) = home.finish(&mut dq);
    assert_eq!(
        (code, stderr.as_str()),
        (
            6,
            "~/project/.claude/skills/dq is a link to '~/.agents/skills/dq': \
             --force replaces it\n"
        )
    );
    assert!(!dir.join(".agents").exists());
    ok(&home, &dir, &["install", "--force"]);
    assert_eq!(
        fs::read_link(dir.join(".claude/skills/dq")).expect("a link"),
        Path::new("../../.agents/skills/dq")
    );
    assert!(home.skill().join("SKILL.md").is_file());
}
