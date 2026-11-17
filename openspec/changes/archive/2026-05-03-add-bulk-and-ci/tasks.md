Делегирование: `[orch]` — оркестратор пишет markdown / меняет config / прогоняет smoke; `[writer]` / `[test-writer]` — Rust-правки идут через subagents `rust-cli-writer` / `rust-cli-test-writer` (правило в `.claude/rules/rust-delegation.md`). Каждая задача self-contained, ≤ 2 часов реальной работы. Зависимости явно прописаны: §2 зависит от §1 (PatchOp существует), §3 от §1 (driver вызывает apply_patch), §4-§6 от §3 (handlers используют bulk driver), §9 от всего предшествующего.

## 1. dq-core: PatchOp + apply_patch + apply_merge

- [x] 1.1 [writer] Создать `crates/dq-core/src/transform/mod.rs`. Объявить публичные re-exports: `pub use patch::{PatchOp, apply_patch};`, `pub use merge::apply_merge;`, `pub use diff::diff;` (последний — после §2). Добавить `pub mod transform;` в `crates/dq-core/src/lib.rs` и re-exports из crate root: `pub use transform::{PatchOp, apply_patch, apply_merge, diff};`.

- [x] 1.2 [writer] Создать `crates/dq-core/src/transform/patch.rs`. Объявить:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub enum PatchOp {
      Add { path: Pointer, value: Value },
      Remove { path: Pointer },
      Replace { path: Pointer, value: Value },
      Move { from: Pointer, path: Pointer },
      Copy { from: Pointer, path: Pointer },
      Test { path: Pointer, value: Value },
  }
  ```
  Реализовать `Serialize`/`Deserialize` через `serde` так, чтобы JSON-on-the-wire matched RFC 6902 (`{"op":"add","path":"/a/b","value":1}`). Field `op` — discriminant; `path`/`from` сериализуются как RFC 6901 strings (re-use `Pointer::Display`/`Pointer::parse`).

- [x] 1.3 [writer] Реализовать `pub fn apply_patch(doc: &mut Document, ops: &[PatchOp]) -> Result<()>`. Семантика D1+D2:
  1. `let mut working = doc.clone();` — clone-on-apply.
  2. Для каждого op:
     - `Add { path, value }` → `working.set_at(&path, value.clone())?` (mkdir-p уже работает в M2).
     - `Remove { path }` → `working.del_at(&path)?`.
     - `Replace { path, value }` → проверить `working.span_at(&path).is_some()` → `set_at(&path, value.clone())?`. Если span отсутствует → `Error::Path { kind: MissingKey }`.
     - `Move { from, path }` → `let v = read_value_at(&working, &from)?; working.del_at(&from)?; working.set_at(&path, v)?;` (read_value_at — приватный helper, walks the Value tree using Pointer segments).
     - `Copy { from, path }` → как Move, но без `del_at`.
     - `Test { path, value }` → `let actual = read_value_at(&working, &path)?; if actual != *value { return Err(Error::PatchTestFailed { pointer: path.as_canonical(), expected: value.clone(), actual }); }`.
  3. На любую Err — return Err(...) **без** замены `*doc`. На success — `*doc = working;`.

  Файл также содержит приватный `fn read_value_at(doc: &Document, ptr: &Pointer) -> Result<Value>` — копирует из `doc.value()` walking by segments. Не использовать `Document::set_at` для чтения.

- [x] 1.4 [writer] В `crates/dq-core/src/error.rs` добавить вариант:
  ```rust
  #[error("RFC 6902 test op failed at {pointer}: expected {expected}, got {actual}")]
  PatchTestFailed { pointer: String, expected: Value, actual: Value }
  ```
  С kind_name `"patch_test_failed"`. Variant требует `Value: Display` (он уже есть в `document.rs`).

- [x] 1.5 [writer] В `crates/dq-core/src/pointer.rs`: добавить `pub fn is_array_append(&self) -> bool` (last segment == "-"). Дополнить `Document::set_at` для случая `Segment::Key("-")` или `is_array_append`: при resolve в array, использовать `array.len()` как append index. `Document::del_at` на `/-` → `Error::PointerInvalid` или вариант (новый? оценить — лучше InvalidInput marker через handler, не domain Error).

- [x] 1.6 [writer] Создать `crates/dq-core/src/transform/merge.rs`. Реализовать `pub fn apply_merge(doc: &mut Document, patch: &Value) -> Result<()>`. Семантика D3:
  1. `let mut working = doc.clone();`.
  2. `merge_into(&mut working, &Pointer::default(), patch)?;` где приватный recursive walker:
     - patch не Map → `working.set_at(&base, patch.clone())?`
     - patch — Map → for each (k, v): build `child_path = base.with_segment(Key(k))`. If `v == Null` → `working.del_at(&child_path).ok();` (silent NOP if missing per RFC 7396). Else if existing target is Map AND v is Map → recurse. Else → `working.set_at(&child_path, v.clone())?`.
  3. `*doc = working;`.

- [x] 1.7 [test-writer] В `crates/dq-core/tests/transform_patch.rs`: ≥ 12 тестов покрывающих все 6 RFC 6902 ops, mixed sequences, atomic rollback на test failure, мове из nested map в array tail (`/-`), copy preserves source, malformed pointer.

- [x] 1.8 [test-writer] В `crates/dq-core/tests/transform_merge.rs`: ≥ 8 тестов: scalar replace, recursive map merge, null removal, null on missing key (silent NOP), array replacement (full not element-wise), null nested removes whole subtree.

## 2. dq-core: structural diff

- [x] 2.1 [writer] Создать `crates/dq-core/src/transform/diff.rs`. Реализовать `pub fn diff(a: &Value, b: &Value) -> Vec<PatchOp>`. Алгоритм D4:
  - Внутренний рекурсивный helper `diff_at(path: &Pointer, a: &Value, b: &Value, ops: &mut Vec<PatchOp>)`.
  - Type mismatch / both scalars not equal → push `Replace { path: path.clone(), value: b.clone() }`.
  - Both Map: keys в a − b → Remove; keys в b − a → Add; common keys → recurse with `path.append(Key(k))`.
  - Both Array: aligned indices recurse. Длина a > b → Remove for tail (in reverse index order!) — иначе indices сдвигаются. Длина a < b → Add для tail с numeric path (NOT `/-` — diff produces concrete indices).
  - Nullable: null vs Map / null vs scalar — treat as type mismatch → Replace.

- [x] 2.2 [test-writer] В `crates/dq-core/tests/transform_diff.rs`: ≥ 10 тестов:
  - Equal docs → empty.
  - Single scalar change → 1 op replace.
  - Type change → 1 op replace at root.
  - Map key removal + addition (one each) → 2 ops.
  - Nested change → 1 op deep.
  - Array element replace → 1 op replace at index.
  - Array length increase → Add ops at tail.
  - Array length decrease → Remove ops in reverse order.
  - Round-trip property: `apply_patch(&diff(a,b), Document::value_only(a, ...)).value() ==semantic== b` for ≥ 5 hand-picked pairs.

- [x] 2.3 [test-writer] В `crates/dq-core/tests/transform_diff.rs` добавить proptest (≥ 100 cases): `random a, b: Value` (через свою стратегию для small Map/Array of scalars), property: `apply_patch(diff(a,b), &mut a_clone) == Ok(()) && a_clone.value() ==semantic== b`. Use `proptest_strategies` модуль уже есть в M2 round_trip_property — переиспользовать.

## 3. dq-cli: bulk driver

- [x] 3.1 [writer] Обновить `crates/dq-cli/Cargo.toml`: добавить `globset = "0.4"`, `rayon = "1.10"`, `walkdir = "2"` в `[dependencies]`. Workspace pin в `Cargo.toml` корне (rename to mention "M3 §3 bulk driver" в comment).

- [x] 3.2 [writer] Создать `crates/dq-cli/src/bulk.rs`. Объявить:
  ```rust
  pub trait FileOp: Sync {
      fn apply(&self, path: &Utf8Path) -> anyhow::Result<FileOpResult>;
  }
  pub enum FileOpResult { Modified { diff: Option<String>, output_bytes: Option<Vec<u8>> }, Unchanged, Skipped(String) }
  pub fn run_per_file(files: Vec<Utf8PathBuf>, op: &dyn FileOp, cli: &Cli, out: &mut dyn Write) -> anyhow::Result<()>;
  pub fn expand_glob(pattern: &Utf8Path) -> anyhow::Result<Vec<Utf8PathBuf>>;
  ```
  `expand_glob` правила D5: detection meta-chars `* ? [ {` через `pat.as_str().chars().any(|c| matches!(c, '*'|'?'|'['|'{'))`; на match — split into longest non-meta prefix + glob suffix; walk через `walkdir::WalkDir::new(prefix).into_iter().filter(globset::GlobMatcher::is_match)`.
  
  `run_per_file` правила D6+D7+D8:
  - Single file (`files.len() == 1`) → call `op.apply` directly, no summary line, return error verbatim.
  - Bulk: `let parallel = cli.parallel.unwrap_or(1);`. Если `parallel == 0` → `current_num_threads`. Если parallel == 1 → sequential `for file in files`. Иначе `rayon::ThreadPoolBuilder::new().num_threads(parallel).build()?` + `pool.install(|| files.par_iter().map(...).collect())`.
  - Per-file outputs буфферизуются в `Vec<u8>`. После join — print in `files` order to `out`.
  - `--check` short-circuit: на `--check` flag, никогда не зовём `atomic_write`. Вместо apply, op.apply возвращает FileOpResult::Modified с output_bytes; bulk driver сравнивает с source bytes; если разные → накопить в "would change" list. После всего: print list, return Ok(()) с exit code 1 через `SilentError` или новый marker. Decision: introduce `crate::error::CheckPending` newtype, exit_code mapper returns 1 (GENERIC).
  - `--continue-on-error` short-circuit: errors не bubble; собираются в `Vec<(path, anyhow::Error)>`. После всего: print summary + per-file errors на stderr; вернуть `Err(WRITE_FAILED marker)` если хоть одна.

- [x] 3.3 [writer] В `crates/dq-cli/src/cli/args.rs` добавить три global флага:
  ```rust
  /// In bulk mode, do not abort on first file failure.
  #[arg(long = "continue-on-error", global = true)]
  pub continue_on_error: bool,
  /// Fan out across N threads (0 = num CPUs).
  #[arg(long = "parallel", global = true, value_name = "N")]
  pub parallel: Option<usize>,
  /// Idempotency gate: exit 1 if any file would be modified, do not write.
  #[arg(long = "check", global = true)]
  pub check: bool,
  ```
  Расширить `ensure_no_write_flags` чтобы reject'ил `--check`, `--continue-on-error`, `--parallel` (если N>1) для read commands. Расширить `ensure_write_flags_consistent`: `--check` ⊥ `-i`, `--check` ⊥ `--diff`, `--check` ⊥ `--backup`, `--parallel` > 1 без glob — InvalidInput "no parallel work for single file".

- [x] 3.4 [writer] В `crates/dq-cli/src/error.rs` добавить:
  ```rust
  /// Marker for `--check` mode reporting changes pending. Maps to exit 1.
  #[derive(Debug, thiserror::Error)]
  #[error("{count} file(s) would be modified")]
  pub struct CheckPending { pub count: usize }
  ```
  В `crates/dq-cli/src/exit_code.rs::exit_code_for_error` добавить branch `if err.downcast_ref::<CheckPending>().is_some() { return GENERIC; }` (constant 1).

- [x] 3.5 [test-writer] В `crates/dq-cli/src/bulk.rs` добавить `#[cfg(test)] mod tests` с ≥ 8 unit-тестами:
  - `expand_glob` literal path → vec![path.clone()] (no FS access).
  - `expand_glob` `'**/*.yaml'` в tempdir с YAML/JSON/TXT файлами → только YAML.
  - `expand_glob` нет matches → `Err`.
  - run_per_file single → no summary в output.
  - run_per_file bulk сequential → summary appears, files in order.
  - run_per_file --check + identical input → exit code via CheckPending.count == 0 → return Ok.
  - run_per_file --check + different → CheckPending.count > 0.
  - run_per_file --continue-on-error + один failing → returns WRITE_FAILED-mapped error, summary list.

## 4. dq-cli: `patch` command

- [x] 4.1 [writer] Создать `crates/dq-cli/src/cli/args/patch.rs`:
  ```rust
  #[derive(Debug, Args)]
  pub struct PatchArgs {
      pub file: Utf8PathBuf,
      pub ops: Option<String>, // inline JSON / "-" / "@<path>"
      #[arg(long = "ops-from", value_parser = clap::value_parser!(Utf8PathBuf))]
      pub ops_from: Option<Utf8PathBuf>,
      #[arg(long = "line-format")]
      pub line_format: bool,
      #[arg(long = "no-create")]
      pub no_create: bool,
  }
  ```
  Re-export в `cli/args.rs` (`pub use patch::PatchArgs;`), регистрировать в `Command::Patch(PatchArgs)`.

- [x] 4.2 [writer] Создать `crates/dq-cli/src/commands/patch.rs::run(cli, args, input_format, use_color, out)`. Schema:
  1. `cli.ensure_write_flags_consistent()?`.
  2. Resolve ops source (mirror `set::resolve_value`): inline / stdin / `@` / `--ops-from`. Output: `Vec<u8>`.
  3. Parse ops. If `args.line_format` → split lines, parse each as `op pointer [json-value]` → build `Vec<PatchOp>`. Else → `serde_json::from_slice::<Vec<PatchOp>>(&bytes)`.
  4. Build `FileOp` impl that captures ops + cli flags. Method:
     - read source bytes, parse to Document via `parse_to_document` (re-use set's helper or extract to io_helpers).
     - `apply_patch(&mut doc, &ops)?`.
     - return FileOpResult::Modified with output_bytes = doc.original_bytes().to_vec().
  5. Resolve target files: `bulk::expand_glob(&args.file)?` → `bulk::run_per_file(files, &op, cli, out)`.

- [x] 4.3 [writer] В `crates/dq-cli/src/lib.rs::dispatch`: добавить `Command::Patch(args) => commands::patch::run(...)`.

- [x] 4.4 [test-writer] В `crates/dq-cli/tests/unit_patch.rs`: ≥ 6 handler-уровневых тестов (через `dq::run` + tempfile):
  - inline JSON ops, single-file → стандартный stdout output.
  - stdin ops (`-`) — defer integration test if stdin tricky; skip with TODO.
  - `--line-format` + multi-line stdin/file → applied.
  - test op failure → exit 1, файл unchanged on disk.
  - Bulk glob с одинаковыми ops на 3 YAML files.
  - `--check` + ops который изменил бы файл → exit 1 (CheckPending), nothing on disk.

## 5. dq-cli: `merge` command

- [x] 5.1 [writer] Создать `crates/dq-cli/src/cli/args/merge.rs`:
  ```rust
  #[derive(Debug, Args)]
  pub struct MergeArgs {
      pub file: Utf8PathBuf,
      pub patch: Option<String>,
      #[arg(long = "patch-from", value_parser = clap::value_parser!(Utf8PathBuf))]
      pub patch_from: Option<Utf8PathBuf>,
  }
  ```
  Re-export, register `Command::Merge(MergeArgs)`.

- [x] 5.2 [writer] Создать `crates/dq-cli/src/commands/merge.rs::run`. Идентично `patch::run`, но: ops parsed как single `Value` (not Vec<PatchOp>), call `apply_merge`. Source: inline JSON / stdin / `@` / `--patch-from`.

- [x] 5.3 [writer] Зарегистрировать в dispatch.

- [x] 5.4 [test-writer] В `crates/dq-cli/tests/unit_merge.rs`: ≥ 5 tests (recursive merge, null removes, array replace, multi-file glob, --check).

## 6. dq-cli: `diff` command

- [x] 6.1 [writer] Создать `crates/dq-cli/src/cli/args/diff.rs`:
  ```rust
  #[derive(Debug, Args)]
  pub struct DiffArgs {
      pub a: Utf8PathBuf,
      pub b: Utf8PathBuf,
      #[arg(long = "unified")]
      pub unified: bool,
  }
  ```
  Re-export. **Конфликт имён:** `Command::Diff` vs global flag `--diff`. Clap-wise это OK (subcommand vs flag — disambiguation explicit), но добавить тест что `dq diff a.yaml b.yaml` парсится как Subcommand и `dq set f.yaml /x 1 --diff` парсится как flag.

- [x] 6.2 [writer] Создать `crates/dq-cli/src/commands/diff.rs::run`. Schema:
  1. ensure_no_write_flags (diff is read-only — checks `-i`/`--backup`/`--check`/`--continue-on-error`/`--parallel` rejected; `cli.diff` flag is irrelevant for `diff` subcommand).
  2. Load both files via `load_document_with_path`. Tag-mismatch допускается (YAML vs JSON file).
  3. If `args.unified` → render обе документа в JSON (stable ordering) через `JsonReporter`, call `crate::diff::render_unified`, write на stdout.
  4. Else → `let ops = dq_core::diff(&a.value(), &b.value());` → serialize ops через `serde_json::to_string_pretty(&ops)?` для `-F json`, через reporter для других forms (re-use ConvertHandler logic — это fine).

- [x] 6.3 [writer] Зарегистрировать в dispatch.

- [x] 6.4 [test-writer] В `crates/dq-cli/tests/unit_diff.rs`: ≥ 6 tests (equal files → empty array, single replace, type change → root replace, --unified mode, cross-format YAML vs JSON).

## 7. dq-cli: bulk-mode integration in set/del

- [x] 7.1 [writer] Refactor `crates/dq-cli/src/commands/set.rs`: Extract existing logic into `struct SetFileOp<'a> { args: &'a SetArgs, cli: &'a Cli }; impl FileOp for SetFileOp { ... }`. `run` теперь:
  1. ensure_write_flags_consistent (extended).
  2. resolve_value (один раз — value одинаков для всех files).
  3. `let files = bulk::expand_glob(&args.file)?;`.
  4. Если `files.len() > 1` (bulk) — все переменные mut closures должны быть captured by ref.
  5. `bulk::run_per_file(files, &op, cli, out)`.

  Single-file path должен оставаться bit-identical (golden tests M2 продолжают проходить).

- [x] 7.2 [writer] Идентичный refactor для `crates/dq-cli/src/commands/del.rs`.

- [x] 7.3 [test-writer] В `crates/dq-cli/tests/cli_bulk.rs`: ≥ 8 integration тестов через `dq::run`:
  - bulk set on 5 yaml files with same pointer/value.
  - bulk set with `--continue-on-error` and 1 templated → exit 7, summary `Failed: 1`.
  - bulk del on 3 files.
  - `--check` happy path (no changes pending) on 3 already-modified files → exit 0.
  - `--check` mixed (2 need changes, 3 don't) → exit 1, list 2.
  - `--parallel 4` smoke (10 files, all succeed).
  - Glob no matches → exit 5.
  - Bulk `--diff` mode prints per-file diff with `=== <path> ===` markers.

## 8. dq-cli: `convert -i`

- [x] 8.1 [writer] В `crates/dq-cli/src/cli/args/convert.rs`: добавить `#[arg(long = "keep-source")] pub keep_source: bool`.

- [x] 8.2 [writer] В `crates/dq-cli/src/commands/convert.rs`: добавить `-i` branch. Логика:
  1. Если `cli.in_place` → require `cli.format != Console` (output format must be specified for in-place).
  2. Compute target path = source path with extension swapped to `cli.format`'s canonical extension. Если target == source → InvalidInput "convert -i to same format is no-op".
  3. Render output bytes.
  4. `atomic_write::write(&target, &bytes, cli.backup)?`.
  5. If `!args.keep_source && target != source` → `fs::remove_file(source).map_err(|e| Error::WriteIo { path: source, source: e })?`.
  6. Bulk: integrate via `bulk::run_per_file` so `dq convert 'manifests/*.yaml' -i -F json` работает.

- [x] 8.3 [writer] В `crates/dq-cli/src/cli/args.rs::ensure_write_flags_consistent`: разрешить `cli.in_place && cli.format != Console` ТОЛЬКО для `Command::Convert`. Т.к. это handler-specific check, перенести в `Command::Convert` handler первой строкой через explicit override flag в args (e.g. `args._allows_format_with_in_place = true`) — или duplicate validation in convert handler. Pragmatic: оставить global rejection и сделать convert::run обходить ensure_write_flags_consistent через прямой вызов individual checks (`if cli.in_place && cli.diff { return Err(InvalidInput...) }` minus the `-i + -F` rejection).

- [x] 8.4 [test-writer] В `crates/dq-cli/tests/unit_convert.rs` (existing): добавить ≥ 5 -i tests:
  - `convert deploy.yaml -i -F json` → deploy.json existences, deploy.yaml removed.
  - `convert deploy.yaml -i -F json --keep-source` → both exist.
  - `convert deploy.yaml -i -F yaml` → InvalidInput.
  - bulk `convert 'tmpdir/*.yaml' -i -F json` → all converted.
  - convert with `--backup` + `-i` → `.bak` of original alongside new file.

## 9. Integration: golden runner + snapshots + smoke

- [x] 9.1 [test-writer] В `crates/dq-cli/tests/cli_patch_merge_diff.rs` создать insta snapshots для:
  - patch happy path.
  - patch test failure error message.
  - merge with null removal.
  - diff JSON output (3 fixtures).
  - diff --unified output.

- [x] 9.2 [test-writer] В `crates/dq-cli/tests/golden.rs` (existing): расширить runner для bulk fixtures. Создать `crates/dq-cli/tests/fixtures/bulk/` с примером — 3 yaml файла, ожидаемый summary.

- [x] 9.3 [test-writer] В `crates/dq-cli/tests/cli_smoke.rs`: добавить +5 smoke сценариев по DoD M3:
  - `dq set 'tmpdir/k8s/**/*.yaml' /spec/replicas 3 -i` (через tempdir с 5 файлами).
  - `dq diff a.yaml b.yaml -F json`.
  - `dq patch deploy.yaml @ops.json -i`.
  - `dq merge deploy.yaml @patch.json -i`.
  - `dq convert deploy.yaml -i -F json`.

- [x] 9.4 [test-writer] В `crates/dq-cli/tests/cli_snapshots.rs`: snapshot для new error variant `PatchTestFailed` (console + JSON rendering).

## 10. Plan delta + meta + verification

- [x] 10.1 [orch] Обновить `dq-plan.md` M3 секцию: пометить implemented after archive. Добавить cross-link `2026-MM-DD-add-bulk-and-ci`.

- [x] 10.2 [orch] Обновить `dq-plan.md` Tech stack: `globset`, `rayon`, `walkdir` пришли в M3 (раньше упоминались как "M3 deps").

- [x] 10.3 [orch] Обновить `README.md` status: `M3 alpha — read + write + bulk + CI`. Добавить examples секцию: `dq patch ...`, `dq merge ...`, `dq diff ...`, glob example.

- [x] 10.4 [orch] `cargo build --workspace --all-targets` зелёный.

- [x] 10.5 [orch] `cargo test --workspace --all-features` — все existing 438 тестов M2 + new M3 тесты зелёные.

- [x] 10.6 [orch] `cargo clippy --workspace --all-targets --all-features -- -D warnings` зелёный.

- [x] 10.7 [orch] `cargo fmt --all -- --check` зелёный.

- [x] 10.8 [orch] `cargo deny check` зелёный.

- [x] 10.9 [orch] Manual smoke по DoD M3:
  - `dq set 'k8s/**/*.yaml' /spec/replicas 3 -i` обновляет 50 файлов одной командой, summary `Modified: 47, Skipped: 3 (already up to date)`.
  - `dq diff prod-values.yaml staging-values.yaml -F json` — читаемый JSON Patch.
  - Round-trip: `dq diff a.yaml b.yaml -F json | dq patch a.yaml -` produces b.yaml.
  - `dq patch deploy.yaml @ops.json -i` с RFC 6902 примера.
  - `dq merge deploy.yaml @patch.json -i` с RFC 7396 примера.

- [x] 10.10 [orch] `openspec validate add-bulk-and-ci --strict` — `Change is valid`.

- [x] 10.11 [orch] `openspec archive add-bulk-and-ci` — после merge в main.
