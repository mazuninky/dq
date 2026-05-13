## Why

Anthropic-овский skill-loader предъявляет жёсткий контракт к содержимому `skill/` директорий: `SKILL.md` обязан иметь YAML-фронтматтер с `name`/`description`, имя должно матчить `^[a-z0-9][a-z0-9_-]*$`, а описание после folding'a не должно превышать 1024 символа — иначе оно молча обрезается лоадером. Параллельно у скиллов появилась практика `skill/evals/evals.json` для проверочных сценариев — там тоже стабильная схема (`skill_name`, `evals[].{id, prompt, expected_output, assertions}`).

Сейчас этот контракт нигде в линт-эко dq не покрыт. Авторы skill'ов либо узнают о труне-кейте описания постфактум (например, поиск по issue-tracker'у Anthropic), либо городят локальные правила. Конкретный пример: репо [`mazuninky/atl`](https://github.com/mazuninky/atl) держит две локальные копии под `.dq/rules/skill-*.yml` + sidecar `skill-evals.schema.json` ровно с этой логикой — и аналогичные правила нужны как минимум `dq` (этот репо тоже шипит `skill/SKILL.md`), и любому другому skill-репо.

dq уже имеет правильную форму для такого случая — namespaced `@std/<ecosystem>/` rulesets для устоявшихся внешних контрактов (k8s, dockerfile, github-actions, npm, openapi, terraform). Контракт Anthropic skill-loader'a — ровно такой же класс: стабильный, апстрим-задаваемый, нужен многим репо. Логично положить эти правила в `@std/skills/` и убрать дублирование на уровне dq core.

## What Changes

- **NEW `@std/skills` namespace** с двумя правилами (логика финализирована после сверки с [официальной spec'ой Anthropic skills](https://code.claude.com/docs/en/skills#frontmatter-reference)):
  - `skills.frontmatter` — markdown rule, проверяет фронтматтер `SKILL.md`. **Не требует** `name` / `description` (оба опциональны по спеке: `name` → directory name, `description` → first markdown paragraph). Валидирует только если поле присутствует: `name` должен матчить `^[a-z0-9][a-z0-9-]*$` (lowercase, digits, hyphens; **underscore запрещён**) и не превышать 64 символа; combined `description + when_to_use` ≤ 1,536 chars (per spec — после этого truncation в skill listing'е). Fires на markdown-файлах с frontmatter'ом; `match.glob` сознательно не используется (см. design.md — ограничение test-runner'a). False-positive risk минимален: Hugo/Jekyll blog с `title`/`date` не использует поля, которые мы проверяем.
  - `skills.evals-schema` — JSON Schema 2020-12 rule поверх `evals.json`-shape'a (`skill_name` + `evals[]`). Sidecar `evals.schema.json` embedded через тот же механизм, что `@std/jsonschema/helm-values-template.schema.json` и `@std/openapi/openapi-info.schema.json`. Shape-discriminating filter `has("skill_name") and has("evals")` гарантирует, что неродственные JSON файлы не валидируются. Контракт — informal Anthropic skill-creator convention, не публичная spec; правило это явно фиксирует в description.
- **NEW embedding for `skills` namespace** в [`crates/dq-lint/src/embed.rs`](../../../crates/dq-lint/src/embed.rs) — добавление в `NAMESPACES`, новые arms во всех 5 lookup-функциях (`std_ruleset`, `std_test_files`, `std_rule_files`, `std_schema`, `std_schema_files`), новые статики `SKILLS_RULES`, `SKILLS_TESTS`, `SKILLS_RULE_FILES`, `SKILLS_SCHEMA_FILES`.
- **README bump**: «64 standard rules across 8 namespaces» → «66 across 9 namespaces», добавление `skills` в перечень.
- **Anti-scope (явно НЕ входит):**
  - Community rules registry / `dq rules add github:...` — это M12, отдельный change. Сейчас распространение `@std/skills` только через статическое embedding в бинарь dq.
  - Проверка остальной структуры skill-репо (`scripts/`, `references/`, `evals/results/`) — слишком project-specific, оставляем за пределами `@std`. Авторы скилл-репо могут добавить локальные правила в `./.dq/rules/`.
  - Семантическая валидация значений в `evals[]` (например, что `expected_output` действительно описывает поведение CLI) — это работа для evals runtime, не для статического линтера.
  - Поддержка legacy skill-format (skill.yaml вместо SKILL.md) — Anthropic уже на SKILL.md, legacy не покрываем.

## Impact

- **Affected specs:** ничего не ломается — `data-query-exec` спек уже описывает `RuleSource::Std(&'static str)` как открытый список; добавление namespace'а не меняет публичный API.
- **Affected code:**
  - [`crates/dq-lint/src/embed.rs`](../../../crates/dq-lint/src/embed.rs) — +1 namespace во всех таблицах
  - [`crates/dq-lint/rules/skills/`](../../../crates/dq-lint/rules/skills/) — новая директория с 5 файлами (2 rule YAML, 2 test YAML, 1 schema JSON)
  - [`README.md`](../../../README.md) — счётчик namespaces и rule count
- **User-visible:**
  - `dq rules list` теперь показывает `@std/skills`.
  - `dq lint skill/SKILL.md` auto-bind'ит `@std/skills` благодаря markdown-overlap.
  - `dq rules add @std/skills` материализует оба правила + schema файл под `./.dq/rules/skills/`.
- **Downstream consumers** ([`mazuninky/atl`](https://github.com/mazuninky/atl) и сам этот репо `mazuninky/dq`): после релиза dq с этим change'ем удаляют локальные копии `skill-frontmatter.yml`/`skill-evals-schema.yml`/`skill-evals.schema.json` в пользу auto-bind'a из `@std/skills`. Никакого breaking — старые локальные копии продолжают работать, просто становятся избыточны.

## Reference

Источник для миграции — текущие atl-правила:
- <https://github.com/mazuninky/atl/blob/master/.dq/rules/skill-frontmatter.yml>
- <https://github.com/mazuninky/atl/blob/master/.dq/rules/skill-evals-schema.yml>
- <https://github.com/mazuninky/atl/blob/master/.dq/rules/skill-evals.schema.json>

Контракт skill-loader'a (folding description перед 1024-char truncation, `name` regex) описан в публичной документации Anthropic agent skills — мы зеркалим его как stable upstream contract.
