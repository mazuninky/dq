# Tasks

## 1. Diagnose (before code)

- [ ] 1.1 [author] Перезапустить `cargo bench -p dq-core --bench parse -- --quick` на чистом branch'е и зафиксировать before-числа в [design.md](design.md) §1 (если успели разойтись с теми, что в proposal.md).
- [ ] 1.2 [author] Подтвердить, что `parse/yaml/10000` и `parse/toml/10000` на той же машине дают ~50 ms и ~65 ms — это taget-уровень для JSON после фикса.

## 2. Implementation

- [ ] 2.1 [delegate to rust-cli-writer] Создать `LineIndex` структуру **внутри** [`crates/dq-core/src/parsers/json.rs`](../../../crates/dq-core/src/parsers/json.rs) (private module-level type, не экспортировать):
  - Поля: `newline_offsets: Vec<usize>`, `indents: Vec<std::cell::OnceCell<u32>>`.
  - Конструктор `LineIndex::new(bytes: &[u8])` — один проход по байтам, заполняет `newline_offsets`; инициализирует `indents` как `vec![OnceCell::new(); newline_offsets.len() + 1]`.
  - Методы: `line_start(&self, offset) -> usize`, `line_end(&self, offset, total: usize) -> usize`, `indent_for(&self, bytes, offset) -> u32`. Логика — см. [design.md](design.md) §2.2.
  - `partition_point` (stdlib slice method) для O(log L) lookup'а.
  - **НЕ использовать** crate `memchr` напрямую как dependency — это уже подгружается через transitive, но для явного использования потребует bump в `Cargo.toml`. Использовать `bytes.iter().enumerate().filter_map(|(i, &b)| (b == b'\n').then_some(i)).collect()`. На фоне `serde_json::from_slice` это пренебрежимо.
- [ ] 2.2 [delegate to rust-cli-writer] Передать `LineIndex` в `Scanner`:
  - Поле `Scanner.lines: LineIndex` (построить в `build_span_map` перед `scanner.scan_value(...)`, передать в конструктор `Scanner`).
  - Изменить сигнатуру `Scanner::new` или `build_span_map` — взять `LineIndex` как параметр.
- [ ] 2.3 [delegate to rust-cli-writer] Заменить вызовы `compute_line_range(self.bytes, &value_range)` и `compute_indent(self.bytes, start)` в `record_scalar` (стр. 575-576) и `record_empty_container` (стр. 607-608) на вызовы `self.lines.line_start(...)..self.lines.line_end(...)` и `self.lines.indent_for(...)`.
- [ ] 2.4 [delegate to rust-cli-writer] Решить судьбу старых helper'ов `compute_line_range` (стр. 623) и `compute_indent` (стр. 639):
  - Если ни один другой call-site не остался — удалить (предварительно проверить `rg compute_line_range crates/`).
  - Если что-то использует — оставить как есть с пометкой `// kept for non-hot callers`. **Не** заменять остальные call-site'ы наугад — outside scope.

## 3. Regression tests

- [ ] 3.1 [delegate to rust-cli-test-writer] Создать [`crates/dq-core/tests/parse_json_perf_smoke.rs`](../../../crates/dq-core/tests/parse_json_perf_smoke.rs):
  - Тест `parses_10k_element_flat_array_under_1s` — строит `serde_json::to_vec(&(0..10_000).collect::<Vec<_>>())`, парсит через `dq_core::format::by_name("json").unwrap().parse(...)`, assert'ит wall-time < 1.0 s (3× headroom над целевыми ~300 ms; учитывает медленные CI runner'ы).
  - Тест `parses_10k_element_pretty_array_under_1s` — то же, но через `serde_json::to_vec_pretty` (multi-line JSON). Должен пройти что до, что после фикса — отлавливает регрессии в "easy" варианте.
  - **Не** использовать `std::time::Instant` напрямую в assert'е, обернуть в helper `assert_under(duration, || { ... })` с понятным сообщением: «JSON parse of 10k elements took {N}s, expected < 1s — likely O(n²) regression». Цель: явно ловит O(n²) regression'ы в будущем.
- [ ] 3.2 [delegate to rust-cli-test-writer] Проверить, что существующие property-tests в [`crates/dq-core/tests/`](../../../crates/dq-core/tests/) (parse-roundtrip, span-fidelity) продолжают пройти без изменений. Если нашёлся test, который assert'ит конкретную форму `ValueSpan.line_range` или `.indent` — он должен пройти на тех же фикстурах байт-в-байт. Если падает — означает, что новая логика расходится с старой; найти и пофиксить.

## 4. Verification

- [ ] 4.1 [verify] `cargo test -p dq-core` — зелёный.
- [ ] 4.2 [verify] `cargo test --workspace --all-features` — зелёный (snapshot'ы dq-cli ходят через SpanMap для `dq set -i`, `dq del -i`, `dq fmt -i`; если что-то поплыло — поломаются).
- [ ] 4.3 [verify] `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] 4.4 [verify] `cargo fmt --all -- --check`.
- [ ] 4.5 [verify] `cargo bench -p dq-core --bench parse -- --quick` — `parse/json/10000` ≤ 300 ms. Зафиксировать after-числа в PR-описании рядом с before.
- [ ] 4.6 [verify] Manual smoke: `cargo build --release && time ./target/release/dq query '. | length' <(seq 1 10000 | jq -s .)` — должно отработать за < 200 ms.

## 5. Documentation

- [ ] 5.1 [author] PR-описание содержит before/after табличку по `parse/json/{100,1000,10000}` (criterion вывод).
- [ ] 5.2 [author] Если фикс уменьшает p99-latency `dq lint` на больших workload'ах — упомянуть в release notes для следующей версии. README менять не нужно (perf не часть public surface'а).

## 6. Archive

- [ ] 6.1 После merge'a — `openspec/changes/perf-json-parse-linear/` → `openspec/changes/archive/2026-MM-DD-perf-json-parse-linear/`.
