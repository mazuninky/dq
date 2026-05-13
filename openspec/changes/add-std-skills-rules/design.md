# Design — `@std/skills` namespace

## Why a namespace instead of `./.dq/rules/` copy-paste

Те же правила нужны минимум двум собственным репо (`mazuninky/dq`, `mazuninky/atl`) и понятно нужны любому стороннему скилл-репо. Промотать их через `@std` решает три проблемы:

1. Один источник правды для контракта Anthropic'овского skill-loader'a. Если они поднимут лимит description с 1024 до, скажем, 2048 — фиксить в одном месте.
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

## Auto-bind boundary — `match.glob` constraint

Auto-bind в dq работает по format overlap ([crates/dq-exec/src/loader.rs:132](../../../crates/dq-exec/src/loader.rs:132)): `@std/skills` подтягивается если в discovered_formats есть `markdown` или `json` (то есть в дефолтной массе случаев). Без дополнительного фильтра `skills.frontmatter` цеплял бы каждый блог-пост с frontmatter, а `skills.evals-schema` — потенциально `package.json`'ы (мы отфильтровываем shape через `match.filter`, но evaluation всё равно происходит на каждом файле).

Решение: тайтнем `RuleMatch.glob` ([crates/dq-exec/src/rule.rs:149](../../../crates/dq-exec/src/rule.rs:149)):

- `skills.frontmatter`: `glob: '**/SKILL.md'` — фронтматтер-проверка только на канонично-названных файлах.
- `skills.evals-schema`: `glob: '**/evals.json'` — schema-проверка только на файлах с именем `evals.json` (стандартная локация — `skill/evals/evals.json`).

Это аналог того, как `@std/dockerfile` бы цеплял только `Dockerfile`/`*.dockerfile` через format detection: glob — это explicit narrowing для рулсетов, чьи правила технически могут читать любой файл соответствующего формата.

Если у кого-то путь не канонический (`skill/Skill.md`, `evals/data.json`) — правило не сработает auto-bind'ом, но можно явно: `dq lint --rules @std/skills <path>`. Эту особенность зафиксируем в rule description.

## Description-length check — folded scalars

Anthropic skill-loader делает `' '.join(description.split())` перед truncation на 1024 — то есть folding запускающихся пробельных run'ов в один space. Если в SKILL.md фронтматтер описан как folded scalar (`description: >`), YAML парсер уже сделает эту нормализацию; но если автор написал block-scalar literal (`description: |`) с newline'ами, fold не произойдёт, а лоадер всё равно фолданёт.

Поэтому в jq:

```jq
($fm.description // "") | tostring | gsub("\\s+"; " ") | length
```

— мы зеркалим folding-семантику лоадера независимо от выбора скаляр-стиля автором. Это делает правило корректным для обоих случаев и единообразным с тем, что делает upstream.

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

Тесты у atl уже хорошие (5 случаев для frontmatter, 5 для evals). Мигрируем 1:1, меняя только rule id в `expected.violations[].rule`. Existing `cargo test -p dq-lint` тест-раннер автоматически подхватит новые `*.test.yml` через `std_test_files("skills")` (см. [crates/dq-lint/tests/](../../../crates/dq-lint/tests/)).

Один edge case: тест с oversized description у atl использует 1500 chars в quoted scalar — это сохраняем, потому что YAML folded-block parser quirks в комментарии у atl были выяснены опытным путём.

## Anti-scope decisions

- **Не делаем правило на «SKILL.md должен существовать в skill/»** — формат skill-репо разнообразный (плагины Claude Code, system-prompts, и т.д.); requiring SKILL.md выходит за scope `@std`.
- **Не делаем правило на references/, scripts/ обязательности** — это бизнес-конвенция конкретного автора, не upstream-контракт.
- **Не делаем `keyword`-проверку из новой Anthropic skill spec** — если она появится в апстриме как обязательное поле, добавим в follow-up; сейчас опциональное.
- **Не парсим SKILL.md description на пустоту/качество** — слишком субъективно; rule-author может добавить через локальное правило с jq.
