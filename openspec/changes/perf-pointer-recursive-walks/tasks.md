# Tasks

## 1. Diagnose (before code)

- [x] 1.1 [author] Перезапустить `cargo bench -p dq-core --bench pointer -- --quick` на чистом branch'е и зафиксировать before-числа в [design.md](design.md) §7 если они расходятся с теми, что в proposal.md.
- [x] 1.2 [author] Добавить новый bench-group `pointer/recursive_walk/{1,4,16,64}` в [crates/dq-core/benches/pointer.rs](../../../crates/dq-core/benches/pointer.rs) **до** изменений в production-коде. Запустить — зафиксировать before-числа (этот бенч моделирует ровно hot-path; до фикса он должен показать O(n²) скейлинг).

## 2. Implementation — pointer API

- [x] 2.1 [delegate to rust-cli-writer] В [`crates/dq-core/src/pointer.rs`](../../../crates/dq-core/src/pointer.rs) сразу после `impl Pointer { ... pub fn with_segment(...) ... }` (~стр. 62) добавить:
  ```rust
  /// Append a segment in place. O(1) amortized.
  ///
  /// Use this in recursive walks that need to extend the pointer for
  /// descent and shrink it afterwards — pair with [`Pointer::pop_segment`].
  /// For the functional "build a new owned pointer" use case, see
  /// [`Pointer::with_segment`].
  pub fn push_segment(&mut self, seg: Segment) {
      self.0.push(seg);
  }
  
  /// Remove and return the last segment in place. O(1).
  /// Returns `None` when called on a root pointer.
  pub fn pop_segment(&mut self) -> Option<Segment> {
      self.0.pop()
  }
  ```
  Никаких других изменений в API.

## 3. Implementation — diff refactor

- [x] 3.1 [delegate to rust-cli-writer] В [`crates/dq-core/src/transform/diff.rs`](../../../crates/dq-core/src/transform/diff.rs) поменять сигнатуры внутренних helper'ов с `path: &Pointer` на `path: &mut Pointer`:
  - `diff` (top-level entry — оставить публичный API `diff(a, b) -> Vec<PatchOp>` неизменным, но внутри сразу создавать `let mut path = Pointer::default()` и передавать в recursive helper).
  - `diff_maps`, `diff_arrays` (точные имена — см. файл; есть приватный recursive helper, в который надо протащить `&mut`).
- [x] 3.2 [delegate to rust-cli-writer] Заменить все шесть `path.with_segment(...)` call-site'ов на стр. 95, 104, 113, 123, 133, 142 паттерном:
  ```rust
  path.push_segment(seg);
  // ... recurse / emit ...
  path.pop_segment();
  ```
  Где требуется emit (`PatchOp::Add { path: ... , ... }`), сделать `path.clone()` **внутри** push/pop окна — это даёт правильный owned `Pointer`.
  
  Балансировать push/pop вручную — каждый `push` должен match'нуть с `pop` в конце scope'а. Использовать early-return осторожно; если loop'у нужен `continue` после push'а — не забыть pop. Для надёжности — в кажный loop iteration делать push **перед** match/if и pop **после**.

## 4. Implementation — merge refactor

- [x] 4.1 [delegate to rust-cli-writer] В [`crates/dq-core/src/transform/merge.rs`](../../../crates/dq-core/src/transform/merge.rs) поменять `merge_into` (или эквивалентный recursive helper) с `base: &Pointer` на `base: &mut Pointer`. Заменить call-site `base.with_segment(...)` на стр. 58 на push/pop pattern.
- [x] 4.2 [delegate to rust-cli-writer] Если `apply_merge` (top-level, [merge.rs:37](../../../crates/dq-core/src/transform/merge.rs:37)) использует `base.clone()` или нет — оставить public API того же shape'а (`apply_merge(doc: &mut Document, patch: &Value) -> Result<()>`). Внутри сразу создавать `Pointer::default()` и передавать как `&mut`.

## 5. Regression tests

- [x] 5.1 [delegate to rust-cli-test-writer] В [`crates/dq-core/tests/prop_pointer.rs`](../../../crates/dq-core/tests/prop_pointer.rs) добавить property tests:
  - `push_pop_matches_with_segment(segs)` — сравнить equality между `Pointer` собранным через `with_segment`-цепочку и через `push_segment`-цикл. См. [design.md](design.md) §5.1 для точной формы.
  - `push_then_pop_is_identity(start, seg)` — после push+pop pointer идентичен исходному.
- [x] 5.2 [delegate to rust-cli-test-writer] В inline-test'ах в [`crates/dq-core/src/pointer.rs`](../../../crates/dq-core/src/pointer.rs) (`#[cfg(test)] mod tests`) добавить unit-tests:
  - `push_then_pop_round_trips` — empty pointer push'нутый и поп'нутый идентичен `Pointer::default()`.
  - `pop_on_empty_returns_none` — `Pointer::default().pop_segment() == None`.
  - `push_segment_extends_segments` — после push, `segments().last()` == push'нутый сегмент.
- [x] 5.3 [delegate to rust-cli-test-writer] Убедиться, что существующие тесты в [`crates/dq-core/tests/`](../../../crates/dq-core/tests/) для `transform::diff` (см. `transform_*.rs`-файлы) и `apply_merge` (там же) проходят без изменений. Никаких новых fixture'ов; рефакторинг должен быть наблюдаемо invisible.

## 6. Verification

- [x] 6.1 [verify] `cargo test -p dq-core` — зелёный.
- [x] 6.2 [verify] `cargo test --workspace --all-features` — зелёный. Особенно snapshot'ы в [crates/dq-cli/tests/](../../../crates/dq-cli/tests/) для `dq diff` / `dq patch` / `dq merge` — должны пройти байт-в-байт.
- [x] 6.3 [verify] `cargo clippy --workspace --all-targets --all-features -- -D warnings` — особое внимание к `clippy::let_and_return`, `clippy::needless_borrow` после рефакторинга на `&mut`.
- [x] 6.4 [verify] `cargo fmt --all -- --check`.
- [x] 6.5 [verify] `cargo bench -p dq-core --bench pointer -- --quick`:
  - `pointer/with_segment/64` — те же ~115 µs (не оптимизировался, и это ОК).
  - `pointer/recursive_walk/64` — должен показать линейный скейлинг от depth=1 (~50 ns) до depth=64 (~4 µs), а не O(n²) curve.

## 7. Documentation

- [x] 7.1 [author] PR-описание содержит:
  - Before/after для `pointer/recursive_walk/{1,4,16,64}`.
  - Описание трейд-оффа: `with_segment` НЕ оптимизирован, потому что (а) это микро-API, (б) единственный pathological hot-path использовал его в рекурсии — теперь использует push/pop, (в) `with_segment` оставлен для backwards-compat и one-shot use (plugin runtime).
- [x] 7.2 [author] Один-абзац комментарий в `Pointer::push_segment` doc'у указывает, что для recursive tree walks это предпочтительный путь, и линкует на `with_segment` для one-shot случаев.

## 8. Archive

- [ ] 8.1 После merge'a — `openspec/changes/perf-pointer-recursive-walks/` → `openspec/changes/archive/2026-MM-DD-perf-pointer-recursive-walks/`.
