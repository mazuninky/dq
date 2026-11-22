//! Atomic file replacement: the one way dq writes a user's file.
//!
//! 1. The path is resolved: a symbolic link names the file it points to, and
//!    that file is the one replaced, in its own directory. The link stays.
//! 2. A write that follows from what was read is bound to it ([`read`],
//!    [`rewrite`]): the path must still lead to the very file that was read,
//!    the same way, and its size and time of modification must be those it
//!    had. A link that was pointed elsewhere, a file that became a link, was
//!    replaced by another or rewritten in place, is refused, not written.
//! 3. The new contents go to a temporary file in the **same directory**:
//!    `rename(2)` across filesystems fails with `EXDEV` and is not atomic.
//! 4. The temporary file takes the access of the file it replaces: owner,
//!    group, permission bits and ACL, as far as the process may set them
//!    (`access::keep`). It is never open to more than that file was, not
//!    even while it is written. With no file to take them from, it is created
//!    as any new file is, `0666` less the umask — which the system applies:
//!    reading the umask means changing it for the whole process.
//! 5. If `backup` is requested and the file exists, its contents are copied
//!    to `<file>.bak`, next to it and with its access, *before* the rename,
//!    so a failed rename still leaves a recoverable copy on disk.
//! 6. [`tempfile::TempPath::persist`] renames it over the file
//!    (`rename(2)`; `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` on Windows),
//!    right after the path has been checked a last time. Where no file was
//!    found, the rename is one that replaces nothing: what appears in the
//!    meantime is never lost, whatever [`IfExists`] says.
//!
//! An ACL (macOS) or a flag can deny deletion, which renaming a file, or
//! another over it, is too: such a file is refused before anything is made
//! (`deletable`). A new file gets such an ACL once it is in its place, and is
//! its owner's alone until then.
//!
//! What is left unguarded:
//!
//! - the time between the last check and the rename. A link put at the path
//!   in it is replaced, not followed; a directory above it that is swapped
//!   for another, or for a link, *is* followed, at any time: of a path that
//!   is no link only the file it names is compared, not the way there;
//! - a file rewritten in place, to the same size, whose time of modification
//!   stays: the file system keeps it coarsely, or it was set back;
//! - a process that has the old file open, and goes on writing to it.
//!
//! Not kept, because the file is a new one under the old name: extended
//! attributes (the SELinux label among them), NFSv4 ACLs on Linux, file
//! flags, timestamps, and its other hard links, which go on naming the old
//! contents.
//!
//! All I/O failures while writing are wrapped in [`crate::Error::WriteIo`] so
//! the CLI can distinguish read-side and write-side failures when mapping to
//! exit codes.

use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use camino::Utf8Path;
use tempfile::{NamedTempFile, TempPath};

use crate::{Error, Result};

#[cfg(unix)]
mod access;
#[cfg(unix)]
mod acl;
mod deletable;

/// The file a path led to when it was read: what [`rewrite`] is bound to.
#[derive(Debug, Clone)]
pub struct Original {
    /// Where the path led, and the file that was read; `None` when the path
    /// leads to no regular file (a pipe).
    place: Option<Place>,
}

/// Read the file at `path`, following links, and note which file it is.
///
/// # Errors
///
/// [`Error::Io`] as for any read: missing, not permitted, a directory, ...
pub fn read(path: &Utf8Path) -> Result<(Vec<u8>, Original)> {
    let read = || -> io::Result<_> {
        let mut file = File::open(path.as_std_path())?;
        // Before the contents: a change while they are read shows, too.
        let entry = Entry::of(&file.metadata()?);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        // The file is the one that was open, wherever the path leads by now.
        let place = resolve(path.as_std_path(), Links::Follow)
            .ok()
            .map(|place| Place {
                entry: Some(entry),
                ..place
            });
        Ok((bytes, Original { place }))
    };
    read().map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Note which file `path` leads to, without reading it. `None` when it leads
/// to no regular file.
#[must_use]
pub fn locate(path: &Utf8Path) -> Option<Original> {
    let place = resolve(path.as_std_path(), Links::Follow).ok()?;
    place
        .entry
        .is_some()
        .then_some(Original { place: Some(place) })
}

/// Atomically write `content` to `path`, the file `original` was [`read`]
/// from. `false` when it holds exactly that already: it is left alone.
///
/// The file keeps its permission bits, and its owner, group and ACL as far
/// as the process may set them. When `path` is a symbolic link, the file it
/// points to is the one written; the link stays.
///
/// If `backup` is `true`, the previous contents are copied first to
/// `<file>.bak` (always appended, whatever the extension), next to the file
/// itself, not to a link that names it. An existing `.bak` is replaced.
///
/// # Errors
///
/// Returns [`Error::WriteIo`], naming `path`, if any of the following fail:
/// - `path` does not lead to the file that was read any more, or not the
///   same way: it, or a link on the way, was replaced or removed; or the file
///   was rewritten;
/// - the file is not a regular one (a pipe), or may not be deleted;
/// - the file's directory cannot be written (missing, not permitted);
/// - the temporary file cannot be created or written, or cannot take the
///   file's permission bits;
/// - the backup cannot be made, or something other than a regular file is
///   at `<file>.bak`: a link there is neither followed nor replaced;
/// - the final rename fails.
pub fn rewrite(path: &Utf8Path, original: &Original, content: &[u8], backup: bool) -> Result<bool> {
    let rewrite = || {
        let (place, existing) = original.find(path)?;
        if existing.holds(content)? {
            return Ok(false);
        }
        let write = Write {
            name: path,
            links: Links::Follow,
            there: IfExists::replace(backup),
            place,
            existing: Some(existing),
        };
        write.commit(content, None)?;
        Ok(true)
    };
    rewrite().map_err(write_error(path))
}

/// Atomically write `content` to `path`, whatever is there: an existing file
/// as [`rewrite`] does; a new one is created with `0666` less the umask.
///
/// For contents that do not come from the file. An edit is a [`read`] and a
/// [`rewrite`].
///
/// # Errors
///
/// As [`rewrite`]; what must not change is the file `path` leads to when
/// this is called, or that there is none.
pub fn write(path: &Utf8Path, content: &[u8], backup: bool) -> Result<()> {
    Write::to(path, Links::Follow, IfExists::replace(backup))
        .and_then(|write| write.commit(content, None))
        .map_err(write_error(path))
}

/// What [`write_like`] does about a file that is there already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfExists {
    /// Leave it, and fail.
    Refuse,
    /// Replace it.
    Replace,
    /// Copy it to `<file>.bak`, then replace it.
    BackUp,
}

impl IfExists {
    fn replace(backup: bool) -> Self {
        if backup { Self::BackUp } else { Self::Replace }
    }
}

/// As [`write`], for a file that takes the place of another under a new
/// name: `source`, from which `original` was [`read`]. A file that does not
/// exist yet takes the access of that one instead of a new file's. What
/// becomes of an existing one `there` says; replaced, its access stays.
///
/// `path` is a name dq made up, so a symbolic link there is no file the user
/// asked to edit: it is refused, neither written through nor replaced.
///
/// # Errors
///
/// As [`write`]; also [`Error::WriteIo`] when `path` is a symbolic link, or
/// anything is there with [`IfExists::Refuse`], and, naming `source`, when
/// that is not the file that was read any more.
pub fn write_like(
    path: &Utf8Path,
    content: &[u8],
    there: IfExists,
    source: &Utf8Path,
    original: &Original,
) -> Result<()> {
    let (_, like) = original.find(source).map_err(write_error(source))?;
    Write::to(path, Links::Refuse, there)
        .and_then(|write| write.commit(content, Some(&like)))
        .map_err(write_error(path))
}

/// Check, before anything is written, that [`write_like`] would take `path`:
/// nothing is there (`false`), or a regular file (`true`), as the file system
/// tells names apart. Whether that file may stay in the way is for the caller
/// to say with [`IfExists::Refuse`]; with the others it must be one that can
/// be replaced, and backed up.
///
/// # Errors
///
/// [`Error::WriteIo`], as [`write_like`] would have it.
pub fn check_new(path: &Utf8Path, there: IfExists) -> Result<bool> {
    let check = || {
        let place = resolve(path.as_std_path(), Links::Refuse)?;
        if place.entry.is_none() {
            return Ok(false);
        }
        if there != IfExists::Refuse {
            check_replaceable(&place.path)?;
        }
        if there == IfExists::BackUp {
            let at = backup_path_for(&place.path);
            check_backup_place(&at).map_err(|err| not_backed_up(&at, &err))?;
        }
        Ok(true)
    };
    check().map_err(write_error(path))
}

/// Check, before anything is written, that the system will let [`remove`]
/// take `path` away, as far as it tells beforehand: an ACL or a flag may
/// forbid it.
///
/// # Errors
///
/// [`Error::WriteIo`] when it says that it will not.
pub fn check_remove(path: &Utf8Path) -> Result<()> {
    if deletable::may_delete(path.as_std_path()) {
        return Ok(());
    }
    Err(write_error(path)(not_deletable("removed")))
}

/// Remove `path`, from which `original` was [`read`]. Of a link, the link
/// goes, not the file it points to.
///
/// # Errors
///
/// [`Error::WriteIo`] when `path` does not lead to the file that was read
/// any more, or cannot be removed.
pub fn remove(path: &Utf8Path, original: &Original) -> Result<()> {
    original
        .find(path)
        .and_then(|_| fs::remove_file(path.as_std_path()))
        .map_err(write_error(path))
}

/// Wrap an error of writing `path`.
fn write_error(path: &Utf8Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::WriteIo { path, source }
}

fn changed() -> io::Error {
    io::Error::other("it changed while dq was running: it is not the file that was read any more")
}

/// The error of a file that cannot be `done`.
fn not_deletable(done: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("it cannot be {done}: its ACL or flags, or its directory, forbid deleting it"),
    )
}

impl Original {
    /// Check that `path`, from which this was read, led where it did
    /// `before`: to the same path. The file there may be another: one that was
    /// written in the meantime.
    ///
    /// # Errors
    ///
    /// [`Error::WriteIo`], naming `path`, when it led elsewhere.
    pub fn check_way(&self, path: &Utf8Path, before: &Self) -> Result<()> {
        let way = |original: &Self| Some(original.place.as_ref()?.path.clone());
        if way(self) == way(before) {
            return Ok(());
        }
        Err(write_error(path)(changed()))
    }

    /// Find the file again: where `path` leads, and the file there, open.
    fn find(&self, path: &Utf8Path) -> io::Result<(Place, Existing)> {
        let place = match (resolve(path.as_std_path(), Links::Follow), &self.place) {
            (Ok(now), Some(then)) if now == *then => now,
            (Err(err), None) => return Err(err),
            _ => return Err(changed()),
        };
        match Existing::open(&place)? {
            Some(existing) => Ok((place, existing)),
            None => Err(changed()),
        }
    }
}

/// What a file is, whatever it is called, and what tells that it is as it
/// was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    /// The device and the inode.
    #[cfg(unix)]
    id: (u64, u64),
    /// The time of creation, where there is one: the inode of a file that
    /// was deleted may be given to the next one.
    created: Option<SystemTime>,
    /// The size, and the time of modification where there is one: a file
    /// rewritten in place is the same file, but not what was read.
    size: u64,
    modified: Option<SystemTime>,
}

impl Entry {
    fn of(found: &Metadata) -> Self {
        Self {
            #[cfg(unix)]
            id: {
                use std::os::unix::fs::MetadataExt as _;
                (found.dev(), found.ino())
            },
            created: found.created().ok(),
            size: found.len(),
            modified: found.modified().ok(),
        }
    }
}

/// What to do about a link at the last component of a path.
#[derive(Debug, Clone, Copy)]
enum Links {
    Follow,
    Refuse,
}

/// Where a path leads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    /// The path of the file itself: no link in the last component. For a
    /// path that is no link, that path as it was given; for a link, one that
    /// cannot be the link's, so it also tells whether a link led there.
    path: PathBuf,
    /// The regular file there; `None` when there is nothing.
    entry: Option<Entry>,
}

/// Find where `path` leads. One look at `path` decides whether it is a link.
fn resolve(path: &Path, links: Links) -> io::Result<Place> {
    let found = match fs::symlink_metadata(path) {
        Ok(found) => found,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(Place {
                path: path.to_path_buf(),
                entry: None,
            });
        }
        Err(err) => return Err(err),
    };
    // Only a link is made canonical: a plain path stays as it was given, so
    // writing it needs nothing from the directories above.
    let (path, found) = match links {
        _ if !found.is_symlink() => (path.to_path_buf(), found),
        Links::Refuse => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "it is a symbolic link, which is neither written through nor replaced; \
                 remove it or convert the file it points to",
            ));
        }
        Links::Follow => {
            let real = fs::canonicalize(path).map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => io::Error::new(
                    io::ErrorKind::NotFound,
                    "it is a symbolic link to a file that does not exist",
                ),
                _ => err,
            })?;
            let found = fs::symlink_metadata(&real)?;
            (real, found)
        }
    };
    if !found.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it is not a regular file",
        ));
    }
    Ok(Place {
        path,
        entry: Some(Entry::of(&found)),
    })
}

/// A file that is there already.
struct Existing {
    /// Open for reading; `None` when the process may not read it.
    file: Option<File>,
    found: Metadata,
}

impl Existing {
    /// Open the file at `place`, which must be the one found there.
    fn open(place: &Place) -> io::Result<Option<Self>> {
        let Some(entry) = place.entry else {
            return Ok(None);
        };
        let (file, found) = match open_no_follow(&place.path) {
            Ok(file) => {
                let found = file.metadata()?;
                (Some(file), found)
            }
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                (None, fs::symlink_metadata(&place.path)?)
            }
            // Gone, or a link now.
            Err(_) if resolve(&place.path, Links::Refuse).ok().as_ref() != Some(place) => {
                return Err(changed());
            }
            Err(err) => return Err(err),
        };
        if Entry::of(&found) != entry {
            return Err(changed());
        }
        Ok(Some(Self { file, found }))
    }

    fn contents(&self) -> io::Result<Vec<u8>> {
        let Some(mut file) = self.file.as_ref() else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        let mut bytes = Vec::new();
        file.rewind()?;
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Whether the file holds exactly `content`.
    fn holds(&self, content: &[u8]) -> io::Result<bool> {
        let same_size = usize::try_from(self.found.len()) == Ok(content.len());
        Ok(same_size && self.file.is_some() && self.contents()? == content)
    }
}

/// Open `path` to read it, not through a link, and without waiting for the
/// other end of a pipe.
fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

/// A write about to be made.
struct Write<'a> {
    /// What the user called the file.
    name: &'a Utf8Path,
    links: Links,
    there: IfExists,
    /// Where that leads.
    place: Place,
    /// The file there.
    existing: Option<Existing>,
}

impl<'a> Write<'a> {
    fn to(name: &'a Utf8Path, links: Links, there: IfExists) -> io::Result<Self> {
        let place = resolve(name.as_std_path(), links)?;
        if there == IfExists::Refuse && place.entry.is_some() {
            return Err(appeared());
        }
        let existing = Existing::open(&place)?;
        Ok(Self {
            name,
            links,
            there,
            place,
            existing,
        })
    }

    /// Check that the name leads where it led.
    fn check(&self) -> io::Result<()> {
        match resolve(self.name.as_std_path(), self.links) {
            Ok(now) if now == self.place => Ok(()),
            _ => Err(changed()),
        }
    }

    /// Put `content` in the file's place. `like` gives the access of a file
    /// that does not exist yet.
    ///
    /// A file is only replaced when it is the one that was found: where there
    /// was none, the rename makes sure that there still is none.
    fn commit(self, content: &[u8], like: Option<&Existing>) -> io::Result<()> {
        let lost = match &self.existing {
            Some(existing) => self.replace(existing, content),
            None => self.create(content, like),
        }?;
        for what in lost {
            tracing::warn!("{}: {what}", self.name);
        }
        Ok(())
    }

    /// Put `content` in the place of `existing`. What of its access was lost.
    fn replace(&self, existing: &Existing, content: &[u8]) -> io::Result<Vec<&'static str>> {
        let path = &self.place.path;
        check_replaceable(path)?;
        let new = stage(path, content, Some(existing))?;
        if self.there == IfExists::BackUp {
            self.check()?;
            let at = backup_path_for(path);
            back_up(existing, &at).map_err(|err| not_backed_up(&at, &err))?;
        }
        self.check()?;
        new.put(path, &OVER)
    }

    /// Put `content` where nothing is. What of the access of `like` was lost.
    fn create(&self, content: &[u8], like: Option<&Existing>) -> io::Result<Vec<&'static str>> {
        let path = &self.place.path;
        if !deletable::may_delete_in(dir_of(path)?) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "its directory forbids deleting the files in it, and so renaming them",
            ));
        }
        let put = stage(path, content, like)?.put(path, &TO_NEW);
        put.map_err(|err| match err.kind() {
            io::ErrorKind::AlreadyExists => appeared(),
            _ => err,
        })
    }
}

fn appeared() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "a file appeared there while dq was running; it is not replaced",
    )
}

/// Check that the system lets the file at `path`, if any, be replaced.
fn check_replaceable(path: &Path) -> io::Result<()> {
    if deletable::may_delete(path) {
        return Ok(());
    }
    Err(not_deletable("replaced"))
}

/// Check that a backup may go to `at`: nothing is there, or a file that may
/// be replaced.
fn check_backup_place(at: &Path) -> io::Result<()> {
    match fs::symlink_metadata(at) {
        Ok(found) if !found.is_file() => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "something other than a regular file is there",
        )),
        Ok(_) => check_replaceable(at),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn not_backed_up(at: &Path, err: &io::Error) -> io::Error {
    let at = at.to_string_lossy();
    io::Error::new(
        err.kind(),
        format!("cannot back up to '{}': {err}", crate::one_line(&at)),
    )
}

/// Copy the contents and access of `existing` to `at`, atomically.
fn back_up(existing: &Existing, at: &Path) -> io::Result<()> {
    check_backup_place(at)?;
    stage(at, &existing.contents()?, Some(existing))?.put(at, &OVER)?;
    Ok(())
}

/// A temporary file holding `content`, ready to be renamed to `path`: in its
/// directory, with the access of `like` or a new file's.
fn stage(path: &Path, content: &[u8], like: Option<&Existing>) -> io::Result<Temp> {
    let mut temp = Temp::new(create_temp(dir_of(path)?, like)?);
    temp.file.write_all(content)?;
    // After the content: writing clears the set-user-ID and set-group-ID
    // bits, and so does changing the owner.
    if let Some(like) = like {
        temp.lost = keep_access(&temp.file, like)?;
    }
    Ok(temp)
}

fn dir_of(path: &Path) -> io::Result<&Path> {
    match path.parent() {
        Some(dir) if dir.as_os_str().is_empty() => Ok(Path::new(".")),
        Some(dir) => Ok(dir),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic write requires a parent directory; root-only paths are not supported",
        )),
    }
}

/// A temporary file, which goes when it is dropped, unless it has been put
/// where it belongs.
struct Temp {
    file: File,
    /// `None` once it is there.
    path: Option<TempPath>,
    /// What of the access it was to have could not be kept, to tell the user.
    lost: Vec<&'static str>,
}

/// A way to rename a temporary file.
type Way = fn(&mut Temp, &Path) -> io::Result<()>;

/// How to put a file over what is there.
const OVER: [Way; 1] = [Temp::rename];
/// How to put a file where nothing may be, the best way first.
const TO_NEW: [Way; 2] = [Temp::rename_to_new, Temp::claim_and_rename];

impl Temp {
    fn new(temp: NamedTempFile) -> Self {
        let (file, path) = temp.into_parts();
        Self {
            file,
            path: Some(path),
            lost: Vec::new(),
        }
    }

    /// Put it at `path`, in the first of the `ways` that the file system has.
    /// What of its access was lost, by then or on the way.
    fn put(mut self, path: &Path, ways: &[Way]) -> io::Result<Vec<&'static str>> {
        match self.move_to(path, ways) {
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                self.put_without_acl(path, ways, err)?;
            }
            done => done?,
        }
        Ok(std::mem::take(&mut self.lost))
    }

    fn move_to(&mut self, path: &Path, ways: &[Way]) -> io::Result<()> {
        let [way, others @ ..] = ways else {
            return Err(io::ErrorKind::Unsupported.into());
        };
        match way(self, path) {
            Err(err) if !others.is_empty() && self.lacks_way(&err) => self.move_to(path, others),
            done => done,
        }
    }

    /// Put it at `path` once that was `denied`, which may be its own ACL's
    /// doing (macOS): it is renamed without, and is its owner's alone until
    /// it has it again.
    #[cfg(unix)]
    fn put_without_acl(&mut self, path: &Path, ways: &[Way], denied: io::Error) -> io::Result<()> {
        let access = access::Access::of((&self.file.metadata()?).into(), Some(&self.file));
        if !matches!(access.acl, Some(Some(_))) {
            return Err(denied);
        }
        access::withdraw_acl(&self.file, access.mode.bits)?;
        self.move_to(path, ways)?;
        self.lost.extend(match access::keep(&self.file, &access) {
            Ok(kept) => losses(kept),
            Err(_) => vec![LOST_ACL],
        });
        Ok(())
    }

    #[cfg(not(unix))]
    fn put_without_acl(
        &mut self,
        _path: &Path,
        _ways: &[Way],
        denied: io::Error,
    ) -> io::Result<()> {
        Err(denied)
    }

    /// Rename it to `to`, over what is there.
    fn rename(&mut self, to: &Path) -> io::Result<()> {
        self.move_by(|temp| temp.persist(to))
    }

    /// Rename it to `to`, where nothing may be: `AlreadyExists` otherwise.
    fn rename_to_new(&mut self, to: &Path) -> io::Result<()> {
        self.move_by(|temp| temp.persist_noclobber(to))
    }

    /// [`Temp::rename_to_new`] for a file system that has no such rename, and
    /// no hard links either: an empty file, which is only created where
    /// nothing is, takes the name first.
    fn claim_and_rename(&mut self, to: &Path) -> io::Result<()> {
        let claim = Claim::of(to)?;
        self.rename(to).inspect_err(|_| claim.give_back())
    }

    fn move_by(
        &mut self,
        rename: impl FnOnce(TempPath) -> std::result::Result<(), tempfile::PathPersistError>,
    ) -> io::Result<()> {
        let Some(path) = self.path.take() else {
            return Ok(()); // it is there
        };
        rename(path).map_err(|failed| {
            self.path = Some(failed.path);
            failed.error
        })
    }

    /// Whether `err` says that the file system has no such way to rename it:
    /// no rename that replaces nothing, or no hard links.
    #[cfg(unix)]
    fn lacks_way(&mut self, err: &io::Error) -> bool {
        match err.raw_os_error() {
            Some(code) if code == libc::ENOTSUP || code == libc::EOPNOTSUPP => true,
            Some(libc::ENOSYS | libc::EINVAL) => true,
            // Said of hard links, but of a file that may not be renamed at
            // all, too: no name is taken for one of those.
            Some(libc::EPERM) => self.can_be_renamed(),
            _ => false,
        }
    }

    #[cfg(not(unix))]
    fn lacks_way(&mut self, _err: &io::Error) -> bool {
        false
    }

    /// Whether it can be renamed, found out by giving it another name beside
    /// the one it has.
    #[cfg(unix)]
    fn can_be_renamed(&mut self) -> bool {
        let Some(old) = self.path.take() else {
            return false; // it is there
        };
        let moved = dir_of(&old).and_then(|dir| {
            tempfile::Builder::new().make_in(dir, |name| match fs::symlink_metadata(name) {
                Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
                Err(_) => fs::rename(&old, name),
            })
        });
        let Ok(moved) = moved else {
            self.path = Some(old);
            return false;
        };
        // Nothing is at the old name any more.
        drop(old.keep());
        self.path = Some(moved.into_temp_path());
        true
    }
}

#[cfg(unix)]
impl Drop for Temp {
    fn drop(&mut self) {
        // On macOS the ACL it was given may deny deleting it.
        if self.path.is_some()
            && let Ok(Some(_)) = acl::read(&self.file)
        {
            let _ = access::withdraw_acl(&self.file, 0);
        }
    }
}

/// A name taken by an empty file.
struct Claim<'a> {
    path: &'a Path,
    entry: Entry,
}

impl<'a> Claim<'a> {
    /// Take the name `path`; `AlreadyExists` when something has it.
    fn of(path: &'a Path) -> io::Result<Self> {
        let entry = Entry::of(&File::create_new(path)?.metadata()?);
        Ok(Self { path, entry })
    }

    /// Remove the empty file, unless it is another file by now.
    fn give_back(&self) {
        let there = fs::symlink_metadata(self.path);
        if there.is_ok_and(|there| Entry::of(&there) == self.entry) {
            let _ = fs::remove_file(self.path);
        }
    }
}

/// An empty temporary file in `dir`. One to stand in for `like` is open to
/// its owner alone, and to no more than the owner of `like` had.
#[cfg(unix)]
fn create_temp(dir: &Path, like: Option<&Existing>) -> io::Result<NamedTempFile> {
    use std::os::unix::fs::PermissionsExt as _;

    // The mode of `open(2)`, from which the system takes the umask off. The
    // file is open for writing whatever its mode is.
    let mode = like.map_or(0o666, |like| like.found.permissions().mode() & 0o600);
    let file = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(mode))
        .tempfile_in(dir)?;
    if like.is_some() {
        access::strip_acl(file.as_file())?;
    }
    Ok(file)
}

#[cfg(not(unix))]
fn create_temp(dir: &Path, _like: Option<&Existing>) -> io::Result<NamedTempFile> {
    NamedTempFile::new_in(dir)
}

/// Give `file` the access of `like`; what the process may not give it, to
/// tell the user.
#[cfg(unix)]
fn keep_access(file: &File, like: &Existing) -> io::Result<Vec<&'static str>> {
    let like = access::Access::of((&like.found).into(), like.file.as_ref());
    access::keep(file, &like).map(losses)
}

#[cfg(unix)]
const LOST_ACL: &str = "its ACL could not be kept, so only its owner has access";

/// What was not kept, to tell the user.
#[cfg(unix)]
fn losses(kept: access::Kept) -> Vec<&'static str> {
    let mut lost = Vec::new();
    if !kept.owner {
        lost.push("its owner or group could not be kept");
    }
    if !kept.bits {
        lost.push("its permission bits could not all be kept");
    }
    if !kept.acl {
        lost.push(LOST_ACL);
    }
    lost
}

#[cfg(not(unix))]
fn keep_access(_file: &File, _like: &Existing) -> io::Result<Vec<&'static str>> {
    Ok(Vec::new())
}

/// The backup path of `path`: always the whole name plus `.bak`. So
/// `foo.yaml` → `foo.yaml.bak`, `foo` → `foo.bak`, and `foo.bak` →
/// `foo.bak.bak`.
fn backup_path_for(path: &Path) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(".bak");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;

    #[test]
    fn write_creates_new_file_with_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target =
            Utf8PathBuf::from_path_buf(dir.path().join("out.txt")).expect("utf-8 tempdir path");

        write(&target, b"hello, world", false).expect("write succeeds");

        let on_disk = std::fs::read(target.as_std_path()).expect("read back");
        assert_eq!(on_disk, b"hello, world");
    }

    #[test]
    fn write_with_backup_preserves_old_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target =
            Utf8PathBuf::from_path_buf(dir.path().join("out.txt")).expect("utf-8 tempdir path");

        std::fs::write(target.as_std_path(), b"old contents").expect("seed");

        write(&target, b"new contents", true).expect("write succeeds");

        let on_disk = std::fs::read(target.as_std_path()).expect("read back");
        assert_eq!(on_disk, b"new contents");

        let backup = Utf8PathBuf::from(format!("{target}.bak"));
        let backup_contents = std::fs::read(backup.as_std_path()).expect("read backup");
        assert_eq!(backup_contents, b"old contents");
    }

    #[test]
    fn write_without_backup_leaves_no_bak_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target =
            Utf8PathBuf::from_path_buf(dir.path().join("out.txt")).expect("utf-8 tempdir path");

        std::fs::write(target.as_std_path(), b"old").expect("seed");
        write(&target, b"new", false).expect("write succeeds");

        let backup = Utf8PathBuf::from(format!("{target}.bak"));
        assert!(!backup.exists(), "no backup expected when backup=false");
    }

    #[test]
    fn backup_path_for_always_appends_bak() {
        for (path, bak) in [
            ("foo.yaml", "foo.yaml.bak"),
            ("foo.bak", "foo.bak.bak"),
            ("foo", "foo.bak"),
            ("dir/foo.tar.gz", "dir/foo.tar.gz.bak"),
        ] {
            assert_eq!(backup_path_for(Path::new(path)), PathBuf::from(bak));
        }
    }

    /// Every name in `dir`, sorted: nothing but what a test put there.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read_dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn utf8(path: &Path) -> &Utf8Path {
        Utf8Path::from_path(path).expect("utf-8 path")
    }

    /// The path and the cause of the write error `result` must be.
    fn write_error(result: Result<()>) -> (Utf8PathBuf, String) {
        match result.expect_err("the write must fail") {
            Error::WriteIo { path, source } => (path, source.to_string()),
            other => panic!("expected WriteIo, got {other:?}"),
        }
    }

    #[test]
    fn writing_a_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).expect("mkdir");

        let (path, cause) = write_error(write(utf8(&sub), b"new", true));

        assert_eq!(path, utf8(&sub));
        assert_eq!(cause, "it is not a regular file");
        assert_eq!(names(dir.path()), ["sub"]);
    }

    #[test]
    fn a_missing_directory_is_a_write_error_naming_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("no/such/out.txt");

        let (path, _) = write_error(write(utf8(&target), b"new", false));

        assert_eq!(path, utf8(&target));
        assert!(names(dir.path()).is_empty());
    }

    /// Modes and links. No test depends on the umask: a mode is set with
    /// `chmod`, or compared with that of a file created next to it.
    #[cfg(unix)]
    mod unix {
        use std::fs::File;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, chown, symlink};
        use std::process::Command;

        use super::*;

        fn mode(path: &Path) -> u32 {
            fs::symlink_metadata(path).expect("lstat").mode() & 0o7777
        }

        /// A file holding `content` with `mode`.
        fn seed(path: &Path, content: &str, mode: u32) {
            fs::write(path, content).expect("seed");
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
        }

        fn read(path: &Path) -> String {
            fs::read_to_string(path).expect("read")
        }

        /// [`write_like`] "new" to `path`, for the file `source`.
        fn like(path: &Path, source: &Path) -> Result<()> {
            let (_, original) = super::super::read(utf8(source)).expect("read");
            let there = IfExists::BackUp;
            write_like(utf8(path), b"new", there, utf8(source), &original)
        }

        fn assert_link(path: &Path, to: &str) {
            assert_eq!(
                fs::read_link(path).expect("still a link"),
                PathBuf::from(to)
            );
        }

        #[test]
        fn an_existing_file_keeps_its_mode() {
            let dir = tempfile::tempdir().expect("tempdir");
            for bits in [
                0o600, 0o644, 0o664, 0o755, 0o444, 0o400, 0o640, 0o606, 0o777, 0o4755, 0o2755,
                0o1644, 0o7711,
            ] {
                let target = dir.path().join(format!("{bits:o}"));
                seed(&target, "old", bits);
                // A system may refuse some of the special bits.
                let before = mode(&target);

                write(utf8(&target), b"new", false).expect("write");

                assert_eq!(read(&target), "new");
                assert_eq!(mode(&target), before, "{bits:o}");
            }
        }

        #[test]
        fn a_new_file_has_the_mode_of_any_new_file() {
            let dir = tempfile::tempdir().expect("tempdir");
            let plain = dir.path().join("plain");
            File::create(&plain).expect("create");
            let target = dir.path().join("out");

            write(utf8(&target), b"new", true).expect("write");

            assert_eq!(mode(&target), mode(&plain));
            assert_eq!(names(dir.path()), ["out", "plain"]);
        }

        #[test]
        fn the_temporary_file_is_never_more_open_than_the_file_it_replaces() {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("out");
            seed(&target, "old", 0o644);
            for bits in [0o777, 0o644, 0o444, 0o400, 0o040, 0o000] {
                fs::set_permissions(&target, fs::Permissions::from_mode(bits)).expect("chmod");
                let like = Existing {
                    file: None,
                    found: fs::metadata(&target).expect("stat"),
                };

                let temp = create_temp(dir.path(), Some(&like)).expect("temp");

                let open = mode(temp.path());
                assert_eq!(open & !(bits & 0o600), 0, "{bits:o}: {open:o}");
            }
        }

        /// Whether `path` can be given the group `gid`, which it then has.
        fn set_group(path: &Path, gid: u32) -> bool {
            chown(path, None, Some(gid)).is_ok()
                && fs::metadata(path).is_ok_and(|found| found.gid() == gid)
        }

        #[test]
        fn a_group_the_process_belongs_to_is_kept() {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("out");
            seed(&target, "old", 0o660);
            let default = fs::metadata(&target).expect("stat").gid();
            let groups = Command::new("id").arg("-G").output().expect("id -G");
            let other = String::from_utf8(groups.stdout)
                .expect("ids")
                .split_whitespace()
                .filter_map(|gid| gid.parse().ok())
                .find(|&gid: &u32| gid != default && set_group(&target, gid));
            // A user with one group has no other to give it.
            let Some(other) = other else { return };
            fs::set_permissions(&target, fs::Permissions::from_mode(0o660)).expect("chmod");

            write(utf8(&target), b"new", true).expect("write");

            for path in [target, dir.path().join("out.bak")] {
                assert_eq!(fs::metadata(&path).expect("stat").gid(), other);
                assert_eq!(mode(&path), 0o660);
            }
        }

        /// On macOS a new file has the group of its directory, `wheel` for
        /// `/tmp`, whoever makes it: that is how a user gets a file of a
        /// group they are not in. (Linux has no such way without root.)
        #[cfg(target_os = "macos")]
        #[test]
        fn a_group_that_cannot_be_kept_costs_the_group_its_access() {
            let shared = tempfile::tempdir_in("/private/tmp").expect("tempdir");
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("out");
            let made = shared.path().join("out");
            seed(&made, "old", 0o640);
            let foreign = fs::metadata(&made).expect("stat").gid();
            fs::rename(&made, &target).expect("same volume");
            let probe = dir.path().join("probe");
            File::create(&probe).expect("create");
            if set_group(&probe, foreign) || fs::metadata(&probe).expect("stat").gid() == foreign {
                return; // root, or a member of it
            }

            write(utf8(&target), b"new", false).expect("not being allowed is no error");

            assert_ne!(fs::metadata(&target).expect("stat").gid(), foreign);
            assert_eq!(mode(&target), 0o600, "the new group must not gain access");
        }

        #[test]
        fn a_link_stays_and_the_file_it_points_to_is_written() {
            let dir = tempfile::tempdir().expect("tempdir");
            let other = dir.path().join("other");
            let links = dir.path().join("links");
            fs::create_dir(&other).expect("mkdir");
            fs::create_dir(&links).expect("mkdir");
            let real = other.join("real");
            let absolute = real.to_str().expect("utf-8").to_owned();
            for (name, to) in [
                ("relative", "../other/real"),
                ("absolute", absolute.as_str()),
                ("chain", "chain-1"),
                ("chain-1", "absolute"),
            ] {
                symlink(to, links.join(name)).expect("symlink");
            }
            for (name, to) in [
                ("relative", "../other/real"),
                ("absolute", absolute.as_str()),
                ("chain", "chain-1"),
            ] {
                // Through a directory that is itself a link, too.
                let _ = fs::remove_file(dir.path().join("linked-dir"));
                symlink("links", dir.path().join("linked-dir")).expect("symlink");
                for at in ["links", "linked-dir"] {
                    seed(&real, "old", 0o754);

                    let link = dir.path().join(at).join(name);
                    write(utf8(&link), b"new", false).expect("write");

                    assert_link(&link, to);
                    assert_eq!(read(&real), "new", "{at}/{name}");
                    assert_eq!(mode(&real), 0o754, "{at}/{name}");
                }
            }
            assert_eq!(names(&other), ["real"]);
            assert_eq!(names(&links), ["absolute", "chain", "chain-1", "relative"]);
        }

        #[test]
        fn a_link_that_names_no_regular_file_is_refused_and_stays() {
            let dir = tempfile::tempdir().expect("tempdir");
            let at = |name: &str| dir.path().join(name);
            fs::create_dir(at("dir")).expect("mkdir");
            let fifo = Command::new("mkfifo").arg(at("fifo")).status();
            assert!(fifo.expect("mkfifo").success());
            let links = [
                ("dangling", "nowhere"),
                ("dangling-dir", "no/such/dir/file"),
                ("loop", "loop-1"),
                ("loop-1", "loop"),
                ("self", "self"),
                ("to-dir", "dir"),
                ("to-fifo", "fifo"),
            ];
            for (name, to) in links {
                symlink(to, at(name)).expect("symlink");
            }
            let before = names(dir.path());
            let dangling = "it is a symbolic link to a file that does not exist";
            for (name, cause) in [
                ("dangling", Some(dangling)),
                ("dangling-dir", Some(dangling)),
                // The system's own words.
                ("loop", None),
                ("self", None),
                ("to-dir", Some("it is not a regular file")),
                ("to-fifo", Some("it is not a regular file")),
                ("fifo", Some("it is not a regular file")),
            ] {
                let (path, found) = write_error(write(utf8(&at(name)), b"new", true));

                assert_eq!(path, utf8(&at(name)), "the name the user gave");
                if let Some(cause) = cause {
                    assert_eq!(found, cause, "{name}");
                }
            }
            for (name, to) in links {
                assert_link(&at(name), to);
            }
            assert_eq!(names(dir.path()), before, "nothing made, nothing left");
        }

        #[test]
        fn a_backup_is_of_the_file_itself_with_its_mode() {
            let dir = tempfile::tempdir().expect("tempdir");
            let other = dir.path().join("other");
            fs::create_dir(&other).expect("mkdir");
            let real = other.join("real.yaml");
            seed(&real, "old", 0o640);
            let link = dir.path().join("link.yaml");
            symlink("other/real.yaml", &link).expect("symlink");
            // One from before, open to more than the file and read-only.
            seed(&other.join("real.yaml.bak"), "older", 0o444);

            write(utf8(&link), b"new", true).expect("write");

            assert_link(&link, "other/real.yaml");
            assert_eq!(names(dir.path()), ["link.yaml", "other"]);
            assert_eq!(names(&other), ["real.yaml", "real.yaml.bak"]);
            assert_eq!((read(&real).as_str(), mode(&real)), ("new", 0o640));
            let bak = other.join("real.yaml.bak");
            assert_eq!((read(&bak).as_str(), mode(&bak)), ("old", 0o640));
        }

        #[test]
        fn a_backup_never_goes_through_a_link_or_in_place_of_one() {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("out");
            let victim = dir.path().join("victim");
            let bak = dir.path().join("out.bak");
            seed(&victim, "victim", 0o644);
            seed(&target, "old", 0o644);
            for to in ["victim", "nowhere", "out"] {
                symlink(to, &bak).expect("symlink");

                let (path, cause) = write_error(write(utf8(&target), b"new", true));

                assert_eq!(path, utf8(&target));
                assert_eq!(
                    cause,
                    format!(
                        "cannot back up to '{}': something other than a regular file is there",
                        bak.display()
                    )
                );
                assert_link(&bak, to);
                assert_eq!(read(&target), "old", "not written without its backup");
                assert_eq!(read(&victim), "victim");
                assert_eq!(names(dir.path()), ["out", "out.bak", "victim"]);
                fs::remove_file(&bak).expect("unlink");
            }
        }

        #[test]
        fn a_file_like_another_takes_its_mode() {
            let dir = tempfile::tempdir().expect("tempdir");
            let at = |name: &str| dir.path().join(name);
            seed(&at("real"), "source", 0o751);
            symlink("real", at("link")).expect("symlink");
            for source in ["real", "link"] {
                let new = at(&format!("from-{source}"));

                like(&new, &at(source)).expect("write");

                assert_eq!((read(&new).as_str(), mode(&new)), ("new", 0o751));
            }
            // One that is there keeps its own, and is backed up.
            seed(&at("there"), "old", 0o604);
            like(&at("there"), &at("real")).expect("write");
            assert_eq!(
                (read(&at("there")).as_str(), mode(&at("there"))),
                ("new", 0o604)
            );
            let bak = at("there.bak");
            assert_eq!((read(&bak).as_str(), mode(&bak)), ("old", 0o604));
        }

        #[test]
        fn a_file_like_another_is_never_written_to_a_link() {
            let dir = tempfile::tempdir().expect("tempdir");
            let at = |name: &str| dir.path().join(name);
            seed(&at("source"), "source", 0o644);
            seed(&at("victim"), "victim", 0o644);
            for to in ["victim", "nowhere", "source", "out"] {
                symlink(to, at("out")).expect("symlink");

                let (path, cause) = write_error(like(&at("out"), &at("source")));

                assert_eq!(path, utf8(&at("out")));
                assert!(cause.starts_with("it is a symbolic link"), "{cause}");
                assert_link(&at("out"), to);
                assert_eq!(read(&at("victim")), "victim");
                assert_eq!(read(&at("source")), "source");
                assert_eq!(names(dir.path()), ["out", "source", "victim"]);
                fs::remove_file(at("out")).expect("unlink");
            }
        }

        #[test]
        fn a_source_that_is_not_the_file_that_was_read_is_refused() {
            let dir = tempfile::tempdir().expect("tempdir");
            let at = |name: &str| dir.path().join(name);
            seed(&at("source"), "source", 0o644);
            let (_, original) = super::super::read(utf8(&at("source"))).expect("read");
            seed(&at("victim"), "victim", 0o600);
            fs::remove_file(at("source")).expect("rm");
            for swap in [None, Some("victim")] {
                if let Some(to) = swap {
                    symlink(to, at("source")).expect("symlink");
                }
                for result in [
                    write_like(
                        utf8(&at("out")),
                        b"new",
                        IfExists::BackUp,
                        utf8(&at("source")),
                        &original,
                    ),
                    remove(utf8(&at("source")), &original),
                ] {
                    let (path, cause) = write_error(result);
                    assert_eq!(path, utf8(&at("source")));
                    assert_eq!(cause, CHANGED);
                }
            }
            assert_link(&at("source"), "victim");
            assert_eq!(names(dir.path()), ["source", "victim"]);
        }

        const CHANGED: &str =
            "it changed while dq was running: it is not the file that was read any more";

        /// [`rewrite`] what `swap` did to the directory after the file was
        /// read by the name `file.yaml`.
        fn rewrite_after(dir: &Path, swap: impl FnOnce(&Path)) -> Result<bool> {
            let file = dir.join("file.yaml");
            let (_, original) = super::super::read(utf8(&file)).expect("read");
            swap(dir);
            rewrite(utf8(&file), &original, b"new", true)
        }

        #[test]
        fn a_file_replaced_after_it_was_read_is_not_written() {
            type Swap = fn(&Path);
            let swaps: [(&str, Swap); 6] = [
                ("by a link", |dir| {
                    fs::remove_file(dir.join("file.yaml")).expect("rm");
                    symlink("victim", dir.join("file.yaml")).expect("symlink");
                }),
                // The same file, but by way of a link.
                ("by a link to itself", |dir| {
                    fs::rename(dir.join("file.yaml"), dir.join("moved")).expect("mv");
                    symlink("moved", dir.join("file.yaml")).expect("symlink");
                }),
                ("by a file", |dir| {
                    fs::rename(dir.join("victim"), dir.join("file.yaml")).expect("mv");
                }),
                ("by nothing", |dir| {
                    fs::remove_file(dir.join("file.yaml")).expect("rm");
                }),
                ("by a directory", |dir| {
                    fs::remove_file(dir.join("file.yaml")).expect("rm");
                    fs::create_dir(dir.join("file.yaml")).expect("mkdir");
                }),
                ("by a pipe", |dir| {
                    fs::remove_file(dir.join("file.yaml")).expect("rm");
                    let made = Command::new("mkfifo").arg(dir.join("file.yaml")).status();
                    assert!(made.expect("mkfifo").success());
                }),
            ];
            for (what, swap) in swaps {
                let dir = tempfile::tempdir().expect("tempdir");
                seed(&dir.path().join("file.yaml"), "old", 0o644);
                seed(&dir.path().join("victim"), "victim", 0o600);
                let after = std::cell::RefCell::new(Vec::new());

                let result = rewrite_after(dir.path(), |dir| {
                    swap(dir);
                    after.replace(names(dir));
                });

                let (path, cause) = write_error(result.map(drop));
                assert_eq!(path, utf8(&dir.path().join("file.yaml")));
                assert_eq!(cause, CHANGED, "{what}");
                assert_eq!(names(dir.path()), after.into_inner(), "{what}");
                let victim = dir.path().join("victim");
                if victim.exists() {
                    assert_eq!((read(&victim).as_str(), mode(&victim)), ("victim", 0o600));
                }
            }
        }

        #[test]
        fn a_link_pointed_elsewhere_after_the_read_is_not_followed() {
            type Swap = fn(&Path);
            let swaps: [(&str, Swap); 5] = [
                ("the link", |dir| {
                    fs::remove_file(dir.join("file.yaml")).expect("rm");
                    symlink("two/real", dir.join("file.yaml")).expect("symlink");
                }),
                ("a link on the way", |dir| {
                    fs::remove_file(dir.join("current")).expect("rm");
                    symlink("two", dir.join("current")).expect("symlink");
                }),
                ("the link, for the file", |dir| {
                    fs::remove_file(dir.join("file.yaml")).expect("rm");
                    fs::hard_link(dir.join("one/real"), dir.join("file.yaml")).expect("link");
                }),
                ("the file", |dir| {
                    fs::rename(dir.join("one/real"), dir.join("moved")).expect("mv");
                    fs::write(dir.join("one/real"), "other").expect("write");
                }),
                ("the file, for a link", |dir| {
                    fs::remove_file(dir.join("one/real")).expect("rm");
                    symlink("../two/real", dir.join("one/real")).expect("symlink");
                }),
            ];
            for (what, swap) in swaps {
                let dir = tempfile::tempdir().expect("tempdir");
                for release in ["one", "two"] {
                    fs::create_dir(dir.path().join(release)).expect("mkdir");
                    seed(&dir.path().join(release).join("real"), release, 0o644);
                }
                symlink("one", dir.path().join("current")).expect("symlink");
                symlink("current/real", dir.path().join("file.yaml")).expect("symlink");

                let result = rewrite_after(dir.path(), swap);

                let (path, cause) = write_error(result.map(drop));
                assert_eq!(path, utf8(&dir.path().join("file.yaml")));
                assert_eq!(cause, CHANGED, "{what}");
                assert_eq!(read(&dir.path().join("two/real")), "two", "{what}");
                assert_eq!(names(&dir.path().join("one")), ["real"], "{what}");
                assert_eq!(names(&dir.path().join("two")), ["real"], "{what}");
            }
        }

        /// The last look is right before the rename, and before the backup.
        #[test]
        fn a_file_replaced_while_it_is_written_is_refused() {
            for backup in [false, true] {
                for existing in [true, false] {
                    let dir = tempfile::tempdir().expect("tempdir");
                    let file = dir.path().join("file.yaml");
                    seed(&dir.path().join("victim"), "victim", 0o644);
                    if existing {
                        seed(&file, "old", 0o644);
                    }
                    let there = IfExists::replace(backup);
                    let write = Write::to(utf8(&file), Links::Follow, there).expect("found");
                    let _ = fs::remove_file(&file);
                    symlink("victim", &file).expect("symlink");

                    let err = write.commit(b"new", None).expect_err("refused");

                    let cause = if existing { CHANGED } else { APPEARED };
                    assert_eq!(err.to_string(), cause);
                    assert_link(&file, "victim");
                    assert_eq!(read(&dir.path().join("victim")), "victim");
                    assert_eq!(names(dir.path()), ["file.yaml", "victim"]);
                }
            }
        }

        #[test]
        fn another_link_to_the_file_that_was_read_is_written() {
            let dir = tempfile::tempdir().expect("tempdir");
            seed(&dir.path().join("real"), "old", 0o640);
            symlink("real", dir.path().join("file.yaml")).expect("symlink");

            let written = rewrite_after(dir.path(), |dir| {
                fs::remove_file(dir.join("file.yaml")).expect("rm");
                symlink("./real", dir.join("file.yaml")).expect("symlink");
            });

            assert!(written.expect("the same file"));
            let real = dir.path().join("real");
            assert_eq!((read(&real).as_str(), mode(&real)), ("new", 0o640));
            assert_link(&dir.path().join("file.yaml"), "./real");
        }

        #[test]
        fn a_file_that_holds_the_contents_already_is_left_alone() {
            let dir = tempfile::tempdir().expect("tempdir");
            let file = dir.path().join("file.yaml");
            seed(&file, "new", 0o444);
            let before = fs::metadata(&file).expect("stat");

            assert!(!rewrite_after(dir.path(), |_| ()).expect("no change"));

            let after = fs::metadata(&file).expect("stat");
            assert_eq!(
                (after.ino(), after.mtime_nsec()),
                (before.ino(), before.mtime_nsec())
            );
            assert_eq!(names(dir.path()), ["file.yaml"]);
        }

        #[test]
        fn a_pipe_is_read_and_never_written() {
            let dir = tempfile::tempdir().expect("tempdir");
            let pipe = dir.path().join("pipe");
            let made = Command::new("mkfifo").arg(&pipe).status();
            assert!(made.expect("mkfifo").success());
            let writer = {
                let pipe = pipe.clone();
                std::thread::spawn(move || fs::write(pipe, "old").expect("write"))
            };

            let (bytes, original) = super::super::read(utf8(&pipe)).expect("read");

            writer.join().expect("writer");
            assert_eq!(bytes, b"old");
            let (_, cause) = write_error(rewrite(utf8(&pipe), &original, b"new", true).map(drop));
            assert_eq!(cause, "it is not a regular file");
            assert_eq!(names(dir.path()), ["pipe"]);
        }

        #[test]
        fn a_new_file_may_go_where_nothing_or_a_regular_file_is() {
            let dir = tempfile::tempdir().expect("tempdir");
            let at = |name: &str| dir.path().join(name);
            seed(&at("file"), "", 0o644);
            fs::create_dir(at("dir")).expect("mkdir");
            symlink("file", at("link")).expect("symlink");
            symlink("nowhere", at("dangling")).expect("symlink");
            for (name, there) in [
                ("nothing", Some(false)),
                ("file", Some(true)),
                ("dir", None),
                ("link", None),
                ("dangling", None),
            ] {
                for what in [IfExists::Refuse, IfExists::Replace, IfExists::BackUp] {
                    assert_eq!(check_new(utf8(&at(name)), what).ok(), there, "{name}");
                }
            }

            // A file that is to be backed up must be able to be.
            fs::create_dir(at("file.bak")).expect("mkdir");
            for (what, refused) in [
                (IfExists::Refuse, false),
                (IfExists::Replace, false),
                (IfExists::BackUp, true),
            ] {
                let checked = check_new(utf8(&at("file")), what);
                assert_eq!(checked.is_err(), refused, "{what:?}");
                if refused {
                    let (path, cause) = write_error(checked.map(drop));
                    assert_eq!(path, utf8(&at("file")));
                    // As the write itself has it.
                    let written = write(utf8(&at("file")), b"new", true);
                    assert_eq!(write_error(written).1, cause);
                    assert!(cause.ends_with("something other than a regular file is there"));
                }
            }
        }

        /// A link is refused as what it is, the rest as what appeared.
        #[test]
        fn a_new_file_never_takes_the_place_of_another() {
            let dir = tempfile::tempdir().expect("tempdir");
            let at = |name: &str| dir.path().join(name);
            seed(&at("source"), "source", 0o644);
            let (_, original) = super::super::read(utf8(&at("source"))).expect("read");
            seed(&at("out"), "old", 0o640);

            let result = write_like(
                utf8(&at("out")),
                b"new",
                IfExists::Refuse,
                utf8(&at("source")),
                &original,
            );

            let (path, cause) = write_error(result);
            assert_eq!(path, utf8(&at("out")));
            assert_eq!(cause, APPEARED);
            assert_eq!(
                (read(&at("out")).as_str(), mode(&at("out"))),
                ("old", 0o640)
            );
            assert_eq!(names(dir.path()), ["out", "source"]);
        }

        const APPEARED: &str = "a file appeared there while dq was running; it is not replaced";

        /// What appears after the last look, which the rename must see,
        /// whatever may be done to a file that is there before.
        #[test]
        fn a_file_that_appears_while_a_new_one_is_written_is_not_replaced() {
            type Make = fn(&Path);
            let made: [(&str, Make); 4] = [
                ("a file", |out| fs::write(out, "other").expect("write")),
                ("a link", |out| symlink("source", out).expect("symlink")),
                ("a dangling link", |out| {
                    symlink("nowhere", out).expect("symlink");
                }),
                ("a directory", |out| fs::create_dir(out).expect("mkdir")),
            ];
            let ways = [
                // `write_like`, and `write`.
                (Links::Refuse, IfExists::Refuse),
                (Links::Refuse, IfExists::Replace),
                (Links::Refuse, IfExists::BackUp),
                (Links::Follow, IfExists::Replace),
                (Links::Follow, IfExists::BackUp),
            ];
            for (what, make) in made {
                for (links, there) in ways {
                    let dir = tempfile::tempdir().expect("tempdir");
                    let at = |name: &str| dir.path().join(name);
                    seed(&at("source"), "source", 0o644);
                    let out = at("out");
                    let write = Write::to(utf8(&out), links, there).expect("nothing there");
                    make(&out);
                    let before = fs::symlink_metadata(&out).expect("lstat");

                    let err = write.commit(b"new", None).expect_err("refused");

                    assert_eq!(err.to_string(), APPEARED, "{what} {there:?}");
                    let after = fs::symlink_metadata(&out).expect("lstat");
                    assert_eq!(after.ino(), before.ino(), "{what} {there:?}");
                    assert_eq!(read(&at("source")), "source", "{what} {there:?}");
                    if before.is_file() {
                        assert_eq!(read(&out), "other");
                    }
                    assert_eq!(names(dir.path()), ["out", "source"], "{what} {there:?}");
                }
            }
        }

        fn temp_in(dir: &Path, content: &str) -> Temp {
            let mut temp = Temp::new(create_temp(dir, None).expect("temp"));
            temp.file.write_all(content.as_bytes()).expect("write");
            temp
        }

        #[test]
        fn a_claimed_name_is_taken_or_given_back() {
            let dir = tempfile::tempdir().expect("tempdir");
            let out = dir.path().join("out");

            let mut temp = temp_in(dir.path(), "new");
            temp.claim_and_rename(&out).expect("nothing is there");
            assert_eq!(read(&out), "new");
            drop(temp);
            assert_eq!(names(dir.path()), ["out"]);

            let mut temp = temp_in(dir.path(), "newer");
            let err = temp.claim_and_rename(&out).expect_err("refused");
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(read(&out), "new");
            drop(temp);
            assert_eq!(names(dir.path()), ["out"]);

            // The rename fails: the name is free again.
            let mut temp = temp_in(dir.path(), "newer");
            fs::remove_file(temp.path.as_ref().expect("not renamed")).expect("rm");
            let other = dir.path().join("other");
            temp.claim_and_rename(&other).expect_err("gone");
            assert_eq!(names(dir.path()), ["out"]);

            // Unless another file has it by then.
            let claim = Claim::of(&other).expect("free");
            fs::rename(&out, &other).expect("mv");
            claim.give_back();
            assert_eq!(read(&other), "new");
            assert_eq!(names(dir.path()), ["other"]);
        }

        fn unsupported(_temp: &mut Temp, _to: &Path) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::ENOTSUP))
        }

        fn not_permitted(_temp: &mut Temp, _to: &Path) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::EPERM))
        }

        fn denied(_temp: &mut Temp, _to: &Path) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::EACCES))
        }

        #[test]
        fn the_next_way_is_taken_where_the_file_system_lacks_one() {
            let ways: [(&[Way], Option<i32>); 5] = [
                (&TO_NEW, None),
                (&[unsupported, Temp::claim_and_rename], None),
                // Of hard links: the file can be renamed.
                (&[not_permitted, Temp::claim_and_rename], None),
                (&[denied, Temp::claim_and_rename], Some(libc::EACCES)),
                (&[unsupported], Some(libc::ENOTSUP)),
            ];
            for (ways, fails) in ways {
                let dir = tempfile::tempdir().expect("tempdir");
                let out = dir.path().join("out");

                let result = temp_in(dir.path(), "new").put(&out, ways);

                match fails {
                    None => {
                        assert!(result.expect("put").is_empty());
                        assert_eq!(read(&out), "new");
                        assert_eq!(names(dir.path()), ["out"]);
                    }
                    Some(code) => {
                        let err = result.expect_err("refused");
                        assert_eq!(err.raw_os_error(), Some(code));
                        assert!(names(dir.path()).is_empty());
                    }
                }
            }
        }

        /// The file system has the way, but not for a file that may not be
        /// renamed: then no name is taken that could not be given back.
        #[cfg(target_os = "macos")]
        #[test]
        fn no_name_is_claimed_for_a_file_that_may_not_be_renamed() {
            let dir = tempfile::tempdir().expect("tempdir");
            let out = dir.path().join("out");
            let mut temp = temp_in(dir.path(), "new");
            let at = temp.path.as_ref().expect("a path").to_path_buf();
            let flag = |flag: &str| {
                let done = Command::new("/usr/bin/chflags").arg(flag).arg(&at).status();
                assert!(done.expect("chflags").success());
            };
            flag("uchg");

            let result = temp.move_to(&out, &TO_NEW);
            let next = temp.move_to(&out, &[Temp::rename_to_new, unsupported]);

            let left = names(dir.path());
            flag("nouchg");
            for result in [result, next] {
                let err = result.expect_err("refused");
                assert_eq!(err.raw_os_error(), Some(libc::EPERM));
            }
            assert_eq!(left, [at.file_name().expect("a name").to_string_lossy()]);
            drop(temp);
            assert!(names(dir.path()).is_empty());
        }

        /// `FS_IMMUTABLE_FL` and `FS_APPEND_FL` of `linux/fs.h`.
        #[cfg(target_os = "linux")]
        const IMMUTABLE: libc::c_int = 0x10;
        #[cfg(target_os = "linux")]
        const APPEND: libc::c_int = 0x20;

        /// `chattr =flags path`, as far as these two go; `false` when the process may not, or the file system cannot.
        #[cfg(target_os = "linux")]
        fn set_flags(path: &Path, flags: libc::c_int) -> bool {
            use std::os::fd::AsRawFd as _;

            const FOR_USERS: libc::c_int = IMMUTABLE | APPEND;
            let file = File::open(path).expect("open");
            let mut now: libc::c_int = 0;
            // SAFETY: the descriptor is open, and both requests take an `int`.
            unsafe {
                if libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETFLAGS, &raw mut now) != 0 {
                    return false;
                }
                let flags = (now & !FOR_USERS) | flags;
                libc::ioctl(file.as_raw_fd(), libc::FS_IOC_SETFLAGS, &raw const flags) == 0
            }
        }

        /// What `chattr` forbids, root too: only root can set it, and not on
        /// every file system.
        #[cfg(target_os = "linux")]
        #[test]
        fn what_a_flag_forbids_is_refused_and_nothing_is_left() {
            let root = tempfile::tempdir().expect("tempdir");
            let source = root.path().join("source");
            seed(&source, "source", 0o644);
            let dir = root.path().join("dir");
            fs::create_dir(&dir).expect("mkdir");
            let file = dir.join("file.yaml");
            seed(&file, "old", 0o644);
            for (flag, flagged) in [(IMMUTABLE, &file), (APPEND, &file), (APPEND, &dir)] {
                if !set_flags(flagged, flag) {
                    return;
                }
                let replaced = [
                    rewrite_after(&dir, |_| ()).map(drop),
                    write(utf8(&file), b"new", true),
                    check_new(utf8(&file), IfExists::Replace).map(drop),
                ];
                let removable = check_remove(utf8(&file));
                // Next to a flagged file, a new one may be made.
                let made = like(&dir.join("out"), &source);
                let in_dir = names(&dir);
                assert!(set_flags(flagged, 0));

                for result in replaced.into_iter().chain([removable]) {
                    let (_, cause) = write_error(result);
                    assert!(
                        cause.ends_with("or its directory, forbid deleting it"),
                        "{cause}"
                    );
                }
                assert_eq!(read(&file), "old");
                if flagged == &dir {
                    let (_, cause) = write_error(made);
                    assert!(cause.starts_with("its directory forbids"), "{cause}");
                    assert_eq!(in_dir, ["file.yaml"]);
                } else {
                    made.expect("written");
                    assert_eq!(in_dir, ["file.yaml", "out"]);
                    fs::remove_file(dir.join("out")).expect("rm");
                }
            }
            check_remove(utf8(&file)).expect("nothing forbids it now");

            // Where the flag is not told, the rename says so, and no name is
            // claimed that could not be given back.
            if set_flags(&dir, APPEND) {
                let mut temp = temp_in(&dir, "new");
                let result = temp.move_to(&dir.join("out"), &TO_NEW);
                let left = names(&dir).len();
                assert!(set_flags(&dir, 0));
                assert_eq!(
                    result.expect_err("refused").raw_os_error(),
                    Some(libc::EPERM)
                );
                assert_eq!(left, 2, "file.yaml and the temporary file");
            }
        }

        #[test]
        fn a_file_rewritten_in_place_after_it_was_read_is_not_written() {
            let later = SystemTime::now() + std::time::Duration::from_secs(60);
            let rewrite_in_place = |file: &Path, content: &str, modified: SystemTime| {
                fs::write(file, content).expect("write");
                let file = File::options().write(true).open(file).expect("open");
                file.set_modified(modified).expect("utimes");
            };
            for (content, moved) in [("old!", false), ("OLD", true), ("old", true)] {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                seed(&file, "old", 0o644);
                let was = fs::metadata(&file).expect("stat").modified().expect("time");
                let modified = if moved { later } else { was };

                let result = rewrite_after(dir.path(), |_| {
                    rewrite_in_place(&file, content, modified);
                });

                let (path, cause) = write_error(result.map(drop));
                assert_eq!((path.as_path(), cause.as_str()), (utf8(&file), CHANGED));
                assert_eq!(read(&file), content);
                assert_eq!(names(dir.path()), ["file.yaml"]);
            }
            // And while it is written: before the rename and the backup.
            for backup in [false, true] {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                seed(&file, "old", 0o644);
                let there = IfExists::replace(backup);
                let write = Write::to(utf8(&file), Links::Follow, there).expect("found");
                rewrite_in_place(&file, "other", later);

                let err = write.commit(b"new", None).expect_err("refused");

                assert_eq!(err.to_string(), CHANGED);
                assert_eq!(read(&file), "other");
                assert_eq!(names(dir.path()), ["file.yaml"]);
            }
        }

        /// The inode of a deleted file may be given to the next one, of the
        /// same size and time even: the time it was created tells.
        #[test]
        fn a_file_is_told_by_each_thing_known_of_it() {
            type Other = fn(&mut Entry);
            let others: [(&str, Other); 5] = [
                ("device", |entry| entry.id.0 += 1),
                ("inode", |entry| entry.id.1 += 1),
                ("creation", |entry| {
                    entry.created = match entry.created {
                        Some(_) => None,
                        // This file system does not tell.
                        None => Some(SystemTime::UNIX_EPOCH),
                    };
                }),
                ("size", |entry| entry.size += 1),
                ("modification", |entry| {
                    entry.modified = Some(SystemTime::UNIX_EPOCH);
                }),
            ];
            let dir = tempfile::tempdir().expect("tempdir");
            let file = dir.path().join("file.yaml");
            seed(&file, "old", 0o644);
            let found = fs::metadata(&file).expect("stat");
            assert_eq!(Entry::of(&found).created, found.created().ok());
            for (what, make_other) in others {
                let (_, mut original) = super::super::read(utf8(&file)).expect("read");
                let place = original.place.as_mut().expect("a regular file");
                make_other(place.entry.as_mut().expect("a regular file"));

                let result = rewrite(utf8(&file), &original, b"new", false);

                assert_eq!(write_error(result.map(drop)).1, CHANGED, "{what}");
                assert_eq!(read(&file), "old", "{what}");
            }
        }

        #[test]
        fn what_could_not_be_kept_is_said_and_costs_the_others_their_access() {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("out");
            for (bits, kept) in [(0o664, 0o600), (0o755, 0o700), (0o4755, 0o4700)] {
                seed(&target, "old", bits);
                if mode(&target) != bits {
                    continue; // the system refuses some of the special bits
                }
                // As for a file the process may not read.
                let like = Existing {
                    file: None,
                    found: fs::metadata(&target).expect("stat"),
                };
                let temp = create_temp(dir.path(), Some(&like)).expect("temp");

                let lost = keep_access(temp.as_file(), &like).expect("not an error");

                assert_eq!(
                    lost,
                    ["its ACL could not be kept, so only its owner has access"]
                );
                assert_eq!(mode(temp.path()), kept, "{bits:o}");
            }
            for (owner, bits, acl, expected) in [
                (true, true, true, &[][..]),
                (false, true, true, &["its owner or group could not be kept"]),
                (
                    true,
                    false,
                    true,
                    &["its permission bits could not all be kept"],
                ),
            ] {
                assert_eq!(losses(access::Kept { owner, bits, acl }), expected);
            }
        }

        /// Documented, not wished for: the rename makes a new file.
        #[test]
        fn another_hard_link_goes_on_naming_the_old_contents() {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("out");
            let other = dir.path().join("other");
            seed(&target, "old", 0o644);
            fs::hard_link(&target, &other).expect("link");

            write(utf8(&target), b"new", false).expect("write");

            assert_eq!(read(&target), "new");
            assert_eq!(read(&other), "old");
        }

        /// The ACL of the file at `path`.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        fn acl_of(path: &Path) -> Option<Vec<u8>> {
            acl::read(&File::open(path).expect("open")).expect("an ACL or none")
        }

        /// [`rewrite`] `file.yaml` in `dir` with a backup, and make
        /// `new.yaml` like it: each has the mode `bits` and the ACL `list`.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        fn assert_kept(dir: &Path, bits: u32, list: Option<&[u8]>) {
            let file = dir.join("file.yaml");
            assert_eq!((mode(&file), acl_of(&file).as_deref()), (bits, list));

            assert!(rewrite_after(dir, |_| ()).expect("rewrite"));
            like(&dir.join("new.yaml"), &file).expect("write");

            for name in ["file.yaml", "file.yaml.bak", "new.yaml"] {
                let path = dir.join(name);
                assert_eq!(acl_of(&path).as_deref(), list, "{name}");
                assert_eq!(mode(&path), bits, "{name}");
            }
            assert_eq!(read(&file), "new");
            assert_eq!(read(&dir.join("file.yaml.bak")), "old");
        }

        /// POSIX ACLs, as the kernel keeps them: no tool is needed to set
        /// one, and a file system without them ends the test.
        #[cfg(target_os = "linux")]
        mod posix_acl {
            use super::*;

            const NOBODY: u32 = u32::MAX;

            /// `(tag, permissions, id)` entries as `system.posix_acl_*`: user
            /// 1, named user 2, group 4, mask 16, other 32.
            fn list(entries: &[(u16, u16, u32)]) -> Vec<u8> {
                let mut list = 2u32.to_le_bytes().to_vec();
                for (tag, permissions, id) in entries {
                    list.extend(tag.to_le_bytes());
                    list.extend(permissions.to_le_bytes());
                    list.extend(id.to_le_bytes());
                }
                list
            }

            /// Give the file at `path` an ACL; `false` without ACLs.
            fn set(path: &Path, list: &[u8]) -> bool {
                acl::set(&File::open(path).expect("open"), Some(list)).is_ok()
            }

            /// Give what is created in `dir` an ACL.
            fn set_default(dir: &Path, list: &[u8]) -> bool {
                use std::os::unix::ffi::OsStrExt as _;

                let dir = std::ffi::CString::new(dir.as_os_str().as_bytes()).expect("no NUL");
                // SAFETY: both names are C strings, the buffer as long as said.
                unsafe {
                    libc::setxattr(
                        dir.as_ptr(),
                        c"system.posix_acl_default".as_ptr(),
                        list.as_ptr().cast(),
                        list.len(),
                        0,
                    ) == 0
                }
            }

            #[test]
            fn a_user_let_in_is_still_and_the_group_is_not() {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                seed(&file, "old", 0o600);
                let acl = list(&[
                    (1, 6, NOBODY),
                    (2, 6, 2000),
                    (4, 0, NOBODY),
                    (16, 6, NOBODY),
                    (32, 0, NOBODY),
                ]);
                if !set(&file, &acl) {
                    return;
                }
                // The group bits are the mask: as a mode, they would let the
                // group in.
                assert_kept(dir.path(), 0o660, Some(&acl));
            }

            #[test]
            fn a_user_kept_out_is_still() {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                seed(&file, "old", 0o644);
                let acl = list(&[
                    (1, 6, NOBODY),
                    (2, 0, 2000),
                    (4, 4, NOBODY),
                    (16, 4, NOBODY),
                    (32, 4, NOBODY),
                ]);
                if !set(&file, &acl) {
                    return;
                }
                assert_kept(dir.path(), 0o644, Some(&acl));
            }

            #[test]
            fn a_file_without_an_acl_does_not_inherit_one() {
                let dir = tempfile::tempdir().expect("tempdir");
                seed(&dir.path().join("file.yaml"), "old", 0o640);
                let acl = list(&[
                    (1, 7, NOBODY),
                    (2, 7, 2000),
                    (4, 7, NOBODY),
                    (16, 7, NOBODY),
                    (32, 5, NOBODY),
                ]);
                if !set_default(dir.path(), &acl) {
                    return;
                }
                let plain = dir.path().join("plain");
                File::create(&plain).expect("create");
                let inherited = acl_of(&plain).expect("what is created there inherits it");
                assert_kept(dir.path(), 0o640, None);

                // A file that is new inherits it, as any other.
                write(utf8(&dir.path().join("made")), b"new", false).expect("write");
                assert_eq!(acl_of(&dir.path().join("made")), Some(inherited));
                assert_eq!(mode(&dir.path().join("made")), mode(&plain));
            }
        }

        #[cfg(target_os = "macos")]
        mod extended_acl {
            use super::*;

            fn chmod(args: &[&str], path: &Path) {
                let done = Command::new("/bin/chmod").args(args).arg(path).status();
                assert!(done.expect("chmod").success());
            }

            #[test]
            fn a_user_kept_out_is_still() {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                seed(&file, "old", 0o644);
                chmod(&["+a", "nobody deny read"], &file);
                let acl = acl_of(&file);
                assert!(acl.is_some());
                assert_kept(dir.path(), 0o644, acl.as_deref());
            }

            #[test]
            fn a_file_without_an_acl_does_not_inherit_one() {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                seed(&file, "old", 0o600);
                chmod(&["+a", "nobody allow read,file_inherit"], dir.path());
                let plain = dir.path().join("plain");
                File::create(&plain).expect("create");
                assert!(
                    acl_of(&plain).is_some(),
                    "what is created there inherits it"
                );

                let like = Existing {
                    file: None,
                    found: fs::metadata(&file).expect("stat"),
                };
                let temp = create_temp(dir.path(), Some(&like)).expect("temp");
                assert_eq!(acl_of(temp.path()), None, "not even while it is written");
                drop(temp);
                assert_kept(dir.path(), 0o600, None);
            }

            const NOT_REPLACEABLE: &str = "it cannot be replaced: its ACL or flags, or its \
                                           directory, forbid deleting it";

            /// Replacing it would be deleting it.
            #[test]
            fn a_file_that_may_not_be_deleted_is_refused_and_nothing_is_left() {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                seed(&file, "old", 0o644);
                for (protect, undo) in [
                    (["/bin/chmod", "+a", "everyone deny delete"], ["-N"]),
                    (["/usr/bin/chflags", "-h", "uchg"], ["nouchg"]),
                ] {
                    let run = |program: &str, args: &[&str]| {
                        let done = Command::new(program).args(args).arg(&file).status();
                        assert!(done.expect(program).success());
                    };
                    run(protect[0], &protect[1..]);
                    let results = [
                        rewrite_after(dir.path(), |_| ()).map(drop),
                        write(utf8(&file), b"new", false),
                        write(utf8(&file), b"new", true),
                    ];
                    let removable = check_remove(utf8(&file));
                    let checked = [IfExists::Refuse, IfExists::Replace, IfExists::BackUp]
                        .map(|there| check_new(utf8(&file), there));
                    run(protect[0], &undo);

                    let [refuse, replace, back_up] = checked;
                    assert!(refuse.expect("to be refused by the caller"));
                    for result in results
                        .into_iter()
                        .chain([replace.map(drop), back_up.map(drop)])
                    {
                        let (path, cause) = write_error(result);
                        assert_eq!(
                            (path.as_path(), cause.as_str()),
                            (utf8(&file), NOT_REPLACEABLE)
                        );
                    }
                    assert_eq!(
                        write_error(removable).1,
                        NOT_REPLACEABLE.replace("replaced", "removed")
                    );
                    assert_eq!(read(&file), "old");
                    assert_eq!(names(dir.path()), ["file.yaml"]);
                    check_remove(utf8(&file)).expect("nothing forbids it now");
                }
            }

            #[test]
            fn a_directory_whose_files_may_not_be_deleted_is_left_empty() {
                let root = tempfile::tempdir().expect("tempdir");
                let source = root.path().join("source");
                seed(&source, "old", 0o644);
                let dir = root.path().join("dir");
                fs::create_dir(&dir).expect("mkdir");
                chmod(&["+a", "everyone deny delete_child"], &dir);

                let (_, cause) = write_error(like(&dir.join("out"), &source));

                chmod(&["-N"], &dir);
                assert!(cause.starts_with("its directory forbids"), "{cause}");
                assert!(names(&dir).is_empty());
            }

            /// The list is put on once the new file is in its place: its
            /// owner's alone until then, and as the source after.
            #[test]
            fn a_new_file_takes_a_list_that_forbids_its_renaming() {
                for (there, entries) in [
                    (IfExists::Refuse, &["everyone deny delete"][..]),
                    (IfExists::Replace, &["everyone deny delete"]),
                    (
                        IfExists::BackUp,
                        &["everyone deny delete,writesecurity", "nobody deny read"],
                    ),
                ] {
                    let dir = tempfile::tempdir().expect("tempdir");
                    let source = dir.path().join("source");
                    let out = dir.path().join("out");
                    seed(&source, "old", 0o640);
                    for entry in entries {
                        chmod(&["+a", entry], &source);
                    }
                    let acl = acl_of(&source);
                    assert!(acl.is_some());
                    let (_, original) = super::super::super::read(utf8(&source)).expect("read");

                    write_like(utf8(&out), b"new", there, utf8(&source), &original)
                        .expect("written");

                    let found = (read(&out), mode(&out), acl_of(&out));
                    let of_dir = names(dir.path());
                    chmod(&["-N"], &source);
                    chmod(&["-N"], &out);
                    assert_eq!(found, ("new".to_owned(), 0o640, acl));
                    assert_eq!(of_dir, ["out", "source"]);
                }
            }

            /// Whatever fails once the temporary file has the list.
            #[test]
            fn a_temporary_file_that_may_not_be_deleted_goes_all_the_same() {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = dir.path().join("source");
                let out = dir.path().join("out");
                seed(&source, "old", 0o640);
                chmod(&["+a", "everyone deny delete"], &source);
                let (_, original) = super::super::super::read(utf8(&source)).expect("read");
                let (_, like) = original.find(utf8(&source)).expect("there");

                let temp = stage(&out, b"new", Some(&like)).expect("staged");
                assert!(acl_of(temp.path.as_ref().expect("a path")).is_some());
                drop(temp);
                let after_drop = names(dir.path());

                // And when renaming it fails for another reason, too.
                let write = Write::to(utf8(&out), Links::Refuse, IfExists::Refuse).expect("free");
                fs::write(&out, "other").expect("write");
                let err = write.commit(b"new", Some(&like)).expect_err("refused");
                let after_rename = names(dir.path());

                chmod(&["-N"], &source);
                assert_eq!(after_drop, ["source"]);
                assert_eq!(err.to_string(), APPEARED);
                assert_eq!(read(&out), "other");
                assert_eq!(after_rename, ["out", "source"]);
            }

            /// Renames a file without a list, and then forbids giving it one.
            fn rename_for_good(temp: &mut Temp, to: &Path) -> io::Result<()> {
                if acl::read(&temp.file)?.is_some() {
                    return denied(temp, to);
                }
                temp.rename(to)?;
                let done = Command::new("/usr/bin/chflags")
                    .arg("uchg")
                    .arg(to)
                    .status();
                assert!(done.expect("chflags").success());
                Ok(())
            }

            #[test]
            fn a_list_that_is_lost_on_the_way_is_told_with_what_was_lost_before() {
                let dir = tempfile::tempdir().expect("tempdir");
                let out = dir.path().join("out");
                let mut temp = temp_in(dir.path(), "new");
                fs::set_permissions(
                    temp.path.as_ref().expect("a path"),
                    fs::Permissions::from_mode(0o644),
                )
                .expect("chmod");
                chmod(
                    &["+a", "nobody deny read"],
                    temp.path.as_ref().expect("a path"),
                );
                temp.lost.push("before");

                let lost = temp.put(&out, &[rename_for_good]);

                let done = Command::new("/usr/bin/chflags")
                    .arg("nouchg")
                    .arg(&out)
                    .status();
                assert!(done.expect("chflags").success());
                assert_eq!(lost.expect("it is there"), ["before", LOST_ACL]);
                assert_eq!(acl_of(&out), None);
                assert_eq!(mode(&out), 0o600, "its owner's alone");
            }

            #[test]
            fn an_inherited_list_is_kept_as_it_is() {
                let dir = tempfile::tempdir().expect("tempdir");
                let file = dir.path().join("file.yaml");
                chmod(&["+a", "nobody allow read,file_inherit"], dir.path());
                seed(&file, "old", 0o600);
                // The directory says something else by now.
                chmod(&["-N"], dir.path());
                chmod(&["+a", "everyone allow write,file_inherit"], dir.path());
                let acl = acl_of(&file);
                assert!(acl.is_some());
                assert_kept(dir.path(), 0o600, acl.as_deref());
            }
        }
    }
}
