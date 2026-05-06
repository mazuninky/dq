## Why

Сейчас в `dq` три параллельных представления значения — `dq_core::Value`, `serde_json::Value` и `jaq_json::Val` — и каждая команда платит конвертацией между ними. Это терпимо ровно до тех пор, пока трёх вещей не нужно одновременно:

1. **Точные line/col в lint-диагностиках.** [`dq-exec/src/evaluator.rs:23-28`](../../../crates/dq-exec/src/evaluator.rs:23) явно фиксирует костыль: «value adapter strips parser-provided byte spans … evaluator defaults `line` and `col` to `1`. Rules that need a real position derive it via the `loc.line` jq override». Это ломает SARIF-regions, LSP-интеграцию и user trust к выхлопу `dq lint`.
2. **Per-violation fix.** M10 (`dq fix`, см. README §Status) ограничен whole-document `fix.jq`: теряет комментарии, не композируется между правилами, идемпотентность проверяется тупым повторным прогоном. Anti-scope в README прямо говорит: «per-violation fixes / explicit ops vocabulary deferred».
3. **WASM-плагины как сторонние правила и фиксеры.** В `dq-plan.md` (M12 — community registry / WASM) WASM назван целью, но без стабильного plugin-ABI первый же сторонний плагин завязывается на внутренние типы хоста и блокирует любую эволюцию.

Все три проблемы решаются одним фундаментом: явным IR-слоем с span-propagation контрактом и edit-ops словарём, плюс стабильной WIT-схемой как plugin-ABI поверх него. Делать их раздельно — три раза переписывать границу `dq-core ↔ dq-transform ↔ dq-exec`. Делать вместе — один заход, общий вокабуляр, согласованные тесты.

## What Changes

- **NEW IR-слой в `dq-core`.** Тип `Ir` (rename/wrapper над текущим `Value`) с обязательным `Provenance`-параллелем: каждый узел знает свой исходный pointer и опционально `ValueSpan`. Контракт: span-info переживает любую read-only трансформацию, для которой соответствие input↔output определимо.
- **Span-aware value adapter в `dq-transform`.** Существующий мост `Value ↔ jaq_json::Val` ([`dq-transform/src/jq.rs:278`](../../../crates/dq-transform/src/jq.rs:278)) расширяется: либо обёрткой `(Val, ProvenancePtr)`, либо побочным каналом `ProvenanceMap`. Чистые pass-through фильтры (`.foo`, `.[]`, `select`, `map`, `to_entries`/`from_entries` round-trip) сохраняют spans; для фильтров, где соответствие не определимо (`length`, арифметика, конструкторы), span гасится в `None` явно, а не по умолчанию. **BREAKING**: `dq-exec::Evaluator::evaluate_file` принимает `&Ir` вместо `&serde_json::Value`.
- **Edit-ops vocabulary в `dq-core`.** Новый enum `EditOp { Add { path: Pointer, value: Value }, Replace { path: Pointer, value: Value }, Remove { path: Pointer } }` (имена соответствуют RFC 6902 JSON Patch — см. spec `data-query-edit-ops`) плюс `EditScript = Vec<EditOp>`. Существующие `Document::set_at` / `del_at` ([`dq-core/src/document/mod.rs:420`](../../../crates/dq-core/src/document/mod.rs:420)) реализуются поверх ops, не наоборот. Ops применяются через уже существующие `ScalarRenderer` / `InsertionRenderer` ([`dq-core/src/textual_edit/mod.rs:86`](../../../crates/dq-core/src/textual_edit/mod.rs:86)) — comment preservation бесплатно.
- **Per-violation fix в `dq-exec`.** Rule schema получает `fix.ops:` блок (jq-выражение, возвращающее массив ops) альтернативой к whole-document `fix.jq`. **BREAKING (на уровне rule schema, не CLI)**: `Fixer` дополнительно умеет применять `EditScript`. Идемпотентность проверяется на уровне ops («второй прогон даёт пустой EditScript»), не сравнением полных дампов. Существующие `fix.jq` правила (`@std/k8s/image-pull-policy-always`, `@std/npm/has-license`) продолжают работать через trivial transpile в `[{op: "replace", path: "", value: <new doc>}]`.
- **Plugin ABI на WIT/Component Model.** Новый крейт `dq-plugin` с WIT-схемой: плагин получает доступ к `Ir` через host-side capabilities (`get_at`, `iterate`, `format_tag`) и возвращает `Vec<Diagnostic>` (lint-плагин) или `EditScript` (fix-плагин). Wasmtime как runtime, feature-gated (`--features plugins`). Стабильность WIT даёт обратную совместимость для сторонних плагинов через семантическое версионирование схемы.
- **Anti-scope (твёрдая граница).** НЕ унифицируем `dq_core::Value` с `serde_json::Value` — `BigInt/BigFloat` как textual literals и `IndexMap` order требуются для round-trip preservation, выкидывать их нельзя. НЕ форкаем jaq — span propagation делается через wrapper-Val, не модификацией jaq-core. НЕ embed-им jaq в WASM-плагин — фильтры jq доступны плагину только через host call.

## Capabilities

### New Capabilities

- `data-query-ir`: типы `Ir` + `Provenance`, контракт span-propagation сквозь read-only трансформации, value-adapter Ir↔jaq_json::Val со span-сохранением. Фундамент, на котором стоят остальные две capability.
- `data-query-edit-ops`: словарь `EditOp` / `EditScript`, семантика применения через текущие renderer-факторы, идемпотентность ops, композиция между правилами/плагинами.
- `data-query-plugin-abi`: WIT-схема плагина, host capabilities surface, runtime-загрузка через wasmtime, версионирование схемы, изоляция плагина (memory limits, no fs/net by default).

### Modified Capabilities

- `data-query-transform`: jq value-adapter теперь span-aware — see Requirement «Value adapter between `serde_json::Value` and `jaq_json::Val`» в [`openspec/specs/data-query-transform/spec.md:63`](../../specs/data-query-transform/spec.md). Контракт расширяется обязательством сохранять Provenance для пропускающих фильтров.
- `data-query-exec`: `Evaluator::evaluate_file` сигнатура меняется (`&Ir` вместо `&serde_json::Value`); `Diagnostic.line/col` источник перенесён с `loc.line` jq-fallback на native span lookup через `loc.pointer`; `Rule.fix` типизированная схема расширяется опциональным `ops:`; `Fixer::apply` принимает `EditScript` дополнительно к whole-document `fix.jq`.

> Не модифицируются на spec-уровне: `data-query-rules` (rule library catalog/namespace policy без изменений), `data-query-write` (CLI-семантика `dq set`/`dq del` неизменна, перевод их internal-path на `EditScript` — implementation detail без изменений требований).

## Impact

**Затронутые крейты:**
- `dq-core` — новые модули `ir/`, `edit_ops/`; рефактор `document/` под Ir.
- `dq-transform` — span-aware value adapter, контрактные тесты propagation.
- `dq-exec` — Evaluator/Fixer работают через Ir + EditScript; диагностики берут позицию из span.
- `dq-cli` — миграция `set` / `del` / `fix` / `lint` хендлеров на новые сигнатуры.
- `dq-plugin` (новый крейт) — WIT, runtime, host capabilities.
- `dq-lint` — два существующих fix-правила переводятся на `fix.ops` как референс.

**Зависимости:** добавляются `wasmtime` и `wit-bindgen` (или альтернативы) под feature-gate `plugins`. Без feature — статический бинарь не растёт.

**Совместимость:**
- CLI surface не меняется — все breaking — внутри Rust API.
- Rule schema BREAKING на уровне internal contract: `fix.ops` это **новое опциональное** поле, существующие правила работают; deprecation `fix.jq` — soft, без удаления в этом change.
- Fixer-идемпотентность: семантика жёстче — ops-based check вместо текстового сравнения. Существующие правила должны пройти, но возможны edge cases с правилами, чей `fix.jq` был не идемпотентен «по байтам, но идемпотентен по value» — такие найдём в тестах.

**Риски и meтрики успеха:**
- Размер change большой (вся триада сразу). Разбивка по milestones внутри tasks.md обязательна; design.md фиксирует, какой минимум должен ландиться вместе и что можно отколоть.
- Span propagation через jq — наименее предсказуемая часть. Минимальное определение успеха: для текущих 46 `@std` правил line/col в диагностиках совпадает с тем, что даёт ручная attribution через `loc.line`.
- Plugin ABI — самая большая поверхность для будущих breaking changes. Минимальное определение успеха: один out-of-tree пример WASM-плагина (lint и fix), документированный в `docs/`, который продолжает работать после следующих двух patch-релизов.
