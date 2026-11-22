//! `dq skill` — the agent skill of this dq (`install`, `update`, `status`,
//! `remove`).

use clap::{Args, Subcommand};

/// Arguments for `dq skill <SUBCOMMAND>`.
#[derive(Debug, Args)]
pub struct SkillArgs {
    /// Operation to perform on the skill.
    #[command(subcommand)]
    pub command: SkillCommand,
}

/// Subcommands of `dq skill`.
#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Write the skill, and the link to it for Claude Code.
    Install(SkillWriteArgs),
    /// Bring an installed skill to the version of this dq.
    Update(SkillWriteArgs),
    /// Say where the skill is, of which version, and whether it was changed.
    Status(SkillStatusArgs),
    /// Remove the files dq installed, and the link to them.
    Remove(SkillWriteArgs),
}

/// Arguments for `dq skill install`, `update` and `remove`.
#[derive(Debug, Args)]
pub struct SkillWriteArgs {
    /// Work on the skill of the current directory (`.agents/skills/dq`)
    /// instead of the home directory's.
    #[arg(long)]
    pub project: bool,

    /// Go ahead where files were changed since dq wrote them, where others
    /// are in their place, or where dq did not install the directory: dq's
    /// own files are replaced (or removed), every other file stays.
    ///
    /// Not overridden: a skill directory that is a symbolic link, a manifest
    /// of a newer dq, a directory in the place of a file of the skill, a file
    /// or a directory in the place of Claude Code's link (another link there
    /// is replaced), root in the home directory of another user.
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `dq skill status`.
#[derive(Debug, Args)]
pub struct SkillStatusArgs {
    /// Look at the skill of the current directory (`.agents/skills/dq`)
    /// instead of the home directory's.
    #[arg(long)]
    pub project: bool,
}
