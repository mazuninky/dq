# distribution Specification (delta)

## ADDED Requirements

### Requirement: Prebuilt binaries for four targets per release

Every tagged release SHALL ship prebuilt binaries for the following targets, packaged as `tar.gz` (Unix) or `zip` (Windows):
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Each artifact MUST contain the `dq` binary at the archive root and a `man/` and `completions/` directory with the matching man pages and shell completions for that release. The artifact filename MUST follow the pattern `dq-v<VERSION>-<TARGET>.{tar.gz,zip}`.

#### Scenario: All four targets present in a release
- **WHEN** a tag matching `v*` is pushed and the release workflow completes
- **THEN** the GitHub Release page contains four archives matching the pattern above

### Requirement: SHA256 checksums file alongside artifacts

Every release SHALL include a `dq-checksums.txt` file listing one SHA256 hash per artifact, in the standard `<hex> *<filename>` format consumable by `sha256sum --check`. The checksums file MUST be regenerated as part of the release workflow (not committed to the repo) and uploaded to the GitHub Release.

#### Scenario: Install script verifies checksum before installing
- **WHEN** `scripts/install.sh` downloads a release artifact
- **THEN** it downloads `dq-checksums.txt` and verifies the artifact's SHA256 with `shasum -a 256 -c` (BSD/macOS) or `sha256sum -c` (Linux) before extracting

### Requirement: POSIX-sh `install.sh` installer

`scripts/install.sh` SHALL be a POSIX-sh script (`#!/bin/sh`, `set -eu`) that installs `dq` from GitHub Releases. It SHALL:
- Detect target via `uname -s` and `uname -m`, mapping to release naming.
- Accept `--version vX.Y.Z` (default: latest), `--install-dir DIR` (default: `~/.local/bin` for non-root, `/usr/local/bin` for root), `--no-modify-path`, `--repo OWNER/NAME` flags.
- Download the appropriate artifact + the `dq-checksums.txt` file.
- Verify the SHA256.
- Extract to a tempdir, move `dq` to `INSTALL_DIR`, chmod +x.
- Run `dq --version` as a self-test.
- Print a "Add `INSTALL_DIR` to your PATH" warning if the directory is not on `$PATH` (suppressed by `--no-modify-path`).
- `trap`-cleanup the tempdir on any exit path.

The script MUST NOT depend on `jq`, `curl`-only-flags absent from BusyBox (for alpine compatibility), or shell-specific features (no bash-isms).

#### Scenario: Curl-pipe-sh installs latest
- **WHEN** the user runs `curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/main/scripts/install.sh | sh`
- **THEN** the latest release's `dq` binary is installed to `~/.local/bin/dq` (or `/usr/local/bin/dq` if running as root) and `dq --version` prints the installed version

#### Scenario: --version pins to a specific release
- **WHEN** the user runs `bash scripts/install.sh --version v0.5.0`
- **THEN** the `v0.5.0` artifact is fetched, verified, and installed

### Requirement: Multi-stage alpine Dockerfile

The repo SHALL contain a `Dockerfile` that:
- Uses `rust:1.94-alpine` for the build stage and `alpine:3.21` for the runtime stage.
- Ships only the `dq` binary in `/usr/local/bin/dq` in the runtime stage.
- Adds a non-root `dq` user (UID 1000) and runs as that user.
- Sets `ENTRYPOINT ["/usr/local/bin/dq"]` and `CMD ["--help"]`.
- Includes the `org.opencontainers.image.source` LABEL pointing at the GitHub repo.

A second `Dockerfile.scratch` SHALL provide a `FROM scratch` minimal image using a static-musl-linked binary, for size-sensitive consumers.

A `.dockerignore` SHALL exclude `target/`, `.git/`, `.github/`, `.claude/`, `.openspec/`, `docs/`, `spikes/`, `*.md`, `*.lock`, and `.DS_Store`.

#### Scenario: Docker image built locally
- **WHEN** the user runs `docker build -t dq:test .`
- **THEN** the build completes successfully

#### Scenario: Docker image runs as non-root
- **WHEN** the user runs `docker run --rm dq:test --version`
- **THEN** the binary's version is printed and the container exits 0

### Requirement: Homebrew formula template in repo

`packaging/homebrew/dq.rb` SHALL be a Homebrew formula that downloads the appropriate macOS or Linux tarball from the GitHub Release for the running platform, installs the `dq` binary, man pages, and shell completions. The formula MUST include a `test do` block that verifies `dq --version` matches the formula's version.

The formula's `version`, `url`, and `sha256` fields are placeholders rewritten by the tap repo's release automation on each tag. The formula in this repo is the canonical source; the `mazuninky/homebrew-tap` repo is external infrastructure tracked alongside the v0.1 release.

#### Scenario: Formula installs binary and completions
- **WHEN** a user runs `brew install mazuninky/tap/dq` after the tap repo has been bootstrapped
- **THEN** the binary lands in Homebrew's bin path and the formula's `test do` block passes

### Requirement: AUR PKGBUILD template in repo

`packaging/aur/PKGBUILD` SHALL be an Arch Linux PKGBUILD that downloads the appropriate Linux tarball (`x86_64` or `aarch64`) from the GitHub Release, installs the binary to `/usr/bin/dq`, man pages to `/usr/share/man/man1/`, and shell completions to the standard Arch completion paths (`/usr/share/bash-completion/completions/`, `/usr/share/zsh/site-functions/`, `/usr/share/fish/vendor_completions.d/`).

A `packaging/aur/.SRCINFO` companion file SHALL be present so the AUR submission does not require running `makepkg --printsrcinfo` on a maintainer machine.

The `pkgver` field is a placeholder rewritten by the AUR submission automation on each tag.

#### Scenario: PKGBUILD validates with namcap (offline)
- **WHEN** an Arch maintainer runs `namcap PKGBUILD`
- **THEN** namcap reports no errors (warnings about the placeholder SHA256 are acceptable until the first real release fills them in)

### Requirement: Claude Code skill manifest in repo

`skill/SKILL.md` and `skill/skill.json` SHALL be present and conform to the Anthropic skill manifest schema (front-matter with `name`, `description`, `version`, `triggers`; body with sections covering install, common patterns, format coverage, exit codes, anti-scope).

The skill is installable via the standard Claude Code skill workflow once the repo is published. Submitting `mazuninky/dq` to the public `skills.sh` registry is a post-release administrative step tracked alongside the v0.1 announcement.

#### Scenario: Skill manifest parses
- **WHEN** the skill creator skill or any standard YAML-frontmatter parser reads `skill/SKILL.md`
- **THEN** the front-matter parses successfully and exposes `name`, `description`, `version`, `triggers` fields

### Requirement: GitHub Actions release workflow

`.github/workflows/release.yml` SHALL trigger on `push` of any tag matching `v*` (and on `workflow_dispatch` for manual re-runs). It SHALL:
- Run a build matrix over the four targets defined above. Each job builds via `cargo build --release --locked --target $TARGET`.
- Generate man pages and shell completions via the existing hidden `dq generate-docs --output-dir DIR` command.
- Package each target's binary + docs into a `tar.gz` (Unix) or `zip` (Windows) archive.
- After all matrix jobs complete, run a release job that downloads all artifacts, generates `dq-checksums.txt`, optionally signs with `cosign` if `secrets.COSIGN_PRIVATE_KEY` is configured (no-op otherwise), and uploads everything to a new GitHub Release via `softprops/action-gh-release@v2`.

The workflow MUST work on a fresh fork without any secrets configured (cosign signing is the only optional step).

The third-party action vendors used MUST be limited to `actions/`, `dtolnay/`, `softprops/`, and `taiki-e/` (the last only for the linux aarch64 cross-toolchain step).

#### Scenario: Tag push triggers release build
- **WHEN** a tag matching `v*` is pushed to the repo
- **THEN** the release workflow runs, builds all four targets, and publishes a GitHub Release with the artifacts and `dq-checksums.txt`

#### Scenario: Cosign signing is optional
- **WHEN** the workflow runs in a fork without `COSIGN_PRIVATE_KEY` configured
- **THEN** the cosign step is skipped and the workflow completes successfully

### Requirement: GitHub Actions CI workflow

`.github/workflows/ci.yml` SHALL trigger on `push` and `pull_request`. It SHALL run a matrix over `ubuntu-latest`, `macos-14`, `windows-latest` with steps:
- Checkout
- Install the Rust stable toolchain via `dtolnay/rust-toolchain@stable` (the workspace is pinned to `1.94` in `rust-toolchain.toml`, which `rustup` honours; the action installs whatever stable channel is current and lets the toolchain file pin the exact version)
- `cargo build --workspace --all-targets`
- `cargo test --workspace --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo fmt --all -- --check`

The Linux job additionally runs `cargo deny check` to enforce license + advisory policy. macOS and Windows jobs skip `cargo deny check` (the policy is platform-independent; one OS is enough).

#### Scenario: PR triggers full CI matrix
- **WHEN** a pull request is opened against `main`
- **THEN** all three OS jobs run all five steps and the PR cannot be merged until they pass
