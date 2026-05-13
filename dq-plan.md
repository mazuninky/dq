# dq — план проекта

## Что это

`dq` (data query) — agent-friendly Rust CLI для работы со структурированными данными и платформа для написания линтеров поверх произвольных форматов (YAML, JSON, TOML, Markdown, и других).

Рабочее имя бинарника: `dq`. Имя core-библиотеки на crates.io: `dquery` или `dq-core` (потому что `dq` на crates.io уже занят пакетом dual-quaternions; финальный выбор — перед первым релизом 0.1).

Распространяется как single static binary: prebuilt-релизы для Linux/macOS/Windows, install-скрипт, self-update, completions, skill для Claude Code, calendar versioning (`YYYY.WW.patch`).

## Зачем

`dq` закрывает две задачи в одном бинаре:

1. **CLI для работы со структурированными данными в типичных DevOps-сценариях** (k8s, helm, github actions, конфиги) с гарантией round-trip (комментарии, порядок ключей, стиль кавычек сохраняются на in-place edit), structured errors с line/col/snippet и agent-friendly интерфейсом.
2. **Платформа для написания линтеров** — простой YAML-формат правил с jq-выражениями для check и fix, библиотека готовых ruleset'ов для популярных форматов (`@std/k8s`, `@std/github-actions`, `@std/dockerfile`, и других), расширяемость без Rust-кода.

Главная аудитория — разработчики, DevOps-инженеры, **и AI-агенты в CI/CD pipelines**. Третья категория — основной differentiator: инструмент строится с нуля под автоматизированное использование.

## Дизайн-принципы

**Agent-first.** Все ошибки — структурированные, с line/col, span, suggestions, did_you_mean. JSON-вывод ошибок в `-F json` — машиночитаемый, не stack trace. Команды атомарные: одна команда — одна операция, без скрытого состояния. Path syntax — JSON Pointer (RFC 6901), а не свой DSL: невозможно ошибиться с экранированием.

**Round-trip safety.** Любой read-modify-write цикл сохраняет: комментарии, blank lines, порядок ключей, стиль кавычек, anchor'ы и alias'ы (для YAML), inline-vs-block-вывод массивов, числовую точность (без потери на больших int64). Это разрабатывается **до** функциональности; функциональность без round-trip не релизится.

**Format-agnostic.** Одни и те же команды (`get`, `set`, `del`, и т.д.) работают для YAML, JSON, TOML — потому что внутренняя модель Document одна, а Format trait отвечает за парсинг и сериализацию. Это даёт пользователю предсказуемость; агенту — отсутствие надобности учить отдельный синтаксис под формат.

**Composable.** Бинарь читает stdin, пишет stdout. Любая команда работает в pipeline. CLI — тонкая обёртка над публичной библиотекой `dquery`. Это значит, что других Rust-разработчиков не надо учить shell-out — они подключают крейт.

**Honest scope.** В core нет своего query-DSL. Когда нужна экспрессивность (фильтры по условию, трансформации, арифметика) — встраивается `jaq` (Rust-нативный jq) опциональной фичей. Свой query-язык не изобретаем.

**Preserve forward compatibility.** Все output-форматы — стабильные стандарты (RFC 6901, RFC 6902, RFC 7396, JSON Schema 2020-12, JSON Patch). Никаких proprietary-форматов в нашем выходе.

## Архитектура

Workspace из четырёх крейтов плюс CLI:

```
dq-workspace/
├── crates/
│   ├── dq-core/         # data model, format traits, parsers, writers
│   ├── dq-transform/    # atomic ops (set/del/merge/patch) + jaq adapter
│   ├── dq-exec/         # rule runtime: AST, evaluator, reports
│   ├── dq-lint/         # standard rule library (k8s, markdown, npm)
│   └── dq-cli/          # binary, all command logic
├── rules/               # standard ruleset definitions (YAML files)
├── skills/dq/           # Claude Code skill content
├── scripts/             # install.sh, release helpers
└── docs/
```

**Layer 1 — Data (`dq-core`).** Чистая библиотека. Определяет:

- `trait Format` — методы `parse(bytes) -> Document`, `write(doc, &mut dyn Write) -> ()`, `extensions() -> &'static [&'static str]`, `name() -> &'static str`.
- `Document` — внутренняя модель. Гибрид: для key-value-форматов это recursive enum типа `Value::{Null, Bool, Int(i64), BigInt(String), Float(f64), BigFloat(String), String, Array, Map}` (с `IndexMap` для сохранения порядка ключей) с метаданными на каждом узле (комментарии, стиль кавычек, position — добавляются в M2). Для tree-форматов (markdown в M9) — отдельная `Tree` с типизированными узлами.
- `Pointer` — типизированный JSON Pointer (RFC 6901) с операциями navigate/set/delete и Levenshtein-2 `did_you_mean` для path-ошибок.
- `Error` — `thiserror`-based enum: варианты `Io`, `Parse { line, col, span, snippet, ... }`, `Path { pointer, matched_prefix, kind, did_you_mean }`, `UnsupportedFormat`, `Format`. Метод `kind_name()` стабилен для маппинга exit-кодов.
- `pub type Result<T> = std::result::Result<T, Error>` — alias per crate для крейт-внутреннего использования. Command handlers возвращают `anyhow::Result` (см. CLI ниже).

Round-trip обеспечивает event-based парсер на уровне формата: для YAML это `saphyr`, для TOML — `toml_edit`, для JSON — собственный wrapper с сохранением форматирования, для остальных форматов — best-effort или явное «no preserve».

**Layer 2 — Transform (`dq-transform`).** Атомарные операции и интеграция с jaq.

- `Op` — `Set(pointer, value)`, `Delete(pointer)`, `Merge(other)`, `Patch(rfc6902_ops)`. Каждая `Op::apply(&mut Document)` — pure function.
- `JqEngine` — обёртка над `jaq-core`, принимает Document, выражение, возвращает iterator результатов. Опциональная фича `embedded-jq` (default ON), отключается через `--no-default-features` для минимального бинаря.
- `TransformPipeline` — композиция `Op` и jq-выражений. Используется и в data-командах, и в lint-engine (когда правило хочет автофикс).

**Layer 3 — Exec (`dq-exec`).** Runtime для линтеров.

- `Rule` — структура правила, парсится из YAML.
- `RuleSet` — коллекция правил.
- `Evaluator` — берёт RuleSet и Document, возвращает `Vec<Diagnostic>` (с severity, message, position, optional fix).
- `Diagnostic` — структурированный отчёт с location, severity (error/warn/info), rule_id, message, suggested_fix.
- `Reporter` — форматирует диагностики в console/json/sarif (для GitHub Actions).
- `RuleLoader` — discovers rules: `@std/...` встроенные, `./.dq/rules/` локальные, `path/to/rules.yml` — explicit.

**Layer 4 — Lint (`dq-lint`).** Стандартная библиотека правил.

- `rules/k8s/*.yml` — Kubernetes manifests
- `rules/markdown/*.yml` — markdown style and content
- `rules/npm/*.yml` — package.json, tsconfig.json
- `rules/github-actions/*.yml` — workflow files
- (потом) `rules/terraform/*.yml`, `rules/openapi/*.yml`

Файлы embed'ятся в бинарь через `include_str!` и доступны как `@std/k8s`, `@std/markdown` и т.д.

**CLI (`dq-cli`).** Бинарь. Тонкая обёртка над всеми слоями. Использует clap v4 derive для парсинга. Команды разделены на семейства, но один бинарь.

Архитектурные требования к бинарю (приняты из skill `/rust-cli` как baseline, см. [docs/archive/plan-validation-rust-cli-2026-05-03.md](docs/archive/plan-validation-rust-cli-2026-05-03.md)):

- **Тонкий `main.rs`** (≤ 80 не-пустых, не-комментарных строк): SIGPIPE→`SIG_DFL` на Unix → clap parse → init `tracing-subscriber` через `try_init()` → lock stdout/stderr → dispatch в `commands::*::run(...)` → exit-code mapping. Бизнес-логика только в `lib.rs` модулях.
- **SIGPIPE handler** на Unix-targets обязателен (`libc::signal(libc::SIGPIPE, libc::SIG_DFL)`). Без этого `dq paths big.yaml | head` паникует на broken pipe — недопустимо для агентов в CI.
- **Reporter trait + DI.** `trait Reporter { fn report(&self, value, w: &mut dyn Write) -> Result<()>; }` с реализациями `Console`/`Json`/`Yaml`/`Toml`/`Jsonl`/`Toon`. Factory в `main.rs` (wiring layer); handlers получают `&dyn Reporter` параметром, никогда не строят свой и никогда не зовут `io::stdout()`. `use_color: bool` тредится параметром (никаких `std::env::set_var`).
- **Exit codes как named constants** в `pub mod exit_code` (SUCCESS=0, GENERIC=1, NOT_FOUND=2, PARSE_ERROR=3, VALIDATE_FAIL=4, IO_ERROR=5, INVALID_INPUT=6). `exit_code_for_error(&anyhow::Error) -> i32` через `downcast_ref` на `dq_core::Error`. Проект не использует магических чисел.
- **Errors на двух слоях.** Domain errors через `thiserror` per-crate (`dq_core::Error`, и так далее). Command handlers возвращают `anyhow::Result<()>` для удобного `?`. Опциональный `miette`-renderer поверх — в M2/M3 для caret/span/snippet рендера, не как primary error type.
- **Логирование строго через `tracing::*!`.** Никаких `println!`/`eprintln!` вне `main.rs` panic-paths и Reporter-реализаций (которые пишут пользовательский output). `EnvFilter` чтит `RUST_LOG`; verbosity mapping `-v`=INFO, `-vv`=DEBUG, `-vvv`=TRACE, `-q`=ERROR; default WARN. `try_init()` вместо `init()` чтобы integration-тесты не падали на повторной инициализации.
- **Non-interactive контракт.** Ноль prompts, ноль spinners. Output байт-в-байт идентичен под `| cat` и в interactive-терминале (кроме ANSI-цветов). Color resolution: `--no-color` > `NO_COLOR` env > `CLICOLOR_FORCE` env > `is_terminal(stdout)`.

## Поддерживаемые форматы

| Формат | Read | Write | Round-trip | Парсер |
|---|---|---|---|---|
| YAML | ✓ | ✓ | full (comments, anchors, key order, quote style) | `saphyr` (event-based) |
| JSON | ✓ | ✓ | full | свой (вокруг serde_json + position tracking) |
| TOML | ✓ | ✓ | full | `toml_edit` |
| HCL | ✓ | ✓ | partial (no formatting) | `hcl-rs` |
| INI | ✓ | ✓ | partial (preserve quotes via flag) | `rust-ini` |
| .env | ✓ | ✓ | best-effort | `dotenvy` + custom |
| Dockerfile | ✓ | — | n/a (read-only, для linting) | `dockerfile-parser-rs` |
| .gitignore / .dockerignore | ✓ | — | n/a (ignore-list format) | свой |
| CSV/TSV | ✓ | ✓ | n/a (tabular only) | `csv` crate |
| TOON | — | ✓ | n/a (write-only, для LLM context) | крейт `toon-format` (0.4) |
| JSONL/NDJSON | ✓ | ✓ | n/a (line-oriented) | свой |
| Markdown | ✓ | ✓ | full (M9, через comrak) | `comrak` |
| MD frontmatter | ✓ | ✓ | full (within MD doc) | свой парсер блока + delegate |
| XML | ✓ | partial | M11 — comments/CDATA/PI/namespaces/decl сохраняются; mixed-content opaque (`tracing::warn!`); pretty-printing не сохраняется | `quick-xml` |

**Что в этом списке НЕТ и почему:**

- **MessagePack/CBOR/Bencode** — бинарные форматы, отдельный класс задач, не нужно для DevOps use cases.
- **Lua tables** — слишком нишевый формат (issue запрашивался один раз для prosody).
- **Protobuf-text/Avro** — требуют schema-aware парсинга, отдельный сложный мир.
- **GraphQL/SQL** — это языки, не данные. Не наш scope.

## Path syntax

**JSON Pointer (RFC 6901)** — primary path-syntax для всех команд. Минимальный, однозначный, агент почти не ошибается:

```
/spec/template/spec/containers/0/image
/metadata/labels/app.kubernetes.io~1name        # ~1 экранирует /
```

**JSONPath (RFC 9535)** — только в команде `select`, для read-only фильтрации:

```
$.spec.containers[*].image
$..containers[?(@.name == "app")]
```

**jq-выражения** — в команде `query`, через embedded jaq:

```
'.spec.replicas |= . + 1'
'.spec.containers[] | select(.image | endswith(":latest"))'
```

**AST-селекторы** для tree-форматов (markdown) — research отложен до M9. Кандидаты: CSS-style (`heading[level=1]`), своё, или адаптация чего-то существующего.

## Полный список команд

### Data commands

```
dq get <file> <pointer>              # читает значение по pointer'у
dq set <file> <pointer> <value>      # записывает значение
dq del <file> <pointer>              # удаляет значение
dq exists <file> <pointer>           # exit 0/1, без stdout
dq keys <file> <pointer>             # ключи объекта
dq values <file> <pointer>           # значения объекта
dq len <file> <pointer>              # длина массива/строки/объекта
dq type <file> <pointer>             # тип значения
dq paths <file>                      # все pointer'ы файла как JSON-tree
dq select <file> <jsonpath>          # JSONPath query, может вернуть массив
dq query <file> <jq-expr>            # полная jq-семантика через jaq
dq convert <file>                    # читает, выводит в другом формате (-F)
dq fmt <file>                        # pretty-print без изменения данных
dq diff <a> <b>                      # структурный diff (output: JSON Patch)
dq validate <file>                   # exit 0/1, structured error если invalid
dq patch <file> <ops>                # применить RFC 6902 JSON Patch
dq merge <base> <override>           # RFC 7396 Merge Patch
```

### Lint commands

```
dq lint <files>                      # запустить ruleset на файлах
dq check <rule.yml> <files>          # запустить одно правило
dq test <rules-dir>                  # unit-tests для правил
dq fix <files>                       # автофикс violations
dq explain <rule-id>                 # документация по правилу
dq rules list                        # все доступные правила
dq rules add <ruleset>               # подключить готовый ruleset
```

### Self-management

```
dq self check                        # есть ли новая версия
dq self update [--to <ver>]          # обновиться
dq completions <shell>               # генерация completions
dq config get|set|list               # пользовательские настройки
dq init                              # инициализация .dq/ в проекте
```

### Глобальные флаги

```
-F, --format <fmt>          # input/output format override (auto-detect by default)
-v, --verbose               # ArgAction::Count: -v=INFO, -vv=DEBUG, -vvv=TRACE
-q, --quiet                 # ERROR-only logging; conflicts_with verbose
-i, --in-place              # запись обратно в файл (M2+; M1 парсит и отвергает)
    --diff                  # вывести diff, не писать
    --backup                # сохранить .bak файл при -i
    --check                 # exit 1 если изменений нет/требуется (для CI)
    --doc <idx|all>         # multi-document YAML: выбор документа
    --root                  # вывод полного документа после set (вместо измененного значения)
    --sort-keys             # стабильная канонизация ключей
    --indent <N>            # ширина отступа
    --flow-style <mode>     # block|flow|auto для массивов
    --quote-style <mode>    # double|single|auto для строк
    --strip-comments        # удалить комментарии при выводе
    --allow-templates       # не ошибаться на Helm/Go-template файлах
    --raw-template-strings  # treat templated values как opaque strings
    --continue-on-error     # для multi-file операций
    --parallel <N>          # parallelism для glob операций
    --no-color              # отключить ANSI цвета
    --no-pager              # не вызывать $PAGER даже если tty
```

Все флаги — `global = true` в clap, доступны в каждом subcommand.

Env-overrides: `DQ_FORMAT`, `DQ_IN_PLACE`, `DQ_NO_COLOR`, `DQ_CONFIG`, `NO_COLOR`, `CLICOLOR_FORCE`, `RUST_LOG`.

## Структура линтера и формат правила

Правило — YAML-файл. Минимальная структура:

```yaml
# rules/k8s/no-latest-tag.yml
id: k8s.no-latest-tag
description: |
  Containers should not use the :latest image tag because it makes
  rollbacks impossible and ties production to mutable references.
severity: error
match:
  format: yaml
  filter: '.kind == "Deployment" or .kind == "StatefulSet"'
check:
  jq: '.spec.template.spec.containers[] | select(.image | test(":latest$"))'
  message: "Container '{{ .name }}' uses :latest tag (image: {{ .image }})"
fix:
  # optional automatic fix
  prompt: "Pin to a specific tag"
references:
  - https://kubernetes.io/docs/concepts/containers/images/#image-names
```

Структура правила:

- `id` — уникальный, формат `<namespace>.<rule-name>`. Стандартные используют namespace `k8s`, `md`, `npm` и т.д.
- `description` — что правило проверяет и зачем (важно для `dq explain`).
- `severity` — `error`, `warn`, `info`. Влияет на exit code.
- `match` — критерии применимости: формат файла, filter-выражение по содержимому, glob по имени.
- `check` — jq-выражение (через embedded jaq), которое возвращает violations. Если результат не пустой — есть violation. Каждый элемент результата используется для построения сообщения.
- `message` — шаблон сообщения с подстановкой полей из violation.
- `fix` — опциональный автофикс: `set`/`del`/`patch`-операции или jq-трансформация.
- `references` — ссылки на документацию.
- `loc` — опциональный override location для diagnostic'а. Используется когда правило проверяет generated файл и хочет указать на оригинал. Формат: `{ file: <path>, line: <n> }`. Поля могут быть jq-выражениями над violation.

**Unit tests для правил.** Рядом с каждым правилом — файл `<rule>.test.yml` с фикстурами. Запускается через `dq test <rules-dir>`. Это критично — без тестов правила писать страшно. Формат:

```yaml
# rules/k8s/no-latest-tag.test.yml
tests:
  - name: deployment with latest tag triggers
    input: |
      kind: Deployment
      spec:
        template:
          spec:
            containers:
              - name: app
                image: my-app:latest
    expected:
      violations:
        - rule: k8s.no-latest-tag
          message_contains: "uses :latest"
  - name: pinned tag passes
    input: |
      kind: Deployment
      spec:
        template:
          spec:
            containers:
              - image: my-app:v1.2.3
    expected:
      violations: []
```

Стандартные ruleset'ы:

- `@std/k8s` — Kubernetes manifests (no-latest-tag, has-resources-limits, has-liveness-probe, no-host-network, no-privileged, и т.д., целевая партия 15-20 правил).
- `@std/markdown` — стиль и контент (heading-order, no-empty-links, code-blocks-have-lang, no-broken-relative-links, frontmatter-required-fields, и т.д., 15-20 правил).
- `@std/npm` — package.json и tsconfig.json (semver-versions, no-private-publish, engines-required, strict-tsconfig, 5-10 правил).
- `@std/github-actions` — workflow files (pin-actions-by-sha, no-pull-request-target-without-guard, timeout-minutes-required, 5-10 правил).

Custom rules лежат в `.dq/rules/` в корне проекта или указываются явно через `dq lint --rules path/to/rules.yml`.

Composite-rules (M11) — правило для одного формата вызывает парсер другого. Пример: «code-blocks с языком yaml в markdown должны быть валидным YAML» — query селектит code blocks из markdown AST, потом каждый передаётся в YAML-парсер.

## Roadmap

Версионирование calendar-based: `YYYY.WW.patch` (как у atl). Релиз каждой milestone — отдельная минорная версия.

### M1 — Read-only foundation

**Цель:** read-команды для key-value-форматов работают, конверсия форматов работает, агент может исследовать незнакомый файл одним вызовом. Без write-операций.

**Команды:** `get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select`, `convert` (только output, без in-place), `validate`.

**Форматы:** YAML, JSON, TOML на чтение. На запись только в M2 — но тут уже работает write для конверсии (поскольку форматирование не сохраняется при смене формата). TOON write. JSONL read+write.

**Технически:** clap v4 derive, `camino::Utf8PathBuf` для всех путей, structured errors через `thiserror` per-crate (`dq_core::Error` enum) + `anyhow::Result` в command handlers, `tracing` + `tracing-subscriber` (`EnvFilter`, `fmt`) для логирования, JSON Pointer (своя реализация в `dq-core`, см. Layer 1), JSONPath через `jsonpath-rust` 0.7 (только в `select`), TOON через крейт `toon-format` 0.4, dispatch формата по расширению/содержимому/`-F`. Reporter trait с DI; SIGPIPE→SIG_DFL на Unix; exit-codes в `pub mod exit_code`.

**Парсеры в M1:** для read-only нам не нужен round-trip — берём `serde_yml`/`serde_json` (с фичами `preserve_order`+`arbitrary_precision`)/`toml` (стандартные serde-крейты). Это упрощает M1 и снимает риск.

**Distribution:** ничего, кроме `cargo install` и собранного бинаря из релиза. Установочный скрипт — в M6.

**Что НЕ входит:** любые write-операции в существующие файлы, jq, линтеры, multi-file glob, markdown.

**Definition of done:**
- `dq get`, `dq paths`, `dq convert`, `dq validate` работают на репрезентативных k8s/helm/github-actions YAML-файлах (golden-runner ≥ 20 fixture-файлов).
- Все команды имеют `-F json` структурированный output. Ошибки парсинга показывают line/col + caret.
- SIGPIPE smoke-тест (`dq paths big.yaml | head -n 1` без panic, на Unix).
- Color resolution precedence-тест (`--no-color` > `NO_COLOR` > `CLICOLOR_FORCE` > TTY-detect; через `Command::env`, без `std::env::set_var`).
- Snapshot-тесты (insta) для structured Path/Parse error rendering — JSON и console-with-`--no-color`.
- Property-тест (proptest, ≥ 100 cases) round-trip Pointer↔canonical для случайно сгенерированных Value-деревьев.
- `dq generate-docs --output-dir <DIR>` создаёт man pages и shell-completions для bash/zsh/fish/powershell.
- Тесты `cargo test --workspace --all-features` зелёные; runtime cold ≤ 30s.

**Spike в начале M1:** написать 5-6 типичных read-сценариев, посмотреть, какие операции над Document'ом нужны на самом деле — и под это спроектировать публичный API `dq-core`.

### M2 — Safe writes

**Цель:** редактирование YAML/TOML без разрушения файла. Это самый рискованный технический блок проекта; всё после M2 зависит от того, что round-trip работает.

**Команды:** `set`, `del`. Флаги `-i/--in-place`, `--diff`, `--backup` для всех write-команд.

**Технически (после спайка — выбран подход textual-edit):** YAML write-pat использует `saphyr-parser` (низкоуровневый event API, **не** высокоуровневый `saphyr` крейт) исключительно для **structural discovery** — events со span'ами сворачиваются в `Pointer → ByteRange` span map; original bytes в `Document` не переписываются emitter'ом. `Document::set_at` модифицирует только нужный span. Это тот же textual-edit принцип, что использует `toml_edit` для TOML. Comments/blank lines/quote style сохраняются автоматически — байты вокруг них не трогаются. Custom emitter не нужен (и не реализуем — `saphyr-parser` scanner отбрасывает comment-токены до event stream'а, [issue #103](https://github.com/saphyr-rs/saphyr/issues/103)).

Read-pat M1 на `serde_yml` остаётся параллельно (для read-команд `get`/`paths`/...) — миграция read-pat на saphyr-parser-based парсер отдельный refactor change post-M2.

TOML round-trip через `toml_edit` (через `ImDocument::parse` — он сохраняет span'ы, тогда как `DocumentMut::from_str` их теряет). JSON round-trip — собственный span-builder со state-machine'ом byte-сканера, indent style detection (2/4/tab), JSONC rejection.

Helm/Go-template guard: `--allow-templates` / `--raw-template-strings` — глобальные флаги для `set`/`del`. Default — `Error::TemplatedFile` (exit 3) с hint mentioning обоих escape-hatch'ей.

**Безопасность записи:** atomic via `tempfile::NamedTempFile::new_in(parent)` + `persist(target)` (использует `rename` на Unix, `MoveFileEx` с `MOVEFILE_REPLACE_EXISTING` на Windows). Опциональный `--backup` пишет `.bak` рядом перед persist. На Windows — особый случай (rename'ы, открытые файлы), нужен явный тест (CI matrix отложен до M6 distribution; smoke-test gated `#[cfg(target_os = "windows")]` `#[ignore]`).

**Exit codes M2:** `WRITE_FAILED = 7` для `Error::WriteIo` и `Error::WriteUnavailable`; `Error::TemplatedFile` маппится в `PARSE_ERROR = 3`. Read-side IO остаётся `IO_ERROR = 5`.

**Helm/Go-template guard:** препроцессор детектит `{{ }}` шаблоны до парсинга. Если найдены — по умолчанию error с понятным сообщением: «File appears to be a Go template (Helm/Argo). dq cannot safely round-trip templated YAML. Use --allow-templates to proceed (formatting may break) or --raw-template-strings to treat templated values as opaque strings before parsing.» Это убирает целый класс багов с порчей шаблонов на in-place edit.

**Number precision:** числа сохраняются в исходном текстовом представлении. Для парсинга используется `serde_json::Number::from_str` или собственный обёртка над `String` для значений, не помещающихся в `i64`/`u64`/`f64` без потери. Round-trip большого ID (`4722366482869645213696`) даёт ровно ту же строку.

**Что НЕ входит:** bulk-операции, multi-file write, diff между файлами, форматирующие флаги типа `--sort-keys`.

**Definition of done:** `dq set` и `dq del` на репрезентативном Helm chart с комментариями, anchor'ами и multi-document разделителями дают diff, в котором изменена ровно одна строка. Goldensnapshots тестируют round-trip на 25+ репрезентативных файлов (k8s/helm/hugo/github-actions/Cargo/package). Property test (proptest, ≥ 100 cases) для round-trip Pointer↔canonical YAML на random-generated документах. Workspace test runtime cold ≤ 30s.

**Risk mitigation:** перед началом M2 — две недели спайка по `saphyr`-event API на репрезентативных файлах. Если round-trip не получается с приемлемым качеством — переоцениваем стратегию (либо собственный парсер на 2-3 месяца, либо релиз без сохранения форматирования с честным указанием в README).

### M3 — Bulk и CI ✅ Implemented 2026-05-03 (см. [openspec/changes/add-bulk-and-ci/](openspec/changes/add-bulk-and-ci/))

**Цель:** одна команда — много изменений или много файлов.

**Команды:** `patch` (RFC 6902 JSON Patch + упрощённый построчный формат), `merge` (RFC 7396 Merge Patch), `diff` (структурный, output JSON Patch по умолчанию, опционально unified).

**Multi-file:** все write-команды принимают glob: `dq set 'k8s/**/*.yaml' /spec/replicas 3 -i`. Флаги `--continue-on-error` (для CI), `--parallel N` (для глобов в сотни файлов), `--check` (exit 1 если требуется изменение — idempotency check).

**Технически:** `globset` или `glob` крейт. Параллелизм через `rayon` с глобальной семантикой (одинаковая операция на каждый файл). Diff-движок: рекурсивный обход с генерацией JSON Patch операций.

**Что НЕ входит:** трансформации (jaq), строгое сравнение (с учётом порядка ключей, который нам по умолчанию неважен), линтеры.

**Definition of done:** `dq set 'k8s/**/*.yaml' /spec/template/spec/containers/0/image my-image:v2 -i` обновляет 50 файлов одной командой, выводит summary `Modified: 47, Skipped: 3 (already up to date)`. `dq diff prod-values.yaml staging-values.yaml` даёт читаемый структурный diff.

### M4 — Стиль и нормализация ✅ Implemented 2026-05-04 (см. [openspec/changes/add-style-and-normalization/](openspec/changes/add-style-and-normalization/))

**Цель:** всё, что связано с «как должен выглядеть результат».

**Команды:** `fmt` (re-emit через native writer; drops comments — это intentional contract canonicalizer'а; M2 textual-edit pipeline остаётся для `set`/`del`/`patch`/`merge`, где сохранение комментариев — это контракт), полировка `validate --check` для CI (accepts the flag for symmetry с pre-commit hook entries — same parse-only semantics as без флага).

**Глобальные флаги в M4:** `--sort-keys` (deep-recursive map-key sort через `dq_core::canonicalize_keys`, applied на re-emit paths — `fmt`/`convert -i`; для textual-edit splice paths — `set`/`del`/`patch`/`merge` — флаг принимается, но это no-op, чтобы не ломать M2 round-trip контракт), `--indent <N>` (JSON/JSONL honor; YAML/TOML accept and ignore — for YAML deferred to a future milestone with comment-preserving emitter, for TOML grammar-fixed). Threaded через `dq_core::WriteOptions { sort_keys, indent }` (`#[non_exhaustive]`) и новый `Format::write_with_options(doc, w, opts)` trait method.

**Deferred to M5+ (originally listed for M4 but require comment-preserving emitter — `serde_yml` / `toml_edit::DocumentMut` / собственный JSON writer не surface these knobs, и saphyr-parser scanner discards comment tokens до event stream'а per [issue #103](https://github.com/saphyr-rs/saphyr/issues/103)):** `--flow-style block|flow|auto`, `--quote-style double|single|auto`, `--strip-comments`. Эти три флага становятся meaningful когда landing комплект M5+ saphyr-emitter rewrite или альтернативную YAML library; M4 ships только две flag'и whose value is unambiguous.

**Pre-commit hook:** `.pre-commit-hooks.yaml` в repo root с `dq-fmt-check` (`dq fmt --check $FILES`) и `dq-validate` (`dq validate $FILES`) entries. `language: system` в M4 (предполагает `dq` на PATH); M6 distribution rewrite-нет на `language: rust` после `cargo install dq` story.

**Что НЕ входит:** автофикс линтеров (M10), jq-driven transforms (M7), markdown / tree-format (M9), три deferred flag'и выше.

**Definition of done:** `dq fmt --sort-keys -i k8s/**/*.yaml` нормализует все файлы (через bulk driver M3). `dq fmt --check` в pre-commit ловит ненормализованные файлы (exit 1, list of would-change paths to stdout). `dq convert -F json --indent 4` производит 4-space JSON. Pre-commit интеграция работает в типовом проекте. 569 тестов зелёные после implementation; M2/M3 golden suite не регрессирован (`WriteOptions::default()` byte-identical with M3 writer behaviour).

### M5 — Расширение форматов ✅ Implemented 2026-05-04 (см. [openspec/changes/archive/2026-05-04-add-format-extensions/](openspec/changes/archive/2026-05-04-add-format-extensions/))

**Цель:** покрытие форматов, нужных для линтеров в M8.

**Добавляются:** HCL (read+write через `hcl-rs`, без сохранения форматирования в v1), INI/.properties (read+write с preserve quotes), .env (read+write через `dotenvy`+wrapper), CSV/TSV (только array-of-objects на верхнем уровне, через `csv` крейт), Dockerfile (read-only, через `dockerfile-parser-rs` — для linting), .gitignore/.dockerignore (read-only, как ignore-list для линтеров типа «не игнорить .env файлы»).

**Markdown frontmatter:** парсер `.md`-файла извлекает YAML/TOML/JSON-блок в начале, делегирует существующим парсерам, сохраняет тело документа как opaque строку. Read+write.

**Что НЕ входит:** полноценный markdown AST (это M9), XML write, более экзотичные форматы (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL) — добавляются только при наличии конкретного use case.

**Definition of done:** все перечисленные форматы работают в командах `get`, `set`, `convert`, `validate` (где применимо). Frontmatter в Hugo/Jekyll/Obsidian-стиле редактируется без потери тела документа. Dockerfile парсится и доступен для query через `dq get` (`/from`, `/run/0`, и т.д.).

### M6 — Distribution ✅ Implemented 2026-05-04 (см. [openspec/changes/archive/2026-05-06-add-distribution/](openspec/changes/archive/2026-05-06-add-distribution/))

**Цель:** инструмент устанавливается в одну команду, обновляется встроенно, имеет skill для Claude Code.

**Артефакты:**
- Prebuilt binaries для `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`. Cross-compile через `cross` или GitHub Actions matrix.
- `scripts/install.sh` — curl-pipe-sh с `--version`, `--install-dir`. Скопировать структуру из atl.
- `dq self check`, `dq self update [--to <ver>]` — обновление из GitHub Releases.
- `dq completions <shell>` для bash/zsh/fish/powershell через `clap_complete`.
- Man pages через `clap_mangen`.
- Docker image: alpine-based и scratch-based (минимальный) на Docker Hub и ghcr.io. Non-root user.
- Homebrew tap: `brew install mazuninky/tap/dq`.
- AUR PKGBUILD.
- Skill для Claude Code на skills.sh: `npx skills add mazuninky/dq`. Skill покрывает все команды M1-M5, типичные паттерны (k8s/helm/hugo workflows, CI integration), edge cases.
- Output formatters: console (с цветом), json, sarif (для GitHub PR annotations), junit (для legacy CI типа Jenkins/GitLab), tap (для Perl/JS-tooling).

**CI:** GitHub Actions с release-on-tag, signed releases (committer signature), checksums файл с SHA256 для всех артефактов.

**Performance baseline:** бенчмарки на репрезентативных задачах (read большого values.yaml, set + write k8s manifest, multi-file glob на 100 файлов). Цифры публикуются в README.

**Definition of done:** `curl -sSfL ... | sh` ставит свежую версию на macOS/Linux. `dq self update` работает. Skill устанавливается через skills.sh и используется Claude Code'ом для генерации правильных команд.

### M7 — Transform layer (jaq) ✅ Implemented 2026-05-04 (см. [openspec/changes/add-transform-layer/](openspec/changes/add-transform-layer/))

**Цель:** добавить полноценную jq-семантику для read-операций и сложных трансформаций.

**Команды:** `query` — принимает jq-выражение, работает над любым форматом (читает в Document, конвертирует в jq-совместимый JSON, выполняет, конвертирует обратно).

**Технически:** интеграция `jaq-core` + `jaq-std`. Адаптер Document ↔ jaq Value. Опциональная фича `embedded-jq` (default ON), при выключении `query` команда деградирует до spawn внешнего `jq` если установлен, иначе error.

**Использование внутри:** `set` с jq-режимом — `dq set <file> --jq '.spec.replicas |= . + 1' -i`. Это покрывает дыру атомарного дизайна для трансформаций.

**Что НЕ входит:** exec engine, линтеры (это требует jq и поэтому идёт после).

**Definition of done:** `dq query` проходит подмножество jq-тестов из `jq-1.7-test`. `dq set --jq` работает на типичных трансформациях (увеличить replicas, переименовать ключ, конвертировать поле).

### M8 — Exec engine + первые линтеры ✅ Implemented 2026-05-04 (см. [openspec/changes/archive/2026-05-06-add-exec-engine/](openspec/changes/archive/2026-05-06-add-exec-engine/))

**Цель:** платформа для линтеров работает, есть готовые ruleset'ы для трёх форматов, инструмент решает задачу «прогони линтер на CI».

**Команды:** `lint`, `check`, `test` (unit-tests для правил), `explain`, `rules list`, `rules add`. (`fix` — в M10.)

**Технически:** `dq-exec` крейт. Парсер YAML-формата правила. Evaluator: для каждого файла определяет матчящие правила (`match.format`, `match.filter`), запускает `check.jq`, для каждого результата строит Diagnostic с поддержкой `loc` override из правила (важно для generated-файлов). Reporter с output форматами console (с цветом), json, sarif, junit, tap.

**Unit testing для правил:** `dq test rules/` находит все `*.test.yml` файлы, для каждого запускает фикстуры через rule evaluator, сравнивает с `expected.violations`. Без unit tests правила писать страшно — это первоклассная фича, не nice-to-have.

**Стандартные ruleset'ы:**
- `@std/k8s` — 15-20 правил для Kubernetes manifests, с тестами.
- `@std/npm` — 5-10 правил для package.json и tsconfig.json, с тестами.
- `@std/github-actions` — 5-10 правил для workflow файлов, с тестами.
- `@std/dockerfile` — 5-10 правил (no-latest-base-image, has-healthcheck, no-add-use-copy, и т.д.).

**Discovery:** `dq lint <files>` без `--rules` — использует все `@std/*` rulesets, применимые к данным форматам, плюс `.dq/rules/*.yml` если существуют.

**Что НЕ входит:** markdown (M9), автофикс (M10), composite-rules (M11), JSON Schema (M11), community registry (M12), WASM-плагины (M12).

**Definition of done:** `dq lint k8s/**/*.yaml` находит все типичные нарушения в репрезентативном кластерном репо. SARIF-вывод корректно показывается в GitHub PR annotations. `dq test rules/` зелёный для всех `@std/*` ruleset'ов (≥40 правил с тестами). `dq explain k8s.no-latest-tag` показывает описание правила.

### M9 — Markdown / tree-format ✅ Implemented 2026-05-05 (см. [openspec/changes/archive/2026-05-05-add-markdown-tree-format/](openspec/changes/archive/2026-05-05-add-markdown-tree-format/))

**Цель:** linting markdown-документации. Это первый tree-формат и валидация архитектуры на разнообразии моделей данных.

**Технически:** `comrak` для CommonMark+GFM парсинга с round-trip. Tree модель в `dq-core`: `Tree` со типизированными узлами (`Heading`, `Paragraph`, `Link`, `CodeBlock`, и т.д.) и position info.

**AST-селекторы:** research в начале M9 (отложен в плане специально). Кандидаты: CSS-style (`heading[level=1]`, `link[external]`, `codeblock[lang="yaml"]`) или своё минимальное. Решение принимается после прототипа.

**Стандартный ruleset:** `@std/markdown` — 15-20 правил (heading order, no-empty-links, code-blocks-have-lang, no-broken-relative-links, frontmatter-required-fields, no-trailing-whitespace, и т.д.).

**Что НЕ входит:** composite-rules (markdown → yaml-валидация code blocks), это M11.

**Definition of done:** `dq lint docs/**/*.md` проходит на 1Orbit-документации (Confluence GDS, type system docs). AST-селекторы стабильны и документированы.

### M10 — Автофиксы ✅ Implemented 2026-05-05 (см. [openspec/changes/archive/2026-05-06-add-autofix/](openspec/changes/archive/2026-05-06-add-autofix/))

**Цель:** `dq fix` работает для всех format'ов с round-trip.

**Технически:** `Rule.fix` секция содержит трансформацию (jq-выражение или явный набор ops). При `dq fix` для каждого нарушения применяется fix, файл записывается атомарно. С `--diff` показывается, что будет исправлено, без записи. С `--check` exit 1 если есть исправимые нарушения (для CI).

**Безопасность:** fix идемпотентный (повторное применение не меняет результат). Если fix не идемпотентен — это баг правила, валидация при загрузке. Auto-applied фиксы логируются.

**Definition of done:** `dq fix --check` в pre-commit ловит исправимые проблемы. `dq fix -i` чинит и пишет атомарно. Round-trip сохраняется (форматирование вокруг исправления остаётся прежним).

### M11 — JSON Schema, composite-rules, расширенные форматы ✅ Implemented 2026-05-07 (см. [openspec/changes/add-validation-and-extended-formats/](openspec/changes/add-validation-and-extended-formats/))

**Цель:** убрать оставшиеся пробелы в покрытии.

**JSON Schema:** валидация против JSON Schema 2020-12 через `jsonschema` крейт. Реализуется как стандартное правило (`@std/jsonschema`) — не отдельная команда, потому что архитектурно это просто rule с особым check'ом. `Rule.check` стал `oneOf [jq | schema | schema_file | extract+nested]`; `instancePath` JSON Schema-ошибок мапится 1:1 в RFC 6901 `Pointer`. `$ref` ограничены internal-references — HTTP/file `$ref` отвергаются на этапе компиляции. Три референс-правила: `@std/jsonschema/{kubernetes-crd-shape, helm-values-against-schema, openapi-3.1-shape}`.

**Composite-rules:** правило в одном формате может извлекать данные и валидировать через парсер другого. `extract:` jq-выражение возвращает `[{value, format, anchor}]`, `nested:` рекурсивно типизированное правило. Координаты вложенных диагнозов проектируются на исходный файл через anchor + inline-offset. Hardcoded `MAX_EXTRACT_DEPTH = 4` защищает от self-similar extract'а. Inner-format parse failure эмитит `<outer>.parse-failed` как outer-rule violation. Первое правило: `@std/markdown/code-blocks-yaml-valid`.

**Inline-level position spans:** `Provenance::Original` расширен опциональным `inline_offset: Option<InlineBaseline>`. YAML block scalars (`|`, `>`, `|-`, `>-`) и markdown fenced code blocks обязательно выставляют inline-baseline; остальные парсеры — `None` (best-effort для JSON-strings с `\n`). Backward-compatible — existing callers не ломаются.

**XML read+write** через `quick-xml` 0.36 — добавляется как новый формат с conventional-key мэппингом (`@attrs`, `#text`, `#comments`, `#cdata`, `#pi`, `#xml`). Round-trip **partial**: structure/attrs/comments/CDATA/PI/namespaces/decl сохраняются; mixed-content (текст вперемежку с элементами) folds в `#text` с `tracing::warn!`.

**Расширенные форматы:** Terraform HCL правила (`@std/terraform` — 8 правил: secrets/tags/version-pinning/security/state-backend/sensitive-outputs/variable-docs), OpenAPI (`@std/openapi` — 6 правил: info-required/paths/uniqueness/responses/no-trailing-slash/security). OpenAPI shipped без `oas3` зависимости — все правила выражены через jq + JSON Schema (фича-гейт исключён, бинарь меньше). Standard rule library теперь ровно 64 правила в 8 namespaces (`k8s`, `dockerfile`, `npm`, `github-actions`, `markdown`, `jsonschema`, `terraform`, `openapi`).

**Known limitations:**
- HCL parser (M5) не populating'ит `Provenance::Original.span` — все Terraform диагнозы report at line 1, col 1. Span-aware HCL — отдельный отложенный change.
- XML mixed-content opaque на round-trip; `tracing::warn!` сообщает пользователю.
- XML parser принимает только UTF-8-совместимые input'ы — `quick-xml` 0.36 без feature-flag'а `encoding` отвергает XML декларации с UTF-16 / Windows-1251 / прочими non-UTF-8 кодировками. Re-encode in UTF-8 перед `dq` для legacy Windows / Visual Studio файлов.
- `data-query-plugin-abi` WIT не extended — inline-spans **не** прокидываются в WASM-плагины в этом change'е.
- XSD / RelaxNG / Schematron / OpenAPI runtime-validation — anti-scope.

**Definition of done:** ✅ JSON Schema покрывает типичные schema-validation use cases. ✅ Composite rules работают для cross-format валидации. ✅ XML round-trip (partial) для config-shaped XML. ✅ 64 правила в стандартной библиотеке (8 namespaces). ✅ `cargo test --workspace --all-features` зелёный (1202 passed); `cargo test --workspace --no-default-features` зелёный (1187 passed).

### M12 — Community rules registry + WASM-плагины

**Цель:** rule sets публикуются третьими лицами и устанавливаются одной командой; для случаев, где YAML+jq недостаточно, доступны WASM-плагины.

**Git-based registry для rulesets.** `dq rules add github:owner/repo` клонирует репо в `~/.config/dq/rules/<owner>/<repo>`. Манифест ruleset'a (`dq-rules.yml` в корне) описывает rules, версии, deps. `dq rules update` обновляет установленные rulesets. Поддержка ref'ов: `dq rules add github:owner/repo@v1.2.3` или `@main`. Для приватных репо — стандартные git credentials (ssh-keys, GitHub tokens). Никакого OCI — git проще, диагностируемее, и не требует доп. инфраструктуры.

**Curated index:** небольшой curated index популярных rulesets на сайте проекта или в README. Не центральный registry в стиле npm — слишком много инфраструктуры.

**WASM-плагины.** Для случаев, когда YAML+jq недостаточно (свой формат файла, специфический парсер, кастомный check, нестандартная aggregation). Плагины пишутся на любом языке, который компилируется в WASM (Rust, Go, AssemblyScript, etc.), реализуют WIT-интерфейс `dq-plugin`. Runtime через `wasmtime` или `wasmer`.

WIT-интерфейс плагина минимальный:
- `parse(bytes) -> Document` — для регистрации нового формата.
- `check(document, rule_config) -> Vec<Violation>` — для регистрации custom check'а в правиле.
- `format(document) -> bytes` — для регистрации writer'а нового формата.

Плагины устанавливаются: `dq plugin add github:owner/dq-plugin-foo` (тот же git-based механизм, что и для rules), кэшируются в `~/.config/dq/plugins/`. Загружаются в runtime через WASM ABI. Песочница изолирует плагин от файловой системы (никаких syscall'ов, кроме явно whitelist'ованных через WASI).

**Что НЕ входит в M12:** native plugins (dlopen/.so), HTTP-loaded плагины, plugin marketplace в стиле npm.

**Definition of done:** `dq rules add github:org/dq-rules-mycompany` работает. `dq plugin add github:org/dq-plugin-prometheus` ставит и регистрирует плагин для нового формата. Минимум один эталонный WASM-плагин опубликован (например, для парсинга `.prometheusrules.yaml`-файлов или собственного 1Orbit-формата как dogfood). Несколько community-rulesets опубликованы (минимум те, что я опубликую сам как пример).

## Tech stack

**Rust toolchain:**
- MSRV: stable 1.94+ (как у atl).
- Edition: 2024.
- Workspace cargo, lefthook для git hooks, rustfmt + clippy в CI.

**Ключевые зависимости:**
- `clap` v4 (derive macros) — CLI.
- `clap_complete` — completions.
- `clap_mangen` — man pages.
- `camino` — UTF-8 пути (`Utf8PathBuf`/`Utf8Path`) везде вместо `std::path`. Убирает `.to_str().unwrap()` из всего кодбейса.
- `tracing` + `tracing-subscriber` (env-filter, fmt) — логирование. Никакого `log` крейта; никаких `println!`/`eprintln!` для диагностики.
- `thiserror` (per-crate domain errors) + `anyhow` (command handlers) — primary error stack. `miette` опционально для рендера диагностик в M2/M3, не как основной error type.
- `saphyr-parser` — YAML write-pat span builder (M2+; низкоуровневый event API, **не** высокоуровневый `saphyr`).
- `serde_yml` — YAML read-pat (M1+; остаётся параллельно для read-команд).
- `serde_json` (фичи `preserve_order` + `arbitrary_precision`) — JSON, сохранение порядка ключей и точности больших чисел.
- `toml_edit` — TOML round-trip (M2+; через `ImDocument::parse` для span preservation).
- `similar` — unified diff для `--diff` флага в `set`/`del` (M2+).
- `regex` — template guard pattern matching (M2+).
- `tempfile` — atomic writes через `NamedTempFile::new_in` + `persist` (M2+).
- `toon-format` 0.4 — TOON encoder (write-only output для LLM context).
- `comrak` — markdown (M9).
- `jaq-core` + `jaq-std` — jq engine (M7).
- `jsonpath-rust` 0.7 — JSONPath (RFC 9535) для команды `select` (M1).
- `jsonschema` — JSON Schema validation (M11).
- `indexmap` (с `serde` фичей) — order-preserving Map в `Document`.
- `globset` — glob matching (M3 §3 bulk driver).
- `walkdir` — file-tree traversal от longest non-meta prefix glob'а (M3 §3).
- `rayon` — параллелизм для multi-file (M3 §3 `--parallel <N>`).
- `regex` уже выше; `tempfile` уже выше (atomic writes + integration tests).
- `libc` — SIGPIPE→SIG_DFL на Unix (M1).
- `serde` для JSON output структур.
- `csv`, `quick-xml`, `dotenvy`, `hcl-rs`, `rust-ini`, `dockerfile-parser-rs` — форматы по необходимости.
- `wasmtime` (или `wasmer`) — WASM runtime для плагинов (M12).
- `git2` или shell-out на `git` — клонирование rulesets и плагинов из git-репо (M12).

**Build / CI:**
- GitHub Actions с matrix builds для всех таргетов.
- `cross` для Linux cross-compile.
- Cargo workspaces.
- `[profile.release]`: `lto = true`, `codegen-units = 1`, `strip = true` — минимальный одиночный binary.
- `rust-toolchain.toml` пинит channel (1.94 в M1; rustup auto-installs).
- `cargo-deny` для license/security audit.
- `cargo-nextest` для тестов.
- Goldensnapshots тесты на round-trip (`insta`).
- Property-based tests для round-trip (`proptest`).
- Benchmarks через `criterion`.

**Documentation:**
- `mdbook` для основной документации (как у Rust tooling).
- README + CONTRIBUTING.md + SECURITY.md.
- `docs.rs` для библиотечного API.

## Тестирование

Стратегия тестирования принимается из skill `/rust-cli` (`references/cli-testing.md`) как нормативный документ — не пересогласовывается на каждом milestone.

**Пирамида:**
- ~75% **unit-тестов** в `crates/<crate>/src/<module>.rs` через `#[cfg(test)] mod tests`. Чистые функции (Pointer parse, did_you_mean, Format trait reflexivity, exit-code mapping).
- ~20% **component-тестов** в `crates/<crate>/tests/<feature>.rs` — тестируют публичный API крейта. Для `dq-cli` — handlers через in-process `dq::run(&Cli, use_color, &mut Vec<u8>, &mut Vec<u8>)`. Никакого `assert_cmd` на этом слое.
- ~5% **CLI integration-тестов** в `crates/dq-cli/tests/cli_*.rs` через `assert_cmd` + `predicates`. Smoke-сценарии, exit-codes, snapshot-рендеринг ошибок.

**Обязательные паттерны:**
- `tracing_subscriber::fmt().try_init()` (НЕ `.init()`) — повторные вызовы `dq::run` в одном процессе не должны паниковать.
- `Registry::with_items(...)` test-constructor для любой singleton-структуры (для M8 линтеров) — с `#[cfg(any(test, feature = "test-util"))]`. Никаких `pub(crate)` / `#[cfg(test)] pub fn __test_*` escape-hatches.
- `use_color: bool` тредится через параметры; **запрещено** `std::env::set_var("NO_COLOR", ...)` где-либо. CLI integration-тесты используют `Command::env("NO_COLOR", "1")` для изоляции.
- SIGPIPE smoke-тест на Unix-targets.
- Color resolution precedence-тест (`--no-color` > `NO_COLOR` > `CLICOLOR_FORCE` > TTY-detect).
- Snapshot-тесты (`insta`) для structured error rendering — JSON и console-with-`--no-color`.
- Property-тесты (`proptest`, ≥ 100 cases) для invariants: round-trip Pointer↔canonical, parse-write fidelity для number-precision, и т.д.
- Goldensnapshots ≥ 20 fixture-файлов в `tests/fixtures/golden/` (M1: 21; растёт по мере добавления форматов в M5/M9).

**Runtime budget:** полный suite `cargo test --workspace --all-features` cold ≤ 30s, warm ≤ 10s. Если падает за пределы — рефакторить тесты, не код (per skill).

**Делегирование:** все Rust-правки идут через subagent `rust-cli-writer` (production) и `rust-cli-test-writer` (тесты). Правило в [.claude/rules/rust-delegation.md](.claude/rules/rust-delegation.md).

## Чего не будет никогда

Явный anti-scope, чтобы избежать scope creep:

- Свой query DSL. Используется JSON Pointer + jq (через jaq).
- Web playground. Не наш формат distribution.
- GUI. Терминальный инструмент.
- TUI. Не нужен.
- Native plugin system на shared libraries (dlopen/.so). Только WASM в M12.
- HTTP-fetch как input. Слишком много security surface'а; пользователь делает curl сам.
- Templating вывода через Tera/Handlebars. Это вне scope «query/edit структурированных данных».
- Bencode / MessagePack / CBOR. Бинарные форматы, отдельный класс.
- Lua tables. Слишком нишево.
- Admission controller для k8s. dq живёт в pre-commit/CI, не в кластере.
- Свой declarative policy language. Используется jq, точка.
- OCI-registry для distribution rulesets. Только git-based в M12. OCI добавляет complexity и vendor-lock на registry-инфраструктуру; git универсален.
- Pixel-perfect совместимость с конкретными существующими линтерами. Мы делаем универсальный engine; стандартная библиотека правил даёт coverage в популярных доменах.

## Что делать первым (для кодинг-агента)

Конкретный порядок шагов перед коммитом первой строки:

1. **Создать репо** `github.com/mazuninky/dq` (приватный или публичный — на усмотрение). MIT-лицензия. Структура из секции «Архитектура».

2. **Cargo workspace skeleton:**
   ```
   cargo new --bin dq-cli
   cargo new --lib crates/dq-core
   ```
   `Cargo.toml` workspace с `members = ["dq-cli", "crates/*"]`. Базовая зависимость `clap` v4 в `dq-cli`.

3. **Спайк по `saphyr` round-trip (1-2 недели до M1 implementation).** Цель: на пяти файлах разной сложности (простой YAML с комментариями, Helm chart с anchor'ами, k8s multi-document, GitHub Actions workflow, Hugo frontmatter) — написать parse → modify-one-value → write, проверить, что diff содержит ровно одну строку. Если работает — план M2 валиден. Если нет — на этом этапе ловим проблему и переоцениваем стратегию.

4. **M1 в порядке задач:**
   - Define `Document` enum в `dq-core` (с serde).
   - Implement `Format` trait + три формата на чтение через стандартные serde-крейты.
   - Implement `Pointer` — JSON Pointer parser + navigate/get.
   - Implement structured Error с line/col + caret rendering.
   - CLI: `dq get` ↔ `dq paths` ↔ `dq exists` ↔ `dq keys/values/len/type` ↔ `dq convert` ↔ `dq validate`.
   - Tests: integration tests с реальными файлами в `tests/fixtures/`.
   - Manual smoke tests на k8s/helm/github-actions репо.

5. **Релиз v0.1 (M1 done) на GitHub Releases.** Release notes описывают, что инструмент сейчас умеет (read-only) и что планируется. Параллельно начинать M2 spike.

6. **Дальше — по roadmap.**

## Метрики успеха

**Количественные (для каждого милестоуна):**
- M1: бинарь работает на 50+ типичных read-сценариях.
- M2: round-trip golden-snapshot на 30+ реальных файлов.
- M3: bulk-операция на 100+ файлов выполняется через одну команду без shell-out.
- M6: install через `curl|sh` за <30 секунд, бенчмарки опубликованы в README.
- M8: первая партия из 30+ правил в стандартной библиотеке, покрытие минимум трёх форматов.

**Качественные:**
- M2: ноль regressions в формате файла на golden-snapshot тестах.
- M6: skill для Claude Code установлен и используется агентом без ошибок в типичных задачах.
- M8: внешний пользователь (не я) может написать своё правило за <10 минут, прочитав только `dq explain` и одну страницу docs.

**Долгосрочные:**
- 1k звёзд на GitHub в первый год после M6 (distribution).
- ≥3 community-rulesets опубликованы к моменту M12.
- Brand mentions в дискуссиях (Reddit, HN, dev.to) — проактивный мониторинг.

## Открытые вопросы

Не блокирует начало работы, но требует решения по ходу:

- Финальное имя core-крейта: `dquery` vs `dq-core` vs другое. Перед публикацией на crates.io.
- AST-селекторный язык для markdown: closed by M9 — JSON Pointer + jq, no new selector DSL (см. [add-markdown-tree-format design D1](openspec/changes/archive/2026-05-05-add-markdown-tree-format/design.md)).
- WASM runtime для плагинов: `wasmtime` vs `wasmer`. Решается перед началом M12 на основе: размер скомпилированного бинаря, скорость холодного старта плагина, активность maintenance. По умолчанию — `wasmtime` (более популярен в CLI-tools).
- Поведение `--sort-keys`: глобальный флаг или только для `fmt`/`convert`. По умолчанию — глобальный, но с предупреждением что reordering меняет файл.
- Confluence/Hugo/Jekyll-specific frontmatter правила — в `@std/markdown` или отдельный `@std/static-sites`. По мере появления спроса.
- Format support для экзотичных форматов (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL): добавлять только при наличии конкретного use case или PR от community. Не делать proactively.

---

Документ передаётся кодинг-агенту как entry point. При работе над конкретными milestone'ами агент должен возвращаться к этому документу для проверки scope'а и принципов; никакой milestone не должен расширяться сверх описанного без явного решения автора.

Версия плана: draft 3, май 2026 (после validation против `/rust-cli` skill — см. [docs/archive/plan-validation-rust-cli-2026-05-03.md](docs/archive/plan-validation-rust-cli-2026-05-03.md)).
