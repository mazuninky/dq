# Design — `@std/skills` namespace

## Why a namespace instead of `./.dq/rules/` copy-paste

Те же правила нужны минимум двум собственным репо (`mazuninky/dq`, `mazuninky/atl`) и понятно нужны любому стороннему скилл-репо. Промотать их через `@std` решает три проблемы:

1. Один источник правды для контракта Anthropic'овского skill-loader'a. Если они поднимут лимит description с 1,536 до, скажем, 2048 — фиксить в одном месте.
2. Distribution бесплатный: уже встроено в бинарь, не требует `dq rules add` или сторонней инфраструктуры.
3. Discoverability — `dq rules list` показывает namespace явно, типового потребителя не нужно учить, какой именно репо/файл откуда вендорить.

Альтернатива «git submodule из `mazuninky/dq-rules-skills`» отложена — community rules registry это M12 (см. [dq-plan.md:509](../../../dq-plan.md:509)), а пока механизма «package rules outside the binary» нет. Внутреннее embedding — единственный shipped механизм.

## Rule IDs and file names

Конвенция `@std/*` — `<namespace>.<rule-id>` ([crates/dq-lint/rules/markdown/frontmatter-required-fields.yml](../../../crates/dq-lint/rules/markdown/frontmatter-required-fields.yml) → `id: markdown.frontmatter-required-fields`). Поэтому:

| File | Rule ID |
|------|---------|
| `crates/dq-lint/rules/skills/frontmatter.yml` | `skills.frontmatter` |
| `crates/dq-lint/rules/skills/evals-schema.yml` | `skills.evals-schema` |
| `crates/dq-lint/rules/skills/evals.schema.json` | (sidecar, нет id) |

В atl правила назывались `atl.skill-frontmatter` / `atl.skill-evals-schema` — префикс `skill-` дублирует namespace, в core он redundant. Дроп.

## Rule logic — что валидируем, что НЕ валидируем

Первая итерация правила (миграция из atl as-is) содержала несколько багов, выявленных при сверке с [официальной спекой Claude Code skills](https://code.claude.com/docs/en/skills#frontmatter-reference). Финальная семантика после исправлений:

### `skills.frontmatter`

| Что проверяем | Источник | Severity |
|---|---|---|
| Если `name` присутствует — должен матчить `^[a-z0-9][a-z0-9-]*$` (lowercase, digits, hyphens) | Spec: "Lowercase letters, numbers, and hyphens only" | error |
| Если `name` присутствует — длина ≤ 64 chars | Spec: "max 64 characters" | error |
| Combined `description` + `when_to_use` (если есть) ≤ 1,536 chars | Spec: "combined `description` and `when_to_use` text is truncated at 1,536 characters" | error |

**Что НЕ проверяем (исправление багов первой итерации):**

- `name` и `description` обязательны: **нет**. Per spec, `name` опционален (fallback на directory name), `description` опционален (fallback на первый параграф markdown).
- Underscore в `name`: **нет** (был в atl regex `^[a-z0-9][a-z0-9_-]*$`). Spec явно запрещает.
- 1,024-char limit на `description` после `gsub("\\s+"; " ")`: **нет** (тоже из atl). Реальный cap — 1,536 на **combined** `description + when_to_use`. Whitespace folding не описан в спеке как часть truncation-логики; считаем raw post-YAML-parse length.
- Валидация типов / enum-значений остальных 13 полей фронтматтера (`disable-model-invocation`, `allowed-tools`, `model`, `effort`, `context`, `agent`, `hooks`, `paths`, `shell`, etc.): **отложена** до follow-up'a. Спека эволюционирует (особенно `model`/`effort` enum'ы); лучше оставить правила минимальными.

### `skills.evals-schema`

JSON Schema 2020-12 валидация поверх shape'a `{skill_name, evals[].{id, prompt, expected_output, assertions[].{text, type}}}` с `additionalProperties: false`. Source-of-truth — Anthropic-овский skill-creator плагин (см. dq's own `evals.json` подобный shape; формальной публичной спецификации evals.json нет, но Anthropic ships ровно этот shape во всех publicly-distributed skill repos).

Note: это **informal upstream convention**, не часть публичной skill spec. Description правила это явно фиксирует, чтобы downstream'ы понимали стабильность контракта.

## Auto-bind boundary — почему не используем `match.glob`

Изначально я планировал тайтнуть `match.glob: '**/SKILL.md'` и `'**/evals.json'` чтобы правила не цепляли неродственные markdown / JSON файлы при auto-bind'е по format-overlap'у. **Это не сработало** по причине ограничения test-runner'a:

`crates/dq-exec/src/test_runner.rs:308` — runner передаёт в `evaluator.evaluate_file()` путь самой `.test.yml` фикстуры, а не виртуальное имя типа `SKILL.md`. Поэтому `match.glob` промахивается на всех фикстурных кейсах → правило не срабатывает → 8 из 14 фикстур ломаются с "missing diagnostic".

Существующие `@std` правила (например, [`@std/jsonschema/helm-values-against-schema.yml`](../../../crates/dq-lint/rules/jsonschema/helm-values-against-schema.yml)) обходят это: в самих правилах `glob` отсутствует, описание лишь рекомендует пользователям добавить `glob` локально при override'е. Я следую той же конвенции.

**Trade-off:** правила фигурируют при auto-bind'е на всех markdown/json файлах в проекте. Mitigation:
- `skills.frontmatter` — `match.filter: '.frontmatter != null'` исключает markdown без frontmatter'a. Дополнительно: rule firings только на specific bad inputs (regex/length cap для `name`, 1,536-cap для description+when_to_use). Hugo/Jekyll-blog с `title`/`date`/`tags` не использует поля, которые мы проверяем, и тихо проходит.
- `skills.evals-schema` — `match.filter: 'has("skill_name") and has("evals")'` shape-discriminating. JSON файл без обоих ключей не валидируется. Это сильнее, чем glob, потому что охватывает любой путь.

**Follow-up:** добавить `path:` поле в `RuleTestCase` (`crates/dq-exec/src/test_runner.rs:44`) чтобы фикстуры могли симулировать виртуальный путь файла, и тогда `match.glob` станет тестируемым. Это отдельный change — не блокирует текущий.

## Schema sidecar embedding

Sidecar `evals.schema.json` — точно такой же класс, как `helm-values-template.schema.json` в `@std/jsonschema/` и `openapi-info.schema.json` в `@std/openapi/`. Embedding pattern уже отшлифован ([crates/dq-lint/src/embed.rs:687](../../../crates/dq-lint/src/embed.rs:687)):

```rust
static SKILLS_SCHEMA_FILES: &[(&str, &str)] = &[(
    "evals.schema.json",
    include_str!("../rules/skills/evals.schema.json"),
)];
```

И в `std_schema` / `std_schema_files` добавить `skills` arm. Никаких новых механизмов, точное переиспользование M11 Phase 3.

## Test fixtures

14 фикстур (9 для `frontmatter`, 5 для `evals-schema`). Существующий `cargo test -p dq-lint --test std_rulesets_pass` прогоняет их через `RuleTester` после того, как тест-функция `std_skills_fixtures_pass` была добавлена в [`crates/dq-lint/tests/std_rulesets_pass.rs`](../../../crates/dq-lint/tests/std_rulesets_pass.rs).

### Известный edge case при автогенерации фикстур

Quoted YAML scalar (`"AAA..."`) сохраняет длину строки литерально — не сворачивает whitespace. Это удобно для тестов на длину, но требует точного подсчёта символов. При первой версии фикстуры на «oversized description» содержали 1,240 символов вместо ≥1,536, поэтому правило не фейлило (а это правильный результат для 1,240 < 1,536). Финальные фикстуры используют python-сгенерированные строки 1,700 / 900+900 chars для гарантированного overflow'a.

## Anti-scope decisions

- **Не делаем правило на «SKILL.md должен существовать в skill/»** — формат skill-репо разнообразный (плагины Claude Code, system-prompts, и т.д.); requiring SKILL.md выходит за scope `@std`.
- **Не делаем правило на references/, scripts/ обязательности** — это бизнес-конвенция конкретного автора, не upstream-контракт.
- **Не валидируем enum-значения `model`/`effort`/`context`** — эти enum'ы эволюционируют чаще, чем сам skill spec. Лучше fallback на runtime validation в loader'е.
- **Не парсим SKILL.md description на пустоту/качество** — слишком субъективно; rule-author может добавить через локальное правило с jq.
- **Не валидируем `evals[].files[]` пути** — это runtime concern.
