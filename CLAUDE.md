# dq — Claude Code project context

`dq` (data query) is a single-binary Rust CLI for structured data files (YAML/JSON/TOML/HCL/INI/.env/CSV/Dockerfile/Markdown/XML) and a lint+autofix engine on top. The user-facing surface lives in [README.md](README.md). This file is for Claude — repo orientation, what's experimental, conventions, and explicit anti-scope.

## Repo layout

```
crates/
  dq-cli/        clap CLI, subcommand handlers, output renderers
  dq-core/       parsers, IR, textual-edit (saphyr) writers, formats
  dq-exec/       lint runtime: rule loader, evaluator, JSON Schema + composite checks, fixer, fixture runner
  dq-lint/       the embedded @std rule library (rule YAML, fixtures, schema sidecars)
  dq-transform/  embedded jq engine (jaq) for query, set --jq, rules
  dq-plugin/     WASM plugin runtime (wasmtime, WIT contract, behind feature)
examples/        plugin-rust reference plugin
spikes/          throwaway investigations (saphyr/, etc.)
docs/            contributor docs: architecture.md, reference/ (current behaviour), plans/ (in flight),
                 roadmap.md, decisions.md (dated log), plugin-abi.md
docs/archive/    historical docs, incl. the original plan (dq-plan-2026-05.md, Russian)
skills/dq/       the agent skill, embedded in the binary (`dq skill install`); evals/ is not
```

## Stable surface vs experimental

Everything described in [README.md](README.md) is the stable surface — assume it works and write code against it. Two pockets are explicitly experimental and behind opt-in:

- **WASM plugin runtime** — `dq-plugin` crate, gated by `--features plugins`. WIT contract is `dq:plugin@0.1.0`; v0.1.0 is a preview, breaking changes possible before v1.0.0. Sandbox runs without WASI: no network, no filesystem, no processes. Fuel ~1 s CPU, memory cap 64 MiB. Plugin discovery is non-recursive lexically-sorted `*.wasm` under `--plugins <DIR>`. Without the feature flag the flag is parsed but loading any `*.wasm` exits 6 (`InvalidInput`). Contract: [docs/plugin-abi.md](docs/plugin-abi.md) together with [crates/dq-plugin/wit/dq-plugin.wit](crates/dq-plugin/wit/dq-plugin.wit).

For in-flight work see [docs/plans/](docs/plans/) and [docs/roadmap.md](docs/roadmap.md). Don't rely on this CLAUDE.md to track what is active; it'll go stale.

## Anti-scope (deferred / explicitly out)

These are *decisions*, not omissions — don't add them without a plan in `docs/plans/` that revisits the decision:

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

CI mirrors this exactly ([.github/workflows/ci.yml](.github/workflows/ci.yml)). MSRV is **1.100**, pinned in [rust-toolchain.toml](rust-toolchain.toml).

Plugin runtime tests are gated:

```sh
cargo test -p dq-plugin --features plugins
```

End-to-end smoke for the example plugin:

```sh
cd examples/plugin-rust && cargo component build --release --target wasm32-unknown-unknown
cd ../.. && cargo run --features plugins -- lint \
    --plugins examples/plugin-rust/target/wasm32-unknown-unknown/release config.yaml
```

## Conventions

- **Errors**: `thiserror` for typed enums in libraries, `anyhow` only in `dq-cli` for the topmost handler. Each error variant maps to a fixed exit code (see README's exit-codes table).
- **Tracing**: `tracing` + `tracing_subscriber` everywhere. `tracing::warn!` for rule-author bugs (idempotency violation, mixed XML content). Don't `eprintln!` from libraries.
- **Write semantics**: textual-edit (`set` / `del` with a pointer, `patch`, `merge`, rule `fix.ops` on YAML / JSON / TOML) preserves comments and ordering; jq-driven (`set --jq`, `fmt`, `convert`, rule `fix.jq`) re-emits via the format's writer and drops comments (plain `fmt` leaves a TOML file as it is). Point edits on HCL / XML / INI / dotenv / CSV / TSV / JSON Lines have no textual edit: they change the value tree and rewrite the whole file through the writer, as `set --jq` does. Markdown takes no edits. The trade-off is intentional — surface it to users when it matters.
- **Atomic writes**: every `-i` write goes through tempfile + rename. Bulk runs report `Modified / Skipped / Failed`; bulk uses exit 7 if any file failed.
- **Format detection**: extension + filename heuristics in `crates/dq-core/src/format.rs` (`detect`, then the exact names in `FILENAME_FALLBACK` and the prefixes in `PREFIX_FALLBACK`, e.g. `.env.*`); formats are registered in `crates/dq-core/src/parsers/mod.rs` (`registry()`). `--input-format <format>` overrides (and is how stdin `-` names its format); `-F` is the output format only and never picks the parser. Unknown extensions are an error, not a guess.
- **`@std` rule namespacing**: `@std/<namespace>/<rule-id>` — namespace is the directory under `crates/dq-lint/rules/`. User rules under `./.dq/rules/` are bound automatically without a namespace prefix.
- **Composite rules**: `extract:` returns `[{value, format, anchor}]`; each item is reparsed in a different format and run through a `nested:` rule. Recursion bounded at `MAX_EXTRACT_DEPTH = 4`. Inner-format parse failures emit `<outer>.parse-failed`. Inline position spans live in `Provenance::Original.inline_offset`; the public lookup is `Ir::inline_offset_for(&pointer)`.
- **Schema rules**: `$ref` restricted to internal references — HTTP/file `$ref` rejected at compile time. `schema_file` paths resolved relative to the rule directory; absolute paths and `..` escapes rejected.
- **Skill**: `skills/dq/SKILL.md` and `references/` are embedded in the binary by `crates/dq-cli/src/skill/content.rs`; a new reference file is added there too (a test compares the list). Update the skill with the CLI surface it describes.

## How to extend

- **New format**: implement the `Format` trait in `crates/dq-core/src/parsers/<name>.rs`, register it in `parsers::registry()` (and in the filename table in `crates/dq-core/src/format.rs` if it has no telling extension), add round-trip tests under `crates/dq-core/tests/`.
- **New `@std` rule**: drop a `*.yml` under `crates/dq-lint/rules/<namespace>/`, add a `*.test.yml` fixture next to it. `cargo test -p dq-lint` runs the fixture suite.
- **New subcommand**: add the clap args in `crates/dq-cli/src/cli/args/<name>.rs` (registered in `cli/args.rs`), the handler in `crates/dq-cli/src/commands/`, exit-code mapping in `crates/dq-cli/src/exit_code.rs`.
- **Anything user-visible or cross-crate**: write `docs/plans/<name>.md` (goal, design, remaining work) before writing code. When it ships, add a dated entry to [docs/decisions.md](docs/decisions.md) and update the pages under [docs/reference/](docs/reference/).

## License

MIT — see [LICENSE](LICENSE).
