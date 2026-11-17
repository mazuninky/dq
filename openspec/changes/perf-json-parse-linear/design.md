# Design — Linearise JSON SpanMap builder

## 1. Root cause

Парсер делится на два этапа:

1. `serde_json::from_slice(bytes)` — строит `serde_json::Value`. Это апстримный код, O(n).
2. `build_span_map(bytes)` — наш собственный `Scanner` снова проходит по `bytes`, эммитя `ValueSpan` для каждого узла в `SpanMap`. Это **наша** часть.

Quadratic поведение живёт в (2). Конкретно — в двух helper'ах, которые вызываются из `record_scalar` (`crates/dq-core/src/parsers/json.rs:565-586`) и `record_empty_container` (`crates/dq-core/src/parsers/json.rs:597-618`):

```rust
// Stripped excerpt from json.rs:639-654
fn compute_indent(bytes: &[u8], index: usize) -> u32 {
    let cap = bytes.len();
    let mut line_start = index.min(cap);
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;                                  // ← backward scan
    }
    let mut indent = 0_u32;
    for &b in &bytes[line_start..cap.min(index)] {
        if b == b' ' || b == b'\t' { indent += 1; }
        else { break; }
    }
    indent
}

// json.rs:623-636
fn compute_line_range(bytes: &[u8], value_range: &Range<usize>) -> Range<usize> {
    let mut start = value_range.start.min(bytes.len());
    while start > 0 && bytes[start - 1] != b'\n' { start -= 1; }   // ← backward scan
    // ...forward scan to next `\n`...
}
```

Оба helper'а делают **линейный backward scan** от позиции скаляра до `\n` (или начала буфера). Для single-line JSON `[v0, v1, …, vN-1]` без переносов строк:

- Скаляр `vk` начинается на offset'е ~`k × record_width`.
- `compute_indent(bytes, k × record_width)` сканирует назад **~k × record_width байт**, пока не упрётся в начало буфера.
- Σ k for k in 0..N = N(N-1)/2 = **O(N²)**.

Для 10 000-element массива это ~50M backward-step'ов, что на современном CPU занимает несколько секунд. Подтверждается замером: 100 → 1 000 → 10 000 даёт 1.67 ms → 143 ms → 13.94 s — ratio'ы ×86 и ×97, классический quadratic curve.

`pretty-print JSON` (с `\n` после каждого элемента) не страдает: каждый scan назад находит `\n` через несколько байт, поэтому `compute_indent` де-факто O(1). Бенч использует `serde_json::to_vec` (compact, без переносов) — поэтому проблема в нём проявляется максимально.

## 2. Fix — precomputed line table

### 2.1 Data structures

Перед `scanner.scan_value(...)` (см. `build_span_map` в `json.rs:230`) построить две таблицы:

```rust
struct LineIndex {
    /// Sorted byte offsets of `\n` characters in the buffer. Implicit:
    /// line 0 starts at offset 0; line `i` (i ≥ 1) starts at
    /// `newline_offsets[i-1] + 1`.
    newline_offsets: Vec<usize>,
    /// Indent (spaces+tabs from line start) per line. Lazily computed
    /// the first time a span on line `i` is recorded, cached thereafter.
    indents: Vec<OnceCell<u32>>,
}
```

Построение `newline_offsets` — один линейный проход:

```rust
let newline_offsets: Vec<usize> = memchr::memchr_iter(b'\n', bytes).collect();
```

`memchr` уже в transitive-зависимостях через `regex` / `serde_yml` / `quick-xml`, добавлять не надо. Если по какой-то причине окажется, что нет — fallback `bytes.iter().enumerate().filter_map(...)` тоже линеен.

`indents` инициализируется как `vec![OnceCell::new(); newline_offsets.len() + 1]`.

### 2.2 Lookup API

```rust
impl LineIndex {
    /// Returns the byte offset of the start of the line containing `offset`.
    /// O(log L) where L = number of lines.
    fn line_start(&self, offset: usize) -> usize {
        let line = self.newline_offsets.partition_point(|&n| n < offset);
        if line == 0 { 0 } else { self.newline_offsets[line - 1] + 1 }
    }

    /// Returns the byte offset of the start of the line *after* the one
    /// containing `offset`, or `bytes.len()` if `offset` is on the last line.
    fn line_end(&self, offset: usize) -> usize { /* symmetric */ }

    /// Returns the indent of the line containing `offset`. O(log L)
    /// first call per line, O(1) thereafter via `OnceCell`.
    fn indent_for(&self, bytes: &[u8], offset: usize) -> u32 {
        let line_idx = self.newline_offsets.partition_point(|&n| n < offset);
        self.indents[line_idx].get_or_init(|| {
            let start = self.line_start(offset);
            let mut indent = 0_u32;
            for &b in &bytes[start..] {
                if b == b' ' || b == b'\t' { indent += 1; }
                else { break; }
            }
            indent
        }).clone()
    }
}
```

Замены в `record_scalar` / `record_empty_container`:

```rust
let line_range = self.lines.line_start(value_range.start)..self.lines.line_end(value_range.end);
let indent = self.lines.indent_for(self.bytes, value_range.start);
```

### 2.3 Complexity

Для бенча `parse/json/N`:
- Построение `LineIndex`: один проход по `bytes` — O(B) где B = размер буфера.
- N вызовов `record_scalar`: каждый делает 2 × `partition_point` (O(log L)) + 1 lazy `OnceCell::get_or_init` (O(line_width) первый раз, O(1) дальше).
- Total: **O(B + N log L)**. Для single-line JSON L = 1, suffix → O(N). Для pretty-print L ~ N, suffix → O(N log N).

В обоих случаях соответствует scaling'у YAML/TOML.

## 3. Что *не* меняем

- **Public API `Json::parse`** — тот же `Result<Document>`. SpanMap-ключи — те же RFC 6901 pointer'ы. Все 175+ snapshot'ов в [crates/dq-core/tests/](../../../crates/dq-core/tests/) и [crates/dq-cli/tests/snapshots/](../../../crates/dq-cli/tests/snapshots/) должны пройти байт-в-байт.
- **`Scanner` rest-of-API** (sibling-функции `scan_object`, `scan_array`, `skip_whitespace`, etc.) — не трогаем. Изменения локализованы в `record_scalar` / `record_empty_container` + добавление `LineIndex` в `Scanner` struct.
- **`pointer_for(path)` (стр. 689)** — выглядит O(d × n), но для разумной глубины d < 16 это ≪ узкого места. Оставляем; см. §5 для альтернативы.

## 4. Альтернативы — отвергнуто

### 4.1 «Использовать serde_json::Deserializer::position()»

Идея: вместо собственного `Scanner`-walk'а, получать byte-position из самого `serde_json` через `DeserializeSeed` API. **Отвергнуто** — `Deserializer::position()` даёт `(line, col, byte)` для парсера, но не для каждого `Value`-узла; для построения SpanMap всё равно нужен собственный токенизатор. М4 (Markdown front-matter) и М11 (XML) используют тот же паттерн «второй проход по байтам после семантического парсера»; ломать единообразие ради одного формата — overhead.

### 4.2 «Перестроить SpanMap lazy, при первом обращении»

Идея: парсер возвращает только `Value`; SpanMap строится по требованию (когда вызывается `Document::set_at` или `del_at`). **Отвергнуто** — `dq lint` ходит в SpanMap для каждой emit'ed diagnostic'и (loc-pointer span resolution), и `dq query`/`get`/`fmt` тоже хотят его для round-trip-write. Lazy перенесёт стоимость с парсинга на первый span-lookup. Net win для linear workload'ов = 0.

### 4.3 «Полностью свой JSON-парсер с встроенными spans»

Идея: написать однопроходный JSON-парсер, который сразу эммитит `Value` + spans. **Отвергнуто** — JSON corner cases (numbers, escapes, surrogates) уже отлажены в `serde_json`; переписывание это месяцы work'a + риск regression'а. Текущий двухпроходный подход правильный, проблема только в helper'ах.

## 5. Future work (вне scope этого change'а)

- `pointer_for(path)` инкрементальный (push/pop на `&mut String`) — мелкий win для глубоких документов. Не блокер.
- Аналогичный аудит [`crates/dq-core/src/parsers/yaml_spans.rs`](../../../crates/dq-core/src/parsers/yaml_spans.rs) — YAML использует `saphyr-parser` event-API, который вряд ли страдает тем же, но бенч `parse/yaml/10_000` показывает ~53 ms против целевых ~30 ms — есть headroom, но не критично.
- Перенести `LineIndex` в `dq-core::source::LineIndex` как public utility, если другим парсерам понадобится (markdown front-matter sometimes wants line numbers). Не сейчас — пока локализуем в `parsers/json.rs`.

## 6. Risk assessment

- **Correctness risk:** низкий. SpanMap-семантика инвариантна — если `line_start(offset)` возвращает то же, что `compute_line_range(...).start`, наблюдаемое поведение идентично. Property-test `parse_json::round_trip_with_spans` (см. [crates/dq-core/tests/parse_json.rs](../../../crates/dq-core/tests/parse_json.rs)) уже покрывает это.
- **Memory overhead:** `LineIndex` держит `Vec<usize>` пропорциональный числу строк + `Vec<OnceCell<u32>>` той же длины. Для 10k-line JSON ≈ 160 KB. Pretty-print 10k-element массив с одним элементом на строку — 10k строк = 240 KB. Приемлемо: парсер уже держит сам `Value` + SpanMap, оба значительно больше.
- **Compile-time risk:** нулевой — изменения изолированы в одном файле.
- **Regression-test coverage:** §3 spec'а tasks'ах добавляет `parse_json_perf_smoke.rs` с wall-time assertion'ом; criterion-бенч ([crates/dq-core/benches/parse.rs](../../../crates/dq-core/benches/parse.rs)) — для after-метрик.
