Делегирование: `[orch]` — оркестратор пишет markdown / меняет config / прогоняет smoke; `[writer]` / `[test-writer]` — Rust-правки идут через subagents `rust-cli-writer` / `rust-cli-test-writer` (правило в `.claude/rules/rust-delegation.md`). Каждая задача self-contained, ≤ 2 часов реальной работы. Зависимости явно прописаны: §2 зависит от §1 (WriteOptions существует), §3 от §1+§2 (handler использует write_with_options), §4 от §3 (set/del/patch/merge/convert принимают opts), §5 от §3 (validate accepts --check), §6 от всего предшествующего.

## 1. dq-core: WriteOptions + canonicalize_keys

- [x] 1.1 [writer] Создать `crates/dq-core/src/write_options.rs`. Объявить:
  ```rust
  #[derive(Debug, Clone, Default)]
  #[non_exhaustive]
  pub struct WriteOptions {
      pub sort_keys: bool,
      pub indent: Option<u8>,
  }
  
  pub fn canonicalize_keys(value: &Value) -> Value { /* deep recursive sort */ }
  ```
  Add `pub mod write_options;` в `crates/dq-core/src/lib.rs` и re-exports `pub use write_options::{WriteOptions, canonicalize_keys};` из crate root.

- [x] 1.2 [test-writer] В `crates/dq-core/src/write_options.rs` `#[cfg(test)] mod tests`: ≥ 6 unit-тестов:
  - `canonicalize_keys` на `Value::Null` / scalar return как есть.
  - `canonicalize_keys` на `Map { z: 1, a: 2, m: 3 }` → keys `["a", "m", "z"]`.
  - `canonicalize_keys` deep nested `{ z: { y: 1, a: 2 } }` → recursion.
  - `canonicalize_keys` на массиве `[{z: 1, a: 2}, {y: 3, b: 4}]` → элементы canonicalized.
  - Idempotence: `canonicalize_keys(canonicalize_keys(v)) == canonicalize_keys(v)`.
  - `WriteOptions::default()` дает `{ sort_keys: false, indent: None }`.

## 2. dq-core: Format::write_with_options

- [x] 2.1 [writer] В `crates/dq-core/src/format.rs` добавить метод в `trait Format`:
  ```rust
  fn write_with_options(
      &self,
      doc: &Document,
      w: &mut dyn Write,
      opts: &WriteOptions,
  ) -> Result<()> {
      let _ = opts;
      self.write(doc, w)
  }
  ```
  Импортировать `WriteOptions` из crate root.

- [x] 2.2 [writer] В `crates/dq-core/src/parsers/json.rs` override `write_with_options`. Логика: если `opts.sort_keys` → walk через `canonicalize_keys` перед serialization. Если `opts.indent.is_some()` → use `serde_json::ser::PrettyFormatter::with_indent(b" ".repeat(N as usize).as_slice())` вместо стандартного. `indent == Some(0)` → compact (no PrettyFormatter, use Compact).

- [x] 2.3 [writer] В `crates/dq-core/src/parsers/jsonl.rs` override `write_with_options`. Per-line: применить sort_keys + indent те же правила (но JSONL обычно compact one-line; `--indent` редко имеет смысл; documented).

- [x] 2.4 [writer] В `crates/dq-core/src/parsers/yaml.rs` override `write_with_options`. Если `opts.sort_keys` → call `canonicalize_keys`. `opts.indent` → ignore (no-op в M4 per design D6); add `tracing::warn!` only if user explicitly passed Some(N) AND format == Yaml — done in CLI dispatch не здесь, чтобы не зависеть от tracing в core.

- [x] 2.5 [writer] В `crates/dq-core/src/parsers/toml.rs` override `write_with_options`. Если `opts.sort_keys` → use `toml_edit::DocumentMut::sort_values_by` или canonicalize_keys + re-emit. `opts.indent` → ignore.

- [x] 2.6 [test-writer] В `crates/dq-core/tests/write_options.rs`: ≥ 8 интеграционных тестов:
  - JSON sort_keys round-trip: `{"z":1,"a":2}` → `{"a":2,"z":1}`.
  - JSON indent=4: `{"a":1,"b":2}` → 4-space output.
  - JSON indent=0: compact one-line.
  - JSONL sort_keys per-line.
  - YAML sort_keys round-trip.
  - TOML sort_keys round-trip.
  - `WriteOptions::default()` produces byte-identical output to `write` for all 4 formats.
  - Big-int + sort_keys preserves precision.

## 3. dq-cli: fmt command + global flags

- [x] 3.1 [writer] Создать `crates/dq-cli/src/cli/args/fmt.rs`:
  ```rust
  #[derive(Debug, Args)]
  pub struct FmtArgs {
      pub file: Utf8PathBuf,
  }
  ```
  Re-export в `cli/args.rs` (`pub use fmt::FmtArgs;`), регистрировать `Command::Fmt(FmtArgs)`.

- [x] 3.2 [writer] Добавить два global флага в `crates/dq-cli/src/cli/args.rs`:
  ```rust
  /// Sort map keys alphabetically when re-emitting (no-op for textual-edit splice).
  #[arg(long = "sort-keys", global = true)]
  pub sort_keys: bool,
  /// Indentation width for indented formats (json/jsonl honor; yaml/toml ignore).
  #[arg(long = "indent", global = true, value_name = "N")]
  pub indent: Option<u8>,
  ```
  В `Cli::ensure_no_write_flags`: НЕ добавлять `--sort-keys` и `--indent` в offenders list (они read-tolerant). В `Cli` добавить method `pub fn write_options(&self) -> dq_core::WriteOptions { dq_core::WriteOptions { sort_keys: self.sort_keys, indent: self.indent } }`.

- [x] 3.3 [writer] Создать `crates/dq-cli/src/commands/fmt.rs::run(cli, args, input_format, use_color, out)`. Schema:
  1. `cli.ensure_write_flags_consistent()?` — same rules as set/del.
  2. Build `FmtFileOp { args: &FmtArgs, opts: WriteOptions, cli: &Cli }` implementing `crate::bulk::FileOp`. The `apply` method:
     - read source bytes, parse to Document (через `commands::io_helpers::load_document_with_path`).
     - render output via `format.write_with_options(&doc, &mut buf, &opts)`.
     - сравнить bytes: `if buf == source_bytes` → return `FileOpResult::Unchanged`. Else → `FileOpResult::Modified { diff: None, output_bytes: Some(buf) }`.
  3. `let files = bulk::expand_glob(&args.file)?;`.
  4. `bulk::run_per_file(files, &op, cli, out)`.
  
  ВАЖНО: `fmt` использует bulk::run_per_file как любая write-команда — driver сам обрабатывает `-i`/`--diff`/`--check`/`--backup`/`--continue-on-error`/`--parallel`. `--sort-keys`/`--indent` подхватываются из cli.write_options().

- [x] 3.4 [writer] В `crates/dq-cli/src/lib.rs::dispatch`: добавить `Command::Fmt(args) => commands::fmt::run(...)`. В `commands/mod.rs` добавить `pub mod fmt;`.

- [x] 3.5 [test-writer] В `crates/dq-cli/tests/unit_fmt.rs`: ≥ 8 handler-уровневых тестов (через `dq::run` + tempfile):
  - default fmt to stdout produces re-emitted YAML.
  - `-i` writes back atomically.
  - `--check` exits 0 on canonical file.
  - `--check` exits 1 on non-canonical file.
  - `--diff` shows unified diff without writing.
  - `--sort-keys -i` reorders keys in YAML.
  - bulk glob with mixed canonical/non-canonical (3 of 5 non-canonical) → `Modified: 3, Skipped: 2`.
  - source format preserved (YAML→YAML, not JSON).

## 4. dq-cli: thread WriteOptions through existing write commands

- [x] 4.1 [writer] В `crates/dq-cli/src/commands/convert.rs`: signature update — `run` принимает дополнительный параметр `opts: &dq_core::WriteOptions` (или вычисляется внутри из `cli.write_options()`). В `render_to_format` использовать `format.write_with_options(doc, &mut buf, opts)` вместо `format.write`.

- [x] 4.2 [writer] В `crates/dq-cli/src/commands/set.rs`: после успешного `Document::set_at`, при rendering output (`-i` / stdout / `--diff`) — продолжать использовать textual-edit splice результат (Document::original_bytes), `--sort-keys`/`--indent` НЕ применяются (D5). Add `tracing::debug!` при наличии флага: "сохранен byte-order existing keys; --sort-keys is a no-op for textual-edit splice".

- [x] 4.3 [writer] В `crates/dq-cli/src/commands/del.rs`: same as set — splice result, no re-emit.

- [x] 4.4 [writer] В `crates/dq-cli/src/commands/patch.rs` и `merge.rs`: same as set — splice result, no re-emit.

- [x] 4.5 [writer] В `crates/dq-cli/src/lib.rs::dispatch`: build `let opts = cli.write_options();` once, thread to `convert::run` and `fmt::run` parameters.

## 5. dq-cli: validate --check tolerance

- [x] 5.1 [writer] В `crates/dq-cli/src/commands/validate.rs`: relax `cli.ensure_no_write_flags()?` — заменить на explicit per-flag validation, ИЛИ написать новый helper `cli.ensure_no_write_flags_except_check()` который reject'ит все write flags KROME `--check`. Pragmatic: in validate handler, add line `let _ = cli.check; // accepted as no-op`, и просто not include `--check` в `ensure_no_write_flags` rejection. CLEANER: в `Cli::ensure_no_write_flags`, **remove** `--check` from offenders BUT keep rejection of `-i`/`--diff`/`--backup`/`--continue-on-error`/`--parallel`. This affects all read commands — `--check` becomes universally tolerated.
  
  REVISED SOLUTION: keep `Cli::ensure_no_write_flags` as-is (rejects `--check`), but in validate.rs replace `cli.ensure_no_write_flags()?` с custom logic that allows `--check`:
  ```rust
  if cli.in_place || cli.diff || cli.backup || cli.continue_on_error || cli.parallel.is_some() {
      // build offenders list and InvalidInput
  }
  // --check is tolerated for validate
  ```

- [x] 5.2 [test-writer] В `crates/dq-cli/tests/unit_validate.rs`: добавить 2 tests — `validate --check` accepted on valid file (exit 0); `validate --check` on invalid file → exit 4.

## 6. Integration: smoke + snapshots

- [x] 6.1 [test-writer] В `crates/dq-cli/tests/cli_smoke.rs`: добавить 3 smoke сценариев по DoD M4:
  - `dq fmt --sort-keys -i tmpdir/k8s/**/*.yaml` (через tempdir с 5 файлами) — все нормализованы.
  - `dq fmt --check broken-file.yaml` exit 1.
  - `dq convert deploy.yaml -F json --indent 4` — output 4-space.

- [x] 6.2 [test-writer] В `crates/dq-cli/tests/cli_snapshots.rs`: snapshot для `fmt --diff` per-file marker output, `fmt --sort-keys` rendered YAML.

- [x] 6.3 [test-writer] В `crates/dq-cli/tests/cli_write_flags.rs`: добавить теsts что `dq get config.yaml /a --sort-keys` exits 0 (read-tolerant), `dq set f.yaml /x 1 --sort-keys -i` exits 0 (accepted, no-op for splice).

## 7. Pre-commit hooks file

- [x] 7.1 [orch] Создать `.pre-commit-hooks.yaml` в repo root:
  ```yaml
  - id: dq-fmt-check
    name: dq fmt --check
    description: Verify files are canonically formatted (run `dq fmt -i` to fix).
    entry: dq fmt --check
    language: system
    files: '\.(yaml|yml|json|toml|jsonl)$'
  - id: dq-validate
    name: dq validate
    description: Verify files parse without syntax errors.
    entry: dq validate
    language: system
    files: '\.(yaml|yml|json|toml|jsonl)$'
  ```

## 8. Plan delta + meta + verification

- [x] 8.1 [orch] Обновить `dq-plan.md` M4 секцию: пометить implemented after archive. Добавить cross-link на archive folder. Расширить раздел заметкой о deferred flags (`--quote-style`/`--flow-style`/`--strip-comments`) и техническом обосновании (нужен comment-preserving emitter — saphyr-parser scanner discards comment tokens per issue #103).

- [x] 8.2 [orch] Обновить `README.md` status: `M4 alpha — adds dq fmt + --sort-keys + --indent`. Добавить examples секцию: `dq fmt`, `dq fmt --check`, `dq convert -F json --indent 4`.

- [x] 8.3 [orch] `cargo build --workspace --all-targets` зелёный.

- [x] 8.4 [orch] `cargo test --workspace --all-features` — все existing M3 тесты + new M4 тесты зелёные.

- [x] 8.5 [orch] `cargo clippy --workspace --all-targets --all-features -- -D warnings` зелёный.

- [x] 8.6 [orch] `cargo fmt --all -- --check` зелёный.

- [x] 8.7 [orch] Manual smoke по DoD M4:
  - `dq fmt --sort-keys -i k8s/**/*.yaml` нормализует все файлы.
  - `dq fmt --check` ловит ненормализованные файлы (exit 1).
  - Pre-commit интеграция работает (manual в local checkout).
  - `dq validate --check` accepted for symmetry.

- [x] 8.8 [orch] `openspec validate add-style-and-normalization --strict` — `Change is valid`.

- [x] 8.9 [orch] `openspec archive add-style-and-normalization` — после merge в main.
