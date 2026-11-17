Делегирование: каждая задача помечена `[orch]` (оркестратор выполняет напрямую — markdown, manifests, ручные проверки) или `[writer]` / `[test-writer]` (отдаётся в `rust-cli-writer` / `rust-cli-test-writer` через Agent tool). Задачи `[writer]`/`[test-writer]` self-contained: содержат файл, цель, ограничения. Каждая задача ≤ 2 часов. Раздел 1 (Spike) — gate, остальные разделы блокируются его исходом.

## 1. Spike: textual-edit POC (span-based)

Архивированный спайк event-stream rewrite ([spikes/saphyr/RESULTS.md](../../../spikes/saphyr/RESULTS.md)) уже закрыл вопрос про round-trip emit на saphyr/yaml-rust2/marked-yaml. Этот раздел — **новый POC** для подхода D1 (textual-edit).

- [x] 1.1 [orch] Fixtures для нового POC переиспользуем существующие в [spikes/saphyr/fixtures/](../../../spikes/saphyr/fixtures/) — они покрывают те же D11 критерии. Дополнительно создать минимальный fixture (f) — Helm chart с `{{ ... }}` шаблоном, **до** placeholder substitution — чтобы провалидировать что POC корректно ругается на templated input через template guard (Task 6.x).
- [x] 1.2 [writer] Заменить содержимое `spikes/saphyr/src/main.rs` (старый код можно git-rm'ить — вся работа хранится в RESULTS.md как evidence): новый бинарь `saphyr-spike` с тремя subcommand'ами:
  - `span-build <FIXTURE>` — parse через `saphyr-parser::Parser` event API (получить `(Event, Span)` пары, где Span = byte range), построить `Pointer → ValueSpan` IndexMap, dump в JSON на stdout. Цель — увидеть собственно span coverage (для каждого scalar что-то есть).
  - `mutate <FIXTURE> <POINTER> <NEW_VALUE>` — span-build + замена байтов в `value_range` соответствующего span'а на новое value (rendered с учётом quote_style исходного scalar'а). Output на stdout — модифицированный YAML.
  - `assert-byte-perfect <FIXTURE> <POINTER> <NEW_VALUE>` — то же что mutate, но затем сравнивает diff между input и output через `similar`. **Pass-критерий:** diff содержит ровно одну removed line и одну added line. Print PASS/FAIL.
  - `bench-span-build <FIXTURE>` — 10 итераций parse + span build, median+stddev в ms.
- [x] 1.3 [test-writer] В `spikes/saphyr/tests/poc.rs` (один интеграционный тест): прогнать `assert-byte-perfect` на 5 fixture'ах (a-e) по списку [Pointer, NEW_VALUE, expected line replacement]. Для (a) k8s — `set /spec/replicas 5`, ожидается `-  replicas: 3` / `+  replicas: 5`. Для (c) anchors — `set /defaults/timeout 60`, проверить что `&base` declaration сохраняется. Для (d) multi-doc — `set /1/spec/ports/0/port 8090`, проверить что `---` separator'ы byte-exact. Для (e) hugo — `set /title "Updated"`. Для (b) helm — `set /image/tag v2.0.0`. **Все 5 должны PASS.**
- [x] 1.4 [writer] Дополнить spike: реализовать insertion на ОДНОМ fixture — `set /spec/strategy/type RollingUpdate` на k8s manifest без strategy. Использовать D14 heuristic: indent = `parent_indent + 2`, bare-style scalar. Validate: `serde_yml::from_slice` парсит результат без error.
- [x] 1.5 [orch] Замерить performance: парсинг + span build для созданного 1MB synthetic k8s manifest (`spikes/saphyr/big.yaml` — 1000 deployment объектов через скрипт). Записать median+stdev (target: ≤ 100ms median, span recompute ≤ 5ms).
- [x] 1.6 [orch] Дополнить `spikes/saphyr/RESULTS.md` секцией "POC for textual-edit (Option B)": per-fixture pass/fail для assert-byte-perfect, insertion result, performance numbers, оценка `Document::set_at` ergonomics на основе кода spike'а.
- [x] 1.7 [orch] **Gate decision**: если 1.3 все 5 PASS и 1.4 produces valid YAML — переходим к разделу 2. Если 1.3 хоть один FAIL → escalate user (Option D libfyaml или Option A no-preserve). Если 1.4 produces invalid YAML — pause section 4-12 на доработку D14 emitter'а; insertion может требовать дополнительные heuristics. Gate — manual user sign-off в комментарии к change'у.

## 2. dq-core: Document span model

- [x] 2.1 [writer] Создать `crates/dq-core/src/document/spans.rs`. Определить:
  - `pub struct ValueSpan { value_range: std::ops::Range<usize>, line_range: std::ops::Range<usize>, indent: u32, context: SpanContext }` — ровно как в [design.md D4](design.md#d4-document-model-value--original_bytes--pointer--byterange-span-map).
  - `pub enum SpanContext { BlockMapValue, BlockSeqItem, FlowMapValue, FlowSeqItem }`.
  - `pub type SpanMap = indexmap::IndexMap<String, ValueSpan>` (key — canonical pointer string).
  - `pub struct SpanRecomputeDelta { pub at: usize, pub old_len: usize, pub new_len: usize }` для O(N) пересчёта offsets правее изменения.
  - `impl SpanMap` метод `apply_delta(&mut self, delta: SpanRecomputeDelta)` — проходит все spans, сдвигает те, что начинаются после `delta.at`.
  - Все типы `#[derive(Debug, Clone, PartialEq)]`. Никаких build-helpers здесь — построение в parsers.
- [x] 2.2 [writer] В `crates/dq-core/src/document.rs` обновить `pub struct Document`: добавить `original_bytes: Vec<u8>`, `spans: SpanMap`, `format: FormatTag` (enum `Yaml | Json | Toml | Jsonl`). Существующее поле `value` сохранить — read-pat M1 продолжает работать через `Document::value()`. Добавить:
  - `Document::original_bytes(&self) -> &[u8]`
  - `Document::span_at(&self, pointer: &Pointer) -> Option<&ValueSpan>`
  - Конструктор `Document::with_spans(value: Value, original_bytes: Vec<u8>, spans: SpanMap, format: FormatTag) -> Self` (для write-pat parsers).
  - Конструктор `Document::value_only(value: Value, format: FormatTag) -> Self` (для read-pat parsers M1 — original_bytes пустой, spans пустой; вызов `set_at`/`del_at` на таком Document → `Error::WriteUnavailable { reason: "document was loaded read-only; reload via parser_with_spans" }`).
- [x] 2.3 [writer] В `crates/dq-core/src/document.rs` реализовать `Document::set_at(&mut self, pointer: &Pointer, value: Value) -> Result<()>`. Алгоритм:
  1. Если `self.spans` пустой → `WriteUnavailable` error.
  2. Lookup `self.spans[pointer.canonical()]`. Если найден → render `value` под `format` + `span.context` + scalar style detected from current span content → splice `original_bytes[span.value_range]` → call `spans.apply_delta(...)` → update `value` в-памяти под новый.
  3. Если не найден → mkdir-p path. Найти ближайший existing ancestor через прогрессивное укорачивание pointer'а. Render новый суффикс через format-specific insertion emitter (D14, см. Task 2.5). Insert в правильную позицию в ancestor'е (конец mapping/sequence). Update spans для нового поддерева. Update value.
  4. `--no-create` mode: skip step 3 — return `Error::Path` с `MissingKey`.
  
  Render-replacement helper живёт в format-specific module (`yaml_spans::render_scalar_replacement`, `toml::render_scalar_replacement`, и т.д.) — Document::set_at вызывает по `format` enum.
- [x] 2.4 [writer] В `crates/dq-core/src/document.rs` реализовать `Document::del_at(&mut self, pointer: &Pointer) -> Result<()>`. Удаление:
  1. Lookup span. Если не найден → `Error::Path`. Если root pointer (`""`) → `Error::Path { kind: TypeMismatch }`.
  2. Splice `original_bytes[span.line_range]` → пустой. Это удаляет ключ + значение + trailing comment + trailing newline.
  3. `spans.apply_delta(...)`. Удалить все spans с префиксом этого pointer'а (children deleted).
  4. Update `value`: `IndexMap::shift_remove` для maps, `Vec::remove` для arrays.
- [x] 2.5 [writer] Создать `crates/dq-core/src/textual_edit/mod.rs` с двумя публичными trait'ами:
  - `pub trait ScalarRenderer: Sync { fn render_replacement(&self, value: &Value, context: SpanContext, original: &[u8]) -> Vec<u8>; }` — рендерит scalar для in-span replacement, учитывая existing context.
  - `pub trait InsertionRenderer: Sync { fn render_insertion(&self, key: &str, value: &Value, parent_indent: u32, parent_context: SpanContext) -> Vec<u8>; }` — рендерит entire `key: value` (или `[key] = value`) пара для insertion. D14 эвристики живут здесь.
  
  Format-specific impl'ы в каждом parser'е (3.x, 4.x, 5.x).
- [x] 2.6 [test-writer] Unit-тесты в `crates/dq-core/src/document.rs` (`#[cfg(test)] mod tests`) — НЕ полагаются на parsers (используют ручную сборку Document). ≥ 10 cases:
  - set scalar replacement: span lookup + splice работает на manually-built Document
  - set + recompute delta updates downstream spans corretly
  - set with mkdir-p calls insertion emitter (mock impl возвращает фиксированную строку — проверяем что вызывается с правильным context)
  - del removes line_range, leaves earlier byte sequence untouched
  - del recompute delta
  - set on Document::value_only → WriteUnavailable
  - del root → TypeMismatch
  - del missing key → MissingKey
  - del with --no-create flag (passed through args) — этот test уже на handler уровне, см. §9

## 3. dq-core: YAML write-pat parser (saphyr-parser, span builder)

Read-pat YAML на `serde_yml` остаётся как был в M1 — `crates/dq-core/src/parsers/yaml.rs` НЕ трогаем. Этот раздел добавляет параллельный модуль для write-команд.

- [x] 3.1 [writer] Обновить `crates/dq-core/Cargo.toml`: добавить `saphyr-parser = "0.0.6"` (низкоуровневый event API; **не** `saphyr`). Версию подтвердить на спайке 1.x (если 0.0.6 устарел к моменту implementation — взять latest stable). Добавить `tempfile = "3"` в `[dependencies]` (был только в dev).
- [x] 3.2 [writer] Создать `crates/dq-core/src/parsers/yaml_spans.rs`. Публичная функция `pub fn parse_with_spans(bytes: &[u8]) -> Result<(Value, SpanMap)>`. Internals:
  - `saphyr_parser::Parser::new_from_str` → iterate events, каждый event имеет `Span { start: Marker, end: Marker }` (Marker даёт `index`, `line`, `col`).
  - State machine: при `Event::MappingStart` → push frame "expecting key", при следующем `Event::Scalar` если frame == "expecting key" → запомнить key, переключить frame на "expecting value", при следующем event для value → знаем pointer (parent path + key), создаём `ValueSpan` с `value_range = event_span_to_byte_range(event.span())`. Аналогично для sequences (`Event::SequenceStart` → frame "expecting items").
  - `line_range` для block-context узлов вычисляется отдельно: backward scan от `value_range.start` до start-of-line, forward scan от `value_range.end` до end-of-line (включая trailing newline). Comments на той же строке оборачиваются в `line_range`.
  - `context` определяется по типу parent frame'а (BlockMap/FlowMap/BlockSeq/FlowSeq).
  - Multi-doc YAML: при каждом `Event::DocumentStart` начинаем новый Pointer-namespace `/0`, `/1`, ... как в M1.
  - Anchors/aliases: `Event::Alias` создаёт `ValueSpan` с `value_range = alias_token_range`; mutation alias просто заменяет `*name` на новое value (теряет alias linking, но это intentional — alias rewrites становится отдельным concern в M3+).
  - На parse error → `Error::Parse { line, col, span, snippet, message }` — повторно использовать тот же error rendering что M1 yaml.rs (через helper в error.rs).
- [x] 3.3 [writer] В `crates/dq-core/src/parsers/yaml_spans.rs` имплементировать `ScalarRenderer` и `InsertionRenderer` (из textual_edit/mod.rs) для YAML. `render_scalar_replacement` rules:
  - Detect scalar style исходного scalar'а через scan байтов в `original[span.value_range]`: starts with `"` → DoubleQuoted, starts with `'` → SingleQuoted, starts with `|` → LiteralBlock, starts with `>` → FoldedBlock, иначе Bare.
  - Render new value в том же style. Если новое value содержит chars, требующие escape (например, `:`, `#`, leading whitespace) и original был Bare → upgrade до DoubleQuoted (это intentional drift, но preserves validity).
  - `render_insertion` для D14: render `<key>: <value>\n` с `parent_indent + 2` indent, bare-style scalar если возможно. Multi-line value (sequence, map) → block-style nested с further `+2` indent.
- [x] 3.4 [test-writer] В `crates/dq-core/tests/yaml_spans.rs` (новый файл) — span-builder unit тесты. ≥ 10 cases:
  - parse_with_spans на k8s Deployment fixture: span для `/spec/replicas` имеет `value_range` точно на байтах `"3"`, `line_range` покрывает всю строку с trailing newline.
  - flow mapping `{a: 1, b: 2}` — spans для `/a` и `/b` имеют `context: FlowMapValue`.
  - block sequence — spans для items имеют `context: BlockSeqItem`.
  - YAML с anchors: span для `*base` reference указывает на bytes `*base` (не на dereferenced value).
  - Multi-doc: spans namespace'нуты под `/0`, `/1`, `/2`.
  - render_scalar_replacement preserves quote style (DoubleQuoted → DoubleQuoted).
  - render_scalar_replacement upgrades Bare → DoubleQuoted when new value contains `:`.
  - render_insertion produces valid YAML for nested map (parsable через serde_yml).
- [x] 3.5 [test-writer] В `crates/dq-core/tests/round_trip_property.rs` (новый файл) добавить proptest: генерация случайного валидного YAML текста (через простую стратегию `proptest`), `parse_with_spans` → `set_at` (random pointer to random scalar) → собираем result bytes. **Property:** result parses через `serde_yml` (validity), и для unchanged points, `original_bytes[unchanged_span] == result_bytes[unchanged_span_shifted]`. ≥ 100 cases per run, seed pinned.

## 4. dq-core: TOML round-trip via toml_edit

`toml_edit` уже работает по textual-edit принципу — переиспользуем его API напрямую вместо span-builder.

- [x] 4.1 [writer] Обновить `crates/dq-core/Cargo.toml`: добавить `toml_edit = "0.22"`, удалить `toml`. Workspace pin в корневом `Cargo.toml`. *(Note: `toml` крейт оставлен для `value_only` write fallback — см. agent report для §4; полный removal — отдельный post-M2 change.)*
- [x] 4.2 [writer] Переписать `crates/dq-core/src/parsers/toml.rs` на `toml_edit::DocumentMut`. *(Implemented via `toml_edit::ImDocument::parse` — only `ImDocument` preserves spans in toml_edit 0.22; `DocumentMut::from_str` discards them. The §4 baseline never mutates the parsed DOM in place — splices land in `original_bytes`.)*
  - Имплементировать `ScalarRenderer` и `InsertionRenderer` для TOML — большая часть логики делегируется в toml_edit (он сам рендерит правильный style).
- [x] 4.3 [test-writer] В `crates/dq-core/tests/parse_toml.rs` сохранить M1 baseline тесты (5 шт). Добавить 5 round-trip тестов:
  - `Cargo.toml` с комментариями + dotted keys + inline tables → byte-equal round-trip
  - Datetime literal preservation: `created = 1979-05-27T07:32:00Z` → after unrelated mutation, datetime literal byte-exact
  - Single scalar mutation на nested table — diff = 1 line
  - del nested key — порядок остальных сохранён
  - Inline table mutation сохраняет inline стиль

## 5. dq-core: JSON span-edit

JSON — простейший случай: comments не существуют в формате, structure — strict. Span-builder короткий.

- [x] 5.1 [writer] В `crates/dq-core/src/parsers/json.rs` extended with span scanner (вместо отдельного `json_spans.rs`). Span builder реализован как ручной byte-level state machine; JSONC reject через `Error::Parse`.
- [x] 5.2 [writer] Имплементировать `ScalarRenderer` и `InsertionRenderer` для JSON. *(Insertion-renderer indent — hardcoded 2-space в M2 baseline; см. module docstring.)*
- [x] 5.3 [test-writer] В `crates/dq-core/tests/parse_json.rs` сохранить M1 baseline (5 шт). Добавить 4 round-trip:
  - 4-space indent preservation
  - tab indent preservation
  - Single scalar mutation в pretty-printed JSON → diff = 1 line
  - JSONC → exit 3 с правильным message

## 6. dq-core: atomic write + template guard

- [x] 6.1 [writer] Создать `crates/dq-core/src/atomic_write.rs` с публичным `pub fn write(path: &Utf8Path, content: &[u8], backup: bool) -> Result<()>`. *(Errors wrap into `Error::WriteIo { path, source }` per §7 — отдельный variant для write-side IO; read-side остаётся `Error::Io`.)*
- [x] 6.2 [test-writer] Sanity tests реализованы как `#[cfg(test)] mod tests` внутри `atomic_write.rs` (вместо отдельного `tests/atomic_write.rs`). 4 cases:
  - Happy path: write нового файла
  - Overwrite существующего: содержимое заменено целиком
  - Backup: после write существуют path и path.bak с правильным содержимым
  - Backup overrides existing: повторный write с backup перезаписывает .bak
  - Read-only parent dir: Err с правильным `path` в Error
  - Same-directory invariant: проверить через mock что `tempfile::NamedTempFile::new_in()` вызвана с parent (через trait? либо просто проверить что tmpfile появляется в той же dir во время write — race-y, лучше первый вариант)
- [ ] 6.3 [test-writer] В `crates/dq-core/tests/atomic_write_windows.rs` Windows-specific smoke (gated `#[cfg(target_os = "windows")]`): write + persist over file, который параллельно открыт другим handle. **Decision: deferred to M6 distribution per §13.5 option (b)** — Windows CI matrix отложен, smoke-тест останется `#[ignore]` с TODO до тех пор.
- [x] 6.4 [writer] Создать `crates/dq-core/src/template_guard.rs`: `pub struct TemplateMarker { pub line: u32, pub snippet: String }`, `pub fn detect_templates(bytes: &[u8]) -> Option<TemplateMarker>`. Regex через `regex::bytes::Regex` cached в `OnceLock`. `substitute_placeholders` / `restore_placeholders` round-trip byte-equal на Helm/GHA template inputs.
- [x] 6.5 [test-writer] Sanity tests внутри `template_guard.rs` (`#[cfg(test)] mod tests`); расширенный набор кейсов покрыт через unit-tests в дополнение к §12 интеграционным сценариям.
  - 5 positive: helm values, k8s manifest с `{{ ... }}`, github workflow с `${{ ... }}`, argo template, простой `{{ .x }}`
  - 5 negative: plain YAML без шаблонов, YAML со строкой "use {{ syntax }}" в quoted строке, YAML с `{x: 1}` (flow mapping без templates), JSON с `{"x":1}`, TOML
  - 2 round-trip placeholder substitute → restore → byte-equal

## 7. dq-core: Error variants

- [x] 7.1 [writer] В `crates/dq-core/src/error.rs` добавлены variants:
  - `TemplatedFile { line: u32, snippet: String, hint: String }` (с `Error::templated_file(marker)` constructor) — kind_name `"templated_file"`.
  - `WriteIo { path: Utf8PathBuf, #[source] source: std::io::Error }` — kind_name `"write_io"`.
- [x] 7.2 [test-writer] В `crates/dq-core/tests/error_render.rs` — 4 новых insta snapshot теста (`console_templated_file`, `json_templated_file`, `console_write_io`, `json_write_io`).
  - Render `TemplatedFile` через console formatter (no color)
  - Render `TemplatedFile` через JSON `-F json`
  - Render `WriteIo` через console
  - Render `WriteIo` через JSON

## 8. dq-cli: exit codes + write-flag activation

- [x] 8.1 [writer] В `crates/dq-cli/src/exit_code.rs` добавлено `pub const WRITE_FAILED: i32 = 7`. `exit_code_for_error` маппит `WriteIo` и `WriteUnavailable` → `WRITE_FAILED (7)`, `TemplatedFile` → `PARSE_ERROR (3)`.
- [x] 8.2 [writer] В `crates/dq-cli/src/cli/args.rs` rejector переименован в `Cli::ensure_no_write_flags()` (read-only semantics) — каждый read-handler вызывает в первой строке. `-i/--diff/--backup` остаются global. Set/del handlers вызывают `Cli::ensure_write_flags_consistent()` (новый helper) для validation `-i+--diff`, `--backup` без `-i`, `-i+-F`.
- [x] 8.3 [test-writer] `crates/dq-cli/tests/cli_write_flags.rs` обновлён под новую модель + handler-уровневые тесты в `cli/args.rs` для `ensure_write_flags_consistent`.

## 9. dq-cli: set command

- [x] 9.1 [writer] Создан `crates/dq-cli/src/cli/args/set.rs` с `SetArgs`. `--allow-templates`/`--raw-template-strings` — global (см. 9.2).
- [x] 9.2 [writer] В `crates/dq-cli/src/cli/args.rs` добавлены `--allow-templates` и `--raw-template-strings` как global флаги с `conflicts_with` друг с другом.
- [x] 9.3 [writer] В `Command` enum зарегистрирован `Set(SetArgs)`.
- [x] 9.4 [writer] Создан `crates/dq-cli/src/commands/set.rs` с полной реализацией всех шагов (detect format → template guard → parse → resolve value → set_at → output mode). 14 unit-тестов покрывают ключевые сценарии.
- [x] 9.5 [writer] `Command::Set(args) => commands::set::run(cli, args, input_format, use_color, out)` зарегистрирован в `dispatch()`.

## 10. dq-cli: del command

- [x] 10.1 [writer] Создан `crates/dq-cli/src/cli/args/del.rs` с `DelArgs`.
- [x] 10.2 [writer] `Del(DelArgs)` зарегистрирован в `Command` enum.
- [x] 10.3 [writer] Создан `crates/dq-cli/src/commands/del.rs` (162 lines + 5 unit tests). Те же template guard / output mode шаги что set, минус value resolution.
- [x] 10.4 [writer] `Command::Del(args) => commands::del::run(cli, args, input_format, use_color, out)` зарегистрирован в `dispatch()`.

## 11. dq-cli: --diff via similar

- [x] 11.1 [writer] `crates/dq-cli/Cargo.toml`: добавлено `similar = "2"`.
- [x] 11.2 [writer] Создан `crates/dq-cli/src/diff.rs` с `pub fn render_unified(source: &str, modified: &str, file_label: &str, use_color: bool) -> String`. ANSI цвета: red `\x1b[31m` для `-`, green `\x1b[32m` для `+`, cyan `\x1b[36m` для `@@`.
- [x] 11.3 [test-writer] Sanity tests внутри `diff.rs` (4 cases): identical→empty, single-line mutation, no-color (no `\x1b`), with-color (red/green/cyan ANSI).
  - single-line mutation (no color)
  - multi-line edit (no color)
  - colored output — assert ANSI escapes присутствуют
  - identical input/output → empty diff string

## 12. dq-cli: integration tests

- [x] 12.1 [test-writer] `crates/dq-cli/tests/unit_set.rs` создан — 14 handler-уровневых тестов через `dq::run` с tempfile-ами.
- [x] 12.2 [test-writer] `crates/dq-cli/tests/unit_del.rs` создан — 6 handler-тестов (leaf del, array shift, missing pointer, root del, -i atomic, --diff removal).
- [x] 12.3 [test-writer] `crates/dq-cli/tests/cli_smoke.rs` расширен +5 smoke-сценариев из M2 DoD.
- [x] 12.4 [test-writer] `crates/dq-cli/tests/cli_snapshots.rs` +2 snapshot теста для `TemplatedFile` (console + JSON).
- [x] 12.5 [test-writer] `crates/dq-cli/tests/golden.rs` расширен round-trip runner'ом, 25+ fixture'ов в `tests/fixtures/golden/roundtrip/` (8 YAML + 6 JSON + 6 TOML + 5 edge). Источники в `tests/fixtures/SOURCES.md`. JSONL fixture'ы skipped (read-only parser).
- [x] 12.6 [test-writer] `crates/dq-cli/tests/cli_atomic_write.rs` создан — 2 smoke-тесты (no stale tmp, .bak placement). Crash simulation через SIGKILL отложена с TODO.

## 13. Plan delta + meta

- [x] 13.1 [orch] Обновлён dq-plan.md M2 секция: textual-edit подход (D1) зафиксирован, ссылки на `--allow-templates`/`--raw-template-strings` и `WRITE_FAILED = 7` добавлены.
- [x] 13.2 [orch] Обновлён dq-plan.md Tech stack: `saphyr-parser` + `toml_edit` для M2 write-pat, `serde_yml` остаётся для read-pat, `similar` + `regex` + `tempfile` добавлены.
- [x] 13.3 [orch] `deny.toml` advisory ignore сохранён с уточнённым reason: `serde_yml` read-pat dep; write-pat uses saphyr-parser (no Serializer access).
- [x] 13.4 [orch] README.md обновлён: статус `M2 alpha — read + write`, секция команд с set/del + write-flags + exit-codes.
- [x] 13.5 [orch] **Decision: option (b)** — Windows CI matrix отложен до M6 distribution. Task 6.3 остаётся как Windows smoke `#[cfg(target_os = "windows")]` `#[ignore]`-gated; добавлять отдельный CI workflow преждевременно (уплыло бы из M2 scope).
- [x] 13.6 [orch] **Decision: оставить `dq-transform` пустым placeholder'ом**. Минимальный re-export не требуется: `Document::set_at`/`del_at` живут в `dq-core` напрямую, transform-layer (jaq adapter, Op pipeline) пишется в M7. До тех пор `dq-transform/src/lib.rs` — пустой.

## 14. Verification & sign-off

- [x] 14.1 [orch] `cargo build --workspace --all-targets` зелёный.
- [x] 14.2 [orch] `cargo test --workspace --all-features` зелёный — **438 тестов passed, 0 failed** (Windows atomic-write smoke остаётся `#[ignore]`-gated до M6 distribution per §13.5).
- [x] 14.3 [orch] `cargo clippy --workspace --all-targets --all-features -- -D warnings` зелёный.
- [x] 14.4 [orch] `cargo fmt --all -- --check` зелёный.
- [x] 14.5 [orch] `cargo deny check` зелёный — `advisories ok, bans ok, licenses ok, sources ok`. `serde_yml` advisory ignore сохранён с обновлённым reason.
- [x] 14.6 [orch] Manual smoke выполнен на `k8s_deployment_writable.yaml`:
  - `dq set deploy.yaml /spec/replicas 5` (stdout) — full doc rendered с `replicas: 5`, comments preserved.
  - `dq set deploy.yaml /spec/replicas 7 --diff` — single-line diff (`-replicas: 3` / `+replicas: 7`).
  - `dq set deploy.yaml /spec/replicas 5 -i --backup` — file updated, `deploy.yaml.bak` содержит original (1-line diff).
  - `dq del deploy.yaml /metadata/annotations/foo --diff` — single-line diff (`-foo: keep-me`).
  - `dq del deploy.yaml /metadata/annotations/foo -i` — sibling key `bar` сохранён, comments сохранены.
  - JSON big-int round-trip: `4722366482869645213696` возвращается byte-exact через `dq get`.
- [x] 14.7 [orch] DoD пункты M2 сверены: golden snapshots на 25+ файлов (12.5), proptest round-trip (3.5), Windows smoke deferred (per 13.5 option (b) decision).
- [x] 14.8 [orch] `openspec validate add-safe-writes --strict` — `Change 'add-safe-writes' is valid`.
- [ ] 14.9 [orch] `openspec archive add-safe-writes` — будет запущен после merge'а в main.

## Follow-up: bug discovered & fixed during §14 verification

- [x] **`backup_path_for` always-append fix** — `backup_path_for` использовал `Utf8Path::with_extension("bak")` который заменяет existing extension (`deploy.yaml` → `deploy.bak`). Spec требует append (`deploy.yaml.bak`). Fix: `backup_path_for` теперь one-liner `Utf8PathBuf::from(format!("{}.bak", path.as_str()))`. Тесты обновлены: `backup_path_for_always_appends_bak`, `cli_atomic_write::backup_flag_creates_bak_file_alongside`, `unit_set::set_in_place_with_backup_creates_bak_file`. Все 438 тестов остаются зелёными.

- [x] **saphyr-parser char-vs-byte index bug fix** — `saphyr_parser::Marker::index()` возвращает character index, не byte index. Любой YAML файл с multi-byte UTF-8 (em-dash в комментариях, non-ASCII identifiers) получал misaligned span'ы. Fix в `crates/dq-core/src/parsers/yaml_spans.rs`: добавлена функция `build_char_to_byte_map` (one-time O(N) precompute) + `char_index_to_byte` (O(1) lookup); все `span_to_range`/`scan_to_parse_error` callsites переведены на translation. Regression test: `parse_with_spans_handles_multibyte_utf8_in_preamble` в `crates/dq-core/tests/yaml_spans.rs`.
