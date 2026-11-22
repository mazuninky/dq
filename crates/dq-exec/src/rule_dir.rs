//! What a rule directory holds: the walk behind `--rules DIR`,
//! `./.dq/rules/`, `dq test DIR` and `dq rules add DIR`, and which of its
//! files are rules and which are fixtures.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{ExecError, Result};

/// Extensions of a rule file, and of a fixture after `.test`; ASCII case
/// does not matter.
const RULE_FILE_EXTENSIONS: [&str; 2] = ["yml", "yaml"];

/// How the names start that `dq rules add` writes under before it renames.
pub const STAGING_PREFIX: &str = ".dq-";

/// How many ASCII letters and digits come after [`STAGING_PREFIX`], and then
/// nothing.
pub const STAGING_RANDOM: usize = 6;

/// Whether `name` is one `dq rules add` writes under, `.dq-` and six letters
/// and digits: what is there is not a rule, or not yet.
#[must_use]
pub fn is_staging_name(name: &OsStr) -> bool {
    let random = name.to_str().and_then(|n| n.strip_prefix(STAGING_PREFIX));
    random.is_some_and(|random| {
        random.len() == STAGING_RANDOM && random.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

/// What [`files_under`] does about what it cannot follow or read by way of a
/// link: a link that cannot be followed — it leads to nothing, back to itself,
/// through a file, past what may not be looked at, by more links than the
/// system follows — and a directory behind one that cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DanglingLinks {
    /// An error, all of it: in a directory of rules it may have been a
    /// directory of them, such as a submodule that is not checked out.
    Refuse,
    /// A link that cannot be followed is an error when it is named as a rule
    /// file or a fixture. Any other, and a directory that cannot be read when
    /// a link led to it, or above it, is passed over with a warning. The walk
    /// is over any part of a repository, where a link may have nothing to do
    /// with rules.
    RefuseRules,
}

/// A file a walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkedFile {
    /// Its name: the path the walk took from its directory, as the caller
    /// spelled that. Not for opening the file: the links on it may be more
    /// than a system follows on one path.
    pub path: PathBuf,
    /// Where it is. No part of it but the last is a link.
    pub location: PathBuf,
}

impl WalkedFile {
    /// The location of the file itself, when the one found is a link.
    ///
    /// # Errors
    ///
    /// [`ExecError::Io`] when it cannot be resolved.
    pub fn real(&self) -> Result<PathBuf> {
        std::fs::canonicalize(&self.location)
            .map_err(|source| io_error(&self.path, &self.location, source))
    }
}

/// Every file under `dir`, at any depth, symbolic links to files and
/// directories followed — `dq rules add --symlink` makes them. In the order
/// of the names along the path, so it does not change between runs.
///
/// A directory is walked once, under the path to it with the fewest links,
/// and of those the first by name: a link to one that is in `dir`, or that a
/// link before it leads to, adds nothing and cannot loop. A file that is
/// linked to comes once per link, under the link's path.
///
/// An editor's lock (`.#a.yml`, a link to no path) is never a rule, and what
/// has a staging name ([`is_staging_name`]) is left out: a `dq rules add` that
/// was killed may have left it. A directory of that name is warned about.
///
/// # Errors
///
/// [`ExecError::Io`] naming a link that cannot be followed or what cannot be
/// read, as `dangling` says, or a FIFO, socket or device named as a rule file
/// or a fixture: skipping it would leave out rules without a word.
pub fn files_under(dir: &Utf8Path, dangling: DanglingLinks) -> Result<Vec<WalkedFile>> {
    let mut files = Vec::new();
    let mut walked: HashSet<PathBuf> = HashSet::new();
    // Each link to a directory is a tree of its own, walked where it really
    // is and after the trees before it: no path dq opens goes through the
    // links that led there, which add up to more than a system follows.
    let mut trees = VecDeque::from([Tree {
        path: dir.as_std_path().to_path_buf(),
        location: std::fs::canonicalize(dir)
            .map_err(|source| io_error(dir.as_std_path(), dir.as_std_path(), source))?,
        linked: false,
    }]);
    while let Some(tree) = trees.pop_front() {
        let mut entries = walkdir::WalkDir::new(&tree.location)
            .sort_by_file_name()
            .into_iter();
        while let Some(entry) = entries.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) if tree.linked && dangling == DanglingLinks::RefuseRules => {
                    warn_skipped(&tree.walk_error(err));
                    continue;
                }
                Err(err) => return Err(tree.walk_error(err)),
            };
            let path = tree.path_of(entry.path());
            // The directory of the walk and what a link leads to are there by
            // the name the user gave them.
            if entry.depth() > 0 && is_staging_name(entry.file_name()) {
                if entry.file_type().is_dir() {
                    tracing::warn!(
                        "skipped {}: left by a `dq rules add` that did not finish; \
                         it may be deleted",
                        dq_core::one_line(&path.to_string_lossy())
                    );
                    entries.skip_current_dir();
                }
                continue;
            }
            let file_type = if entry.path_is_symlink() {
                match follow(entry.path()) {
                    Ok(Followed::Directory(location)) => {
                        trees.push_back(Tree {
                            path,
                            location,
                            linked: true,
                        });
                        continue;
                    }
                    Ok(Followed::Other(file_type)) => file_type,
                    Err(err) if is_lock(entry.path(), &err) => continue,
                    Err(err) => {
                        let err = io_error(&path, entry.path(), err);
                        if dangling == DanglingLinks::Refuse || is_named_as_rule(&path) {
                            return Err(err);
                        }
                        warn_skipped(&err);
                        continue;
                    }
                }
            } else {
                entry.file_type()
            };
            if file_type.is_dir() {
                if !walked.insert(entry.into_path()) {
                    entries.skip_current_dir();
                }
            } else if file_type.is_file() {
                files.push(WalkedFile {
                    path,
                    location: entry.into_path(),
                });
            } else if is_named_as_rule(&path) {
                let source = std::io::Error::other("not a regular file");
                return Err(io_error(&path, entry.path(), source));
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// A directory to walk: the walk's own, or one a link leads to.
struct Tree {
    /// Its name, as in [`WalkedFile::path`].
    path: PathBuf,
    /// Where it is, no part of it a link.
    location: PathBuf,
    /// Whether a link led to it.
    linked: bool,
}

impl Tree {
    /// The name of what is at `location` in the tree.
    fn path_of(&self, location: &Path) -> PathBuf {
        match location.strip_prefix(&self.location) {
            Ok(rest) if rest.as_os_str().is_empty() => self.path.clone(),
            Ok(rest) => self.path.join(rest),
            Err(_) => location.to_path_buf(),
        }
    }

    /// The error of the walk over the tree, on the path it was raised for.
    fn walk_error(&self, err: walkdir::Error) -> ExecError {
        let location = err.path().unwrap_or(&self.location).to_path_buf();
        let text = err.to_string();
        let source = err
            .into_io_error()
            .unwrap_or_else(|| std::io::Error::other(text));
        io_error(&self.path_of(&location), &location, source)
    }
}

/// Say that a walk went on without what `err` names.
fn warn_skipped(err: &ExecError) {
    if let ExecError::Io { path, source } = err {
        tracing::warn!("skipped {}: {source}", dq_core::one_line(path.as_str()));
    }
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
            Err(err) => return Err(io_error(path.as_std_path(), path.as_std_path(), err)),
        }
    }
    Ok(Some(cwd.join(".dq").join("rules")))
}

/// What a link leads to.
enum Followed {
    /// A directory, where it is.
    Directory(PathBuf),
    /// Anything else.
    Other(std::fs::FileType),
}

/// Follow the link at `location` to its end.
fn follow(location: &Path) -> std::io::Result<Followed> {
    let file_type = std::fs::metadata(location)?.file_type();
    Ok(if file_type.is_dir() {
        Followed::Directory(std::fs::canonicalize(location)?)
    } else {
        Followed::Other(file_type)
    })
}

/// Whether the link at `location`, which cannot be followed for `err`, is an
/// editor's lock.
fn is_lock(location: &Path, err: &std::io::Error) -> bool {
    location
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(".#"))
        && leads_nowhere(location, err)
}

/// Whether `path` is named as a rule file or a fixture.
fn is_named_as_rule(path: &Path) -> bool {
    has_rule_extension(Utf8Path::new(&path.to_string_lossy()))
}

/// Whether the link at `location`, which cannot be followed for `err`, is one
/// to nothing or back to itself.
fn leads_nowhere(location: &Path, err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::NotFound || is_loop(location)
}

/// Whether the links on the absolute path `path` lead back to themselves: to
/// follow one of them it would have to be followed already.
///
/// The system's error does not tell. It is the same (`ELOOP`) for a chain of
/// more links than it follows, which leads to something.
fn is_loop(path: &Path) -> bool {
    enum Step {
        Enter(OsString),
        /// The link has been followed: the path is now what it leads to.
        Followed(PathBuf),
    }
    let steps_of = |path: &Path| -> Vec<Step> {
        let parts = path.components().rev();
        parts
            .filter(|part| *part != Component::CurDir)
            .map(|part| Step::Enter(part.as_os_str().to_owned()))
            .collect()
    };
    let mut at = PathBuf::new();
    let mut following: HashSet<PathBuf> = HashSet::new();
    // Links followed to the end. Followed again at every mention, links that
    // mention each other twice would double the work at each one.
    let mut leads_to: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut steps = steps_of(path);
    while let Some(step) = steps.pop() {
        let part = match step {
            Step::Followed(link) => {
                following.remove(&link);
                leads_to.insert(link, at.clone());
                continue;
            }
            Step::Enter(part) if part == ".." => {
                at.pop();
                continue;
            }
            Step::Enter(part) => part,
        };
        at.push(part);
        if let Some(target) = leads_to.get(&at) {
            at.clone_from(target);
        } else if let Ok(target) = std::fs::read_link(&at) {
            if !following.insert(at.clone()) {
                return true;
            }
            steps.push(Step::Followed(at.clone()));
            steps.extend(steps_of(&target));
            at.pop();
        }
    }
    false
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
#[must_use]
pub fn parent_rule_path(fixture: &Utf8Path) -> Option<Utf8PathBuf> {
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

/// [`ExecError::Io`] on `path`, whose `location` says so when it is a link
/// whose target is missing: the link itself is there. The target is put on
/// one line ([`dq_core::one_line`]); whoever made the link chose its text.
fn io_error(path: &Path, location: &Path, source: std::io::Error) -> ExecError {
    let target = match source.kind() {
        std::io::ErrorKind::NotFound => std::fs::read_link(location).ok(),
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

    #[test]
    fn a_staging_name_is_the_whole_name() {
        for name in [".dq-a1B2c3", ".dq-abcdef", ".dq-000000"] {
            assert!(is_staging_name(OsStr::new(name)), "{name}");
        }
        for name in [
            ".dq-",
            ".dq-abcde",
            ".dq-abcdefg",
            ".dq-abcdef.yml",
            ".dq-old-abcdef",
            ".dq-abc.ef",
            ".dq-rules",
            "x.dq-abcdef",
            "dq-abcdef",
            ".DQ-abcdef",
            ".dq-abcdé",
        ] {
            assert!(!is_staging_name(OsStr::new(name)), "{name}");
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
                .map(|file| {
                    let path = file.path.strip_prefix(dir).expect("under dir");
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
        }

        /// `DIR/00` to `DIR/NN`, each with a rule `r.yml` and a link `next` to
        /// the one after it.
        fn chain(dir: &Utf8Path, count: usize) {
            for n in 0..count {
                let dir = dir.join(format!("{n:02}"));
                std::fs::create_dir_all(&dir).expect("mkdir");
                std::fs::write(dir.join("r.yml"), n.to_string()).expect("write");
                if n + 1 < count {
                    symlink(format!("../{:02}", n + 1), dir.join("next")).expect("link");
                }
            }
        }

        #[test]
        fn the_links_on_the_way_to_a_file_do_not_add_up() {
            let (_temp, root) = tree();
            // Each directory is there without a link.
            chain(&root.join("in"), 60);
            let expected: Vec<String> = (0..60).map(|n| format!("{n:02}/r.yml")).collect();
            assert_eq!(found(&root.join("in")), expected);
            // Each directory is there by way of the one before it alone.
            chain(&root.join("out"), 60);
            symlink("../out/00", root.join("real/next")).expect("link");
            for dangling in [DanglingLinks::Refuse, DanglingLinks::RefuseRules] {
                let files = files_under(&root.join("real"), dangling).expect("walk");
                assert_eq!(files.len(), 62);
                for file in &files {
                    std::fs::read(&file.location).expect("read");
                    let real = file.real().expect("real");
                    assert_eq!(
                        Ok(real),
                        std::fs::canonicalize(&file.location).map_err(|_| ())
                    );
                }
                // Two, then the chain: its end is the deepest, so the first.
                assert_eq!(files[0].path, root.join("real/a.yml"));
                let end = root.join("real").join("next/".repeat(60));
                assert_eq!(files[1].path, end.join("r.yml"));
                let text = std::fs::read_to_string(&files[1].location).expect("read");
                assert_eq!(text, "59");
            }
        }

        #[test]
        fn directories_that_all_link_to_each_other_are_walked_once_each() {
            let (_temp, root) = tree();
            let dir = root.join("set");
            let names: Vec<String> = (0..45).map(|n| format!("{n:02}")).collect();
            for name in &names {
                let dir = dir.join(name);
                std::fs::create_dir_all(&dir).expect("mkdir");
                std::fs::write(dir.join("r.yml"), "").expect("write");
                for other in &names {
                    symlink(format!("../{other}"), dir.join(format!("to-{other}"))).expect("link");
                }
            }
            let expected: Vec<String> = names.iter().map(|name| format!("{name}/r.yml")).collect();
            assert_eq!(found(&dir), expected);
            // From one of them, every other is one link away.
            assert_eq!(found(&dir.join("07"))[..2], ["r.yml", "to-00/r.yml"]);
            assert_eq!(found(&dir.join("07")).len(), 45);
        }

        #[test]
        fn a_directory_is_found_under_the_path_with_the_fewest_links() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            // Named before the directory, and by way of another link.
            symlink("sub", dir.join("0")).expect("link");
            std::fs::create_dir_all(root.join("out/deep")).expect("mkdir");
            std::fs::write(root.join("out/deep/c.yml"), "").expect("write");
            symlink("../out", dir.join("1")).expect("link");
            symlink("deep", root.join("out/0")).expect("link");
            symlink("../../out/deep", dir.join("sub/2")).expect("link");
            assert_eq!(found(&dir), ["1/deep/c.yml", "a.yml", "sub/b.yml"]);
        }

        /// [`is_loop`], which must not take for ever to tell.
        fn is_loop_in_time(path: &Utf8Path) -> bool {
            let (tx, rx) = std::sync::mpsc::channel();
            let link = path.to_path_buf();
            std::thread::spawn(move || tx.send(is_loop(link.as_std_path())));
            rx.recv_timeout(std::time::Duration::from_secs(20))
                .unwrap_or_else(|err| panic!("is_loop({path}): {err}"))
        }

        #[test]
        fn a_loop_is_told_from_a_chain_that_is_only_long() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            let link = |target: &str, name: &str| symlink(target, dir.join(name)).expect("link");
            for (target, name) in [
                ("self", "self"),
                ("two", "one"),
                ("one", "two"),
                ("grows/x", "grows"),
                ("sub/../back/x", "back"),
                ("self/x", "by-way-of"),
                ("sub/down", "up"),
                ("../up", "sub/down"),
            ] {
                link(target, name);
            }
            for name in ["self", "one", "two", "grows", "back", "by-way-of", "up"] {
                assert!(is_loop(dir.join(name).as_std_path()), "{name}");
            }
            link("nothing", "gone");
            link("a.yml/x", "not-a-directory");
            // The same link twice on one path.
            link("sub", "s");
            link("s/../s/../s/b.yml", "twice");
            // A chain no system follows in one go.
            link("sub", "chain00");
            for n in 1..80 {
                link(&format!("chain{:02}", n - 1), &format!("chain{n:02}"));
            }
            // Each mentions the one before it twice.
            link("sub/..", "twice00");
            for n in 1..80 {
                let before = format!("twice{:02}", n - 1);
                link(
                    &format!("{before}/../real/{before}"),
                    &format!("twice{n:02}"),
                );
            }
            for name in [
                "gone",
                "not-a-directory",
                "s",
                "twice",
                "chain79",
                "twice79",
                "a.yml",
            ] {
                assert!(!is_loop_in_time(&dir.join(name)), "{name}");
            }
            assert!(!is_loop(dir.join("nothing").as_std_path()));
        }

        #[test]
        fn a_link_that_cannot_be_followed_stops_a_walk_for_rules_only() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            // No system follows as many in one go.
            symlink("sub", dir.join("chain00")).expect("link");
            for n in 1..80 {
                let target = format!("chain{:02}", n - 1);
                symlink(target, dir.join(format!("chain{n:02}"))).expect("link");
            }
            let too_many = "chain79".to_owned();
            let targets = [too_many, "a.yml/x".to_owned(), "x".repeat(300)];
            let err = files_under(&dir, DanglingLinks::Refuse).expect_err("too many");
            assert!(err.to_string().contains("real/chain"), "{err}");
            let files = files_under(&dir, DanglingLinks::RefuseRules).expect("walk");
            assert_eq!(files.len(), 2, "{files:?}");
            std::fs::create_dir(root.join("set")).expect("mkdir");
            for target in targets {
                for (name, stops) in [("notes", false), ("b.yml", true), ("b.test.YAML", true)] {
                    let link = root.join("set").join(name);
                    symlink(format!("../real/{target}"), &link).expect("link");
                    let walk = |dangling| files_under(&root.join("set"), dangling);
                    let err = walk(DanglingLinks::Refuse).expect_err(&target);
                    assert!(err.to_string().contains(link.as_str()), "{err}");
                    let walked = walk(DanglingLinks::RefuseRules);
                    assert_eq!(walked.is_err(), stops, "{name} -> {target}: {walked:?}");
                    std::fs::remove_file(&link).expect("rm");
                }
            }
        }

        #[test]
        fn a_link_to_itself_cannot_be_followed() {
            let (_temp, root) = tree();
            let dir = root.join("real");
            // Directly, and by way of another link.
            for (target, link) in [("loop", "loop"), ("two", "one"), ("one", "two")] {
                symlink(target, dir.join(link)).expect("link");
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
