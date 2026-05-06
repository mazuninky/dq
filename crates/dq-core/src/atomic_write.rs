//! Atomic file replacement helper used by the M2 write path.
//!
//! The strategy follows `add-safe-writes` D5:
//!
//! 1. Create a [`tempfile::NamedTempFile`] in the **same directory** as the
//!    target. Same-directory placement is critical because cross-filesystem
//!    `rename(2)` returns `EXDEV` on Linux and is not atomic.
//! 2. Write the new contents into the tempfile.
//! 3. If `backup` is requested and the target already exists, copy the old
//!    contents to `<target>.bak` *before* the rename so a failed persist still
//!    leaves a recoverable copy on disk.
//! 4. Call [`tempfile::NamedTempFile::persist`], which wraps both Unix
//!    `rename(2)` and Windows `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`.
//!
//! All I/O failures are wrapped in [`crate::Error::WriteIo`] so the CLI can
//! distinguish read-side and write-side failures when mapping to exit codes.

use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};

use crate::{Error, Result};

/// Atomically write `content` to `path`.
///
/// If `backup` is `true` and `path` already exists, the previous contents are
/// copied to `<path>.bak` (always, irrespective of the target's existing
/// extension) before the new contents are committed.
///
/// # Errors
///
/// Returns [`Error::WriteIo`] if any of the following fail:
/// - the target directory cannot be opened (no parent directory, missing dir);
/// - the tempfile cannot be created or written;
/// - the backup copy fails;
/// - the final rename/persist fails.
pub fn write(path: &Utf8Path, content: &[u8], backup: bool) -> Result<()> {
    // `path.parent()` returns `None` for paths without a parent component
    // (e.g. `"foo"` viewed as a relative path resolves to `Some("")`, but a
    // bare root like `/` has none). Reject such paths explicitly — we cannot
    // create a same-directory tempfile without a parent.
    let parent = match path.parent() {
        Some(p) if !p.as_str().is_empty() => p.to_path_buf(),
        Some(_) => Utf8PathBuf::from("."),
        None => {
            return Err(Error::WriteIo {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "atomic write requires a parent directory; root-only paths are not supported",
                ),
            });
        }
    };

    let mut tmp =
        tempfile::NamedTempFile::new_in(parent.as_std_path()).map_err(|e| Error::WriteIo {
            path: path.to_path_buf(),
            source: e,
        })?;

    tmp.write_all(content).map_err(|e| Error::WriteIo {
        path: path.to_path_buf(),
        source: e,
    })?;

    if backup && path.exists() {
        let backup_path = backup_path_for(path);
        std::fs::copy(path.as_std_path(), backup_path.as_std_path()).map_err(|e| {
            Error::WriteIo {
                path: path.to_path_buf(),
                source: e,
            }
        })?;
    }

    tmp.persist(path.as_std_path())
        .map_err(|persist_err| Error::WriteIo {
            path: path.to_path_buf(),
            source: persist_err.error,
        })?;

    Ok(())
}

/// Compute the backup path for `path` by always appending `.bak` to the full
/// path string. So `foo.yaml` → `foo.yaml.bak`, `foo` → `foo.bak`, and
/// `foo.bak` → `foo.bak.bak`.
fn backup_path_for(path: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{}.bak", path.as_str()))
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            backup_path_for(&Utf8PathBuf::from("foo.yaml")),
            Utf8PathBuf::from("foo.yaml.bak"),
        );
        assert_eq!(
            backup_path_for(&Utf8PathBuf::from("foo.bak")),
            Utf8PathBuf::from("foo.bak.bak"),
        );
        assert_eq!(
            backup_path_for(&Utf8PathBuf::from("foo")),
            Utf8PathBuf::from("foo.bak"),
        );
        assert_eq!(
            backup_path_for(&Utf8PathBuf::from("foo.tar.gz")),
            Utf8PathBuf::from("foo.tar.gz.bak"),
        );
    }
}
