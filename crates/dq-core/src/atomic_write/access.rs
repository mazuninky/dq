//! Giving the temporary file the access of the file it replaces: owner,
//! group, permission bits and ACL, as far as the process may set them.

use std::fs::File;
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, fchown};

use super::acl;

/// Owner, group and permission bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mode {
    pub(super) uid: u32,
    pub(super) gid: u32,
    /// The twelve bits of `chmod`. Where there is a Linux ACL, the group
    /// bits are its mask, not what the group may do.
    pub(super) bits: u32,
}

impl From<&std::fs::Metadata> for Mode {
    fn from(found: &std::fs::Metadata) -> Self {
        Self {
            uid: found.uid(),
            gid: found.gid(),
            bits: found.mode() & 0o7777,
        }
    }
}

/// The access to keep.
#[derive(Debug)]
pub(super) struct Access {
    pub(super) mode: Mode,
    /// `None` when the file has an ACL, or may have one, that could not be
    /// read; `Some(None)` when it has none.
    pub(super) acl: Option<Option<Vec<u8>>>,
}

impl Access {
    /// The access of a file, found to be `mode`; `file` is open when the
    /// process may read it.
    pub(super) fn of(mode: Mode, file: Option<&File>) -> Self {
        Self {
            mode,
            acl: file.and_then(|file| acl::read(file).ok()),
        }
    }
}

/// What could be kept. Not being allowed to is no error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Kept {
    /// The owner and the group.
    pub(super) owner: bool,
    /// The bits that go with the owner, group and ACL the file has now.
    pub(super) bits: bool,
    pub(super) acl: bool,
}

/// The file that takes over an [`Access`]: an open temporary file.
pub(super) trait Staged {
    fn mode(&self) -> io::Result<Mode>;
    fn set_bits(&self, bits: u32) -> io::Result<()>;
    fn set_owner(&self, uid: u32) -> io::Result<()>;
    fn set_group(&self, gid: u32) -> io::Result<()>;
    fn acl(&self) -> io::Result<Option<Vec<u8>>>;
    fn set_acl(&self, list: Option<&[u8]>) -> io::Result<()>;
}

impl Staged for File {
    fn mode(&self) -> io::Result<Mode> {
        Ok(Mode::from(&self.metadata()?))
    }

    fn set_bits(&self, bits: u32) -> io::Result<()> {
        self.set_permissions(std::fs::Permissions::from_mode(bits))
    }

    fn set_owner(&self, uid: u32) -> io::Result<()> {
        fchown(self, Some(uid), None)
    }

    fn set_group(&self, gid: u32) -> io::Result<()> {
        fchown(self, None, Some(gid))
    }

    fn acl(&self) -> io::Result<Option<Vec<u8>>> {
        acl::read(self)
    }

    fn set_acl(&self, list: Option<&[u8]>) -> io::Result<()> {
        acl::set(self, list)
    }
}

/// Take away an ACL `file` inherited from its directory.
///
/// # Errors
///
/// The system's; also when the ACL is there afterwards all the same, since
/// its entries may open the file to others than the mode does.
pub(super) fn strip_acl(file: &dyn Staged) -> io::Result<()> {
    if file.acl()?.is_none() {
        return Ok(());
    }
    file.set_acl(None)?;
    match file.acl()? {
        None => Ok(()),
        Some(_) => Err(io::Error::other(
            "the ACL it inherits from its directory cannot be removed",
        )),
    }
}

/// Take away the ACL `file` has been given, and with it what the mode `bits`
/// open it to beyond its owner.
///
/// That first: on Linux the group bits were the mask of the ACL, and are the
/// group's own once the ACL is gone.
///
/// # Errors
///
/// As [`strip_acl`]; also when the bits cannot be set.
pub(super) fn withdraw_acl(file: &dyn Staged, bits: u32) -> io::Result<()> {
    file.set_bits(bits & 0o700)?;
    strip_acl(file)
}

/// Give `file`, open to its owner at most and without an ACL, the access
/// `like`.
///
/// Each step leaves the file open to no more than `like` says: the group
/// before the bits that apply to it; the ACL before the bits, which it may
/// take back (a `deny` entry) and, on Linux, gives their meaning (the group
/// bits are its mask); the owner after them, since giving the file away can
/// also be giving up the right to set them; the set-user-ID, set-group-ID
/// and sticky bits last. Those are not for every process to set on every
/// system, and go when they cannot be.
///
/// An ACL that cannot be kept leaves the file to its owner alone: what it
/// granted or denied to others is not to be told from the mode.
///
/// What came of a change is read back rather than taken from its result.
pub(super) fn keep(file: &dyn Staged, like: &Access) -> io::Result<Kept> {
    let Mode { uid, gid, bits } = like.mode;
    let new = file.mode()?;
    if new.gid != gid {
        file.set_group(gid).ok();
    }
    let same_group = file.mode()?.gid == gid;
    let same_acl = keep_acl(file, like, same_group)?;
    let set_bits = |bits: u32| -> io::Result<()> {
        if file.mode()?.bits == bits {
            return Ok(());
        }
        file.set_bits(bits)
    };
    let open_to = if same_acl { 0o7777 } else { 0o7700 };
    set_bits(kept_bits(bits & 0o777, true, same_group) & open_to)?;
    if new.uid != uid {
        file.set_owner(uid).ok();
    }
    let same_owner = file.mode()?.uid == uid;
    let wanted = kept_bits(bits, same_owner, same_group) & open_to;
    set_bits(wanted).ok();
    Ok(Kept {
        owner: same_owner && same_group,
        // Linux drops set-group-ID without a word when the process is not in
        // the file's group.
        bits: file.mode()?.bits == wanted,
        acl: same_acl,
    })
}

/// Give `file` the ACL of `like`; `false` when it cannot have it, and has
/// none.
fn keep_acl(file: &dyn Staged, like: &Access, same_group: bool) -> io::Result<bool> {
    let Some(wanted) = &like.acl else {
        return Ok(false);
    };
    let Some(list) = wanted else {
        return Ok(true); // it has none, and neither has the file
    };
    // A Linux ACL says what the file's group may do: with another group it
    // is another list.
    let may = same_group || !cfg!(target_os = "linux");
    if may
        && file.set_acl(Some(list)).is_ok()
        && file
            .acl()
            .is_ok_and(|now| now.as_deref() == Some(list.as_slice()))
    {
        return Ok(true);
    }
    withdraw_acl(file, like.mode.bits)?;
    Ok(false)
}

/// The permission bits of a file that had `bits`, once it has its owner and
/// group or other ones: nobody may gain access by the change.
///
/// Another owner does not inherit set-user-ID. With another group, its
/// members were "others" before and the members of the old one are "others"
/// now, so both classes get the bits that both had, and set-group-ID goes.
pub(super) fn kept_bits(bits: u32, same_owner: bool, same_group: bool) -> u32 {
    let mut kept = bits;
    if !same_owner {
        kept &= !0o4000;
    }
    if !same_group {
        let both = (kept >> 3) & kept & 0o007;
        kept = (kept & 0o5700) | (both << 3) | both;
    }
    kept
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The process of a [`Fake`]: its ids, and what else it may do.
    #[derive(Clone, Copy)]
    struct Process {
        uid: u32,
        groups: &'static [u32],
        /// `CAP_CHOWN`.
        may_chown: bool,
        /// `CAP_FOWNER`: set the mode and ACL of any file.
        may_chmod: bool,
        /// The file system has ACLs, and the process may set them.
        may_set_acl: bool,
        /// The file system keeps the ACL it is given.
        keeps_acl: bool,
        /// An ACL can be taken off a file again.
        drops_acl: bool,
        /// The ACL is a POSIX one: the mode shows it, and keeps showing it
        /// once it is gone. Without it the mode is another thing (macOS).
        posix_acl: bool,
    }

    const USER: Process = Process {
        uid: 1000,
        groups: &[1000, 2000],
        may_chown: false,
        may_chmod: false,
        may_set_acl: true,
        keeps_acl: true,
        drops_acl: true,
        posix_acl: true,
    };

    /// A temporary file as the system would keep it, that checks after every
    /// step that it is open to no more than the file it stands in for.
    struct Fake {
        process: Process,
        like: Mode,
        /// Whether `like` has an ACL: until the file has it too, only its
        /// owner may be let in.
        like_acl: bool,
        now: RefCell<(Mode, Option<Vec<u8>>)>,
        steps: RefCell<Vec<&'static str>>,
    }

    impl Fake {
        fn new(process: Process, like: &Access) -> Self {
            let now = Mode {
                uid: process.uid,
                gid: process.groups[0],
                bits: like.mode.bits & 0o600,
            };
            Self {
                process,
                like: like.mode,
                like_acl: like.acl != Some(None),
                now: RefCell::new((now, None)),
                steps: RefCell::default(),
            }
        }

        fn owns(&self) -> bool {
            self.now.borrow().0.uid == self.process.uid || self.process.may_chmod
        }

        /// Make a change, then compare what each of these people may do with
        /// what they might before: those of the groups, and anyone.
        fn step(&self, name: &'static str, change: impl FnOnce(&mut (Mode, Option<Vec<u8>>))) {
            change(&mut self.now.borrow_mut());
            self.steps.borrow_mut().push(name);
            let now = self.now.borrow();
            let class = |mode: &Mode, gid: u32| {
                if mode.gid == gid {
                    (mode.bits >> 3) & 7
                } else {
                    mode.bits & 7
                }
            };
            for gid in [self.like.gid, now.0.gid, u32::MAX] {
                let before = class(&self.like, gid);
                let gained = class(&now.0, gid) & !before;
                assert_eq!(gained, 0, "group {gid} gained by {name}: {:o}", now.0.bits);
            }
            if self.like_acl && now.1.is_none() {
                assert_eq!(now.0.bits & 0o077, 0, "open without its ACL by {name}");
            }
            if now.0.bits & 0o4000 != 0 {
                assert_eq!(now.0.uid, self.like.uid, "set-user-ID of another: {name}");
            }
            if now.0.bits & 0o2000 != 0 {
                assert_eq!(now.0.gid, self.like.gid, "set-group-ID of another: {name}");
            }
        }
    }

    fn denied<T>() -> io::Result<T> {
        Err(io::ErrorKind::PermissionDenied.into())
    }

    impl Staged for Fake {
        fn mode(&self) -> io::Result<Mode> {
            Ok(self.now.borrow().0)
        }

        fn set_bits(&self, bits: u32) -> io::Result<()> {
            if !self.owns() {
                return denied();
            }
            let process = self.process;
            self.step("bits", |now| {
                now.0.bits = bits;
                // Linux, without `CAP_FSETID`: no error.
                if !(process.may_chown || process.groups.contains(&now.0.gid)) {
                    now.0.bits &= !0o2000;
                }
            });
            Ok(())
        }

        fn set_owner(&self, uid: u32) -> io::Result<()> {
            if !self.process.may_chown {
                return denied();
            }
            self.step("owner", |now| {
                now.0.uid = uid;
                now.0.bits &= 0o1777;
            });
            Ok(())
        }

        fn set_group(&self, gid: u32) -> io::Result<()> {
            let member = self.owns() && self.process.groups.contains(&gid);
            if !(self.process.may_chown || member) {
                return denied();
            }
            self.step("group", |now| {
                now.0.gid = gid;
                now.0.bits &= 0o1777;
            });
            Ok(())
        }

        fn acl(&self) -> io::Result<Option<Vec<u8>>> {
            Ok(self.now.borrow().1.clone())
        }

        fn set_acl(&self, list: Option<&[u8]>) -> io::Result<()> {
            if !(self.owns() && self.process.may_set_acl) {
                return denied();
            }
            let process = self.process;
            let like = self.like.bits & 0o777;
            self.step("acl", |now| {
                now.1 = match list {
                    Some(list) if process.keeps_acl => Some(list.to_vec()),
                    // Less than it was given.
                    Some(list) => Some(list[1..].to_vec()),
                    None if process.drops_acl => None,
                    None => return,
                };
                // The user, the mask and the others of the list.
                if process.posix_acl && now.1.is_some() {
                    now.0.bits = (now.0.bits & 0o7000) | like;
                }
            });
            Ok(())
        }
    }

    fn access(uid: u32, gid: u32, bits: u32, acl: Option<Option<Vec<u8>>>) -> Access {
        Access {
            mode: Mode { uid, gid, bits },
            acl,
        }
    }

    /// Keep `like` as `process`: the mode the file ends with, its ACL, and
    /// what was kept.
    fn kept(process: Process, like: &Access) -> (Mode, Option<Vec<u8>>, Kept) {
        let fake = Fake::new(process, like);
        let kept = keep(&fake, like).expect("not being allowed is no error");
        let (mode, acl) = fake.now.into_inner();
        (mode, acl, kept)
    }

    const ALL: Kept = Kept {
        owner: true,
        bits: true,
        acl: true,
    };

    #[test]
    fn whoever_may_set_it_keeps_everything() {
        let root = Process {
            uid: 0,
            groups: &[0],
            may_chown: true,
            may_chmod: true,
            may_set_acl: true,
            keeps_acl: true,
            drops_acl: true,
            posix_acl: true,
        };
        let list = Some(b"list".to_vec());
        for process in [
            root,
            // Once the file is given away, its mode is no longer root's to
            // set without `CAP_FOWNER`: all but the special bits comes first.
            Process {
                may_chmod: false,
                ..root
            },
        ] {
            for bits in [0o640, 0o664, 0o755, 0o400, 0o000] {
                for acl in [None, list.clone()] {
                    let like = access(1000, 2000, bits, Some(acl.clone()));
                    assert_eq!(kept(process, &like), (like.mode, acl, ALL), "{bits:o}");
                }
            }
        }
        let like = access(1000, 2000, 0o6755, Some(None));
        assert_eq!(kept(root, &like), (like.mode, None, ALL));
    }

    #[test]
    fn a_user_keeps_a_group_of_theirs() {
        for bits in [0o640, 0o660, 0o775, 0o2775] {
            let like = access(1000, 2000, bits, Some(None));
            assert_eq!(kept(USER, &like), (like.mode, None, ALL), "{bits:o}");
        }
    }

    #[test]
    fn an_owner_or_a_group_that_cannot_be_kept_lets_nobody_in() {
        for (uid, gid, bits, kept_gid, kept_bits) in [
            (1000, 3000, 0o640, 1000, 0o600),
            (1000, 3000, 0o664, 1000, 0o644),
            (0, 2000, 0o6664, 2000, 0o2664),
            (0, 3000, 0o6775, 1000, 0o755),
        ] {
            let like = access(uid, gid, bits, Some(None));
            let mode = Mode {
                uid: 1000,
                gid: kept_gid,
                bits: kept_bits,
            };
            let expected = Kept {
                owner: false,
                ..ALL
            };
            assert_eq!(kept(USER, &like), (mode, None, expected), "{bits:o}");
        }
    }

    #[test]
    fn an_acl_that_cannot_be_kept_leaves_the_file_to_its_owner() {
        let unable = Process {
            may_set_acl: false,
            ..USER
        };
        let expected = Kept { acl: false, ..ALL };
        let forgetful = Process {
            keeps_acl: false,
            ..USER
        };
        for (process, acl) in [
            (unable, Some(Some(b"list".to_vec()))),
            (forgetful, Some(Some(b"list".to_vec()))),
            // One that could not be read.
            (USER, None),
        ] {
            for (bits, kept_bits) in [(0o664, 0o600), (0o755, 0o700), (0o4755, 0o4700)] {
                let like = access(1000, 1000, bits, acl.clone());
                let mode = Mode {
                    bits: kept_bits,
                    ..like.mode
                };
                assert_eq!(kept(process, &like), (mode, None, expected));
            }
        }
    }

    /// The group entry of a Linux ACL is about the group the file had.
    #[test]
    fn an_acl_is_not_kept_for_another_group_on_linux() {
        let list = Some(b"list".to_vec());
        let like = access(1000, 3000, 0o664, Some(list.clone()));
        let linux = cfg!(target_os = "linux");
        let process = Process {
            posix_acl: linux,
            ..USER
        };
        let (mode, acl, kept) = kept(process, &like);
        assert_eq!(mode.bits, if linux { 0o600 } else { 0o644 });
        assert_eq!(acl, if linux { None } else { list });
        assert_eq!((kept.owner, kept.acl), (false, !linux));
    }

    /// A temporary file made in a set-group-ID directory has the group of
    /// that, which its owner need not be in.
    #[test]
    fn a_set_group_id_bit_the_system_drops_is_noticed() {
        for (bits, dropped) in [(0o2664, 0o664), (0o6755, 0o4755), (0o664, 0o664)] {
            let like = access(1000, 3000, bits, Some(None));
            let fake = Fake::new(USER, &like);
            fake.now.borrow_mut().0.gid = 3000;

            let kept = keep(&fake, &like).expect("not being allowed is no error");

            assert_eq!(fake.now.borrow().0.bits, dropped);
            let expected = Kept {
                bits: bits == dropped,
                ..ALL
            };
            assert_eq!(kept, expected, "{bits:o}");
        }
    }

    /// What is not read back was not taken away.
    #[test]
    fn an_acl_that_stays_is_an_error() {
        let like = access(1000, 1000, 0o664, Some(Some(b"list".to_vec())));
        let clinging = Process {
            drops_acl: false,
            ..USER
        };
        let fake = Fake::new(clinging, &like);
        assert!(strip_acl(&fake).is_ok(), "there is none");
        fake.set_acl(Some(b"list")).expect("set");

        assert!(strip_acl(&fake).is_err());

        // The same when it is there but is not the one that was wanted.
        let fake = Fake::new(
            Process {
                keeps_acl: false,
                ..clinging
            },
            &like,
        );
        assert!(keep(&fake, &like).is_err());
    }

    #[test]
    fn an_acl_that_cannot_be_read_is_not_none() {
        let file = tempfile::tempfile().expect("temp");
        let mode = Mode::from(&file.metadata().expect("stat"));
        assert_eq!(Access::of(mode, None).acl, None);
        assert_eq!(Access::of(mode, Some(&file)).acl, Some(None));
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            // A descriptor that gives access to the path alone.
            let path = File::options()
                .read(true)
                .custom_flags(libc::O_PATH)
                .open("/")
                .expect("open");
            assert_eq!(Access::of(mode, Some(&path)).acl, None);
        }
    }

    #[test]
    fn another_owner_or_group_gains_nothing() {
        for (bits, same_owner, same_group, expected) in [
            (0o7777, true, true, 0o7777),
            (0o6755, false, true, 0o2755),
            (0o6755, true, false, 0o4755),
            (0o6755, false, false, 0o755),
            // What the old group had is nobody's now.
            (0o640, true, false, 0o600),
            (0o660, true, false, 0o600),
            (0o664, true, false, 0o644),
            (0o775, true, false, 0o755),
            // The members of the old group had less than others.
            (0o604, true, false, 0o600),
            (0o1646, true, false, 0o1644),
            (0o400, false, false, 0o400),
        ] {
            assert_eq!(
                kept_bits(bits, same_owner, same_group),
                expected,
                "{bits:o} {same_owner} {same_group}"
            );
        }
    }
}
