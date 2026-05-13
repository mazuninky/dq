# dq

[![CI](https://github.com/mazuninky/dq/actions/workflows/ci.yml/badge.svg)](https://github.com/mazuninky/dq/actions/workflows/ci.yml)
[![Release](https://github.com/mazuninky/dq/actions/workflows/release.yml/badge.svg)](https://github.com/mazuninky/dq/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.94-blue)](rust-toolchain.toml)

Agent-friendly Rust CLI for structured data files (YAML, JSON, TOML, HCL, INI, .env, CSV, Dockerfile, Markdown frontmatter, XML) and a platform for writing linters over them. Query, in-place edit with round-trip preservation, format conversion, JSON Patch / Merge Patch, and lint+autofix engine — all in a single static binary.

- **Read & query** with JSON Pointer (RFC 6901) and embedded jq (via `jaq`).
- **In-place edit** preserving comments, key order, and quote style for YAML/TOML.
- **Convert** between any pair of supported formats; `dq fmt` canonicalizes & sorts keys.
- **Patch** with RFC 6902 JSON Patch and RFC 7396 Merge Patch.
- **Lint** with 64 standard rules across `@std/{k8s, dockerfile, github-actions, markdown, npm, jsonschema, terraform, openapi}`, plus user rules in `.dq/rules/`.
- **Autofix** via `dq fix` — rules carry `fix.jq` whole-document transforms; idempotency enforced at runtime.
- **Schema validation** with JSON Schema 2020-12 (inline or file-based) as a first-class rule type.
- **Composite rules** that extract one format embedded in another (e.g. YAML inside Markdown code blocks) and lint it with diagnostics projected back.
- **Plugins** (experimental, behind `--features plugins`) — sandboxed WASM lint+fix via the WIT-described `dq:plugin@0.1.0` ABI.
- **CI-friendly output**: SARIF 2.1.0 (GitHub Code Scanning), JUnit XML (GitLab/Jenkins), TAP 13.

## Installation

### From GitHub Releases with attestation verification (recommended)

Every `dq-*.tar.gz` published to GitHub Releases is signed via [SLSA build provenance](https://slsa.dev/). The attestation proves the archive was built by this repo's `release.yml` workflow at a specific tag, signed by a Sigstore short-lived certificate tied to GitHub's OIDC identity. Download and verify before installing:

```sh
# Download the artifact for your platform from the latest release
gh release download --repo mazuninky/dq --pattern 'dq-*-x86_64-unknown-linux-gnu.tar.gz'

# Verify the SLSA provenance (fails if the archive was not built by this repo)
gh attestation verify dq-*-x86_64-unknown-linux-gnu.tar.gz --repo mazuninky/dq

# Extract and install
tar -xzf dq-*-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 0755 dq-*/dq /usr/local/bin/dq
```

Prebuilt artifacts are available for Linux (x86_64, aarch64) and macOS (aarch64). On platforms without a prebuilt asset, build from source — see below.

### Homebrew (macOS arm64, Linux x86_64 + aarch64)

A Homebrew tap is published at [`mazuninky/homebrew-tap`](https://github.com/mazuninky/homebrew-tap). The formula installs the binary, man pages, and shell completions for `bash`, `zsh`, and `fish`:

```sh
brew install mazuninky/tap/dq
```

The formula tracks the latest release and is bumped automatically by the release workflow.

### Quick install via the install script

For convenience on a trusted workstation, [`scripts/install.sh`](scripts/install.sh) automates download + checksum verification + extraction. **Note:** this pattern pipes a remote shell script into `sh` and executes it with your user's privileges. Pin to a specific version, or review the script first, before running on any machine that handles secrets:

```sh
# Recommended: pin to a specific release
curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/master/scripts/install.sh | sh -s -- --version v2026.20.1
```

```sh
# Latest (unpinned)
curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/master/scripts/install.sh | sh
```

The script installs to `~/.local/bin` (non-root) or `/usr/local/bin` (root); use `--install-dir DIR` to override. It verifies SHA256 against the published `dq-checksums.txt` but does **not** run `gh attestation verify` — for full supply-chain assurance use the verified path above.

### Docker (GHCR)

Each release publishes a multi-arch image (`linux/amd64`, `linux/arm64`) to GHCR with build provenance and SBOM attached:

```sh
docker run --rm -v "$PWD:/work" ghcr.io/mazuninky/dq:latest get config.yaml /name
```

Pin to a specific release tag (`v2026.20.1`) or to a digest (`@sha256:…`) for reproducibility. See [Use in GitHub Actions](#use-in-github-actions) below.

### From source

Requires Rust stable **1.94 or newer**.

```sh
cargo install --locked --git https://github.com/mazuninky/dq dq-cli
```

To enable the experimental WASM plugin runtime:

```sh
cargo install --locked --features plugins --git https://github.com/mazuninky/dq dq-cli
```

### Self-update

Once installed, `dq` can update itself from GitHub Releases:

```sh
dq self check                  # is a newer release available?
dq self update                 # download + verify SHA256 + atomic replace
dq self update --to v2026.20.1 # pin to a specific YYYY.WW.BUILD release
```

## Quick start

```sh
# Read — JSON Pointer (RFC 6901) on any supported format
dq get deployment.yaml /spec/replicas
dq get terraform.tf /backend/s3/region
dq get pom.xml /project/0/version/0/#text
dq get post.md /frontmatter/value/title

# Query — embedded jq via jaq, with output rendered into any format
dq query '.spec.containers[].image' deploy.yaml -F json
dq query post.md '.children[] | select(.type == "heading") | .level'

# Edit — atomic in-place with comment / key-order preservation
dq set deploy.yaml /spec/replicas 3 -i
dq set 'k8s/**/*.yaml' --jq '.image |= sub(":latest"; ":v1")' -i --parallel 4
dq del config.toml /deprecated_key -i

# Patch — RFC 6902 JSON Patch & RFC 7396 Merge Patch
dq patch config.yaml @ops.json -i
dq merge package.json @overrides.json -i

# Convert between formats
dq convert users.csv -F json
dq convert app.json -F yaml -i

# Diff — structural, JSON Patch by default
dq diff a.yaml b.yaml
dq diff a.yaml b.yaml --unified

# Lint — auto-detects @std rulesets from filename + user rules under .dq/rules/
dq lint k8s/**/*.yaml
dq lint --rules @std/k8s deploy.yaml
dq lint -F sarif file.yaml > lint.sarif

# Fix — apply each matching rule's fix.jq transform
dq fix --check k8s/**/*.yaml          # pre-commit gate
dq fix --diff k8s/**/*.yaml           # preview only
dq fix -i --rules @std/k8s deploy.yaml
```

## Supported formats

| Format | Extensions | Read | Write | Round-trip notes |
|---|---|---|---|---|
| YAML | `.yaml`, `.yml` | ✅ | ✅ | textual edit preserves comments, anchors, quote style |
| JSON | `.json` | ✅ | ✅ | `preserve_order` for object keys |
| JSON Lines | `.jsonl`, `.ndjson` | ✅ | ✅ | one record per line |
| TOML | `.toml` | ✅ | ✅ | textual edit preserves comments & ordering |
| HCL | `.tf`, `.hcl` | ✅ | ✅ | labels-as-keys nesting; spans report at line 1 |
| XML | `.xml`, `.pom`, `.xsd` | ✅ | ✅ | partial round-trip; mixed content folds into `"#text"` |
| INI / properties | `.ini`, `.properties` | ✅ | ✅ | section/key pointers |
| dotenv | `.env`, `.env.*` | ✅ | ✅ | flat KEY=VALUE map |
| CSV / TSV | `.csv`, `.tsv` | ✅ | ✅ | array-of-records |
| Markdown | `.md`, `.markdown` | ✅ | ✅ (frontmatter) | full CommonMark + GFM as queryable AST |
| Dockerfile | `Dockerfile`, `*.Dockerfile` | ✅ | — | read-only |
| ignore-lists | `.gitignore`, `.dockerignore` | ✅ | — | flat array |

Format is auto-detected from extension or filename and can be forced with `-F`.

## Linting

`dq lint` matches each input file against rulesets and emits diagnostics with file/line/column anchors. Auto-binding picks `@std/<namespace>` based on filename heuristics (`*.tf` → `@std/terraform`, `Dockerfile` → `@std/dockerfile`, etc.) and adds anything under `./.dq/rules/`.

```sh
dq lint k8s/**/*.yaml                    # auto-bind @std/k8s + .dq/rules/
dq lint --rules @std/k8s deploy.yaml     # explicit ruleset
dq check k8s.no-latest-tag deploy.yaml   # single rule
dq check --inline 'id: my.rule
severity: error
match: { format: yaml }
check: { jq: ".spec | has(\"replicas\")", message: "no replicas" }' f.yaml
dq test crates/dq-lint/rules/            # run *.test.yml fixtures
dq explain k8s.no-latest-tag             # rule description / severity / refs
dq rules list                            # all available rulesets
dq rules add @std/k8s                    # materialise @std/k8s under .dq/rules/k8s/
```

A rule's `check:` is a `oneOf` over four kinds:

- `jq` — embedded jq expression returning a boolean or an array of failures.
- `schema` — inline JSON Schema 2020-12.
- `schema_file` — path to a JSON Schema file (resolved relative to the rule directory; `..` escapes rejected, `$ref` restricted to internal references).
- `extract` + `nested` — composite cross-format rule. The outer jq returns `[{value, format, anchor}]`; each item is reparsed as the inner format and run through a `nested:` rule. Recursion bounded at depth 4. Inner-format parse failures emit `<outer>.parse-failed`.

Reference composite rules ship out of the box: `@std/markdown/code-blocks-yaml-valid`, `@std/jsonschema/{kubernetes-crd-shape, helm-values-against-schema, openapi-3.1-shape}`.

### Output formats

| Format | Flag | Use case |
|---|---|---|
| Text | `-F text` (default) | terminal; one line per diagnostic |
| SARIF 2.1.0 | `-F sarif` | GitHub Code Scanning |
| JUnit XML | `-F junit` | GitLab CI, Jenkins |
| TAP 13 | `-F tap` | TAP consumers |
| JSON | `-F json` | scripting, jq pipelines |

`--strict` promotes warning-severity violations to failures (exit 1).

## Autofix

Rules can carry an optional `fix.jq` — a whole-document transform that the engine applies to every matching file. `dq fix` honours the same write-mode discipline as `dq set` / `dq del`:

```sh
dq fix --check k8s/**/*.yaml             # pre-commit gate (exit 1 if changes pending)
dq fix --diff k8s/**/*.yaml              # unified diff to stdout
dq fix -i --rules @std/k8s deploy.yaml   # atomic in-place
dq fix -i --backup --parallel 4 'src/**/*.yaml'
```

`Fixer` enforces idempotency at runtime — applying `fix.jq` twice must yield the same value or the rule is skipped with a `tracing::warn!`. Two reference rules ship with `fix:` blocks: `@std/k8s/image-pull-policy-always` and `@std/npm/has-license`.

**Caveat**: re-emit goes through the format's native writer, so YAML/TOML comments are dropped on fix (same trade-off as `dq set --jq`).

## Configuration

User rules live under `./.dq/rules/` as `*.yml` files (one rule per file) and are auto-bound on every `dq lint` / `dq fix` invocation.

```yaml
# .dq/rules/no-debug-images.yml
id: org.no-debug-images
severity: error
match: { format: yaml, path: 'k8s/**/*.yaml' }
check:
  jq: '.spec.containers[]?.image | test("debug|nightly") | not'
  message: 'no debug/nightly images allowed in k8s manifests'
```

`dq rules add @std/<ns>` materialises a standard ruleset into `./.dq/rules/<ns>/` for local override or vendoring.

## Global write flags & exit codes

Write commands (`set`, `del`, `patch`, `merge`, `convert`, `fmt`, `fix`) share these flags:

| Flag | Effect |
|---|---|
| `-i`, `--in-place` | atomic rename write |
| `--diff` | unified diff to stdout, no write |
| `--backup` | write `.bak` next to the file |
| `--check` | idempotency gate; exit 1 if changes pending |
| `--continue-on-error` | bulk partial-failure tolerant |
| `--parallel <N>` | rayon thread pool |
| `--sort-keys` | re-emit with sorted map keys (no-op for textual-edit `set`/`del`) |
| `--indent <N>` | JSON / JSONL only |
| `--allow-templates` / `--raw-template-strings` | Helm / Go-template guard |

Bulk multi-file mode prints `Modified: N, Skipped: M, Failed: K`. Single-file invocation stays byte-identical.

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | generic (`--check` changes pending, `PatchTestFailed`) |
| 2 | not found |
| 3 | parse error (incl. templated file) |
| 4 | validate fail |
| 5 | I/O error (read; `self check` / `update` network errors) |
| 6 | invalid input |
| 7 | write failed (write IO, renderer unavailable, bulk partial-failure, self-update atomic-replace failure) |

## Plugins (experimental)

WASM lint+fix plugins via a WIT-described ABI and a `wasmtime` runtime (Component Model). Each plugin is a separate `*.wasm` file loaded into a sandbox without WASI: no network, no filesystem, no processes. Fuel budget ~1 s CPU, memory cap 64 MiB. The package contract `dq:plugin@0.1.0` is semver-versioned: minor is additive, major is breaking (the host refuses to load a plugin with a different major). Behind the `plugins` cargo feature so the default static binary stays small.

```sh
# Build dq with plugin support enabled
cargo install --locked --features plugins dq-cli

# Build the example plugin
cd examples/plugin-rust && cargo component build --release
mkdir -p ../../plugins
cp target/wasm32-wasip2/release/dq_plugin_example_noop.wasm ../../plugins/

# Use it
cd ../..
dq lint --plugins ./plugins config.yaml
dq fix  --plugins ./plugins config.yaml
```

`--plugins <DIR>` discovery is non-recursive and lexically sorted. Without the feature flag the flag is parsed but loading any `*.wasm` exits 6 (`InvalidInput`).

> **v0.1.0 experimental WIT preview.** Breaking changes to the WIT schema, host-imported interfaces, and diagnostic / EditScript marshalling are possible before `v1.0.0`. Pin a specific dq version in CI until the ABI stabilizes.

See [examples/plugin-rust/](examples/plugin-rust/) for a minimal Rust reference plugin and [openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md](openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md) for the authoritative WIT contract.

## CI integration

`dq validate -F sarif` and `dq lint -F sarif` produce SARIF 2.1.0 documents that GitHub Code Scanning ingests directly:

```yaml
# .github/workflows/lint.yml
- name: Install dq
  run: curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/master/scripts/install.sh | sh

- name: Validate manifests
  continue-on-error: true   # exit 4 on parse errors must not skip upload-sarif below
  run: dq validate -F sarif 'k8s/**/*.yaml' 2> dq-results.sarif

- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: dq-results.sarif
```

Pre-commit hooks shipped at the repo root ([`.pre-commit-hooks.yaml`](.pre-commit-hooks.yaml)) cover `dq fmt --check` and `dq validate` for fast local feedback.

## Use in GitHub Actions

The release-time Docker workflow publishes a multi-arch image (`linux/amd64`, `linux/arm64`) to `ghcr.io/mazuninky/dq` with build provenance and SBOM attached, plus a separate signed [build provenance attestation](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations) issued through GitHub. Pin by digest for reproducibility:

```yaml
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Lint manifests
        uses: docker://ghcr.io/mazuninky/dq@sha256:<digest>
        with:
          args: lint 'k8s/**/*.yaml' -F sarif
      - uses: github/codeql-action/upload-sarif@v3
        if: always()
        with:
          sarif_file: dq-results.sarif
```

Get the digest from the [Docker workflow run summary](https://github.com/mazuninky/dq/actions/workflows/docker.yml) for the release you want to pin, or via `docker buildx imagetools inspect ghcr.io/mazuninky/dq:<version>`. Verify the attestation:

```sh
gh attestation verify oci://ghcr.io/mazuninky/dq:<version> --owner mazuninky
```

For non-containerised runners, install with the script and run `dq` directly — the SARIF integration in [CI integration](#ci-integration) above shows the script-based path.

## Claude Code skill

A `dq` skill for [Claude Code](https://claude.ai/code) is shipped under [`skill/SKILL.md`](skill/SKILL.md) with install instructions, common patterns, format coverage, and exit codes — so Claude can use `dq` as a tool without re-deriving its surface.

## Shell completions

```sh
dq completions bash > ~/.local/share/bash-completion/completions/dq
dq completions zsh  > "${fpath[1]}/_dq"
dq completions fish > ~/.config/fish/completions/dq.fish
dq man                                # troff man page on stdout
dq man lint                           # per-subcommand page
```

## Contributing

Contributions are welcome. Start with [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md) for build instructions, testing, and the pull-request workflow. Please report security issues privately — see [`.github/SECURITY.md`](.github/SECURITY.md).

For an overview of the source tree, conventions, and explicit anti-scope, see [`CLAUDE.md`](CLAUDE.md).

## Documentation

- **[CLAUDE.md](CLAUDE.md)** — repo orientation for contributors and Claude Code: crate layout, conventions, anti-scope, how to extend.
- **[skill/SKILL.md](skill/SKILL.md)** — Claude Code skill: install + common patterns + format coverage + exit codes.
- **[openspec/changes/](openspec/changes/)** — active and archived OpenSpec changes (specs + design + tasks).
- **[dq-plan.md](dq-plan.md)** — design doc: architecture, roadmap, anti-scope.
- **[CHANGELOG.md](CHANGELOG.md)** — release notes.

## Releases & versioning

`dq` uses **calendar versioning** in the form `YYYY.WW.BUILD`:

- `YYYY` — ISO year.
- `WW` — ISO week number (zero-padded, `01`–`53`).
- `BUILD` — monotonic build counter within that week, starting at `1`.

The version lives in the workspace root `[workspace.package].version`; every member crate inherits it via `version.workspace = true`. Git tag `vYYYY.WW.BUILD` is the source of truth.

**Cutting a release:**

```sh
scripts/bump-version.sh              # computes next version, updates Cargo.toml + lock, commits, tags
scripts/bump-version.sh --dry-run    # print the next version without side effects
git push origin master && git push origin v$(scripts/bump-version.sh --dry-run)
```

The push of a `vYYYY.WW.BUILD` tag triggers [.github/workflows/release.yml](.github/workflows/release.yml):

1. `verify-version` — asserts the tag matches `cargo pkgid -p dq-cli`.
2. `build` (matrix: `x86_64-linux-gnu`, `aarch64-linux-gnu`, `aarch64-apple-darwin`) — release binary + `dq generate-docs` man/completions, packaged as tar.gz with per-target `.sha256` sidecars.
3. `release` — combined `dq-checksums.txt`, [build provenance attestation](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations), optional cosign signing (only when `COSIGN_PRIVATE_KEY` is set), GitHub Release with auto-generated notes.
4. `bump-homebrew-tap` — opens a PR against [mazuninky/homebrew-tap](https://github.com/mazuninky/homebrew-tap) updating `Formula/dq.rb` (skipped without `HOMEBREW_TAP_TOKEN`).

## License

MIT — see [LICENSE](LICENSE).
