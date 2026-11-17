# dq — Claude Code project context

`dq` (data query) is a single-binary Rust CLI for structured data files (YAML/JSON/TOML/HCL/INI/.env/CSV/Dockerfile/Markdown/XML) and a lint+autofix engine on top. The user-facing surface lives in [README.md](README.md). This file is for Claude — repo orientation, what's experimental, conventions, and explicit anti-scope.

## Repo layout

```
crates/
  dq-cli/        clap CLI, subcommand handlers, output renderers
  dq-core/       parsers, IR, textual-edit (saphyr) writers, formats
  dq-exec/       span-aware lint pipeline + reporters (SARIF/JUnit/TAP/text)
  dq-lint/       rule loader, jq evaluator (jaq), composite rules, @std rules
  dq-transform/  jq-driven transforms (set --jq, query)
  dq-plugin/     WASM plugin runtime (wasmtime, WIT contract, behind feature)
examples/        plugin-rust reference plugin
spikes/          throwaway investigations (saphyr/, etc.)
openspec/        OpenSpec changes — active under changes/, shipped under changes/archive/
docs/archive/    historical docs
skills/dq/       Claude Code skill bundle published to skills.sh (SKILL.md + skill.json)
.claude/skills/  contributor-only skills (openspec-*) — see "Skills" convention below
dq-plan.md       architecture + roadmap (treat as design doc, not as truth about current state)
```

## Stable surface vs experimental

Everything described in [README.md](README.md) is the stable surface — assume it works and write code against it. Two pockets are explicitly experimental and behind opt-in:

- **WASM plugin runtime** — `dq-plugin` crate, gated by `--features plugins`. WIT contract is `dq:plugin@0.1.0`; v0.1.0 is a preview, breaking changes possible before v1.0.0. Sandbox runs without WASI: no network, no filesystem, no processes. Fuel ~1 s CPU, memory cap 64 MiB. Plugin discovery is non-recursive lexically-sorted `*.wasm` under `--plugins <DIR>`. Without the feature flag the flag is parsed but loading any `*.wasm` exits 6 (`InvalidInput`). Spec: [openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md](openspec/changes/add-ir-foundation/specs/data-query-plugin-abi/spec.md).

For active in-flight work, look at the directory listing of [openspec/changes/](openspec/changes/) — anything not under `archive/` is current. Don't rely on this CLAUDE.md to track which spec is active; it'll go stale.

## Anti-scope (deferred / explicitly out)

These are *decisions*, not omissions — don't add them without a spec change:

- Community plugin registry.
- `--quote-style` / `--flow-style` / `--strip-comments` flags. Require a comment-preserving emitter; currently only the textual-edit path preserves, native writers don't.
- XSD / RelaxNG / Schematron schema validators. Only JSON Schema 2020-12 is supported.
- OpenAPI runtime request/response validation. Only OpenAPI shape rules.
- HCL spans — Terraform diagnostics report at line 1 today.
- Outbound network calls from rules — `$ref` and `schema_file` are local-only; `extract`+`nested` reparses inline. The CLI itself only hits the network for `dq self check` / `update`.

## Build & test

```sh
cargo build --workspace --all-targets
cargo test  --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo deny check
```

CI mirrors this exactly ([.github/workflows/ci.yml](.github/workflows/ci.yml)). MSRV is **1.94**, pinned in [rust-toolchain.toml](rust-toolchain.toml).

Plugin runtime tests are gated:

```sh
cargo test -p dq-plugin --features plugins
```

End-to-end smoke for the example plugin:

```sh
cd examples/plugin-rust && cargo component build --release
cd ../.. && cargo run --features plugins -- lint \
    --plugins examples/plugin-rust/target/wasm32-wasip2/release config.yaml
```

## Conventions

- **Errors**: `thiserror` for typed enums in libraries, `anyhow` only in `dq-cli` for the topmost handler. Each error variant maps to a fixed exit code (see README's exit-codes table).
- **Tracing**: `tracing` + `tracing_subscriber` everywhere. `tracing::warn!` for rule-author bugs (idempotency violation, mixed XML content). Don't `eprintln!` from libraries.
- **Write semantics**: textual-edit (`set` / `del` of structural pointers on YAML / TOML) preserves comments and ordering; jq-driven (`set --jq`, `fix`) re-emits via the format's writer and drops comments. The trade-off is intentional — surface it to users when it matters.
- **Atomic writes**: every `-i` write goes through tempfile + rename. Bulk runs report `Modified / Skipped / Failed`; bulk uses exit 7 if any file failed.
- **Format detection**: extension + filename heuristics in `dq-core/src/formats/detect.rs`. `-F <format>` overrides. Unknown extensions are an error, not a guess.
- **`@std` rule namespacing**: `@std/<namespace>/<rule-id>` — namespace is the directory under `crates/dq-lint/rules/std/`. User rules under `./.dq/rules/` are bound automatically without a namespace prefix.
- **Composite rules**: `extract:` returns `[{value, format, anchor}]`; each item is reparsed in a different format and run through a `nested:` rule. Recursion bounded at `MAX_EXTRACT_DEPTH = 4`. Inner-format parse failures emit `<outer>.parse-failed`. Inline position spans live in `Provenance::Original.inline_offset`; the public lookup is `Ir::inline_offset_for(&pointer)`.
- **Schema rules**: `$ref` restricted to internal references — HTTP/file `$ref` rejected at compile time. `schema_file` paths resolved relative to the rule directory; absolute paths and `..` escapes rejected.
- **Skills**: the only skill published to [skills.sh](https://skills.sh) is `skills/dq/`. The `npx skills add mazuninky/dq` CLI also scans `<repo>/.claude/skills/` as one of its priority paths (see [vercel-labs/skills/src/skills.ts](https://github.com/vercel-labs/skills/blob/main/src/skills.ts) — `parseSkillMd`), so any SKILL.md committed under `.claude/skills/` MUST carry `metadata.internal: true` in its frontmatter, or it will surface to end users on install. Claude Code itself ignores the flag and continues to load these skills for contributors.

## How to extend

- **New format**: implement the `Format` trait in `dq-core/src/formats/<name>.rs`, register in the detection table, add round-trip tests under `crates/dq-core/tests/`.
- **New `@std` rule**: drop a `*.yml` under `crates/dq-lint/rules/std/<namespace>/`, add a `*.test.yml` fixture next to it. `cargo test -p dq-lint` runs the fixture suite.
- **New subcommand**: add the clap struct in `dq-cli/src/cli.rs`, handler in `dq-cli/src/handlers/`, exit-code mapping in `dq-cli/src/exit.rs`.
- **Anything user-visible or cross-crate**: open an OpenSpec change under `openspec/changes/<name>/` with `spec.md` + `design.md` + `tasks.md` before writing code; archive when shipped.

## License

MIT — see [LICENSE](LICENSE).
