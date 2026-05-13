## Why

Только что добавленный criterion-харнесс ([crates/dq-core/benches/parse.rs](../../../crates/dq-core/benches/parse.rs)) показал, что JSON-парсер катастрофически нелинеен. Замеры (release, M1 Pro, `cargo bench -p dq-core --bench parse -- --quick`):

| записей | время | per-record |
|---|---|---|
| 100 | 1.67 ms | 16.7 µs |
| 1 000 | 143 ms | 143 µs |
| 10 000 | **13.94 s** | 1.4 ms |

10× данных → ~100× время. YAML и TOML на тех же данных линейны (10× → 10×). Это значит, что `dq query`/`dq lint`/`dq fix` на JSON-логах размером в десятки тысяч записей (типичный shape — exported events, NDJSON dump, snyk SBOM, large package-lock) занимает **десятки секунд** на чисто парсинг, тогда как мог бы за сотни миллисекунд.

Узкое место найдено в [`crates/dq-core/src/parsers/json.rs`](../../../crates/dq-core/src/parsers/json.rs) (см. design.md):
- [`compute_indent`](../../../crates/dq-core/src/parsers/json.rs:639) — backward scan от `index` до начала строки. Вызывается из `record_scalar` (стр. 576) для каждого скаляра.
- [`compute_line_range`](../../../crates/dq-core/src/parsers/json.rs:623) — тот же паттерн, вызывается из `record_scalar` (стр. 575).

Для single-line JSON-массива `[v0, v1, …, vn]` элемент `k` живёт на смещении ~`k * record_width`. `compute_indent` для каждого `k` сканирует назад ~`k * record_width` байт. Σ k for k in 1..n = O(n²).

Сам `serde_json::from_slice` (на котором стоит парсер) — линейный; проблема только в SpanMap-builder'е, который запускается **после** распарсенной структуры для аннотирования IR'а byte-диапазонами.

## What Changes

- **Линеаризация SpanMap-builder'а в `parsers/json.rs`** — вместо `compute_indent`/`compute_line_range`, которые сканируют буфер на каждый скаляр, один раз перед скан-loop'ом строится таблица `line_starts: Vec<usize>` (offset каждой `\n`+1). Положение строки для byte-offset — `line_starts.binary_search(&offset)` → O(log L). Indent кешируется per-line в `Vec<u32>` индексированном line-id'ом — индент это whitespace **в начале строки**, идентичен для всех скаляров на одной строке.
- **`pointer_for(path)` остаётся как есть** — для типичного дерева глубиной < 16 это не O(n²), а O(d × n), что приемлемо.
- **Регрессионный бенч-assertion**: `parse/json/10_000` должен пройти за **≤ 300 ms** (порядок YAML/TOML на этих же данных). Криterion'овский ratchet не подключаем (отдельная инфраструктура), но в [crates/dq-core/tests/](../../../crates/dq-core/tests/) добавляется `parse_json_perf_smoke.rs` — параметризованный тест, который парсит `[i for i in 0..10_000]` и assert'ит wall-time < 1.0 s (3× от целевой границы — запас для slow runners в CI).
- **Round-trip и span-семантика не меняются.** Все существующие fixture'ы / property-tests / snapshot'ы продолжают проходить байт-в-байт. Это perf-only фикс; observable API parser'а тот же.

## Impact

- **Affected specs:** ничего не меняется. `data-query-format` спецификация описывает контракт парсера через round-trip и span-fidelity, а не через сложность; обновление документации не требуется.
- **Affected code:**
  - [`crates/dq-core/src/parsers/json.rs`](../../../crates/dq-core/src/parsers/json.rs) — рефактор `Scanner::scan_value` и его span-helpers (~50–100 строк изменений)
  - [`crates/dq-core/tests/parse_json_perf_smoke.rs`](../../../crates/dq-core/tests/parse_json_perf_smoke.rs) — новый regression test
  - [`crates/dq-core/benches/parse.rs`](../../../crates/dq-core/benches/parse.rs) — без изменений; этот же бенч используется для before/after метрик
- **User-visible:**
  - `dq query '.[].id' big-array.json` — секунды → миллисекунды на 10k-element массивах.
  - `dq lint big-package-lock.json` — соответствующее ускорение в lint pipeline'е (90%+ времени уходит на парсинг).
  - Никаких изменений в выводе, exit code'ах, или flag'ах.
- **Downstream consumers:** нулевой риск — никаких API изменений. SpanMap-ключи (RFC 6901 pointer'ы) идентичны до и после.

## Reference

- Бенч-вывод из [crates/dq-core/benches/parse.rs](../../../crates/dq-core/benches/parse.rs) (`cargo bench -p dq-core --bench parse -- --quick`) — фиксирует before-метрики.
- Аналог из serde_json — `serde_json::Deserializer` использует ровно этот же приём (cached `line_starts`) во внутренних error-rendering paths; см. [serde-rs/json#L1183 в `src/read.rs`](https://github.com/serde-rs/json/blob/master/src/read.rs) (если нужно — апстрим-подтверждение, что приём стандартный).
- Конкретные локации проблемы — см. [design.md](design.md) §1.
