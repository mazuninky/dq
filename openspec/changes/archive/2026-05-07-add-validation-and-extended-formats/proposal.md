## Why

После M10 у `dq` есть rule engine, fixer и WASM-plugin runtime, но в покрытии остаются три зияющие дыры, которые блокируют типичные production-юзкейсы и закрывают milestone M11 ([dq-plan.md:486](../../../dq-plan.md:486)):

1. **JSON Schema validation отсутствует.** Команды-soft-spot: `dq lint values.yaml --schema values.schema.json` (Helm), валидация k8s-CRD, OpenAPI request/response. Сейчас пользователь вынужден ставить отдельный `ajv-cli`/`check-jsonschema` рядом с `dq` — расходится с anti-scope «один бинарь под все DevOps-данные».
2. **Composite rules невозможны.** Правило в одном формате не может извлечь подстроку, распарсить её другим парсером и заявить нарушения с правильными координатами. Конкретные кейсы: yaml-code-blocks в markdown должны быть валидным YAML; `package.json` `scripts.*` должны ссылаться на существующие команды; Helm `values.yaml` должен соответствовать `values.schema.json`. Без этого `@std/markdown` ловит только syntactic-проблемы, а content-валидация выводится наружу.
3. **Inline-level позиции и XML — последние «звёздочки в anti-scope».** README §Status явно перечисляет «inline-level position spans (M11), XML write (M11+)» как отложенное. Inline-spans нужны диагностикам внутри multiline-strings (yaml-block scalar, markdown code block) — без них SARIF указывает на начало блока, а не на конкретную строку. XML read+write закрывает последний из «классических» config-форматов (Maven/Spring/Tomcat/Android).

Решаем все три блока в одном change'е, потому что они **не независимы**: composite-rules используют inline-spans для координат вложенного парсинга; JSON Schema-rule семантически — частный случай composite (input → schema-validator); XML — следующий формат, на котором эти механизмы естественно проверяются (Maven `pom.xml` валидируется по XSD как composite-кейс). Делать раздельно — три раза рефакторить границу `dq-exec` ↔ `dq-core::ir`.

## What Changes

- **NEW JSON Schema rule type в `dq-exec`.** `Rule.check` получает альтернативу `jq:` — `schema:` (inline JSON Schema 2020-12 как YAML-подобъект) и `schema_file:` (путь к `.schema.json` рядом с правилом). Валидация через crate `jsonschema` 0.34+. Каждая schema-violation мапится в один `Diagnostic` с `loc.pointer` = `instancePath` ошибки (RFC 6901 совпадает с нашим Pointer'ом 1:1). Ruleset `@std/jsonschema/{kubernetes-crd,helm-values,openapi-3.1}.yml` поставляются как референс.
- **NEW composite-rule механизм в `dq-exec`.** `Rule` получает опциональную секцию `extract:` (jq-выражение, возвращающее массив `{value: <string>, format: <"yaml"|"json"|"toml"|...>, anchor: <Pointer>}`); evaluator парсит каждое извлечённое значение указанным форматом, прогоняет на нём вложенный `nested:` блок (любые `check`/`schema`/`extract` рекурсивно), а возвращённые `Diagnostic` репроецирует на координаты исходного файла через **anchor + inline-span**. Идемпотентность: рекурсия ограничена `depth = 4` (config через `Evaluator::with_max_extract_depth`); при превышении — `ExecError::CompositeDepthExceeded`.
- **Inline-level position spans в `dq-core::ir`.** `Provenance` дополняется полем `inline_offset: Option<InlineOffset>` где `InlineOffset = { byte_start: usize, line_offset: u32, col_offset: u32 }` — относительный сдвиг внутри родительского scalar-узла. Парсеры маркируют scalars, чьё содержимое — multiline-blob (YAML block scalar `|`/`>`, markdown fenced code block, JSON string с `\n`); composite-rule evaluator берёт inline-offset нарушения из вложенного parse и складывает с absolute-position anchor'а. **BREAKING (internal)**: `Provenance::span` сигнатура расширена — внешние плагины через WIT не затронуты (используют `format_tag` host-call).
- **NEW Terraform/HCL ruleset (`@std/terraform`).** 8–10 правил поверх существующего HCL-парсера (M5): `no-hardcoded-secrets`, `tag-required`, `provider-pinned`, `no-public-ingress`, `state-backend-required`, `module-pin-version`, `output-no-sensitive-without-flag`, `variable-has-description`. Без новых формат-зависимостей — HCL уже в `format-support`.
- **NEW OpenAPI ruleset (`@std/openapi`).** 6–8 правил для OpenAPI 3.0/3.1 (формат — обычный YAML/JSON, валидация через типизированную модель `oas3` 0.16+ как composite-rule под капотом): `info-required-fields`, `paths-non-empty`, `operation-id-unique`, `response-200-or-201-required`, `no-trailing-slash`, `security-defined`, `schema-no-additional-properties-true`. Зависимость на `oas3` — feature-gated `--features openapi`.
- **XML read + write в `format-support`.** Новый `XmlFormat` через `quick-xml` 0.36 (event-based, поддерживает round-trip атрибутов и порядка узлов). Round-trip контракт: comments, CDATA, processing-instructions, namespace prefixes сохраняются; whitespace-only text-nodes сохраняются как opaque; mixed-content (текст вперемежку с элементами) — opaque (явно документировано как **partial round-trip** в `XmlFormat::write`). Маппинг XML→`Document::Value`: элемент → object с conventional ключами `@attr` (атрибуты), `#text` (содержимое), `<children>` (массив дочерних). XSD-валидация — НЕ в scope (отдельный `@std/xml-xsd` change позже, нужен отдельный crate).
- **Anti-scope (твёрдая граница):**
  - НЕ добавляем JSON Schema **draft-07/2019-09** — только 2020-12 (последний стабильный). Правила, ссылающиеся на старые draft, должны заявить `$schema` явно — `jsonschema` crate сам обработает.
  - НЕ строим polyglot-Document, объединяющий XML и key-value-форматы. `Document::Value` остаётся как сейчас; XML мапится в существующую модель с conventional keys.
  - НЕ делаем XSD-валидацию, RelaxNG, Schematron, OpenAPI request/response **runtime**-валидацию (это middleware, не lint).
  - НЕ расширяем jaq для работы с inline-spans — composite-rules используют inline-spans **после** jq-evaluation, во время map-back в Diagnostic.
  - НЕ добавляем CUE/EDN/Jsonnet/HOCON/nginx/SPDX/TextProto/VCL — anti-scope из [dq-plan.md:600-612](../../../dq-plan.md:600).

## Capabilities

### New Capabilities

- `data-query-jsonschema`: типизированный `schema:` / `schema_file:` блок в `Rule.check`, маппинг JSON Schema 2020-12 violations в `Diagnostic` через `instancePath ↔ Pointer`, рендеринг `keywordLocation` в message, поставляемые ruleset'ы `@std/jsonschema/*`.
- `data-query-composite-rules`: схема `extract:` + `nested:` в Rule, рекурсивный evaluator с max-depth, репроекция inner-Diagnostic координат через anchor + inline-offset, обработка parse-ошибок вложенного формата (mapped в `severity: error` нарушения с `rule_id = <outer>.parse-failed`).

### Modified Capabilities

- `data-query-ir`: `Provenance` расширяется опциональным `inline_offset` для multiline scalars; контракт parser'ов — yaml-block-scalar и markdown fenced-code-block ОБЯЗАНЫ выставлять inline-baseline, остальные — best-effort. Существующие потребители (`Diagnostic.line/col`) backward-compatible — `None` означает «нет inline-смещения», логика та же что сейчас.
- `data-query-exec`: `Rule` schema добавляет `check.schema`/`check.schema_file`/`extract`/`nested`; `Evaluator::evaluate_file` валидирует mutual-exclusion `jq` ↔ `schema` ↔ `extract` (ровно один из трёх в `check`); `Reporter`-payload не меняется (новые поля внутри Diagnostic — backward-compatible).
- `data-query-rules`: рост standard-rule-count contract'а: `@std/terraform` (≥8 правил), `@std/openapi` (≥6 правил), `@std/jsonschema` (≥3 референс-правила); `list_std_rulesets()` возвращает три новых namespace'а.
- `format-support`: добавляется XML read+write через `quick-xml`; формальное снятие deferred-flag «XML write» из спеки M5; auto-detection по расширению `.xml`; `--format xml` принимается. Round-trip контракт XML — **partial** (comments/CDATA/PI/namespaces сохраняются; mixed-content — opaque), явно документировано в spec'е.

> Не модифицируются на spec-уровне: `data-query-write` (CLI-семантика `dq set`/`dq del` для XML наследуется от Format trait), `data-query-edit-ops` (EditOp работают над `Value`, агностичны к формату), `data-query-plugin-abi` (WIT-схема не меняется; плагины не получают inline-offset в этом change'е, отложено в M12+).

## Impact

**Затронутые крейты:**
- `dq-core` — расширение `ir/provenance.rs` под `inline_offset`; парсеры YAML и markdown code-block устанавливают inline-baseline; новый модуль `format/xml.rs`.
- `dq-exec` — новые модули `rule/schema_check.rs`, `rule/composite.rs`; `Rule` enum расширяется; `Evaluator` получает recursion-budget; новый `ExecError::CompositeDepthExceeded`, `ExecError::SchemaCompile`, `ExecError::NestedParseFailed`.
- `dq-lint` — три новых namespace'а под `crates/dq-lint/rules/{terraform,openapi,jsonschema}/` с `*.test.yml` фикстурами; `list_std_rulesets()` обновлён.
- `dq-cli` — handler-уровневых изменений нет (Rule extensibility капсулирована в `dq-exec`); `dq lint`/`dq fix`/`dq test` работают с новыми правилами без изменений.
- `dq-transform` — без изменений (composite-rules используют существующий jaq adapter для `extract:` jq-выражений).

**Новые runtime-зависимости:**
- `jsonschema` 0.34+ (lib-only, без CLI features) — JSON Schema 2020-12 validator. Без feature-gate (small bin impact, core selling point).
- `oas3` 0.16+ — typed OpenAPI model. Feature-gate `--features openapi` (default ON; пользователи минимального бинаря отключают).
- `quick-xml` 0.36+ (с фичей `serialize`) — XML парсер. Без feature-gate (parity с `serde_json`/`toml_edit`).

**Совместимость:**
- CLI surface не меняется. Новые правила — opt-in через `--rules @std/terraform` и т.д., либо через auto-discovery когда `dq lint *.tf`.
- Rule schema BREAKING на уровне internal contract: `check` теперь `oneOf [jq, schema, schema_file, extract+nested]`. Существующие правила (все имеют только `check.jq`) парсятся без изменений; serde validation добавит mutual-exclusion check.
- IR `Provenance` ABI не stable публично — изменение non-BREAKING для downstream consumers.
- Plugin WIT (data-query-plugin-abi) НЕ меняется в этом change'е.

**Risk:**
- `oas3` crate — менее зрелый чем `serde_json`/`jsonschema`; mitigated тем что используется только под `--features openapi` для OpenAPI ruleset.
- XML round-trip — `quick-xml` event API похож на `saphyr-parser` event-pattern, который мы уже освоили в M2; spike не нужен.
- composite-rule recursion — DoS-vector если `extract` возвращает self-similar контент. Mitigation — hardcoded `max_depth=4`, configurable только через builder API (не через rule YAML).
