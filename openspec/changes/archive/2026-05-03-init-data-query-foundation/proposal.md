## Why

Сейчас в репозитории нет кода — только [dq-plan.md](../../../dq-plan.md) и пустой README. Чтобы дальше работать по roadmap'у, нужна твёрдая первая итерация: cargo workspace со всеми крейтами и работающие read-команды для YAML/JSON/TOML.

`/rust-cli` skill задаёт обязательный baseline стиля (см. [docs/archive/plan-validation-rust-cli-2026-05-03.md](../../../docs/archive/plan-validation-rust-cli-2026-05-03.md)) — ряд пунктов в плане не зафиксирован: `camino::Utf8PathBuf`, `tracing`, SIGPIPE, `Reporter`-DI, exit codes как константы, `--verbose`/`-v`, release profile, и переход с `miette` как primary errors на `thiserror`+`anyhow`. Foundation-change исправляет план и одновременно реализует M1 в строгом соответствии с конвенциями.

Цель — после этого change'а у проекта есть бинарь `dq`, который читает YAML/JSON/TOML, отвечает на основные read-вопросы агента (`paths`, `get`, `select`, `convert`, `validate`) и выдаёт structured errors с line/col/caret. Все последующие milestone'ы (M2 round-trip writes, M3 bulk, и т.д.) пристыковываются как отдельные OpenSpec change'и.

## What Changes

- **Cargo workspace** с пятью крейтами: `dq-core`, `dq-transform`, `dq-exec`, `dq-lint`, `dq-cli`. В M1 наполняются только `dq-core` и `dq-cli`; остальные — пустые скелеты с placeholder lib.rs (для устойчивого workspace-resolver и чтобы в M2+ не делать структурную перестройку).
- **`dq-core`**: `Document` enum (Null/Bool/Int/BigInt/Float/String/Array/Map с per-node metadata: comments, quote-style, position), `trait Format`, парсеры YAML/JSON/TOML на чтение через стандартные serde-крейты (`serde_yml` / `serde_json` / `toml`), `Pointer` (JSON Pointer RFC 6901 — parser + navigate), `Error` enum через `thiserror` с `kind`, `path`, `line`, `col`, `span`, `suggestions`, `pub type Result<T>` alias.
- **`dq-cli`**: бинарь с тонким `main.rs` (SIGPIPE→SIG_DFL на Unix, parse args, init tracing, dispatch). Reporter trait с реализациями `Console`/`Json`/`Toon` (через `toon-format` крейт), output factory в `main.rs`, handlers возвращают `serde_json::Value`. Команды M1: `get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select` (JSONPath), `convert` (output-only, без `-i`), `validate`. Глобальные флаги: `-F/--format`, `-v/--verbose` (count, conflicts_with quiet), `-q/--quiet`, `--no-color`, `--no-pager`, `--jq`/`--template` отложены до M7. Exit codes — `pub mod exit_code` с named constants и `exit_code_for_error` mapper.
- **Project-wide rules**: `.claude/rules/rust-delegation.md` (адаптировано из mazuninky/atl) — orchestrator делегирует Rust-правки в `rust-cli-writer`/`rust-cli-test-writer`. `.claude/settings.json` блокирует `--no-verify` / `LEFTHOOK=0`.
- **Build / DX**: `rust-toolchain.toml` (pin 1.94+ с auto-install rustup), `[profile.release]` lto=true/codegen-units=1/strip=true, `lefthook.yml` (fmt-check, clippy `-D warnings`, `cargo nextest run --all-features`), `cargo-deny` для license/security audit, `clippy.toml` baseline.
- **Plan delta**: `dq-plan.md` правится в части Tech stack, Архитектура (Layer 1, CLI), Глобальных флагов и Поддерживаемых форматов — детали в [docs/archive/plan-validation-rust-cli-2026-05-03.md](../../../docs/archive/plan-validation-rust-cli-2026-05-03.md).
- **Что НЕ меняется (anti-scope для M1)**: ноль write-команд (`set`/`del`/`patch`/`merge` — M2+), ноль jq (M7), ноль линтеров (M8), ноль multi-file glob (M3), ноль `-i/--in-place`, ноль форматов помимо YAML/JSON/TOON-on-write/JSON/TOML.

## Capabilities

### New Capabilities

- `data-query-read`: read-only data query commands (`get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `convert`, `validate`) с predictable JSON-вывода и path-синтаксисом RFC 6901.
- `format-support`: `trait Format` и парсеры формата → `Document` для YAML / JSON / TOML на чтение, плюс writer'ы для конверсии формата (без round-trip — это M2). Newline-delimited JSON (JSONL) read+write. TOON write-only (через `toon-format`).
- `path-syntax`: типизированный JSON Pointer (RFC 6901) для всех read/write команд и JSONPath (RFC 9535) для команды `select`. jq-выражения в `query` отложены до M7.
- `cli-shell`: бинарь `dq` — main.rs контракт (SIGPIPE, парсинг clap, init tracing, dispatch), глобальные флаги, exit-codes как named constants, output Reporter trait (Console/Json/Toon) с DI, non-interactive контракт, completions/man-pages stubs.

### Modified Capabilities

(none — это первый change в проекте, существующих специй нет)

## Impact

- **Code**: создаётся `Cargo.toml` (workspace) + 5 manifest'ов под `crates/*`, `crates/dq-core/src/{lib,document,format,parsers/{yaml,json,toml},pointer,error}.rs`, `crates/dq-cli/src/{main,bin,commands/{get,exists,keys,values,len,type_cmd,paths,select,convert,validate},output/{mod,console,json,toon},cli/{mod,args},error,exit_code}.rs`, и пустые `crates/{dq-transform,dq-exec,dq-lint}/src/lib.rs`.
- **Dependencies (новые)**: `clap` 4 (derive,wrap_help), `camino`, `thiserror`, `anyhow`, `tracing`, `tracing-subscriber` (env-filter, fmt), `serde`, `serde_json`, `serde_yml`, `toml`, `jsonpath-rust`, `toon-format` 0.4. Dev-deps: `assert_cmd`, `predicates`, `tempfile`, `insta`, `pretty_assertions`.
- **Project meta**: `.claude/{rules/rust-delegation.md,settings.json}`, `lefthook.yml`, `rust-toolchain.toml`, `clippy.toml`, `deny.toml`.
- **Documentation**: README обновлён до краткого "what is dq + status: M1 alpha", `dq-plan.md` правится по [docs/archive/plan-validation-rust-cli-2026-05-03.md](../../../docs/archive/plan-validation-rust-cli-2026-05-03.md).
- **CI** (вне scope этого change'а, отмечается как dependency): GitHub Actions с matrix builds — это M6, в M1 только локальный lefthook.
- **Backward compatibility**: ничего ломать нечего, проект новый.
