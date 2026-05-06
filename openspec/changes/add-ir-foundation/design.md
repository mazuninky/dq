## Context

Текущая read/transform/write граница в `dq` устроена так:

- Парсеры в [`dq-core/src/parsers/`](../../../crates/dq-core/src/parsers/) производят `Document { value: Value, original_bytes, spans: SpanMap, format }`.
- `Value` (см. [`dq-core/src/document/mod.rs:39`](../../../crates/dq-core/src/document/mod.rs:39)) — собственный enum с `BigInt/BigFloat` как textual literals и `IndexMap<String, Value>` для порядка ключей.
- При первом пересечении границы в jq значение конвертируется через `value_to_serde_json` ([`dq-cli/src/commands/io_helpers.rs:179`](../../../crates/dq-cli/src/commands/io_helpers.rs:179)) → `serde_json::Value` → `jaq_json::Val` ([`dq-transform/src/jq.rs:278`](../../../crates/dq-transform/src/jq.rs:278)). На этой границе `SpanMap` отбрасывается.
- `Evaluator::evaluate_file` ([`dq-exec/src/evaluator.rs`](../../../crates/dq-exec/src/evaluator.rs)) принимает `&serde_json::Value` и в комментарии явно фиксирует костыль: `line/col` дефолтятся в `1`, real position восстанавливается через `loc.line` jq-override.
- Write-path использует `Document::set_at` / `del_at` через `ScalarRenderer` / `InsertionRenderer` factory ([`dq-core/src/textual_edit/mod.rs:86`](../../../crates/dq-core/src/textual_edit/mod.rs:86)). Comment preservation работает только потому, что эти renderer-ы видят `original_bytes` и `ValueSpan`.
- `Fixer` ([`dq-exec/src/fixer.rs`](../../../crates/dq-exec/src/fixer.rs)) применяет `fix.jq` whole-document — то есть **не** ходит через renderer-ы, а заменяет всё дерево, что и обнуляет comments на re-emit (см. README §Status: «Comment preservation: same trade-off as `dq set --jq`»).

Stakeholders:
- Авторы lint-правил (`@std/*` + сторонние) — нужны точные line/col без `loc.line` обвязки.
- Авторы fix-правил — нужны point-edits с сохранением комментариев и проверяемой композицией между правилами.
- Авторы будущих WASM-плагинов (M12) — нужен стабильный ABI, по которому можно публиковать плагины и не зависеть от внутренних типов хоста.
- AI-агенты в CI — главный differentiator проекта. Точные позиции в SARIF и предсказуемые fix-диффы — их основной запрос.

## Goals / Non-Goals

**Goals:**
1. **Span-propagation contract.** Lint-правила могут получать точный `(file, line, col, span)` для каждой диагностики, не парся int-строки через jq.
2. **Edit-ops vocabulary.** Fix-правила и плагины декларируют изменения в виде набора операций (subset of [RFC 6902](https://www.rfc-editor.org/rfc/rfc6902)), а не «вот вам новый документ». Comment preservation бесплатна, идемпотентность проверяется на уровне ops.
3. **Plugin ABI.** Стабильная WIT-схема, по которой третьи стороны пишут lint/fix плагины на любом языке, компилирующемся в WASM; runtime-загрузка через wasmtime, изоляция (no fs/net by default, memory limits).
4. **Zero CLI churn.** Все breaking-изменения — внутри Rust API между крейтами. Пользователь CLI не замечает перехода кроме улучшенных диагностик и точечных fix-диффов.
5. **Phased delivery.** Change крупный; tasks.md разбивает работу на 5 фаз, каждая landable отдельно с зелёным CI и продакшен-семантикой.

**Non-Goals:**
- Унификация `dq_core::Value` с `serde_json::Value` или `jaq_json::Val`. `BigInt/BigFloat` как textual literals и `IndexMap` order требуются для round-trip preservation; их выкидывать нельзя.
- Форк jaq. Span propagation не должен требовать модификации jaq-core.
- Embedded jq внутри WASM-плагина. Плагины зовут jq через host capability; компилировать jaq в WASM не нужно (сотни КБ без выигрыша).
- Произвольная span-propagation через любую jq-программу. Контракт — pointer-based: правило, которое хочет точную локацию, **обязано** вернуть JSON Pointer в выхлопе или через override; евалюатор делает span-lookup. Магической propagation `[.foo[].bar]` нет.
- Sandbox-эскалация для плагинов (fs/net/process). Phase 5 даёт минимум — read-only доступ к Ir через host calls; всё остальное deferred.

## Decisions

### D1. IR-слой — это `Ir` + `Provenance`, а не replacement для `Value`

**Решение:**
```rust
// dq-core::ir
pub struct Ir<'a> {
    value: &'a Value,            // существующий enum, не трогаем
    provenance: &'a ProvenanceMap,
    format: FormatTag,
}

pub struct OwnedIr {              // для случаев, где нужен owned
    value: Value,
    provenance: ProvenanceMap,
    format: FormatTag,
}

pub type ProvenanceMap = HashMap<Pointer, Provenance>;

pub enum Provenance {
    /// Узел соответствует pointer в исходном документе.
    Original { pointer: Pointer, span: Option<ValueSpan> },
    /// Узел синтезирован трансформацией; span отсутствует.
    Synthetic { reason: SyntheticReason },
}
```

**Почему не rename `Value` → `Ir`:** noise rename ломает абсолютно все call-site'ы во всех 5 крейтах ради «красивого имени». `Value` — известный термин, и его сохраняем. `Ir` — это пара `(Value, Provenance)`, концептуально новая сущность.

**Почему provenance — отдельный side-channel `Pointer → Provenance`, а не поле в каждом `Value`:**
1. `Value` остаётся `Clone`-cheap; никаких лишних аллокаций за обход дерева, у которого все узлы Synthetic.
2. Большинство трансформаций не меняют `Value`, только перетасовывают узлы — вкладывать pointer в каждый узел заставит обновлять весь массив на любом slice/map.
3. SpanMap уже устроена так же ([`dq-core/src/document/spans.rs`](../../../crates/dq-core/src/document/spans.rs)) — pointer-keyed. Provenance — продолжение той же модели.

**Альтернативы рассмотрены:**
- Rename `Value` → `Ir`, добавить `provenance: Provenance` в каждый узел. Отверг: см. п. 2 выше.
- Использовать `serde_json::Value` как Ir. Отверг: теряется `BigInt/BigFloat` точность.
- Положить provenance внутрь `Document` напрямую. Отверг: тогда нечего передавать в `dq-transform` — `Document` слишком тяжёлый (тащит `original_bytes`).

### D2. Span propagation через jq — pointer-based, без форка jaq

**Решение:** правило, желающее точный `loc`, обязано эмитить либо JSON Pointer строку, либо пару `[pointer, payload]`. Rule schema получает поле `loc.pointer:` (jq-выражение, возвращающее JSON Pointer). Evaluator делает `Document.spans.get(pointer)` → `ValueSpan` → line/col.

```yaml
# Old (M8-M10):
check:
  jq: '.spec.containers[] | select(.imagePullPolicy != "Always") | .name'
  message: 'imagePullPolicy != Always for container {{ . }}'
loc:
  line: '.spec.containers[] | select(.imagePullPolicy != "Always") | input_line_number'
  # обвязка для парсинга строк, костыль

# New (this change):
check:
  jq: '.spec.containers | to_entries[] | select(.value.imagePullPolicy != "Always") | [(("/spec/containers/" + (.key|tostring)) as $p | $p), .value.name]'
  message: 'imagePullPolicy != Always for container {{ .[1] }}'
loc:
  pointer: '.[0]'
```

**Почему не value-wrapping (`Val + ProvenancePtr`):** требует patch jaq-core или собственной обёртки, которая ломается на любом упоминании `Val` внутри runtime. Сложно, хрупко, дорого по поддержке.

**Почему не two-pass evaluation (один проход для значения, другой для пути):** удваивает стоимость каждого правила, расхождение между двумя выражениями — источник бесконечных bug-ов.

**Почему не `loc.path:` filter, который дёргает встроенный jaq `path()`:** `path()` в jaq возвращает array of segments, нам нужно RFC 6901 string. Поэтому правила сами эмитят строку через builder-выражение `[("/foo/" + (key|tostring))]` — явно, читабельно, без скрытой магии.

**Compatibility:** старое поле `loc.line` остаётся работать как fallback. Если правило не задало `loc.pointer`, evaluator пытается старый путь. Deprecation — soft, без удаления в этом change.

### D3. EditScript = JSON Patch (RFC 6902) subset

**Решение:** `EditOp` — enum, ограниченный четырьмя ops из RFC 6902: `add`, `replace`, `remove`, и (опционально) `move`. `EditScript = Vec<EditOp>` с детерминированным порядком применения. Сериализация — стандартный JSON Patch, тот же формат, что уже принят `dq patch`.

```rust
// dq-core::edit_ops
pub enum EditOp {
    Add { path: Pointer, value: Value },
    Replace { path: Pointer, value: Value },
    Remove { path: Pointer },
    // Move/Copy/Test deferred — не нужны для fix-сценариев M11.
}

pub struct EditScript(Vec<EditOp>);
```

**Почему JSON Patch, а не свой формат:** уже стандарт, уже понятен пользователям, `dq patch` уже его принимает, авторы плагинов на любом языке знают. Не плодим параллельных вокабуляров.

**Почему не RFC 7396 (Merge Patch):** не выражает удаление по пути, не выражает массивных операций — слишком слабый словарь для fix.

**Почему не свой PointerOp (более выразительный):** YAGNI. Если в M12+ потребуется (`indent`, `quote-style`, etc.), расширим через extension fields, придерживаясь RFC 6902 base.

### D4. Применение EditScript идёт через существующие renderer-ы

**Решение:** `EditScript::apply(&mut Document)` под капотом вызывает `Document::set_at` / `del_at` для каждой ops. Те уже умеют через `ScalarRenderer` / `InsertionRenderer` сохранять comments в YAML/TOML/JSON. **Никакой новой write-machinery — только новый интерфейс над существующей.**

**Порядок применения:** ops применяются последовательно в том порядке, в котором эмитированы. Идемпотентность ops — обязанность правила; runtime проверяет re-apply через diff, не через ord swap. Если два правила emit-ят конфликтующие edits на одной строке — конфликт детектится на стадии compose (см. D6).

**Почему не batch + transactional rollback:** для M11 достаточно «применили или упали целиком». `Document::set_at` уже атомарен per-op (in-memory + spans + bytes согласованы). При ошибке во время apply откатываемся к pre-Fix снапшоту `Document.clone()`.

### D5. `fix.ops` в rule schema — jq-выражение, возвращающее JSON Patch массив

**Решение:** rule schema получает блок:
```yaml
fix:
  ops: '[{op: "replace", path: "/spec/replicas", value: 3}]'  # jq-выражение
```

Evaluator компилирует это как обычный jq-фильтр через `JqEngine::compile`. Runtime прогоняет фильтр на input value, парсит результат как JSON Patch, конвертирует в `EditScript`, применяет.

**Сосуществование с `fix.jq`:** оба поля валидны. `fix.ops` имеет приоритет, если задан. `fix.jq` остаётся deprecated-но-работающим (через trivial transpile в `[{op: "replace", path: "", value: <new doc>}]`). Один SemVer-major spustja — удалить `fix.jq`.

**Идемпотентность:** runtime check — если первое применение не изменило `original_bytes` (EditScript произвёл no-op в конкретном документе), правило ОК. Если изменило, но второе применение тоже изменило — `tracing::warn!` и rule skip, как сейчас в `Fixer` для `fix.jq`.

### D6. Композиция между правилами — sequential, не parallel

**Решение:** для одного файла правила применяются в declaration order их рулсетов. Каждое правило видит документ после предыдущих fixes. Конфликты не детектятся явно; если правило A заменило ноду N, и правило B пытается заменить ту же ноду — последний победил.

**Почему не граф зависимостей:** overengineering для M11. Для большинства сценариев правила оперируют разными ключами; для overlapping — пользователь сам располагает их в порядке намерения через rules.yml. Документируем явно.

**Параллелизм между файлами** уже есть в `dq fix --parallel` (rayon); сохраняем как было.

### D7. Plugin ABI — WIT/Component Model + wasmtime

**Решение:** новый крейт `dq-plugin` с:
- WIT-схемой `wit/dq-plugin.wit` — описывает host imports (capabilities, доступные плагину) и plugin exports (что плагин экспортирует).
- Wasmtime runtime, feature-gated (`--features plugins`). Без feature крейт компилируется как заглушка, статический бинарь не растёт.
- `wit-bindgen` для авто-генерации Rust-side bindings (host).

**Host imports (что плагин видит):**
```wit
// dq-plugin/wit/dq-plugin.wit
package dq:plugin@0.1.0;

interface ir {
    type pointer = string;
    type format-tag = string;

    /// Корневое значение документа, как JSON-byte-stream (CBOR в v0.2+).
    get-root: func() -> list<u8>;

    /// Значение по pointer; null = миссинг.
    get-at: func(p: pointer) -> option<list<u8>>;

    /// Children указанного pointer (object keys / array indices).
    iterate: func(p: pointer) -> list<pointer>;

    format-tag: func() -> format-tag;
}

interface jq {
    /// Скомпилировать jq-выражение (handle ID).
    compile: func(expr: string) -> result<u32, string>;

    /// Выполнить скомпилированный фильтр; результат — поток JSON-values
    /// сериализованных в bytes.
    eval: func(handle: u32, input: pointer) -> result<list<list<u8>>, string>;
}

world plugin {
    import ir;
    import jq;

    /// Lint-плагин: вернёт диагностики.
    export lint: func() -> list<diagnostic>;

    /// Fix-плагин: вернёт edit-script (JSON Patch).
    export fix: func() -> result<list<u8>, string>;
}

record diagnostic {
    rule-id: string,
    severity: severity,
    message: string,
    pointer: option<string>,
}

enum severity { error, warn, info }
```

**Изоляция:** wasmtime config с `consume_fuel(true)` (защита от бесконечных циклов), `memory_max(64 * 1024 * 1024)`, no WASI. Плагин не видит fs/net/process. Попытка вызвать unimported function → trap.

**Сериализация:** v0.1 — JSON через `serde_json` для совместимости с любым языком. v0.2+ — CBOR через `ciborium` для скорости, опционально, по feature-gate в WIT.

**Версионирование:** WIT package version (`@0.1.0`) — semver. Patch — bug fixes only. Minor — additive (новые host imports, optional fields). Major — breaking. Runtime отказывается грузить плагин с major != current.

**Альтернативы рассмотрены:**
- Pure C ABI через `extern "C"`. Отверг: каждый язык-source делает свой binding, плохо переносится.
- Pure WASM без Component Model. Отверг: плоский linear memory ABI принуждает писать ручные сериализаторы; WIT даёт structured types.
- Wit-bindgen без wasmtime, через wasmer. Отверг: wasmtime stable, поддерживается Bytecode Alliance, у dq уже нет других wasm-runtimes.

### D8. Phased delivery (фазы и их минимально landable scope)

Каждая фаза landable отдельным PR с зелёным CI и продакшен-семантикой. Зависимости между фазами — strict; в обратную сторону отколоть нельзя без re-design.

| Фаза | Содержание | Landable отдельно | Зависит от |
|------|------------|-------------------|------------|
| **1. IR types** | `dq-core::ir` модуль, `Ir`/`OwnedIr`/`Provenance`/`ProvenanceMap`. Парсеры populate Provenance. Behavior change: zero. | Да (только новые типы, никто их пока не использует). | — |
| **2. Span-aware lint** | `Evaluator::evaluate_file(&Ir, ...)`. `loc.pointer` поле в rule schema. SpanMap lookup для line/col. Старый `loc.line` остаётся как fallback. | Да (lint становится точнее, fix не трогаем). | 1 |
| **3. Edit-ops vocabulary** | `dq-core::edit_ops::{EditOp, EditScript}`. `EditScript::apply(&mut Document)`. Refactor `Document::set_at`/`del_at` под капотом на ops. | Да (внутренний рефактор, write CLI semantics неизменна). | 1 |
| **4. Per-violation fix** | `fix.ops` в rule schema. `Fixer::apply_ops`. Два `@std` правила с `fix.ops` как референс. `fix.jq` остаётся deprecated-но-работающим. | Да (новые правила работают по новому пути, старые по старому). | 3 |
| **5. Plugin ABI** | `dq-plugin` крейт, WIT, wasmtime runtime, host capabilities. Один out-of-tree пример lint+fix плагина. CLI flag `--plugins <DIR>`. | Да (feature-gated, без feature CLI как был). | 2, 4 |

Phase 1+2 = «span-aware lint». Phase 3+4 = «edit-ops fix». Phase 5 = «plugin runtime». Если в процессе обнаружится, что Phase 5 слишком велика — выделим в отдельный change `add-plugin-runtime`, но как single change-зонтик это всё-таки оправдано: WIT-схема ссылается на типы из Phase 1 (`pointer`, `format-tag`) и Phase 3 (edit-script JSON Patch). Без них схема фрагментирована.

## Risks / Trade-offs

**[R1] Span propagation через jq решает только pointer-emitting правила.** Правила, чей `check.jq` теряет input-pointer (например, агрегирующие фильтры), не получат точную локацию.
→ Mitigation: документация явно описывает «как написать правило, которое сохраняет pointer». 46 существующих `@std` правил — наш test-set; в Phase 2 переводим их и считаем coverage. Если меньше 90% дают точный `loc`, пересматриваем подход.

**[R2] EditScript идемпотентность runtime-check может маскировать non-idempotent правила.** Текущий `Fixer` для `fix.jq` сравнивает байты после двух применений; для ops мы сравниваем re-apply produced empty script — это слабее, потому что ops может быть «формально не пустой» но дать тот же byte-output.
→ Mitigation: в Phase 4 обе проверки выполняются одновременно (ops empty AND bytes equal) и оба должны пройти; rule skip + `tracing::warn!` если расходятся. Не серебряная пуля, но ловит большинство bugs.

**[R3] WASM-плагин может зависнуть.** Без `consume_fuel` плагин с бесконечным циклом висит CPU.
→ Mitigation: wasmtime epoch-based interruption + fuel limit. Per-plugin invocation budget — 100M fuel units (≈ секунда CPU на M1). Превышение → trap, плагин помечен poisoned, `tracing::error!`, остальные плагины продолжают работу.

**[R4] WIT schema evolution блокирует hostside refactor.** Если Phase 1-4 земляне, и потом Phase 5 фиксирует WIT с типом `pointer = string`, мы навсегда обязаны его поддерживать.
→ Mitigation: WIT v0.1.0 — preview feature, на эту версию даём explicit `compatibility: experimental` в README и плагин-changelog'е. v1.0.0 (lock-in) — отдельный change позднее, после out-of-tree feedback'а от 2-3 сторонних плагинов.

**[R5] Перегруженность change'а.** 5 фаз — это размер 2-3 обычных milestones. Tasks.md рискует разрастись до сотен пунктов; ревью одного PR на всё нереалистично.
→ Mitigation: каждая фаза = отдельный PR. Tasks.md группирует задачи по фазам с явными `Phase N landable boundary` маркерами. Между PRs допустимо ландиться независимо при условии green CI.

**[R6] Wasmtime — heavy dependency.** Cold-binary без feature не растёт, но build-time-зависимости (wasmtime, cranelift) утяжеляют CI.
→ Mitigation: feature-gate `plugins`. По умолчанию off. CI имеет два build-job'а: core (no features) и full (`--features plugins`). Большинство user'ов берут `cargo install dq-cli` без feature → быстрый install.

**[R7] Span propagation для Markdown/Frontmatter.** Markdown AST уже сложнее простого Value-tree, и у `Frontmatter` есть `body: Vec<u8>` payload без spans для тела. Span-aware lint для них работает только в header-части.
→ Mitigation: документировано. Plugin/lint правил для Markdown body работают со spans на уровне header; для тела — на уровне whole-file (line=1). Phase 2 не пытается решить эту проблему; M9 уже принял этот compromise.

## Migration Plan

**Roll-out:**
- Phase 1 — additive only, никакой миграции.
- Phase 2 — additive в rule schema (`loc.pointer` опциональное). Существующие правила не трогаем; deprecation `loc.line` объявляем в `CHANGELOG.md`, удаление через два minor relese'а.
- Phase 3 — internal-only refactor. Никаких user-visible изменений; CLI testы должны пройти неизменными.
- Phase 4 — additive (`fix.ops` опциональное). Два `@std` правила переводим как референс, остальные оставляем на `fix.jq`. Постепенное переводнение — отдельный follow-up change.
- Phase 5 — opt-in feature. По умолчанию выключено; пользователь сам делает `cargo install --features plugins` или скачивает `dq-cli-plugins` release-вариант.

**Rollback:**
- Phase 1, 3 — реверт коммита, ничего не теряем.
- Phase 2 — revert + одно cleanup правило для `loc.pointer`-using правил, которые без span-lookup упадут. Ноль production-импакт, потому что `@std` правила пока не успеют перейти на `loc.pointer`.
- Phase 4 — revert. Правила, которые перешли на `fix.ops`, временно регрессируют (но `fix.jq` у них всё ещё есть — `fix.ops` строится поверх, не заменяет).
- Phase 5 — revert. Плагины перестают грузиться, CLI flag даёт «feature not enabled in this build».

**Schema version bumps:**
- WIT package — `0.1.0` после Phase 5; следующие feature add → `0.2.0`, breaking → `1.0.0` (отдельный change).
- Rule schema — добавление полей `loc.pointer` и `fix.ops` без breaking; deprecation `loc.line` / `fix.jq` оформляется через docs, не через схема-revision.

## Open Questions

1. **Какая stable serialization-форма для EditScript между плагином и хостом — JSON или CBOR?** Phase 5 v0.1 — JSON (universal, медленно). Решение про CBOR откладываем до первых measurements; если CBOR даёт >10% общего runtime'а на типичном rule pack — переключаемся, иначе остаёмся на JSON.

2. **Должны ли fix-плагины видеть результаты lint-плагинов в той же сессии, или каждый запускается изолированно?** Текущая позиция — изолированно (lint и fix — раздельные `world`-ы в WIT, разные WASM modules). Если в M12+ окажется востребован access pattern «fix видит свои собственные lint findings» — тогда отдельный change.

3. **Provenance::Synthetic — какие reasons enum-нируем?** Минимум: `Constructed` (literal в jq-expression), `Aggregated` (length, sum), `Computed` (арифметика). Полный список в Phase 1; tasks.md фиксирует какой-нибудь стартовый набор.

4. **Один WASM-плагин может быть и lint, и fix одновременно (один module exports оба `lint` и `fix`)?** Да, через WIT-`world` с обоими экспортами. Документируем как рекомендуемый pattern для плагинов, реализующих и detection, и autofix одной семьи правил.

5. **Можно ли зафиксировать subset jq, безопасный для span propagation?** В Phase 2 — нет; правило само decide. В будущем, возможно, статический анализатор jq-expr'а сможет вывести «эти фильтры теряют path, эти — нет» и подсказывать. Open question, не блокирует change.
