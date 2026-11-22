# `dq skill`: the agent skill

The skill for coding agents is embedded in the binary, so it always describes
the version it came with. `dq skill` needs no network.

| Command | What it does |
|---|---|
| `dq skill install [--project] [--force]` | Write the skill and the Claude Code link. |
| `dq skill update [--project] [--force]` | Bring an installed skill to this version. With none installed: a note, exit 0. |
| `dq skill status [--project]` | Where it is, which version, what was changed; `-F json` for a fixed set of keys. |
| `dq skill remove [--project] [--force]` | Remove what dq wrote. |

## Where

- `~/.agents/skills/dq/`, a directory several agents read.
- When Claude Code is present (`$CLAUDE_CONFIG_DIR`, else `~/.claude`), a
  relative symbolic link to it in its `skills/` directory. Only `install` makes
  the link.
- `--project`: `.agents/skills/dq/` and `.claude/skills/dq` of the current
  directory.

`.dq-skill.json` in the skill directory lists the version and the SHA-256 of
each file dq wrote. While dq writes, it also holds the digests the run is going
to leave (`unfinished`), so a run that was cut short is finished by the next one,
of any version.

## What dq does not touch

Refused with exit 6, naming what is in the way:

- **A file of dq's that was edited.** `--force` replaces it.
- **A directory dq did not install.** `--force` writes dq's files into it and
  leaves the rest. A file or link where dq writes one is replaced only with
  `--force`, a directory never.
- **A skill directory that is itself a symbolic link** (a checkout someone works
  on): never, `--force` or not.
- **Something in place of the Claude Code link**: another link is replaced with
  `--force`, a file or directory never.
- **A manifest of a newer dq, or one that cannot be read** or is damaged: over
  256 files or 128 KiB, or with a path over 255 bytes, among others. A damaged
  one can be taken over with `install --force`; `remove` needs a manifest.
- **A file that was saved while dq ran**: it is neither what dq wrote nor what it
  saw. The run stops there, and the next one goes on.
- **A skill directory of another user**, or, as root, one whose nearest existing
  directory is another user's.
- **Something that is no directory in place of `~/.claude/skills`** (a link to
  nothing too), when `install` would make the link there.
- **Something that is no directory in place of `.agents` or `.agents/skills`**,
  a link to nothing included: never, `--force` or not. `status` says `blocked`.
- **Root in someone else's home** (`sudo`).

Other files in the directory are yours: they are ignored, and `remove` leaves
the directory if there are any, naming up to three.

Commands that write lock the skill directory and wait at most 10 seconds for
another dq, then exit 7.

`remove` takes away the Claude Code link of the `CLAUDE_CONFIG_DIR` it is run
with. With another one than at install, the old link stays and leads to nothing.

## Automatic update

After a command, the first run of a newer dq updates the global skill, if dq
wrote it and no file of it was edited or is missing. It prints nothing and does
not change the exit code; a failure is a debug log line. An edited skill is left
alone: `dq self check` and `dq skill status` say so.

It does not run under `CI`, with `DQ_NO_SKILL_UPDATE` set to anything but `0`,
as root in someone else's home or with a skill directory of another user, for
`dq skill` and `dq self`, and in debug builds. It never waits for a lock, never
installs a skill, never goes back to an older version, and leaves a project's
skill alone. A file dq wrote is replaced atomically, runs at the same time do not
disturb each other, and a file that is saved meanwhile is left alone.

`dq self update` updates the skill with the binary (`--no-skill` leaves it).

## `status -F json`

One object with the keys `path`, `installed`, `state`, `version`, `dq_version`,
`edited`, `missing`, `colliding`, `in_the_way`, `unlisted`, `link` and
`link_state`. `colliding` lists files that are not dq's where `install` or
`update` writes one (`--force` replaces them); `in_the_way` lists what is never
replaced, such as a directory; `unlisted` can hold a directory dq may not read.

## Exit codes

0 done; 5 / 7 a file could not be read / written; 6 refused.
