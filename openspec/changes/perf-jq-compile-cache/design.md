# Design — Compile-time jq cache in `Evaluator::new`

## 1. Current compile chain

[`Evaluator::new(rulesets)`](../../../crates/dq-exec/src/evaluator.rs:201) — единственный production-entry для компиляции. Внутри:

```text
Evaluator::new
└─ for set in rulesets:
   └─ for rule in set.rules:
      └─ compile_rule_to_depth(rule, source, 0, MAX_EXTRACT_DEPTH)
         ├─ JqEngine::compile(rule.match.filter)      # if present
         ├─ compile_check(rule, source, …)
         │  └─ for jq-variant check: JqEngine::compile(rule.check.jq)
         │  └─ for composite: compile_composite(...)
         │     └─ JqEngine::compile(rule.check.extract)
         │     └─ compile_rule_to_depth(nested_rule, ...)  # ↻ recursion
         ├─ JqEngine::compile(rule.loc.pointer)        # if present
         ├─ JqEngine::compile(rule.loc.file)           # if present
         ├─ JqEngine::compile(rule.loc.line)           # if present
         ├─ JqEngine::compile(rule.fix.jq)             # if present
         └─ JqEngine::compile(rule.fix.ops)            # if present
```

`compile_rule_to_depth` определена в [evaluator.rs:331–428](../../../crates/dq-exec/src/evaluator.rs:331), `compile_composite` — в [composite.rs:112+](../../../crates/dq-exec/src/composite.rs:112). Это **два** call-site'а, в которые надо протащить cache.

Профиль `@std/k8s` (28 правил):
- `match.filter` уникальных значений: ~10 (фильтры типа `.kind == "Deployment"`, `.kind == "Service"` и т.п. встречаются в 2–4 правилах каждый).
- `loc.pointer` уникальных: ~4.
- `check.jq` уникальных: ~12.
- Итого ~26 уникальных выражений на ~80 compile-call'ов. Cache hit-rate ~67%.

## 2. Cache shape

```rust
// In dq-exec/src/evaluator.rs
type JqCache = std::collections::HashMap<String, std::sync::Arc<dq_transform::JqEngine>>;

fn compile_or_cached(
    cache: &mut JqCache,
    expr: &str,
    rule_id: &str,
) -> Result<Arc<JqEngine>> {
    if let Some(engine) = cache.get(expr) {
        return Ok(Arc::clone(engine));
    }
    let engine = Arc::new(JqEngine::compile(expr).map_err(|err| ExecError::RuleCompile {
        rule_id: rule_id.to_string(),
        source: err,
    })?);
    cache.insert(expr.to_string(), Arc::clone(&engine));
    Ok(engine)
}
```

Все шесть call-site'ов `JqEngine::compile` в `compile_rule_to_depth` заменяются на `compile_or_cached(cache, expr, &rule.id)?`. `compile_check`/`compile_composite` берут тот же `&mut JqCache`.

Сигнатуры:
```rust
pub(crate) fn compile_rule_to_depth(
    rule: Rule,
    source: &RuleSource,
    current_depth: usize,
    max_depth: usize,
    cache: &mut JqCache,                         // ← new
) -> Result<CompiledRule>;

pub(crate) fn compile_composite(
    outer_rule_id: &str,
    extract: &str,
    nested: &Rule,
    message: &str,
    source: &RuleSource,
    current_depth: usize,
    max_depth: usize,
    cache: &mut JqCache,                         // ← new
) -> Result<CompiledCompositeCheck>;
```

`Evaluator::new` создаёт пустой cache, передаёт его в каждый `compile_rule_to_depth(...)`. Cache живёт только до завершения `Evaluator::new` (lexically scoped) — после возврата он дропается, освобождая `String` ключи и owned-Arc'и. `CompiledRule`'ы держат свои `Arc<JqEngine>` копии.

## 3. Почему не глобальный кеш

Альтернатива — `static JQ_CACHE: OnceLock<RwLock<HashMap<String, Arc<JqEngine>>>>` в `dq-transform`. **Отвергнуто** по трём причинам:

1. **Memory leak в долгоиграющих процессах.** `dq` сейчас — short-lived CLI, но если в будущем кто-то встроит `dq-exec` в long-running daemon (LSP-server-style?), глобальный append-only кеш — утечка. Per-Evaluator кеш освобождается естественно.
2. **Test pollution.** Тесты, которые компилируют умышленно невалидные выражения для проверки `Err(...)`-веток, не должны влиять на соседние тесты. С глобальным кешем нужна `clear()` логика, проактивный thread-local'ing, или вместо `static` — `Lazy<RwLock<...>>` + `with_jq_cache_scope(|| { ... })` builder. Лишний overhead.
3. **Никакой пользы для bulk-cases.** Per-Evaluator cache уже даёт 100% hit-rate в bulk-режиме, потому что `Evaluator` строится один раз и переиспользуется через `Arc<CompiledRule>` (см. [crates/dq-cli/src/bulk.rs:240+](../../../crates/dq-cli/src/bulk.rs)). Глобальный cache даёт хит только для **двух последовательных** `dq lint` вызовов в одном процессе — несуществующий use case для CLI.

Per-Evaluator cache — sweet spot: простой, локальный, нулевой риск.

## 4. CompiledRule field migration

Текущая структура (`evaluator.rs:133–156`):

```rust
pub(crate) struct CompiledRule {
    pub(crate) rule: Rule,
    pub(crate) filter_engine: Option<JqEngine>,
    pub(crate) check: CompiledCheck,
    pub(crate) glob_matcher: Option<GlobMatcher>,
    pub(crate) loc_file_engine: Option<JqEngine>,
    pub(crate) loc_line_engine: Option<JqEngine>,
    pub(crate) loc_pointer_engine: Option<JqEngine>,
    pub(crate) fix_engine: Option<JqEngine>,
    pub(crate) fix_ops_engine: Option<JqEngine>,
}
```

После:

```rust
pub(crate) struct CompiledRule {
    pub(crate) rule: Rule,
    pub(crate) filter_engine: Option<Arc<JqEngine>>,
    pub(crate) check: CompiledCheck,
    pub(crate) glob_matcher: Option<GlobMatcher>,
    pub(crate) loc_file_engine: Option<Arc<JqEngine>>,
    pub(crate) loc_line_engine: Option<Arc<JqEngine>>,
    pub(crate) loc_pointer_engine: Option<Arc<JqEngine>>,
    pub(crate) fix_engine: Option<Arc<JqEngine>>,
    pub(crate) fix_ops_engine: Option<Arc<JqEngine>>,
}
```

`CompiledCheck` — отдельный enum в [evaluator.rs](../../../crates/dq-exec/src/evaluator.rs); там есть варианты `Jq`, `Schema`, `Composite`. У `Jq` есть поле `engine: JqEngine` — мигрирует так же на `Arc<JqEngine>`. У `Composite` есть `extract_engine: JqEngine` — тоже.

Все call-site'а `(*engine).run(...)` или `engine.run(...)` через `Arc<T>: Deref<Target = T>` работают без изменений.

## 5. Call-site shape (read side)

Текущий код в `Evaluator::evaluate_file` ([evaluator.rs:261+](../../../crates/dq-exec/src/evaluator.rs:261)) и `Fixer::fix_file` ([fixer.rs](../../../crates/dq-exec/src/fixer.rs)) делает что-то вроде:

```rust
if let Some(engine) = &rule.filter_engine {
    let results = engine.run(&value)?;       // method-call через Deref
    // ...
}
```

После миграции тот же код работает — `&Arc<JqEngine>` autoderef'ится в `&JqEngine` для вызова `.run(...)`. Компилятор не выдаст diff.

Места, где есть `engine.clone()` (если такие есть — нужно проверить grep'ом), станут O(1) refcount-bump'ом. Net win.

## 6. Test plan

### 6.1 Unit-level

[`crates/dq-exec/src/evaluator.rs`](../../../crates/dq-exec/src/evaluator.rs) — добавить unit test `cache_dedupes_identical_filters`:

```rust
#[test]
fn cache_dedupes_identical_filters() {
    let yaml = r#"
- id: rule_a
  match: { format: yaml, filter: '.kind == "Deployment"' }
  check: { jq: 'true' }
  message: 'a'
- id: rule_b
  match: { format: yaml, filter: '.kind == "Deployment"' }  # same filter
  check: { jq: 'true' }
  message: 'b'
"#;
    let set = RuleSet::from_str(yaml, RuleSource::Inline).unwrap();
    let evaluator = Evaluator::new(vec![set]).unwrap();
    let (a, b) = (
        &evaluator.rules()[0].filter_engine,
        &evaluator.rules()[1].filter_engine,
    );
    assert!(Arc::ptr_eq(
        a.as_ref().unwrap(),
        b.as_ref().unwrap()
    ));
}
```

Требует `pub(crate) fn rules(&self) -> &[Arc<CompiledRule>]` accessor — он уже есть для composite tests или добавляется тривиально.

### 6.2 Regression bench

[`crates/dq-exec/benches/evaluate.rs`](../../../crates/dq-exec/benches/evaluate.rs) — существующий бенч `evaluator_new` уже измеряет это. После фикса `--quick`-вывод должен показать ≤ 6 ms против текущих ~17 ms. PR-описание содержит сравнение.

### 6.3 Workspace-wide

Поскольку миграция меняет тип `engine.run(...)` call-site'ов (autoderef работает, но компилятор может ругнуться на конкретные `&JqEngine` lifetime'ы в редких местах), critical что `cargo test --workspace --all-features` зелёный. Не должно быть skipped'ов.

## 7. Alternatives rejected

### 7.1 Sentinel `IDENTITY: Lazy<JqEngine>` для `.`

Идея: компилировать `.` (трехсимвольный no-op filter) один раз глобально через `Lazy`, потому что он встречается в 5+ правилах как noop-fix. **Отвергнуто** — частный случай, общий cache решает то же самое единообразно.

### 7.2 Shadow `JqEngine::compile_cached(expr: &str) -> Arc<JqEngine>`

Идея: добавить static cache внутри `dq-transform::jq` и экспортировать `compile_cached` параллельно с `compile`. **Отвергнуто** — это глобальный cache в маскировке, с теми же проблемами (см. §3). Также увеличивает public API `dq-transform`, который сейчас минимальный.

### 7.3 Hash-based deduplication через `BTreeMap<u64, Arc<JqEngine>>`

Идея: ключ кеша — FNV/xxhash от выражения, а не сама строка, чтобы избежать аллокации `String::from(expr)` для cache lookup. **Отвергнуто** — `HashMap<String, _>::get(expr)` берёт `&str` через `Borrow`-trait, аллокации нет на cache hit. На cache miss — одна `String::from(expr)` на insert, ничтожно. Хэш-ключи дают коллизии с probability ~2^-32, против нулевой у `String`-ключей. Не стоит.

## 8. Risk assessment

- **Correctness:** низкий. `Arc<JqEngine>` имплементит `Deref<Target = JqEngine>`, поэтому method-call sites компилируются без изменений. `JqEngine::run` — `&self` метод (без мутации), и `Arc` shared-ownership безопасен.
- **Memory:** ~+8 байт на каждый engine field (Arc-overhead). На 28-rule `@std/k8s` это ~1.5 КБ. Незаметно.
- **Concurrency:** `jaq-json` с feature `sync` swap'ит `Rc` на `Arc` внутри `Filter`, делая `JqEngine: Send + Sync`. `Arc<JqEngine>` тоже `Send + Sync`. Rayon-driven `Evaluator::evaluate_file` (если/когда такой будет) работает без блокировок.
- **Compilation time:** zero impact — cache не уменьшает workload компилятора, только runtime.
- **Test stability:** проверить, что юниты в [crates/dq-transform/src/jq.rs:743–820](../../../crates/dq-transform/src/jq.rs:743) (тесты для `JqEngine::compile`) продолжают работать — они не используют `Evaluator`, поэтому fix их не касается. Но прогон `cargo test -p dq-transform` обязателен в verification.
