# Format reference

Per-format details, write-mode semantics, and the contract behind every output format. Read this when something round-trips badly, when picking between `set POINTER VALUE` and `set --jq`, or when wiring `dq` into CI.

## Format coverage matrix

| Format            | Extensions                       | Read | Write | Pointer prefix examples                                              | Round-trip notes                                              |
|-------------------|----------------------------------|------|-------|----------------------------------------------------------------------|----------------------------------------------------------------|
| YAML              | `.yaml`, `.yml`                  | ✓    | ✓     | `/spec/replicas`, `/items/0/name`                                    | Textual-edit preserves comments, anchors, quote style, key order. Native writer drops them. |
| JSON              | `.json`                          | ✓    | ✓     | `/users/0/email`                                                     | `preserve_order` keeps object key order on writes.            |
| JSONL / NDJSON    | `.jsonl`, `.ndjson`              | ✓    | ✓     | line-addressable as `/0`, `/1`, …                                    | Line-oriented; no comment concept.                            |
| TOML              | `.toml`                          | ✓    | ✓     | `/database/host`, `/dependencies/serde`                              | Textual-edit preserves comments and ordering.                 |
| HCL / Terraform   | `.tf`, `.hcl`                    | ✓    | ✓     | `/resource/aws_s3_bucket/example/bucket`                              | Labels become nested keys. Spans report at line 1 only.       |
| XML / Maven       | `.xml`, `.pom`, `.xsd`           | ✓    | ✓     | `/project/0/version/0/#text`, `/dependencies/0/dependency/0/groupId/0/#text` | Every element folds to an array (siblings repeat). Character data lives under `#text`. Mixed content collapses. |
| INI / properties  | `.ini`, `.properties`            | ✓    | ✓     | `/server/host`, `/database/url`                                      | Two-level: `/section/key`. No comments after write.           |
| dotenv            | `.env`, `.env.*`                 | ✓    | ✓     | `/DATABASE_URL`, `/PORT`                                             | Flat `KEY=VALUE`. Source order + quoting preserved on textual edit. |
| CSV / TSV         | `.csv`, `.tsv`                   | ✓    | ✓     | `/0/name`, `/0/age` (array of records, header = keys)                | Tabular; no nested structure.                                 |
| Markdown          | `.md`, `.markdown`               | ✓    | ✓ (frontmatter only) | `/frontmatter/value/title`, `/frontmatter/kind`, `/children/0/level` (CommonMark AST) | Frontmatter (`---` YAML / `+++` TOML / `{}` JSON) is parsed; body becomes a queryable AST. Writes touch frontmatter only — body stays verbatim. |
| Dockerfile        | `Dockerfile`, `*.Dockerfile`     | ✓    | —     | `/instructions/0/cmd`                                                | Read-only.                                                    |
| ignore-lists      | `.gitignore`, `.dockerignore`    | ✓    | —     | `/0`, `/1`, …                                                        | Flat array. Read-only.                                        |
| TOON              | n/a                              | —    | ✓     | n/a                                                                  | Output-only; agent-friendly compact token format.             |

Format is auto-detected from extension or filename. Force with `-F <format>`. `-F` is mandatory when reading from `-` (stdin) — no extension to sniff.

## Write modes

`dq` has three write paths, each with a different contract. Picking the wrong one is the most common source of "the comments disappeared!" bug reports.

### 1. Textual-edit splice — comments PRESERVED

Used by `set POINTER VALUE`, `del POINTER`, `patch FILE @ops.json`, `merge FILE @doc.json`. The engine parses the file, locates the byte range that backs the target node, and splices the new bytes in. Anything outside the spliced range (comments, ordering, quote style for YAML/TOML, blank lines) survives untouched.

```bash
# Pointer-edit — preserves all surrounding context
dq set deploy.yaml /spec/replicas 5 -i
dq del config.toml /[features]/legacy -i
dq patch deploy.yaml @ops.json -i
dq merge package.json @overrides.json -i
```

**Use this path** when you need to bump one value in a file authored by a human, and that human expects their comments to still be there tomorrow.

### 2. jq-driven transform — comments DROPPED

Used by `set --jq EXPR`, `fix`. The whole document goes through jq (jaq), then back through the format's native writer.

```bash
# Conditional / structure-changing transform
dq set 'k8s/**/*.yaml' --jq '.spec.template.spec.imagePullPolicy = "IfNotPresent"' -i
dq set values.yaml --jq '.image |= sub(":latest"; ":v1")' -i
dq fix -i 'k8s/**/*.yaml'                 # @std/* fix.jq blocks
```

**Use this path** when you need conditional logic, walks, or structural rewrites — the kind of thing JSON Pointer alone cannot express. Accept that comments are gone.

The `--jq` filter must produce **exactly one** output value. Multi-output streams (e.g. `.[]`) and empty streams (e.g. `empty`) are rejected with INVALID_INPUT — preventing silent data loss.

### 3. `convert` / `fmt` — comments DROPPED

```bash
dq convert config.toml -F yaml -i         # format swap
dq convert deploy.yaml -F json --indent 2
dq fmt 'k8s/**/*.yaml' -i --sort-keys     # canonicalise + sort
dq fmt --check 'k8s/**/*.yaml'            # CI idempotency gate
```

Same trade-off as the jq-driven path — the entire document is re-emitted through the native writer. `fmt` exists precisely so the project can adopt a `gofmt` / `prettier` posture: one canonical form per file.

### Write-mode flags

`set`, `del`, `patch`, `merge`, `convert`, `fmt`, `fix` all share these:

| Flag                    | Effect                                                                     |
|-------------------------|----------------------------------------------------------------------------|
| `-i`, `--in-place`      | Atomic rename write (tempfile + rename).                                   |
| `--diff`                | Unified diff to stdout, no write.                                          |
| `--backup`              | Write `.bak` next to the file (requires `-i`).                             |
| `--check`               | Idempotency gate — exit 1 if changes pending, no write.                    |
| `--continue-on-error`   | Bulk: don't abort on the first failing file.                               |
| `--parallel <N>`        | Rayon worker count for bulk mode. `0` = `current_num_threads()`.           |
| `--sort-keys`           | Deep-recursive map key sort (no-op for textual-edit `set`/`del`).          |
| `--indent <N>`          | JSON / JSONL only; YAML/TOML ignore.                                       |
| `--allow-templates`     | Skip template-block fix-up; parse only when the file happens to be valid as-is. |
| `--raw-template-strings`| Substitute `{{ ... }}` with opaque placeholders, restore on write.         |

`--check` is mutually exclusive with `-i`, `--diff`, and `--backup`. `--backup` requires `-i`.

### Bulk mode

When the input is a glob that matches multiple files (`'k8s/**/*.yaml'`, `'*.toml'`), `dq` switches to bulk:

- Prints a summary on stderr: `Modified: N, Skipped: M, Failed: K`
- Without `--continue-on-error`: aborts on the first failure, exits 7
- With `--continue-on-error`: processes every file, exits 7 if any failed, 0 otherwise
- Single-file invocation (glob matches one file) is byte-identical to non-glob mode

```bash
dq set 'k8s/*.yaml' /spec/template/spec/imagePullPolicy IfNotPresent -i --parallel 4
dq set 'k8s/*.yaml' /spec/replicas 3 -i --continue-on-error
```

### Templated files (Helm, Argo, GitHub Actions)

YAML files containing raw template syntax (`{{ .Values.image }}`, `${{ secrets.X }}`) typically fail to parse. Two escape hatches:

- **`--raw-template-strings`** (recommended) — pre-scan substitutes every `{{ ... }}` block with a deterministic placeholder string, parses, edits, then restores the originals on write. Works for almost every Helm / Argo / GitHub Actions file.
- **`--allow-templates`** — does no substitution. Only useful when the file happens to be valid YAML/TOML/JSON even with the template syntax in place (uncommon).

```bash
dq set values.yaml /image/tag v2 -i --raw-template-strings
dq lint --raw-template-strings 'charts/*/values.yaml'
```

Without either flag, templated files exit 3 (parse error).

### Atomic write contract

Every `-i` write goes through:

1. Render new bytes into a tempfile in the same directory.
2. `fsync` the tempfile.
3. `rename(tmp, target)` — atomic per POSIX.

So either the old file is intact, or the new file is fully written. Power-loss / SIGKILL never leaves a half-written target. `--backup` saves the old contents as `<file>.bak` before the rename.

Bulk uses one tempfile per target file, processed in parallel under `--parallel`. A failing rename in one file does not affect others; the summary reports per-file outcomes.

## Output formats

`-F <format>` is global — every command honours it. Available values: `console` (default), `json`, `yaml`, `toml`, `jsonl`, `toon`, `sarif`, `junit`, `tap`.

**`-F` has cross-talk with input parsing.** It always sets the output format, but on several read commands (`get`, `select`, `keys`, `values`, `len`, `type`, `exists`, `paths`) it ALSO overrides input-format detection. `query` is currently more forgiving — it uses `-F` only as an input hint when needed. Treat the strict behaviour as the contract: when reading from a file with a normal extension, either omit `-F` or pass a value matching the file's actual format.

```bash
# WRONG — forces JSON input parser on a YAML file; some commands exit 3
dq -F json select deploy.yaml '$.spec.replicas'    # parse error, exit 3
dq -F json get deploy.yaml /spec/replicas          # parse error, exit 3

# RIGHT — let extension drive input format; use -F only on output stages
dq select deploy.yaml '$.spec.replicas'                # default console output
dq query '.spec.replicas' deploy.yaml -F toon          # YAML in (by extension), TOON out

# Stdin needs -F because there's no extension to sniff
cat deploy.yaml | dq -F yaml query '.spec.replicas' -
```

| Format    | Use case                                                       | Notes |
|-----------|----------------------------------------------------------------|-------|
| `console` | Human-readable; lint produces one line per diagnostic          | Default. For read commands, mostly renders as compact YAML. |
| `json`    | Scripting, `jq` pipelines, machine readers                     | `--indent N` controls pretty-print width. |
| `yaml`    | Reading YAML through a transform                               | Native writer (comments dropped on round-trip). |
| `toml`    | Writing TOML output                                            | Same. |
| `jsonl`   | One value per line; great for line-oriented pipelines          | `--indent` ignored. |
| `toon`    | **Agent-friendly** — preserves all data in significantly fewer tokens than JSON | Use when the output goes back into your (the agent's) context. |
| `sarif`   | SARIF 2.1.0 for GitHub Code Scanning                           | **Emitted to stderr.** Redirect with `2>`. |
| `junit`   | JUnit XML for GitLab / Jenkins / etc.                          | Stderr. |
| `tap`     | TAP 13 for Bats, `prove`, other TAP consumers                  | Stdout. |

### Picking by reader

```bash
# Agent reads the output → toon
dq -F toon query '.spec.containers[].image' deploy.yaml

# Script parses the output → json
dq -F json query '.items[] | {kind, name}' k8s.yaml | jq -r '.name'

# CI report ingest → sarif (stderr!) or junit
dq lint -F sarif 'k8s/**/*.yaml' 2> dq-results.sarif
dq lint -F junit 'src/**/*.yaml' 2> dq-junit.xml
```

### Console-format gotcha

For read commands (`get`, `query`, `select`), `-F console` renders compact-YAML-ish output that's pleasant for humans but not stable for parsers. If a downstream tool consumes the output, switch to `-F json` or `-F toon`.

## Self-update & versioning

CalVer: `YYYY.WW.BUILD` (e.g. `v2026.20.1`). Git tag is the source of truth.

```bash
dq self check                  # is a newer release available?
dq self update                 # download + SHA256 verify + atomic replace
dq self update --to v2026.20.1 # pin to a specific YYYY.WW.BUILD release
```

`self update` exits 5 on network failure, 7 on atomic-replace failure. The new binary is verified against the published `dq-checksums.txt` before the replace; mismatches abort with exit 7.

## Anti-examples

**Expecting `fmt` to preserve YAML comments**
```bash
# WRONG — `fmt` is canonicalisation, it goes through the native writer
dq fmt deploy.yaml -i
# RIGHT — for a single value bump, use textual-edit set
dq set deploy.yaml /spec/replicas 5 -i
# If canonicalisation is the goal, expect comments to be gone (intentional)
```

**Using `--diff` and `-i` together**
```bash
# WRONG — mutually exclusive output modes (exit 6)
dq set values.yaml /image/tag v2 --diff -i
# RIGHT — separate steps
dq set values.yaml /image/tag v2 --diff       # preview
dq set values.yaml /image/tag v2 -i           # apply
```

**`--indent` on YAML / TOML**
```bash
# WRONG — silently ignored; YAML/TOML use their own emitter
dq convert deploy.yaml -F yaml --indent 4
# RIGHT — JSON / JSONL only
dq convert deploy.yaml -F json --indent 4
```

**Helm values without `--raw-template-strings`**
```bash
# WRONG — `{{ ... }}` confuses the YAML parser, exit 3
dq set values.yaml /image/tag v2 -i
# RIGHT — substitute templates, restore on write
dq set values.yaml /image/tag v2 -i --raw-template-strings
```

**Sending SARIF to stdout in CI**
```bash
# WRONG — SARIF goes to stderr; `>` captures empty stdout
dq lint -F sarif 'k8s/**/*.yaml' > dq.sarif
# RIGHT
dq lint -F sarif 'k8s/**/*.yaml' 2> dq.sarif
```
