## Context

M5 archived the read+write+convert+fmt scope across ten formats. The remaining gap before the v0.1 announcement is **distribution** — installs, updates, CI ergonomics. Without curl-pipe-sh, every adoption attempt collides with `cargo install` (Rust toolchain in CI image, 2–3 minute build, no SHA pin). Without SARIF, GitHub PR annotations don't show parse errors. Without a Claude Code skill, the third audience tier (AI agents) has no on-ramp.

The technical risk is moderate but bounded:

- **Self-update is the one user-visible rope.** A botched download must NOT corrupt the existing binary on disk. We pay `self_update`'s audited atomic-replace (download → verify SHA256 → atomic rename) instead of rolling our own. No custom code touches the filesystem write path.
- **GitHub Actions release matrix has bootstrap cost.** Cross-compiling `aarch64-unknown-linux-gnu` from `ubuntu-latest` requires either `cross-rs` (extra container layer) or `taiki-e/setup-cross-toolchain-action` (apt-installs `gcc-aarch64-linux-gnu`, no extra container). We pick the latter to stay within the "no third-party action vendors beyond checkout / rust-toolchain / action-gh-release" guardrail.
- **Skill manifest format is shifting.** `skills.sh` is the de-facto Claude Code skill registry but the manifest schema has churned in the last quarter. We ship an in-repo `skill/` directory whose shape matches the *current* documented schema, and accept that one or two field renames may land before the registry submission.

**Current state:** M5 archived (`openspec/changes/archive/2026-05-04-add-format-extensions/`). Active changes: `add-distribution` (this document).

**Constraints:**

- Conventions from `/rust-cli` skill are unchanged: thin `main.rs`, Reporter with DI, exit codes as named constants, no `println!` outside `main.rs` / Reporter implementations.
- Rust code edits are delegated to `rust-cli-writer` / `rust-cli-test-writer` per `.claude/rules/rust-delegation.md`. Markdown / shell / yaml / Dockerfile / packaging templates stay with the orchestrator.
- M1–M5 single-file behaviour and golden snapshots stay byte-identical. Three new subcommands and one new reporter are additive.
- Dependencies must be MIT/Apache-2.0 to pass `cargo deny check`. Both new deps (`self_update`, `ureq`) are MIT/Apache-2.0 dual.
- The release workflow MUST work on a fresh fork without secrets — `COSIGN_PRIVATE_KEY` is optional; absence skips signing instead of failing.

**Stakeholders:**

- AI agents in CI/CD (the third audience tier per dq-plan.md): need install-without-toolchain and SARIF.
- DevOps engineers running k8s/helm/hugo workflows: need brew install, docker pull, and the GH Actions snippet.
- Future M8 (lint engine): the SARIF reporter and the install story land here so M8 can focus on rules.
- Future M11 (composite rules / JSON Schema): no new distribution requirements; M6 is the canonical "how to ship" infrastructure.

## Goals / Non-Goals

**Goals:**
- `curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/main/scripts/install.sh | sh` installs the latest release in <30 seconds on macOS / Linux.
- `dq self check` reports whether a newer version exists.
- `dq self update` atomically replaces the running binary with the latest (or `--to vX.Y.Z`).
- `dq completions zsh > ~/.zsh/completions/_dq` provisions zsh completions in one line.
- `dq man get | man -l -` opens the `dq get` man page without installing it.
- `dq validate -F sarif config.yaml > result.sarif` produces a SARIF 2.1.0 document GitHub Code Scanning ingests.
- A tag push to GitHub triggers a release workflow that produces four signed (when keys configured) prebuilt tarballs + checksums file + man pages + completions, and creates a GitHub Release with all artifacts.
- `docker run mazuninky/dq:latest get config.yaml /spec/replicas` works.
- A Claude Code skill in `skill/` is installable via the `skills.sh` workflow and contains the M1–M5 command surface.

**Non-Goals:**
- JUnit and TAP reporters — first real consumer is M8's lint engine, deferred.
- Performance benchmarks against existing tools — research artefact, not a build-blocker; tracked as a follow-up issue.
- Windows `.msi` installer — `Expand-Archive` + `Move-Item` is sufficient for the alpha; deferred until first user actually requests it.
- Standing up the `mazuninky/homebrew-tap` GitHub repo — packaging template lives here, the tap repo is external infrastructure.
- Submitting the skill to the public `skills.sh` registry — manifest lives here; submission is a post-release administrative step.
- `cosign verify` instructions for end users — release ships signed artifacts when `COSIGN_PRIVATE_KEY` is configured; surfacing the verify command to users waits for v1.0.
- Auto-update on startup — opt-in `dq self update` only. Surprising users with mid-CLI updates is a regression for agents/automation.

## Decisions

### D1. `dq self check` and `dq self update` are separate subcommands, not a single update with `--check`

**Decision:** ship `dq self` with two children: `check` (no-op, prints comparison) and `update [--to <ver>]` (downloads + replaces). Mirrors `rustup self check` / `rustup self update`.

**Alternatives:**
- Single `dq update [--check]`: shorter, but conflates "tell me what's available" (read-only, safe in CI) with "actually replace my binary" (write, dangerous in CI). The `self` namespace is the standard place for "operations on the tool itself". Rejected.
- `dq update` with `--check` flag at the top level (no `self` namespace): conflicts with the future `dq update` semantic for "apply an update operation to a document" if we ever want it. Reserve the verb. Rejected.

**Trade-offs:** more typing (`dq self check` vs `dq update --check`). Worth it for namespace hygiene.

### D2. Self-update uses the `self_update` crate, not a hand-rolled implementation

**Decision:** depend on `self_update` crate (current stable: 0.41). It already implements: GitHub Releases asset selection by target triple, atomic replacement on Unix (rename) and Windows (move-with-replace), SHA256 verification against a published checksums file, signature verification (we don't ship a separate signature flow yet — we use the SHA256 path).

**Alternatives:**
- Hand-rolled `ureq` download + `tempfile::NamedTempFile::persist`: ~200 LOC, would need to replicate the Windows MoveFileEx-with-MOVEFILE_REPLACE_EXISTING trick. The `self_update` crate already does this and is widely audited (`cargo`-ecosystem CI tools depend on it). Rejected on NIH grounds.
- `cargo binstall`-style approach (a separate binary that wraps every Rust CLI): out of scope; users expect `dq self update` to be a `dq` subcommand, not a separate `cargo install dq-updater` step.

**Trade-offs:** `self_update` brings in `flate2`, `tar`, and `zip` (the last only when the `windows-zip` feature is enabled). The dependency graph adds ~6 transitive crates; binary size grows ~400 KiB. Acceptable for the install ergonomics. The crate's own dependencies are MIT/Apache-2.0; no `cargo deny` policy update required.

### D3. `dq self check` uses `ureq` directly, NOT `self_update`'s API for the version check

**Decision:** the check path makes a single GitHub API call (`GET /repos/mazuninky/dq/releases/latest`) via `ureq` and compares the `tag_name` string to `env!("CARGO_PKG_VERSION")`. The `self_update` crate's `current_version` API would also work, but adding the full `self_update::backends::github::ReleaseList::configure()` setup for what amounts to one HTTP GET wastes ~50 transitive lines.

**Alternatives:**
- Use `self_update::backends::github::ReleaseList`: pulls in the full builder pattern for what we need to be a one-liner. Rejected on simplicity grounds.
- Use `reqwest`: heavier dependency (tokio runtime, full HTTP/2). Rejected — `ureq` blocking is enough for one synchronous call.
- Skip the check command entirely, fold the check into `update`: see D1 rationale.

**Trade-offs:** two HTTP-client crates in the binary (`ureq` for check + whatever `self_update` uses internally — also `reqwest` blocking by default). To stay slim, configure `self_update` with `default-features = false` and only the features needed (`rustls`, `compression-flate2`, `archive-tar`, `archive-zip` for windows). Verified against `self_update` 0.41's feature flags.

### D4. SARIF reporter expects a `{ "diagnostics": [...] }` value shape; query commands using `-F sarif` are rejected

**Decision:** `SarifReporter::report` expects the input `serde_json::Value` to be an object with a `diagnostics` array. Each diagnostic has `path`, `line`, `col`, `message`, `severity`. Anything else errors with `InvalidInput`. `validate` is the M6 consumer; M8's lint engine reuses the same shape.

**Alternatives:**
- Auto-wrap any value as a single SARIF result with the value's stringified form: makes `dq get config.yaml /name -F sarif` "work" but produces meaningless SARIF (one notification with the value as the message). Misleading. Rejected.
- Reject `-F sarif` at clap parse time when the subcommand is not `validate` or `lint`: requires per-subcommand value-validation logic in clap. The runtime reject path is simpler and produces the same UX (exit 6 with a message naming the unsupported combination). Chosen.
- Define a `Diagnostics` newtype in dq-cli, have `validate::run` return it, dispatch through a separate trait method: bigger refactor than needed for one consumer. Defer until M8 actually has multiple producers.

**Trade-offs:** the M6 SARIF reporter is single-purpose (validate-only). M8 will reuse the value shape; we lock the shape in design D4 so M8 doesn't have to refactor. Documented in `crates/dq-cli/src/output/sarif.rs` doc comment.

### D5. Release workflow uses native runners for macOS / Windows; Linux aarch64 cross-compiles from ubuntu-latest

**Decision:** the GitHub Actions matrix has four entries:
- `x86_64-unknown-linux-gnu` on `ubuntu-latest` — native build.
- `aarch64-unknown-linux-gnu` on `ubuntu-latest` — cross-compile via `taiki-e/setup-cross-toolchain-action@v1` (apt-installs `gcc-aarch64-linux-gnu`).
- `aarch64-apple-darwin` on `macos-14` — native build (macos-14 is M1).
- `x86_64-pc-windows-msvc` on `windows-latest` — native build.

**Alternatives:**
- All targets via `cross-rs`: requires a Docker container per build, adds startup overhead, and pulls a third-party action vendor. The native-where-possible approach is the standard pattern for Rust CLI release pipelines. Rejected.
- Skip aarch64-linux until M7+: macOS aarch64 is the M1 default for the project's primary author (and Apple Silicon dominates dev workstations); Linux aarch64 covers the AWS Graviton / Raspberry Pi user. Both are first-class.

**Trade-offs:** `taiki-e/setup-cross-toolchain-action` is a fourth third-party action vendor (after `actions/checkout`, `dtolnay/rust-toolchain`, `softprops/action-gh-release`). Acceptable — it's a common, audited action with no transitive risk we can identify, and the alternative (cross-rs container) is heavier.

### D6. `install.sh` defaults to `~/.local/bin`, falls back to `/usr/local/bin` when run as root

**Decision:** the install script picks `INSTALL_DIR` in this order:
1. `--install-dir DIR` flag if provided.
2. `$DQ_INSTALL_DIR` if set.
3. `~/.local/bin` if the user is non-root.
4. `/usr/local/bin` if the user is root or the user runs `sudo install.sh`.

**Alternatives:**
- Always `/usr/local/bin`: requires sudo on every install. Annoying for one-off testing. Rejected.
- Always `~/.local/bin`: Linux distros put it in PATH by default; macOS does not. Surprises macOS users. Rejected for non-tty messaging — we print "Add ~/.local/bin to your PATH" when the directory isn't on `$PATH`.
- Auto-detect via `which dq` and overwrite the existing path: dangerous if a system package manager owns it. Reject.

**Trade-offs:** users have to read one line of installer output to know where the binary went. Acceptable.

### D7. SHA256 checksums are published as a single `dq-checksums.txt` file alongside the tarballs

**Decision:** the release job (after the matrix completes) downloads all artifacts, runs `sha256sum dq-*.tar.gz dq-*.zip > dq-checksums.txt`, and uploads the checksums file as a release asset. `install.sh` and `dq self update` both verify against this single file. Format is the standard `sha256sum --check` format (`<hex> *<filename>` per line).

**Alternatives:**
- One `.sha256` file per tarball: simpler download path but four files to manage. Aggregation is cleaner.
- Inline sha256 in the GitHub Release body: hard to consume programmatically. Reject.
- Use `cosign`-attested checksums: better but requires the maintainer to manage signing keys; the optional cosign flow runs alongside SHA256, doesn't replace it.

**Trade-offs:** the verifier must download two artifacts (tarball + checksums). One extra HTTP request per install. Acceptable.

### D8. Docker image is alpine-based, not distroless

**Decision:** `Dockerfile` builds on `rust:1.94-alpine` and ships on `alpine:3.21`. The image has `/bin/sh`, `apk` is not installed in the runtime stage.

**Alternatives:**
- Distroless (`gcr.io/distroless/cc`): smaller image, but requires glibc binary (not musl). We'd need a separate non-static build. Reject for v0.1 — alpine is more familiar.
- `FROM scratch` static-musl-only: smallest possible. Shipped as `Dockerfile.scratch` for size-sensitive consumers; the default `Dockerfile` keeps the alpine base for shell access (debugging, COPY-from chains).
- Ubuntu/debian base: ~80 MiB, dwarfs the binary. Rejected on size grounds.

**Trade-offs:** two Dockerfiles to maintain. Acceptable — the contents are nearly identical; only the FROM stages and the static-link flag differ.

### D9. Skill body uses the format documented at https://docs.skills.sh as of 2026-05-04, not bleeding-edge

**Decision:** `skill/SKILL.md` and `skill/skill.json` follow the schema documented in the Anthropic Claude Code skill creator skill (see the `anthropic-skills:skill-creator` skill loaded into this session). Specifically: front-matter `name`, `description`, `triggers`, `version`, body markdown with sections "Install", "Common patterns", "Format coverage", "Exit codes", "Anti-scope".

**Alternatives:**
- Whatever `skills.sh` happens to support today: their schema is documented but has churned; we'd be chasing. Reject.
- A custom JSON manifest: ignores the convention; would block `npx skills add mazuninky/dq` from working.

**Trade-offs:** if the schema lands a breaking change before the v0.1 release, we update the manifest in a follow-up. Low cost — it's two text files.

## Risks / Open Questions

### R1. `self_update` 0.41 + GitHub API rate limits

GitHub's unauthenticated API rate limit is 60 requests/hour per IP. A user running `dq self check` 60 times in an hour (e.g. a CI loop) would hit the limit. The crate's docs recommend setting `GITHUB_TOKEN` for CI use; we surface this in the `dq self check` failure message ("Add GITHUB_TOKEN env var to raise rate limit").

**Mitigation:** document the rate limit + workaround in the `dq self check --help` text. Out of scope: implementing automatic GHES backoff or local caching of the version check.

### R2. macOS Gatekeeper / Windows SmartScreen warnings on unsigned binaries

The release workflow signs with `cosign` when the secret is configured, but cosign signatures don't satisfy Apple notarization or Windows code signing. Users will see a "downloaded from the internet" warning the first time they run the binary on macOS (right-click → Open) and a SmartScreen prompt on Windows.

**Mitigation:** documented in README's Install section ("if Gatekeeper warns, right-click → Open"). Notarization (Apple Developer ID + `notarytool`) and Windows Authenticode signing are deferred to v1.0 — both require paid certificates we don't have for the alpha.

### R3. Homebrew tap repo doesn't exist yet

The formula in `packaging/homebrew/dq.rb` references `https://github.com/mazuninky/dq/releases/download/v0.1.0/dq-aarch64-apple-darwin.tar.gz`. Until the v0.1.0 tag is published, the formula doesn't resolve. The `mazuninky/homebrew-tap` GitHub repo also has to be created (one-time bootstrap).

**Mitigation:** the formula file ships in this PR for review; standing up the tap repo and copying the formula on each tag is a release-engineering follow-up tracked alongside the v0.1 announcement issue.

### R4. AUR PKGBUILD untested without a real Arch user

The `packaging/aur/PKGBUILD` is a template based on the Arch packaging guidelines. It hasn't been validated against a real `makepkg` run. The `.SRCINFO` is generated from the PKGBUILD assumptions; if any line has a syntax error, AUR submission fails.

**Mitigation:** the file ships for review; first AUR submission validates it. Marked in the change description as "best-effort template, validation pending first real submission".

### R5. SARIF schema validation

The SARIF 2.1.0 schema is large (~1500 lines of JSON Schema). The reporter emits a hand-coded subset (one `runs` entry, one `tool` driver, `results` array). If we miss a required field GitHub Code Scanning rejects the file.

**Mitigation:** reference the GitHub Actions example in the SARIF reporter's doc comment; ship a snapshot test (`insta`) against a known-good SARIF document the maintainer manually validated against the Microsoft SARIF Multitool. Not running the multitool in CI — it's a Java tool with its own dependency surface. Open to a follow-up adding a schema validation step in the CI matrix.

### Open Questions

- **Q1.** Should `dq self update` honour `--dry-run`? `cargo install --dry-run` exists, so user expectation is established. Deferred — the cost is one bool flag and a print-instead-of-replace branch; add when the first user asks.
- **Q2.** Should the install script add the install directory to `$PATH` automatically (write to `~/.bashrc`)? Most install scripts do (`rustup-init.sh`, `bun install`, `deno install`). The `--no-modify-path` flag is the opt-out. The current draft prints "Add to PATH" instructions; auto-modify is deferred until users complain.
- **Q3.** Should the Docker image's `ENTRYPOINT` include a default command other than `--help`? `dq` with no args today prints help — fine. If we change M7+ behaviour to default to `query` mode (jq style), revisit.
