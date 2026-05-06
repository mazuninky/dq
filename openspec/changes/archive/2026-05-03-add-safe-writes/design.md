## Context

M1 поднял read-only foundation: `Document` = `Value` enum (Null/Bool/Int/BigInt/Float/BigFloat/String/Array/Map<IndexMap>) без metadata, парсеры через `serde_yml`/`serde_json`/`toml`, write только для конверсии формата (с явным WARN про потерю комментариев). M2 — самый рискованный технический блок проекта: добавить `set`/`del` так, чтобы любая правка сохраняла комментарии, blank lines, key order, quote style, anchors, indent и числовую точность. **Без round-trip M2 не выходит** — релиз без сохранения форматирования снимает половину differentiator'а.

**Текущее состояние:** M1 заархивирован, активных changes нет, все DoD M1 зелёные. Write-флаги `-i/--diff/--backup` в clap уже определены, при попытке использования возвращают `InvalidInput` (exit 6) — это intentional placeholder под M2.

**Constraints:**
- Anti-scope: bulk через glob (M3), `patch`/`merge` (M3), `--sort-keys`/`--quote-style` (M4), новые форматы (M5), линтеры (M8+).
- Конвенции `/rust-cli`: тонкий main.rs (≤80 строк), Reporter с DI, exit-codes как named constants, нет `println!`, нет `pub(crate)` escape-hatches в тестах.
- Делегирование: любая правка `*.rs`/`Cargo.toml` идёт через subagent `rust-cli-writer` / `rust-cli-test-writer`.
- Spike-first: первая задача в `tasks.md` — двухнедельный спайк по `saphyr`-event API на репрезентативных файлах **до** имплементации остального ([dq-plan.md:361](../../../dq-plan.md)). Если спайк показывает, что round-trip не достижим в приемлемом качестве — переоценка стратегии (либо собственный YAML парсер на 2-3 месяца, либо релиз без preserve с честным указанием).

**Stakeholders:**
- AI-агенты в CI/CD — главный потребитель `set`/`del` (90% сценариев — машинная правка манифестов).
- DevOps-инженеры — потребитель CLI напрямую (`dq set k8s/deploy.yaml /spec/replicas 3 -i`).
- Будущие milestone'ы: M3 строит `patch`/`merge`/multi-file поверх `set`/`del`, M4 строит `fmt --sort-keys` поверх metadata из M2, M10 (autofix) использует те же atomic-write/template-guard.

## Goals / Non-Goals

**Goals:**
- `set`/`del` работают на YAML/JSON/TOML с round-trip сохранением комментариев, blank lines, key order, quote style, anchors/alias (YAML), indent. Изменённая строка в diff'е соответствует семантике операции (например, `set /spec/replicas 5` меняет ровно одну строку, не сдвигает соседние).
- Atomic write: либо файл целиком обновлён, либо не тронут. Никаких частичных записей даже при `kill -9` или disk-full.
- Big-int precision: `4722366482869645213696` через round-trip `set` → `get` возвращает byte-exact ту же строку.
- Helm/Go-template guard: попытка `set` на templated YAML по умолчанию даёт structured error с message и `did_you_mean: --raw-template-strings`, не молчаливый fallback.
- `dq set helm/values.yaml /image/tag v1.2.3 --diff` — рабочий CI-сценарий "увидеть, что поменяется".
- Все DoD M2 ([dq-plan.md:359](../../../dq-plan.md)) выполнены: golden round-trip на 20-30 файлов из открытых проектов, snapshot diff'ов, Windows atomic-write smoke.

**Non-Goals:**
- Bulk через glob (`dq set 'k8s/**/*.yaml' ...`) — M3.
- `patch` (RFC 6902), `merge` (RFC 7396), structural `diff` между файлами — M3.
- `convert -i` — M3 (см. proposal — write контракт M2 фокусируется на сохранении формата, смена формата в in-place требует отдельного DoD).
- Транс­формации через jq — M7.
- `Op`/`TransformPipeline` API в `dq-transform` — M3/M7. В M2 `set`/`del` живут напрямую в `dq-core` + `dq-cli` без абстракции.
- Format-flags типа `--sort-keys`/`--indent N`/`--quote-style` — M4.
- Сохранение форматирования при `--allow-templates` mode — best-effort, без гарантий (это явный escape-hatch для пользователей, которые приняли trade-off).
- WindowsACL/symlink-handling beyond what `tempfile` уже даёт — отложено до M6 (distribution).

## Decisions

### D1. YAML round-trip через textual-edit (span-based) — `saphyr-parser` для structural discovery, не для emit

**Решение:** для write-команд на YAML файлах используем подход textual-edit, который проверен в Rust-экосистеме крейтом `toml_edit` для TOML. `crates/dq-core/src/parsers/yaml_spans.rs` использует **`saphyr-parser`** (низкоуровневый, event-based) исключительно чтобы пройти по документу и собрать `Pointer → ByteRange` span map. `Document` хранит `original_bytes: Vec<u8>` рядом со span map. **Никакого emitter'а не пишется.** `Document::set_at(&Pointer, Value)` рендерит новое значение в локальные байты и заменяет соответствующий span в `original_bytes`. Comments, blank lines, quote style вокруг spans остаются нетронутыми, потому что байты вокруг них никогда не переписываются.

**Read-pat M1 не трогаем:** read-команды (`get`, `paths`, `keys`, `values`, ...) продолжают использовать `serde_yml` как в M1 — он быстрее на parse-to-Value и его API стабилен. Write-команды используют параллельный saphyr-parser-based span builder. Это два code paths для одного формата, но миграция read-pat на saphyr-parser — отдельный refactor change post-M2 (если вообще нужен).

**Что мы не делаем и почему:**
- (a) Custom YAML emitter на saphyr-parser events — **технически нереализуемо** под M2 budget. Спайк ([spikes/saphyr/RESULTS.md](../../../spikes/saphyr/RESULTS.md)) показал, что saphyr-parser scanner отбрасывает comment-байты до event stream'а ([saphyr issue #103](https://github.com/saphyr-rs/saphyr/issues/103) с января 2026, без roadmap'а). Восстановить то, чего нет в input'е, emitter не может. Custom scanner+parser+emitter — ~6 месяцев работы.
- (b) `libfyaml` через FFI — единственный published parser, сохраняющий comment tokens. Отвергнуто: ломает M6 single-static-binary contract, добавляет C dependency, vendoring требует CI matrix expansion.
- (c) `yaml-rust2` 0.11.0 — та же scanner-level потеря (issue #21 explicitly redirected to saphyr).
- (d) Полный отказ от round-trip (Option A) — теряем главный differentiator.

**Trade-offs textual-edit подхода:**
- Insertion новых ключей не идеально форматируется (см. D14).
- Span map привязана к exact byte sequence — после `set_at` нужен пересчёт span'ов справа от изменения. Реализация — на спайке (Task 1.1).
- Need to handle YAML 1.2 quirks: when replacing scalar inside flow context (`{a: 1, b: 2}`) vs block context (`a: 1\nb: 2`) — синтаксис нового scalar'а должен match исходный context. Решается через "render replacement using context style detected from original span surroundings".

### D2. TOML round-trip через `toml_edit`, замена `toml`

**Решение:** в `crates/dq-core/src/parsers/toml.rs` переключиться с крейта `toml` на `toml_edit`. `toml_edit` уже **реализует именно textual-edit подход** — это и есть proof что D1 правильный. Он хранит full document tree с positions, comments, formatting, и предоставляет mutable API для surgical edit'ов. Используется `cargo` для `Cargo.toml` operations — миллионный production stress test.

**Альтернативы:**
- Продолжить `toml` + параллельная сериализация — невозможно по той же причине, что для YAML: `toml::Value` не имеет места под comments.
- Свой парсер — TOML грамматика проще YAML, но `toml_edit` уже зрелый.

**Trade-offs:** `toml_edit::DocumentMut` — не Value-like API. Нужен mapping `toml_edit::Item` ↔ наш `Value` (для read-команд через `Document::value()`). Возможны несовпадения семантики (datetime literals, inline tables vs standard tables) — fixed-test fixtures в `crates/dq-core/tests/parse_toml.rs` гарантируют совместимость с M1.

### D3. JSON preservation — собственный wrapper поверх `serde_json` + raw text capture

**Решение:** для JSON round-trip не используем сторонний крейт. Парсер в `crates/dq-core/src/parsers/json.rs` сохраняет:
- IndexMap key order (уже работает в M1 через `preserve_order` feature)
- Big-int как `BigInt(literal_text)` (уже работает в M1 через `arbitrary_precision`)
- Detected indent style (2-space, 4-space, tab) — определяется на парсинге, восстанавливается при write
- Trailing newline at EOF — сохраняется
- **Comments — JSON формально не поддерживает.** Если файл содержит JSONC-style `//` или `/* */`, парсер возвращает `Parse` error с сообщением "comments are not valid JSON; if this is JSONC, use --format jsonc (not yet supported)". Это honest behaviour — JSONC поддержка отложена, нет faked.

**Альтернативы:**
- `serde_json::Value` + diff — теряет порядок ключей и indent. Отвергнуто.
- `jsonc-parser` крейт — добавляет зависимость только под edge case JSONC, который мы пока не поддерживаем.

**Trade-offs:** JSON round-trip проще YAML/TOML потому что нет comments. Большая часть работы — выбрать правильный indent при write. Detection: смотрим первые 5 строк, считаем leading spaces/tabs. Если ambiguous (single-line minified) — default 2 spaces.

### D4. Document model: `Value` + `original_bytes` + `Pointer → ByteRange` span map

**Решение:** `Value` enum остаётся как в M1 — простая sum type. `Document` расширяется двумя новыми полями для write-pat'а; read-pat (`Document::value()`) ничего не знает о них:

```rust
pub struct Document {
    root: Value,                                  // M1 — read-pat использует это
    original_bytes: Vec<u8>,                      // M2 — текст исходного файла, нетронутый
    spans: indexmap::IndexMap<String, ValueSpan>, // M2 — Pointer.canonical() → location info
    format: Format,                               // нужен для render replacement в правильном style
}

pub struct ValueSpan {
    /// Byte range в `original_bytes` который покрывает значение узла —
    /// то, что заменяется при `set_at`. НЕ включает ключ или surrounding whitespace.
    value_range: std::ops::Range<usize>,
    /// Byte range всей "logical line" узла — ключ + значение + trailing comment.
    /// Используется при `del_at` чтобы удалить вместе с indent + newline.
    line_range: std::ops::Range<usize>,
    /// Indent текущего узла (для render insertion в эту же позицию).
    indent: u32,
    /// Стиль контекста: block vs flow. Render replacement должен match.
    context: SpanContext, // BlockMapValue | BlockSeqItem | FlowMapValue | FlowSeqItem
}
```

**Принцип работы:**
- `set_at(&pointer, value)`: lookup `spans[pointer.canonical()]` → render `value` в `Vec<u8>` под текущий `context` + `indent` + format-specific quote-style → `original_bytes.splice(span.value_range, rendered)`. Затем — пересчёт offsets всех spans справа от изменения (delta = rendered.len() - span.len()).
- `del_at(&pointer)`: удалить `span.line_range` (ключ + значение + comment + trailing newline). Пересчёт offsets справа.
- `set_at` для несуществующего pointer'а с mkdir-p: проходим вверх по pointer'у до первого существующего ancestor (через `spans` лookup), рендерим **новый поддокумент** (key:value chain до конца pointer'а), вставляем в конец mapping/sequence ancestor'а в правильный indent. **Это где живёт D14 (heuristic insertion).**

**Альтернативы:**
- (a) Embed metadata в `Value` (e.g., `AnnotatedValue { value, meta }`) — ломает все M1 read pattern matching, требует migration 200+ usages.
- (b) `Annotated<T>` newtype — то же самое, меньше, но всё равно ломает M1.
- (c) **Параллельная metadata map** (вариант, который мы рассматривали изначально) — невозможен потому что comment-метаданных просто нет в input event stream'е (D1 explanation). Хранить пустые metadata бессмысленно.
- (d) `original_bytes + spans` (выбранное) — M1 read-команды получают `&Value` через `Document::value()`, не зная о spans. Write-команды получают `&mut Document::original_bytes` и переписывают spans. Чистое разделение concerns.

**Trade-offs:**
- Span pересчёт после каждого `set_at`/`del_at` — O(N) проход по всем spans (где N = количество узлов). Acceptable — для 1MB k8s manifest это ~5000 spans, microsecond-level операция.
- `Document` теперь несёт `Vec<u8>` целиком в памяти — для 100MB файла это 100MB памяти. Acceptable для M2 use cases (config файлы редко > 10MB).
- Бинарная attached-data — нельзя сериализовать `Document` через `serde::Serialize` напрямую (если нужно — отдельный serializer skip'ает spans). Не критично — мы не сериализуем `Document`, мы пишем `original_bytes` как byte stream.

### D5. Atomic write: `tempfile::NamedTempFile::persist()` + same-directory placement

**Решение:** реализация в `crates/dq-core/src/atomic_write.rs`:

```rust
pub fn write_atomic(path: &Utf8Path, content: &[u8], backup: bool) -> Result<()> {
    let dir = path.parent().expect("file path must have parent");
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(content)?;
    if backup && path.exists() {
        std::fs::copy(path, path.with_extension("bak"))?;
    }
    tmp.persist(path).map_err(|e| Error::Io { path: path.into(), source: e.error })?;
    Ok(())
}
```

Same-directory placement критичен — `rename` через filesystem boundaries не атомарен (на Linux это `EXDEV`). `tempfile::NamedTempFile::persist()` обрабатывает Windows-specific case (там `MoveFileExW` с `MOVEFILE_REPLACE_EXISTING`).

**Альтернативы:**
- `std::fs::write` + ручной `rename` — теряет atomic guarantee при crash между write и rename.
- `atomicwrites` крейт — добавляет dep, но `tempfile` уже есть и предлагает то же самое.

**Trade-offs:** На Windows `rename`-over-open-handle всё равно может fail если файл открыт другим процессом. **Этот edge case покрывается отдельным тестом** на Windows-CI (или явно skip с TODO до M6, если CI-matrix недоступен в M2 — обсудить в open questions).

### D6. Helm/Go-template guard: regex-detection с whitelist escape-hatches

**Решение:** препроцессор в `crates/dq-core/src/template_guard.rs`:

```rust
const TEMPLATE_RE: &str = r"\{\{[-\s]?[\.\w]";  // {{ .Values.x }}, {{- if ...}}

pub fn detect_templates(bytes: &[u8]) -> Option<TemplateMarker>;
pub struct TemplateMarker { pub line: u32, pub snippet: String }
```

Запускается **до** парсинга в `Format::parse`. Если detected:
- Default — `Error::TemplatedFile { detected_marker: ... }`, exit 3 (PARSE_ERROR), message с двумя escape-hatch.
- `--allow-templates` — пропускаем check, парсим как обычный YAML. Шаблоны типа `{{ .x }}` — невалидный YAML, `serde_yml`/`saphyr` упадут на парсинге. Этот режим имеет смысл только для частично-templated файлов (например, GitHub workflow с `${{ ... }}` — там GitHub Expression Language, не Go template, но синтаксис похож).
- `--raw-template-strings` — препроцессор заменяет `{{ ... }}` на placeholder string (например, `__DQ_TPL_<sha256>__`) перед парсингом, при write — обратно. Это даёт работающий round-trip для Helm charts ценой того, что `dq` не понимает значения внутри шаблонов (для агента, который меняет `image.tag` — это OK).

**Альтернативы:**
- Нет guard'а вообще — пользователь получит cryptic YAML parse error. Plain bad UX.
- Только error без escape-hatches — закрывает Helm use case полностью. Большая часть DevOps-пользователей работает с Helm.
- Учить `dq` понимать Go template AST — отдельный гигантский проект, не M2.

**Trade-offs:** Regex может дать false positive на YAML, который случайно содержит `{{` (например, в строке-литерале с {{ как символами). Mitigation — только если это первый non-whitespace token в строке или follows `:` (структурная позиция). На спайке проверим на 50+ Helm charts и 20+ "обычных" YAML.

### D7. Diff display через `similar` крейт

**Решение:** для `--diff` флага использовать `similar = "2"` (проверенный, активно поддерживаемый, использует Myers/Patience algorithms). Output — unified diff с цветом (если `use_color=true`).

**Альтернативы:**
- `diffy` — менее популярен, минимальный API.
- Свой Myers diff — overkill, ~500 строк не-тривиального кода.

**Trade-offs:** `similar` добавляет ~30 KB в release binary. Acceptable.

### D8. Exit code 7 (WRITE_FAILED) и mapping

**Решение:** добавить в [crates/dq-cli/src/exit_code.rs](../../../crates/dq-cli/src/exit_code.rs):

```rust
pub const WRITE_FAILED: i32 = 7;
```

Mapping в `exit_code_for_error`:
- `Error::Io { ... }` где path — write target, не read source → `WRITE_FAILED` (новое)
- `Error::TemplatedFile { ... }` → `PARSE_ERROR` (3) — это parse-time issue
- Existing mappings (Path → 2, Parse → 3) не меняются

**Различение read-IO vs write-IO**: мы знаем контекст в command handler — `set`/`del` оборачивают write-side errors в `WriteIoError` newtype, который downcast'ится отдельно. Альтернативно — добавить флаг `Error::Io { during_write: bool }`, но newtype чище.

### D9. Write CLI dispatch: `set` принимает значение из 4 источников

**Решение:** `dq set <FILE> <POINTER> [VALUE]`:

| Источник значения | Триггер | Парсинг |
|---|---|---|
| Inline argument | `dq set f.yaml /x foo` | string literal, попытка JSON-parse если starts with `{`/`[`/digit/`true`/`false`/`null` |
| Stdin | `dq set f.yaml /x -` | Весь stdin как `-F json` (или явный `--value-format yaml`) |
| File | `dq set f.yaml /x @value.json` | Detect format по extension, parse |
| Flag `--value-from` | `dq set f.yaml /x --value-from data.yaml` | Same as `@`, явный синтаксис |

`-` и `@` соглашения совпадают с `curl`/`jq` — знакомые агенту и человеку.

**Альтернативы:**
- Только inline string — ломает edge case "set value to a complex object".
- Отдельная команда `dq set-from-file` — ухудшает discoverability.

**Trade-offs:** "looks like JSON" эвристика может быть surprising. Mitigation — `--value-string` форсирует строку даже для `"true"`/`"42"`.

### D10. Sub-pointer `set`: создаёт промежуточные узлы; `del` ошибается

**Решение:**
- `dq set f.yaml /a/b/c value` где `/a/b` не существует → создаёт `{a: {b: {c: value}}}` (mkdir-p semantics). Семантика наследуется от `jq` `setpath`.
- `dq del f.yaml /a/b/c` где `/a/b/c` не существует → exit 2 (NOT_FOUND), не silent.

Опция `--no-create` (на `set`) форсирует exit 2 если pointer не существует — для агентов, которые проверяют, что они правят что-то конкретное.

**Альтернативы:**
- Симметрия (`set` ошибается тоже) — ломает добавление новых ключей, главный use case.
- Симметрия в обратную сторону (`del` silent) — ломает CI-сценарии типа "ensure key absent" — нет способа узнать, был ли key вообще.

### D11. Spike POC для textual-edit подхода: критерии успеха

**Решение:** Task 1.1 в `tasks.md` — спайк textual-edit POC поверх `saphyr-parser` events. Цель спайка — построить minimal `SpanMap` из event stream'а и продемонстрировать byte-exact preservation на single-scalar mutation. Критерии успеха фиксируются ДО старта:

1. **Span discovery работает:** для 5/5 fixture'ов (те же что в archived spike — `spikes/saphyr/fixtures/`) парсер выдаёт positions для каждого scalar value, и Pointer→ByteRange map строится без unwrap'ов и panic'ов.
2. **Single-scalar mutation byte-perfect:** на 5/5 fixture'ов выполнить `set_at` на одном scalar (например, `/spec/replicas` на k8s, `/title` на hugo, etc.). Diff между source и output должен быть **ровно одна изменённая строка** — comments, blank lines, surrounding indent, anchor declarations нетронуты. Это **главный критерий** — он валидирует, что весь подход работает.
3. **Insertion работает на простом случае:** на 1 fixture'е выполнить `set_at` на новый key (например, `/spec/strategy/type RollingUpdate` на manifest без strategy). Result должен быть валидным YAML, parsable через `serde_yml::from_slice`. Идеального форматирования не требуем (D14) — только validity + правильный indent относительно parent mapping'а.
4. **Performance:** парсинг + span build для 1MB k8s manifest ≤ 100ms на M1 MacBook. Span recompute после mutation ≤ 5ms.
5. **API ergonomics:** `Document::set_at(&Pointer, Value)` написать без unsafe и без >3 уровней вложенных match'ей (исключая render-replacement helper, который может быть формат-specific).

Архивированный spike (`spikes/saphyr/`) уже закрыл вопрос про event-stream rewrite — этот новый POC focused строго на textual-edit. Если критерий 2 (mutation byte-perfect) или 5 (ergonomics) fail → эскалация: либо переключение на Option D (`fyaml` через FFI с vendored libfyaml), либо Option A (no-preserve fallback). **Спайк не может занять > 2 недель** — если за 2 недели критерий 2 не выполнен, эскалация решения.

### D12. Что НЕ переезжает в `dq-transform` в M2

**Решение:** `set`/`del` живут в `crates/dq-core/src/document.rs::Document::{set_at,del_at}` (low-level, чистая mutation на Document), вызываются из `crates/dq-cli/src/commands/{set,del}.rs`. `dq-transform` остаётся placeholder.

**Rationale:** в M3 `Op` enum с `Set`/`Delete`/`Merge`/`Patch` natural место для миграции; M2 — преждевременная абстракция. Ровно одна команда per crate (set, del) — нет полиморфизма, нечего абстрагировать.

### D13. `serde_yml` advisory ignore остаётся в M2

**Решение:** `serde_yml` остаётся в dep tree `dq-core` (для read-pat), поэтому ignore в [deny.toml](../../../deny.toml) сохраняется. Удаление крейта — отдельный refactor change post-M2 если когда-либо будем унифицировать read-pat на saphyr-parser-based парсер. **Это сознательный compromise:** read-pat M1 работает, тесты зелёные, миграция ради консистентности — overkill в M2 budget.

### D14. Insertion of new keys/maps: heuristic emitter for inserted region

**Решение:** при `set_at` на pointer, который не существует целиком (требует mkdir-p), генерируем новый text fragment для inserted region через **format-specific heuristic emitter** — не через сохранение оригинального форматирования (его нет, узел новый). Эмитттер живёт в `crates/dq-core/src/textual_edit/insert_yaml.rs` (и аналоги для TOML/JSON) и генерирует:

- YAML: block-style mapping с indent = `parent_indent + 2`, scalar в bare-style если возможно, double-quoted если содержит спец. символы. Comments не ставим. Blank lines не ставим.
- TOML: для inline-tables используем inline syntax, иначе — добавляем новую `[parent.section]` в конец файла. `toml_edit` уже делает это правильно — переиспользуем его API.
- JSON: indent наследуется из root metadata (D3); ничего special.

**Trade-off:** insertion не выглядит как "написал бы человек" — нет comment'а, нет blank line отделителя, indent может не соответствовать локальной convention. **Документируется в man page для `set` и в README:** "Inserting new keys uses default formatting; existing keys preserve formatting exactly. Run `dq fmt` after bulk insertions for polish (M4)."

**Альтернативы:**
- Idеal "human-like" emitter — отвергнуто: требует understanding локального стиля файла (какой indent, как форматируются комментарии, какой quote style preferable), что близко к решению full-emitter problem'ы D1.
- Запретить insertion (`set` ошибается на missing pointer без `--no-create`) — отвергнуто: ломает основной use case "agent добавляет label/annotation в манифест".

**Implementation guard:** unit-тест `test_insert_renders_valid_yaml` парсит результат через `serde_yml::from_slice` — если новая insertion ломает синтаксис, тест fail'ит. Это безопасность от regression'ов.

## Risks / Trade-offs

- **[Risk] `saphyr-parser` 0.0.6 не выдаёт positions для каждого Event** (issue в API: некоторые ScalarStyle варианты могут терять offset). → **Mitigation:** спайк в Task 1.1 explicitly проверяет это. Если positions неполные — fallback на manual byte scan через `marker.index()` API, либо переключение на Option D (libfyaml).
- **[Risk] Span recompute после mutation становится bottleneck** для документов с >50k узлов. → **Mitigation:** golden runner включает 1MB+ файлы; если slow — переключение на BTreeMap-based span store с O(log N) rebalance вместо linear scan.
- **[Risk] Atomic write на Windows ломается в edge case open-file** (антивирус держит handle). → **Mitigation:** явный Windows-CI smoke test, документированный fallback на retry-with-backoff (3 попытки) с tracing::warn.
- **[Risk] Helm template guard regex даёт false positive** на legitimate YAML, который случайно содержит `{{`. → **Mitigation:** structural-position check (D6), тест на 20+ "обычных" YAML без шаблонов в первой неделе. Если false positive ratio > 1% — переключаемся на context-aware lexer (медленнее, но точнее).
- **[Risk] `set` semantics неоднозначны** для типизации (`set /x 42` — int или string `"42"`?). → **Trade-off:** D9 описывает heuristic; `--value-string` — явный escape. Документировать в man page.
- **[Risk] Big-int precision ломается** через round-trip set→get. → **Mitigation:** proptest в DoD с генерацией случайных big int строк длиной до 100 символов.
- **[Trade-off] `--allow-templates` без round-trip гарантий** — пользователь явно accept'ит, что форматирование может сломаться. Документация и `tracing::warn!` при включении флага.
- **[Trade-off] M2 не делает bulk** — `dq set 'k8s/**/*.yaml'` не работает до M3. Mitigation: README/man page явно ссылается на M3 для bulk; обходной путь — shell loop + `xargs`.

## Migration Plan

M2 — incremental добавление поверх M1, без breaking changes для read-команд:

1. **Spike (textual-edit POC)** (1-2 weeks) — `tasks.md` Section 1. Decision point: критерий 2 (single-scalar mutation byte-perfect) green-lights весь раздел.
2. **`dq-core` write-pat parser** (1 week) — `saphyr-parser` based span builder в `parsers/yaml_spans.rs`; `toml_edit` migration в `parsers/toml.rs`; JSON span builder в `parsers/json.rs`. Read-pat (`serde_yml`) НЕ трогаем — M1 read тесты должны остаться зелёными без правок.
3. **`Document::set_at`/`del_at` API + textual_edit helpers + atomic_write** (1 week) — низкоуровневые mutation + write helpers + insertion emitter (D14).
4. **Template guard** (3 days) — regex + escape-hatches.
5. **CLI commands `set`/`del`** (3 days) — handler + dispatch + Cli args + unit tests.
6. **`--diff` через similar** (2 days).
7. **Golden runner update** (3 days) — 30+ файлов, snapshot diff'ы (один scalar mutation = ровно один changed line).
8. **Windows CI atomic-write smoke** (TBD — см. open questions).
9. **DoD verification**, archive change.

**Rollback:** при провале спайка (критерий 2 fail) — change архивируется без merge, заводится новый `m2-libfyaml` change (Option D — vendored libfyaml через FFI) или `m2-no-preserve` (Option A — fallback без preservation).

## Open Questions

1. **Windows CI matrix в M2 или ждать M6?** Сейчас `lefthook.yml` запускает только локальные проверки на dev-машинах (Mac/Linux). DoD M2 требует Windows atomic-write тест. Варианты:
   - (a) Поднять GitHub Actions Windows job уже в M2 — lifts CI infrastructure раньше плана.
   - (b) Skip Windows test с `#[cfg(not(target_os = "windows"))]` + TODO до M6 — риск нагнать regression в M3-M5.
   - **Рекомендация:** (a) — поднять минимальный Windows job (`cargo test atomic_write` only, не весь suite) в M2. Это 1 файл `.github/workflows/windows-atomic-write.yml`, ~30 LoC.

2. **Span representation: `IndexMap<String, ValueSpan>` vs `BTreeMap` vs flat `Vec<(Pointer, Span)>`** (D4 implementation detail). Решается на спайке (Task 1.1) на основе performance профиля и ergonomics. По умолчанию `IndexMap` — O(1) lookup + поддерживает порядок добавления (полезен для debug).

3. **`--value-format` flag для stdin/file source** (D9): нужен ли явный override, или довольствуемся detect-by-content/extension? Inclination — добавить, потому что для stdin extension'а нет, а content detection — heuristic. Финальное решение — после набросков handler'а.

4. **`set` numeric typing heuristic** (D9): "looks like JSON literal" → JSON parse. Edge case — `set /x 1.0` = float `1.0` или string `"1.0"`? jq делает float. Принимаем `jq` semantics (float), документируем.

5. **`dq-transform` совсем пустой в M2 vs минимальный re-export?** D12 говорит "placeholder". Альтернатива — re-export `Document::set_at`/`del_at` из `dq-transform::ops::{Set, Del}` сразу как stub `Op` enum, чтобы M3 не делал structural перестройку. Inclination — оставить placeholder, M3 free выбирать форму без backward concerns. Финальное — на старте раздела "tasks → CLI commands".
