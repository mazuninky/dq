# dq

[![CI](https://github.com/mazuninky/dq/actions/workflows/ci.yml/badge.svg)](https://github.com/mazuninky/dq/actions/workflows/ci.yml)
[![Release](https://github.com/mazuninky/dq/actions/workflows/release.yml/badge.svg)](https://github.com/mazuninky/dq/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.100-blue)](rust-toolchain.toml)

Agent-friendly Rust CLI for structured data files (YAML, JSON, TOML, HCL, INI, .env, CSV, Dockerfile, Markdown frontmatter, XML) and a platform for writing linters over them. Query, in-place edit with round-trip preservation, format conversion, JSON Patch / Merge Patch, and lint+autofix engine — all in a single static binary.

- **Read & query** with JSON Pointer (RFC 6901) and embedded jq (via `jaq`).
- **In-place edit** preserving comments, key order, and quote style for YAML/TOML.
- **Convert** between any pair of supported formats; `dq fmt` canonicalizes & sorts keys.
- **Patch** with RFC 6902 JSON Patch and RFC 7396 Merge Patch.
- **Lint** with 66 standard rules across `@std/{k8s, dockerfile, github-actions, markdown, npm, jsonschema, terraform, openapi, skills}`, plus user rules in `.dq/rules/`.
- **Autofix** via `dq fix` — rules carry `fix.ops` edit scripts (comments kept) or `fix.jq` whole-document transforms; idempotency enforced at runtime.
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

Prebuilt artifacts are available for Linux (x86_64, aarch64) and macOS (aarch64, x86_64). On platforms without a prebuilt asset, build from source — see below.

### Homebrew (macOS arm64 + x86_64, Linux x86_64 + aarch64)

A Homebrew tap is published at [`mazuninky/homebrew-tap`](https://github.com/mazuninky/homebrew-tap). The formula installs the binary, man pages, and shell completions for `bash`, `zsh`, and `fish`:

```sh
brew install mazuninky/tap/dq
```

The formula is bumped to each new release in the tap repository, shortly after the release is published.

### Quick install via the install script

For convenience on a trusted workstation, [`scripts/install.sh`](scripts/install.sh) automates download + checksum verification + extraction. **Note:** this pattern pipes a remote shell script into `sh` and executes it with your user's privileges. Pin to a specific version, or review the script first, before running on any machine that handles secrets:

```sh
# Recommended: pin to a specific release
curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/master/scripts/install.sh | sh -s -- --version v2026.47.3
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

Pin to a specific release tag (`v2026.47.3`) or to a digest (`@sha256:…`) for reproducibility. See [Use in GitHub Actions](#use-in-github-actions) below.

### From source

Requires Rust stable **1.100 or newer**.

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
dq self update --to v2026.47.3 # pin to a specific YYYY.WW.BUILD release
dq self update --dry-run       # download + verify SHA256, replace nothing
```

Before anything is extracted, `self update` checks the archive's SHA-256 against its line in the release's `dq-checksums.txt`. A mismatch, a release without `dq-checksums.txt`, a malformed file or one with no line for the archive refuses the update with exit 4 and leaves the installed binary untouched. The checksum comes from the same release as the archive, so it catches a corrupted or swapped download, not a compromised release; the build-provenance attestation is not checked — use `gh attestation verify` for that (see [Installation](#installation)).

## Quick start

```sh
# Read — JSON Pointer (RFC 6901) on any supported format
dq get deployment.yaml /spec/replicas
dq get terraform.tf /terraform/backend/s3/region
dq get pom.xml /project/0/version/0/#text
dq get post.md /frontmatter/value/title

# Query — embedded jq via jaq, with output rendered into any format
dq query '.spec.template.spec.containers[].image' deploy.yaml -F json
dq query '.children[] | select(.type == "heading") | .level' post.md

# Edit — atomic in-place with comment / key-order preservation
dq set deploy.yaml /spec/replicas 3 -i
dq set 'k8s/**/*.yaml' --jq '.spec.template.spec.containers[].image |= sub(":latest"; ":v1")' -i --parallel 4
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

# Fix — apply each matching rule's fix (fix.ops or fix.jq)
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
| Markdown | `.md`, `.markdown` | ✅ | — | full CommonMark + GFM as queryable AST; no edits (`fmt` keeps it verbatim) |
| Dockerfile | `Dockerfile`, `*.Dockerfile` | ✅ | — | read-only |
| ignore-lists | `.gitignore`, `.dockerignore` | ✅ | — | flat array |

Format is auto-detected from extension or filename and can be forced with `--input-format <format>`, which stdin (`-`) always needs: `cat deploy.yaml | dq get - /kind --input-format yaml`, `dq lint - --input-format yaml -F sarif < deploy.yaml`. `-F` is the output format on every command and never picks the parser, so `dq paths config.yaml -F json` reads YAML and prints JSON. `set`, `del`, `patch`, `merge`, `fmt` and `fix` keep a file in its own format: a `-F` naming another one is a usage error (exit 2), the same one is accepted; `convert` is how a file changes format.

Point edits (`set` and `del` with a pointer, `patch`, `merge`) change YAML, JSON and TOML in place, keeping comments and formatting. On HCL, XML, INI, dotenv, CSV/TSV and JSON Lines they change the value and write the whole file anew through the format's writer, as `set --jq` does: formatting is not kept, nor are comments (XML keeps its own). A value the format cannot hold (a nested map in INI, dotenv or CSV, a second XML root) is refused with exit 1 and the file is left as it was. JSON Lines records are addressed by index, as `get` addresses them (`dq set log.jsonl /0/level warn`). Markdown takes no point edits (exit 7).

## Linting

`dq lint` matches each input file against rulesets and emits diagnostics with file/line/column anchors. Auto-binding picks `@std/<namespace>` based on filename heuristics (`*.tf` → `@std/terraform`, `Dockerfile` → `@std/dockerfile`, etc.) and adds anything under `./.dq/rules/`.

```sh
dq lint k8s/**/*.yaml                    # auto-bind @std/k8s + .dq/rules/
dq lint --rules @std/k8s deploy.yaml     # explicit ruleset
dq check --rule k8s.no-latest-tag deploy.yaml   # single rule
dq check --inline 'id: my.rule
description: A Deployment declares its replicas.
severity: error
match: { format: yaml, filter: ".kind == \"Deployment\"" }
check: { jq: "select(.spec | has(\"replicas\") | not)", message: "no replicas" }' f.yaml
dq test crates/dq-lint/rules/            # run *.test.yml fixtures
dq test @std/k8s                        # run the fixtures embedded for @std/k8s
dq explain k8s.no-latest-tag             # rule description / severity / refs
dq rules list                            # all available rulesets
dq rules add @std/k8s                    # materialise @std/k8s under .dq/rules/k8s/
```

A rule's `check:` is a `oneOf` over four kinds:

- `jq` — embedded jq expression; **every value it emits is one violation**, `true`, `false` and `null` included, so it emits only the failures (`select(...)`) and nothing when the file passes. An array is one violation; emit its items (`.[]`) for one each. The message template reads fields of the violation (`{{ .name }}`).
- `schema` — inline JSON Schema 2020-12.
- `schema_file` — path to a JSON Schema file (resolved relative to the rule directory; `..` escapes rejected, `$ref` restricted to internal references).

A schema reads the document's own numbers. A NaN or an infinity (`.nan`, `-.inf`, TOML `nan`) is a `number`, never a `string` or an `integer`; an infinity is past every bound on its side, and a NaN fails every bound (`minimum`, `maximum`, the exclusive ones); neither is a multiple of anything. The bounds, `multipleOf`, `integer`, `enum` and `const` read a big number by its exact value, the schema's own as well as the document's, in a YAML rule as in a `schema_file`: `18446744073709551617` is past `maximum: 18446744073709551616`, `0.1` is not `const: 0.10000000000000000001`, `18446744073709551616` is an integer and `1e-400` is not; `x` is a `multipleOf` `d` when `x / d` is an integer, so `0.3` is a multiple of `0.1`, only `0` is one of `1e400`, and `multipleOf: 1e-400` compiles. A YAML schema may list `.inf` or `-.inf` in `enum` or `const`, where it matches the infinity of its sign; as a bound it does not compile. Two NaNs equal nothing — though `uniqueItems` counts them as one item. Messages spell these values as the document does: `.nan is not of type "string"`.
- `extract` + `nested` — composite cross-format rule. The outer jq returns `[{value, format, anchor}]`; each item is reparsed as the inner format and run through a `nested:` rule. Recursion bounded at depth 4. Inner-format parse failures emit `<outer>.parse-failed`.

A reference composite rule ships out of the box: `@std/markdown/code-blocks-yaml-valid`. The `@std/jsonschema/{kubernetes-crd-shape, helm-values-against-schema, openapi-3.1-shape}` rules are schema rules (`schema` / `schema_file`). Every rule field is in [docs/reference/lint-engine.md](docs/reference/lint-engine.md), the check kinds in [docs/reference/rule-checks.md](docs/reference/rule-checks.md).

### Output formats

| Format | Flag | Use case |
|---|---|---|
| Console | `-F console` (default) | terminal |
| SARIF 2.1.0 | `-F sarif` | GitHub Code Scanning |
| JUnit XML | `-F junit` | GitLab CI, Jenkins |
| TAP 13 | `-F tap` | TAP consumers |
| JSON | `-F json` | scripting, jq pipelines |

All of them go to stdout. `--strict` promotes warning-severity violations to failures (exit 1); an error-severity one is exit 4 either way.

## Autofix

Rules can carry an optional `fix:` that `dq fix` applies to every file the rule reports on, with the same write modes as `dq set` / `dq del`. Two forms:

- `fix.ops` (preferred) — a jq expression that returns a JSON Patch array (`add`, `replace`, `remove`). It is applied as a textual edit, so comments and layout survive on YAML, JSON and TOML; on the other writable formats the file is rewritten through its writer.
- `fix.jq` — a whole-document transform whose result replaces the document, re-emitted through the format's writer, so comments in it are dropped (same trade-off as `dq set --jq`).

```yaml
fix:
  ops: 'if has("license") then [] else [{op: "add", path: "/license", value: "UNLICENSED"}] end'
```

```sh
dq fix --check k8s/**/*.yaml             # pre-commit gate (exit 1 if changes pending)
dq fix --diff k8s/**/*.yaml              # unified diff to stdout
dq fix -i --rules @std/k8s deploy.yaml   # atomic in-place
dq fix -i --backup --parallel 4 'src/**/*.yaml'
```

Idempotency is enforced at runtime: a fix that would change the file again on a second run is skipped with a warning. Two reference rules ship with `fix:` blocks: `@std/npm/has-license` (`fix.ops`) and `@std/k8s/image-pull-policy-always` (`fix.jq`). The full contract is in [docs/reference/lint-engine.md](docs/reference/lint-engine.md#fixes).

## Configuration

User rules live under `./.dq/rules/` as `*.yml` files (one rule per file) and are auto-bound on every `dq lint` / `dq fix` invocation.

```yaml
# .dq/rules/no-debug-images.yml
id: org.no-debug-images
description: Debug and nightly images must not reach the cluster.
severity: error
match: { format: yaml, glob: 'k8s/**/*.yaml' }
check:
  jq: '.spec.template.spec.containers[]? | select(.image | test("debug|nightly"))'
  message: "container '{{ .name }}' uses {{ .image }}: no debug/nightly images in k8s manifests"
```

`dq rules add @std/<ns>` materialises a standard ruleset into `./.dq/rules/<ns>/` for local override or vendoring.

## Multi-document YAML streams

A YAML file with several `---`-separated documents (a Kubernetes manifest bundle, say) is a stream. `lint`, `check` and `fix` treat each document as a file of its own: rules match, check, locate and fix one document at a time, and diagnostics carry the line in the file. `dq fix` writes every document back, `---` separators included (a `fix.ops` script's `remove ""` drops its own document); a `fix.jq` re-emits only the content of the document it fixes (dropping the comments among it) and leaves its `---` line, the comment lines before the next `---` and the other documents byte for byte. On a YAML file of one document a `fix.jq` re-emit works the same way: the directives, the `---` line and the comment lines before and after the content stay.

`--doc N|all` picks the document a command works on; a file that is not a stream is document 0:

```sh
dq get bundle.yaml /metadata/name                 # document 0
dq --doc 1 get bundle.yaml /metadata/name         # the second document
dq --doc all query '.[].kind' bundle.yaml         # the stream as an array of documents
dq --doc 1 set bundle.yaml /spec/replicas 3 -i    # pointer relative to document 1
dq set bundle.yaml /1/spec/replicas 3 -i          # the same edit without --doc
dq set bundle.yaml /1 '{"kind": "Namespace"}' -i  # replace document 1 as a whole
dq del bundle.yaml /1 -i                          # remove document 1 with its --- line
dq --doc 1 lint bundle.yaml                       # lint the second document only
dq --doc 1 convert bundle.yaml -F json            # convert one document
```

Read commands (`get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `query`) read document 0 unless told otherwise. `keys` and `values` list an object's keys and values, or an array's indices and items; on a scalar they — and `len` on a number, a boolean or a null — are exit 1 (`cannot list the keys of '/n': it is int, not an object or an array`). `get`, `values`, `select` and `query` write a NaN or an infinity one way per output format, whatever format it came from: `.nan` / `.inf` / `-.inf` on the console, in YAML and in TOON, `nan` / `inf` in TOML, exit 1 in JSON and JSONL; a number too big for 64 bits keeps its exact value. A float is written in its shortest form that reads back as the same number — `1e300`, `1.5e20`, `3.0` in JSON, TOML, HCL, `.env`, INI, CSV and XML (HCL reads `3.0` back as a float); the console spells it as `-F yaml` does, `1.0e+300` — never as a run of 300 digits (TOON alone writes plain decimals, as its spec requires). In a `select` filter a NaN and an infinity are numbers: `$[?@.n > 1]` matches `.inf` and not `.nan`, and a NaN equals nothing (`!=` holds). The filter compares in `f64`, so two infinities are not `==`, and a number past `f64` precision compares as the `f64` nearest it. `set --jq` without `--doc` sees the stream as an array of documents and must return one. `--doc all` is refused by `set` and `del`, any `--doc` by `convert -i` and by commands that work on the whole file (`fmt`, `validate`, `diff`, `patch`, `merge`, …): a usage error, exit 2. An index past the last document is exit 2 as well; over several files (a glob, a directory, a list), a file without that document is skipped with a note on stderr and the run goes on — unless no file holds it, which is exit 2.

`set FILE /N VALUE` (or `dq --doc N set FILE '' VALUE`) writes document N anew through the YAML writer: its `---` line, the comment lines after its content and every other document stay byte for byte, the comments among its content go. `del FILE /N` removes document N with its `---` line and the comment lines just above it. In a file that is no stream, `set FILE '' VALUE` replaces the whole document the same way, keeping the comment lines it starts with (YAML, JSON and TOML; a TOML document must stay a table). `del FILE ''` and `set FILE ''` on a whole stream are usage errors, exit 2. `patch` addresses the documents of a stream as `/N` the same way (`remove /N`, `replace /N`, `/N/...` inside one; `add /N` inserts a new document there, `add /-` appends one); `merge` refuses a stream, exit 6: a merge patch applies to one document.

## Global write flags & exit codes

Write commands (`set`, `del`, `patch`, `merge`, `convert`, `fmt`, `fix`) share these flags:

| Flag | Effect |
|---|---|
| `-i`, `--in-place` | atomic rename write; keeps the file's permissions, edits the file a link leads to |
| `--diff` | unified diff to stdout, no write |
| `--backup` | with `-i`: keep the old file as `<file>.bak` next to it |
| `--check` | idempotency gate; exit 1 if changes pending |
| `--continue-on-error` | bulk partial-failure tolerant |
| `--parallel <N>` | rayon thread pool |
| `--sort-keys` | re-emit with sorted map keys (no-op for textual-edit `set`/`del`) |
| `--indent <N>` | JSON / JSONL only |
| `--allow-templates` / `--raw-template-strings` | Helm / Go-template guard |

Bulk multi-file mode prints `Modified: N, Skipped: M, Failed: K`; Skipped counts files whose bytes would not change (never rewritten, mtime kept) and files without the `--doc N` document. Single-file invocation stays byte-identical. In a glob, `*` and `?` stop at `/` and `**` crosses directories (`'k8s/**/*.yaml'`); a pattern without a directory (`'2-*.yaml'`) matches in the current directory, and an existing path is taken literally even with `[` in it. `convert` to stdout takes a glob only when it matches one file (exit 6 otherwise). `convert -i` does not replace a file that is already at the target (exit 6) unless `--overwrite` or `--backup` is given.

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | generic (`exists` answering no, `--check` changes pending, `PatchTestFailed`, a value a read command, `convert` or `set --jq` cannot write in the output format, such as NaN or Infinity in JSON; `keys` / `values` / `len` on a node of the wrong type) |
| 2 | not found (a JSON Pointer or a `--doc N` the file does not have); usage error (incl. `--doc` on a command that cannot honour it, stdin without `--input-format`, a `-F` naming another format than the file a write command keeps, `-i` or `--check` on stdin) |
| 3 | parse error (incl. templated file); invalid rule, or a rule that raised (`lint` / `check` / `fix`, unless `--allow-rule-errors`) |
| 4 | validate fail; `self update` refused an archive at its SHA-256 check |
| 5 | I/O error (read; `self check` / `update` network errors) |
| 6 | invalid input |
| 7 | write failed (write IO, the file changed while dq was running, renderer unavailable, textual edit refused because the file would not read back as the change (`edit_rejected`), bulk partial-failure, self-update atomic-replace failure) |

## Plugins (experimental)

WASM lint+fix plugins via a WIT-described ABI and a `wasmtime` runtime (Component Model). Each plugin is a separate `*.wasm` file loaded into a sandbox without WASI: no network, no filesystem, no processes. Fuel budget ~1 s CPU, memory cap 64 MiB. The package contract `dq:plugin@0.1.0` is semver-versioned: minor is additive, major is breaking (the host refuses to load a plugin with a different major). Behind the `plugins` cargo feature so the default static binary stays small.

```sh
# Build dq with plugin support enabled
cargo install --locked --features plugins --git https://github.com/mazuninky/dq dq-cli

# Build the example plugin
cd examples/plugin-rust && cargo component build --release --target wasm32-unknown-unknown
mkdir -p ../../plugins
cp target/wasm32-unknown-unknown/release/dq_plugin_example_noop.wasm ../../plugins/

# Use it
cd ../..
dq lint --plugins ./plugins config.yaml
dq fix  --plugins ./plugins config.yaml
```

`--plugins <DIR>` discovery is non-recursive and lexically sorted. Without the feature flag the flag is parsed but loading any `*.wasm` exits 6 (`InvalidInput`).

`get-root` and `get-at` hand a plugin the document as JSON: a number too big for 64 bits keeps its exact digits, and a NaN or an infinity — which JSON cannot hold — comes as the string `".nan"`, `".inf"` or `"-.inf"`. `jq.eval` runs the filter over the document's own value at an RFC 6901 pointer, so there a NaN is a number (`isnan` holds); its outputs come back encoded the same way.

> **v0.1.0 experimental WIT preview.** Breaking changes to the WIT schema, host-imported interfaces, and diagnostic / EditScript marshalling are possible before `v1.0.0`. Pin a specific dq version in CI until the ABI stabilizes.

See [examples/plugin-rust/](examples/plugin-rust/) for a minimal Rust reference plugin. The contract is [docs/plugin-abi.md](docs/plugin-abi.md) together with the WIT file [crates/dq-plugin/wit/dq-plugin.wit](crates/dq-plugin/wit/dq-plugin.wit).

## CI integration

`dq lint -F sarif` writes a SARIF 2.1.0 document to stdout that GitHub Code Scanning ingests directly:

```yaml
# .github/workflows/lint.yml
- name: Install dq
  run: curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/master/scripts/install.sh | sh

- name: Lint manifests
  continue-on-error: true   # exit 4 on an error-severity finding must not skip upload-sarif below
  run: dq lint -F sarif 'k8s/**/*.yaml' > dq-results.sarif

- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: dq-results.sarif
```

A file that does not parse stops `dq lint` with exit 3 and no SARIF. `dq validate -F sarif FILE` checks one file and reports a parse error as SARIF on stderr (nothing when the file parses, exit 4 when it does not).

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
        continue-on-error: true
        run: >
          docker run --rm -v "$PWD:/work" ghcr.io/mazuninky/dq@sha256:<digest>
          lint 'k8s/**/*.yaml' -F sarif > dq-results.sarif
      - uses: github/codeql-action/upload-sarif@v3
        if: always()
        with:
          sarif_file: dq-results.sarif
```

Get the digest from the summary of the [Release workflow run](https://github.com/mazuninky/dq/actions/workflows/release.yml) for the release you want to pin (the image is built there, after the release), or via `docker buildx imagetools inspect ghcr.io/mazuninky/dq:<version>`. Verify the attestation:

```sh
gh attestation verify oci://ghcr.io/mazuninky/dq:<version> --owner mazuninky
```

For non-containerised runners, install with the script and run `dq` directly — the SARIF integration in [CI integration](#ci-integration) above shows the script-based path.

## Claude Code skill

dq carries a skill for coding agents such as [Claude Code](https://claude.ai/code): all `dq` commands, flags, output formats and common workflows, so an agent can use `dq` without re-deriving its surface. The text is [`skills/dq/SKILL.md`](skills/dq/SKILL.md) and its references, embedded in the binary.

```sh
dq skill install            # ~/.agents/skills/dq, linked from ~/.claude/skills
dq skill install --project  # .agents/skills/dq of the current directory
dq skill status
dq skill remove
```

An installed skill follows dq: the first run of a newer version updates it, however dq was updated, unless you edited it. `DQ_NO_SKILL_UPDATE=1` turns that off. It needs no network. Details: [docs/reference/skill.md](docs/reference/skill.md).

The skill covers reads, in-place edits with round-trip preservation, format conversion, JSON Patch / Merge Patch, lint + autofix, SARIF / JUnit / TAP reporters, and CI integration patterns.

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
- **[skills/dq/SKILL.md](skills/dq/SKILL.md)** — Claude Code skill: install + common patterns + format coverage + exit codes.
- **[docs/](docs/README.md)** — contributor documentation: [architecture](docs/architecture.md), [reference](docs/README.md#reference) for every command, format and the lint engine, [roadmap](docs/roadmap.md) (plans in flight and open work), [decision log](docs/decisions.md), [plugin ABI](docs/plugin-abi.md).
- **[CHANGELOG.md](CHANGELOG.md)** — release notes.

## Releases & versioning

`dq` uses **calendar versioning** in the form `YYYY.WW.BUILD`:

- `YYYY` — ISO year.
- `WW` — ISO week number without a leading zero (`1`–`53`): Cargo versions are SemVer, which forbids one, so the first week of 2027 is `2027.1.1`.
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
2. `build` (matrix: `x86_64-linux-gnu`, `aarch64-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin`) — release binary + `dq generate-docs` man/completions, packaged as tar.gz with per-target `.sha256` sidecars.
3. `release` — combined `dq-checksums.txt`, [build provenance attestation](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations), GitHub Release with auto-generated notes.

The Homebrew formula (`Formula/dq.rb` in [mazuninky/homebrew-tap](https://github.com/mazuninky/homebrew-tap)) is bumped by a separate pull request there once the release is out; the release workflow does not touch the tap.

## License

MIT — see [LICENSE](LICENSE).
