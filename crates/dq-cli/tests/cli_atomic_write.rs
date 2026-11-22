//! Atomic-write side-effect tests.
//!
//! These confirm the contract of `dq_core::atomic_write::write` end-to-end
//! through the binary:
//! - After `dq set f.yaml /x 1 -i`, the target file holds the new content
//!   AND no `tempfile`-style intermediate files (`.tmp.<random>`) remain in
//!   the parent directory.
//! - With `--backup`, both the target file and the corresponding `.bak` file
//!   exist with the right content.
//! - Every command that writes keeps the permission bits of the file, and
//!   edits the file a symbolic link points to; the link stays.
//!
//! NOTE: simulating a SIGKILL mid-rename to verify atomicity in the strict
//! "no torn writes" sense is out of scope for the Rust test harness — that
//! would need an external supervisor. M2 tasks.md flags this as a TODO.

use std::path::PathBuf;
use std::process::Command as StdCommand;

use assert_cmd::Command;

fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// Read every file in `dir` and return their (relative) names sorted. Used
/// to assert "only these files are present, nothing else".
fn list_files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn in_place_set_leaves_no_temp_files_behind() {
    // Seed an isolated tempdir with one YAML file so we can assert "exactly
    // these files exist after the write". If atomic_write leaks a `.tmp.*`
    // file, the post-condition will catch it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path: PathBuf = dir.path().join("config.yaml");
    std::fs::write(&path, "spec:\n  replicas: 3\n").expect("seed file");

    let path_str = path.to_str().unwrap();
    dq().args(["set", path_str, "/spec/replicas", "5", "-i", "--no-color"])
        .assert()
        .success();

    // File contents updated.
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("replicas: 5"),
        "in-place mutation must apply, got:\n{contents}",
    );

    // Only the original file remains — no `.tmp.<random>` leftovers.
    let files = list_files(dir.path());
    assert_eq!(
        files,
        vec!["config.yaml".to_owned()],
        "tempdir must contain only the target file, found: {files:?}",
    );
}

#[test]
fn backup_flag_creates_bak_file_alongside() {
    // With `-i --backup`, the target file is updated AND a `.bak` sibling
    // contains the original bytes. The backup-path rule in
    // `atomic_write::backup_path_for` is "always append .bak", so
    // `config.yaml` → `config.yaml.bak`.
    let dir = tempfile::tempdir().expect("tempdir");
    let path: PathBuf = dir.path().join("config.yaml");
    let original = "spec:\n  replicas: 3\n";
    std::fs::write(&path, original).expect("seed file");

    let path_str = path.to_str().unwrap();
    dq().args([
        "set",
        path_str,
        "/spec/replicas",
        "5",
        "-i",
        "--backup",
        "--no-color",
    ])
    .assert()
    .success();

    // Target file updated.
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("replicas: 5"),
        "in-place mutation must apply, got:\n{contents}",
    );

    // Backup contains the original bytes.
    let bak_path = dir.path().join("config.yaml.bak");
    let bak_contents = std::fs::read_to_string(&bak_path)
        .unwrap_or_else(|e| panic!("backup file missing at {}: {e}", bak_path.display()));
    assert_eq!(bak_contents, original, "backup must contain original bytes",);

    // Exactly two files in the dir: the target and the .bak. No leftover
    // tempfiles from atomic_write.
    let files = list_files(dir.path());
    assert_eq!(
        files,
        vec!["config.yaml".to_owned(), "config.yaml.bak".to_owned()],
        "tempdir must contain only target + backup, found: {files:?}",
    );
}

// Reference an unused std::process::Command so the import does not become
// dead-code if the assert_cmd shape ever changes.
#[allow(dead_code)]
fn _std_command_unused() -> StdCommand {
    StdCommand::new("true")
}

/// Modes and links, through every command that writes. No test depends on
/// the umask of the run: modes are set with `chmod`, and the tests about the
/// umask set one in a shell of their own.
#[cfg(unix)]
mod access {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::path::Path;

    use super::*;

    const MODES: [u32; 6] = [0o600, 0o644, 0o664, 0o755, 0o444, 0o400];
    const BEFORE: &str = "a:   1\nb: 1\n";

    const RULE: &str = r#"
id: test.a-is-two
description: /a is 2
severity: warn
match:
  format: yaml
check:
  jq: 'select(.a != 2)'
  message: "a must be 2"
fix:
  ops: 'if .a != 2 then [{"op":"replace","path":"/a","value":2}] else [] end'
"#;

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).expect("lstat").mode() & 0o7777
    }

    fn seed(path: &Path, content: &str, mode: u32) {
        // A read-only file of the round before cannot be written over.
        let _ = fs::remove_file(path);
        fs::write(path, content).expect("seed");
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).expect("read")
    }

    fn link_target(path: &Path) -> String {
        fs::read_link(path)
            .expect("still a link")
            .to_str()
            .expect("utf-8")
            .to_owned()
    }

    /// Each command that edits a file in place, as `(name, arguments)`.
    /// The file is `FILE`; a rule file is at `rule`.
    fn edits(rule: &Path) -> Vec<(&'static str, Vec<String>)> {
        let rule = rule.to_str().expect("utf-8");
        [
            ("set", vec!["set", "FILE", "/a", "2", "-i"]),
            ("set --jq", vec!["set", "FILE", "--jq", ".a = 2", "-i"]),
            ("del", vec!["del", "FILE", "/b", "-i"]),
            (
                "patch",
                vec![
                    "patch",
                    "FILE",
                    r#"[{"op":"replace","path":"/a","value":2}]"#,
                    "-i",
                ],
            ),
            ("merge", vec!["merge", "FILE", r#"{"a":2}"#, "-i"]),
            ("fmt", vec!["fmt", "FILE", "-i"]),
            ("fix", vec!["fix", "FILE", "--rules", rule, "-i"]),
        ]
        .into_iter()
        .map(|(name, args)| (name, args.into_iter().map(str::to_owned).collect()))
        .collect()
    }

    /// Run one of [`edits`] on `file`, in `dir`.
    fn edit(dir: &Path, args: &[String], file: &str) -> assert_cmd::assert::Assert {
        let args = args
            .iter()
            .map(|arg| if arg == "FILE" { file } else { arg.as_str() });
        dq().current_dir(dir).args(args).assert()
    }

    /// A directory, and a rule file outside of it for `fix`.
    fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("work");
        fs::create_dir(&dir).expect("mkdir");
        let rule = root.path().join("rule.yml");
        fs::write(&rule, RULE).expect("rule");
        (root, dir, rule)
    }

    #[test]
    fn every_command_keeps_the_mode_of_the_file() {
        let (_root, dir, rule) = sandbox();
        let file = dir.join("f.yaml");
        for (name, args) in edits(&rule) {
            for bits in MODES {
                seed(&file, BEFORE, bits);

                edit(&dir, &args, "f.yaml").success();

                assert_ne!(read(&file), BEFORE, "{name} must change the file");
                assert_eq!(mode(&file), bits, "{name} {bits:o}");
                assert_eq!(list_files(&dir), ["f.yaml"], "{name} {bits:o}");
            }
        }
    }

    #[test]
    fn every_command_edits_the_file_a_link_points_to() {
        let (_root, dir, rule) = sandbox();
        let other = dir.join("other");
        fs::create_dir(&other).expect("mkdir");
        let real = other.join("real.yaml");
        let absolute = real.to_str().expect("utf-8").to_owned();
        let links = [
            ("relative.yaml", "other/real.yaml"),
            ("absolute.yaml", absolute.as_str()),
            ("chain.yaml", "chain-1"),
            ("chain-1", "./chain-2"),
            ("chain-2", absolute.as_str()),
        ];
        for (name, to) in links {
            symlink(to, dir.join(name)).expect("symlink");
        }
        let before = list_files(&dir);
        for (name, args) in edits(&rule) {
            for link in ["relative.yaml", "absolute.yaml", "chain.yaml"] {
                seed(&real, BEFORE, 0o755);

                edit(&dir, &args, link).success();

                assert_ne!(read(&real), BEFORE, "{name} {link}");
                assert_eq!(mode(&real), 0o755, "{name} {link}");
                for (link, to) in links {
                    assert_eq!(link_target(&dir.join(link)), to, "{name}");
                }
                assert_eq!(list_files(&dir), before, "{name} {link}");
                assert_eq!(list_files(&other), ["real.yaml"], "{name} {link}");
            }
        }
    }

    #[test]
    fn a_link_to_no_file_is_a_read_error_and_stays() {
        let (_root, dir, rule) = sandbox();
        fs::create_dir(dir.join("sub")).expect("mkdir");
        let links = [
            ("dangling.yaml", "nowhere.yaml"),
            ("loop.yaml", "loop-1.yaml"),
            ("loop-1.yaml", "loop.yaml"),
            ("dir.yaml", "sub"),
        ];
        for (name, to) in links {
            symlink(to, dir.join(name)).expect("symlink");
        }
        let before = list_files(&dir);
        for (name, args) in edits(&rule) {
            for link in ["dangling.yaml", "loop.yaml", "dir.yaml"] {
                edit(&dir, &args, link)
                    .code(5)
                    .stderr(predicates::str::contains(link));
            }
            for (link, to) in links {
                assert_eq!(link_target(&dir.join(link)), to, "{name}");
            }
            assert_eq!(list_files(&dir), before, "{name}");
        }
    }

    #[test]
    fn the_umask_neither_takes_from_an_edited_file_nor_gives_to_it() {
        let (_root, dir, _rule) = sandbox();
        for umask in ["077", "022", "000"] {
            for bits in MODES {
                let file = dir.join("f.yaml");
                seed(&file, BEFORE, bits);
                seed(&dir.join("c.yaml"), BEFORE, bits);
                let _ = fs::remove_file(dir.join("c.json"));

                let script = format!(
                    r#"umask {umask}; "$0" set f.yaml /a 2 -i --backup && "$0" convert c.yaml -F json -i"#
                );
                Command::new("/bin/sh")
                    .current_dir(&dir)
                    .env("HOME", &dir)
                    .args(["-c", &script])
                    .arg(assert_cmd::cargo::cargo_bin("dq"))
                    .assert()
                    .success();

                for name in ["f.yaml", "f.yaml.bak", "c.json"] {
                    assert_eq!(mode(&dir.join(name)), bits, "{umask} {name}");
                }
                assert_eq!(list_files(&dir), ["c.json", "f.yaml", "f.yaml.bak"]);
            }
        }
    }

    #[test]
    fn a_backup_is_next_to_the_file_a_link_points_to() {
        let (_root, dir, rule) = sandbox();
        let other = dir.join("other");
        fs::create_dir(&other).expect("mkdir");
        symlink("other/real.yaml", dir.join("link.yaml")).expect("symlink");
        for (name, mut args) in edits(&rule) {
            args.push("--backup".to_owned());
            for bits in MODES {
                seed(&other.join("real.yaml"), BEFORE, bits);

                // The second run replaces a `.bak` that may be read-only.
                edit(&dir, &args, "link.yaml").success();

                let bak = other.join("real.yaml.bak");
                assert_eq!(read(&bak), BEFORE, "{name} {bits:o}");
                assert_eq!(mode(&bak), bits, "{name}");
                assert_ne!(read(&other.join("real.yaml")), BEFORE, "{name}");
                assert_eq!(mode(&other.join("real.yaml")), bits, "{name}");
                assert_eq!(list_files(&dir), ["link.yaml", "other"], "{name}");
                assert_eq!(list_files(&other), ["real.yaml", "real.yaml.bak"], "{name}");
            }
        }
    }

    #[test]
    fn a_link_where_the_backup_goes_is_refused_before_anything_is_written() {
        let (_root, dir, rule) = sandbox();
        seed(&dir.join("f.yaml"), BEFORE, 0o644);
        seed(&dir.join("victim"), "victim\n", 0o644);
        symlink("victim", dir.join("f.yaml.bak")).expect("symlink");
        for (name, mut args) in edits(&rule) {
            args.push("--backup".to_owned());

            edit(&dir, &args, "f.yaml")
                .code(7)
                .stderr(predicates::str::contains(
                    "I/O error writing 'f.yaml': cannot back up to 'f.yaml.bak': \
                 something other than a regular file is there",
                ));

            assert_eq!(read(&dir.join("f.yaml")), BEFORE, "{name}");
            assert_eq!(read(&dir.join("victim")), "victim\n", "{name}");
            assert_eq!(link_target(&dir.join("f.yaml.bak")), "victim", "{name}");
            assert_eq!(list_files(&dir), ["f.yaml", "f.yaml.bak", "victim"]);
        }
    }

    #[test]
    fn a_write_that_fails_behind_a_link_names_the_link_and_leaves_nothing() {
        let (_root, dir, _rule) = sandbox();
        let other = dir.join("other");
        fs::create_dir(&other).expect("mkdir");
        seed(&other.join("real.yaml"), BEFORE, 0o644);
        symlink("other/real.yaml", dir.join("link.yaml")).expect("symlink");
        fs::set_permissions(&other, fs::Permissions::from_mode(0o500)).expect("chmod");
        if fs::write(other.join("probe"), "").is_ok() {
            return; // root writes wherever it wants
        }

        let assert = dq()
            .current_dir(&dir)
            .args(["set", "link.yaml", "/a", "2", "-i"])
            .assert();

        fs::set_permissions(&other, fs::Permissions::from_mode(0o700)).expect("chmod");
        assert
            .code(7)
            .stderr(predicates::str::contains("I/O error writing 'link.yaml': "));
        assert_eq!(read(&other.join("real.yaml")), BEFORE);
        assert_eq!(list_files(&dir), ["link.yaml", "other"]);
        assert_eq!(list_files(&other), ["real.yaml"]);
    }

    #[test]
    fn a_link_whose_file_would_not_change_is_left_alone() {
        let (_root, dir, _rule) = sandbox();
        let file = dir.join("f.yaml");
        seed(&file, BEFORE, 0o644);
        symlink("f.yaml", dir.join("link.yaml")).expect("symlink");
        let before = fs::metadata(&file).expect("stat");
        for args in [
            &["set", "link.yaml", "/a", "1", "-i", "--backup"][..],
            &["set", "link.yaml", "/a", "2"],
            &["set", "link.yaml", "/a", "2", "--diff"],
        ] {
            dq().current_dir(&dir).args(args).assert().success();
        }
        dq().current_dir(&dir)
            .args(["set", "link.yaml", "/a", "2", "--check"])
            .assert()
            .code(1);

        let after = fs::metadata(&file).expect("stat");
        assert_eq!(
            (after.ino(), after.mtime_nsec()),
            (before.ino(), before.mtime_nsec())
        );
        assert_eq!(list_files(&dir), ["f.yaml", "link.yaml"]);
    }

    #[test]
    fn a_glob_keeps_modes_and_passes_links_by() {
        let (_root, dir, rule) = sandbox();
        let sub = dir.join("sub");
        fs::create_dir(&sub).expect("mkdir");
        for (name, mut args) in edits(&rule) {
            for parallel in ["1", "4"] {
                let files = [
                    ("a.yaml", 0o755),
                    ("b.yaml", 0o600),
                    ("c.yaml", 0o444),
                    ("d.yaml", 0o664),
                ];
                for (file, bits) in files {
                    seed(&sub.join(file), BEFORE, bits);
                }
                seed(&dir.join("outside.yaml"), BEFORE, 0o644);
                let links = [
                    ("in.yaml", "a.yaml"),
                    ("out.yaml", "../outside.yaml"),
                    ("void.yaml", "nowhere"),
                ];
                for (link, to) in links {
                    let _ = fs::remove_file(sub.join(link));
                    symlink(to, sub.join(link)).expect("symlink");
                }
                args.push(format!("--parallel={parallel}"));

                edit(&dir, &args, "sub/*.yaml")
                    .success()
                    .stdout("Modified: 4, Skipped: 0, Failed: 0\n");

                args.pop();
                for (file, bits) in files {
                    assert_ne!(read(&sub.join(file)), BEFORE, "{name} {file}");
                    assert_eq!(mode(&sub.join(file)), bits, "{name} {file}");
                }
                for (link, to) in links {
                    assert_eq!(link_target(&sub.join(link)), to, "{name}");
                }
                assert_eq!(read(&dir.join("outside.yaml")), BEFORE, "{name}");
                assert_eq!(list_files(&sub).len(), files.len() + links.len());
            }
        }
    }

    #[test]
    fn a_file_and_links_to_it_in_one_run_are_edited_once() {
        let (_root, dir, rule) = sandbox();
        let rule = rule.to_str().expect("utf-8");
        symlink("f.yaml", dir.join("link.yaml")).expect("symlink");
        symlink("link.yaml", dir.join("chain.yaml")).expect("symlink");
        for parallel in ["1", "4"] {
            for files in [
                ["f.yaml", "link.yaml", "chain.yaml"],
                ["chain.yaml", "link.yaml", "f.yaml"],
            ] {
                seed(&dir.join("f.yaml"), BEFORE, 0o755);

                dq().current_dir(&dir)
                    .arg("fix")
                    .args(files)
                    .args(["--rules", rule, "-i", "--backup", "--parallel", parallel])
                    .assert()
                    .success()
                    .stdout("Modified: 1, Skipped: 2, Failed: 0\n");

                assert_eq!(read(&dir.join("f.yaml")), "a:   2\nb: 1\n");
                assert_eq!(mode(&dir.join("f.yaml")), 0o755);
                assert_eq!(read(&dir.join("f.yaml.bak")), BEFORE);
                assert_eq!(link_target(&dir.join("link.yaml")), "f.yaml");
                assert_eq!(link_target(&dir.join("chain.yaml")), "link.yaml");
                assert_eq!(
                    list_files(&dir),
                    ["chain.yaml", "f.yaml", "f.yaml.bak", "link.yaml"]
                );
            }
        }
    }

    /// `dq convert FILE -F json -i` in `dir`.
    fn convert(dir: &Path, file: &str, more: &[&str]) -> assert_cmd::assert::Assert {
        dq().current_dir(dir)
            .args(["convert", file, "-F", "json", "-i"])
            .args(more)
            .assert()
    }

    #[test]
    fn convert_gives_the_new_file_the_mode_of_the_source() {
        let (_root, dir, _rule) = sandbox();
        for bits in MODES {
            seed(&dir.join("f.yaml"), BEFORE, bits);

            convert(&dir, "f.yaml", &[]).success();

            assert_eq!(mode(&dir.join("f.json")), bits);
            assert_eq!(list_files(&dir), ["f.json"]);
            fs::remove_file(dir.join("f.json")).expect("rm");
        }
    }

    #[test]
    fn convert_over_a_file_keeps_the_mode_of_that_file() {
        let (_root, dir, _rule) = sandbox();
        seed(&dir.join("f.yaml"), BEFORE, 0o755);
        seed(&dir.join("f.json"), "{}\n", 0o640);

        convert(&dir, "f.yaml", &["--backup", "--keep-source"]).success();

        assert_eq!(mode(&dir.join("f.json")), 0o640);
        assert_eq!(mode(&dir.join("f.json.bak")), 0o640);
        assert_eq!(read(&dir.join("f.json.bak")), "{}\n");
        assert_eq!(mode(&dir.join("f.yaml")), 0o755);
    }

    #[test]
    fn convert_of_a_link_writes_next_to_the_link_and_removes_the_link() {
        let (_root, dir, _rule) = sandbox();
        let other = dir.join("other");
        fs::create_dir(&other).expect("mkdir");
        seed(&other.join("real.yaml"), BEFORE, 0o754);
        symlink("other/real.yaml", dir.join("link.yaml")).expect("symlink");

        convert(&dir, "link.yaml", &[]).success();

        assert_eq!(list_files(&dir), ["link.json", "other"]);
        assert_eq!(mode(&dir.join("link.json")), 0o754);
        assert_eq!(read(&other.join("real.yaml")), BEFORE);
        assert_eq!(mode(&other.join("real.yaml")), 0o754);
    }

    #[test]
    fn convert_refuses_a_link_at_any_target_before_it_writes_anything() {
        let (_root, dir, _rule) = sandbox();
        for name in ["a.yaml", "m.yaml", "z.yaml"] {
            seed(&dir.join(name), BEFORE, 0o644);
        }
        seed(&dir.join("victim.json"), "{}\n", 0o644);
        for to in ["victim.json", "nowhere.json"] {
            symlink(to, dir.join("m.json")).expect("symlink");
            let before = list_files(&dir);

            convert(&dir, "*.yaml", &[])
                .code(7)
                .stdout("")
                .stderr(predicates::str::contains(
                    "I/O error writing 'm.json': it is a symbolic link",
                ));

            assert_eq!(list_files(&dir), before);
            assert_eq!(read(&dir.join("victim.json")), "{}\n");
            fs::remove_file(dir.join("m.json")).expect("unlink");
        }
    }

    #[test]
    fn convert_refuses_a_backup_over_a_source_before_it_writes_anything() {
        let (_root, dir, _rule) = sandbox();
        // `x.yaml.bak` would be replaced before it is read.
        for (name, content) in [
            ("0.json", "{\"v\": 0}\n"),
            ("x.json", "{\"v\": 1}\n"),
            ("x.yaml", "v: 2\n"),
            ("x.yaml.bak", "{\"v\": 3}\n"),
        ] {
            seed(&dir.join(name), content, 0o644);
        }
        let before = contents(&dir);
        let run = |more: &[&str]| {
            dq().current_dir(&dir)
                .args(["convert", "*.{json,bak}", "-F", "yaml", "-i"])
                .args(["--input-format", "json"])
                .args(more)
                .assert()
        };

        run(&["--backup"])
            .code(6)
            .stdout("")
            .stderr(predicates::str::contains(
                "convert -i to yaml: the backup of 'x.yaml' would replace 'x.yaml.bak', which is \
                 to be converted too; rename one of them first. Nothing was converted",
            ));
        assert_eq!(contents(&dir), before);

        // Without backups nothing is lost.
        run(&[]).success();
        assert_eq!(list_files(&dir), ["0.yaml", "x.yaml", "x.yaml.yaml"]);
        assert_eq!(read(&dir.join("x.yaml.yaml")), "v: 3\n");
    }

    #[test]
    fn convert_says_that_a_target_is_a_hard_link_of_its_source() {
        let (_root, dir, _rule) = sandbox();
        seed(&dir.join("a.yaml"), BEFORE, 0o644);
        fs::hard_link(dir.join("a.yaml"), dir.join("a.json")).expect("link");

        convert(&dir, "a.yaml", &[])
            .code(6)
            .stderr(predicates::str::contains(
                "'a.yaml' and its target 'a.json' are two names of one file (a hard link)",
            ));

        assert_eq!(list_files(&dir), ["a.json", "a.yaml"]);
    }

    /// See `a_group_that_cannot_be_kept_costs_the_group_its_access` of
    /// `dq-core`: the group of `/tmp` is one the user is not in.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_owner_or_group_that_cannot_be_kept_is_a_warning() {
        let (_root, dir, rule) = sandbox();
        let shared = tempfile::tempdir_in("/private/tmp").expect("tempdir");
        let made = shared.path().join("f.yaml");
        seed(&made, BEFORE, 0o640);
        let foreign = fs::metadata(&made).expect("stat").gid();
        let probe = dir.join("probe");
        fs::write(&probe, "").expect("probe");
        if std::os::unix::fs::chown(&probe, None, Some(foreign)).is_ok() {
            return; // root, or a member of it
        }
        fs::remove_file(&probe).expect("rm");
        for (name, mut args) in edits(&rule) {
            seed(&made, BEFORE, 0o640);
            fs::rename(&made, dir.join("f.yaml")).expect("same volume");

            args.push("--backup".to_owned());
            edit(&dir, &args, "f.yaml")
                .success()
                .stdout("")
                .stderr(predicates::str::contains(
                    "WARN f.yaml: its owner or group could not be kept",
                ));

            assert_eq!(mode(&dir.join("f.yaml")), 0o600, "{name}");
            assert_eq!(mode(&dir.join("f.yaml.bak")), 0o600, "{name}");
        }
        // Nothing to say where everything is kept.
        seed(&dir.join("g.yaml"), BEFORE, 0o640);
        dq().current_dir(&dir)
            .args(["set", "g.yaml", "/a", "2", "-i"])
            .assert()
            .success()
            .stderr("");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_acl_is_kept_and_one_is_not_inherited() {
        let (_root, dir, rule) = sandbox();
        let acl = |name: &str| {
            let listed = StdCommand::new("/bin/ls")
                .args(["-le", name])
                .current_dir(&dir)
                .output()
                .expect("ls");
            let listed = String::from_utf8(listed.stdout).expect("utf-8");
            listed
                .lines()
                .skip(1)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let chmod = |args: &[&str]| {
            let done = StdCommand::new("/bin/chmod")
                .args(args)
                .current_dir(&dir)
                .status();
            assert!(done.expect("chmod").success());
        };
        for (name, mut args) in edits(&rule) {
            args.push("--backup".to_owned());
            for denied in [true, false] {
                seed(&dir.join("f.yaml"), BEFORE, 0o600);
                seed(&dir.join("c.yaml"), BEFORE, 0o600);
                let _ = fs::remove_file(dir.join("c.json"));
                let expected = if denied {
                    chmod(&["+a", "nobody deny read", "f.yaml", "c.yaml"]);
                    vec![" 0: user:nobody deny read".to_owned()]
                } else {
                    Vec::new()
                };
                chmod(&["+a", "nobody allow read,file_inherit", "."]);

                edit(&dir, &args, "f.yaml").success();
                convert(&dir, "c.yaml", &[]).success();

                chmod(&["-N", "."]);
                for file in ["f.yaml", "f.yaml.bak", "c.json"] {
                    assert_eq!(acl(file), expected, "{name} {file}");
                }
            }
        }
    }

    /// The audit line of `fix`, at `-v`.
    #[test]
    fn a_fix_is_logged_once_for_each_name() {
        let (_root, dir, rule) = sandbox();
        seed(&dir.join("f.yaml"), BEFORE, 0o644);
        symlink("f.yaml", dir.join("link.yaml")).expect("symlink");

        let assert = dq()
            .current_dir(&dir)
            .args(["-v", "fix", "f.yaml", "link.yaml", "-i", "--rules"])
            .arg(&rule)
            .assert()
            .success();

        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("utf-8");
        for name in ["path=f.yaml", "path=link.yaml"] {
            let lines = stderr
                .lines()
                .filter(|line| line.contains("dq fix") && line.contains(name));
            assert_eq!(lines.count(), 1, "{name} in {stderr}");
        }
    }

    #[test]
    fn convert_refuses_a_link_where_the_new_file_goes() {
        let (_root, dir, _rule) = sandbox();
        seed(&dir.join("f.yaml"), BEFORE, 0o644);
        seed(&dir.join("victim.json"), "{}\n", 0o644);
        // The last one would be read and then removed as the source.
        for to in ["victim.json", "nowhere.json", "f.json", "f.yaml"] {
            symlink(to, dir.join("f.json")).expect("symlink");
            for more in [&[][..], &["--backup"], &["--keep-source"]] {
                convert(&dir, "f.yaml", more)
                    .code(7)
                    .stderr(predicates::str::contains(
                        "I/O error writing 'f.json': it is a symbolic link",
                    ));

                assert_eq!(link_target(&dir.join("f.json")), to);
                assert_eq!(read(&dir.join("f.yaml")), BEFORE);
                assert_eq!(read(&dir.join("victim.json")), "{}\n");
                assert_eq!(list_files(&dir), ["f.json", "f.yaml", "victim.json"]);
            }
            fs::remove_file(dir.join("f.json")).expect("unlink");
        }
    }

    #[test]
    fn convert_never_removes_the_file_it_has_just_written() {
        let (_root, dir, _rule) = sandbox();
        seed(&dir.join("f.YAML"), BEFORE, 0o644);
        let one_name = dir.join("f.yaml").exists();

        let assert = dq()
            .current_dir(&dir)
            .args(["convert", "f.YAML", "-F", "yaml", "-i"])
            .assert();

        if one_name {
            // A file system that ignores case, as on macOS.
            assert.code(6);
            assert_eq!(read(&dir.join("f.YAML")), BEFORE);
            assert_eq!(list_files(&dir), ["f.YAML"]);
        } else {
            assert.success();
            assert_eq!(list_files(&dir), ["f.yaml"]);
        }
    }

    #[test]
    fn convert_of_a_glob_keeps_each_mode() {
        let (_root, dir, _rule) = sandbox();
        for bits in MODES {
            seed(&dir.join(format!("{bits:o}.yaml")), BEFORE, bits);
        }

        convert(&dir, "*.yaml", &[])
            .success()
            .stdout("Modified: 6, Skipped: 0, Failed: 0\n");

        for bits in MODES {
            assert_eq!(mode(&dir.join(format!("{bits:o}.json"))), bits);
        }
        assert_eq!(list_files(&dir).len(), MODES.len());
    }

    /// Every file of `dir` with its content, to see that nothing changed.
    fn contents(dir: &Path) -> Vec<(String, String)> {
        list_files(dir)
            .into_iter()
            .map(|name| (name.clone(), read(&dir.join(name))))
            .collect()
    }

    #[test]
    fn convert_refuses_two_sources_of_one_target_before_it_writes_anything() {
        let (_root, dir, _rule) = sandbox();
        // Earlier and later files of the run have no conflict.
        for (name, content) in [
            ("0.yaml", "n: 0\n"),
            ("a.yaml", "n: 1\n"),
            ("a.yml", "n: 2\n"),
            ("z.yml", "n: 3\n"),
        ] {
            seed(&dir.join(name), content, 0o644);
        }
        for there in [false, true] {
            if there {
                seed(&dir.join("a.json"), "{\"old\": 1}\n", 0o644);
            }
            let before = contents(&dir);
            for more in [&[][..], &["--keep-source"], &["--backup"]] {
                convert(&dir, "*.{yaml,yml}", more)
                    .code(6)
                    .stdout("")
                    .stderr(predicates::str::contains(
                        "convert -i to json: 'a.yaml' and 'a.yml' would both be written to \
                         'a.json'; rename one of them first. Nothing was converted",
                    ));
                assert_eq!(contents(&dir), before, "{more:?}");
            }
        }

        // Without the conflict the same run goes through.
        fs::rename(dir.join("a.yml"), dir.join("b.yml")).expect("mv");
        convert(&dir, "*.{yaml,yml}", &[])
            .success()
            .stdout("Modified: 4, Skipped: 0, Failed: 0\n");
        assert_eq!(list_files(&dir), ["0.json", "a.json", "b.json", "z.json"]);
    }

    #[test]
    fn convert_compares_targets_as_the_file_system_does() {
        let (_root, dir, _rule) = sandbox();
        seed(&dir.join("A.yaml"), "n: 1\n", 0o644);
        seed(&dir.join("a.yml"), "n: 2\n", 0o644);
        let one_name = dir.join("a.yaml").exists();
        let before = contents(&dir);

        let assert = convert(&dir, "*.{yaml,yml}", &[]);

        if one_name {
            assert.code(6).stderr(predicates::str::contains(
                "'A.yaml' and 'a.yml' would both be written to 'A.json' \
                 (the same file as 'a.json' here)",
            ));
            assert_eq!(contents(&dir), before);
        } else {
            assert.success();
            assert_eq!(list_files(&dir), ["A.json", "a.json"]);
        }
    }

    #[test]
    fn convert_tells_files_of_several_directories_apart() {
        let (_root, dir, _rule) = sandbox();
        for sub in ["one", "two"] {
            fs::create_dir(dir.join(sub)).expect("mkdir");
        }
        seed(&dir.join("one/a.yaml"), "n: 1\n", 0o644);
        seed(&dir.join("two/a.yml"), "n: 2\n", 0o644);

        convert(&dir, "*/*.{yaml,yml}", &[]).success();

        assert_eq!(list_files(&dir.join("one")), ["a.json"]);
        assert_eq!(list_files(&dir.join("two")), ["a.json"]);
    }

    #[test]
    fn convert_refuses_a_target_that_is_a_source_before_it_writes_anything() {
        let (_root, dir, _rule) = sandbox();
        seed(&dir.join("0.yaml"), "n: 0\n", 0o644);
        seed(&dir.join("a.yaml"), "n: 1\n", 0o644);
        seed(&dir.join("a.json"), "{\"n\": 2}\n", 0o644);
        let before = contents(&dir);
        for (glob, format, source) in [
            ("*.{yaml,json}", "json", "a.json"),
            // `0.yaml` is the first that is its own target.
            ("*.{yaml,json}", "yaml", "0.yaml"),
            ("a.{yaml,json}", "yaml", "a.yaml"),
        ] {
            dq().current_dir(&dir)
                .args(["convert", glob, "-F", format, "-i"])
                .assert()
                .code(6)
                .stderr(predicates::str::contains(format!(
                    "target path '{source}' is identical to source"
                )));
            assert_eq!(contents(&dir), before, "{glob} {format}");
        }
    }

    /// Today's behaviour, kept: an existing file that is no source of the
    /// run is replaced without a word, `--backup` keeps a copy.
    #[test]
    fn convert_replaces_an_unrelated_file_at_the_target() {
        let (_root, dir, _rule) = sandbox();
        seed(&dir.join("a.yaml"), "n: 1\n", 0o644);
        seed(&dir.join("a.json"), "{\"old\": 1}\n", 0o644);

        convert(&dir, "a.yaml", &[]).success().stdout("");

        assert_eq!(
            contents(&dir),
            [("a.json".to_owned(), "{\n  \"n\": 1\n}\n".to_owned())]
        );
    }
}
