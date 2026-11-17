# Tasks

## 1. Diagnose (before code)

- [ ] 1.1 [author] Перезапустить `cargo bench -p dq-transform --bench jq -- --quick` и `cargo bench -p dq-exec --bench evaluate -- --quick` на чистом branch'е. Зафиксировать before-числа в [design.md](design.md) §1 если расходятся с теми, что в proposal.md.
- [ ] 1.2 [author] Подсчитать число уникальных jq-выражений в `@std/k8s` и `@std/openapi` (самые крупные namespace'ы). Конкретное число влияет на ожидаемый speed-up.

## 2. Implementation

- [ ] 2.1 [delegate to rust-cli-writer] В [`crates/dq-exec/src/evaluator.rs`](../../../crates/dq-exec/src/evaluator.rs) добавить:
  ```rust
  pub(crate) type JqCache = std::collections::HashMap<String, std::sync::Arc<dq_transform::JqEngine>>;
  
  fn compile_or_cached(
      cache: &mut JqCache,
      expr: &str,
      rule_id: &str,
  ) -> Result<std::sync::Arc<dq_transform::JqEngine>> { /* per design.md §2 */ }
  ```
- [ ] 2.2 [delegate to rust-cli-writer] Поменять тип всех six `Option<JqEngine>` полей в `CompiledRule` на `Option<Arc<JqEngine>>`. Аналогично — поля типа `JqEngine` в `CompiledCheck::Jq` / `CompiledCheck::Composite` / `CompiledCompositeCheck`. Импорт `use std::sync::Arc`.
- [ ] 2.3 [delegate to rust-cli-writer] Добавить параметр `cache: &mut JqCache` в сигнатуры:
  - `compile_rule_to_depth` ([evaluator.rs:331](../../../crates/dq-exec/src/evaluator.rs:331))
  - `compile_check` ([evaluator.rs:438+](../../../crates/dq-exec/src/evaluator.rs:438))
  - `compile_composite` ([composite.rs:112+](../../../crates/dq-exec/src/composite.rs:112))
  
  Протащить cache через recursion'ы (composite → compile_rule_to_depth → compile_check → compile_composite).
- [ ] 2.4 [delegate to rust-cli-writer] Заменить все вызовы `JqEngine::compile(expr).map_err(...)` в `compile_rule_to_depth` (стр. 337–417) и `compile_composite` на `compile_or_cached(cache, expr, &rule.id)?`. Шесть call-site'ов в `compile_rule_to_depth`, плюс call-site'ы в `compile_check` (для `Check::Jq`) и `compile_composite` (для `extract:`).
- [ ] 2.5 [delegate to rust-cli-writer] В [`Evaluator::new`](../../../crates/dq-exec/src/evaluator.rs:201) создать пустой `let mut cache = JqCache::new();` перед циклом по rule'ам и передать его в `compile_rule_to_depth(...)`.
- [ ] 2.6 [delegate to rust-cli-writer] Проверить call-site'ы `engine.run(...)` в:
  - [`crates/dq-exec/src/evaluator.rs`](../../../crates/dq-exec/src/evaluator.rs) (`evaluate_file`, `run_schema_check`, etc.)
  - [`crates/dq-exec/src/fixer.rs`](../../../crates/dq-exec/src/fixer.rs)
  - [`crates/dq-exec/src/schema_check.rs`](../../../crates/dq-exec/src/schema_check.rs)
  - [`crates/dq-exec/src/composite.rs`](../../../crates/dq-exec/src/composite.rs)
  
  Они должны компилироваться без изменений благодаря `Arc<T>: Deref<Target = T>` (autoderef). Если какой-то site падает с error E0599 / E0277 — добавить явный `(**engine)` или `engine.as_ref()`. Если site хочет `&JqEngine` — `engine.as_ref()`. **Не** менять семантику ни в одном месте, только типы.

## 3. Regression tests

- [ ] 3.1 [delegate to rust-cli-test-writer] В [`crates/dq-exec/src/evaluator.rs`](../../../crates/dq-exec/src/evaluator.rs) добавить unit test `cache_dedupes_identical_filters` (см. [design.md](design.md) §6.1).
  - Если для доступа к `Arc<JqEngine>` нужен `pub(crate) fn rules(&self)`-accessor — добавить (или использовать существующий, если уже есть).
  - Проверка через `Arc::ptr_eq` (два правила с тем же фильтром — одна и та же Arc-инстанция).
- [ ] 3.2 [delegate to rust-cli-test-writer] Добавить unit test `cache_does_not_collapse_different_filters`:
  - Два правила с разными filter'ами → две разные Arc-инстанции (`!Arc::ptr_eq`).
  - Простая sanity check, что cache key не glob-collapse'ит.
- [ ] 3.3 [delegate to rust-cli-test-writer] Добавить test `cache_does_not_persist_across_evaluator_news`:
  - Создать два `Evaluator::new(...)`-вызова с одним и тем же выражением.
  - `Arc::ptr_eq` между ними должен вернуть `false` (cache lexically-scoped, не глобальный).

## 4. Verification

- [ ] 4.1 [verify] `cargo test -p dq-exec` — зелёный.
- [ ] 4.2 [verify] `cargo test --workspace --all-features` — зелёный.
- [ ] 4.3 [verify] `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] 4.4 [verify] `cargo fmt --all -- --check`.
- [ ] 4.5 [verify] `cargo bench -p dq-exec --bench evaluate -- --quick` — `evaluator_new` для `@std/k8s` ≤ 6 ms. Зафиксировать after-числа в PR-описании.
- [ ] 4.6 [verify] Manual smoke: `time ./target/release/dq lint crates/dq-cli/tests/fixtures/k8s_deployment.yaml` — должно быть на 10–15 ms быстрее чем до фикса (на repeat-вызовах с warm FS-cache).

## 5. Documentation

- [ ] 5.1 [author] PR-описание содержит before/after для `evaluator_new` и `jq/recompile_same_filter/100`. Числа из бенчей.
- [ ] 5.2 [author] Один-абзац комментарий в `compile_or_cached` объясняет invariant'ы: cache — per-Evaluator, не глобальный; cache key — exact string match; Arc-shared engines безопасны для concurrent `run()` (благодаря `Send + Sync` гарантиям jaq-json `sync` feature'а).

## 6. Archive

- [ ] 6.1 После merge'a — `openspec/changes/perf-jq-compile-cache/` → `openspec/changes/archive/2026-MM-DD-perf-jq-compile-cache/`.
