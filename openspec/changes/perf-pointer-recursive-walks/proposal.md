## Why

Бенч [crates/dq-core/benches/pointer.rs](../../../crates/dq-core/benches/pointer.rs) показал, что `Pointer::with_segment` нелинеен по глубине пути:

| глубина | время | соотношение |
|---|---|---|
| 1 | 65 ns | baseline |
| 4 | 750 ns | ×11.5 (для ×4 глубины — ожидалось ×4) |
| 16 | 8.3 µs | ×125 (ожидалось ×16) |
| 64 | **115 µs** | **×1772 (ожидалось ×64)** |

Причина — [crates/dq-core/src/pointer.rs:62-66](../../../crates/dq-core/src/pointer.rs:62):

```rust
pub fn with_segment(&self, seg: Segment) -> Self {
    let mut segs = self.0.clone();      // ← O(current depth)
    segs.push(seg);
    Self(segs)
}
```

Каждый вызов клонирует `Vec<Segment>` целиком. При рекурсивной сборке пути от корня до листа на глубине `n` суммарная стоимость — Σ k for k in 1..n = **O(n²)**.

Hot paths, в которых это превращается в реальную потерю:
- [`transform::diff` ↔ `diff_maps` / `diff_arrays`](../../../crates/dq-core/src/transform/diff.rs:95) — шесть `with_segment` call-site'ов в рекурсивном walk'е через дерево. Для документа глубиной `d` с `N` листьями стоимость — O(N × d²) вместо целевого O(N × d).
- [`transform::merge::merge_into`](../../../crates/dq-core/src/transform/merge.rs:58) — один `with_segment` на каждый ключ patch'а, рекурсивно. Same shape.
- [`dq-plugin::runtime`](../../../crates/dq-plugin/src/runtime.rs:170) — два call-site'а, but it's one-level (не рекурсивно), на горячем пути не сидит. Cold-ish.

Реальный ущерб: для типичного K8s-deployment с depth ~6 и ~200 листьями, текущая стоимость `diff(a, b)` — ~200 × 36 = 7200 segment-copy'ев. С линейным walk'ом было бы ~200 × 6 = 1200. Разница в 6× — на больших Helm-values'ах (depth 10+, листьев тысячи) становится заметной (~миллисекунды на diff). Регрессионные `dq diff` бенчмарки пока не существуют, но `parse_yaml`-snapshot'ы на helm-values уже на грани p99 = 100 ms threshold'а в [crates/dq-cli/tests/](../../../crates/dq-cli/tests/), который мы периодически обходим snapshot'ами.

## What Changes

- **Добавить mutation API на `Pointer`** в [crates/dq-core/src/pointer.rs](../../../crates/dq-core/src/pointer.rs):
  - `pub fn push_segment(&mut self, seg: Segment)` — O(1) amortized (внутренний `Vec::push`).
  - `pub fn pop_segment(&mut self) -> Option<Segment>` — O(1).
  - Существующий `with_segment(&self, seg) -> Self` **оставляем** — у него правильная семантика для callers'ов, которым нужен owned-result (один call-site в `dq-plugin/runtime.rs`). Cost остаётся O(d) per call, но это не hot path.
- **Рефакторинг recursive walk'ов** на push/pop:
  - [`crates/dq-core/src/transform/diff.rs`](../../../crates/dq-core/src/transform/diff.rs) — все шесть `path.with_segment(...)` call-site'ов (стр. 95, 104, 113, 123, 133, 142) переходят на `path.push_segment(...)` + `path.pop_segment()` в scope guard'е. `path` принимается как `&mut Pointer` вместо `&Pointer`.
  - [`crates/dq-core/src/transform/merge.rs`](../../../crates/dq-core/src/transform/merge.rs) — call-site `base.with_segment(...)` на стр. 58 → push/pop. `base` — `&mut Pointer` вместо `&Pointer`.
  - [`crates/dq-plugin/src/runtime.rs`](../../../crates/dq-plugin/src/runtime.rs) — **не трогаем**. Это не рекурсивный walk, один-level extension; рефакторинг под push/pop увеличит код без win'а.
- **Регрессионные тесты:**
  - Существующий бенч `pointer/with_segment/64` остаётся как есть (это микро-бенч **именно** `with_segment`, его поведение не меняется — мы не оптимизируем `with_segment`, мы делаем рекурсивные walk'и через push/pop). 
  - **Новый бенч** `pointer/recursive_walk/{depth}` в [crates/dq-core/benches/pointer.rs](../../../crates/dq-core/benches/pointer.rs): моделирует ровно паттерн `diff`/`merge` — push, recurse, pop. After-target: линейный в глубине.
  - Property-test, что `with_segment` и push/pop эквивалентны observably (одинаковый итоговый pointer при тех же seg-последовательностях). Размещается в [crates/dq-core/tests/prop_pointer.rs](../../../crates/dq-core/tests/prop_pointer.rs) рядом с существующими property-tests.
- **Anti-scope (явно НЕ входит):**
  - Замена `Vec<Segment>` на `Arc<Vec<Segment>>` или `im::Vector`. Альтернатива рассмотрена и отвергнута — см. design.md §3. `Pointer::clone` сейчас в hot path'ах НЕ доминирует, и `Arc`-overhead на refcount'ах на каждый `pointer_segments()` или `is_root()` query плохо amortise'ится.
  - `SmallVec<[Segment; 8]>` — частичный win для shallow путей, но всё равно O(n²) при depth > 8. Не закрывает корневую проблему.
  - Изменение `Segment` enum'а (например, hold'ить `Cow<str>` вместо `String`). Отдельная история про key-allocation; не блокер.

## Impact

- **Affected specs:** ничего не меняется. `Pointer` — utility type, не часть OpenSpec-описанного контракта.
- **Affected code:**
  - [`crates/dq-core/src/pointer.rs`](../../../crates/dq-core/src/pointer.rs) — +2 public method'а (`push_segment`, `pop_segment`); ~10 строк.
  - [`crates/dq-core/src/transform/diff.rs`](../../../crates/dq-core/src/transform/diff.rs) — рефакторинг шести call-site'ов; ~20 строк изменений (зависит от стиля).
  - [`crates/dq-core/src/transform/merge.rs`](../../../crates/dq-core/src/transform/merge.rs) — рефакторинг одного call-site'а + сигнатуры `merge_into`; ~5 строк.
  - [`crates/dq-core/tests/prop_pointer.rs`](../../../crates/dq-core/tests/prop_pointer.rs) — +1 property test.
  - [`crates/dq-core/benches/pointer.rs`](../../../crates/dq-core/benches/pointer.rs) — +1 bench group `recursive_walk`.
- **User-visible:**
  - `dq diff a.json b.json` — заметно быстрее на глубоких документах. На plain K8s-deployment'ах эффект < 1 ms (не заметен пользователю); на 10-level deep Helm values с тысячами листьев — миллисекунды.
  - `dq merge` / `dq patch` (через `apply_merge` / `apply_patch`) — то же самое.
  - Никаких изменений в выводе, exit code'ах, flag'ах.
- **Downstream consumers:** zero risk. Public API `Pointer` остаётся обратно-совместимым (`with_segment` оставлен, новые методы — добавочные). `dq-plugin`, `dq-cli`, и любые external консьюмеры компилируются без изменений.

## Reference

- Бенч-вывод из [crates/dq-core/benches/pointer.rs](../../../crates/dq-core/benches/pointer.rs) — фиксирует before-метрики.
- Альтернативы (Arc-storage, SmallVec) и почему они отвергнуты — см. [design.md](design.md) §3.
- Existing pattern: `IndexMap::shift_insert_with_capacity` и подобные in-place mutation API в стандартной библиотеке Rust — добавление `push_segment`/`pop_segment` следует тому же шаблону "expose mutation API parallel to functional one".
