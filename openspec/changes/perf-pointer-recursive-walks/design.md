# Design — Push/pop walk pattern for `diff` and `merge`

## 1. Pattern outline

Текущий код в [`transform/diff.rs:113-120`](../../../crates/dq-core/src/transform/diff.rs:113) (упрощённый):

```rust
fn diff_maps(path: &Pointer, a: &IndexMap<String, Value>, b: &IndexMap<String, Value>, ops: &mut Vec<PatchOp>) {
    for (k, vb) in b {
        match a.get(k) {
            Some(va) => {
                let child = path.with_segment(Segment::Key(k.clone()));  // ← O(depth)
                diff(&child, va, vb, ops);                                // ← recurses
            }
            None => {
                ops.push(PatchOp::Add { path: path.with_segment(...), value: vb.clone() });
            }
        }
    }
}
```

Каждый виток цикла создаёт `child`-pointer полным копированием родительского `Vec<Segment>`. При глубине `d` каждый `with_segment` — O(d) аллокация + копирование. На листе глубиной `D` суммарная цена пути — Σ k for k in 1..D = O(D²).

После рефакторинга:

```rust
fn diff_maps(path: &mut Pointer, a: &IndexMap<String, Value>, b: &IndexMap<String, Value>, ops: &mut Vec<PatchOp>) {
    for (k, vb) in b {
        path.push_segment(Segment::Key(k.clone()));                       // ← O(1) amortized
        match a.get(k) {
            Some(va) => diff(path, va, vb, ops),                          // recurses with mutated path
            None => ops.push(PatchOp::Add { path: path.clone(), value: vb.clone() }),
        }
        path.pop_segment();                                                // ← O(1)
    }
}
```

`path.clone()` остаётся (в `Add`/`Remove` ops — emit нужно owned `Pointer`), но это **один** O(d) clone per emitted op, а не O(d) per traversal step. Стоимость пути теперь — O(D) на путь, O(N × D) на полный walk (N леафов, средняя глубина D).

## 2. Push/pop API design

```rust
impl Pointer {
    /// Append a segment in place. Mirrors `Vec::push`. O(1) amortized.
    pub fn push_segment(&mut self, seg: Segment) {
        self.0.push(seg);
    }

    /// Remove and return the last segment in place. Mirrors `Vec::pop`. O(1).
    /// Returns `None` when called on a root pointer.
    pub fn pop_segment(&mut self) -> Option<Segment> {
        self.0.pop()
    }
}
```

Прямые wrapper'ы поверх `Vec<Segment>`'s API. Никаких инвариантов, помимо тех, что уже есть в `Pointer::new`: любой `Vec<Segment>` — валидный pointer (RFC 6901 escape'ы решаются в `as_canonical`, а не на структурном уровне).

**Не** делаем guard-pattern (`SegmentGuard<'a>` со встроенным `pop` в `Drop`) — это переусложняет site'ы; вызывающий код в `diff_maps`/`merge_into` достаточно простой, чтобы вручную балансить push/pop. Если когда-нибудь появится 5-й рекурсивный call-site — пересмотрим.

## 3. Альтернативы — отвергнуто

### 3.1 `Arc<Vec<Segment>>` для cheap-clone

Идея: `Pointer(Arc<Vec<Segment>>)`. `clone()` становится O(1), `with_segment` всё ещё O(d), но parent-pointer переиспользуется в рекурсии (siblings share parent).

**Почему отвергнуто:**

- Каждый `with_segment` всё равно копирует `Vec<Segment>` (потому что Arc-inner immutable; чтобы push'нуть, нужно `Arc::make_mut` который копирует если refcount > 1). Для diff'а, где параллельно живут `a_path` и `b_path` от одного parent'а — refcount всегда > 1, поэтому `make_mut` copies. Net: O(d) per call, как было.
- Single-deep-pointer микро-бенч `pointer/with_segment/64` остаётся O(n²) — `Arc` не помогает, потому что бенч строит ровно одну цепочку, не tree.
- Все query-методы (`segments()`, `is_root()`, `as_canonical()`) добавляют atomic refcount-load (Acquire). Не катастрофа, но и не free.
- `Pointer: Hash + Eq` через `Arc<Vec<_>>`: совместимо, но `hash` всё равно обходит весь content. Так что cache lookups (например, `HashMap<Pointer, _>` в diagnostic dedup) — те же self-time'ы.

Чистая просадка по сравнению с push/pop, который **избегает** allocation'ов вовсе на hot-path'е.

### 3.2 `SmallVec<[Segment; 8]>`

Идея: stack-allocate'ить первые 8 сегментов, heap'ить остальное.

**Почему отвергнуто:**

- При depth ≤ 8 `with_segment` всё равно копирует `[Segment; 8]` (8 enum'ов с `String` inside — это не бесплатно, ~96 bytes copy + 8 refcount-bump'ов через `String::clone`).
- При depth > 8 — O(n²) возвращается.
- Push/pop fix решает обе категории глубин одной операцией.

`SmallVec` имеет смысл для одиночных `Pointer`-аллокаций на heap (когда `Pointer::clone` массово делается на shallow путях), но это не наш hot path.

### 3.3 `im::Vector<Segment>` / `imbl::Vector<Segment>` (persistent vector)

Идея: персистентная коллекция с O(log n) push и O(1) clone.

**Почему отвергнуто:**

- O(log n) push *worse* чем O(1) push в `Vec`. Win только при cheap-clone сценариях (которых у нас почти нет в `diff`/`merge` — мы push'аем и dis'card'им parent сразу).
- Внешняя dependency (`imbl` ≈ 50KB compiled). cargo-deny audit (новая license-check'а в [deny.toml](../../../deny.toml)).
- Hash/Eq invariants — нужно проверить, что persistent-vector hash совместим с byte-equivalent `Vec<Segment>`. Сейчас derived hash через `[T]::hash` — стандартный. У `imbl::Vector` — TBD.

Слишком много overhead'а за нулевой win в наших workload'ах.

## 4. Why we keep `with_segment(&self) -> Self`

[`dq-plugin/src/runtime.rs:170-173`](../../../crates/dq-plugin/src/runtime.rs:170) использует:

```rust
let canonical_keys = obj.keys()
    .map(|k| pointer.with_segment(Segment::Key(k.clone())).as_canonical())
    .collect::<Vec<_>>();
```

Здесь call-site хочет owned `String` через `as_canonical()` после `with_segment` — нельзя удобно сделать через `&mut`. Можно было бы:

```rust
let canonical_keys = obj.keys()
    .map(|k| {
        pointer.push_segment(Segment::Key(k.clone()));
        let s = pointer.as_canonical();
        pointer.pop_segment();
        s
    })
    .collect::<Vec<_>>();
```

но это требует `&mut pointer` в plugin runtime'е, который сейчас `&Pointer` (immutable). Plugin call — не recursive walk, ровно один уровень углубления, поэтому stays O(d). Не трогаем — не hot path.

## 5. Test plan

### 5.1 Property test (correctness)

В [`crates/dq-core/tests/prop_pointer.rs`](../../../crates/dq-core/tests/prop_pointer.rs):

```rust
proptest! {
    #[test]
    fn push_pop_matches_with_segment(segs in prop::collection::vec(any_segment(), 0..32)) {
        // Build via with_segment chain.
        let mut chained = Pointer::default();
        for s in &segs { chained = chained.with_segment(s.clone()); }
        
        // Build via push_segment.
        let mut mutated = Pointer::default();
        for s in &segs { mutated.push_segment(s.clone()); }
        
        prop_assert_eq!(chained, mutated);
    }
    
    #[test]
    fn push_then_pop_is_identity(start in any_pointer(), seg in any_segment()) {
        let mut p = start.clone();
        p.push_segment(seg);
        let popped = p.pop_segment();
        prop_assert_eq!(p, start);
        prop_assert!(popped.is_some());
    }
}
```

### 5.2 Unit tests для push/pop

В `pointer.rs::tests` (inline tests):
- `push_then_pop_round_trips` — empty → push → pop → empty.
- `pop_on_empty_returns_none` — `Pointer::default().pop_segment() == None`.
- `push_segment_works_on_default` — root → push → segments == [seg].

### 5.3 Regression bench

В [`crates/dq-core/benches/pointer.rs`](../../../crates/dq-core/benches/pointer.rs) добавить:

```rust
fn bench_recursive_walk(c: &mut Criterion) {
    let mut group = c.benchmark_group("pointer/recursive_walk");
    for &depth in &[1usize, 4, 16, 64] {
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &d| {
            b.iter(|| {
                let mut p = Pointer::default();
                // Simulate diff's push/pop pattern at depth d.
                for i in 0..d {
                    p.push_segment(Segment::Index(i));
                }
                let canon = black_box(p.as_canonical());
                for _ in 0..d {
                    p.pop_segment();
                }
                canon
            });
        });
    }
    group.finish();
}
```

After-target: линейный в `depth`. Если depth 64 займёт > 5× от depth 16 — регрессия.

### 5.4 Behaviour tests для `diff`/`merge`

Существующие в [`crates/dq-core/tests/`](../../../crates/dq-core/tests/) для `diff`, `apply_patch`, `apply_merge` (включая property-tests, если есть). Все должны пройти байт-в-байт — рефакторинг не меняет semantics.

## 6. Risk assessment

- **Correctness:** **низкий**. push/pop — прямые wrapper'ы поверх `Vec::push`/`Vec::pop`. Property-test §5.1 формально доказывает equivalence.
- **API stability:** **нулевой риск**. `Pointer::with_segment` остаётся. Новые public методы `push_segment`/`pop_segment` — additive.
- **Performance:** **только win**. Worst case — push/pop в hot path заменяет clone-based emission, который НЕ имел O(n²) проблемы изначально (например, plugin runtime). Там оставляем `with_segment`.
- **Build:** **zero** new dependencies.
- **Concurrency:** **N/A**. `Pointer` не пересекается между потоками без явного `clone`.
- **Test pollution:** **none**. Изменения localизованы в `dq-core`, ни один тест из других crate'ов не должен зависеть от deep-pointer'ов в горячем коде.

## 7. Estimated impact

For `transform::diff` on a typical Helm-values document (depth ~10, ~500 leaves):

- Before: ~500 × 100 segment-copies = 50 000 ops.
- After: ~500 × 10 segment-copies (только на emit Add/Remove) + linear walk = ~5 000 ops + (push+pop) constant.

10× win. Wall-time effect — пара миллисекунд на типичных входах, ощутимо на больших Helm-values'ах и больших K8s manifests.
