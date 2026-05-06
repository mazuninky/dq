# Plan validation against `/rust-cli` conventions

Дата: 2026-05-03. Источник конвенций: skill `rust-cli` (`~/.claude/skills/rust-cli/SKILL.md`) + `references/cli-code-style.md` + `references/cli-testing.md`. Проверяемый артефакт: [dq-plan.md](../../dq-plan.md), draft 2 (май 2026).

Цель документа — зафиксировать, где план согласован с конвенциями `rust-cli`, какие пункты надо доуточнить и какие — переписать. Конфликты решаются в пользу `/rust-cli` (это базовый стиль для всех Rust-CLI в личной экосистеме).

## Сводка

| Класс | Кол-во | Статус |
|---|---|---|
| ✅ Совпадает | 14 | без изменений |
| ⚠ Не указано в плане, надо добавить | 11 | дописать в Tech stack / Архитектура / M1 |
| ❌ Конфликт, надо переписать | 2 | принять `/rust-cli` |

## ✅ Что уже совпадает

| План | `/rust-cli` |
|---|---|
| `clap` v4 (derive) | ✓ derive + `global = true` для глобальных флагов |
| Edition 2024 | ✓ |
| MSRV 1.94+ | ✓ (atl на 1.95.0; можно поднять) |
| Cargo workspace | ✓ |
| `lefthook` для git hooks | ✓ |
| `rustfmt --check` + `clippy -D warnings` в CI | ✓ |
| `cargo-nextest` для тестов | ✓ нейтрально |
| `insta` для snapshot-тестов | ✓ |
| `proptest` для round-trip | ✓ |
| `criterion` для бенчмарков | ✓ |
| `globset` для glob | ✓ |
| `rayon` для параллелизма | ✓ |
| `clap_complete` / `clap_mangen` | ✓ |
| `tempfile` (для atomic writes; в тестах тоже) | ✓ |

## ⚠ Что надо добавить в план

Перечислены пункты, которых в плане нет, но которые `/rust-cli` требует как обязательные. Большинство — для `dq-cli`, часть — общая по workspace.

### 1. `camino::Utf8PathBuf` вместо `std::path::PathBuf`

Конвенция: `camino::Utf8PathBuf` / `Utf8Path` везде, чтобы избавиться от `.to_str().unwrap()`. План не упоминает camino. **Действие:** добавить `camino` в зависимости core/cli и зафиксировать в принципах "Дизайн-принципы".

### 2. `tracing` + `tracing-subscriber` для логирования

Конвенция: `tracing` + `tracing-subscriber` с `EnvFilter`, mapping verbosity 0=WARN/1=INFO/2=DEBUG/3+=TRACE, `with_target(false)`, респект `RUST_LOG` и `NO_COLOR`. **Никогда** `println!`/`eprintln!` для диагностики и **никогда** `log` крейт. План не описывает logging-стек. **Действие:** добавить в Tech stack.

### 3. SIGPIPE handler в `main.rs`

Конвенция: на Unix восстановить SIGPIPE до `SIG_DFL` в `main`, чтобы `dq paths big.json | head` завершался корректно, а не паниковал на broken pipe. Это критично для агента (он любит pipe в head/jq). План не упоминает. **Действие:** включить как требование в архитектуру `dq-cli` и в M1 DoD.

### 4. `Reporter` trait + writer-injection

Конвенция: при поддержке нескольких output-форматов — `trait Reporter { fn report(&self, result: &RunResult, w: &mut dyn Write) -> Result<()>; }`. Команды получают `&dyn Reporter` параметром, **никогда** не создают свой. Локaлизация stdout — один раз `io::stdout().lock()` в `main.rs`. План говорит "Reporter форматирует диагностики" в `dq-exec`, но не обобщает паттерн на data-команды. **Действие:** распространить `Reporter` на data-команды в `dq-cli` (минимум `Console` / `Json` / `Toon`), зафиксировать DI.

### 5. Exit codes как named constants

Конвенция: `pub mod exit_code { pub const SUCCESS: i32 = 0; pub const GENERIC: i32 = 1; pub const NOT_FOUND: i32 = 2; ... }`, mapping через `anyhow::Error::downcast_ref` в `exit_code_for_error`. План говорит "exit 0/1" без структуры. **Действие:** определить exit-коды модулем уже в M1 (минимум: 0 OK, 1 generic, 2 not-found/path, 3 parse, 4 validate fail).

### 6. Глобальный `--verbose` / `-v` (count action)

Конвенция: `#[arg(short, long, action = clap::ArgAction::Count, global = true)]` для `-v`/`-vv`/`-vvv` + `--quiet` с `conflicts_with = "verbose"`. План перечисляет `--no-color`, `--no-pager`, но не `-v`/`-q`. **Действие:** добавить в "Глобальные флаги".

### 7. Release profile с LTO

Конвенция: `[profile.release] lto = true; codegen-units = 1; strip = true`. План не задаёт. **Действие:** добавить в workspace `Cargo.toml`.

### 8. `pub type Result<T> = std::result::Result<T, Error>` per crate

Конвенция: каждый error-модуль определяет alias для крейт-внутреннего использования. Command handlers используют `anyhow::Result`. План не описывает. **Действие:** правило для `dq-core`, `dq-transform`, `dq-exec`, `dq-lint`.

### 9. Non-interactive mode как явный принцип

Конвенция: ноль prompt'ов, ноль spinner'ов, output идентичен под `| cat` и в CI. План это подразумевает (бинарь работает в CI, агенты), но не формулирует явно. **Действие:** добавить в "Дизайн-принципы" пунктом или объединить с Agent-first.

### 10. DI для command handlers (testability)

Конвенция: handler'ы получают зависимости (`&dyn Reporter`, `&mut dyn Write`, `&dyn ConfigSource`, `&Registry`) как параметры; никогда не создают сами. Для 1–3 — explicit params, для большего — `CommandContext`. План не описывает testability-стратегию `dq-cli`. **Действие:** зафиксировать в архитектуре.

### 11. `tracing_subscriber::fmt().try_init()` + Registry test-constructor

Конвенция: `try_init()` (не `.init()`), чтобы повторные вызовы `run()` в тестах не падали. Registry должен иметь `#[cfg(any(test, feature = "test-util"))] pub fn with_items(...)`. Запрет `pub(crate)` / `#[cfg(test)] pub fn __test_*` escape-hatches и `std::env::set_var("NO_COLOR", ...)` в тестах (`use_color: bool` тредится параметром). **Действие:** добавить в раздел тестирования.

## ❌ Что надо переписать

### Конфликт 1: error-стек

**В плане** (строка 66, 516): "`Error` — единая ошибка с line/col, span, kind (parse/path/type/io), suggestions" + dependency `miette` или собственный wrapper.

**`/rust-cli` требует:**
- domain errors через `thiserror` (один enum на модуль, named-fields для multi-context, `#[from]` для simple wrappers, `#[source]` для wrapped без `#[from]`);
- command handlers возвращают `anyhow::Result<()>` для удобного `?`;
- exit-коды из `anyhow::Error` через `downcast_ref` на domain `Error`.

`miette` не запрещён, но он — **рендерер диагностик**, а не основной error type. Использовать `miette` стоит для красивого pretty-print парсер-ошибок (caret, span, snippet) — но через newtype, который оборачивает `thiserror`-enum в `miette::Diagnostic`. Внутри библиотек ошибки строго `thiserror`.

**Решение:**
- В `dq-core` и трансформ-крейтах — `thiserror` enums с `kind`, `path`, `line`, `col`, `span`, `suggestions`.
- В `dq-cli` handlers — `anyhow::Result`.
- Опциональный `miette`-renderer для console-вывода парсер-ошибок и lint-диагностик. Это не conflict с планом, а уточнение: план говорит "miette **или** wrapper", и `/rust-cli` отвечает: "wrapper на thiserror — да, miette — поверх него, для рендера".

### Конфликт 2: TOON encoder

**В плане** (строки 110, 547): "TOON | — | ✓ | n/a (write-only, для LLM context) | свой энкодер" + явно в anti-scope не упоминается.

**`/rust-cli` требует:** `toon-format = "0.4"` крейт.

**Решение:** отказаться от собственного энкодера, добавить `toon-format` в зависимости `dq-cli`. Свой энкодер — стоимость поддержки без выгоды; и при обновлениях TOON-нотации мы будем отставать. Это меняет одну строку в Tech stack и одну в таблице форматов.

## Действия по пунктам — кратко

| # | Где править | Тип |
|---|---|---|
| 1 | dq-plan.md `Tech stack` + `Дизайн-принципы` | add |
| 2 | dq-plan.md `Tech stack` | add |
| 3 | dq-plan.md `Архитектура → CLI` + `M1 DoD` | add |
| 4 | dq-plan.md `Архитектура → CLI` | add |
| 5 | dq-plan.md `Архитектура → CLI` + `M1` | add |
| 6 | dq-plan.md `Глобальные флаги` | add |
| 7 | dq-plan.md `Tech stack → Build` | add |
| 8 | dq-plan.md `Архитектура → Layer 1/2/3/4` | add |
| 9 | dq-plan.md `Дизайн-принципы` | add |
| 10 | dq-plan.md `Архитектура → CLI` | add |
| 11 | dq-plan.md новая секция `Тестирование` | add |
| K1 | dq-plan.md `Tech stack` (miette → thiserror+anyhow primary) | rewrite |
| K2 | dq-plan.md `Поддерживаемые форматы` (TOON: свой → toon-format) | rewrite |

Все 13 пунктов закодированы в OpenSpec change `init-data-query-foundation` (раздел tasks). Архивировать данный документ можно после того, как dq-plan.md будет обновлён.
