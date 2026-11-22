//! What is in a skill directory: which files are dq's as it wrote them,
//! which were edited, which are not dq's at all. Reads only.
//!
//! Two paths can be one file, on a file system that ignores case: a path
//! finds what is there under another spelling. So paths are grouped by the
//! file they find ([`groups`]), which its identity decides, not a comparison
//! of names.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;

use camino::{Utf8Path, Utf8PathBuf};

use super::content::Edition;
use super::disk::{Found, Seen, find, find_up_to, read_error};
use super::manifest::{MANIFEST, MAX_BYTES, Manifest, ManifestError};
use super::place::identity;

/// A file an operating system puts into a directory it shows; never counted.
const FINDER_FILE: &str = ".DS_Store";

/// The manifest of a skill directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// There is none.
    No,
    /// There is one, and it is usable.
    Yes(Manifest),
    /// There is one, and it is not.
    Unusable(ManifestError),
}

impl Installed {
    /// The manifest, when it is usable.
    pub(super) fn manifest(&self) -> Option<&Manifest> {
        match self {
            Self::Yes(manifest) => Some(manifest),
            Self::No | Self::Unusable(_) => None,
        }
    }
}

/// What a file of the manifest or of the edition at hand is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Not there, as it should be or may be.
    Absent,
    /// Not there, though the manifest lists it.
    Missing,
    /// Not there, for the run that was to write it did not come to it; the
    /// edition has it too.
    Unwritten,
    /// As the edition has it.
    Identical,
    /// As dq wrote it.
    Unedited,
    /// Listed, and neither as dq wrote it nor as the edition has it.
    Edited,
    /// Not dq's, where the edition has a file.
    Colliding,
    /// Not dq's, where a run that did not come to its end was to write a file
    /// the edition has not. In no one's way.
    Theirs,
    /// What stands in the way of a file dq wrote: a link or a file in the
    /// place of a directory, a directory in the place of a file.
    Blocked(String),
    /// The same, of a file only the edition has, or that was yet to be
    /// written.
    Obstructed(String),
}

/// One file of the manifest or of the edition at hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileState {
    /// The path messages name.
    pub path: String,
    /// The paths under which the manifest lists it: one, but for spellings
    /// that are the same file here. None of one that is not dq's: a file that
    /// was yet to be written, when what is there is something else. What is
    /// not listed is never deleted.
    pub listed: Vec<String>,
    /// The path at which the edition has it.
    pub in_edition: Option<&'static str>,
    /// What was there.
    pub seen: Seen,
    /// What that is.
    pub verdict: Verdict,
}

impl FileState {
    /// Whether dq wrote what is there.
    pub(super) fn is_dqs(&self) -> bool {
        matches!(self.verdict, Verdict::Identical | Verdict::Unedited)
    }

    /// Whether it keeps the skill from being updated without being asked: it
    /// was changed, or removed though the edition has it.
    pub(super) fn is_change(&self) -> bool {
        match self.verdict {
            Verdict::Absent
            | Verdict::Unwritten
            | Verdict::Identical
            | Verdict::Unedited
            | Verdict::Theirs => false,
            Verdict::Missing => self.in_edition.is_some(),
            Verdict::Edited | Verdict::Colliding | Verdict::Blocked(_) | Verdict::Obstructed(_) => {
                true
            }
        }
    }
}

/// A skill directory, as an edition sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Survey {
    /// The manifest.
    pub installed: Installed,
    /// Every file of the manifest and of the edition.
    pub files: Vec<FileState>,
    /// Files (and directories that cannot be read) that the manifest does not
    /// list, sorted, whatever the edition has. A file the edition has at the
    /// same path, byte for byte, is not counted: it is what an interrupted
    /// install leaves. They stop nothing but for the colliding among them.
    pub unlisted: Vec<String>,
    /// The manifest's name, when a directory is in its place.
    pub manifest_in_the_way: Option<String>,
}

impl Survey {
    /// The paths messages name of the files with a verdict, sorted.
    fn paths(&self, verdict: &Verdict) -> Vec<String> {
        let files = self.files.iter().filter(|file| file.verdict == *verdict);
        let mut paths: Vec<String> = files.map(|file| file.path.clone()).collect();
        paths.sort();
        paths
    }

    /// Files the manifest lists that were edited.
    pub(super) fn edited(&self) -> Vec<String> {
        self.paths(&Verdict::Edited)
    }

    /// Files the manifest lists that are gone, or have not been written yet.
    pub(super) fn missing(&self) -> Vec<String> {
        let mut missing = self.paths(&Verdict::Missing);
        missing.extend(self.paths(&Verdict::Unwritten));
        missing.sort();
        missing
    }

    /// Files and links at a path the edition has a file at, not dq's.
    pub(super) fn colliding(&self) -> Vec<String> {
        self.paths(&Verdict::Colliding)
    }

    /// What stands in the way of a file the manifest lists.
    pub(super) fn blocked(&self) -> Vec<String> {
        self.obstacles(false)
    }

    /// What stands in the way of a file the manifest lists or the edition
    /// has, each once.
    pub(super) fn in_the_way(&self) -> Vec<String> {
        self.obstacles(true)
    }

    fn obstacles(&self, of_edition: bool) -> Vec<String> {
        let at: BTreeSet<&String> = self
            .files
            .iter()
            .filter_map(|file| match &file.verdict {
                Verdict::Blocked(at) => Some(at),
                Verdict::Obstructed(at) if of_edition => Some(at),
                _ => None,
            })
            .chain(&self.manifest_in_the_way)
            .collect();
        at.into_iter().cloned().collect()
    }
}

/// Look at every file of `dir`, which is a directory, as `edition` sees it.
///
/// # Errors
///
/// [`dq_core::Error::Io`] when the directory or a file of dq's cannot be read.
pub(super) fn survey(dir: &Utf8Path, edition: &Edition) -> Result<Survey, dq_core::Error> {
    let installed = read_manifest(dir)?;
    let manifest_in_the_way = match find(dir, MANIFEST)? {
        Found::Blocked(at) => Some(at),
        _ => None,
    };
    let files = files(dir, installed.manifest(), edition)?.collect::<Result<Vec<_>, _>>()?;
    let mut unlisted: BTreeSet<String> = files_under(dir)?.into_iter().collect();
    unlisted.remove(MANIFEST);
    let mut names = Names::new(dir);
    for file in &files {
        let is_ours = !file.listed.is_empty() || file.verdict == Verdict::Identical;
        if !is_ours || file.seen == Seen::Missing {
            continue;
        }
        // All of them are one file; as a rule there is one, and it has its
        // own name.
        let mut paths = file
            .listed
            .iter()
            .map(String::as_str)
            .chain(file.in_edition);
        if paths.any(|path| unlisted.remove(path)) {
            continue;
        }
        unlisted.remove(&names.spelling(&file.path)?);
    }
    Ok(Survey {
        installed,
        files,
        unlisted: unlisted.into_iter().collect(),
        manifest_in_the_way,
    })
}

/// Every file of `manifest` and `edition` in `dir`, looked at one by one as
/// the iterator goes.
///
/// # Errors
///
/// [`dq_core::Error::Io`] when the directory, or of an item when a file of
/// it, cannot be read.
pub(super) fn files<'a>(
    dir: &'a Utf8Path,
    manifest: Option<&'a Manifest>,
    edition: &'a Edition,
) -> Result<impl Iterator<Item = Result<FileState, dq_core::Error>> + 'a, dq_core::Error> {
    let paths: BTreeSet<&str> = manifest
        .into_iter()
        .flat_map(Manifest::paths)
        .chain(edition.files.iter().map(|file| file.path))
        .collect();
    Ok(groups(dir, paths)?
        .into_iter()
        .map(move |group| file_state(dir, manifest, edition, &group)))
}

/// What the file is that all of `group` find.
fn file_state(
    dir: &Utf8Path,
    manifest: Option<&Manifest>,
    edition: &Edition,
    group: &[&str],
) -> Result<FileState, dq_core::Error> {
    let lists = |path: &&str| manifest.is_some_and(|m| m.digests(path).next().is_some());
    let in_manifest: Vec<&str> = group.iter().copied().filter(lists).collect();
    let any = |is: &dyn Fn(&Manifest, &str) -> bool| {
        manifest.is_some_and(|m| in_manifest.iter().any(|path| is(m, path)))
    };
    let file = edition.files.iter().find(|file| group.contains(&file.path));
    let path = in_manifest.first().copied().unwrap_or(group[0]);
    let found = find(dir, path)?;
    let seen = found.seen();
    // A file a run was yet to write may be anyone's, until it is as dq has it.
    let was_written = any(&|m, path| m.files.contains_key(path));
    let is_as_written = any(&|m, path| {
        let is_seen = |digest: &str| seen == Seen::File(digest.to_owned());
        m.digests(path).any(is_seen)
    });
    let verdict = match found {
        Found::Blocked(at) => {
            let at = spelling(dir, &at)?;
            if was_written {
                Verdict::Blocked(at)
            } else if file.is_some() {
                Verdict::Obstructed(at)
            } else {
                Verdict::Theirs
            }
        }
        Found::Missing if in_manifest.is_empty() => Verdict::Absent,
        Found::Missing if !any(&|m, path| m.may_lack(path)) => Verdict::Missing,
        Found::Missing if !was_written && file.is_some() => Verdict::Unwritten,
        Found::Missing => Verdict::Absent,
        Found::File(bytes, _) if file.is_some_and(|file| file.text.as_bytes() == bytes) => {
            Verdict::Identical
        }
        Found::File(..) | Found::Large | Found::Other if is_as_written => Verdict::Unedited,
        Found::File(..) | Found::Large | Found::Other if was_written => Verdict::Edited,
        Found::File(..) | Found::Large | Found::Other if file.is_some() => Verdict::Colliding,
        Found::File(..) | Found::Large | Found::Other => Verdict::Theirs,
    };
    let listed = if was_written || is_as_written {
        in_manifest.iter().map(|path| (*path).to_owned()).collect()
    } else {
        Vec::new()
    };
    Ok(FileState {
        path: path.to_owned(),
        listed,
        in_edition: file.map(|file| file.path),
        seen,
        verdict,
    })
}

/// `paths`, in groups that find the same file in `dir`.
///
/// Two names of one file that the directory has both (hard links) are two
/// entries, so two groups.
fn groups<'a>(
    dir: &Utf8Path,
    paths: BTreeSet<&'a str>,
) -> Result<Vec<Vec<&'a str>>, dq_core::Error> {
    let mut groups = Vec::new();
    let mut by_identity: BTreeMap<_, Vec<&str>> = BTreeMap::new();
    for path in paths {
        match identity(&dir.join(path)) {
            None => groups.push(vec![path]),
            Some(id) => by_identity.entry(id).or_default().push(path),
        }
    }
    let mut names = Names::new(dir);
    for (_, paths) in by_identity {
        if paths.len() == 1 {
            groups.push(paths);
            continue;
        }
        let mut by_name: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for path in paths {
            by_name.entry(names.spelling(path)?).or_default().push(path);
        }
        groups.extend(by_name.into_values());
    }
    groups.sort();
    Ok(groups)
}

/// `path`, a plain relative path, with every name as the directory it is in
/// has it. From where nothing is there, or no directory that can be read, the
/// rest is as given. No link is followed below `dir`.
///
/// # Errors
///
/// [`dq_core::Error::Io`] when a directory cannot be read.
pub(super) fn spelling(dir: &Utf8Path, path: &str) -> Result<String, dq_core::Error> {
    Names::new(dir).spelling(path)
}

/// The names in the directories under `dir`, each directory read once.
struct Names<'a> {
    dir: &'a Utf8Path,
    read: BTreeMap<Utf8PathBuf, Vec<String>>,
}

impl<'a> Names<'a> {
    fn new(dir: &'a Utf8Path) -> Self {
        Self {
            dir,
            read: BTreeMap::new(),
        }
    }

    /// [`spelling`].
    fn spelling(&mut self, path: &str) -> Result<String, dq_core::Error> {
        let mut at = self.dir.to_path_buf();
        let mut spelled = Vec::new();
        for name in path.split('/') {
            let is_dir = fs::symlink_metadata(&at).is_ok_and(|meta| meta.is_dir());
            let found = if is_dir { self.entry(&at, name)? } else { None };
            let name = found.unwrap_or_else(|| name.to_owned());
            at.push(&name);
            spelled.push(name);
        }
        Ok(spelled.join("/"))
    }

    /// The name that `parent` has for what is found in it as `name`.
    fn entry(&mut self, parent: &Utf8Path, name: &str) -> Result<Option<String>, dq_core::Error> {
        let Some(wanted) = identity(&parent.join(name)) else {
            return Ok(None);
        };
        if !self.read.contains_key(parent) {
            let names = names_in(parent).map_err(|err| read_error(parent, err))?;
            self.read.insert(parent.to_path_buf(), names);
        }
        let names = &self.read[parent];
        if names.iter().any(|found| found == name) {
            return Ok(Some(name.to_owned()));
        }
        // The names of a file that has several (hard links) are the same
        // file too: the one that differs in case is the one that was found.
        let is_it = |found: &&String| identity(&parent.join(found)).as_ref() == Some(&wanted);
        let in_case = |found: &&String| found.eq_ignore_ascii_case(name);
        let found = names
            .iter()
            .filter(in_case)
            .find(is_it)
            .or_else(|| names.iter().find(is_it));
        Ok(found.cloned())
    }
}

/// The names in a directory; none of one that is gone or cannot be listed.
fn names_in(dir: &Utf8Path) -> std::io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::NotFound | ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(err) => return Err(err),
    };
    let mut names = Vec::new();
    for entry in entries {
        names.extend(entry?.file_name().into_string().ok());
    }
    Ok(names)
}

/// The manifest in `dir`.
///
/// # Errors
///
/// [`dq_core::Error::Io`] when it is there and cannot be read.
pub(super) fn read_manifest(dir: &Utf8Path) -> Result<Installed, dq_core::Error> {
    let unusable = |reason: String| Installed::Unusable(ManifestError::Malformed { reason });
    Ok(match find_up_to(dir, MANIFEST, MAX_BYTES)? {
        Found::Missing => Installed::No,
        Found::File(bytes, _) => match Manifest::parse(&bytes) {
            Ok(manifest) => Installed::Yes(manifest),
            Err(err) => Installed::Unusable(err),
        },
        Found::Large => unusable(format!("it is larger than {MAX_BYTES} bytes")),
        Found::Other | Found::Blocked(_) => unusable("it is no regular file".to_owned()),
    })
}

/// Every file (anything but a directory) under `dir`, and every directory in
/// it that cannot be read, as a relative path, sorted; links are not
/// followed. Left out: what a system puts there itself, and the temporary
/// files dq's own writes rename into place — a concurrent dq must not take
/// them for a user's.
///
/// # Errors
///
/// [`dq_core::Error::Io`] when a directory cannot be read for another reason
/// than that it may not be, and when `dir` cannot.
pub(super) fn files_under(dir: &Utf8Path) -> Result<Vec<String>, dq_core::Error> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(dir).min_depth(1) {
        let path = match entry {
            Ok(entry) => {
                let name = entry.file_name().to_string_lossy();
                if entry.file_type().is_dir() || name == FINDER_FILE || is_write_in_progress(&name)
                {
                    continue;
                }
                entry.into_path()
            }
            Err(err) => {
                let path = err.path().filter(|path| *path != dir);
                let path = path.map(std::path::Path::to_path_buf);
                let source = std::io::Error::from(err);
                match (source.kind(), path) {
                    (ErrorKind::NotFound, _) => continue,
                    (ErrorKind::PermissionDenied, Some(path)) => path,
                    _ => return Err(read_error(dir, source)),
                }
            }
        };
        files.push(relative(&path, dir));
    }
    files.sort();
    Ok(files)
}

/// What is left in `dir`, whatever it is: every file, and every directory
/// with nothing in it that can be seen. Sorted.
pub(super) fn left_under(dir: &Utf8Path) -> Vec<String> {
    let mut left: Vec<String> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            !entry.file_type().is_dir()
                || fs::read_dir(entry.path()).map_or(true, |mut entries| entries.next().is_none())
        })
        .map(|entry| relative(entry.path(), dir))
        .collect();
    left.sort();
    left
}

fn relative(path: &std::path::Path, dir: &Utf8Path) -> String {
    let relative = path.strip_prefix(dir).unwrap_or(path);
    relative.to_string_lossy().into_owned()
}

/// Whether `name` is that of a file [`dq_core::atomic_write::write`] has not
/// renamed yet: `tempfile`'s default, `.tmp` and six letters or digits.
fn is_write_in_progress(name: &str) -> bool {
    name.strip_prefix(".tmp")
        .is_some_and(|rest| rest.len() == 6 && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_path_is_spelled_as_the_directories_have_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(tmp.path()).expect("UTF-8");
        fs::create_dir(dir.join("Refs")).expect("mkdir");
        fs::write(dir.join("Refs/A.md"), "").expect("write");
        fs::hard_link(dir.join("Refs/A.md"), dir.join("Refs/0.md")).expect("ln");
        let ignores_case = dir.join("refs").exists();
        for (path, spelled) in [
            ("Refs/A.md", "Refs/A.md"),
            ("Refs/0.md", "Refs/0.md"),
            ("refs/a.md", "Refs/A.md"),
            ("refs/none.md", "Refs/none.md"),
            ("none/a.md", "none/a.md"),
            ("refs/A.MD/x", "Refs/A.md/x"),
        ] {
            let expected = if ignores_case { spelled } else { path };
            assert_eq!(spelling(dir, path).expect("spelling"), expected, "{path}");
        }
        if !ignores_case {
            // Two names that differ in case are two entries, of one file or not.
            fs::hard_link(dir.join("Refs/A.md"), dir.join("Refs/a.md")).expect("ln");
            for path in ["Refs/A.md", "Refs/a.md"] {
                assert_eq!(spelling(dir, path).expect("spelling"), path);
            }
        }
    }

    /// The names of one file are one group, but for those the directory has
    /// each on its own.
    #[test]
    fn paths_are_grouped_by_the_file_they_find() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(tmp.path()).expect("UTF-8");
        fs::write(dir.join("a"), "").expect("write");
        fs::hard_link(dir.join("a"), dir.join("b")).expect("ln");
        let paths = BTreeSet::from(["A", "a", "b", "none"]);
        let expected: &[&[&str]] = if dir.join("A").exists() {
            &[&["A", "a"], &["b"], &["none"]]
        } else {
            &[&["A"], &["a"], &["b"], &["none"]]
        };
        assert_eq!(groups(dir, paths).expect("groups"), expected);
    }

    /// The names of the temporary files, as they are seen while files are
    /// made and replaced.
    #[test]
    fn the_temporary_file_of_an_atomic_write_is_recognised() {
        use dq_core::atomic_write::{create, read_no_link, rewrite};
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = Utf8Path::from_path(tmp.path()).expect("UTF-8");
        let seen = |act: &(dyn Fn() + Sync)| -> BTreeSet<String> {
            let start = std::time::Instant::now();
            let seen = std::sync::Mutex::new(BTreeSet::new());
            let is_seen = || !seen.lock().expect("lock").is_empty();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    while !is_seen() && start.elapsed().as_secs() < 60 {
                        act();
                    }
                });
                while !is_seen() && start.elapsed().as_secs() < 60 {
                    let names = names_in(dir).expect("names");
                    let others = names.into_iter().filter(|name| name != "a");
                    seen.lock().expect("lock").extend(others);
                }
            });
            seen.into_inner().expect("lock")
        };
        let at = dir.join("a");
        let bytes = [vec![b'x'; 4 << 20], vec![b'y'; 4 << 20]];
        let turn = std::sync::atomic::AtomicUsize::new(0);
        let made = seen(&|| {
            let _ = fs::remove_file(&at);
            create(&at, &bytes[0]).expect("created");
        });
        let replaced = seen(&|| {
            let n = turn.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (_, original) = read_no_link(&at).expect("read");
            rewrite(&at, &original, &bytes[n % 2], false).expect("rewritten");
        });
        for names in [made, replaced] {
            assert!(!names.is_empty(), "none was seen");
            assert!(
                names.iter().all(|name| is_write_in_progress(name)),
                "{names:?}"
            );
        }
        for name in [".tmp", ".tmpabc", ".tmpabcdefg", ".tmpab/def", "tmpabcdef"] {
            assert!(!is_write_in_progress(name), "{name}");
        }
    }
}
