# dq

`dq` (data query) — agent-friendly Rust CLI для работы со структурированными данными
(YAML/JSON/TOML/...) и платформа для написания линтеров поверх них. Query, in-place
edit с round-trip preservation, format conversion, JSON Patch / Merge Patch, и
linter-engine — всё в одном single static binary.

## Status

**M11 alpha — JSON Schema validation + composite rules + extended formats. M10 — `dq fix` autofix engine. M12 alpha (experimental) — WASM plugin ABI on WIT + wasmtime.** M1–M10 archived (see [openspec/changes/archive/](openspec/changes/archive/)); M11 lands as [openspec/changes/add-validation-and-extended-formats/](openspec/changes/add-validation-and-extended-formats/) and brings:

- **JSON Schema 2020-12 as a Rule type.** `Rule.check` is now a `oneOf` over `jq` / `schema` (inline) / `schema_file` (path resolved relative to the rule directory; absolute paths and `..` escapes rejected) / `extract`+`nested`. The `jsonschema` crate validates the document; `instancePath` becomes the diagnostic's `Pointer` and `keywordLocation` is included in the message. `$ref` is restricted to internal references — HTTP/file `$ref` is rejected at compile time. Three reference rules ship as `@std/jsonschema/{kubernetes-crd-shape, helm-values-against-schema, openapi-3.1-shape}`.
- **Composite rules (cross-format).** A rule can `extract:` substrings from one format (jq returns `[{value, format, anchor}]`), reparse each item as a different format, and run a `nested:` rule with diagnostics projected back to outer-file coordinates. Recursion bounded at `MAX_EXTRACT_DEPTH = 4`. Inner-format parse failures emit `<outer>.parse-failed` diagnostics. First reference rule: `@std/markdown/code-blocks-yaml-valid`.
- **Inline-level position spans.** YAML block scalars (`|`, `>`, `|-`, `>-`) and markdown fenced code blocks now carry `Provenance::Original.inline_offset = Some(InlineBaseline { 0, 1, 1 })` so composite-rule projection has sub-line precision. `Ir::inline_offset_for(&pointer)` is the new public lookup.
- **XML read+write.** `XmlFormat` via `quick-xml` 0.36 — element structure, attributes, comments, CDATA, processing instructions, namespace prefixes, and the XML declaration round-trip. Mixed content (text interleaved with child elements) folds into `"#text"` and emits a `tracing::warn!` on parse — that's the partial-round-trip contract for config-shaped XML.
- **Two new standard rulesets.** `@std/terraform` (8 rules) and `@std/openapi` (6 rules). Standard rule library now ships 64 rules across 8 namespaces (`k8s`, `dockerfile`, `npm`, `github-actions`, `markdown`, `jsonschema`, `terraform`, `openapi`).

M10 adds the `dq fix` subcommand: rules carry an optional `fix.jq` whole-document transform that the engine applies to every matching file. The handler honours the same write-mode discipline as `dq set` / `dq del` (`-i` atomic write, `--diff` unified-diff to stdout, `--check` pre-commit gate exit 1, `--continue-on-error`, `--parallel`). `Fixer` enforces idempotency at runtime — applying `fix.jq` twice must yield the same value or the rule is skipped with a `tracing::warn!` log line (rule-author bug, never silently double-applied). Two existing `@std` rules (`@std/k8s/image-pull-policy-always`, `@std/npm/has-license`) ship `fix:` blocks as proof. **Comment preservation**: same trade-off as `dq set --jq` — re-emit goes through `Format::write_with_options` and drops comments. M12 lands the `dq-plugin` crate, the `dq:plugin@0.1.0` WIT schema, and a `--plugins <DIR>` global flag for `dq lint` / `dq fix`; feature-gated behind `--features plugins` so the default static binary stays small. See [Plugins (experimental)](#plugins-experimental) below for the contract and a Rust reference plugin.

Anti-scope still deferred: community registry (M12+), `--quote-style` / `--flow-style` / `--strip-comments` (need comment-preserving emitter), XSD / RelaxNG / Schematron schema validators, OpenAPI runtime request/response validation, HCL spans (Terraform diagnostics currently report at line 1).

## Install

**curl-pipe-sh (Linux / macOS):**

```sh
curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/main/scripts/install.sh | sh
```

By default the script installs to `~/.local/bin` (non-root) or `/usr/local/bin` (root). Override with `--install-dir DIR` or `--version vX.Y.Z`.

**Homebrew (macOS / Linux):**

```sh
brew install mazuninky/tap/dq
```

**Docker:**

```sh
docker run --rm -v "$PWD:/work" mazuninky/dq:latest get config.yaml /name
# A FROM-scratch (~5 MiB) variant is also published as mazuninky/dq:scratch.
```

**Cargo (build from source):**

```sh
cargo install --locked --git https://github.com/mazuninky/dq dq-cli
```

**Self-update once installed:**

```sh
dq self check                  # is a newer release available?
dq self update                 # download + verify SHA256 + atomic replace
dq self update --to v0.5.0     # pin to a specific version
```

## Команды (M8)

```
# M1 read: get / exists / keys / values / len / type / paths / select / convert / validate
# M2 write (glob-aware in M3): set / del
dq set <FILE|GLOB> <POINTER> [VALUE]   # mkdir-p set; '-i' atomic write
dq del <FILE|GLOB> <POINTER>           # delete; missing pointer → exit 2

# M3 ops-as-data
dq patch <FILE|GLOB> @<ops.json>       # RFC 6902 JSON Patch (test op aborts atomically)
dq merge <FILE|GLOB> @<patch.json>     # RFC 7396 Merge Patch (null removes)
dq diff <A> <B> [-F json|--unified]    # structural diff, JSON Patch by default

# M3 cross-format
dq convert <FILE|GLOB> -i -F json      # in-place format swap; --keep-source preserves both

# M4 canonicalization
dq fmt <FILE|GLOB>                     # re-emit through native writer (drops comments)
dq fmt <FILE|GLOB> -i --sort-keys      # alphabetize keys at every depth, atomic in-place
dq fmt <FILE|GLOB> --check             # idempotency gate, exit 1 if any file would change
dq convert <FILE> -F json --indent 4   # 4-space indented JSON

# M5 new formats (auto-detected by extension or filename)
dq get terraform.tf /backend/s3/region # HCL — labels-as-keys nesting
dq paths app.ini                       # INI / .properties — section/key pointers
dq get .env /DATABASE_URL              # .env — flat KEY=VALUE map
dq convert users.csv -F json           # CSV / TSV — array-of-records
dq validate Dockerfile                 # Dockerfile — read-only, exit 4 on parse error
dq paths .gitignore                    # .gitignore / .dockerignore — read-only flat array
dq get post.md /title                  # Markdown frontmatter — YAML/TOML/JSON header

# M11 XML (read+write, conventional-key mapping)
dq get pom.xml /project/0/version/0/#text  # Maven `<version>` value
dq convert app.json -F xml             # write XML; round-trip is partial (mixed content opaque)

# M6 distribution
dq completions <shell>                 # bash/zsh/fish/powershell/elvish completion script
dq man [PAGE]                          # troff man page (top-level or per-subcommand)
dq self check                          # check for newer release on GitHub
dq self update [--to <ver>]            # atomic in-place update from GitHub Releases
dq validate -F sarif <FILE>            # SARIF 2.1.0 output for GitHub Code Scanning

# M7 transform layer (embedded jq via jaq)
dq query <EXPR> <FILE>                 # evaluate jq expression, render value stream via -F
dq query '.spec.containers[].image' deploy.yaml -F json
dq set <FILE> --jq '.spec.replicas |= . + 1' -i
dq set 'k8s/**/*.yaml' --jq '.image |= sub(":latest"; ":v1")' -i --parallel 4

# M8 lint engine — 64 standard rules across @std/{k8s, dockerfile, github-actions, markdown, npm, jsonschema, terraform, openapi}
dq lint k8s/**/*.yaml                  # auto-binds @std/k8s + ./.dq/rules/*.yml
dq lint --rules @std/k8s deploy.yaml   # explicit ruleset
dq lint -F sarif file.yaml > lint.sarif # SARIF for GitHub Code Scanning
dq lint -F junit file.yaml > lint.xml  # JUnit XML for GitLab CI / Jenkins
dq lint -F tap file.yaml               # TAP 13 stream
dq lint --strict file.yaml             # warn-severity violations also fail (exit 1)
dq check k8s.no-latest-tag deploy.yaml # single rule against files
dq check --inline 'id: my.rule\nseverity: error\nmatch: { format: yaml }\ncheck: { jq: "...", message: "..." }' f.yaml
dq test crates/dq-lint/rules/          # run *.test.yml fixtures (TAP / JSON output)
dq explain k8s.no-latest-tag           # description / severity / references
dq rules list                          # all available rulesets
dq rules add @std/k8s                  # materialise @std/k8s under ./.dq/rules/k8s/

# M9 markdown — full CommonMark + GFM AST as queryable typed-node tree
dq get post.md /frontmatter/value/title    # YAML/TOML/JSON header values folded into AST
dq query post.md '.children[] | select(.type == "heading") | .level'  # enumerate heading levels
dq query post.md '.children[] | select(.type == "code_block" and .lang == null)'  # code blocks missing lang
dq lint docs/**/*.md                       # auto-binds @std/markdown (18 rules)

# M10 autofix — apply each rule's fix.jq whole-document transform
dq fix --check k8s/**/*.yaml               # pre-commit gate: exit 1 if any file would be changed
dq fix --diff k8s/**/*.yaml                # preview the diff without writing
dq fix -i --rules @std/k8s deploy.yaml     # atomically apply every fix.jq (e.g. imagePullPolicy)
dq fix -i --rules @std/npm package.json    # apply npm.has-license et al.
```

Глобальные write-флаги: `-i/--in-place` (atomic rename), `--diff` (unified
diff stdout), `--backup` (`.bak`), **`--check`** (idempotency gate, exit 1
if changes pending), **`--continue-on-error`** (bulk partial-failure
tolerant), **`--parallel <N>`** (rayon thread pool), `--allow-templates`
/ `--raw-template-strings` (Helm/Go-template guard), **`--sort-keys`**
(re-emit with sorted map keys; no-op for textual-edit `set`/`del`/`patch`/`merge`),
**`--indent <N>`** (JSON/JSONL only in M4; YAML/TOML ignore).

Bulk summary в multi-file mode: `Modified: N, Skipped: M, Failed: K`.
Single-file invocation остаётся byte-identical с M2 — без summary, без
маркеров.

Exit-codes: 0 success, 1 GENERIC (incl. `--check` changes pending,
`PatchTestFailed`), 2 NOT_FOUND, 3 PARSE_ERROR (incl. TemplatedFile), 4
VALIDATE_FAIL, 5 IO_ERROR (read; **`self check`/`update` network errors**),
6 INVALID_INPUT, 7 WRITE_FAILED (write IO, renderer unavailable, **bulk
partial-failure**, **self-update atomic-replace failure**).

## Plugins (experimental)

WASM lint+fix плагины через WIT-описанный ABI и `wasmtime`-runtime
(Component Model). Каждый плагин — отдельный `*.wasm` файл, `dq` его
загружает sandbox'ом без WASI: ни сети, ни файловой системы, ни процессов;
fuel budget ~1 сек CPU, memory cap 64 MiB. Контракт пакета —
`dq:plugin@0.1.0`, версионируется по semver: minor — additive, major —
breaking (host откажется грузить плагин с другим major). Включается за
cargo feature `plugins`:

```sh
# Build dq with plugin support enabled.
cargo install --locked --features plugins dq-cli

# Build the example plugin (see examples/plugin-rust/README.md for the
# full recipe + alternative wasm-tools path).
cd examples/plugin-rust && cargo component build --release
mkdir -p ../../plugins
cp target/wasm32-wasip2/release/dq_plugin_example_noop.wasm ../../plugins/

# Use it.
cd ../..
dq lint --plugins ./plugins config.yaml
dq fix  --plugins ./plugins config.yaml
```

`--plugins <DIR>` discovery — non-recursive, lexically sorted `*.wasm`
под `<DIR>`. Без feature-flag'а флаг парсится, но при попытке загрузить
любой `*.wasm` exit 6 (`InvalidInput`).

> **Warning — v0.1.0 experimental WIT preview.** Breaking changes to the
> WIT schema, host-imported interfaces, and diagnostic / EditScript
> marshalling are possible before `v1.0.0`. Pin a specific dq version in
> CI until the ABI stabilizes.

Подробности:

- **[examples/plugin-rust/](examples/plugin-rust/)** — minimal Rust
  reference plugin (one demo lint diagnostic + empty-EditScript fix) с
  build-recipe для `cargo-component` и `wasm-tools component new`.
- **[openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md](openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md)** —
  authoritative WIT contract, error taxonomy, exit-code mapping.

## CI integration

`dq validate -F sarif` produces SARIF 2.1.0 documents that GitHub Code Scanning ingests directly:

```yaml
# .github/workflows/lint.yml
- name: Install dq
  run: curl -sSfL https://raw.githubusercontent.com/mazuninky/dq/main/scripts/install.sh | sh

- name: Validate manifests
  continue-on-error: true   # exit 4 on parse errors must not skip upload-sarif below
  # `dq validate` writes diagnostics to stderr; redirect with `2>`.
  run: dq validate -F sarif 'k8s/**/*.yaml' 2> dq-results.sarif

- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: dq-results.sarif
```

Pre-commit hook entries shipped at the repo root (`pre-commit-hooks.yaml`) cover `dq fmt --check` и `dq validate` for fast local feedback.

## Documentation

- **[dq-plan.md](dq-plan.md)** — полный план проекта, M1–M12 roadmap, архитектура,
  anti-scope.
- **[openspec/changes/add-transform-layer/](openspec/changes/add-transform-layer/)** —
  активный OpenSpec change M7: spec + design + tasks для transform layer (jq via jaq).
- **[openspec/changes/add-distribution/](openspec/changes/add-distribution/)** —
  M6 distribution change (committed, pending archive).
- **[openspec/changes/archive/](openspec/changes/archive/)** —
  архивные OpenSpec changes M1–M5.
- **[spikes/saphyr/RESULTS.md](spikes/saphyr/RESULTS.md)** — результаты спайка
  по textual-edit подходу для YAML round-trip.
- **[skill/SKILL.md](skill/SKILL.md)** — Claude Code skill: install + common patterns + format coverage + exit codes.

## License

MIT — see [LICENSE](LICENSE).
