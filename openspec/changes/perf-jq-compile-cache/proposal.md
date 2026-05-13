## Why

Бенч [crates/dq-transform/benches/jq.rs](../../../crates/dq-transform/benches/jq.rs) показал, что `JqEngine::compile` не амортизируется при повторных вызовах с тем же выражением:

| вызовов | время | per-call |
|---|---|---|
| 1 | 346 µs | 346 µs |
| 10 | 3.79 ms | 379 µs |
| 100 | 38.98 ms | 390 µs |

Линейный рост — никакого кеша нет. Это видно и в коде: [`JqEngine::compile`](../../../crates/dq-transform/src/jq.rs:227) каждый раз заново гоняет `jaq_core::Loader::load` → `Compiler::compile` без какой-либо памяти между вызовами.

В реальном workload'е `@std/k8s` (28 правил, ~14 уникальных jq-выражений типа `.kind == "Deployment"`, `.spec.replicas`, и т.п.) `Evaluator::new` компилирует **до 6 фильтров на правило** (`match.filter`, `loc.pointer`, `loc.file`, `loc.line`, `fix.jq`, `fix.ops`). Часть из них повторяется — например, `match.filter: '.kind == "Deployment"'` встречается в десяти правилах. При текущей архитектуре каждое из них компилируется заново.

Замер: `cargo bench -p dq-exec --bench evaluate -- --quick` показал `evaluator_new` ≈ 17 ms на `@std/k8s` — это **17 × 1 ms на правило**, что эквивалентно ~3 jq-компиляциям на правило. С кешем будет ~14 × 350 µs = **5 ms**. Экономия ~12 ms на каждый `dq lint` invocation.

Для bulk-сценариев (`dq lint --parallel N` на сотнях файлов) `Evaluator` создаётся один раз и переиспользуется через [`Arc<CompiledRule>`](../../../crates/dq-exec/src/evaluator.rs:71-79), так что экономия — оверхед `dq lint` startup'а, а не per-file. Но для часто запускающихся pipeline'ов (pre-commit hook, CI gate per push) это ощутимо.

## What Changes

- **Cache jq compile через `Arc<JqEngine>`** во время `Evaluator::new`. Cache — простой `HashMap<String, Arc<JqEngine>>`, локальный для одного вызова `Evaluator::new`, передаётся через сигнатуру `compile_rule_to_depth` и `compile_composite`. Не глобальный (per-process) — см. design.md §3.
- **Все engine-поля в `CompiledRule` становятся `Option<Arc<JqEngine>>`**:
  - `filter_engine`, `loc_pointer_engine`, `loc_file_engine`, `loc_line_engine`, `fix_engine`, `fix_ops_engine` — поэтому одна `Arc<JqEngine>` инстанция может оказаться в shared у нескольких `CompiledRule`'ов.
  - `Arc::clone` в `Evaluator::evaluate_file` / `Fixer::fix_file` — O(1) refcount-bump, никаких новых аллокаций per-run.
- **`JqEngine` остаётся как есть.** `Filter<D>` в jaq-core 3.0 не имплементит `Clone` (см. doc-комментарий в [crates/dq-transform/src/jq.rs:198-203](../../../crates/dq-transform/src/jq.rs:198)), поэтому `Arc` — единственный shared-ownership path. Никаких изменений в public API `dq-transform` (`JqEngine::compile` / `run` сигнатуры те же).
- **Регрессионный бенч-assertion** в [crates/dq-exec/benches/evaluate.rs](../../../crates/dq-exec/benches/evaluate.rs): `evaluator_new` для `@std/k8s` ≤ 6 ms (after-target ~5 ms + 20% headroom).
- **Anti-scope (явно НЕ входит):**
  - Глобальный per-process кеш (`OnceLock<DashMap<...>>` в `dq-transform`). Подход рассмотрен и отвергнут — см. design.md §3.
  - Eviction / TTL — кеш живёт только во время одного `Evaluator::new`. Память освобождается, когда `Evaluator` дропается. Filter strings ограничены `Evaluator`-scope'ом (≤ 6 × число правил).
  - Кеширование `glob_matcher` / JSON Schema'ы — те уже амортизированы (glob — один call per rule, schema — pointer-to-static `serde_yml::Value`), бенч не показал их в боттлнеках.
  - One-shot CLI entry-point'ы: [`dq query EXPR`](../../../crates/dq-cli/src/commands/query.rs:79), [`dq set --jq EXPR`](../../../crates/dq-cli/src/commands/set.rs:368), `dq-plugin runtime`. Это однократные компиляции; кеш бесполезен. Не трогаем.

## Impact

- **Affected specs:** ничего не меняется. `data-query-exec` спека описывает `Evaluator::new` через семантику (compile-or-fail upfront), а не через сложность.
- **Affected code:**
  - [`crates/dq-exec/src/evaluator.rs`](../../../crates/dq-exec/src/evaluator.rs) — `CompiledRule` field'ы переезжают на `Option<Arc<JqEngine>>`, `compile_rule_to_depth` берёт `&mut HashMap<String, Arc<JqEngine>>` как параметр, `Evaluator::new` инициализирует кеш и протаскивает его через цикл по rule'ам. ~30 строк изменений.
  - [`crates/dq-exec/src/composite.rs`](../../../crates/dq-exec/src/composite.rs) — `compile_composite` берёт тот же `&mut HashMap` и протаскивает его в рекурсивный `compile_rule_to_depth` для nested rule'ов. ~15 строк.
  - [`crates/dq-exec/src/fixer.rs`](../../../crates/dq-exec/src/fixer.rs), [`crates/dq-exec/src/schema_check.rs`](../../../crates/dq-exec/src/schema_check.rs), сам [`crates/dq-exec/src/evaluator.rs`](../../../crates/dq-exec/src/evaluator.rs) — call-site'ы `engine.run(...)` меняются на `(*engine).run(...)` (через `Arc::as_ref`) или просто `engine.run(...)` если `Arc<T>` имплементит deref до `&T` (так и есть). Поэтому изменения чисто компилятор-driven.
  - [`crates/dq-exec/benches/evaluate.rs`](../../../crates/dq-exec/benches/evaluate.rs) — без изменений; этот же бенч даёт before/after.
- **User-visible:**
  - `dq lint`/`dq fix` startup на больших ruleset'ах ускоряется на ~10–15 ms. Заметнее всего на `@std/k8s` (28 правил с пересечениями).
  - Никаких изменений в выводе, exit code'ах, flag'ах.
- **Downstream consumers:** zero risk. `Evaluator` / `CompiledRule` — `pub(crate)` внутри `dq-exec`; public API `dq-exec` (`Evaluator::new`, `Evaluator::evaluate_file`, `Diagnostic`) — не меняется.

## Reference

- Бенч-вывод из [crates/dq-transform/benches/jq.rs](../../../crates/dq-transform/benches/jq.rs) и [crates/dq-exec/benches/evaluate.rs](../../../crates/dq-exec/benches/evaluate.rs) — фиксирует before-метрики.
- Doc-комментарий в [crates/dq-transform/src/jq.rs:198-203](../../../crates/dq-transform/src/jq.rs:198) уже описывает мотивацию для `Arc<JqEngine>` в контексте rayon-driven bulk path'а (`dq set --jq … --parallel N`). Этот change расширяет тот же приём на compile-time deduplication.
- Структура `CompiledRule` и call-chain — см. [design.md](design.md) §1.
