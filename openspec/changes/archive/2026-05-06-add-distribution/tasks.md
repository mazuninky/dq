Делегирование: `[orch]` — оркестратор пишет markdown / shell scripts / yaml workflows / Dockerfile / packaging templates / skill manifests; `[writer]` / `[test-writer]` — Rust-правки идут через subagents `rust-cli-writer` / `rust-cli-test-writer` (правило в `.claude/rules/rust-delegation.md`). Каждая задача self-contained, ≤ 2 часов реальной работы. Зависимости явно прописаны: §1 готовит фундамент (workspace deps + new args modules + Command enum); §2–§4 — параллельные subcommand handlers (`completions`, `man`, `self`); §5 — SARIF reporter; §6–§11 — packaging artifacts (orchestrator-direct); §12 — verification + meta.

## 1. Foundation: deps, OutputFormat, Command enum

- [ ] 1.1 [writer] Workspace `Cargo.toml`: add `[workspace.dependencies]` entries:
  ```toml
  self_update = { version = "0.41", default-features = false, features = ["rustls", "compression-flate2", "archive-tar", "archive-zip"] }
  ureq = { version = "2.10", default-features = false, features = ["tls"] }
  ```
  Verify both crates' licenses are MIT or Apache-2.0 (use `cargo metadata --format-version 1` after this step).

- [ ] 1.2 [writer] `crates/dq-cli/Cargo.toml`: add `self_update = { workspace = true }` and `ureq = { workspace = true }` under `[dependencies]`. No feature flags needed beyond the workspace defaults.

- [ ] 1.3 [writer] `crates/dq-cli/src/output/mod.rs`: extend `OutputFormat` enum with `Sarif` variant. Update `as_input_format_name` to return `None` for SARIF (output-only). Doc comment cites design D4.

- [ ] 1.4 [writer] `crates/dq-cli/src/cli/args.rs`: add three new arg modules + re-exports + `Command` enum variants:
  - `mod completions; pub use completions::CompletionsArgs;`
  - `mod man; pub use man::ManArgs;`
  - `mod self_cmd; pub use self_cmd::{SelfArgs, SelfUpdateArgs};`
  - `Command::Completions(CompletionsArgs)`, `Command::Man(ManArgs)`, `Command::Self_(SelfArgs)`.
  The `Self_` rename avoids the Rust keyword collision; clap derives the subcommand name from `#[command(name = "self")]`.

- [ ] 1.5 [test-writer] `crates/dq-cli/src/output/mod.rs` `#[cfg(test)] mod tests`: add a unit test confirming `OutputFormat::Sarif` parses from clap (`-F sarif`) and `as_input_format_name() == None`.

## 2. `dq completions <shell>` subcommand

- [ ] 2.1 [writer] Create `crates/dq-cli/src/cli/args/completions.rs`:
  ```rust
  use clap::Args;
  use clap_complete::Shell;

  #[derive(Debug, Args)]
  pub struct CompletionsArgs {
      /// Shell to generate completions for.
      #[arg(value_enum)]
      pub shell: Shell,
  }
  ```
  Doc comment names the user-facing entry point and contrasts with the hidden `generate-docs`.

- [ ] 2.2 [writer] Create `crates/dq-cli/src/commands/completions.rs`:
  ```rust
  use std::io::Write;
  use clap::CommandFactory;
  use crate::cli::{Cli, CompletionsArgs};

  pub fn run(args: &CompletionsArgs, out: &mut dyn Write) -> anyhow::Result<()> {
      let mut cmd = Cli::command();
      clap_complete::generate(args.shell, &mut cmd, "dq", out);
      Ok(())
  }
  ```

- [ ] 2.3 [writer] `crates/dq-cli/src/commands/mod.rs`: add `pub mod completions;`.

- [ ] 2.4 [writer] `crates/dq-cli/src/lib.rs`: add a dispatch arm for `Command::Completions(args) => commands::completions::run(args, out)`. Read-only command — no need to pass a reporter.

- [ ] 2.5 [test-writer] Create `crates/dq-cli/src/commands/completions.rs` `#[cfg(test)] mod tests` (≥4 tests):
  - bash output contains `complete -F` (the bash completion function declaration).
  - zsh output contains `#compdef dq` (the zsh completion header).
  - fish output contains `complete -c dq` (the fish completion verb).
  - powershell output contains `Register-ArgumentCompleter` (the PS verb).

## 3. `dq man [PAGE]` subcommand

- [ ] 3.1 [writer] Create `crates/dq-cli/src/cli/args/man.rs`:
  ```rust
  use clap::Args;

  #[derive(Debug, Args)]
  pub struct ManArgs {
      /// Subcommand to render the man page for. Omit for the top-level dq.1.
      pub page: Option<String>,
  }
  ```

- [ ] 3.2 [writer] Create `crates/dq-cli/src/commands/man.rs`:
  ```rust
  use std::io::Write;
  use clap::CommandFactory;
  use crate::cli::{Cli, ManArgs};
  use crate::error::InvalidInput;

  pub fn run(args: &ManArgs, out: &mut dyn Write) -> anyhow::Result<()> {
      let cmd = Cli::command();
      let target = match &args.page {
          None => cmd.clone(),
          Some(name) => cmd
              .find_subcommand(name)
              .cloned()
              .ok_or_else(|| anyhow::Error::new(InvalidInput::new(format!(
                  "unknown subcommand '{name}' — try `dq man --help`"
              ))))?,
      };
      let man = clap_mangen::Man::new(target);
      man.render(out)?;
      Ok(())
  }
  ```

- [ ] 3.3 [writer] `crates/dq-cli/src/commands/mod.rs`: add `pub mod man;`.

- [ ] 3.4 [writer] `crates/dq-cli/src/lib.rs`: add a dispatch arm for `Command::Man(args) => commands::man::run(args, out)`.

- [ ] 3.5 [test-writer] Create `crates/dq-cli/src/commands/man.rs` `#[cfg(test)] mod tests` (≥3 tests):
  - top-level (`page = None`) renders troff containing `.TH "dq"` (the top-level man page header).
  - subcommand (`page = Some("get")`) renders troff containing `.TH "dq-get"`.
  - unknown subcommand returns `InvalidInput` whose message names the missing page.

## 4. `dq self check` and `dq self update` subcommands

- [ ] 4.1 [writer] Create `crates/dq-cli/src/cli/args/self_cmd.rs`:
  ```rust
  use clap::{Args, Subcommand};

  #[derive(Debug, Args)]
  #[command(name = "self")]
  pub struct SelfArgs {
      #[command(subcommand)]
      pub command: SelfCommand,
  }

  #[derive(Debug, Subcommand)]
  pub enum SelfCommand {
      /// Check whether a newer release is available on GitHub.
      Check,
      /// Download and atomically replace the running binary.
      Update(SelfUpdateArgs),
  }

  #[derive(Debug, Args)]
  pub struct SelfUpdateArgs {
      /// Specific version tag (e.g. v0.2.0) to install. Defaults to latest.
      #[arg(long, value_name = "VER")]
      pub to: Option<String>,
  }
  ```

- [ ] 4.2 [writer] Create `crates/dq-cli/src/commands/self_cmd.rs`. Two functions:
  ```rust
  pub fn run_check(out: &mut dyn Write) -> anyhow::Result<()> { /* hits GitHub releases API via ureq, prints comparison */ }
  pub fn run_update(args: &SelfUpdateArgs) -> anyhow::Result<()> { /* uses self_update::backends::github::Update */ }
  ```
  - `run_check` does a single `GET https://api.github.com/repos/mazuninky/dq/releases/latest` via `ureq::get(...).set("User-Agent", "dq-self-check").call()`, parses `{ "tag_name": "vX.Y.Z" }` via `serde_json`, compares to `env!("CARGO_PKG_VERSION")`. Prints one of: "dq vX.Y.Z is up to date", "newer version available: vA.B.C — run `dq self update` to install", "running pre-release version (local: vX.Y.Z, latest: vA.B.C)".
  - `run_update` configures `self_update::backends::github::Update::configure()` with repo `mazuninky/dq`, target `self_update::get_target()`, `bin_name("dq")`, optional `target_version_tag(args.to)`, then calls `.update_extended()`. Maps `self_update::errors::Error::Network` → `Error::Io`-flavoured `anyhow!` so `exit_code_for_error` produces 5; replace failures map to a `WriteFailed` shape (exit 7).
  - Add a per-handler `EXIT_CHECK_RATE_LIMITED = 5` doc note: when GitHub returns 403 with `X-RateLimit-Remaining: 0`, surface a clearer "GitHub API rate limit hit; set GITHUB_TOKEN to raise" message before the network exit code.

- [ ] 4.3 [writer] `crates/dq-cli/src/commands/mod.rs`: add `pub mod self_cmd;`.

- [ ] 4.4 [writer] `crates/dq-cli/src/lib.rs`: add a dispatch arm for `Command::Self_(args)` that matches on `args.command` and dispatches to `run_check(out)` or `run_update(&u)`.

- [ ] 4.5 [test-writer] Create `crates/dq-cli/src/commands/self_cmd.rs` `#[cfg(test)] mod tests`:
  - factor the version-check business logic into a pure function `compare_versions(local: &str, remote: &str) -> CheckOutcome` so unit tests can cover the three branches (UpToDate / NewerAvailable / PreRelease) without mocking HTTP.
  - render the `CheckOutcome` to a writer through a separate `render_check_outcome(outcome, out)` function — covered by ≥3 tests.
  - `run_update` is integration-tested via a trait-based seam so `self_update` does not actually hit the network; one happy-path test asserts the trait method was called with the expected target / version arguments. Real network test gated behind `#[ignore]`.

## 5. SARIF output reporter

- [ ] 5.1 [writer] Create `crates/dq-cli/src/output/sarif.rs`. `SarifReporter` implements `Reporter`:
  ```rust
  pub struct SarifReporter;

  impl Reporter for SarifReporter {
      fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()> {
          let diagnostics = value
              .get("diagnostics")
              .and_then(|v| v.as_array())
              .ok_or_else(|| anyhow::Error::new(crate::error::InvalidInput::new(
                  "SARIF reporter expects an object with a `diagnostics` array; got something else"
              )))?;
          let sarif = build_sarif(diagnostics);
          serde_json::to_writer_pretty(w, &sarif)?;
          Ok(())
      }
  }
  ```
  `build_sarif` returns a `serde_json::Value` shaped per SARIF 2.1.0:
  ```json
  {
    "$schema": "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.5.json",
    "version": "2.1.0",
    "runs": [{
      "tool": { "driver": { "name": "dq", "version": "X.Y.Z", "informationUri": "https://github.com/mazuninky/dq" } },
      "results": [
        {
          "level": "error",
          "message": { "text": "..." },
          "locations": [{ "physicalLocation": {
            "artifactLocation": { "uri": "..." },
            "region": { "startLine": 1, "startColumn": 1 }
          }}]
        }
      ]
    }]
  }
  ```
  Severity mapping: `"error" → "error"`, `"warn" → "warning"`, `"info" → "note"`. Default to `"warning"` for unknown levels.

- [ ] 5.2 [writer] `crates/dq-cli/src/output/mod.rs`: `pub mod sarif; pub use sarif::SarifReporter;`. Wire `OutputFormat::Sarif → Box::new(SarifReporter)` in the `reporter_for_format` factory in `crates/dq-cli/src/lib.rs`.

- [ ] 5.3 [writer] `crates/dq-cli/src/commands/validate.rs`: when the active reporter is non-Console (use `cli.format != OutputFormat::Console` as the discriminator), construct a `{ "diagnostics": [{...}] }` value instead of the bare `{}` it emits today on parse failure. Existing console output stays byte-identical (the console branch keeps the human-readable string).

- [ ] 5.4 [test-writer] Create `crates/dq-cli/src/output/sarif.rs` `#[cfg(test)] mod tests` (≥5 tests):
  - empty diagnostics → valid SARIF with `runs[0].results == []`.
  - single diagnostic → one `result` with `physicalLocation.region.startLine == expected`.
  - multiple diagnostics in one run produce multiple `results`.
  - unsupported value shape (e.g. a top-level array) returns `InvalidInput`.
  - snapshot test via `insta::assert_json_snapshot!` against a fixture document.

- [ ] 5.5 [test-writer] Add a smoke test in `crates/dq-cli/tests/cli_smoke.rs`: `dq validate -F sarif <broken.yaml>` exits 4 with stdout matching the SARIF JSON shape (`$.version == "2.1.0"`).

## 6. install.sh

- [ ] 6.1 [orch] Create `scripts/install.sh` (POSIX sh, ~150 lines). Sections:
  - `set -eu` shebang + `usage()` printing `--help`, `--version vX.Y.Z`, `--install-dir DIR`, `--no-modify-path`, `--repo OWNER/NAME`.
  - `detect_target()` — `uname -s` → `linux`/`darwin`/`unknown`; `uname -m` → `x86_64`/`aarch64`/`arm64` mapped to release naming.
  - `latest_version()` — curl GitHub API for `/releases/latest`, extract `tag_name`. Use `python -c` or `sed` to parse JSON (no `jq` dependency).
  - `download_and_verify()` — curl tarball + checksums file, verify with `shasum -a 256 -c` (BSD/macOS) or `sha256sum -c` (Linux), extract to a tempdir.
  - `install_binary()` — move `dq` to `INSTALL_DIR`, chmod +x, run `dq --version` to verify.
  - `path_check()` — warn if `INSTALL_DIR` is not on `$PATH`; with `--no-modify-path` skip the warning.
  - PATH default: `~/.local/bin` for non-root, `/usr/local/bin` for root.
  - `trap` cleanup of the tempdir.
  - chmod +x in the file via `git update-index --chmod=+x` later.

- [ ] 6.2 [orch] Add `scripts/install.sh` to a `Test plan` section of the proposal: manual smoke `bash scripts/install.sh --version v0.5.0` (once a real release exists) on macOS arm64 and Linux x86_64.

## 7. GitHub Actions workflows

- [ ] 7.1 [orch] Create `.github/workflows/release.yml`:
  - Trigger: `push: tags: ['v*']` (and `workflow_dispatch` for manual re-runs).
  - Job `build`: matrix over four targets, each runs on the right `runs-on`, uses `dtolnay/rust-toolchain@stable` with `target` set, runs `cargo build --release --locked --target $TARGET`, generates man pages + completions via `cargo run --release --bin dq -- generate-docs --output-dir dist/docs`, packages as `dq-$TAG-$TARGET.tar.gz` (tar) or `.zip` (windows). Uploads via `actions/upload-artifact@v4`.
  - Job `release` (needs build): downloads all artifacts, generates `dq-checksums.txt` via `sha256sum dq-*.tar.gz dq-*.zip`, optionally signs with `cosign` if `secrets.COSIGN_PRIVATE_KEY` is set, uploads to GitHub Release via `softprops/action-gh-release@v2`. Release body includes installation snippets + checksums excerpt.
  - Linux aarch64 cross-compile uses `taiki-e/setup-cross-toolchain-action@v1`.

- [ ] 7.2 [orch] Create `.github/workflows/ci.yml` (only if `.github/workflows/ci.yml` doesn't already exist):
  - Trigger: `push` and `pull_request`.
  - Matrix over `ubuntu-latest`, `macos-14`, `windows-latest`.
  - Steps: checkout, install rust 1.94, `cargo build --workspace --all-targets`, `cargo test --workspace --all-features`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`. Linux job additionally runs `cargo deny check`.

## 8. Dockerfile + .dockerignore

- [ ] 8.1 [orch] Create `Dockerfile` (multi-stage alpine, ~30 lines):
  - Stage 1 `FROM rust:1.94-alpine AS builder`: install `musl-dev`, copy workspace, `cargo build --release --locked`.
  - Stage 2 `FROM alpine:3.21`: copy `/app/target/release/dq` to `/usr/local/bin/dq`. Add non-root user `dq` (UID 1000). `WORKDIR /work`. `USER dq`. `ENTRYPOINT ["/usr/local/bin/dq"]`. `CMD ["--help"]`. `LABEL org.opencontainers.image.source="https://github.com/mazuninky/dq"`.

- [ ] 8.2 [orch] Create `Dockerfile.scratch` (multi-stage scratch + musl static link, ~25 lines):
  - Stage 1: same as `Dockerfile` but uses `RUSTFLAGS="-C target-feature=+crt-static"` for static link.
  - Stage 2 `FROM scratch`: copy only the binary. `ENTRYPOINT ["/dq"]`. No shell available — for size-sensitive consumers only.

- [ ] 8.3 [orch] Create `.dockerignore`:
  ```
  target/
  .git/
  .github/
  .claude/
  .openspec/
  docs/
  spikes/
  *.md
  *.lock
  .DS_Store
  ```

## 9. Homebrew + AUR templates

- [ ] 9.1 [orch] Create `packaging/homebrew/dq.rb`:
  ```ruby
  class Dq < Formula
    desc "Agent-friendly Rust CLI for structured data + linter platform"
    homepage "https://github.com/mazuninky/dq"
    version "0.6.0"           # bumped per release; the tap repo's release automation rewrites this on tag.
    license "MIT"

    on_macos do
      if Hardware::CPU.arm?
        url "https://github.com/mazuninky/dq/releases/download/v#{version}/dq-v#{version}-aarch64-apple-darwin.tar.gz"
        sha256 "REPLACE-WITH-SHA256"
      end
    end

    on_linux do
      if Hardware::CPU.intel?
        url "https://github.com/mazuninky/dq/releases/download/v#{version}/dq-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
        sha256 "REPLACE-WITH-SHA256"
      elsif Hardware::CPU.arm?
        url "https://github.com/mazuninky/dq/releases/download/v#{version}/dq-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
        sha256 "REPLACE-WITH-SHA256"
      end
    end

    def install
      bin.install "dq"
      man1.install Dir["man/dq*.1"]
      bash_completion.install "completions/dq.bash"
      zsh_completion.install "completions/_dq"
      fish_completion.install "completions/dq.fish"
    end

    test do
      assert_match "dq #{version}", shell_output("#{bin}/dq --version")
    end
  end
  ```

- [ ] 9.2 [orch] Create `packaging/aur/PKGBUILD`:
  ```
  # Maintainer: Konstantin Mazunin <konstantin.mazunin@01.tech>
  pkgname=dq
  pkgver=0.6.0
  pkgrel=1
  pkgdesc="Agent-friendly Rust CLI for structured data + linter platform"
  arch=('x86_64' 'aarch64')
  url="https://github.com/mazuninky/dq"
  license=('MIT')
  source_x86_64=("$url/releases/download/v$pkgver/dq-v$pkgver-x86_64-unknown-linux-gnu.tar.gz")
  source_aarch64=("$url/releases/download/v$pkgver/dq-v$pkgver-aarch64-unknown-linux-gnu.tar.gz")
  sha256sums_x86_64=('REPLACE-WITH-SHA256')
  sha256sums_aarch64=('REPLACE-WITH-SHA256')

  package() {
    install -Dm755 "$srcdir/dq" "$pkgdir/usr/bin/dq"
    install -Dm644 "$srcdir/man/dq.1" "$pkgdir/usr/share/man/man1/dq.1"
    install -Dm644 "$srcdir/completions/dq.bash" "$pkgdir/usr/share/bash-completion/completions/dq"
    install -Dm644 "$srcdir/completions/_dq" "$pkgdir/usr/share/zsh/site-functions/_dq"
    install -Dm644 "$srcdir/completions/dq.fish" "$pkgdir/usr/share/fish/vendor_completions.d/dq.fish"
  }
  ```

- [ ] 9.3 [orch] Create `packaging/aur/.SRCINFO`. Generated content matching `PKGBUILD` per `makepkg --printsrcinfo` conventions. (Hand-written stub acceptable — a real AUR maintainer regenerates on first publish.)

## 10. Claude Code skill

- [ ] 10.1 [orch] Create `skill/SKILL.md`. Sections:
  - Frontmatter: `name: dq`, `description: ...` (triggers on yaml-query/json-patch/k8s/helm/github-actions), `version: 0.6.0`.
  - "Install" — curl-pipe-sh + brew + cargo install snippets.
  - "Common patterns" — k8s manifest set-replicas, helm values get-image, github actions update-version, json-patch CI gate, fmt --check pre-commit.
  - "Format coverage" — table from `dq-plan.md`'s Поддерживаемые форматы section.
  - "Exit codes" — table copied from README.
  - "Anti-scope" — what dq does NOT do (DSL, interactive prompts, network without `self`).

- [ ] 10.2 [orch] Create `skill/skill.json`:
  ```json
  {
    "name": "dq",
    "description": "Agent-friendly CLI for YAML/JSON/TOML and a linter platform. Use for queries on structured data, in-place edits with round-trip preservation, format conversion, and CI validation.",
    "version": "0.6.0",
    "triggers": [
      "yaml query", "json patch", "json pointer", "kubernetes manifest",
      "helm values", "github actions yaml", "toml edit", "round-trip yaml",
      "convert yaml to json", "fmt yaml", "yaml lint", "dq cli"
    ],
    "homepage": "https://github.com/mazuninky/dq",
    "license": "MIT"
  }
  ```

## 11. Spec deltas

- [ ] 11.1 [orch] Create `openspec/changes/add-distribution/specs/cli-shell/spec.md` with the four new ADDED requirements (`dq completions`, `dq man`, `dq self`, `OutputFormat::Sarif`) and one MODIFIED requirement (`Anti-scope for M1 binary` — `self` removed from the deferred list). Spec delta uses the OpenSpec `## ADDED Requirements` / `## MODIFIED Requirements` headings per the existing M5 archive's shape.

- [ ] 11.2 [orch] Create `openspec/changes/add-distribution/specs/distribution/spec.md` with the new `distribution` capability's full spec (will land in `openspec/specs/distribution/spec.md` at archive time). Sections: prebuilt artifacts contract, install.sh contract, Docker images contract, packaging templates contract, skill manifest contract.

## 12. Plan delta + meta + verification

- [ ] 12.1 [orch] Update `dq-plan.md` M6 section with `✅ Implemented YYYY-MM-DD (см. [openspec/changes/add-distribution/](openspec/changes/add-distribution/))` marker. Add cross-link.

- [ ] 12.2 [orch] Update `README.md`:
  - Status line moves to `M6 alpha — adds prebuilt binaries, install.sh, dq completions / man / self check / self update, SARIF output, Dockerfile, Homebrew + AUR templates, Claude Code skill`.
  - Add an "Install" section with curl-pipe-sh, brew, docker, cargo install snippets.
  - Add a "CI integration" section with a GitHub Actions snippet showing `dq validate -F sarif | upload-sarif`.

- [ ] 12.3 [orch] `cargo build --workspace --all-targets` зелёный.

- [ ] 12.4 [orch] `cargo test --workspace --all-features` зелёный (cold ≤ 60 s).

- [ ] 12.5 [orch] `cargo clippy --workspace --all-targets --all-features -- -D warnings` зелёный.

- [ ] 12.6 [orch] `cargo fmt --all -- --check` зелёный.

- [ ] 12.7 [orch] `cargo deny check` зелёный (license + advisory check on the two new deps).

- [ ] 12.8 [orch] Manual smoke per DoD M6:
  - `dq completions bash | head -5` prints bash completion script.
  - `dq man | head -5` prints `.TH "dq"` line.
  - `dq self check` prints "up to date" or "newer available" against the current GitHub release (skipped if offline).
  - `dq validate -F sarif <broken.yaml>` produces SARIF JSON.
  - `bash scripts/install.sh --help` prints usage without error.
  - `docker build -t dq:test .` produces an image; `docker run dq:test --version` prints the version.

- [ ] 12.9 [orch] `openspec validate add-distribution --strict` — `Change is valid`.

- [ ] 12.10 [orch] `openspec archive add-distribution` — после merge в main (rename folder to `archive/<date>-add-distribution/`).
