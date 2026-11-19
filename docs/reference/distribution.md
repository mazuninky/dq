# Distribution, releases and CI

How dq is built, versioned, released and installed. The user-facing install
instructions are in the [README](../../README.md#installation); this page
describes the machinery behind them.

## Versioning

Calendar versioning, `YYYY.WW.BUILD`: ISO year, zero-padded ISO week, and a
build counter within the week starting at 1. The version lives in the
workspace `Cargo.toml` (`[workspace.package].version`, inherited by every
crate); the git tag `vYYYY.WW.BUILD` is the source of truth.
`scripts/bump-version.sh` computes the next version, updates `Cargo.toml`
and the lock file, commits and tags (`--dry-run` only prints it).

## Release workflow (`.github/workflows/release.yml`)

Triggered by pushing a tag matching `vYYYY.WW.BUILD`:

1. **verify-version** — the tag must match the workspace version.
2. **build** — a matrix over three targets: `x86_64-unknown-linux-gnu`,
   `aarch64-unknown-linux-gnu` (cross toolchain via
   `taiki-e/setup-cross-toolchain-action`) and `aarch64-apple-darwin`. Each
   job runs `cargo build --release --locked --target … --bin dq`, generates man
   pages and completions with the hidden `dq generate-docs` (native targets
   only), and packages `dq-<version>-<target>.tar.gz` holding the binary,
   `LICENSE`, `README.md`, `man/` and `completions/`, plus a `.sha256`
   sidecar.
3. **release** — collects the archives, writes `dq-checksums.txt` (one
   SHA256 per archive, `sha256sum -c` format), attests SLSA build provenance
   (`actions/attest-build-provenance`), signs with cosign only when a
   `COSIGN_PRIVATE_KEY` secret exists, and publishes the GitHub Release with
   generated notes. The workflow must keep working on a fork without
   secrets.
4. **bump-homebrew-tap** — opens a PR against the `mazuninky/homebrew-tap`
   repository updating `Formula/dq.rb`; skipped without a
   `HOMEBREW_TAP_TOKEN`.

Third-party actions are pinned by commit SHA.

Not shipped, although planned originally: a Windows build
(`x86_64-pc-windows-msvc`, zip archive). See [../roadmap.md](../roadmap.md).

## Container images

- `Dockerfile`: multi-stage, `rust:1.100-alpine` builder, `alpine:3.24`
  runtime, only `/usr/local/bin/dq`, a non-root `dq` user,
  `ENTRYPOINT ["/usr/local/bin/dq"]`, `CMD ["--help"]`,
  `org.opencontainers.image.source` label.
- `Dockerfile.scratch`: `FROM scratch` with a static musl binary, for
  size-sensitive use.
- `.dockerignore` keeps the build context to what `cargo build --bin dq`
  needs.
- `.github/workflows/docker.yml`: on release tags, builds `linux/amd64` and
  `linux/arm64` natively (one runner per architecture), pushes by digest to
  `ghcr.io/mazuninky/dq`, composes a manifest list (`latest`, full tag, bare
  version), attaches SBOM and build provenance. Docker Hub publication and a
  published scratch image were planned but are not part of the workflow.

## Install paths

- **`scripts/install.sh`** — POSIX `sh` (`set -eu`, no bash-isms, no `jq`,
  BusyBox-compatible). Detects the target from `uname`, downloads the archive
  and `dq-checksums.txt`, verifies SHA256 (`sha256sum` or `shasum -a 256`),
  extracts in a temp dir cleaned up by `trap`, installs to `~/.local/bin`
  (non-root) or `/usr/local/bin` (root), runs `dq --version` as a self-test,
  and warns when the install directory is not on `PATH`. Flags: `--version`,
  `--install-dir`, `--repo OWNER/NAME`, `--no-modify-path`. It does not run
  `gh attestation verify`; the README describes that stronger path.
- **Homebrew** — `packaging/homebrew/dq.rb` is the formula template (binary,
  man pages, completions, a `test do` block checking `dq --version`); the
  release job pushes the real version and checksums to the tap.
- **AUR** — `packaging/aur/PKGBUILD` installs the Linux binary, man pages and
  bash/zsh/fish completions. There is no `.SRCINFO` and no submission
  automation yet.
- **From source** — `cargo install --locked --git … dq-cli`, with
  `--features plugins` for the WASM runtime.

## `dq self check` / `dq self update`

`self check` makes one GitHub API call (`ureq`) to the latest release and
compares its `tag_name` with the running version: "up to date", "newer
version available" (suggesting `dq self update`) or "running pre-release
version" — exit 0 in all three cases, 5 on network errors. A rate-limited
response (403 with `X-RateLimit-Remaining: 0`) adds a hint to set
`GITHUB_TOKEN`.

`self update [--to VER]` downloads the release archive for the running
platform through the `self_update` crate and replaces the binary atomically.
Exit 0 on success, 5 on network errors, 6 when the binary lives where the
user cannot write (with a `sudo dq self update` hint), 7 when the replace
fails. `--to` may also install an older release.

**No checksum is verified** by `self update` today: the `self_update`
crate's `checksums` feature is not enabled, contrary to what the README
says. This is an open item in [../roadmap.md](../roadmap.md).

## CI (`.github/workflows/`)

- `ci.yml` — on pushes to `master` and on pull requests: `cargo fmt --check`
  and `cargo clippy --workspace --all-targets --all-features --locked -D
  warnings` on Linux; `cargo build --workspace --all-targets --locked` and
  `cargo test --workspace --all-features --locked` on Linux for PRs and on
  Linux and macOS for pushes; `cargo deny check` on Linux. No Windows job.
- `audit.yml` — `cargo audit` (RustSec) daily and when `Cargo.toml` /
  `Cargo.lock` change on `master`.
- `codeql.yml` — CodeQL analysis.
- `docker.yml`, `release.yml` — above.

The toolchain is pinned to 1.100 in `rust-toolchain.toml` (edition 2024).
Release profile: `lto = true`, `codegen-units = 1`, `strip = true`.

Locally, `lefthook.yml` runs fmt, clippy and `cargo nextest run` before a
commit and `cargo deny check` before a push.

## Pre-commit hooks

`.pre-commit-hooks.yaml` at the repository root offers `dq-fmt-check`
(`dq fmt --check`) and `dq-validate` (`dq validate`) for YAML, JSON, TOML
and JSONL files, with `language: system` (dq must be on `PATH`).

## Claude Code skill

`skills/dq/` (`SKILL.md`, `skill.json`, `references/`, `evals/`) is the
skill published to skills.sh (`npx skills add mazuninky/dq`). It covers
install, common patterns, formats, exit codes and anti-scope. The skill is
the only skill meant to be published from this repository.
