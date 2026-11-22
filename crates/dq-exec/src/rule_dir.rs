//! What a rule directory holds: the walk behind `--rules DIR`,
//! `./.dq/rules/`, `dq test DIR` and `dq rules add DIR`, and which of its
//! files are rules and which are fixtures.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{ExecError, Result};

/// Extensions of a rule file, and of a fixture after `.test`; ASCII case
/// does not matter.
const RULE_FILE_EXTENSIONS: [&str; 2] = ["yml", "yaml"];

/// `ELOOP`, or Windows' `ERROR_CANT_RESOLVE_FILENAME`: what following a link
/// that leads back to itself gives, which `std::io::ErrorKind` has no stable
/// name for. `None` where dq does not know the number: the link is then one
/// that cannot be read, which is never passed over.
const LINK_LOOP: Option<i32> = if cfg!(windows) {
    Some(1921)
} else if cfg!(any(target_os = "linux", target_os = "android")) {
    Some(40)
} else if cfg!(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)) {
    Some(62)
} else {
    None
};

/// Which links that lead nowhere — to nothing, or back to themselves — stop
/// [`files_under`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DanglingLinks {
    /// Any: in a directory of rules it may have been a directory of them,
    /// such as a submodule that is not checked out.
    Refuse,
    /// Those named as a rule file or a fixture. The directory is any part of
    /// a repository, where a link may have nothing to do with rules.
    RefuseRules,
}

/// Every file under `dir`, at any depth, symbolic links to files and
/// directories followed — `dq rules add --symlink` makes them. In the order
/// of the names along the path, so it does not change between runs.
///
/// A directory is walked once, under the first path that reaches it, so a
/// link to one the walk has been to (or is in) adds nothing and cannot loop.
/// A file that is linked to comes once per link, under the link's path.
///
/// An editor's lock (`.#a.yml`, a link to no path) is never a rule.
///
/// # Errors
///
/// [`ExecError::Io`] naming a link that leads nowhere, as `dangling` says,
/// what cannot be read, or a FIFO, socket or device named as a rule file or a
/// fixture: skipping it would leave out rules without a word.
pub fn files_under(dir: &Utf8Path, dangling: DanglingLinks) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut walked: HashSet<PathBuf> = HashSet::new();
    let mut entries = walkdir::WalkDir::new(dir.as_std_path())
        .follow_links(true)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            // A link to a directory the walk is in: already being walked.
            Err(err) if err.loop_ancestor().is_some() => continue,
            Err(err) if is_ignored_link(&err, dangling) => continue,
            Err(err) => return Err(walk_error(dir, err)),
        };
        if entry.file_type().is_dir() {
            if !walked.insert(canonical(entry.path())?) {
                entries.skip_current_dir();
            }
        } else if entry.file_type().is_file() {
            files.push(entry.into_path());
        } else if has_rule_extension(Utf8Path::new(&entry.path().to_string_lossy())) {
            let source = std::io::Error::other("not a regular file");
            return Err(io_error(entry.path(), source));
        }
    }
    Ok(files)
}

/// The directory of the project's own rules, `.dq/rules` under `cwd`, or
/// `None` when there is none: no `.dq`, or no `rules` in it.
///
/// # Errors
///
/// - [`ExecError::Io`] naming `.dq` or `.dq/rules` when it cannot be
///   resolved or read: a link to nothing or to itself, a `.dq` without
///   permission.
/// - [`ExecError::NotADirectory`] when something else is there — a file, also
///   the one a checkout without symbolic links leaves for a link.
pub fn project_dir(cwd: &Utf8Path) -> Result<Option<Utf8PathBuf>> {
    for name in [".dq", ".dq/rules"] {
        let path = cwd.join(name);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(ExecError::NotADirectory {
                    path: Utf8Path::new(".").join(name),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && !path.is_symlink() => {
                return Ok(None);
            }
            Err(err) => return Err(io_error(path.as_std_path(), err)),
        }
    }
    Ok(Some(cwd.join(".dq").join("rules")))
}

/// Whether `err` is a link that leads nowhere and that `dangling` lets pass.
/// Never the directory of the walk itself.
fn is_ignored_link(err: &walkdir::Error, dangling: DanglingLinks) -> bool {
    let Some(path) = err.path().filter(|_| err.depth() > 0) else {
        return false;
    };
    let Some(path) = Utf8Path::from_path(path) else {
        return false;
    };
    let is_lock = path.file_name().is_some_and(|name| name.starts_with(".#"));
    leads_nowhere(path)
        && (is_lock || (dangling == DanglingLinks::RefuseRules && !has_rule_extension(path)))
}

/// The path `path` really has, every link on it followed.
///
/// # Errors
///
/// [`ExecError::Io`] when it cannot be resolved.
pub(crate) fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(|source| io_error(path, source))
}

/// Whether `path` is a link to nothing or back to itself.
fn leads_nowhere(path: &Utf8Path) -> bool {
    path.is_symlink()
        && std::fs::metadata(path).is_err_and(|err| {
            err.kind() == std::io::ErrorKind::NotFound
                || (LINK_LOOP.is_some() && err.raw_os_error() == LINK_LOOP)
        })
}

/// Whether `path` names a rule file: `*.yml` or `*.yaml`, in any case, that
/// is no `*.test.yml` / `*.test.yaml` fixture.
#[must_use]
pub fn is_rule_file(path: &Utf8Path) -> bool {
    has_rule_extension(path) && !is_fixture_file(path)
}

/// Whether `path` names a `dq test` fixture: `*.test.yml` or `*.test.yaml`,
/// in any case.
#[must_use]
pub fn is_fixture_file(path: &Utf8Path) -> bool {
    parent_rule_path(path).is_some()
}

/// The rule file a fixture tests, beside it: `foo.test.yml` is the fixture
/// of `foo.yml`, `foo.test.YAML` of `foo.YAML`. `None` for a path that is no
/// fixture.
pub(crate) fn parent_rule_path(fixture: &Utf8Path) -> Option<Utf8PathBuf> {
    let name = fixture.file_name()?;
    // Lowercasing ASCII keeps every byte offset.
    let lower = name.to_ascii_lowercase();
    RULE_FILE_EXTENSIONS.iter().find_map(|extension| {
        let stem = lower.strip_suffix(&format!(".test.{extension}"))?.len();
        let dot = name.len() - extension.len() - 1;
        Some(fixture.with_file_name(format!("{}{}", &name[..stem], &name[dot..])))
    })
}

fn has_rule_extension(path: &Utf8Path) -> bool {
    path.extension().is_some_and(|extension| {
        RULE_FILE_EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// The error of a walk over `dir`, on the path it was raised for.
fn walk_error(dir: &Utf8Path, err: walkdir::Error) -> ExecError {
    let path = err
        .path()
        .map_or_else(|| dir.as_std_path().to_path_buf(), Path::to_path_buf);
    let text = err.to_string();
    let source = err
        .into_io_error()
        .unwrap_or_else(|| std::io::Error::other(text));
    io_error(&path, source)
}

/// [`ExecError::Io`] on `path`, which says so when `path` is a link whose
/// target is missing: the path itself is there. The target is put on one line
/// ([`dq_core::one_line`]); whoever made the link chose its text.
fn io_error(path: &Path, source: std::io::Error) -> ExecError {
    let target = match source.kind() {
        std::io::ErrorKind::NotFound => std::fs::read_link(path).ok(),
        _ => None,
    };
    let source = match target {
        Some(target) => std::io::Error::new(
            source.kind(),
            format!(
                "a link to '{}', where nothing is",
                dq_core::one_line(&target.to_string_lossy())
            ),
        ),
        None => source,
    };
    ExecError::Io {
        path: Utf8PathBuf::from(path.to_string_lossy().into_owned()),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_and_fixture_files_are_told_by_name_in_any_case() {
        let rules = [
            "a.yml", "d/a.yaml", "a.YML", "a.Yaml", "a.b.yml", "test.yml",
        ];
        let fixtures = ["a.test.yml", "d/a.test.yaml", "a.TEST.YML", "a.test.Yaml"];
        for name in rules.map(Utf8Path::new) {
            assert!(is_rule_file(name) && !is_fixture_file(name), "{name}");
        }
        for name in fixtures.map(Utf8Path::new) {
            assert!(is_fixture_file(name) && !is_rule_file(name), "{name}");
        }
        for name in ["a.json", "yml", "a.test", "a.yml.bak", ".yml"].map(Utf8Path::new) {
            assert!(!is_rule_file(name) && !is_fixture_file(name), "{name}");
        }
        for (fixture, rule) in [
            ("d/a.test.yml", "d/a.yml"),
            ("a.b.test.YAML", "a.b.YAML"),
            ("a.TEST.yml", "a.yml"),
            (".test.yml", ".yml"),
        ] {
            let parent = parent_rule_path(Utf8Path::new(fixture));
            assert_eq!(parent.as_deref(), Some(Utf8Path::new(rule)));
        }
    }

    #[cfg(unix)]
    mod links {
        use super::super::*;
        use pretty_assertions::assert_eq;
        use std::os::unix::fs::symlink;

        /// A temporary directory holding `real/a.yml` and `real/sub/b.yml`.
        fn tree() -> (tempfile::TempDir, Utf8PathBuf) {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("UTF-8");
            std::fs::create_dir_all(root.join("real/sub")).expect("mkdir");
            std::fs::write(root.join("real/a.yml"), "").expect("write");
            std::fs::write(root.join("real/sub/b.yml"), "").expect("write");
            (temp, root)
        }

        /// The files under `dir`, relative to it.
        fn found(dir: &Utf8Path) -> Vec<String> {
            files_under(dir, DanglingLinks::Refuse)
                .expect("walk")
                .iter()
                .map(|path| {
                    let path = path.strip_prefix(dir).expect("under dir");
                    path.to_string_lossy().into_owned()
                })
                .collect()
        }

        #[test]
        fn links_are_followed_and_a_directory_is_walked_once() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            // To the walk itself, above it, and to a directory beside.
            symlink("..", dir.join("sub/loop")).expect("link");
            symlink("../..", dir.join("sub/out")).expect("link");
            symlink("sub", dir.join("z-again")).expect("link");
            symlink("a.yml", dir.join("z.yml")).expect("link");
            assert_eq!(found(&dir), ["a.yml", "sub/b.yml", "z.yml"]);
        }

        #[test]
        fn a_link_to_nothing_is_an_error_that_names_it() {
            let (_temp, root) = tree();
            let link = root.join("real/gone.yml");
            symlink("../nowhere.yml", &link).expect("link");
            for dangling in [DanglingLinks::Refuse, DanglingLinks::RefuseRules] {
                let err = files_under(&root.join("real"), dangling).expect_err("dangling");
                assert_eq!(
                    err.to_string(),
                    format!(
                        "io error reading {link}: a link to '../nowhere.yml', where nothing is"
                    )
                );
            }
            assert!(leads_nowhere(&link));
            assert!(!leads_nowhere(&root.join("real/a.yml")));
            assert!(!leads_nowhere(&root.join("nothing")));
        }

        #[test]
        fn a_link_to_itself_is_one_to_nothing() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            // Directly, and by way of another link.
            for (target, link) in [("loop", "loop"), ("two", "one"), ("one", "two")] {
                symlink(target, dir.join(link)).expect("link");
                assert!(leads_nowhere(&dir.join(link)), "{link}");
            }
            let err = files_under(&dir, DanglingLinks::Refuse).expect_err("loop");
            assert!(err.to_string().contains("real/loop: "), "{err}");
            let files = files_under(&dir, DanglingLinks::RefuseRules).expect("walk");
            assert_eq!(files.len(), 2, "{files:?}");
            files_under(&dir.join("loop"), DanglingLinks::RefuseRules).expect_err("its own");
            symlink("loop.yml", dir.join("loop.yml")).expect("link");
            let err = files_under(&dir, DanglingLinks::RefuseRules).expect_err("loop");
            assert!(err.to_string().contains("real/loop.yml: "), "{err}");
        }

        #[test]
        fn a_special_file_named_as_a_rule_is_an_error() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            let _other = std::os::unix::net::UnixListener::bind(dir.join("s")).expect("socket");
            assert_eq!(found(&dir), ["a.yml", "sub/b.yml"]);
            for name in ["s.yml", "s.test.YAML"] {
                let socket = dir.join(name);
                let _bound = std::os::unix::net::UnixListener::bind(&socket).expect("socket");
                for dangling in [DanglingLinks::Refuse, DanglingLinks::RefuseRules] {
                    let err = files_under(&dir, dangling).expect_err("special");
                    assert_eq!(
                        err.to_string(),
                        format!("io error reading {socket}: not a regular file")
                    );
                }
                std::fs::remove_file(&socket).expect("rm");
            }
        }

        #[test]
        fn the_projects_rule_directory_is_there_or_not_or_an_error() {
            let (_temp, root) = tree();
            let project = || project_dir(&root);
            assert_eq!(project().expect("no .dq"), None);
            std::fs::create_dir(root.join(".dq")).expect("mkdir");
            assert_eq!(project().expect("no rules"), None);
            let rules = root.join(".dq/rules");
            symlink("../real", &rules).expect("link");
            assert_eq!(project().expect("linked"), Some(rules.clone()));
            for (target, what) in [
                ("rules", "io error reading "),
                ("gone", "a link to 'gone', where nothing is"),
                ("../real/a.yml", "./.dq/rules is not a directory"),
                ("../real/a.yml/b", "/.dq/rules: "),
            ] {
                std::fs::remove_file(&rules).expect("rm");
                symlink(target, &rules).expect("link");
                let err = project().expect_err(target).to_string();
                assert!(err.contains(what), "{target}: {err}");
            }
        }

        #[test]
        fn a_link_to_nothing_that_is_no_rule_stops_a_walk_for_rules_only() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            symlink("me@host.1:2", dir.join(".#a.yml")).expect("lock");
            assert_eq!(found(&dir), ["a.yml", "sub/b.yml"]);

            let link = dir.join("company");
            symlink("../not-checked-out", &link).expect("link");
            let err = files_under(&dir, DanglingLinks::Refuse).expect_err("dangling");
            assert!(err.to_string().contains(link.as_str()), "{err}");
            let files = files_under(&dir, DanglingLinks::RefuseRules).expect("walk");
            assert_eq!(files.len(), 2, "{files:?}");

            // The directory itself is never passed over.
            for dangling in [DanglingLinks::Refuse, DanglingLinks::RefuseRules] {
                files_under(&link, dangling).expect_err("the walk's own directory");
            }
        }
    }
}
