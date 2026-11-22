//! What the system lets be deleted, asked before anything is made.
//!
//! Renaming a file, or another over it, is deleting it too: with a target
//! that may not be deleted a temporary file would be written in vain, and in
//! a directory whose files may not be deleted it could not even be removed.
//!
//! - macOS: an ACL can forbid it, of a file (`deny delete`) or of what is in
//!   a directory (`deny delete_child`), and so can a flag (`uchg`). The
//!   system is asked. Of a directory in which no files may be made nothing is
//!   said: making the temporary one fails, which says why and leaves nothing.
//! - Linux: the flags `i` and `a` of a file, and `a` of a directory
//!   (`chattr`), forbid it to root too. Those are looked at, where the file
//!   system tells them.
//! - Elsewhere, and of what else forbids it (a file of another user in a
//!   sticky directory), the rename or the removal says so.

#[cfg(target_os = "linux")]
pub(super) use linux::{may_delete, may_delete_in};
#[cfg(target_os = "macos")]
pub(super) use macos::{may_delete, may_delete_in};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) use other::{may_delete, may_delete_in};

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    use super::super::dir_of;

    /// `_WRITE_OK` of `sys/unistd.h`: of a directory, to add files to it.
    const WRITE_OK: libc::c_int = 1 << 10;
    /// `_DELETE_OK`: to delete what is at the path.
    const DELETE_OK: libc::c_int = 1 << 12;
    /// `_RMFILE_OK`: to delete the files in the directory at the path.
    const RMFILE_OK: libc::c_int = 1 << 14;

    /// Whether what is at `path`, be it a link, may be deleted. `true` when
    /// that cannot be told.
    pub(in super::super) fn may_delete(path: &Path) -> bool {
        may(path, DELETE_OK, libc::AT_SYMLINK_NOFOLLOW)
            || dir_of(path).is_ok_and(|dir| !may(dir, WRITE_OK, 0))
    }

    /// Whether the files in `dir` may be deleted. `true` when that cannot be
    /// told.
    pub(in super::super) fn may_delete_in(dir: &Path) -> bool {
        may(dir, RMFILE_OK, 0) || !may(dir, WRITE_OK, 0)
    }

    fn may(path: &Path, access: libc::c_int, flags: libc::c_int) -> bool {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return true;
        };
        let flags = flags | libc::AT_EACCESS;
        // SAFETY: the path is a C string.
        let found = unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), access, flags) };
        found == 0
            || !matches!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::EACCES | libc::EPERM)
            )
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    use super::super::dir_of;

    /// Whether what is at `path`, be it a link, may be deleted. `true` when
    /// that cannot be told.
    pub(in super::super) fn may_delete(path: &Path) -> bool {
        !has(path, libc::STATX_ATTR_IMMUTABLE | libc::STATX_ATTR_APPEND)
            && dir_of(path).ok().is_none_or(may_delete_in)
    }

    /// Whether the files in `dir` may be deleted. `true` when that cannot be
    /// told.
    pub(in super::super) fn may_delete_in(dir: &Path) -> bool {
        // In an immutable one the temporary file cannot even be made.
        !has(&dir.join("."), libc::STATX_ATTR_APPEND)
    }

    /// Whether what is at `path` has one of the `flags`.
    fn has(path: &Path, flags: libc::c_int) -> bool {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        let mut found = MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: the path is a C string, and there is room for the answer.
        let done = unsafe {
            libc::statx(
                libc::AT_FDCWD,
                path.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
                0,
                found.as_mut_ptr(),
            )
        };
        // SAFETY: zeroes are a valid answer, whether it was filled in or not.
        let found = unsafe { found.assume_init() };
        done == 0 && found.stx_attributes & flags.unsigned_abs() as u64 != 0
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod other {
    use std::path::Path;

    pub(in super::super) fn may_delete(_path: &Path) -> bool {
        true
    }

    pub(in super::super) fn may_delete_in(_dir: &Path) -> bool {
        true
    }
}
