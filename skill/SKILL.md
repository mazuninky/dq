---
name: dq
description: Agent-friendly Rust CLI for YAML/JSON/TOML/HCL/INI/.env/CSV/Dockerfile/Markdown-frontmatter and a linter platform. Use for queries on structured data, in-place edits with round-trip preservation (comments and ordering kept intact), format conversion, JSON Patch / Merge Patch application, and CI validation. Triggers on yaml query / json patch / json pointer / kubernetes manifest / helm values / github actions yaml / toml edit / round-trip yaml / convert yaml to json / fmt yaml / yaml lint.
version: 0.6.0
---

# dq — data query CLI

`dq` is an agent-friendly Rust CLI for structured data files (YAML, JSON, TOML, HCL, INI, .env, CSV, Dockerfile, ignore-lists, Markdown frontmatter) and a platform for writing linters over them. It preserves comments, key order, and quote style on in-place edits; emits structured errors with line/col/snippet; and ships a SARIF reporter for GitHub Code Scanning integration.

## Install

**curl-pipe-sh (Linux / macOS):**
```sh
curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/main/scripts/install.sh | sh
```

**Homebrew (macOS / Linux):**
```sh
brew install mazuninky/tap/dq
```

**Docker:**
```sh
docker run --rm -v "$PWD:/work" mazuninky/dq:latest get config.yaml /name
```

**Cargo:**
```sh
cargo install dq-cli
```

## Common patterns

### Read

```sh
# JSON Pointer reads (RFC 6901). No DSL — just '/path/to/key'.
dq get deployment.yaml /spec/replicas
dq get config.toml /database/host

# JSONPath for queries that span multiple matches (RFC 9535).
dq select kustomize.yaml '$.resources[*].name'

# List every addressable pointer in the document.
dq paths values.yaml | head -20

# Map-side metadata.
dq keys deployment.yaml /spec
dq values deployment.yaml /metadata/labels
dq len deployment.yaml /spec/template/spec/containers
dq type deployment.yaml /spec/replicas        # int
dq exists deployment.yaml /spec/foo           # exit 0 if present, 2 otherwise
```

### Write (in-place, round-trip preserving)

```sh
# Single-file write. Comments and key order survive.
dq set deployment.yaml /spec/replicas 5 -i

# Multi-file glob with parallel workers.
dq set 'k8s/*.yaml' /spec/template/spec/imagePullPolicy IfNotPresent -i --parallel 4

# Diff before commit, no write.
dq set values.yaml /image/tag v2 --diff

# Idempotency gate for CI: exit 1 if any file would change.
dq fmt --check '*.yaml'

# Atomic backup before write.
dq set production.yaml /api/key new-key -i --backup

# Delete a key.
dq del deployment.yaml /metadata/annotations/old-flag -i
```

### Patch / merge (RFC 6902 / 7396)

```sh
dq patch deployment.yaml @ops.json -i      # JSON Patch — atomic; failed `test` op rolls back
dq merge config.yaml @overrides.json -i    # JSON Merge Patch — null removes
```

### Convert between formats

```sh
dq convert deployment.yaml -F json --indent 2
dq convert config.toml -F yaml -i              # in-place format swap
dq convert users.csv -F json                   # CSV → array-of-objects JSON
dq convert post.md -F json                     # frontmatter → JSON (body dropped)
```

### Format / canonicalize

```sh
dq fmt deployment.yaml -i                       # re-emit through native writer
dq fmt 'k8s/*.yaml' -i --sort-keys              # deep-recursive key sort
dq fmt --check 'k8s/*.yaml'                     # CI-friendly idempotency check
```

### CI integration (GitHub Actions)

```yaml
- name: Validate manifests with dq → SARIF
  continue-on-error: true   # exit 4 on parse errors must not skip upload-sarif below
  run: |
    curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/main/scripts/install.sh | sh
    # `dq validate` writes diagnostics to stderr — capture with `2>` so the
    # SARIF document lands in the file the next step ingests.
    dq validate -F sarif 'k8s/*.yaml' 2> dq-results.sarif

- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: dq-results.sarif
```

### Self-update

```sh
dq self check                  # is a newer release available?
dq self update                 # download + verify SHA256 + atomic replace
dq self update --to v0.5.0     # pin to a specific version
```

### Shell completions and man pages

```sh
dq completions bash > /etc/bash_completion.d/dq
dq completions zsh > "${fpath[1]}/_dq"
dq completions fish > ~/.config/fish/completions/dq.fish

dq man | man -l -              # top-level man page
dq man set | man -l -          # per-subcommand
```

## Format coverage

| Format            | Read | Write | Round-trip preservation |
|-------------------|------|-------|-------------------------|
| JSON              | ✓    | ✓     | full                    |
| YAML              | ✓    | ✓     | comments + order        |
| TOML              | ✓    | ✓     | comments + order        |
| JSONL             | ✓    | ✓     | n/a (line-oriented)     |
| TOON              | —    | ✓     | n/a (write-only)        |
| HCL / .tf         | ✓    | ✓     | best-effort (no comments) |
| INI / .properties | ✓    | ✓     | section order; no comments |
| .env              | ✓    | ✓     | source order; quotes     |
| CSV / TSV         | ✓    | ✓     | n/a (tabular)            |
| Dockerfile        | ✓    | —     | read-only                |
| .gitignore / .dockerignore | ✓ | — | read-only                |
| Markdown frontmatter | ✓ | ✓    | YAML/TOML/JSON header; body verbatim |

`fmt` and `convert` re-emit through the native writer (drops comments — intentional per `gofmt`/`prettier` contract). `set` / `del` / `patch` / `merge` use a textual-edit splice path that preserves comments.

## Exit codes

| Code | Meaning                                                        |
|-----:|----------------------------------------------------------------|
|    0 | Success.                                                       |
|    1 | Generic failure (incl. `--check` saw pending changes; `JSON Patch test op` failed). |
|    2 | NOT_FOUND — JSON Pointer addressed a node that does not exist. |
|    3 | PARSE_ERROR — input file failed format-specific parsing.       |
|    4 | VALIDATE_FAIL — `validate` command saw a parse error.          |
|    5 | IO_ERROR — read failed; `self check`/`update` network failure. |
|    6 | INVALID_INPUT — bad CLI flag combination, unknown format, etc. |
|    7 | WRITE_FAILED — atomic write failed; bulk partial-failure mode. |

## Anti-scope (what dq does NOT do)

- **No DSL.** Path syntax is JSON Pointer (RFC 6901) for `get`/`set`/etc. and JSONPath (RFC 9535) for `select`. No bespoke language.
- **No interactive prompts.** Everything is non-interactive — works identically under `| cat`, in CI, in scripts.
- **No network access** outside `dq self check` and `dq self update`. No telemetry.
- **No XML write, no full Markdown body parsing** (yet — M9 / M11 territory).
- **No `query` / `lint` / `fix` commands** in the M6 release. Those land in M7 (jq integration), M8 (lint engine), M10 (autofix).

## Project links

- Homepage: <https://github.com/mazuninky/dq>
- Plan / roadmap: [dq-plan.md](https://github.com/mazuninky/dq/blob/main/dq-plan.md)
- Issues: <https://github.com/mazuninky/dq/issues>
