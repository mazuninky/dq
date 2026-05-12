# Contributing to dq

Thanks for your interest in improving `dq`. This document covers everything you need to build, test, and submit changes.

## Ground rules

Before you open a pull request:

- **Open an OpenSpec change for non-trivial work.** Bug fixes and small improvements can go straight to a PR. New formats, new subcommands, new rule semantics, or any change to the public CLI surface go through [`openspec/changes/<name>/`](../openspec/) first (`spec.md` + `design.md` + `tasks.md`). See [`CLAUDE.md`](../CLAUDE.md) for the workflow.
- **Keep changes focused.** One logical change per PR. A bug fix should not also refactor unrelated code; a new rule should not also bump unrelated dependencies.
- **Do not expand scope unprompted.** Don't add features that were not requested. Don't rewrite code you didn't need to touch. Don't add backwards-compat shims for code paths that don't exist yet.
- **No breaking CLI changes without discussion.** `dq` is a tool people script against and run in CI. Flag renames, removed commands, and changes to default output shape need an issue or spec change first.

## Prerequisites

- **Rust stable, MSRV 1.94** (see `rust-version` in [`Cargo.toml`](../Cargo.toml); the exact toolchain is pinned in [`rust-toolchain.toml`](../rust-toolchain.toml) and installed automatically by `rustup`). Install via [rustup](https://rustup.rs/).
- A POSIX-like shell for the release helper script ([`scripts/bump-version.sh`](../scripts/bump-version.sh)). On Windows, WSL or Git Bash is fine; the CLI itself builds and runs on native Windows after `cargo build`.
- Optional: [lefthook](https://github.com/evilmartians/lefthook) for pre-commit hooks. Install with `brew install lefthook` (or equivalent), then run `lefthook install` once inside the repo.
- Optional: [cargo-nextest](https://nexte.st/) — `lefthook.yml` uses it for the pre-commit test run; `cargo test` works just as well in CI and from the command line.

## Build and test

```sh
cargo check --workspace                                # fast type-check
cargo build --workspace                                # debug build (binary at target/debug/dq)
cargo build --workspace --release                      # optimised binary at target/release/dq
cargo test --workspace --all-features                  # unit + integration tests
cargo test --workspace --all-features <name>           # run a single test by substring match
cargo test --workspace --all-features -- --nocapture   # see stdout from tests
cargo fmt --all                                        # format
cargo fmt --all -- --check                             # check formatting without modifying
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo deny check                                       # license / advisory / source policy
```

CI runs the same set of checks ([`.github/workflows/ci.yml`](workflows/ci.yml), [`audit.yml`](workflows/audit.yml), [`codeql.yml`](workflows/codeql.yml)). Anything green locally should be green in CI.

### Pre-commit hooks

If you installed lefthook, every commit automatically runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo nextest run` in parallel. `cargo deny check` runs on `pre-push`. Configuration lives in [`lefthook.yml`](../lefthook.yml).

### Plugin runtime tests

The WASM plugin runtime is gated behind `--features plugins`:

```sh
cargo test -p dq-plugin --features plugins
```

End-to-end smoke for the example plugin:

```sh
cd examples/plugin-rust && cargo component build --release
cd ../.. && cargo run --features plugins -- lint \
    --plugins examples/plugin-rust/target/wasm32-wasip2/release config.yaml
```

## Project layout

See [`CLAUDE.md`](../CLAUDE.md) for the directory tour, anti-scope notes, and conventions. The short version:

```text
crates/
├── dq-cli/        clap CLI, subcommand handlers, output renderers
├── dq-core/       parsers, IR, textual-edit (saphyr) writers, formats
├── dq-exec/       span-aware lint pipeline + reporters (SARIF/JUnit/TAP/text)
├── dq-lint/       rule loader, jq evaluator (jaq), composite rules, @std rules
├── dq-transform/  jq-driven transforms (set --jq, query)
└── dq-plugin/     WASM plugin runtime (wasmtime, WIT contract, behind feature)
```

## Pull request process

1. **Fork and branch.** Branch from `master`. Name branches descriptively: `fix/yaml-empty-document`, `feat/std-helm-rules`.
2. **Write tests.** Add unit tests next to the code you change, and a round-trip / fixture test for new formats or rules. A PR that changes behaviour without tests will be asked to add them.
3. **Run the full local check** before pushing: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo test --workspace --all-features`.
4. **Write a Conventional Commit message.** The project follows [Conventional Commits](https://www.conventionalcommits.org/) — recent history shows the allowed types:
   - `feat:` – user-visible new capability
   - `fix:` – bug fix
   - `refactor:` – code change that does not alter behaviour
   - `test:` – adding or restructuring tests
   - `docs:` – documentation only
   - `ci:` – CI configuration
   - `chore:` – maintenance, dependency bumps
   - `release:` – version bumps (produced by [`scripts/bump-version.sh`](../scripts/bump-version.sh))
   Optional scope in parentheses (e.g. `feat(lint):`, `fix(core):`) is encouraged.
5. **Open the PR.** Fill out the template: summary, linked issue or OpenSpec change (`Fixes #NN` / `Refs openspec/changes/<name>`), and how you tested. Keep the title short — use the description for detail.
6. **Respond to review.** Push follow-up commits rather than force-pushing, unless a reviewer explicitly asks for a rebase. Mark conversations resolved as you address them.
7. **CI must be green.** All required checks (`lint`, `test`, `cargo deny`, `cargo audit`) gate the PR; if any fails, fix the underlying issue rather than retrying.

## Commit messages

Recent commits to follow as examples:

```text
feat: M11 — JSON Schema, composite rules, XML, extended rulesets (#6)
fix: address 9 bugs from M11 manual-test sweep (#7)
ci: adopt CalVer + harden CI/release (port from atl)
```

Keep the subject line under ~72 characters. Explain *why* in the body if the change is not obvious from the diff.

## Reporting bugs and requesting features

- **Bug?** Use the [bug report template](ISSUE_TEMPLATE/bug_report.md) and include `dq --version`, the affected file (minimised), command line, expected vs actual, and logs (`RUST_LOG=dq=debug` or `-vv` for verbose tracing).
- **Feature?** Use the [feature request template](ISSUE_TEMPLATE/feature_request.md). Describe the use case before the implementation — "I want to lint Helm `values.yaml` against the chart's schema" is more useful than "add `dq lint --helm`".
- **Security?** Do **not** open a public issue. See [`SECURITY.md`](SECURITY.md) for the private disclosure process.
- **Question?** Prefer GitHub Discussions over Issues for usage questions.

## Code of Conduct

Participation in this project is governed by the [Code of Conduct](CODE_OF_CONDUCT.md). By contributing you agree to abide by its terms.
