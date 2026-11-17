## Context

M2 закрыл safe-write контракт для **одного файла** через textual-edit (saphyr-parser span builder + toml_edit + ручной JSON span scanner). M3 поверх этого добавляет три ортогональных слоя: (a) транс­формационные команды `patch`/`merge`/`diff`, которые принимают operations-as-data вместо single-pointer mutation; (b) glob expansion для всех write-команд, чтобы одна инвокация покрывала десятки файлов; (c) `convert -i`, который M2 явно отложил.

Главный technical risk M3 — **не round-trip** (это решено в M2), а **operational consistency**: bulk-mode должен вести себя предсказуемо при partial failure, а parallel write не должен ломать atomic-write контракт M2. Решения вокруг этих двух — center of gravity дизайна.

**Текущее состояние:** M2 archive landed (`2026-05-03-add-safe-writes`). 438 тестов зелёные. Active changes: `add-bulk-and-ci` (этот документ).

**Constraints:**
- Anti-scope per [dq-plan.md:381](../../../dq-plan.md): formatting флаги (`--sort-keys` etc.) — M4; jaq трансформации (`set --jq`) — M7; линтеры — M8+; markdown — M9; transactional bulk-write (rollback всех файлов при failure любого) — никогда (нет roadmap entry).
- Конвенции `/rust-cli` без изменений: тонкий main.rs, Reporter с DI, exit-codes как named constants, нет `println!`.
- Делегирование Rust-правок через rust-cli-writer / rust-cli-test-writer — без исключений.
- M2 single-file behaviour должно остаться bit-identical. Если M3 регрессирует существующий golden snapshot — это блокер.

**Stakeholders:**
- AI-агенты в CI: `--check` mode для pre-commit / PR gate; `diff` как структурированный JSON output для analysis; `patch`/`merge` как operations-as-data interchange.
- DevOps: `dq set 'k8s/**/*.yaml' /spec/replicas 3 -i` — главный selling point M3 vs `find -exec`.
- Future milestones: M4 `fmt --check` использует тот же bulk driver; M10 autofix использует `apply_patch` для применения rule fix'ов.

## Goals / Non-Goals

**Goals:**
- `dq patch <FILE> @ops.json` применяет RFC 6902 patch с честной семантикой `test` (failure → весь patch откатывается, файл не пишется). `add`/`remove`/`replace`/`move`/`copy` работают через те же `Document::set_at` / `del_at` примитивы.
- `dq merge <FILE> @patch.json` применяет RFC 7396 merge — рекурсивный объект-merge, `null` удаляет ключ, не-объекты заменяют.
- `dq diff a.yaml b.yaml` производит `Vec<PatchOp>` (минимальный — без избыточных вложенных replace'ов когда родитель уже заменён).
- `dq set 'k8s/**/*.yaml' /spec/replicas 3 -i` обрабатывает все matched files и печатает `Modified: N, Skipped: M, Failed: K` summary. Partial failure → exit 7, успех → exit 0.
- `--check` exits 1 если хотя бы один файл нуждается в изменении; 0 если все идентичны expected output. Не пишет на диск.
- `--parallel N` (N>1) выполняет writes через rayon thread pool. Atomic-write контракт сохраняется per-file.
- `dq convert deploy.yaml -i -F json` → `deploy.json` создан, `deploy.yaml` удалён (с `--keep-source` — оба остаются).

**Non-Goals:**
- Transactional bulk: rollback **всех** файлов при partial failure. Сложность ≫ value (атомарность через `tempfile.persist()` уже даёт per-file гарантии; cross-file rollback требует distributed-lock-style протокола, и use case практически не возникает в DevOps workflows). M3 явно говорит "успешные файлы остаются изменёнными, summary называет неудачные".
- `--strict` order-sensitive comparison в `diff` (разные порядки ключей считаются разными). Per dq-plan.md "не входит в M3" — semantic diff игнорирует key order, как и `set`.
- Markdown / линтеры / формат-флаги — отдельные milestones.
- Custom JSON Patch dialects (RFC 7386 draft, custom merge strategies). Только две стандартные: 6902 и 7396.

## Decisions

### D1. RFC 6902 / RFC 7396 — apply layer строится поверх `Document::set_at` / `del_at`, не дублирует логику

**Решение:** `dq_core::transform::patch::apply_patch(&mut Document, &[PatchOp])` транс­лирует каждый `PatchOp` в одну или две `set_at`/`del_at` operations. Это сохраняет round-trip семантику бесплатно (textual-edit pipeline уже работает), и не требует параллельной "patch engine" реализации.

Mapping:
- `add /a/b value` — `set_at(&Pointer, Value)` (mkdir-p уже работает в Section §3+ M2; этот change не меняет insertion semantics).
- `remove /a/b` — `del_at(&Pointer)`.
- `replace /a/b value` — `set_at(&Pointer, Value)`. RFC 6902 требует, чтобы target существовал; мы проверяем через `Document::span_at(&pointer)` PRE-write — если `None` → `Error::Path { kind: MissingKey }`.
- `move /from /to` — `value = get(from)` + `del_at(from)` + `set_at(to, value)`. Атомарность в рамках одной apply_patch вызова — сохраняется (вся операция либо проходит целиком, либо вся откатывается, см. D2).
- `copy /from /to` — `value = get(from)` + `set_at(to, value)`. `from` остаётся.
- `test /a/b expected` — read через `Document::value()` + `Pointer::resolve`, сравниваем с expected. На несовпадение → `Error::PatchTestFailed { pointer, expected, actual }` (новый variant в `dq_core::Error`).

**Альтернативы:**
- Реализовать patch engine на `serde_json::Value` без участия Document — теряет round-trip формат-preservation. Отвергнуто.
- Использовать готовый крейт `json-patch` 1.x — мог бы работать, но (a) он работает на `serde_json::Value`, не на `Document`, что снова теряет format preservation; (b) добавляет dependency без выигрыша — RFC 6902 simple enough to implement directly (~200 LOC).

**Trade-offs:** atomicity через "build buffer first, then write" — apply_patch сначала клонирует `Document`, применяет ops к клону, и только при успехе атомарно заменяет original_bytes target Document. Memory cost = один лишний clone — приемлемо для human-scale файлов.

### D2. Patch atomicity — clone-on-apply, не write-on-each-op

**Решение:** `apply_patch` работает на cloned Document. Если любая op (включая `test`) failed → возвращает Err и оригинальный Document не тронут. Только при успехе всего patch'а — заменяем `original_bytes` target'а через `*self = clone`. Это реализует RFC 6902 §5: "any error condition during the application of any operation MUST cause the entire patch to be discarded".

Тот же подход для `apply_merge` (RFC 7396 не имеет error conditions per spec, но we keep contract uniform — null/structural-mismatch errors могут возникнуть в нашей implementation, see D3).

**Альтернативы:**
- Per-op atomicity с rollback log — overkill, добавляет complexity без user-visible benefit.
- Stateless functional apply (return new Document) — менее ergonomic для bulk driver, который держит mutable refs к Documents.

**Trade-offs:** clone overhead ~файл-size bytes per patch. Для типичного manifest'а (~10KB) — копейки. Для огромных файлов (>10MB) — measurable; mitigation — bulk driver обрабатывает файлы по одному, peak memory = max file size.

### D3. RFC 7396 merge — `null` removes, recursion на map'ах, replace на не-объектах

**Решение:** `apply_merge(target: &mut Document, patch: &Value)` рекурсивно:
1. Если `patch` не `Map` — replace target value целиком.
2. Если `patch` — `Map`:
   - For each `(key, value)` in patch:
     - If `value == Value::Null` — `del_at(target, /key)` (если key отсутствует — silent NOP, RFC 7396 §1).
     - Else if `target[key]` is `Map` AND `value` is `Map` — recurse.
     - Else — `set_at(target, /key, value)`.

Это точно RFC 7396 алгоритм. Реализация ~50 LOC.

**Trade-offs:**
- RFC 7396 не позволяет вставлять `null` как значение (всегда означает remove). Для use case "set field to literal null" пользователь должен использовать `dq set` или `dq patch` с `replace` op. Documented в команде `--help`.
- Array merge — full replace (RFC 7396 §1: "если target value является массивом, он будет заменён массивом из patch'а"). Это intentional; element-wise array merge — non-standard и introduces ambiguity.

### D4. Structural diff — recursive walk producing minimal `replace`/`add`/`remove` ops, NO `move`/`copy`

**Решение:** `diff(a: &Value, b: &Value) -> Vec<PatchOp>`:
1. If `a == b` → empty.
2. If types differ — `[PatchOp::Replace { path: "", value: b.clone() }]`.
3. If both `Map` — for each key in symmetric difference: emit `Add`/`Remove`. For each common key: recurse, prepending `/key` to all returned ops.
4. If both `Array` — index-aligned recursion (RFC 6902 array semantics: `replace /a/0 v` для index 0). Длина различается — emit `Remove` для лишних или `Add` (with append index `-`) для добавочных.
5. If both scalars — `[PatchOp::Replace { path: "", value: b.clone() }]`.

**Минимальность:** алгоритм НЕ ищет `move`/`copy` opportunities — это NP-hard в общем случае (longest common subsequence + similarity scoring). Production diff/merge tools (k8s strategic merge patch, jq diff) тоже не оптимизируют move/copy. Trade-off: patches могут быть длиннее теоретического minimum, но генерация — линейная по размеру.

**Альтернативы:**
- Использовать `json-patch` крейт's `diff` — работает на `serde_json::Value`, конверсия туда-обратно теряет точность для BigInt (мы её не теряем потому что наш `Value` это разные variant'ы). Отвергнуто.
- LCS-based array diff — сложность не оправдана.

**Trade-offs:** для перестановок элементов массива получается длинный patch (`replace /0` ... `replace /N`). User-friendly summary в CLI отчёт mitigates this.

### D5. Glob expansion — `globset` крейт, expansion в bulk driver, не в clap

**Решение:** glob argument остаётся `Utf8PathBuf` на уровне clap (clap не делает file-system access). Bulk driver `crates/dq-cli/src/bulk.rs::expand_glob(pattern: &Utf8Path) -> Vec<Utf8PathBuf>` определяет, нужно ли expand'ить (в pattern есть метахаракатеры `*`/`?`/`[`/`{`) и при необходимости walks dir tree через `walkdir` + `globset::GlobMatcher::is_match` per entry.

Если паттерн **не** содержит метасимволов — early-return `vec![pattern.into()]` (single-file fast path, M2 behaviour preserved). Если содержит — обрабатываем как glob.

**Альтернативы:**
- Polluting clap value parser glob expansion — clap value parser не должен делать I/O.
- Использовать `glob` крейт вместо `globset` — `globset` поддерживает `**` always-on (без feature flag), множественные patterns эффективнее, и API ergonomic.
- Использовать shell-side expansion only — OK для интерактивного use, но shell quoting argues для unsupported pattern (`'k8s/**/*.yaml'` quoted делегирует glob программе, не shell'у). И на Windows shell expansion работает иначе. Унифицируем CLI behaviour.

**Trade-offs:**
- Detection метаhereтра через простой `pat.contains(['*','?','[','{'])` — может ложно срабатывать на literal пути с `[` (Windows volume letters в path не triggered, но user-supplied filenames могут). Mitigation: documented в `--help`, и если user реально хочет literal — quoting через shell или escape via `\[`.
- Performance: walkdir на корневом directory может быть медленным для огромных trees. Mitigation: glob detector извлекает longest non-meta prefix из pattern и стартует walkdir с него (e.g., `'helm/charts/**/*.yaml'` стартует с `helm/charts`).

### D6. Parallel driver — rayon `par_iter` over Vec<Utf8PathBuf>, `--parallel N` через `ThreadPoolBuilder`

**Решение:** `bulk::run_per_file(files, op, parallel)`:
1. If `parallel == 1` — sequential `for file in files`.
2. If `parallel > 1` — `ThreadPoolBuilder::new().num_threads(parallel).build_scoped(|s| s.spawn(...))`.
3. If `parallel == 0` — use `rayon::current_num_threads()`.

Per-file op возвращает `BulkResult { path, status: Modified | Unchanged | Failed(Error) }`. Driver собирает Vec результатов, печатает summary, возвращает aggregated exit code.

**Alternative: pure `std::thread::spawn` pool** — больше boilerplate, нет ergonomics rayon's scoped spawn. Отвергнуто.

**Trade-offs:**
- Output ordering: при parallel >1 порядок per-file output (если есть, например `--diff` mode) недетерминированный. Mitigation: per-file output буферизуется в `Vec<u8>`, печатается в финальной serial-fashion в порядке matched files после join'а thread pool. Это compromise: память pиgrows с числом файлов (но bulk-mode-with-parallel предполагает large number of files и user accepts the trade).
- File system contention: parallel writes в одну директорию могут лимитировать throughput через FS metadata locks. Mitigation: эмpically — modern Linux/macOS fine с десятками concurrent rename'ов; defaults оставляем sequential (`--parallel 1`).

### D7. `--check` mode — третий output mode, mutually exclusive с `-i` и `--diff`

**Решение:** `--check` flag добавляется на тот же уровень что `-i`/`--diff` (global). `Cli::ensure_write_flags_consistent` запрещает `--check` + `-i`, `--check` + `--diff`. Семантика:
- Read source.
- Apply transformation.
- Compare result vs source bytes-equal.
- Exit 0 если equal (file is up-to-date).
- Exit 1 если differ (file would be modified).
- Стандартный stdout: structured summary (`would modify: <file>` per file). Совместимо с `--continue-on-error` для bulk.

`--check` + `WRITE_FAILED` — невозможно по конструкции (no write happens), exit 1 reserved для "needs change" semantically.

**Альтернативы:**
- Re-purpose `--diff` в `--check`-like mode — конфликтует с user expectations (`--diff` shows the diff, не gates).
- `--check` как extra subcommand `dq check set ...` — reduce CLI surface clarity. Flag wins.

### D8. Bulk exit codes — partial failure → 7, full success → 0, --check changes pending → 1

| Mode | All files pass | Some fail | All fail | --check, no changes pending | --check, ≥1 change pending |
|---|---|---|---|---|---|
| Single-file (M2 preserved) | 0 | n/a | exit_for_first_error | n/a | n/a |
| Bulk no `--continue-on-error` | 0 | exit_for_first_error (abort) | exit_for_first_error | n/a | n/a |
| Bulk `--continue-on-error` | 0 | 7 (WRITE_FAILED) + summary | 7 | n/a | n/a |
| `--check` | n/a | n/a | n/a | 0 | 1 |

Semantics:
- `--continue-on-error` без `7` если все pass — partial-failure marker, reusing existing constant. `7` уже есть из M2.
- `--check` → 1 не collides с GENERIC: `dq` semantics уже have `exists` returning 1 для "false". Это same family — "answer to a yes/no question is no". Documented в spec.

### D9. `convert -i` — file rename + remove источника, atomic-ish

**Решение:** `dq convert deploy.yaml -i -F json`:
1. Read deploy.yaml, parse, render as JSON.
2. Compute target path: derive from source by swapping extension to `-F` value. Так если source `deploy.yaml` и `-F json` → target `deploy.json`. Если source path не имеет extension → error (cannot disambiguate).
3. Atomic write (через `atomic_write::write`) на target path.
4. **Только при успехе пункта 3** — remove источника. С `--keep-source` — пропустить пункт 4.
5. Если target == source (e.g., `convert .yaml -i -F yaml` — same format) — error InvalidInput "convert -i to same format is a no-op; remove -F".

**Альтернативы:**
- Strictly atomic transition (target write + source delete как один atomic op) — невозможно на standard FS, требовало бы transactional FS support. Mitigation: order операций так, что crash между write и delete оставляет ОБА файла на диске (recoverable state); reverse ordering оставил бы НИ одного (data loss).
- В случае user explicitly хочет atomic source delete — рекомендуем `dq convert ... -F json > new.json && rm old.yaml` или явная двухэтапка.

**Trade-offs:**
- Crash recovery: после crash возможно target существует но source не удалён. User видит две копии — manual recovery trivial.
- Race с conccurrent process читающим source — out of scope (M2 contract: single-process operation).

### D10. `bulk::run_per_file` — common harness for set/del/patch/merge/convert

**Решение:** общая абстракция в `crates/dq-cli/src/bulk.rs`:
```rust
pub trait FileOp {
    fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult>;
}
pub enum FileOpResult {
    Modified { diff: Option<String> },  // --diff mode populates this
    Unchanged,
    Skipped(String),  // user-facing reason
}
pub fn run_per_file(
    files: Vec<Utf8PathBuf>,
    op: &dyn FileOp,
    cli: &Cli,
    out: &mut dyn Write,
) -> anyhow::Result<()>;
```
Каждый command handler собирает `op` (closure-captured args) и вызывает `run_per_file`. Driver обрабатывает: glob expansion, parallel execution, summary, exit code aggregation, `--check` short-circuit, `--diff` per-file marker.

**Trade-offs:** trait + dyn dispatch — minor cost, but enables single test surface for bulk semantics (`tests/cli_bulk.rs`). Альтернатива (generic over `Fn`) — теряет dyn dispatch but less testable as one unit.

### D11. Plan delta — какие части dq-plan.md обновляются

После archive M3:
- `dq-plan.md:371-383` (M3 секция) получает `✅ Implemented YYYY-MM-DD` marker.
- Tech stack ссылка на `globset` и `rayon` фактически приходят в M3 (раньше упоминались в плане как M3+ deps). Markdown-таблица tech stack обновляется.
- README status line: `M3 alpha`.

**Альтернативы:** оставить план без апдейтов до M12 — теряет visibility прогресса и breaks стиль M1/M2 archives.

## Risks / Trade-offs

- **R1 (medium): parallel write contention.** Default `--parallel 1` mitigates. Users opt-in to parallel и принимают memory + FS contention trade.
- **R2 (medium): glob detection false-positives на путях с `[`.** Linux/macOS-only (Windows volume letters не используют `[`). Mitigation: documented escape, и user может всегда обернуть literal в `--no-glob` flag (TBD — оценить нужно ли в M3 или достаточно `\[` через shell).
- **R3 (low): clone-on-apply memory cost.** Negligible for human-scale documents (<1MB). Multi-MB файлы → overhead measurable but still bounded.
- **R4 (low): RFC 6902 `move` через del+set теряет atomicity per-op.** Solved by D2 (clone-on-apply on the whole patch).
- **R5 (medium): `convert -i` deletes source on success — irreversible.** Mitigation: `--backup` global flag works on convert -i too (writes `<source>.bak` before delete).

## Migration Plan

M3 — additive milestone. M2 single-file behaviour остаётся bit-identical. Three transitions:

1. **Glob expansion для existing write commands.** `dq set 'k8s/*.yaml' /x 1 -i` previously failed with `IO_ERROR=5` (no such file). M3 expands glob. Это **observable behaviour change**, но (a) M2 spec явно называет это deferred, (b) failure was no-op для users (no file modified), (c) users polluting filenames с `*` нужно `\*` escape — uncommon enough.

2. **`convert -i` — previously rejected (exit 6).** M3 accepts. Same caveat — M2 spec явно говорит deferred to M3.

3. **New subcommands `patch`/`merge`/`diff`.** Previously rejected as unknown subcommand (clap exit 6). Now valid. No regression on existing commands.

Test strategy: M2 golden suite re-runs unchanged. Bulk tests use new fixtures so no overlap.

## Open Questions

- **Q1.** Should `--no-glob` flag exist для disable glob expansion explicitly? Decision: defer to M3+ refinement — current detector handles 99% case correctly, и shell escape работает для остальных. Re-evaluate если bug reports появятся.
- **Q2.** Should `dq diff` output include `op: "test"` ops for unchanged values (как audit trail)? Decision: NO. Test ops are ассert'ы для apply, не diff representation. Включать их раздувает output без benefit.
- **Q3.** RFC 6902 `path` syntax: `/foo/-` для array append. Should `set_at`/`del_at` understand this? Decision: YES. Add `Pointer::is_array_append()` helper; `set_at` resolves `-` to `len` of target array. `del_at` not applicable to `-`.
