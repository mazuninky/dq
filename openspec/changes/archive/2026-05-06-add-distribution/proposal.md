## Why

M1–M5 made `dq` functionally complete for the read+write+convert+fmt scope across ten formats. What it cannot do yet — and what blocks the *AI agents in CI/CD* audience from picking it up — is **install in one line, update without `cargo install`, and emit machine-readable diagnostics that GitHub Actions / Jenkins / GitLab understand**. Without that, every adoption story stalls at "first you build a Rust toolchain into the runner image, then…".

The M6 envelope per [dq-plan.md:413-433](../../../dq-plan.md) is "tool installs in one command, updates in-place, has a Claude Code skill, ships SARIF/JUnit/TAP output for CI, and has signed prebuilt binaries for four targets". The four targets are the standard Rust-CLI quartet: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`. Cross-compilation runs in GitHub Actions matrix (no `cross` dependency — the matrix builds natively on each runner OS).

Risk envelope is moderate. The Rust additions (three new subcommands, one new reporter) are small and follow the same Reporter / DI patterns established in M1. The packaging additions (install.sh, GH workflow, Dockerfile, Homebrew/AUR templates, skill manifest) are mostly text files with well-trodden shapes; the only invariant they have to preserve is that `--format json` / `--format sarif` output is byte-stable so CI integrations don't churn on every release. **Self-update is the one place a bug is user-visible**: a botched download must NOT corrupt the existing binary on disk. The implementation uses the `self_update` crate which already gets atomic-replace right (download → verify SHA256 → atomic rename), so we pay exactly that crate's audited maturity instead of rolling our own.

## What Changes

### CLI surface

- **`dq completions <shell>` (user-facing).** New top-level subcommand. Writes the completion script for `bash` / `zsh` / `fish` / `powershell` / `elvish` to stdout. Distinct from the existing hidden `dq generate-docs --output-dir DIR` (which writes a directory tree of files for packaging scripts) — `completions` is the documented end-user entry point: `dq completions bash | sudo tee /etc/bash_completion.d/dq`.
- **`dq man [PAGE]` (user-facing).** New top-level subcommand. With no argument, writes the top-level `dq.1` man page to stdout. With `dq man get`, writes the `dq-get.1` page. Lets users do `dq man | man -l -` without installing anything.
- **`dq self check` (user-facing).** Queries `https://api.github.com/repos/mazuninky/dq/releases/latest`, compares the `tag_name` to the running binary's `--version`, prints one of three messages: "up to date", "newer version available: vX.Y.Z (run `dq self update` to install)", or "running pre-release version" (when local version > remote tag). Exit 0 in all three cases; exit 5 on network error so CI can detect "could not check".
- **`dq self update [--to <ver>]` (user-facing).** Downloads the appropriate prebuilt binary from GitHub Releases for the running platform/arch, verifies the SHA256 from the published `dq-checksums.txt`, atomically replaces the running binary using the `self_update` crate. `--to <ver>` pins to a specific tag (sideways downgrade allowed). Exit codes: 0 success, 5 network error, 7 atomic replace failed. **Refuses** to operate when the binary lives under a system path the current user can't write (`/usr/local/bin/dq` for non-root) — exits 6 with a `sudo dq self update` hint.
- **SARIF reporter (`-F sarif`).** New `OutputFormat::Sarif` variant. Implements the `Reporter` trait. Per the spec, `validate` is the only M6 consumer (it emits one `result` per parse error with line/col/snippet). Lint engine consumers in M8 will reuse the same reporter. JUnit and TAP are explicitly **deferred to M8** when there are real diagnostics to render; M6 ships only SARIF because GitHub PR annotations and GitLab Code Quality reports both consume it directly.

### Packaging

- **`scripts/install.sh`.** POSIX-sh curl-pipe-sh installer. Detects OS (`uname -s`) + arch (`uname -m` mapped to release naming). Pulls latest release (or `--version vX.Y.Z`), verifies SHA256 against `dq-checksums.txt`, installs to `--install-dir` (default `~/.local/bin`, falls back to `/usr/local/bin` with `sudo` if the user runs as root). Self-tests: `dq --version` after install. Exits non-zero on any failure with a clear message.
- **`.github/workflows/release.yml`.** Triggered on `push` of any tag matching `v*`. Matrix over four targets (`ubuntu-latest`/`x86_64`, `ubuntu-latest`/`aarch64` via cross-rs, `macos-14`/`aarch64` native, `windows-latest`/`x86_64` native). Each job: `cargo build --release --locked --target $TARGET`, strips, tarballs (`tar.gz` for *nix, `zip` for Windows), uploads. A `release` job collects all artifacts, generates `dq-checksums.txt` (SHA256), creates the GitHub Release, attaches the tarballs + checksums + man pages + completions tarball. Optional `cosign` signing if `COSIGN_PRIVATE_KEY` secret is configured (no-op otherwise — keeps the workflow green for forks).
- **`.github/workflows/ci.yml` (if missing — the CI baseline).** Runs `cargo build / cargo test / cargo clippy / cargo fmt --check / cargo deny check` on push and PR. Three OS matrix (linux/macos/windows). This is the build-validation companion to release.yml; ships if no equivalent exists yet.
- **`Dockerfile`.** Multi-stage. Stage 1 (`builder`): `rust:1.94-alpine` with build deps; runs `cargo build --release --locked`. Stage 2 (final): `alpine:3.21` with only `dq` binary in `/usr/local/bin`. Non-root `dq` user (UID 1000). `ENTRYPOINT ["/usr/local/bin/dq"]`, `CMD ["--help"]`. A second `Dockerfile.scratch` produces a `FROM scratch` minimal image (musl static link) for size-sensitive use cases.
- **`packaging/homebrew/dq.rb`.** Homebrew formula template that downloads the macOS prebuilt tarball from GitHub Releases, installs the binary + man pages + completions. Released to a separate `mazuninky/homebrew-tap` repo (out of scope for this change — the file in this repo is the canonical source the tap repo's release automation copies on each tag).
- **`packaging/aur/PKGBUILD`.** Arch Linux user-repository PKGBUILD that downloads the linux x86_64 tarball, installs binary + man pages + completions. Same release-time copy story as the Homebrew formula.
- **`skill/SKILL.md` + `skill/skill.json`.** Claude Code skill manifest (the `skills.sh` format). Description triggers on "yaml query", "json patch", "kubernetes manifest", "helm values", "github actions yaml", etc. Body links to the README, command list, exit codes, common patterns. Lives in `skill/` so the future `npx skills add mazuninky/dq` source is the dq repo itself (not a separate skills-only repo).

### Meta

- **`dq-plan.md` M6 section.** Marker `✅ Implemented YYYY-MM-DD` plus cross-link to this archived change folder.
- **`README.md`.** Status moves from `M5 alpha` to `M6 alpha`. New "Install" section with curl-pipe-sh, brew, docker examples. New "CI integration" section with the SARIF + GitHub Actions snippet.

### What's NOT in M6 (deferred)

- **JUnit / TAP reporters.** Their first real consumer is M8's lint engine (rule violations as test cases). Validate's single-error-per-file shape doesn't exercise enough of the format to be worth shipping; deferred to M8.
- **Performance benchmarks against existing tools.** The M6 plan calls for them but they're a research/marketing artefact, not a build-blocker; tracked as a follow-up issue. The release workflow still runs `cargo bench` on tag (so regressions get caught), but the README publication of comparison numbers waits.
- **Windows installer (`dq.msi`).** PowerShell `Install-Script` semantics differ enough from POSIX `install.sh` that they warrant their own design pass; deferred until first user actually asks. The Windows zip artifact is enough for now (`Expand-Archive` → `Move-Item`).
- **Homebrew tap repo creation.** The formula file lives here; standing up `mazuninky/homebrew-tap` as a separate GitHub repo is an external infrastructure step, tracked alongside the v0.1 release announcement.
- **`skills.sh` registry submission.** Submitting `mazuninky/dq` to the public skills.sh registry is a post-release administrative step; the manifest lives here so it's ready when that PR is filed.
- **`fix`, `lint`, `query`, `rules`, `test`, `init`, `config` subcommands.** All M7+ scope per the existing cli-shell anti-scope. The new `completions` / `man` / `self check` / `self update` verbs are the only additions.
- **Output format negotiation across read commands.** SARIF makes sense only for diagnostic-shaped outputs (`validate`, future `lint`). For `get` / `paths` / `keys` it has no defined shape — the SARIF reporter for those commands errors with the same `BannedReporter` pattern M5 uses for HCL/INI/etc. as read targets.

## Capabilities

### New Capabilities

- **`distribution`** — covers prebuilt binaries, install.sh, release workflow, Docker images, Homebrew/AUR templates, Claude Code skill, and the contract that every release ships with a `dq-checksums.txt` and signed (when configured) artifacts. This is the single home for "how does dq get onto a machine" so the constraints don't get scattered across cli-shell.

### Modified Capabilities

- **`cli-shell`** — adds Requirements for the four new subcommands (`completions`, `man`, `self check`, `self update`), narrows the M1 anti-scope to remove `self` from the deferred list (it was reserved for "M7 and beyond"; M6 ships it), and adds a Requirement for the SARIF output format alongside the existing console / json / yaml / toml / jsonl / toon set.

(`format-support`, `data-query-read`, `data-query-write`, `data-query-bulk`, `data-query-fmt`, `path-syntax`, `template-guard` are NOT modified — none of M6 changes how documents are parsed, written, or queried.)

## Impact

### Code (Rust — delegated to `rust-cli-writer` / `rust-cli-test-writer`)

- **`crates/dq-cli/src/cli/args/`** — three new arg structs:
  - `completions.rs` — `CompletionsArgs { shell: clap_complete::Shell }`.
  - `man.rs` — `ManArgs { page: Option<String> }`.
  - `self_cmd.rs` — `SelfArgs` enum with `Check` and `Update { to: Option<String> }` variants. (File named `self_cmd` because `self` is reserved.)
- **`crates/dq-cli/src/cli/args.rs`** — re-export the three new structs and add `Completions(CompletionsArgs)` / `Man(ManArgs)` / `Self_(SelfArgs)` variants to the `Command` enum.
- **`crates/dq-cli/src/commands/`** — three new handler files:
  - `completions.rs` — `fn run(args: &CompletionsArgs, out: &mut dyn Write) -> anyhow::Result<()>` calling `clap_complete::generate(args.shell, &mut Cli::command(), "dq", out)`.
  - `man.rs` — `fn run(args: &ManArgs, out: &mut dyn Write) -> anyhow::Result<()>` calling `clap_mangen::Man::new(...).render(out)` for the top-level command, walking subcommands when `args.page` matches one.
  - `self_cmd.rs` — `fn run_check(out: &mut dyn Write) -> anyhow::Result<()>` and `fn run_update(args: &SelfUpdateArgs) -> anyhow::Result<()>` wrapping `self_update::backends::github::Update::configure()`. The check path uses `ureq` directly to hit the GitHub API (smaller dependency footprint than pulling in the full `self_update` async stack).
- **`crates/dq-cli/src/output/sarif.rs`** — new `SarifReporter` implementing `Reporter`. Renders a SARIF 2.1.0 document with one `runs` entry naming `dq` as the tool. For the M6 scope (validate-only consumer), the input value is expected to be a `{ "diagnostics": [ ... ] }` shape; the reporter walks that and emits one `result` per diagnostic. For unsupported value shapes (e.g. a query handler accidentally selecting `-F sarif`), errors with `InvalidInput` (exit 6) — same `BannedReporter` discipline as M5.
- **`crates/dq-cli/src/output/mod.rs`** — add `Sarif` variant to `OutputFormat`. The reporter factory in `lib.rs` wires `OutputFormat::Sarif → Box::new(SarifReporter)`. `as_input_format_name` returns `None` (SARIF is output-only).
- **`crates/dq-cli/src/commands/validate.rs`** — when the active reporter is `SarifReporter` (or any non-Console reporter for that matter), build a `{ "diagnostics": [...] }` value instead of plain `{}` so SARIF and the existing Console / JSON paths produce sensible output for the same parse error. The change is a `match cli.format` branch; existing console output stays byte-identical.
- **`crates/dq-cli/src/lib.rs`** — wire the three new `Command` variants into `dispatch` and the `SarifReporter` into `reporter_for_format`.
- **`crates/dq-cli/Cargo.toml`** — three new deps:
  - `self_update = { version = "0.41", default-features = false, features = ["rustls", "compression-flate2", "archive-tar"] }` — disables the OpenSSL feature (we are deliberately TLS-via-rustls). `windows-zip` feature would land here too if Windows artifacts were `.zip`; release.yml uses `.zip` for Windows so add it.
  - `ureq = { version = "2.10", default-features = false, features = ["tls"] }` — for the `self check` GitHub API call. `default-features = false` keeps the binary slim.
  - `serde_yml` already in workspace via dq-core; SARIF needs only `serde_json` which is already present.
- **`Cargo.toml` (workspace)** — `[workspace.dependencies]` mirrors for `self_update` and `ureq`.

### Code (Markdown / scripts / config — orchestrator-direct)

- **`scripts/install.sh`** — POSIX-sh installer (~150 lines). Detects target, downloads tarball + checksums, verifies SHA256 (using `shasum`/`sha256sum`/`openssl` as available), extracts, installs. `--help`, `--version vX.Y.Z`, `--install-dir DIR`, `--no-modify-path` flags.
- **`.github/workflows/release.yml`** — release-on-tag with four-target matrix. Uses official `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, `softprops/action-gh-release@v2`. No third-party actions outside those three vendors.
- **`.github/workflows/ci.yml`** — three-OS matrix CI. Runs build/test/clippy/fmt/deny on every push/PR.
- **`Dockerfile`** — multi-stage alpine build. ~25 lines.
- **`Dockerfile.scratch`** — multi-stage scratch build with musl static link. ~25 lines.
- **`.dockerignore`** — excludes `target/`, `.git/`, etc. Standard Rust set.
- **`packaging/homebrew/dq.rb`** — Homebrew formula template (~40 lines, `class Dq < Formula` shape).
- **`packaging/aur/PKGBUILD`** — Arch PKGBUILD (~30 lines).
- **`packaging/aur/.SRCINFO`** — auto-generated alongside the PKGBUILD; included so the AUR repo can land it without `makepkg --printsrcinfo` round-trip on a maintainer machine.
- **`skill/SKILL.md`** — Claude Code skill body (~150 lines: install, common patterns, exit codes, format coverage table, anti-scope).
- **`skill/skill.json`** — skill manifest with name, description, triggers, version.

### Tests (delegated to `rust-cli-test-writer`)

- **`crates/dq-cli/src/commands/completions.rs` tests** — three unit tests: bash completion script contains "complete -F", zsh contains "#compdef", fish contains "complete -c dq".
- **`crates/dq-cli/src/commands/man.rs` tests** — three unit tests: top-level page contains ".TH dq", `dq man get` contains ".TH dq-get", unknown page returns InvalidInput.
- **`crates/dq-cli/src/commands/self_cmd.rs` tests** — `run_check` is mocked behind a trait (no real network in unit tests); two tests verify "up to date" and "newer available" rendering. `run_update` is integration-tested by stubbing the `self_update::Update::update_extended` call site behind a trait so the dependency-injection contract holds. Real network call is gated behind `#[ignore]` for occasional manual runs.
- **`crates/dq-cli/src/output/sarif.rs` tests** — five unit tests: empty diagnostics → valid SARIF with empty `results`, single diagnostic → one `result` with line/col, multi-file → multiple `runs`, unsupported value shape → `InvalidInput`, snapshot test via `insta` against a known-good SARIF document.
- **`crates/dq-cli/tests/cli_smoke.rs`** — three new smoke scenarios: `dq completions bash` exits 0 with non-empty stdout, `dq man` exits 0 with stdout containing ".TH", `dq self check` (mocked via env var override pointing at a local fixture) prints the expected line.

### Dependencies (new)

- `self_update = "0.41"` (MIT) — atomic binary self-replacement against GitHub Releases. Audited via `cargo deny check`.
- `ureq = "2.10"` (MIT or Apache-2.0) — minimal blocking HTTP client for the `self check` API call. Same audit.

Both are widely used in the Rust CLI ecosystem (`cargo-install-update`, `rustup`-adjacent tools, etc.); license audit passes by default `cargo deny` policy.

### Backward compatibility

- Every M1–M5 invocation produces byte-identical output. Three new subcommands are additive; `OutputFormat::Sarif` is additive. No existing flag changes meaning.
- Existing hidden `generate-docs` command stays — packaging scripts use it. The new public `completions` / `man` are convenience wrappers around the same `clap_complete` / `clap_mangen` calls but write to stdout (one shell / one page at a time) instead of a directory.

### Project meta

- `dq-plan.md` M6 section gains the `✅ Implemented YYYY-MM-DD` marker.
- `README.md` gains an "Install" and "CI integration" section.
- `openspec/specs/cli-shell/spec.md` gets the four new subcommand requirements + SARIF format requirement after archive.
- `openspec/specs/distribution/spec.md` is created at archive (currently lives only in the change's `specs/distribution/spec.md` delta).
