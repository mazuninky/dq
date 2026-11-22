//! The access control list of an open file, as bytes to carry from one file
//! to another.
//!
//! - Linux: the POSIX ACL, the `system.posix_acl_access` attribute. NFSv4
//!   ACLs (`system.nfs4_acl`) are not read.
//! - macOS: the extended ACL, through the `acl(3)` functions of the system
//!   library, which the `libc` crate does not declare. Not `fcopyfile` with
//!   `COPYFILE_ACL`: it keeps the entries the new file inherited from its
//!   directory and drops the inherited ones of the old file, so the two are
//!   not the same list.
//! - Elsewhere: none.
//!
//! Both functions work on an open file, so no path is looked up again. A
//! file system without ACLs has none, which is no error.

#[cfg(target_os = "linux")]
pub(super) use linux::{read, set};
#[cfg(target_os = "macos")]
pub(super) use macos::{read, set};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use none::{read, set};

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CStr;
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd as _;

    const NAME: &CStr = c"system.posix_acl_access";

    /// The ACL of `file`; `None` when it has none beyond its mode.
    ///
    /// # Errors
    ///
    /// The system's, when there may be one that cannot be read.
    pub(in super::super) fn read(file: &File) -> io::Result<Option<Vec<u8>>> {
        loop {
            // SAFETY: the name is a C string, and a size of 0 with a null
            // buffer asks for the size alone.
            let size = unsafe {
                libc::fgetxattr(file.as_raw_fd(), NAME.as_ptr(), std::ptr::null_mut(), 0)
            };
            let Ok(size) = usize::try_from(size) else {
                return absent(None);
            };
            let mut list = vec![0u8; size];
            // SAFETY: the buffer is `size` bytes long.
            let got = unsafe {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    NAME.as_ptr(),
                    list.as_mut_ptr().cast(),
                    size,
                )
            };
            match usize::try_from(got) {
                // It went away in between, or is a size of nothing.
                Ok(0) => return Ok(None),
                Ok(got) => {
                    list.truncate(got);
                    return Ok(Some(list));
                }
                Err(_) if io::Error::last_os_error().raw_os_error() == Some(libc::ERANGE) => {}
                Err(_) => return absent(None),
            }
        }
    }

    /// Give `file` the ACL `list`, or take the one it has.
    ///
    /// # Errors
    ///
    /// The system's: not permitted, no such ACLs, ...
    pub(in super::super) fn set(file: &File, list: Option<&[u8]>) -> io::Result<()> {
        let done = match list {
            // SAFETY: the name is a C string and the buffer as long as said.
            Some(list) => unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    NAME.as_ptr(),
                    list.as_ptr().cast(),
                    list.len(),
                    0,
                )
            },
            // SAFETY: the name is a C string.
            None => unsafe { libc::fremovexattr(file.as_raw_fd(), NAME.as_ptr()) },
        };
        match (done, list) {
            (0, _) => Ok(()),
            (_, None) => absent(()),
            (_, Some(_)) => Err(io::Error::last_os_error()),
        }
    }

    /// The last error, unless it says that there is no ACL: then `none`.
    fn absent<T>(none: T) -> io::Result<T> {
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ENODATA | libc::EOPNOTSUPP | libc::ENOSYS) => Ok(none),
            _ => Err(err),
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_int, c_void};
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd as _;

    /// `ACL_TYPE_EXTENDED` of `sys/acl.h`.
    const EXTENDED: c_int = 0x100;
    /// `ACL_FIRST_ENTRY` of `sys/acl.h`.
    const FIRST_ENTRY: c_int = 0;

    // `sys/acl.h`: `acl_t` and `acl_entry_t` are pointers to hidden types.
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_set_fd_np(fd: c_int, acl: *mut c_void, kind: c_int) -> c_int;
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_size(acl: *mut c_void) -> libc::ssize_t;
        fn acl_copy_ext_native(
            buffer: *mut c_void,
            acl: *mut c_void,
            size: libc::ssize_t,
        ) -> libc::ssize_t;
        fn acl_copy_int_native(buffer: *const c_void) -> *mut c_void;
        fn acl_free(object: *mut c_void) -> c_int;
    }

    /// An `acl_t` of ours, never null.
    struct Acl(*mut c_void);

    impl Acl {
        /// Take over what an `acl(3)` function returned.
        fn own(acl: *mut c_void) -> io::Result<Self> {
            if acl.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(acl))
        }
    }

    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: allocated by an `acl(3)` function, freed once, here.
            unsafe { acl_free(self.0) };
        }
    }

    /// The ACL of `file`; `None` when it has none beyond its mode.
    ///
    /// # Errors
    ///
    /// The system's, when there may be one that cannot be read.
    pub(in super::super) fn read(file: &File) -> io::Result<Option<Vec<u8>>> {
        // SAFETY: the descriptor is open.
        let acl = match Acl::own(unsafe { acl_get_fd_np(file.as_raw_fd(), EXTENDED) }) {
            Ok(acl) => acl,
            Err(err) => {
                return match err.raw_os_error() {
                    Some(libc::ENOENT | libc::ENOTSUP | libc::EOPNOTSUPP) => Ok(None),
                    _ => Err(err),
                };
            }
        };
        let mut entry = std::ptr::null_mut();
        // SAFETY: a valid ACL, and a place for the entry.
        if unsafe { acl_get_entry(acl.0, FIRST_ENTRY, &mut entry) } != 0 {
            return Ok(None); // a list of no entries
        }
        // SAFETY: a valid ACL.
        let size = unsafe { acl_size(acl.0) };
        let mut list = vec![0u8; usize::try_from(size).map_err(|_| io::Error::last_os_error())?];
        // SAFETY: the buffer is `size` bytes long.
        let got = unsafe { acl_copy_ext_native(list.as_mut_ptr().cast(), acl.0, size) };
        list.truncate(usize::try_from(got).map_err(|_| io::Error::last_os_error())?);
        Ok(Some(list))
    }

    /// Give `file` the ACL `list`, which [`read`] returned, or take the one
    /// it has: a list of no entries is none.
    ///
    /// # Errors
    ///
    /// The system's: not permitted, no such ACLs, ...
    pub(in super::super) fn set(file: &File, list: Option<&[u8]>) -> io::Result<()> {
        let acl = Acl::own(match list {
            // SAFETY: the bytes `acl_copy_ext_native` made of an ACL.
            Some(list) => unsafe { acl_copy_int_native(list.as_ptr().cast()) },
            // SAFETY: no preconditions.
            None => unsafe { acl_init(1) },
        })?;
        // SAFETY: the descriptor is open and the ACL valid.
        match unsafe { acl_set_fd_np(file.as_raw_fd(), acl.0, EXTENDED) } {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod none {
    use std::fs::File;
    use std::io;

    pub(in super::super) fn read(_file: &File) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    pub(in super::super) fn set(_file: &File, _list: Option<&[u8]>) -> io::Result<()> {
        Ok(())
    }
}
