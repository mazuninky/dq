# Tasks — add-ir-foundation

Каждая задача — отдельный промпт для `rust-cli-writer` (код) или `rust-cli-test-writer` (тесты), ≤ 2 часов работы. Между фазами — landable boundary: PR с зелёным CI и продакшен-семантикой. Откол фазы в самостоятельный change допустим, если по итогам Phase 4 окажется, что Phase 5 не помещается в один заход.

## 1. Phase 1 — IR types in `dq-core`

- [x] 1.1 [writer] Создать модуль `crates/dq-core/src/ir/mod.rs` с типами `Ir<'a>`, `OwnedIr`, `Provenance`, `ProvenanceMap = HashMap<Pointer, Provenance>`, `SyntheticReason`. Зарегистрировать `pub mod ir;` в `lib.rs` и реэкспорт `pub use ir::{Ir, OwnedIr, Provenance, ProvenanceMap, SyntheticReason};`. Контракты: `Ir<'_>: Copy`, `OwnedIr: Clone + PartialEq`. Без логики lookup'а — только типы и derives.
- [x] 1.2 [writer] Добавить `OwnedIr::to_borrowed(&self) -> Ir<'_>` и `OwnedIr::into_parts(self) -> (Value, ProvenanceMap, FormatTag)`; реализовать `From<OwnedIr> for (Value, ProvenanceMap, FormatTag)` для удобства.
- [x] 1.3 [writer] Добавить методы `Ir::provenance_for(&self, &Pointer) -> Option<&Provenance>` и `Ir::span_for(&self, &Pointer) -> Option<&ValueSpan>` в `crates/dq-core/src/ir/mod.rs`. Lookup идёт через канонический pointer string. `span_for` возвращает `None` для `Synthetic` и для `Original { span: None }`.
- [x] 1.4 [writer] Добавить `Document::as_ir(&self) -> Ir<'_>` в `crates/dq-core/src/document/mod.rs`. Реализация zero-copy: построить `ProvenanceMap` из существующего `SpanMap` (lazy через small wrapper, или materialise on first call с `OnceCell`). Решение по лени — на усмотрение writer-а; задокументировать в rustdoc.
- [x] 1.5 [writer] В `crates/dq-core/src/parsers/yaml_spans.rs`, `parsers/json.rs`, `parsers/toml.rs` — после построения `Document::with_spans(...)` ничего дополнительно не делать (provenance автогенерится через `as_ir()`); проверить, что для read-only форматов (jsonl/hcl/ini/dotenv/csv/tsv/dockerfile/ignore_list/markdown body) `as_ir().provenance` пустая, format корректный.
- [x] 1.6 [test-writer] Unit-тесты в `crates/dq-core/src/ir/mod.rs` per scenarios из `specs/data-query-ir/spec.md`: `Document::as_ir` zero-copy (мутация `Document.value` отражается на следующем `as_ir()`), `OwnedIr::into_parts` round-trip, `Ir<'_>: Copy` через `assert_copy`.
- [x] 1.7 [test-writer] Integration-тест в `crates/dq-core/tests/`: распарсить YAML с комментариями, убедиться, что `doc.as_ir().span_for(&pointer)` для каждого leaf-pointer соответствует `doc.spans().get(&canonical)`.
- [x] 1.8 [test-writer] Тест в `crates/dq-core/tests/`: распарсить JSONL multi-doc, убедиться `as_ir().provenance` пуста, `as_ir().format == FormatTag::Jsonl`.
- [x] 1.9 **Phase 1 landable boundary** — `cargo test -p dq-core` зелёный; `cargo build --workspace` и `cargo clippy --workspace --all-targets -- -D warnings` без warning'ов; PR ландится самостоятельно (никто пока не использует новые типы — поведение неизменно).

## 2. Phase 2 — Span-aware lint pipeline

- [x] 2.1 [writer] В `crates/dq-transform/src/jq.rs` (или новый файл `ir_adapter.rs`) добавить `pub fn ir_to_val(input: &Ir<'_>) -> Result<Val, JqError>` и `pub fn val_to_owned_ir(val: &Val, format: FormatTag) -> Result<OwnedIr, JqError>`. Реэкспорт через `lib.rs`. По умолчанию provenance в `val_to_owned_ir` — `SyntheticReason::Computed` для каждого узла. Существующие `serde_to_val`/`val_to_serde` — не трогать.
- [x] 2.2 [writer] В `crates/dq-exec/src/evaluator.rs` поменять сигнатуру `Evaluator::evaluate_file` с `(&self, path, &serde_json::Value, format_name)` на `(&self, path, ir: &Ir<'_>, format_name)`. Внутри использовать `ir_to_val` для подачи в jaq. Сохранить семантику остальных шагов pipeline без изменений.
- [x] 2.3 [writer] В `crates/dq-exec/src/rule.rs` добавить поле `pointer: Option<String>` в `Loc` struct (с `serde(default)`); пересобрать `serde(deny_unknown_fields)` так, чтобы оно осталось.
- [x] 2.4 [writer] В `crates/dq-exec/src/evaluator.rs` (`compile_rule`) добавить компиляцию `loc.pointer` jq-выражения как ещё одного `JqEngine`, по аналогии с `loc_line_engine`. Поле `loc_pointer_engine: Option<JqEngine>` в `CompiledRule`.
- [x] 2.5 [writer] В `Evaluator::evaluate_file` реализовать новую chain `loc.pointer → loc.line → intrinsic` per `data-query-exec/spec.md` Requirement «Location override via `loc:`»: eval `loc.pointer`, если результат — non-empty string, прогнать через `ir.span_for(&Pointer::parse(s)?)`; если `Some(span)`, взять `span.line/col`; иначе fallthrough на старый `loc.line` путь. Логировать `tracing::trace!` chain для отладки.
- [x] 2.6 [writer] В `crates/dq-cli/src/commands/lint_core.rs:105` поменять `value_to_serde_json(doc.value())` на построение `&Ir<'_>` через `doc.as_ir()`; передать в `evaluator.evaluate_file(file, &doc.as_ir(), &format_name)`.
- [x] 2.7 [writer] Перевести правило `@std/k8s/image-pull-policy-always` (см. `crates/dq-lint/rules/`) на `loc.pointer` как референс. Старый `loc.line` удалить из правила. Обновить companion `<rule>.test.yml` фикстуры под новые `expected.violations.line`.
- [x] 2.8 [test-writer] Unit-тесты в `crates/dq-exec/src/evaluator.rs` per scenarios из `specs/data-query-exec/spec.md` Requirement «Location override via `loc:`»: `loc.pointer` resolves to span line, fallthrough к `loc.line` при missing span, legacy-only `loc.line` работает, `loc.file` независим.
- [x] 2.9 [test-writer] Integration-тест в `crates/dq-exec/tests/`: загрузить минимальный YAML с известными span'ами, прогнать правило с `loc.pointer`, убедиться, что diagnostic line/col совпадают с `doc.spans().get(...).line`.
- [x] 2.10 [test-writer] Snapshot-тест `dq lint` через `assert_cmd` (insta) на YAML из фикстуры с переведённым `@std/k8s/image-pull-policy-always`: проверить, что вывод line/col точный (не `1`), сравнить с golden.
- [x] 2.11 **Phase 2 landable boundary** — `cargo test --workspace` зелёный; `dq lint` производит точные line/col на тестовом фикстурном YAML; CHANGELOG.md записывает «`loc.pointer` introduced; `loc.line` deprecated, removal in future change».

## 3. Phase 3 — Edit-ops vocabulary in `dq-core`

- [ ] 3.1 [writer] Создать модуль `crates/dq-core/src/edit_ops/mod.rs` с типами `EditOp` (variants: `Add { path: Pointer, value: Value }`, `Replace { path: Pointer, value: Value }`, `Remove { path: Pointer }`) и `EditScript(Vec<EditOp>)`. Реализовать `EditScript::new()`, `push`, `ops`, `is_empty`, `len`, `is_noop`, `IntoIterator`/`FromIterator`. Зарегистрировать `pub mod edit_ops;` в `lib.rs`.
- [ ] 3.2 [writer] Реализовать `serde::Serialize` / `Deserialize` для `EditOp` и `EditScript` в JSON Patch shape (RFC 6902). Парсер отвергает unknown ops (`copy`/`move`/`test`) с `Error::Format` сообщением; по полям — `serde(deny_unknown_fields)`. Использовать `#[serde(tag = "op", rename_all = "lowercase")]`.
- [ ] 3.3 [writer] Реализовать `EditScript::apply(&mut Document) -> Result<()>` в `crates/dq-core/src/edit_ops/mod.rs`. Каждая op идёт через `Document::set_at` / `del_at`. На ошибке — return Err с partial-state (документация явно указывает: caller клонирует Document перед apply для атомарности).
- [ ] 3.4 [writer] Refactor `Document::set_at` / `del_at` (NB: публичный API не меняется) — они должны делегировать в `EditScript::apply` единственного-op'а. Это устраняет дублирование renderer-логики и проверяет, что vocab корректен.
- [ ] 3.5 [test-writer] Unit-тесты в `crates/dq-core/src/edit_ops/mod.rs` per scenarios из `specs/data-query-edit-ops/spec.md`: serialize replace, deserialize JSON Patch array, reject `copy`, `EditScript::is_noop()`, `IntoIterator`/`FromIterator` round-trip.
- [ ] 3.6 [test-writer] Property-тест в `crates/dq-core/tests/edit_ops_proptest.rs` (proptest): для пары `(pointer, value)`, при которых `set_at` succeeds (генерим из существующих парсер-фикстур), `EditScript::Replace` produces byte-identical result vs прямой `set_at`. То же для `Remove` vs `del_at`.
- [ ] 3.7 [test-writer] Тест: multi-op script applies in order (Add /x → Replace /y → проверить bytes); partial-failure (Replace existing OK, Replace missing FAIL → первый op применён в bytes, второй не applied).
- [ ] 3.8 **Phase 3 landable boundary** — `cargo test --workspace` зелёный; `dq set` / `dq del` поведение неизменно (snapshot tests из M2 проходят); никаких user-visible изменений.

## 4. Phase 4 — Per-violation fix via `fix.ops`

- [ ] 4.1 [writer] В `crates/dq-exec/src/rule.rs` поменять `RuleFix { jq: String }` на `RuleFix { jq: Option<String>, ops: Option<String> }`. Сохранить `serde(deny_unknown_fields)`.
- [ ] 4.2 [writer] Добавить custom `serde::Deserialize` или `#[serde(deserialize_with = ...)]` для валидации `at-least-one-of jq/ops`: при обоих `None` возвращать `serde::de::Error::custom("at least one of `jq` or `ops` must be set")`. Convert в `ExecError::Parse` через rule loader path.
- [ ] 4.3 [writer] В `crates/dq-exec/src/evaluator.rs::compile_rule` добавить компиляцию `fix.ops` как `JqEngine` в новое поле `CompiledRule.fix_ops_engine: Option<JqEngine>`.
- [ ] 4.4 [writer] В `crates/dq-exec/src/fixer.rs::Fixer::apply` добавить ветку «если у правила есть `fix_ops_engine`, использовать его»: eval против текущего `Ir`, marshal output как `serde_json::Value`, parse как `EditScript`, применить через `EditScript::apply(&mut doc.clone())`, проверить идемпотентность повторным eval `is_noop()`. На malformed output — `ExecError::FixApply { rule_id, message }`.
- [ ] 4.5 [writer] Реализовать precedence: если у правила одновременно `fix.jq` и `fix.ops`, выбирается `ops`, и логируется `tracing::warn!(rule_id, "fix.jq is shadowed by fix.ops")`.
- [ ] 4.6 [writer] Перевести правило `@std/npm/has-license` на `fix.ops` (удалить `fix.jq`); правило `@std/k8s/image-pull-policy-always` оставить на `fix.jq` для проверки coexistence обеих веток. Обновить companion `<rule>.test.yml` фикстуры.
- [ ] 4.7 [test-writer] Unit-тесты в `crates/dq-exec/src/fixer.rs` per scenarios из `specs/data-query-exec/spec.md` Requirement «`Fixer` runtime»: idempotent ops applied + applied_rules; non-idempotent ops skipped + restored from clone; malformed ops → `ExecError::FixApply`.
- [ ] 4.8 [test-writer] Тест в `crates/dq-exec/src/rule.rs` per scenarios `Rule.fix typed schema`: empty `fix:{}` fails, `fix:{jq:.}` parses, `fix:{ops:.}` parses, `fix:{jq:.,ops:.}` parses (warn at runtime).
- [ ] 4.9 [test-writer] Snapshot-тест в `crates/dq-cli/tests/`: `dq fix` на YAML, где правило с `fix.ops` точечно меняет одно поле — комментарии вокруг этого поля сохраняются (insta-сравнение байтов до/после).
- [ ] 4.10 [test-writer] Тест: `fix.jq` legacy path всё ещё работает (`@std/k8s/image-pull-policy-always` без `loc.pointer`/`fix.ops` миграции — поведение M10 неизменно).
- [ ] 4.11 **Phase 4 landable boundary** — `cargo test --workspace` зелёный; `dq fix` сохраняет комментарии для ops-based правил; `@std/npm/has-license` мигрировано как референс; CHANGELOG.md записывает «`fix.ops` introduced; `fix.jq` deprecated for new rules».

## 5. Phase 5 — Plugin ABI on WIT + wasmtime

- [ ] 5.1 [writer] Создать новый крейт `crates/dq-plugin/` (`Cargo.toml` + `src/lib.rs`). Зарегистрировать в `Cargo.toml` workspace member'ом. `Cargo.toml` объявляет `[features] default = []; plugins = ["dep:wasmtime", "dep:wit-bindgen-runtime"]` (имя биндингов уточнить по актуальной wit-bindgen). Под feature-off — public API стаб с `PluginError::FeatureDisabled`.
- [ ] 5.2 [writer] Написать `crates/dq-plugin/wit/dq-plugin.wit` с пакетом `dq:plugin@0.1.0`: interfaces `ir` (get-root, get-at, iterate, format-tag), `jq` (compile, eval), `world plugin` (imports + exports lint/fix), records `diagnostic`, enum `severity`. Соответствует `specs/data-query-plugin-abi/spec.md` Requirement «WIT schema package `dq:plugin`».
- [ ] 5.3 [writer] Сгенерить host-side bindings через `wit-bindgen` (build.rs или explicit invocation в Cargo.toml `[build-dependencies]`). Документировать в README крейта команду регенерации (для обновления при WIT changes).
- [ ] 5.4 [writer] Реализовать `PluginRuntime::load(path: &Utf8Path) -> Result<PluginHandle, PluginError>` в `crates/dq-plugin/src/runtime.rs`. Wasmtime config: `consume_fuel(true)`, `max_wasm_stack(2 << 20)`, no WASI. Detect WASI imports → `PluginError::DisallowedImport`. Detect WIT version mismatch (major) → `PluginError::SchemaVersion`.
- [ ] 5.5 [writer] Реализовать host imports `ir::*` (get-root → CBOR/JSON serialize root Value; get-at via Pointer parse; iterate via children traversal; format-tag → string).  Источник данных — `&Ir<'_>` или `&Document`, переданный в `PluginRuntime::invoke_*`.
- [ ] 5.6 [writer] Реализовать host imports `jq::*` (compile → host-side `JqEngine::compile`, return handle u32 mapping into per-store engine pool; eval → run engine against input pointer's value).
- [ ] 5.7 [writer] Реализовать `PluginRuntime::invoke_lint(&self, handle, &Ir, file_path) -> Result<Vec<Diagnostic>, PluginError>` с marshalling: WIT `diagnostic` → host `Diagnostic`, line/col из `Ir::span_for(pointer)` если pointer задан, иначе 1/1. Fuel budget: 100M units; превышение → `PluginError::Exhausted`.
- [ ] 5.8 [writer] Реализовать `PluginRuntime::invoke_fix(&self, handle, &Ir) -> Result<EditScript, PluginError>` с парсингом возвращаемых bytes как `EditScript` через `serde_json::from_slice`. На parse failure → `PluginError::MalformedFix { rule_id, source }`.
- [ ] 5.9 [writer] Реализовать `PluginError` enum + `kind_name(&self) -> &'static str` per spec. Variants: `FeatureDisabled`, `SchemaVersion`, `Exhausted`, `Memory`, `DisallowedImport`, `MalformedFix`, `Load`, `Invoke`.
- [ ] 5.10 [writer] В `crates/dq-cli/src/cli.rs` добавить `--plugins <DIR>` flag (global, `Option<Utf8PathBuf>`). В `lint_core.rs` и `fix_core.rs` (если такой существует, иначе fix handler) — discovery `*.wasm` под `<DIR>` non-recursive, lexical sort, load через `PluginRuntime`. Без feature: использование флага с непустым `<DIR>` → `InvalidInput` exit 6.
- [ ] 5.11 [writer] Добавить exit-code mapping в `dq-cli/src/error.rs` per spec: `feature_disabled`/`disallowed_import` → `InvalidInput` (6); `schema_version`/`malformed_fix` → `PARSE_ERROR` (3); `exhausted`/`memory`/`invoke` → `RUNTIME_ERROR` (4); `load` → `RUNTIME_ERROR` (4).
- [ ] 5.12 [writer] Создать пример `examples/plugin-rust/` — минимальный lint+fix плагин на Rust, target `wasm32-wasi-preview2` (или wasm32-unknown-unknown с component model — уточнить по wasmtime версии). README объясняет build + use.
- [ ] 5.13 [writer] Обновить `README.md` (top-level) секцией «Plugins (experimental)» с командой `cargo install --features plugins dq-cli`, ссылкой на `examples/plugin-rust/`, и предупреждением «v0.1.0 WIT preview, breaking before v1.0.0».
- [ ] 5.14 [test-writer] Unit-тест в `crates/dq-plugin/src/error.rs`: `PluginError::kind_name()` covers every variant + no two variants return the same name.
- [ ] 5.15 [test-writer] Integration-тест в `crates/dq-plugin/tests/`: загрузить fixture WASM (build артефакт из `examples/plugin-rust/` или статический CI-prebuilt) → `invoke_lint` returns expected diagnostics. Использовать `cargo build --target wasm32-...` build.rs или коммитить prebuilt `.wasm` под `tests/fixtures/`.
- [ ] 5.16 [test-writer] Integration-тест: fix плагин возвращает EditScript; runtime его применяет; bytes документа меняются ожидаемо.
- [ ] 5.17 [test-writer] Тест: plugin с infinite loop terminates с `PluginError::Exhausted` в пределах fuel budget (не более 5 секунд wall-clock в CI).
- [ ] 5.18 [test-writer] Тест: WASI-importing plugin → `PluginError::DisallowedImport`.
- [ ] 5.19 [test-writer] Тест: `--plugins ./dir` с `*.wasm` файлами под dq-cli, скомпилированном без `plugins` feature → exit 6, stderr содержит `"plugins are not enabled in this build"`. Использует `assert_cmd` + кастомный build profile.
- [ ] 5.20 [test-writer] Тест: WIT schema version mismatch — fixture WASM с major 2.0.0 при host major 0.1.0 → `PluginError::SchemaVersion`.
- [ ] 5.21 **Phase 5 landable boundary** — `cargo test --workspace --features plugins` зелёный; `cargo build --workspace` (no features) тоже зелёный; статический бинарь без `plugins` feature не растёт более чем на 100 KiB (sanity check binary size); CHANGELOG записывает «Plugin ABI v0.1.0 (experimental, breaking before v1.0.0)».

## 6. Закрытие change

- [ ] 6.1 Прогнать `openspec validate add-ir-foundation --type change --strict` — должен пройти после всех фаз.
- [ ] 6.2 Прогнать openspec-archive-change skill для архивации.
