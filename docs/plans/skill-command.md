# `dq skill`: the agent skill ships in the binary and updates with it

**Status: done, released in 2026.48.1.** The owner's decisions are at the end. Current behaviour: [reference/skill.md](../reference/skill.md).

## Problem

The skill under `skills/dq/` describes one version of dq. Whoever installed it keeps that version: nothing updates it when dq is updated, so an agent works from a description of flags and exit codes that are months old. `dq self update` does not know about it, and a package manager cannot: Homebrew runs `post_install` in a sandbox with a temporary `HOME`.

## Goal

- dq installs its own skill, offline, in the version of the binary.
- When dq is updated, by whatever means, an installed skill follows.
- A skill the user edited, or that dq did not install, is never overwritten without being asked.
- If no skill is installed, dq never creates one and writes nothing.

## Design

### The skill is in the binary

`SKILL.md` and `references/*.md` (about 60 KB) are embedded with `include_str!`, as the `@std` rules are. `evals/` stays in the repository. A test compares the embedded list with the directory, so a new reference file cannot be forgotten. `skill.json` is removed: nothing reads it.

### `dq skill`

| command | what it does |
|---|---|
| `dq skill install [--project] [--force]` | write the skill |
| `dq skill update [--project] [--force]` | bring an installed skill to this version; exit 0 and a note if none is installed |
| `dq skill status [--project]` | where it is, which version, whether it was edited; `-F json` |
| `dq skill remove [--project] [--force]` | remove what dq installed |

- **Where.** `~/.agents/skills/dq/`, the directory several agents share, and, when Claude Code is present (`$CLAUDE_CONFIG_DIR` or `~/.claude` exists), a relative symbolic link to it in its `skills/` directory. `--project` uses `.agents/skills/dq/` and `.claude/skills/dq` under the current directory.
- **What dq installed** is known by a manifest in the skill directory, `.dq-skill.json`: the dq version and the SHA-256 of every file it wrote.
- **Nothing is lost.** A save the user makes during a run stops the run half-way; the next one goes on. A directory or link without the manifest, or a file whose hash differs from it, stops `install`, `update` and `remove` (exit 6, naming it); something else where dq writes a file stops `install` and `update`. `--force` goes ahead, but never replaces a directory. Files the manifest does not list are the user's and block nothing. `install` and `update` replace only their own files; `remove` deletes only those, and the directory if it is then empty.
- **Never through a link.** If `dq` in `.agents/skills` is itself a symbolic link (a checkout someone works on), every command but `status` refuses, with `--force` too. So they do when `.agents` or `.agents/skills` is no directory.
- **Writes** go file by file through the atomic write dq already has, bound to the file that was read, and never through a link. The manifest is written before and after, so an interrupted run leaves either version of each file and is repaired by the next one, whatever version it is. Commands that write take a lock on the skill directory and wait at most 10 seconds for it; the automatic update does not wait.

### Updating

1. **On the first run of a new version.** After a command is done, dq looks for the manifest. If the skill it describes is older than the binary and unedited, dq rewrites it. No output, no effect on the exit code; a failure is a debug log line. An edited skill is left alone; `dq self check` and `dq skill status` say so.
2. **`dq self update`** does not wait for that: after replacing the binary it runs the new one with `skill update`. A failure there is a warning. `--no-skill` leaves the skill alone.

The check is one `stat` when no skill is installed and one small read when one is. It is skipped when:

- `CI` is set, or `DQ_NO_SKILL_UPDATE` is set to anything but `0`;
- the manifest's version is the same or newer, so two dq binaries on one machine do not fight;
- dq runs as root while the home directory belongs to someone else (`sudo`);
- the build is not a release build, so developing dq does not touch the developer's skill;
- the command is `dq skill …` or `dq self …`.

Only the global skill is updated this way. A project's skill is under version control, and changing it is the project's decision.

### What it does not do

No network, no Node, no other agents' configuration files, no rules or instruction files.

## Why a write on an ordinary command

dq's principle is one command, one operation, no hidden state, and this is an exception to it, made by the owner: an update through a package manager has no other moment at which the skill could follow, and a skill nobody remembers to update is the problem. The exception is narrow: only files dq wrote, only in the directory the user installed them to, only when the version changes, with an opt-out.

## Phases

1. **`dq skill`** with the embedded content, reference page, README, tests.
2. **The update on the first run and in `self update`**, `self check` line, tests with a real home directory in a temporary place.
3. **Docs:** README and `distribution.md` name `dq skill install` as the way to install; CHANGELOG; `decisions.md`.

`atl` gets the same command with its own implementation.

## Risks

- **Tests that run the real binary must not touch the developer's home.** Release builds only is the first guard; the test helpers set `HOME` as the second, and a test checks that they do.
- **Windows** is not a dq platform today; no link is made there.
- **Agents read the skill while it is replaced.** Each file is replaced atomically; a session that loads it in between can see files of two versions once.

## Decisions (owner, 2026-11-22)

1. The skill is updated on the first run of a new version, not by a hint to run a command.
2. The command is `dq skill install | update | status | remove`.
3. Claude Code and the shared `~/.agents/skills`; no list of agents.
4. No migration from other installers: dq neither reads nor changes their files. A skill directory dq did not create is a foreign directory like any other.
