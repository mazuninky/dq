## Context

Это первая итерация проекта `dq` — кода в репозитории нет, есть только [dq-plan.md](../../../dq-plan.md). Foundation-change должен:

1. развернуть весь cargo workspace (5 крейтов) сразу, чтобы M2/M3/M7/M8 не требовали структурной перестройки;
2. наполнить `dq-core` и `dq-cli` ровно настолько, чтобы M1 read-команды работали на репрезентативных файлах из k8s/helm/github-actions репо;
3. зафиксировать конвенции `/rust-cli` в коде с самого начала — чтобы агенты `rust-cli-writer`/`rust-cli-test-writer`, к которым мы будем делегировать M2+, не получали "технический долг по стилю" в наследство.

Стейкхолдеры: автор (один разработчик), а также AI-агент, который дальше будет писать код. Спецификация и план — единственный контракт между ними.

Constraints:
- Никаких `*.rs` правок в этом change'е своими руками — всё через `rust-cli-writer` / `rust-cli-test-writer` (см. [.claude/rules/rust-delegation.md](../../../.claude/rules/rust-delegation.md)).
- M1 read-only: ноль write-операций, ноль jq, ноль линтеров, ноль multi-file glob (см. [dq-plan.md M1](../../../dq-plan.md#m1--read-only-foundation)).
- В M1 не закладываемся на `saphyr` (event-API сложен, оставлен на M2-spike). Берём `serde_yml` / `serde_json` / `toml`. Это означает, что round-trip в M1 невозможен принципиально — что согласовано с планом.

## Goals

- Cargo workspace из пяти крейтов с минимальным `Cargo.toml`-каркасом для каждого; компилируется чистым `cargo build`.
- Все M1 read-команды работают на трёх форматах (YAML, JSON, TOML) на ≥50 fixture-файлах из открытых k8s/helm/github-actions репо.
- Structured errors: парсер-ошибка содержит `line`, `col`, `span`, snippet; path-ошибка содержит `matched_prefix` + `did_you_mean`.
- Reporter-DI работает: handlers тестируются с `Vec<u8>` без `assert_cmd`.
- SIGPIPE и tracing настроены так, что `dq paths big.yaml | head` корректен и тесты не падают на повторном `try_init`.
- `dq generate-docs` собирает man pages и completions (без CI-интеграции).

## Non-Goals

- Распарсенный YAML с сохранением комментариев / anchor'ов / quote-style — это M2.
- Любая запись в файлы (`-i`, `--diff`, `--backup`) — flag'и парсятся, но не работают.
- jq-выражения, в т.ч. `--jq` — M7. `query` команда отсутствует.
- Любые форматы помимо YAML / JSON / TOML / JSONL (read+write) и TOON (write-only) — остальные форматы M5+.
- CI / GitHub Actions / cross-build / install.sh — это M6.
- Линтеры / `dq lint` / `dq check` / `@std/*` rulesets — это M8.
- `dq self update` — это M6.

## Decisions

### D1. Workspace создаётся целиком в M1, наполняется поэтапно

**Решение:** манифесты для всех пяти крейтов (`dq-core`, `dq-transform`, `dq-exec`, `dq-lint`, `dq-cli`) появляются сразу, у трёх "будущих" `lib.rs` содержит только `pub fn _placeholder() {}` или пустой `pub mod inner;`.

**Альтернатива:** создать только `dq-core` + `dq-cli`, а остальные добавлять по мере роадмапа.

**Почему так:** добавление новых workspace-членов позже триггерит перерезолв `Cargo.lock` и потенциально пересборку. Создав скелеты сразу, мы ловим все breaking-изменения в зависимостях при каждом change'е, а не позже разом. Стоимость пустых крейтов — пять манифестов.

### D2. M1 YAML парсер — `serde_yml`, не `saphyr`

**Решение:** YAML в M1 читается через `serde_yml` (или `serde_yaml_ng` как fallback, если `serde_yml` депрекатили), JSON — через `serde_json`, TOML — через `toml`. Эти крейты дают `serde::Deserialize`, который мы конвертируем в `Document` через `From`.

**Альтернатива:** сразу взять `saphyr` event-API и научиться сохранять метаданные форматирования. Это требование M2.

**Почему так:** план явно отделяет M1 (read) от M2 (round-trip). Использовать event-API в M1 — потратить две недели спайка на рискованную задачу до того, как остальной каркас работает. Лучше сначала довести каркас до DoD на простых парсерах, а в начале M2 сделать запланированный saphyr-spike с ясной выгрузкой "что нужно от формата".

**Цена:** в M2 парсер YAML придётся переписать с нуля под event-API. Чтобы это не было больно, в `dq-core` сразу выделяем `parsers::yaml` как отдельный модуль за `trait Format` — конкретный backend сменим, не трогая остальной код.

### D3. JSON Pointer — собственная реализация в `dq-core`

**Решение:** `Pointer` — типизированная структура с `parse(&str) -> Result<Pointer>`, `resolve(&self, &Document) -> Result<&Value>`, `as_str(&self) -> String`. Никаких внешних крейтов.

**Альтернатива:** `jsonptr` крейт.

**Почему так:** RFC 6901 — простой формат (нам нужны 50 строк кода), а наша `resolve` должна возвращать богатую error-структуру (`matched_prefix`, `did_you_mean`) — внешний крейт даст `Option<&Value>` и спрячет контекст. Свой код контролируем полностью.

### D4. JSONPath для `select` — `jsonpath-rust`

**Решение:** depend on `jsonpath-rust = "0.7"`. Конвертируем `Document` → `serde_json::Value` для запроса (преобразование одностороннее, потому что для `select` нам не нужно сохранение метаданных), отдаём результат как JSON-array.

**Альтернатива:** написать свой JSONPath. Очень дорого; RFC 9535 — большой стандарт.

**Цена:** конверсия `Document` ↔ `serde_json::Value` теряет position-метаданные. В M1 это не нужно (select read-only, position от каждого матча не требуется). Вернёмся к этому в M3 для `diff` и в M8 для линтеров.

### D5. Errors — `thiserror` per crate + `anyhow` в handlers, `miette` отложен

**Решение:** в каждом крейте свой `pub enum Error` через `thiserror`, с named-fields для multi-context вариантов (`Path { pointer, matched_prefix, did_you_mean, kind }`, `Parse { file, line, col, span, snippet, message }`). `dq-cli` handlers возвращают `anyhow::Result<()>`. Маппинг exit-кодов через `downcast_ref`.

**Альтернатива (что было в плане):** `miette` как primary error type.

**Почему сменили:** см. [docs/archive/plan-validation-rust-cli-2026-05-03.md](../../../docs/archive/plan-validation-rust-cli-2026-05-03.md), `Конфликт 1`. `miette` — рендерер диагностик, не error-type. Использовать его как primary означает протащить `miette::Diagnostic` через каждый internal API библиотек, что мешает M8 экосистеме (внешние пользователи `dq-core` не должны быть обязаны зависеть от `miette`). Caret/span-рендеринг можно собрать руками или подцепить `miette` поверх в M2/M3, когда пойдут пользовательские парсер-ошибки сложного вида.

### D6. TOON output — крейт `toon-format`, не свой энкодер

**Решение:** `dq-cli` зависит от `toon-format = "0.4"`. `ToonReporter::report` делегирует в `toon_format::encode`.

**Альтернатива (что было в плане):** свой энкодер, потому что TOON-нотация компактна.

**Почему сменили:** см. [docs/archive/plan-validation-rust-cli-2026-05-03.md](../../../docs/archive/plan-validation-rust-cli-2026-05-03.md), `Конфликт 2`. Нотация компактна, но supporting любой эволюции спецификации (а она ещё не финализирована) вручную — пустая трата времени. Крейт уже учитывает skill-конвенцию.

### D7. Reporter trait, factory только в `main.rs`

**Решение:** `trait Reporter { fn report(&self, value: &serde_json::Value, w: &mut dyn Write) -> anyhow::Result<()>; }`. Реализации `ConsoleReporter`, `JsonReporter`, `ToonReporter` в `crates/dq-cli/src/output/`. Factory `reporter_for_format(format: OutputFormat, use_color: bool) -> Box<dyn Reporter>` — в `main.rs`. Handlers получают `&dyn Reporter` параметром.

**Альтернатива:** factory в `output::mod`. Это была бы небольшая упрощение API, но заставит handlers строить Reporter-ы (через `OutputFormat` параметр), что нарушает принцип "wiring живёт в `main.rs`, handlers его не видят".

### D8. Cli args — split по семьям команд, глобалы только на `Cli`

**Решение:** `crates/dq-cli/src/cli/{mod,args.rs,commands.rs}`. `args.rs` объявляет `Cli` (top-level + globals) и enum `Command { Get(GetArgs), Exists(ExistsArgs), ... }`. Каждый `*Args` — отдельный `#[derive(Args)]` struct в `cli/args/<cmd>.rs`. Глобалы (`-F`, `-v`, `-q`, `--no-color`, `--no-pager`, `--doc`) сидят на `Cli` с `global = true`.

### D9. `Document` в M1 — recursive enum с минимальной метадатой

**Решение:** в M1 `Document::Value` = `enum Value { Null, Bool, Int(i64), BigInt(String), Float(f64), BigFloat(String), String(String), Array(Vec<Value>), Map(IndexMap<String, Value>) }`. Используем `indexmap::IndexMap` для сохранения порядка ключей. Метадата (комментарии, quote-style, position) пока не хранится — добавляется в M2.

**Почему `IndexMap`:** `BTreeMap` сортирует ключи (плохо для round-trip и плохо для агента, который ожидает source order); `HashMap` теряет порядок; `IndexMap` — стандартное решение.

### D10. Read-only flag enforcement через clap

**Решение:** `-i/--in-place`, `--diff`, `--backup` объявлены на `Cli` (global=true), но в M1 любой их парсинг приводит к ранней проверке в `main.rs` после `Cli::parse_from`: если флаг указан, отдаём `Error::WriteUnavailable` с сообщением про M2.

**Почему так, а не убрать флаги:** в M2 флаги уже работают; если в M1 их нет в clap, тесты M1 не покрывают парсинг этих аргументов и при добавлении в M2 могут проявиться регрессии. Лучше пусть существуют изначально и блокируются вежливой ошибкой.

### D11. SIGPIPE — restore до `SIG_DFL` в `main.rs`

**Решение:** в `main.rs` на Unix-targets вызывается ioctl/`signal::sigaction` (или прямой `libc::signal(libc::SIGPIPE, libc::SIG_DFL)` через unsafe), чтобы pipe в `head` не вызывал panic. На Windows — no-op (нет SIGPIPE).

**Источник:** см. `/rust-cli` skill, секция `Key Principle 1` ("Thin main.rs"); это требование от skill, не моё изобретение.

### D12. Tracing init с `try_init`, не `init`

**Решение:** `tracing_subscriber::fmt().with_env_filter(env_filter).with_target(false).try_init()` в `main.rs`. Возвращаемый `Result` игнорируется — `Err` возможен только при двойной инициализации (что бывает в integration-тестах, где один процесс зовёт `run()` дважды).

### D13. Тестирование — пирамида unit / component / CLI integration

**Решение:** ~75% unit (handlers с `Vec<u8>` writer), ~20% component (через `dq-core` API напрямую с tempfile), ~5% CLI integration (`assert_cmd` + `predicates` для smoke и exit-кодов). Snapshot-тесты `insta` для structured error rendering. Golden-file runner для `tests/fixtures/` — каждый файл прогоняется через `paths` и сравнивается со snapshot.

**Стратегия из skill `/rust-cli`:** `references/cli-testing.md` — следуем ей, не пересогласовываем.

## Risks / Trade-offs

- **YAML без round-trip → весь M1 будет переписан в M2** → minor, но ожидаем; модуль `parsers::yaml` изолирован за `trait Format`, остальной код M1 переживёт смену backend'а. Риск: парсер выдаст значения в типах, несовместимых с M2-моделью (особенно числа). Mitigation: в M1 сразу определить `Document::BigInt`/`Document::BigFloat` как `String`-обёртки (D9) — M2 их сохранит.
- **`jsonpath-rust` потенциально неполный по RFC 9535** → small. Mitigation: snapshot-тесты на 20 типичных JSONPath-запросов из k8s/helm; если что-то не работает, пишем явный issue в апстрим и временно добавляем custom path-walker для конкретного синтаксиса.
- **`toon-format` 0.4 — pre-1.0, может ломать API** → small. Mitigation: pin minor version (`toon-format = "0.4"`, не `"^0.4"`); CI делает `cargo update` тест раз в неделю.
- **`indexmap` иногда конфликтует с serde-derive внутри `serde_yml`** → small. Mitigation: явно зависим от свежей версии `indexmap` в workspace `[workspace.dependencies]`.
- **`miette` исключён из M1, но останется ли он удобным в M2/M3** → minor. Mitigation: при M2/M3, если caret-рендеринг становится сложнее простого "line/col + caret + snippet", дёрнуть `miette` как dependency `dq-cli`-only (не `dq-core`), без переписывания error-типов.
- **Делегирование Rust-правок через subagents может замедлить foundation-change** → minor. Mitigation: в tasks.md сразу даны самодостаточные промпты для каждого блока, чтобы оркестратор отправлял большие куски сразу.
- **`Document` модель в M1 окажется неудачной для M2 round-trip** → medium-low. Mitigation: M1 spike (D2) определит API публичный — в начале M1 пишется код, который читает 5 fixture-файлов и крутит над ними M1-команды; если что-то "пахнет" неудачным API, фиксим до того, как реализация расползлась.

## Migration Plan

Не применимо — green-field проект, ничего не мигрируем.

Roll-out: change архивируется после того, как
1. cargo workspace компилируется (`cargo check --all-targets`),
2. вся пирамида тестов зелёная (`cargo nextest run --all-features`),
3. clippy зелёный (`cargo clippy --all-targets -- -D warnings`),
4. ручной smoke на 50+ fixture-файлах (см. [tasks.md](tasks.md) §6.3),
5. `dq-plan.md` обновлён по validation-документу.

## Open Questions

1. **Имя core-крейта на crates.io:** `dquery`, `dq-core` или другое (см. [dq-plan.md "Открытые вопросы"](../../../dq-plan.md#открытые-вопросы)). Не блокирует M1 — крейт пока локальный workspace-член, имя на crates.io решается перед M6 (distribution).
2. **`indexmap` vs другой order-preserving map:** возможно, `linked_hash_map` или собственная обёртка над `Vec<(String, Value)>`. По умолчанию — `indexmap`. Решение откладываем до spike: если попадётся issue с `serde_yml + indexmap`, сменим.
3. **JSONPath на `jsonpath-rust` vs альтернатива (`serde_json_path`):** перепроверить активность maintenance в начале реализации; по умолчанию — `jsonpath-rust` (старше, стабильнее по API).
4. **Поведение `validate` для пустого файла:** ok-как-empty-document или error? По умолчанию — ok (пустой YAML/JSON → `Document::Null`/empty object). Может быть пересмотрено по жалобам пользователей.
5. **Поведение `paths` для очень больших документов (>10MB JSON):** в M1 эмитим всё в память; в M3 (multi-file) понадобится streaming. Открытый вопрос — стоит ли уже в M1 заложить streaming-API в `Pointer`. По умолчанию — нет, простой обход.
