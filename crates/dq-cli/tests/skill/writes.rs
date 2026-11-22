//! How `dq skill` writes: what fails midway, what a run that was cut short
//! leaves, and what the next one makes of it.

use pretty_assertions::assert_eq;

use super::*;

/// The state `status` reports, and that no file of dq's differs or is gone.
fn whole(home: &Home) -> String {
    let status = home.status(&[]);
    assert_eq!(status["edited"], json!([]), "{status:#}");
    assert_eq!(status["missing"], json!([]), "{status:#}");
    status["state"].as_str().expect("state").to_owned()
}

/// The files in the skill directory, without what they hold.
fn names(home: &Home) -> Vec<String> {
    tree(&home.skill())
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// What an installed skill has.
const FILES: [&str; 5] = [
    MANIFEST,
    "SKILL.md",
    "references/formats.md",
    "references/linting.md",
    "references/pointers.md",
];

/// `references/` cannot be written to: `SKILL.md` is new, the rest is the
/// old version's, and so is the manifest. The next run goes on from there,
/// without `--force`.
#[test]
fn an_update_that_fails_midway_is_finished_by_the_next() {
    if !modes_bind() {
        return;
    }
    for command in ["update", "install"] {
        let home = Home::new();
        home.install_edition(&OLD);
        let references = home.skill().join("references");
        chmod(&references, 0o500);
        let (code, stdout, stderr) = home.run(&[command]);
        chmod(&references, 0o700);
        assert_eq!((code, stdout.as_str()), (7, ""), "{stderr}");
        assert!(
            stderr.contains("~/.agents/skills/dq/references/"),
            "{stderr}"
        );
        assert_eq!(
            fs::read_to_string(home.skill().join("SKILL.md")).expect("read"),
            source("SKILL.md")
        );
        assert_eq!(whole(&home), "older");
        // Failing once more changes nothing about that.
        chmod(&references, 0o500);
        assert_eq!(home.run(&[command]).0, 7);
        chmod(&references, 0o700);
        assert_eq!(
            home.ok(&[command]),
            format!("updated dq skill v2020.1.0 to v{VERSION} in ~/.agents/skills/dq\n")
        );
        assert_eq!(whole(&home), "up_to_date");
        assert_eq!(names(&home), FILES, "nothing left over");
    }
}

/// The skill directory cannot be written to: nothing in it changes.
#[test]
fn a_skill_directory_that_cannot_be_written_to_is_a_write_failure() {
    if !modes_bind() {
        return;
    }
    let home = Home::new();
    home.install_edition(&OLD);
    let before = tree(&home.skill());
    chmod(&home.skill(), 0o500);
    for args in [&["update"][..], &["update", "--force"], &["install"]] {
        let (code, stdout, stderr) = home.run(args);
        assert_eq!((code, stdout.as_str()), (7, ""), "{args:?}: {stderr}");
    }
    chmod(&home.skill(), 0o700);
    assert_eq!(tree(&home.skill()), before);
    home.ok(&["update"]);
    assert_eq!(whole(&home), "up_to_date");
}

/// `remove` that fails midway says so, and the next one finishes.
#[test]
fn a_remove_that_fails_midway_is_finished_by_the_next() {
    if !modes_bind() {
        return;
    }
    let home = Home::with_claude();
    home.ok(&["install"]);
    let references = home.skill().join("references");
    chmod(&references, 0o500);
    let (code, stdout, stderr) = home.run(&["remove"]);
    chmod(&references, 0o700);
    assert_eq!((code, stdout.as_str()), (7, ""), "{stderr}");
    assert!(home.skill().join(MANIFEST).is_file(), "the last to go");
    home.ok(&["remove"]);
    assert_eq!(tree(&home.root), []);
    assert!(!home.skill().exists());
}

/// The link cannot be made: the skill is there, and the next run makes it.
#[test]
fn a_link_that_cannot_be_made_is_a_write_failure() {
    if !modes_bind() {
        return;
    }
    let home = Home::with_claude();
    chmod(&home.root.join(".claude"), 0o500);
    let (code, stdout, stderr) = home.run(&["install"]);
    chmod(&home.root.join(".claude"), 0o700);
    assert_eq!((code, stdout.as_str()), (7, ""), "{stderr}");
    assert!(stderr.contains("~/.claude/skills"), "{stderr}");
    assert_eq!(home.status(&[])["state"], json!("up_to_date"));
    assert_eq!(
        home.ok(&["install"]),
        format!(
            "dq skill v{VERSION} in ~/.agents/skills/dq is up to date\n\
             linked ~/.claude/skills/dq\n"
        )
    );
}

/// A first install that was cut short leaves files with no manifest, as this
/// version has them, perhaps with a temporary file: not a foreign directory.
#[test]
fn an_install_that_was_cut_short_is_finished_by_the_next() {
    let home = Home::new();
    for left in [&FILES[1..2], &FILES[1..3], &FILES[1..]] {
        for path in left {
            let at = home.skill().join(path);
            fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
            fs::write(at, source(path)).expect("write");
        }
        fs::write(home.skill().join(".tmpAb12Cd"), "half").expect("write");
        assert_eq!(home.status(&[])["state"], json!("not_installed"));
        assert_eq!(
            home.ok(&["install"]),
            format!("installed dq skill v{VERSION} in ~/.agents/skills/dq\n"),
            "{left:?}"
        );
        assert_eq!(whole(&home), "up_to_date");
        home.ok(&["remove"]);
    }
    // One of another version in their midst is someone else's.
    fs::create_dir_all(home.skill()).expect("mkdir");
    fs::write(home.skill().join("SKILL.md"), "another").expect("write");
    assert!(
        home.refused(&["install"])
            .contains("was not installed by dq (SKILL.md)")
    );
}

/// An update that was cut short anywhere is finished by the next, `--force`
/// or not: the files written so far are this version's, the manifest is not.
#[test]
fn an_update_that_was_cut_short_is_finished_by_the_next() {
    for written in 0..=4 {
        let home = Home::new();
        home.install_edition(&OLD);
        for path in &FILES[1..=written] {
            fs::write(home.skill().join(path), source(path)).expect("write");
        }
        if written == 4 {
            fs::remove_file(home.skill().join("references/gone.md")).expect("rm");
        }
        let status = home.status(&[]);
        assert_eq!(
            (&status["state"], &status["edited"], &status["unlisted"]),
            (&json!("older"), &json!([]), &json!([])),
            "{written}"
        );
        home.ok(&["update"]);
        assert_eq!(whole(&home), "up_to_date", "{written}");
        assert_eq!(names(&home), FILES);
    }
}

/// A file in the place of a directory, a directory in the place of a file:
/// refused before anything is written, `--force` or not.
#[test]
fn what_is_in_the_way_of_the_files_is_refused_before_anything_is_written() {
    type Make = fn(&Path) -> std::io::Result<()>;
    let cases: [(&str, Make); 4] = [
        ("SKILL.md", |at| fs::create_dir(at)),
        ("references/formats.md", |at| {
            fs::create_dir(at).and_then(|()| fs::write(at.join("x"), "theirs"))
        }),
        ("references", |at| fs::write(at, "theirs")),
        ("references", |at| symlink("/nowhere", at)),
    ];
    for (path, make) in cases {
        for installed in [false, true] {
            let home = Home::new();
            let at = home.skill().join(path);
            if installed {
                home.install_edition(&OLD);
                if at.is_dir() {
                    fs::rename(&at, home.root.join("aside")).expect("mv");
                } else {
                    fs::remove_file(&at).expect("rm");
                }
            } else {
                fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
            }
            make(&at).expect("make");
            for args in [&["install", "--force"][..], &["update", "--force"]] {
                // Nothing dq could have installed is there: nothing to update.
                if path == "SKILL.md" && !installed && args[0] == "update" {
                    continue;
                }
                assert_eq!(
                    home.refused(args),
                    format!(
                        "~/.agents/skills/dq cannot hold the files of the skill ({path}): \
                         dq neither goes through a link nor replaces a directory"
                    ),
                    "installed: {installed}"
                );
            }
        }
    }
}

/// A link or a file of the user's inside the skill directory, where dq has
/// nothing to write, stays what it is through everything.
#[test]
fn links_where_dq_writes_nothing_stay() {
    let home = Home::new();
    home.install_edition(&OLD);
    let outside = home.root.join("outside");
    fs::create_dir(&outside).expect("mkdir");
    fs::write(outside.join("file.md"), "theirs").expect("write");
    symlink(&outside, home.skill().join("mine")).expect("symlink");
    symlink("/nowhere", home.skill().join("references/mine.md")).expect("symlink");
    home.ok(&["update"]);
    assert_eq!(whole(&home), "up_to_date");
    assert_eq!(
        home.status(&[])["unlisted"],
        json!(["mine", "references/mine.md"]),
        "the link, not what it leads to"
    );
    assert_eq!(
        home.ok(&["remove"]),
        format!(
            "removed dq skill v{VERSION} from ~/.agents/skills/dq\n\
             kept ~/.agents/skills/dq: not dq's are mine, references/mine.md\n"
        )
    );
    assert!(home.skill().join("mine").is_symlink());
    assert_eq!(
        fs::read_to_string(outside.join("file.md")).expect("read"),
        "theirs"
    );
}

/// The link of an older version's file that this one has not: `--force`
/// keeps it, as any edited file of that kind, and nothing goes through it.
#[test]
fn a_link_in_the_place_of_a_file_this_version_has_not_is_kept() {
    let home = Home::new();
    home.install_edition(&OLD);
    let outside = home.root.join("outside.md");
    fs::write(&outside, "old gone\n").expect("write"); // byte for byte dq's
    let gone = home.skill().join("references/gone.md");
    fs::remove_file(&gone).expect("rm");
    symlink(&outside, &gone).expect("symlink");
    assert!(
        home.refused(&["update"])
            .contains("edited references/gone.md")
    );
    assert!(home.ok(&["update", "--force"]).ends_with(&format!(
        "kept references/gone.md: it was edited, and v{VERSION} has no such file\n"
    )));
    assert!(gone.is_symlink());
    assert_eq!(fs::read_to_string(&outside).expect("read"), "old gone\n");
}

/// What a system puts into a directory it shows is nobody's file, and dq
/// leaves it.
#[test]
fn a_finder_file_is_in_no_ones_way_and_stays() {
    let home = Home::new();
    fs::create_dir_all(home.skill()).expect("mkdir");
    fs::write(home.skill().join(".DS_Store"), "finder").expect("write");
    home.ok(&["install"]); // not a foreign directory
    fs::write(home.skill().join("references/.DS_Store"), "finder").expect("write");
    assert_eq!(home.status(&[])["unlisted"], json!([]));
    home.ok(&["update"]);
    assert_eq!(
        home.ok(&["remove"]),
        format!(
            "removed dq skill v{VERSION} from ~/.agents/skills/dq\n\
             kept ~/.agents/skills/dq: it is not empty (.DS_Store, references/.DS_Store)\n"
        )
    );
    assert_eq!(names(&home), [".DS_Store", "references/.DS_Store"]);
    assert_eq!(home.status(&[])["state"], json!("not_installed"));
    home.ok(&["install"]);
    assert_eq!(whole(&home), "up_to_date");
}

/// What nobody counts as a file of theirs is named, up to three of it.
#[test]
fn what_keeps_the_directory_from_being_empty_is_named() {
    let home = Home::new();
    home.ok(&["install"]);
    for name in [".tmpAb12Cd", "references/.tmpZz9zZz", ".DS_Store"] {
        fs::write(home.skill().join(name), "left").expect("write");
    }
    let kept = |names: &str| {
        format!(
            "removed dq skill v{VERSION} from ~/.agents/skills/dq\n\
             kept ~/.agents/skills/dq: it is not empty ({names})\n"
        )
    };
    assert_eq!(
        home.ok(&["remove"]),
        kept(".DS_Store, .tmpAb12Cd, references/.tmpZz9zZz")
    );
    home.ok(&["install"]);
    fs::create_dir(home.skill().join("empty")).expect("mkdir");
    assert_eq!(
        home.ok(&["remove"]),
        kept(".DS_Store, .tmpAb12Cd, empty and 1 more")
    );
}

/// A directory of the user's that cannot be read is theirs, whatever is in
/// it, and in no one's way.
#[test]
fn a_directory_that_cannot_be_read_is_not_dqs() {
    if !modes_bind() {
        return;
    }
    let home = Home::new();
    home.install_edition(&OLD);
    let mine = home.skill().join("references/mine");
    fs::create_dir(&mine).expect("mkdir");
    fs::write(mine.join("mine.md"), "mine").expect("write");
    chmod(&mine, 0o000);
    let unlisted = |home: &Home| home.status(&[])["unlisted"].clone();
    assert_eq!(unlisted(&home), json!(["references/mine"]));
    for command in ["update", "install"] {
        home.ok(&[command]);
        assert_eq!(whole(&home), "up_to_date");
        assert_eq!(unlisted(&home), json!(["references/mine"]));
    }
    let said = home.ok(&["remove"]);
    let refused = home.run(&["install"]);
    chmod(&mine, 0o700);
    assert_eq!(
        refused,
        (
            6,
            String::new(),
            "~/.agents/skills/dq was not installed by dq (references/mine): \
             --force writes dq's files into it and keeps the rest\n"
                .to_owned()
        )
    );
    assert!(
        said.ends_with("kept ~/.agents/skills/dq: not dq's are references/mine\n"),
        "{said}"
    );
    assert_eq!(
        tree(&home.skill()),
        [("references/mine/mine.md".to_owned(), "mine".to_owned())]
    );
}

/// A directory whose name differs in case is the user's as soon as a file of
/// theirs is in it: it keeps its name, and the files in it are dq's there.
#[test]
fn a_directory_with_a_file_of_the_users_keeps_its_name() {
    let home = Home::new();
    home.install_edition(&cased());
    fs::write(home.skill().join("References/mine.md"), "mine").expect("write");
    home.ok(&["update"]);
    assert_eq!(whole(&home), "up_to_date");
    let expected: &[&str] = if ignores_case(&home) {
        &[
            MANIFEST,
            "References",
            "References/formats.md",
            "References/linting.md",
            "References/mine.md",
            "References/pointers.md",
            "SKILL.md",
        ]
    } else {
        &[
            MANIFEST,
            "References",
            "References/mine.md",
            "SKILL.md",
            "references",
            "references/formats.md",
            "references/linting.md",
            "references/pointers.md",
        ]
    };
    assert_eq!(entries(&home), expected);
    assert_eq!(home.status(&[])["unlisted"], json!(["References/mine.md"]));
    // Nothing more to do, and no more than dq's goes.
    assert_eq!(
        home.ok(&["update"]),
        format!("dq skill v{VERSION} in ~/.agents/skills/dq is up to date\n")
    );
    home.ok(&["remove"]);
    assert_eq!(entries(&home), ["References", "References/mine.md"]);
}

/// The mode of a file does not stand in the way of replacing or removing
/// it: it is dq's, and the directory can be written to.
#[test]
fn read_only_files_of_dqs_are_replaced_and_removed() {
    let home = Home::new();
    let lock = |home: &Home| {
        for path in names(home) {
            chmod(&home.skill().join(path), 0o444);
        }
    };
    home.install_edition(&OLD);
    lock(&home);
    home.ok(&["update"]);
    assert_eq!(whole(&home), "up_to_date");
    assert_eq!(names(&home), FILES);
    lock(&home);
    fs::remove_file(home.skill().join("SKILL.md")).expect("rm");
    home.ok(&["update"]);
    lock(&home);
    home.ok(&["remove"]);
    assert_eq!(tree(&home.root), []);
}

/// An older skill with files whose names this version spells in another case:
/// one with other text, one in a directory, and one with the text it has now.
fn cased() -> Edition {
    static FILES: std::sync::OnceLock<Vec<SkillFile>> = std::sync::OnceLock::new();
    let files = FILES.get_or_init(|| {
        let file = |path, text| SkillFile { path, text };
        let linting = Edition::embedded()
            .text("references/linting.md")
            .expect("linting.md");
        vec![
            file("Skill.md", "old skill\n"),
            file("References/FORMATS.md", "old formats\n"),
            file("References/LINTING.md", linting),
            file("References/gone.md", "old gone\n"),
        ]
    });
    Edition {
        version: "2020.1.0",
        files,
    }
}

/// Whether the file system finds a file by its name in another case.
fn ignores_case(home: &Home) -> bool {
    fs::write(home.root.join("probe"), "").expect("write");
    home.root.join("PROBE").exists()
}

/// Every file and directory in the skill directory.
fn entries(home: &Home) -> Vec<String> {
    let mut entries: Vec<String> = walkdir::WalkDir::new(home.skill())
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.expect("entry");
            let path = entry.path().strip_prefix(home.skill()).expect("inside");
            path.to_str().expect("UTF-8").to_owned()
        })
        .collect();
    entries.sort();
    entries
}

/// The skill is whole, of this version, with the names it has now and no
/// other file or directory.
fn assert_respelled(home: &Home) {
    assert_eq!(whole(home), "up_to_date");
    assert_eq!(home.status(&[])["unlisted"], json!([]));
    assert_eq!(
        entries(home),
        [
            MANIFEST,
            "SKILL.md",
            "references",
            "references/formats.md",
            "references/linting.md",
            "references/pointers.md",
        ]
    );
}

/// A name that changed only in case is one file under its new name, on a
/// file system that ignores case and on one that does not.
#[test]
fn names_that_differ_in_case_only_are_the_new_ones_after_an_update() {
    for args in [&["update"][..], &["install"], &["update", "--force"]] {
        let home = Home::new();
        home.install_edition(&cased());
        assert_eq!(whole(&home), "older");
        assert_eq!(
            home.ok(args),
            format!("updated dq skill v2020.1.0 to v{VERSION} in ~/.agents/skills/dq\n"),
            "{args:?}"
        );
        assert_respelled(&home);
        home.ok(&["remove"]);
        assert_eq!(tree(&home.root), []);
        assert!(!home.skill().exists());
    }
}

/// What a run leaves that was cut short: names that are the new ones
/// already, over text that is not, and some that is.
#[test]
fn a_change_of_case_that_was_cut_short_is_finished_by_the_next() {
    for written in [false, true] {
        let home = Home::new();
        home.install_edition(&cased());
        if ignores_case(&home) {
            for (old, new) in [
                ("Skill.md", "SKILL.md"),
                ("References", "references"),
                ("references/FORMATS.md", "references/formats.md"),
            ] {
                fs::rename(home.skill().join(old), home.skill().join(new)).expect("mv");
            }
        }
        if written {
            for path in ["SKILL.md", "references/formats.md"] {
                let at = home.skill().join(path);
                fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
                fs::write(at, source(path)).expect("write");
            }
        }
        assert_eq!(whole(&home), "older", "{written}");
        assert_eq!(home.status(&[])["unlisted"], json!([]), "{written}");
        home.ok(&["update"]);
        assert_respelled(&home);
    }
}

/// Edited under the old name: refused, named once, and with `--force`
/// replaced where it is the file of the new name, kept where it is not.
#[test]
fn an_edited_file_whose_name_differs_in_case_is_not_lost() {
    let home = Home::new();
    home.install_edition(&cased());
    let edited = home.skill().join("References/FORMATS.md");
    fs::write(&edited, "mine").expect("write");
    for command in ["update", "install"] {
        assert_eq!(
            home.refused(&[command]),
            "~/.agents/skills/dq was changed since dq wrote it: edited References/FORMATS.md \
             (--force writes dq's files in their place)"
        );
    }
    let updated = format!("updated dq skill v2020.1.0 to v{VERSION} in ~/.agents/skills/dq\n");
    let said = home.ok(&["update", "--force"]);
    if ignores_case(&home) {
        assert_eq!(said, updated);
        assert_respelled(&home);
    } else {
        assert_eq!(
            said,
            format!(
                "{updated}kept References/FORMATS.md: \
                 it was edited, and v{VERSION} has no such file\n"
            )
        );
        assert_eq!(whole(&home), "up_to_date");
        assert_eq!(fs::read_to_string(&edited).expect("read"), "mine");
    }
}

/// A file of the user's under a name that differs in case is in the way
/// where it is the file, and in no one's way where it is not.
#[test]
fn a_file_of_the_users_whose_name_differs_in_case_is_replaced_only_by_force() {
    let home = Home::new();
    let ignores_case = ignores_case(&home);
    fs::create_dir_all(home.skill()).expect("mkdir");
    let theirs = home.skill().join("skill.md");
    fs::write(&theirs, "theirs").expect("write");
    assert!(
        home.refused(&["install"])
            .contains("not installed by dq (skill.md)")
    );
    home.ok(&["install", "--force"]);
    if ignores_case {
        assert_respelled(&home);
    } else {
        assert_eq!(whole(&home), "up_to_date");
        home.ok(&["remove"]);
        assert_eq!(
            tree(&home.skill()),
            [("skill.md".to_owned(), "theirs".to_owned())]
        );
    }

    // In the place of a file only the new version has.
    let home = Home::new();
    home.install_edition(&OLD);
    fs::write(home.skill().join("references/LINTING.md"), "theirs").expect("write");
    if ignores_case {
        assert_eq!(
            home.refused(&["update"]),
            "~/.agents/skills/dq was changed since dq wrote it: \
             not dq's: references/linting.md (--force writes dq's files in their place)"
        );
        home.ok(&["update", "--force"]);
        assert_respelled(&home);
    } else {
        home.ok(&["update"]);
        assert_eq!(
            home.status(&[])["unlisted"],
            json!(["references/LINTING.md"])
        );
    }
}

/// The user renamed a file of dq's: where that is the same file, it gets its
/// name back; where it is not, one is missing and the other the user's.
#[test]
fn a_file_renamed_in_case_only_gets_its_name_back() {
    let home = Home::new();
    home.ok(&["install"]);
    fs::rename(home.skill().join("SKILL.md"), home.skill().join("skill.md")).expect("mv");
    let status = home.status(&[]);
    let expected = if ignores_case(&home) {
        (json!([]), json!([]))
    } else {
        (json!(["SKILL.md"]), json!(["skill.md"]))
    };
    assert_eq!(
        (&status["missing"], &status["unlisted"]),
        (&expected.0, &expected.1)
    );
    assert_eq!(
        home.ok(&["update"]),
        format!("restored dq skill v{VERSION} in ~/.agents/skills/dq\n")
    );
    if ignores_case(&home) {
        assert_respelled(&home);
    } else {
        assert_eq!(whole(&home), "up_to_date");
        assert_eq!(home.status(&[])["unlisted"], json!(["skill.md"]));
    }
    let before = tree(&home.skill());
    assert_eq!(
        home.ok(&["update"]),
        format!("dq skill v{VERSION} in ~/.agents/skills/dq is up to date\n")
    );
    assert_eq!(tree(&home.skill()), before);
}

/// Two names of one file (a hard link) are two entries, so one of them is
/// not dq's, whoever the file itself belongs to.
#[test]
fn a_hard_link_to_a_file_of_dqs_is_not_dqs() {
    let home = Home::new();
    home.install_edition(&OLD);
    let gone = home.skill().join("references/gone.md");
    let linting = home.skill().join("references/linting.md");
    fs::hard_link(&gone, &linting).expect("ln");
    let backup = home.skill().join("references/backup.md");
    fs::hard_link(&gone, &backup).expect("ln");
    assert_eq!(
        home.refused(&["update"]),
        "~/.agents/skills/dq was changed since dq wrote it: \
         not dq's: references/linting.md (--force writes dq's files in their place)"
    );
    home.ok(&["update", "--force"]);
    assert_eq!(whole(&home), "up_to_date");
    assert!(!gone.exists(), "dq's, and this version has it not");
    assert_eq!(fs::read_to_string(&backup).expect("read"), "old gone\n");
    assert_eq!(
        home.status(&[])["unlisted"],
        json!(["references/backup.md"])
    );
}

/// A file that is dq's already, byte for byte, is not written again.
#[test]
fn a_file_that_is_up_to_date_is_not_touched() {
    use std::os::unix::fs::MetadataExt;
    let home = Home::new();
    home.ok(&["install"]);
    let inodes = || FILES.map(|path| fs::metadata(home.skill().join(path)).expect("stat").ino());
    let before = inodes();
    home.ok(&["install"]);
    home.ok(&["update", "--force"]);
    assert_eq!(inodes(), before);
    fs::write(home.skill().join("SKILL.md"), "mine").expect("write");
    home.ok(&["update", "--force"]);
    let after = inodes();
    assert_eq!(
        &after[2..],
        &before[2..],
        "but for the manifest and the file"
    );
}
