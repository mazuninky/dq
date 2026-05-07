## Context

После M10/IR foundation у `dq` есть:
- `dq-core::ir::{Ir, OwnedIr, Provenance, ProvenanceMap, FormatTag}` ([data-query-ir spec](../../specs/data-query-ir/spec.md))
- `dq-exec::Rule` со схемой `match`/`check.jq`/`message`/`severity`/`loc`/`fix.jq` ([data-query-exec spec](../../specs/data-query-exec/spec.md))
- `dq-lint` с четырьмя namespace'ами `@std/{k8s,dockerfile,npm,github-actions}` ([data-query-rules spec](../../specs/data-query-rules/spec.md))
- WASM-plugin runtime через WIT/wasmtime — стабильный ABI на `data-query-plugin-abi`
- Format-support: YAML/JSON/TOML round-trip, JSONL/TOON write, M5-форматы (HCL/INI/.env/CSV/Dockerfile/Frontmatter), markdown-tree (M9)

M11 — последний milestone до community-registry M12 — добавляет три блока:

1. **JSON Schema 2020-12** как тип Rule, не отдельная команда. План: [dq-plan.md:486-490](../../../dq-plan.md:486).
2. **Composite-rules** — рекурсивная композиция парсеров для cross-format валидации. План: [dq-plan.md:492-493](../../../dq-plan.md:492).
3. **Extended formats** — Terraform (HCL уже есть в M5, нужен ruleset), OpenAPI (через `oas3`), XML (новый формат, последний из «классических» config-форматов). План: [dq-plan.md:494](../../../dq-plan.md:494).

Дополнительно из README anti-scope: **inline-level position spans** — для точных координат внутри multiline-scalars (yaml block scalar, markdown code block, JSON-string с `\n`).

**Constraints:**
- IR-контракт span-propagation уже зацементирован — расширение Provenance должно быть backward-compatible.
- `data-query-plugin-abi` стабильна на v0.1.0 — inline-spans **не** прокидываются в WIT в этом change'е.
- Rule schema parsing — `serde(deny_unknown_fields)`; новые поля требуют явного оптинного парсинга.
- Standard rule library count contract в [data-query-rules](../../specs/data-query-rules/spec.md) растёт; цифры обновляются через MODIFIED Requirement.

**Stakeholders:**
- AI-агенты в CI/CD — основные потребители JSON Schema (стандартный механизм валидации манифестов) и SARIF-вывода с точными координатами.
- DevOps-инженеры — Terraform/OpenAPI rulesets, XML для legacy-стека (Maven, Spring, Tomcat).
- Rule-авторы (внутренние) — composite-rule API должно быть достаточно простым, чтобы написать «yaml в md code-block должен быть валидным» правило за <30 строк YAML.

## Goals / Non-Goals

**Goals:**
- Закрыть три M11-блока в одном change'е, потому что они разделяют единый рефактор `Rule.check` (oneOf) и `Provenance` (inline-offset).
- Сохранить backward-compatibility всех существующих правил (`@std/k8s`, `@std/npm`, `@std/github-actions`, `@std/dockerfile`, `@std/markdown`) — никаких касаний кроме автоматической переадресации `check.jq` → `Check::Jq`.
- JSON Schema rule даёт user-facing value за минимум кода — composite-rule под капотом не обязателен (хотя schema-rule **технически** один из видов composite, упрощённый API наружу).
- Inline-spans — opt-in для парсеров: YAML block scalars и markdown fenced-code-blocks ОБЯЗАНЫ выставлять; остальные — best-effort, `None` всегда валидно (existing callers не ломаются).
- XML round-trip — best-effort, явно документирован как **partial** (mixed-content opaque), чтобы не повторять M2-spike.

**Non-Goals:**
- НЕ реализуем XSD / RelaxNG / Schematron / OpenAPI request-runtime-валидацию.
- НЕ строим polyglot-Document, объединяющий XML-tree и key-value-`Value`. XML мапится в существующую `Value` через conventional keys.
- НЕ расширяем `data-query-plugin-abi` WIT в этом change'е — inline-offset в плагины откладываем (отдельный change в M12+ при необходимости).
- НЕ форкаем jaq, не трогаем `data-query-transform` adapter — composite-rules используют jq поверх ProvenanceMap, без изменения существующего value-bridge.
- НЕ embed-им JSON Schema в jaq evaluator — schema-validation стоит отдельным шагом в `Evaluator`, не как jq-функция.
- НЕ добавляем `@std/xml-xsd` — XSD в отдельный change позже.

## Decisions

### D1: `Rule.check` становится `oneOf [jq | schema | schema_file | extract+nested]` через serde-untagged enum

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    // ... existing fields ...
    check: Check,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, untagged)]
enum Check {
    Jq { jq: String },
    Schema { schema: serde_yml::Value },
    SchemaFile { schema_file: Utf8PathBuf },
    Composite {
        extract: String,           // jq returning array of {value, format, anchor}
        nested: Box<Rule>,          // recursively typed
    },
}
```

**Why untagged enum vs explicit tag:** существующие правила имеют только `check.jq` без discriminator-поля. `serde(untagged)` позволяет файлам со старым shape парситься без изменений. Цена — менее понятные ошибки парсинга при опечатках (serde пробует все варианты по очереди); mitigated через custom `Deserialize` impl на уровне `Rule`, который сначала смотрит на присутствующие ключи и эмитит точечную ошибку («Rule check must contain exactly one of: jq, schema, schema_file, extract+nested»).

**Alternatives considered:**
- (a) Tagged enum с `check.kind: jq|schema|composite` — ломает все существующие правила (обязательный новый ключ), нужен migration script. Отклонено: M10 stable contract.
- (b) Boxed `dyn CheckImpl` — runtime polymorphism, не serde-natural, сложнее тестировать. Отклонено: enum достаточен.
- (c) Top-level `kind:` поле на Rule — работает, но раздувает schema каждого правила; тот же breaking change что (a). Отклонено.

### D2: JSON Schema валидация — crate `jsonschema` 0.34+, validator-per-rule компилируется один раз при `Evaluator::new`

```rust
struct CompiledSchemaCheck {
    validator: jsonschema::Validator,  // owned, compiled at Evaluator::new
}

impl CompiledSchemaCheck {
    fn validate(&self, ir: &Ir<'_>) -> Vec<Diagnostic> {
        let json = ir.to_serde_json();  // existing converter
        self.validator
            .iter_errors(&json)
            .map(|err| Diagnostic {
                rule_id: ...,
                severity: Severity::Error,
                pointer: Pointer::parse(&err.instance_path.to_string()).unwrap(),
                line, col,  // looked up via ir.span_for(&pointer)
                message: format!("{}: {}", err.keyword_location, err),
                ...
            })
            .collect()
    }
}
```

**Why `jsonschema` 0.34+:** zero unsafe, активно maintained, поддержка 2020-12 (последний draft), 100% test-suite coverage официальных draft-tests, lib-only (без CLI/HTTP-резолверов по умолчанию). Альтернативы:
- `boon` (другой rust-native validator) — меньше contributors, медленнее обновляется.
- `valico` — abandoned, последний релиз 2022.
- shell-out на `check-jsonschema` (Python) — нарушает single-binary anti-scope.

**`schema_file:`** разрешается относительно директории правила (не CWD); абсолютные пути запрещены — это локальный resource, не arbitrary FS read. Validator кэшируется в `RuleSet::compile` и переиспользуется для каждого файла; повторная компиляция per-file была бы O(M*N) hot path.

**Mapping `instancePath` → координаты:** `jsonschema` возвращает RFC 6901 path в `error.instance_path`. У нас `Pointer` тоже RFC 6901 — биективный маппинг `Pointer::parse(&path.to_string())`. `line`/`col` берутся через существующий `Ir::span_for(&pointer)`; если span отсутствует (synthetic node) — fallback `line=1, col=1` (как `loc.line` jq-fallback в M10).

### D3: Composite-rule API — `extract` jq-выражение возвращает массив `{value, format, anchor}`, `nested` рекурсивно типизирован

```yaml
id: markdown.code-blocks-yaml-valid
match:
  format: markdown
check:
  extract: |
    .. | objects | select(.type == "code") | select(.lang == "yaml") |
    {value: .literal, format: "yaml", anchor: .pointer}
  nested:
    id: markdown.code-blocks-yaml-valid.inner
    match:
      format: yaml
    check:
      jq: 'true'  # parse-only; YAML parse failure auto-emits diagnostic
message: "YAML code block is invalid: {{message}}"
```

**Контракт `extract`:** jq-выражение OBLIGED вернуть **массив** объектов с тремя полями:
- `value`: string — байты для повторного парсинга
- `format`: string — name из `FormatTag` (yaml/json/toml/...)
- `anchor`: string — RFC 6901 pointer на родительский scalar в исходном файле

`extract` возвращающее non-array — `ExecError::CompositeExtractNotArray`. Объект без хотя бы одного из трёх полей — `ExecError::CompositeExtractMalformed { missing_field }`.

**Координаты вложенного диагноза:** anchor → `Ir::span_for(&anchor_pointer)` даёт `(line, col)` в исходнике. Inner-Diagnostic несёт inline-offset из `ir.provenance_for(...).inline_offset`. Финальные координаты:
```
final_line = anchor.line + inner.line - 1   // both 1-based
final_col  = if inner.line == 1 { anchor.col + inner.col - 1 } else { inner.col }
```

**Recursion bound:** hardcoded `MAX_EXTRACT_DEPTH = 4`, configurable через `Evaluator::with_max_extract_depth(n)` (только для тестов и debug). При превышении — `ExecError::CompositeDepthExceeded { rule_id, depth }`. Это защита от self-similar extract (`extract: '. | {value: ., format: "json", anchor: ""}'` бесконечно матчит сам себя).

**Inner-rule parse failure:** если `Format::parse(value)` падает — это нарушение **outer-rule**, не silent skip. Эмитим `Diagnostic { rule_id: "<outer>.parse-failed", severity: Error, message: "<inner format> parse failed: <parse-error>", line: anchor.line, col: anchor.col }`. Это ловит «yaml-block в md невалиден» как первый класс violation, без необходимости прописывать это явно.

**Alternatives considered:**
- (a) `extract` как DSL вместо jq — proprietary, не консистентно с остальной системой. Отклонено: jq уже есть.
- (b) `nested` как имя другого правила (cross-reference) — даёт композицию, но требует stable rule-id resolver и усложняет circular-detection. Отклонено: inline проще, ничего не теряем.
- (c) Параллельная схема `validate.format` без recursion — справляется только с тривиальными случаями (parse-only check). `nested: <Rule>` даёт полноценную композицию: vault-of-secrets check'и в OpenAPI rec-spec'ах, k8s-CRDs внутри Helm templates, и т.д.

### D4: `Provenance` расширяется опциональным `inline_offset` — backward-compatible

```rust
pub enum Provenance {
    Original {
        pointer: Pointer,
        span: Option<ValueSpan>,
        inline_offset: Option<InlineBaseline>,  // NEW
    },
    Synthetic { reason: SyntheticReason },
}

pub struct InlineBaseline {
    /// Byte offset within the parent scalar's content where this node's content begins.
    /// `0` for the first character; for YAML block scalars this is `0` (after the indicator+newline).
    pub byte_start: usize,
    /// Line number within the parent scalar's content (1-based).
    pub line: u32,
    /// Column number within the parent scalar's content (1-based).
    pub col: u32,
}
```

**Why opt-in:** существующие `Original { pointer, span }` callsite-ов несколько десятков; миграция на `inline_offset: None` — массовый change. Solution: change `Original` на struct-variant с явным полем; serde/PartialEq derive продолжают работать; callers, конструирующие `Original` вручную (тестовые helpers), получают compile-error и обновляются на `..Default::default()` shorthand. `Display`/`Debug` impl-ы `Provenance` обрабатывают `inline_offset: None` как «не показывать».

**Кто populating'ит inline_offset:**
- **YAML block scalars** (`|`, `>`, `|-`, `>-`) — saphyr-parser отдаёт scalar с `block_indicator` атрибутом; парсер выставляет `inline_offset = Some(InlineBaseline { byte_start: 0, line: 1, col: 1 })` потому что после indicator+newline контент начинается с line 1, col 1. **Обязательно**.
- **Markdown fenced code blocks** — comrak отдаёт `Node::CodeBlock` с `info` (lang) и `literal` (содержимое); парсер выставляет inline-baseline. **Обязательно**.
- **JSON strings с `\n`** — best-effort: парсер JSON в M2 уже возвращает span на open-quote позиции; для escape-sequence `\n` в строке — inline_offset выставляется только если строка попадает на extract в composite-rule (lazy population). **Best-effort**.
- **Все остальные парсеры** — `inline_offset = None`. Composite-rule работает, просто координаты вложенного диагноза показывают anchor-position без line-внутри-строки precision.

**Contract test:** new spec scenario «extract on YAML block scalar produces line-offset-aware diagnostic» — обязательная test для propagation корректности.

### D5: XML формат — `quick-xml` 0.36+, маппинг `Element → Map { "@attrs": Map, "#text": String, <tag>: Array<Element> }`

```rust
// <user id="42"><name>Alice</name><email>a@x</email></user>
// →
// {
//   "@attrs": { "id": "42" },
//   "name": [{ "#text": "Alice" }],
//   "email": [{ "#text": "a@x" }]
// }
```

**Почему conventional keys, а не отдельный `Value::Element` variant:** добавление variant'а — BREAKING change всему `dq-core::Value` API; всё что pattern-match'ит по `Value` (jaq adapter, EditOp dispatcher, formatters) должно быть обновлено. Conventional keys — zero-cost: XML мапится в обычный `Map`, существующие команды (`get`, `set`, `lint`) работают без изменений, `Format::write` собирает обратно по тем же conventional keys.

**Round-trip контракт XML — partial:**
- ✅ Element-tree structure (порядок дочерних элементов через IndexMap; multi-child одного тега через массив)
- ✅ Атрибуты (через `@attrs`)
- ✅ Comments (специальный ключ `#comments` — массив строк, прикреплённый к содержащему элементу)
- ✅ CDATA (через ключ `#cdata` — список pre-existing CDATA-блоков; на write эмитятся в позиции элемента)
- ✅ Processing instructions (`#pi`)
- ✅ XML declaration (top-level `#xml` — версия/encoding)
- ✅ Namespace prefixes (часть имени тега: `xmlns:foo` атрибут + `foo:tag` элемент сохраняются текстуально)
- ❌ Mixed content — текст между элементами (`<p>Hello <b>world</b>!</p>`) — opaque: всё содержимое `<p>` сериализуется в `#text`, теряется позиция `<b>`. Это документировано как known limitation; rule-авторы валидируют XML без mixed-content (config-формат kind, не markup).
- ❌ Whitespace-significant pretty-printing — на write всегда compact с newline-после-закрывающих-тегов. Round-trip `parse → write` НЕ byte-identical для прости-формата с indented children. Это **intentional** контракт `XmlFormat::write` (документировано в spec).

**Alternatives considered:**
- (a) `quick-xml` с serde — даёт декларативный де/сериализатор, но плохо подходит для unknown-shape XML (config-formats, чьи теги мы не знаем заранее). Отклонено: event-based mode подходит лучше для ad-hoc XML.
- (b) `xmltree` или `roxmltree` — read-only, нет write API. Отклонено.
- (c) Отдельный `Value::Element` variant — см. выше, BREAKING. Отклонено.

### D6: `oas3` под feature-gate `--features openapi`, default ON

`oas3` 0.16 — менее зрелый чем `serde_json`/`jsonschema` (4 contributors, последний релиз ~3 месяца). Минимальный бинарь должен иметь возможность сжаться без него:

```toml
[features]
default = ["openapi", "embedded-jq", ...]
openapi = ["dep:oas3"]
```

`@std/openapi/*.yml` правила компилируются в бинарь только при `cfg(feature = "openapi")`; если feature off — `dq lint` на OpenAPI-файле не выдаёт `@std/openapi/*` правил (но и не падает — просто пустой ruleset для этого namespace).

**JSON Schema иначе:** `jsonschema` — central selling point, не feature-gated.

**Implementation note (Phase 5):** реализация выбрала no-`oas3` путь — все 6 OpenAPI правил написаны через jq + JSON Schema (Phase 3 machinery) поверх стандартного YAML/JSON парсера. `oas3` dependency не добавлен, feature flag `openapi` не введён, `@std/openapi` namespace ships unconditionally. Это упростило тулинг (тот же authoring-flow как у `@std/k8s` / `@std/jsonschema`), убрало conditional compilation и дало deferment'а typed-OpenAPI-валидации на отдельный change (M12+). Контракт «≥6 правил в `@std/openapi`» выполняется через jq/schema семантику, а не через typed `oas3::Spec` model.

### D7: Recursion-depth для composite — `MAX_EXTRACT_DEPTH = 4` hardcoded

Эмпирически 4 уровня покрывают:
- yaml в md (1)
- yaml в helm-template в yaml в md (3)
- безумное (4)

Hardcoded в const, configurable только через `Evaluator::with_max_extract_depth(n)` для unit-тестов (`#[cfg(test)]` или test-only feature). НЕ exposed через rule YAML — пользователь не должен бороться с лимитом, max-depth — invariant runtime'а.

При превышении — `ExecError::CompositeDepthExceeded { rule_id, depth, max }`, exit code GENERIC=1 (не PARSE_ERROR=3, потому что это semantic violation engine'а, не parse).

### D8: Standard rule count contract — растёт через MODIFIED Requirement

`data-query-rules` spec'е сейчас формальное «≥40 правил в 4 namespace'ах». M11 добавляет:
- `@std/jsonschema` ≥ 3 референс-правил (`kubernetes-crd-shape`, `helm-values-against-schema`, `openapi-3.1-shape`)
- `@std/terraform` ≥ 8 правил
- `@std/openapi` ≥ 6 правил (только при `cfg(feature = "openapi")`)

Total contract после M11: ≥ 57 правил (40 + 3 + 8 + 6) если все features ON; ≥ 51 если openapi off. Spec'е MODIFIED Requirement формулирует это с conditional clause.

## Risks / Trade-offs

[**Risk: composite-rule infinite recursion**] → Mitigation: hardcoded MAX_EXTRACT_DEPTH=4 (D7), cycle-detection не нужен (depth-limit достаточен).

[**Risk: `oas3` crate quality**] → Mitigation: feature-gated (D6), can be turned off без ущерба остальному; OpenAPI rules — самый отделимый блок, easy to defer если crate ломается.

[**Risk: XML mixed-content data loss on round-trip**] → Mitigation: документировано как known limitation; smoke-test эмитит `tracing::warn!` на parse mixed-content XML (не error — пользователь видит warning, что round-trip может быть lossy для этого файла).

[**Risk: `inline_offset` миграция ломает test-helpers**] → Mitigation: добавляем `Provenance::original(pointer, span)` constructor с default `inline_offset: None`; existing tests мигрируют через find/replace на конструктор.

[**Risk: JSON Schema 2020-12 unsupported $ref schemes**] → Mitigation: `jsonschema` crate валидирует только встроенные `$ref` (по `$id`); HTTP/file-loaded $ref требуют explicit registry, который мы не настраиваем — schema-rule валидируется в isolation, без сетевых походов. Документируется в spec'е как «$ref только internal».

[**Risk: Rule.check serde untagged enum даёт плохие ошибки**] → Mitigation: custom `Deserialize` impl на уровне `Rule` (D1) с явной проверкой mutual-exclusion и точечной ошибкой.

[**Risk: composite-rule координаты неточные на JSON-string с `\n`**] → Mitigation: best-effort для JSON (D4); документировано; YAML block scalars и markdown code blocks (главные use cases) — обязательны и точные.

[**Risk: `data-query-plugin-abi` consumers ожидают inline_offset через WIT**] → Mitigation: WIT не меняется в этом change'е; explicit non-goal. Future change в M12+ может extend WIT — версионирование схемы это покрывает (`dq:plugin@0.2.0`).

## Migration Plan

**Sequence (single PR per phase, integration в порядке):**

1. **Phase 1: XML format** — изолированный, не зависит от others; добавление `XmlFormat` + format-detection + golden-snapshots round-trip. Можно ship'ить независимо.
2. **Phase 2: `Provenance::inline_offset` + парсеры** — расширение IR, миграция test-helpers, populating в YAML block scalars и markdown code blocks. Без этого composite координаты неточные.
3. **Phase 3: `Rule.check` enum рефактор + JSON Schema** — D1 + D2; новые `ExecError` варианты; референс ruleset `@std/jsonschema/*` (3 правила).
4. **Phase 4: composite-rules** — D3 + D7; зависит от Phase 2 (inline-spans) и Phase 3 (Rule.check enum); markdown.code-blocks-yaml-valid как первое composite-правило.
5. **Phase 5: extended rulesets** — `@std/terraform` (8 правил), `@std/openapi` (6 правил, feature-gated); `data-query-rules` spec MODIFIED.

**Rollback:** каждая phase шипится отдельным PR, archive openspec change только когда все 5 phases вмёржены и `cargo test --workspace --all-features` зелёный.

**No CLI surface changes**, миграция пользовательских скриптов не требуется. Существующие правила работают без изменений (Rule.check default-парсится в `Check::Jq`).

## Open Questions

- **`@std/openapi` без `oas3`** — нужны ли fallback-правила, использующие только jq-проверки? На Phase 5 решим: если `oas3` API нестабилен, можем shipping 2-3 простых правил без `oas3` (`info-required-fields`, `paths-non-empty` через jq), а typed-валидацию отложить.
- **Partial XML round-trip vs strict mode** — добавить ли `XmlFormat::write_strict` который падает при mixed-content вместо silent loss? Решение: пока нет; `tracing::warn!` достаточно. Поднимем если будут жалобы.
- **Composite-rule unit tests формат** — `*.test.yml` фикстуры для composite должны парсить вложенные форматы; это естественно работает через Test runner contract'а, но edge-case с parse-failed внутри expected — нужен `expected_error` ключ в фикстуре. Решим в Phase 4 при написании первого composite-теста.
- **`schema_file:` resolution corner cases** — symlinks в rule directory, `..` в path. Решение: `Utf8PathBuf::canonicalize` + assert внутри rule directory; explicit `ExecError::SchemaFileEscapesRuleDir` если выйдет.
