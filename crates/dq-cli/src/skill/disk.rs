//! The files in a skill directory: what is at a path, and putting another
//! file there or deleting it.
//!
//! A write or a deletion is bound to what was seen when the command decided
//! ([`Seen`]): what is there is read once more right before, and if it is
//! something else, it is someone's work and stays. No link below the skill
//! directory is followed: the directories on the way are looked at last of
//! all. Between that and the change a moment remains, which
//! [`dq_core::atomic_write::write`] has no way to close.

use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Read};

use camino::{Utf8Path, Utf8PathBuf};

use super::place::identity;
use super::survey::{files_under, spelling};
use crate::commands::self_cmd::sha256_hex;

/// The largest file read. dq's own files are far smaller; a larger one in
/// their place was not written by dq.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// What a path of the skill holds.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Found {
    /// Nothing.
    Missing,
    /// A regular file.
    File(Vec<u8>),
    /// A regular file larger than was asked for.
    Large,
    /// A link, a device.
    Other,
    /// A directory, or a path that goes through something that is none.
    Blocked(String),
}

/// What a path held when it was looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Seen {
    /// Nothing.
    Missing,
    /// A regular file with this SHA-256.
    File(String),
    /// Anything else.
    Other,
}

impl Found {
    /// What is kept of it.
    pub(super) fn seen(&self) -> Seen {
        match self {
            Self::Missing => Seen::Missing,
            Self::File(bytes) => Seen::File(sha256_hex(bytes)),
            Self::Large | Self::Other | Self::Blocked(_) => Seen::Other,
        }
    }
}

/// What a write or a deletion came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Done {
    /// It was done.
    Yes,
    /// There was nothing to do.
    No,
    /// It was not done: what is there is not what was seen.
    Changed,
}

/// What is at `path`, a plain relative path, in `dir`. No link is followed
/// below `dir`.
///
/// # Errors
///
/// [`dq_core::Error::Io`] when it cannot be told, or not be read.
pub(super) fn find(dir: &Utf8Path, path: &str) -> Result<Found, dq_core::Error> {
    find_up_to(dir, path, MAX_FILE_BYTES)
}

/// [`find`], of a file of at most `max` bytes.
///
/// # Errors
///
/// As [`find`].
pub(super) fn find_up_to(dir: &Utf8Path, path: &str, max: u64) -> Result<Found, dq_core::Error> {
    let mut at = Utf8PathBuf::new();
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        at.push(component);
        let here = dir.join(&at);
        let meta = match fs::symlink_metadata(&here) {
            Ok(meta) => meta,
            // Where no file can have such a name, there is none.
            Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::InvalidFilename) => {
                return Ok(Found::Missing);
            }
            Err(err) => return Err(read_error(&here, err)),
        };
        let last = components.peek().is_none();
        if meta.is_dir() == last {
            return Ok(Found::Blocked(at.into_string()));
        }
        if last {
            if !meta.is_file() {
                return Ok(Found::Other);
            }
            if meta.len() > max {
                return Ok(Found::Large);
            }
            return match read_up_to(&here, max) {
                Ok(bytes) if bytes.len() as u64 > max => Ok(Found::Large),
                Ok(bytes) => Ok(Found::File(bytes)),
                Err(err) if err.kind() == ErrorKind::NotFound => Ok(Found::Missing),
                Err(err) => Err(read_error(&here, err)),
            };
        }
    }
    Ok(Found::Missing)
}

/// The bytes of a file, or the first `max` of them and one.
fn read_up_to(path: &Utf8Path, max: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Put `bytes` at `path` in `dir`. A file that holds them already is not
/// touched. With `seen`, only over what was seen; without, over anything but
/// a directory.
///
/// A link (or anything else that is no regular file) is taken out first, so
/// that nothing is written through it whether or not
/// [`dq_core::atomic_write::write`] follows links.
///
/// # Errors
///
/// [`dq_core::Error`] when what is there cannot be read, or the file not be
/// written, or a link or a directory is in the way.
pub(super) fn write(
    dir: &Utf8Path,
    path: &str,
    bytes: &[u8],
    seen: Option<&Seen>,
) -> Result<Done, dq_core::Error> {
    let at = dir.join(path);
    let found = find(dir, path)?;
    if matches!(&found, Found::File(there) if there == bytes) {
        return Ok(Done::No);
    }
    if matches!(found, Found::Blocked(_)) {
        return Err(in_the_way(&at));
    }
    if seen.is_some_and(|seen| *seen != found.seen()) {
        return Ok(Done::Changed);
    }
    if found == Found::Other {
        unlink(dir, path)?;
    }
    if let Some(parent) = at.parent() {
        fs::create_dir_all(parent).map_err(|err| write_error(parent, err))?;
    }
    put(dir, path, bytes)?;
    Ok(Done::Yes)
}

/// Write `bytes` to `path` in `dir`, if no link is on the way there.
fn put(dir: &Utf8Path, path: &str, bytes: &[u8]) -> Result<(), dq_core::Error> {
    ensure_plain_way(dir, path)?;
    dq_core::atomic_write::write(&dir.join(path), bytes, false)
}

/// Delete the file (or link) at `path` in `dir`, if no link is on the way
/// there. Whether there was one.
fn unlink(dir: &Utf8Path, path: &str) -> Result<bool, dq_core::Error> {
    ensure_plain_way(dir, path)?;
    let at = dir.join(path);
    match fs::remove_file(&at) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(write_error(&at, err)),
    }
}

/// Remove the directory at `path` in `dir`, if it is empty and no link is on
/// the way there. Whether it is gone.
fn remove_empty(dir: &Utf8Path, path: &str) -> Result<bool, dq_core::Error> {
    ensure_plain_way(dir, path)?;
    remove_if_empty(&dir.join(path))
}

/// Delete the file (or link) at `path` in `dir`, and the directories above
/// it, up to `dir`, that this leaves empty. With `seen`, only what was seen;
/// without, anything but a directory.
///
/// # Errors
///
/// [`dq_core::Error`] when what is there cannot be read, or not be deleted,
/// or a link or a directory is in the way.
pub(super) fn delete(
    dir: &Utf8Path,
    path: &str,
    seen: Option<&Seen>,
) -> Result<Done, dq_core::Error> {
    let at = dir.join(path);
    match find(dir, path)? {
        Found::Missing => return Ok(Done::No),
        Found::Blocked(_) => return Err(in_the_way(&at)),
        found if seen.is_some_and(|seen| *seen != found.seen()) => return Ok(Done::Changed),
        _ => {}
    }
    if !unlink(dir, path)? {
        return Ok(Done::No);
    }
    let mut parent = Utf8Path::new(path).parent();
    while let Some(relative) = parent.filter(|p| !p.as_str().is_empty()) {
        if !remove_empty(dir, relative.as_str())? {
            break;
        }
        parent = relative.parent();
    }
    Ok(Done::Yes)
}

/// Remove a directory, if it is empty. Whether it is gone.
pub(super) fn remove_if_empty(dir: &Utf8Path) -> Result<bool, dq_core::Error> {
    match fs::remove_dir(dir) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
        Err(err) if err.kind() == ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(err) => Err(write_error(dir, err)),
    }
}

/// Give what is found at `path` in `dir` under another spelling the one of
/// `path`: each directory on the way, then the file. Whether that was done.
/// A directory keeps its spelling when there is a file in it that is none of
/// `ours`, the paths of dq's files.
///
/// Before the file is written, so that a run that is cut short leaves the
/// file under the old name, or under the new one as it was: either is
/// recognised by the next.
///
/// # Errors
///
/// [`dq_core::Error`] when a directory cannot be read, or not be renamed.
pub(super) fn respell(dir: &Utf8Path, path: &str, ours: &[&str]) -> Result<bool, dq_core::Error> {
    let there = spelling(dir, path)?;
    if there == path {
        return Ok(false);
    }
    let mut parent = dir.to_path_buf();
    let mut names = there.split('/').zip(path.split('/')).peekable();
    while let Some((name, wanted)) = names.next() {
        let (from, to) = (parent.join(name), parent.join(wanted));
        let is_file = names.peek().is_none();
        // Anything else is the same file by a name that is not its own.
        if name != wanted
            && name.eq_ignore_ascii_case(wanted)
            && (is_file || holds_only(&from, dir, ours)?)
        {
            fs::rename(&from, &to).map_err(|err| write_error(&to, err))?;
        }
        parent = to;
    }
    // Some file systems take that for a rename of a file to itself.
    Ok(spelling(dir, path)? != there)
}

/// Whether every file under `parent` is one of `ours`, paths in `dir`.
fn holds_only(parent: &Utf8Path, dir: &Utf8Path, ours: &[&str]) -> Result<bool, dq_core::Error> {
    let ours: BTreeSet<_> = ours
        .iter()
        .filter_map(|path| identity(&dir.join(path)))
        .collect();
    Ok(files_under(parent)?
        .iter()
        .all(|file| identity(&parent.join(file)).is_some_and(|id| ours.contains(&id))))
}

/// That every directory on the way to `path` in `dir` is one, and no link.
fn ensure_plain_way(dir: &Utf8Path, path: &str) -> Result<(), dq_core::Error> {
    let at = dir.join(path);
    for above in at.ancestors().skip(1) {
        if !fs::symlink_metadata(above).is_ok_and(|meta| meta.is_dir()) {
            return Err(in_the_way(&at));
        }
        if above == dir {
            break;
        }
    }
    Ok(())
}

/// [`dq_core::Error::WriteIo`] for a path that cannot be written to.
fn in_the_way(at: &Utf8Path) -> dq_core::Error {
    let err = std::io::Error::other("a link or a directory is in the way");
    write_error(at, err)
}

/// [`dq_core::Error::Io`] for `path`.
pub(super) fn read_error(path: &Utf8Path, source: std::io::Error) -> dq_core::Error {
    dq_core::Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// [`dq_core::Error::WriteIo`] for `path`.
pub(super) fn write_error(path: &Utf8Path, source: std::io::Error) -> dq_core::Error {
    dq_core::Error::WriteIo {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::os::unix::fs::symlink;

    fn dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("UTF-8");
        let dir = root.join("dq");
        fs::create_dir_all(dir.join("references")).expect("mkdir");
        (tmp, dir)
    }

    fn file(text: &str) -> Seen {
        Seen::File(sha256_hex(text.as_bytes()))
    }

    /// What was there when the command decided, what is there now, and
    /// whether that is replaced and deleted.
    #[test]
    fn a_change_is_bound_to_what_was_seen() {
        let (_tmp, dir) = dir();
        let path = "references/a.md";
        let at = dir.join(path);
        let set = |text: Option<&str>| {
            let _ = fs::remove_file(&at);
            fs::create_dir_all(dir.join("references")).expect("mkdir");
            if let Some(text) = text {
                fs::write(&at, text).expect("write");
            }
        };
        let read = || fs::read_to_string(&at).ok();
        for (seen, now, written, deleted) in [
            (Some(file("old")), Some("old"), Done::Yes, Done::Yes),
            (
                Some(file("old")),
                Some("mine"),
                Done::Changed,
                Done::Changed,
            ),
            (Some(file("old")), Some("new"), Done::No, Done::Changed),
            (Some(file("old")), None, Done::Changed, Done::No),
            (Some(Seen::Missing), None, Done::Yes, Done::No),
            (
                Some(Seen::Missing),
                Some("mine"),
                Done::Changed,
                Done::Changed,
            ),
            (Some(Seen::Missing), Some("new"), Done::No, Done::Changed),
            (
                Some(Seen::Other),
                Some("mine"),
                Done::Changed,
                Done::Changed,
            ),
            (None, Some("mine"), Done::Yes, Done::Yes),
            (None, None, Done::Yes, Done::No),
        ] {
            set(now);
            let done = write(&dir, path, b"new", seen.as_ref()).expect("write");
            assert_eq!(done, written, "{seen:?}, {now:?}");
            let expected = if done == Done::Yes { Some("new") } else { now };
            assert_eq!(read().as_deref(), expected, "{seen:?}, {now:?}");
            set(now);
            let done = delete(&dir, path, seen.as_ref()).expect("delete");
            assert_eq!(done, deleted, "{seen:?}, {now:?}");
            let expected = if done == Done::Yes { None } else { now };
            assert_eq!(read().as_deref(), expected, "{seen:?}, {now:?}");
        }
    }

    /// A link in the place of a directory, put there after the command
    /// decided: nothing is written, deleted, renamed or removed behind it.
    #[test]
    fn nothing_is_changed_through_a_link() {
        let (_tmp, dir) = dir();
        let outside = dir.with_file_name("outside");
        fs::create_dir(&outside).expect("mkdir");
        fs::write(outside.join("a.md"), "old").expect("write");
        fs::remove_dir(dir.join("references")).expect("rmdir");
        symlink(&outside, dir.join("references")).expect("symlink");
        let is_in_the_way = |result: Result<(), dq_core::Error>| {
            let err = result.expect_err("in the way");
            assert!(
                matches!(&err, dq_core::Error::WriteIo { .. })
                    && err
                        .to_string()
                        .contains("a link or a directory is in the way"),
                "{err}"
            );
        };
        for seen in [Some(file("old")), None] {
            is_in_the_way(write(&dir, "references/a.md", b"new", seen.as_ref()).map(|_| ()));
            is_in_the_way(write(&dir, "references/b.md", b"new", seen.as_ref()).map(|_| ()));
            is_in_the_way(delete(&dir, "references/a.md", seen.as_ref()).map(|_| ()));
        }
        // The last look of all, which only the change itself comes after.
        fs::create_dir(outside.join("empty")).expect("mkdir");
        is_in_the_way(put(&dir, "references/a.md", b"new"));
        is_in_the_way(unlink(&dir, "references/a.md").map(|_| ()));
        is_in_the_way(remove_empty(&dir, "references/empty").map(|_| ()));
        assert!(outside.join("empty").is_dir());
        is_in_the_way(put(&dir, "references/empty/a.md", b"new"));
        assert!(!outside.join("empty/a.md").exists(), "further up the way");
        ensure_plain_way(&dir, "SKILL.md").expect("no link on the way");
        let moved = dir.with_file_name("moved");
        fs::rename(&dir, &moved).expect("mv");
        symlink(&moved, &dir).expect("symlink");
        is_in_the_way(ensure_plain_way(&dir, "SKILL.md"));
        assert_eq!(
            fs::read_to_string(outside.join("a.md")).expect("read"),
            "old"
        );
        assert!(!outside.join("b.md").exists());
    }

    #[test]
    fn a_name_no_file_can_have_is_that_of_none() {
        let (_tmp, dir) = dir();
        let long = "a".repeat(300);
        for path in [long.clone(), format!("{long}/a.md")] {
            assert_eq!(find(&dir, &path).expect("found"), Found::Missing);
        }
    }

    #[test]
    fn a_file_larger_than_any_of_dqs_is_not_read() {
        let (_tmp, dir) = dir();
        fs::write(dir.join("a"), "1234").expect("write");
        let found = |max| find_up_to(&dir, "a", max).expect("found");
        assert_eq!(found(4), Found::File(b"1234".to_vec()));
        assert_eq!(found(3), Found::Large);
        let file = fs::File::create(dir.join("b")).expect("create");
        file.set_len(MAX_FILE_BYTES + 1).expect("grow");
        assert_eq!(find(&dir, "b").expect("found"), Found::Large);
    }
}
