//! The agent skill the binary carries: writing it to the disk, keeping it at
//! the version of the binary, removing it. `docs/plans/skill-command.md` is
//! the design.
//!
//! What dq wrote is known by a manifest in the skill directory
//! ([`manifest`]). Nothing here overwrites or deletes a file the manifest
//! does not vouch for, unless `--force` says so; nothing is written through a
//! symbolic link in the place of the skill directory or inside it.

mod content;
mod disk;
mod error;
mod first_run;
mod link;
mod manifest;
mod ops;
mod place;
mod survey;

use std::ffi::OsString;

use camino::{Utf8Path, Utf8PathBuf};

pub use content::{Edition, SkillFile};
pub use error::{Refusal, SkillError};
pub use first_run::{FirstRun, Outcome, Skipped};
pub use link::{ClaudeLink, LinkState, Made};
pub use manifest::{MANIFEST, Manifest, ManifestError, SCHEMA};
pub use ops::{
    Freshness, RemoveReport, State, Status, WriteReport, install, installed_note,
    installed_version, remove, status, update,
};

pub(crate) use error::{listed, listed_up_to};

/// The skill directory, relative to the home directory or a project's.
const SKILL_DIR: &str = ".agents/skills/dq";

/// The link to it, relative to Claude Code's directory.
const CLAUDE_LINK: &str = "skills/dq";

/// Claude Code's directory, relative to the home directory or a project's.
const CLAUDE_DIR: &str = ".claude";

/// What the process environment says about where the skill is and whether it
/// may be updated without being asked. Filled in once, at the edge, so that
/// everything else can be tested on a directory of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// `HOME`.
    pub home: Option<OsString>,
    /// `CLAUDE_CONFIG_DIR`.
    pub claude_config_dir: Option<OsString>,
    /// `CI`.
    pub ci: Option<OsString>,
    /// `DQ_NO_SKILL_UPDATE`.
    pub no_skill_update: Option<OsString>,
    /// The effective user id; `None` where there is none.
    pub euid: Option<u32>,
    /// The current directory, for `--project`; `None` when there is none
    /// dq can use, or nobody asked ([`Environment::with_current_dir`]).
    pub current_dir: Option<Utf8PathBuf>,
}

impl Environment {
    /// The environment of this process, without its current directory.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            home: std::env::var_os("HOME"),
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR"),
            ci: std::env::var_os("CI"),
            no_skill_update: std::env::var_os("DQ_NO_SKILL_UPDATE"),
            euid: effective_user(),
            current_dir: None,
        }
    }

    /// With the current directory of this process.
    #[must_use]
    pub fn with_current_dir(mut self) -> Self {
        self.current_dir = std::env::current_dir()
            .ok()
            .and_then(|dir| Utf8PathBuf::from_path_buf(dir).ok());
        self
    }

    /// The home directory, as `HOME` names it.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoHome`] when it is unset or empty, [`Refusal::BadHome`]
    /// when it is no absolute UTF-8 path.
    pub fn home(&self) -> Result<&Utf8Path, Refusal> {
        let home = self
            .home
            .as_deref()
            .filter(|home| !home.is_empty())
            .ok_or(Refusal::NoHome)?;
        Utf8Path::from_path(home.as_ref())
            .filter(|home| home.is_absolute())
            .ok_or_else(|| Refusal::BadHome {
                home: home.to_string_lossy().into_owned(),
            })
    }

    /// Whether `DQ_NO_SKILL_UPDATE` asks dq to leave the skill alone: set to
    /// anything but `0`.
    #[must_use]
    pub fn opted_out(&self) -> bool {
        self.no_skill_update
            .as_deref()
            .is_some_and(|value| value != "0")
    }

    /// The user `dir` belongs to, when dq runs as root and that is someone
    /// else (`sudo`): what dq wrote there would be root's.
    fn foreign_owner(&self, dir: &Utf8Path) -> Option<u32> {
        if self.euid != Some(0) {
            return None;
        }
        place::owner(dir).filter(|owner| *owner != 0)
    }

    /// Whether dq runs as root while the home directory is another user's.
    #[must_use]
    pub fn is_root_in_foreign_home(&self) -> bool {
        self.home()
            .is_ok_and(|home| self.foreign_owner(home).is_some())
    }

    /// The skill of the home directory.
    ///
    /// The link is wanted when Claude Code is there: when the directory
    /// `CLAUDE_CONFIG_DIR` names, or `~/.claude` when it is unset, exists.
    ///
    /// # Errors
    ///
    /// A [`Refusal`] when `HOME` is no directory or `CLAUDE_CONFIG_DIR` no
    /// absolute path.
    pub fn global(&self) -> Result<Target, Refusal> {
        let home = self.home()?;
        if !home.is_dir() {
            return Err(Refusal::BadHome {
                home: home.to_string(),
            });
        }
        let claude = match self.claude_config_dir.as_deref() {
            Some(dir) if !dir.is_empty() => Utf8Path::from_path(dir.as_ref())
                .filter(|dir| dir.is_absolute())
                .ok_or_else(|| Refusal::BadClaudeDir {
                    dir: dir.to_string_lossy().into_owned(),
                })?
                .to_path_buf(),
            _ => home.join(CLAUDE_DIR),
        };
        Ok(Target {
            foreign_owner: self.foreign_owner(home),
            user: self.euid,
            ..Target::new(home, &claude)
        })
    }

    /// The skill of the project in the current directory. Whom that belongs
    /// to does not matter: root in a checkout of another user's is what a
    /// container or a CI job is.
    ///
    /// # Errors
    ///
    /// [`Refusal::NoCurrentDir`] when there is none.
    pub fn project(&self) -> Result<Target, Refusal> {
        let dir = self.current_dir.as_deref().ok_or(Refusal::NoCurrentDir)?;
        Ok(Target::new(dir, &dir.join(CLAUDE_DIR)))
    }
}

/// Where one skill is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The home directory, or the project's.
    pub root: Utf8PathBuf,
    /// The skill directory.
    pub dir: Utf8PathBuf,
    /// The link to it; `None` where Claude Code is not, or links are not.
    pub link: Option<ClaudeLink>,
    /// Whom the home directory belongs to, when this is its skill, dq runs as
    /// root and that is someone else: nothing is written or deleted then.
    pub foreign_owner: Option<u32>,
    /// Whom the skill directory has to belong to, when this is the skill of
    /// the home directory: the user dq runs as, where there are users.
    /// Nothing is written or deleted in anyone else's.
    pub user: Option<u32>,
}

impl Target {
    /// The skill of `root`, where Claude Code keeps its files in `claude`.
    fn new(root: &Utf8Path, claude: &Utf8Path) -> Self {
        let link = (cfg!(unix) && claude.is_dir()).then(|| ClaudeLink {
            place: claude.join(CLAUDE_LINK),
            root: root.to_path_buf(),
            inside: SKILL_DIR.into(),
        });
        Self {
            root: root.to_path_buf(),
            dir: root.join(SKILL_DIR),
            link,
            foreign_owner: None,
            user: None,
        }
    }
}

#[cfg(unix)]
fn effective_user() -> Option<u32> {
    // SAFETY: `geteuid(2)` takes nothing, touches no memory and cannot fail.
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn effective_user() -> Option<u32> {
    None
}

/// Shared by the tests of the skill.
#[cfg(all(test, unix))]
pub(crate) mod testing {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    /// Make sure that `dir`, a directory of the test's, is not root's, also
    /// when root runs the tests. The id of its owner.
    pub(crate) fn not_roots(dir: &Path) -> u32 {
        let owner = || std::fs::metadata(dir).expect("metadata").uid();
        if owner() == 0 {
            std::os::unix::fs::chown(dir, Some(12345), None).expect("chown");
        }
        owner()
    }
}
