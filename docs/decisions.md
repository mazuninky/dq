# Decision log

This file records why dq looks the way it does: one section per shipped change, oldest first, each with what the change did, the decisions it made and the reasons for them (including alternatives that were turned down), and what it deliberately left out. It is history, not a contract. Current behaviour is documented under [reference/](reference/), open work lives in [roadmap.md](roadmap.md), and the WASM plugin contract is in [plugin-abi.md](plugin-abi.md). Where a decision has since been changed by the code, the section says so in a line starting with "Since then".

Dates are the day a change was archived. Milestone numbers (M1…M12) come from the roadmap in the original plan, [archive/dq-plan-2026-05.md](archive/dq-plan-2026-05.md). Several changes were archived on the same day; within a day they are listed in the order they were built. Archive dates lag behind implementation for some of them: M6 (distribution) and M8 (lint engine) were built on 2026-05-04 and M10 (autofix) on 2026-05-05, but all three were archived on 2026-05-06, so the M9 section below refers to the lint engine before its own section appears.

## 2026-05-03 — Read-only foundation (init-data-query-foundation)

Milestone M1. The repository held only the plan; this change set up the whole Cargo workspace and made dq a working read-only tool for YAML, JSON and TOML.

**What it did.** Created five crates at once: `dq-core`, `dq-transform`, `dq-exec`, `dq-lint`, `dq-cli`. Only `dq-core` (document model, `Format` trait, parsers, JSON Pointer, errors) and `dq-cli` (binary, commands, reporters) got real code; the other three were placeholders. Commands: `get`, `exists`, `keys`, `values`, `len`, `type`, `paths`, `select` (JSONPath), `convert` (stdout only), `validate`, plus a hidden `generate-docs` for man pages and completions. Formats: YAML, JSON, TOML and JSONL read; JSONL and TOON write. Parse errors carry line, column and a snippet; path errors carry the matched prefix and did-you-mean suggestions. See [reference/read-commands.md](reference/read-commands.md), [reference/paths.md](reference/paths.md), [reference/cli.md](reference/cli.md).

The change also fixed a set of engineering conventions in the code and in the plan before anything else was written: `camino` UTF-8 paths, `tracing` instead of printing from libraries, SIGPIPE handling, output through an injected `Reporter`, exit codes as named constants, `-v`/`-q`, a size-optimised release profile, and `thiserror` + `anyhow` instead of `miette` as the error type.

**Decisions and reasons**

- **The full workspace up front, filled in milestone by milestone.** Adding members later re-resolves `Cargo.lock` and forces structural churn; five manifests are a cheap price for catching dependency breakage on every change instead of all at once.
- **YAML read through `serde_yml`, not `saphyr`.** The plan separates reading (M1) from round-trip writing (M2). Learning the saphyr event API first would have spent the riskiest two weeks before the rest of the skeleton worked. The YAML parser sits behind the `Format` trait so the backend can change without touching callers.
  Since then: the read path uses `serde_norway`, with `saphyr-parser` building byte spans; numbers past i64/f64 are kept exactly as `BigInt`/`BigFloat` literals instead of being rounded or turned into strings.
- **Own JSON Pointer (RFC 6901) instead of the `jsonptr` crate.** The format is about 50 lines of code, and `resolve` has to return a rich error (`matched_prefix`, `did_you_mean` by Levenshtein distance ≤ 2, at most three candidates); an external crate would return `Option` and hide that context.
- **`jsonpath-rust` 0.7 for `select`, not an own JSONPath.** RFC 9535 is large. Converting to `serde_json::Value` for the query loses positions, which `select` does not need. `serde_json_path` was the alternative to re-check at implementation time.
  Since then: `jsonpath-rust` 1.0, a strict RFC 9535 evaluator; the 0.7 extensions (`.length()`, `.['x']`, `=~`) are rejected.
- **Errors: `thiserror` enums per library crate, `anyhow` only in CLI handlers.** The plan had `miette` as the primary error type; it is a diagnostic renderer, and making it the error type would force every library consumer of `dq-core` to depend on it. Caret rendering could be layered on later in `dq-cli` only. `miette` was never added.
- **TOON output through the `toon-format` crate, not an own encoder.** The TOON spec was still moving; tracking it by hand was not worth it. Since then: `toon-format` 0.7 (TOON spec v4).
- **`Reporter` trait with the factory in the wiring layer.** Handlers receive `&dyn Reporter` and a writer, so they are tested against a `Vec<u8>` without spawning the binary. A factory inside the output module would have made handlers build their own reporters. Only `main.rs` may call `process::exit`, and `main.rs` stays thin: restore SIGPIPE, parse args, init tracing, dispatch.
- **Arguments split per command, global flags only on the top-level `Cli`** (`-F`, `-v`, `-q`, `--no-color`, `--no-pager`, `--doc`).
  Since then (2026-11-20): `-F` is the output format only, a global `--input-format` names the input (and stdin needs it), and `--no-pager` is a hidden no-op.
- **`Value` is a plain recursive enum with `IndexMap` maps** and `BigInt(String)` / `BigFloat(String)` variants from day one. `BTreeMap` would sort keys (bad for round-trip and for agents expecting source order), `HashMap` would lose order. Keeping big numbers as text was the hedge against M2 needing exact numbers.
- **Write flags declared but refused.** `-i`, `--diff`, `--backup` existed in M1 and returned an error, so their parsing was tested before M2 gave them behaviour.
- **SIGPIPE reset to default on Unix**, so `dq paths big.yaml | head` ends quietly instead of panicking. **`tracing` initialised with `try_init`**, because integration tests call the entry point more than once per process.
- **Exit codes as constants:** 0 success, 1 generic (also `exists` answering "no", via a silent error that prints nothing), 2 not found, 3 parse error, 4 validate fail, 5 I/O, 6 invalid input. See the exit-code table in [reference/cli.md](reference/cli.md).
  Since then: clap usage errors (unknown flag, conflicting flags, bad value) exit 2, clap's own code, not 6 as the M1–M5 specs say. Flag checks done in handlers (`-i` with `--diff`, `--backup` without `-i`, write flags on read commands) are invalid input, exit 6.
- **`--doc` selects one document of a multi-document YAML file, default 0.** On a single-document file the flag was ignored.
  Since then: an index past the last document is exit 2, `--doc all` wraps a single document in an array, and commands that cannot honour `--doc` refuse it with a usage error (exit 2). See "Multi-document YAML streams" in the [README](../README.md#multi-document-yaml-streams).
- **Colour precedence** `--no-color` > `NO_COLOR` > `CLICOLOR_FORCE` > TTY detection, tested through the child process environment rather than by mutating the test process's own environment.
- **Test pyramid** of roughly 75 % unit (handlers against in-memory writers), 20 % component, 5 % CLI integration; `insta` snapshots for error rendering; a `proptest` round-trip from every enumerated pointer back to its node; a golden runner over 50+ real-world k8s, Helm and GitHub Actions files. No test-only `pub(crate)` escape hatches in production code.
- **Toolchain and gates:** `rust-toolchain.toml` pin (1.94 then; 1.100 now), release profile `lto = true`, `codegen-units = 1`, `strip = true`, `cargo-deny` licence allow-list, and pre-commit hooks running fmt, clippy with `-D warnings` and the test suite.

**Deferred or left out.** All writes (M2), jq (M7), linting (M8), globs and multi-file runs (M3), CI, install scripts and self-update (M6), every format beyond YAML/JSON/TOML/JSONL/TOON (M5). Open questions at the time: the crates.io name (moot today: workspace crates are `publish = false`, and dq ships through GitHub Releases, Homebrew, GHCR and `cargo install --git`); `validate` on an empty file treated as a valid empty document; streaming `paths` for documents over ~10 MB, left as a simple in-memory walk.

## 2026-05-03 — Safe writes (add-safe-writes)

Milestone M2, called the riskiest block in the plan: everything after it depends on edits that keep comments, blank lines, key order, quoting, anchors, indentation and exact numbers.

**What it did.** Added `set` and `del` for YAML, JSON and TOML, and turned on `-i` (atomic in-place write), `--diff` (unified diff, no write) and `--backup`. Without `-i` or `--diff` the result goes to stdout. Added the Helm/Go-template guard and exit code 7 (`WRITE_FAILED`). See [reference/write-commands.md](reference/write-commands.md), [reference/template-guard.md](reference/template-guard.md), [reference/ir-and-edit-ops.md](reference/ir-and-edit-ops.md).

**Decisions and reasons**

- **Textual edit, not a re-emitting YAML writer.** An earlier spike ([spikes/saphyr/RESULTS.md](../spikes/saphyr/RESULTS.md)) showed that no pure-Rust YAML crate keeps comments: the saphyr-parser scanner discards comment tokens before the event stream (saphyr issue #103, no roadmap), so an emitter cannot restore what it never sees. dq therefore uses `saphyr-parser` only for structure: it walks events and builds a map from canonical pointer to byte range, keeps the original bytes untouched, and `set_at` splices new bytes into one range. Comments survive because the bytes around them are never rewritten. This is how `toml_edit` already works for TOML. Rejected: a custom scanner, parser and emitter (about six months); `libfyaml` over FFI, the only parser that keeps comment tokens (C dependency, breaks the single static binary, bigger CI matrix); `yaml-rust2` (same scanner loss); shipping without round-trip (loses the main differentiator).
- **A spike with success criteria fixed in advance, as a gate.** On five fixtures (k8s manifest, Helm values, anchors, multi-document, Hugo front matter): every scalar gets a span; a one-scalar `set` changes exactly one line of the diff; inserting a missing key produces valid YAML; parse plus span build of a 1 MB manifest ≤ 100 ms and span recompute ≤ 5 ms; at most two weeks. Failure would have meant falling back to libfyaml or to no preservation. The spike passed.
- **Two YAML paths.** Reads kept the `serde_yml` value parser; only writes used the span builder. Unifying them was left for later. Since then: reads go through `serde_norway` with `saphyr-parser` spans (see the M1 section).
- **TOML through `toml_edit`**, the crate `cargo` uses to edit `Cargo.toml`: it already does textual edits with spans. The `toml` crate was kept for whole-document writes (conversion, `fmt`).
- **JSON through an own wrapper over `serde_json`** (`preserve_order`, `arbitrary_precision`), recording the indent style from the first lines and the trailing newline. JSONC comments are a parse error saying so, not silently accepted; adding `jsonc-parser` for an unsupported dialect was rejected.
- **Document = value + original bytes + span map.** Each span records the value range, the whole logical line (for deletes), the indent and the context (block or flow, map or sequence), so a replacement is rendered in the right style. Rejected: metadata embedded in `Value` or an `Annotated<T>` wrapper (would break every M1 match on `Value`), a parallel metadata map (there is no comment data to put in it). Costs accepted: an O(n) offset shift after each edit and the whole file in memory.
- **Inserted keys use a simple per-format emitter**: YAML block style at parent indent + 2, bare scalars where safe, double quotes otherwise; TOML through `toml_edit`; JSON with the file's indent. New content does not look hand-written; running `dq fmt` afterwards is the documented polish. Refusing insertion was rejected because "agent adds a label to a manifest" is the main use case. A test parses every insertion result to catch invalid output.
- **Atomic write:** a temp file in the same directory, then `persist` (rename). Same directory because rename across filesystems is not atomic (`EXDEV`). The `atomicwrites` crate was rejected because `tempfile` already does this. `--backup` writes `<file>.bak` next to the file; an early version replaced the extension (`deploy.bak`) and was fixed to append during verification.
- **Template guard before parsing.** A regex finds `{{ … }}` in structural positions; by default `set`/`del` fail with a templated-file error (exit 3) naming two escape hatches: `--allow-templates` (parse anyway, best effort) and `--raw-template-strings` (swap each template expression for an opaque placeholder, restore it on write). Rejected: no guard (cryptic parse errors), error with no escape hatch (shuts out Helm users), a silent fallback (breaks agents), understanding Go templates (a separate project).
  Since then: the guard runs only in `set`, `del`, `patch` and `merge`, not in `get`, `paths`, `convert` or `validate` as the spec said.
- **`--diff` through the `similar` crate**, over `diffy` (smaller API) and an own Myers diff (~500 lines). Since then: `similar` 3.
- **Exit 7 for write failures**, kept apart from read I/O (exit 5) by wrapping write-side errors in their own type. A templated file maps to 3 (parse).
- **`set` takes its value inline (parsed as JSON if it looks like a JSON literal), from stdin (`-`), from a file (`@path` or `--value-from`)**, following `curl`/`jq` conventions; `--value-string` forces a string; `1.0` is a float, as in jq. A `--value-format` override for stdin was discussed and not added.
- **`set` creates missing parents, `del` of a missing path is exit 2.** Symmetric failure would break adding new keys; a silent `del` would hide whether the key existed. `--no-create` makes `set` exit 2 instead.
  Since then: missing intermediate keys are created as mappings, never arrays. (On YAML this only started working with unreleased fixes on the current branch; before them `set f.yaml /b/c 2` with no `b` failed with exit 2.)
- **`-i` cannot be combined with `-F` on `set`/`del`**: the file extension decides the written format; `-F` only changes stdout output.
- **`set`/`del` live on `Document` in `dq-core`; `dq-transform` stays empty.** An `Op` abstraction for two operations would be premature.

Two bugs found during verification shaped the code: `saphyr_parser::Marker::index()` counts characters, not bytes, so spans were misaligned in files with multi-byte UTF-8 until a char-to-byte map was added; and the backup-name fix above.

**Deferred or left out.** Globs and bulk runs, `patch`, `merge`, `diff`, `convert -i` (all M3); formatting flags (M4); new formats (M5); jq transforms (M7). Formatting of `--allow-templates` output is not guaranteed. The Windows atomic-write smoke test (rename over a file another process holds open) was deferred to M6 behind `#[ignore]`; it never landed, and Windows is not a supported platform today.

## 2026-05-03 — Bulk edits and CI gates (add-bulk-and-ci)

Milestone M3: one command over many files, and many operations on one file.

**What it did.** Added `patch` (RFC 6902 JSON Patch, ops inline, from `@file`, `--ops-from` or stdin, plus a one-op-per-line `--line-format`), `merge` (RFC 7396 Merge Patch) and `diff A B` (a JSON Patch document by default, `--unified` for a text diff). Every write command (`set`, `del`, `patch`, `merge`, `convert`) accepts a glob as its file argument, with `--continue-on-error`, `--parallel N` and `--check`, and prints `Modified: N, Skipped: M, Failed: K` in bulk mode. `convert -i` became possible, with `--keep-source`. See [reference/bulk.md](reference/bulk.md) and [reference/write-commands.md](reference/write-commands.md).

**Decisions and reasons**

- **RFC 6902 and RFC 7396, not a new dialect.** They are what JSON Patch admission webhooks, CloudFormation transforms and agent tooling already speak.
- **Patch and merge are built on `set_at`/`del_at`**, so they keep the M2 textual-edit guarantees for free. The `json-patch` crate was rejected: it works on `serde_json::Value`, which would lose format preservation, and RFC 6902 is about 200 lines.
  Since then: `patch` follows RFC 6902 strictly and never creates missing parents (`--no-create` on `patch` is a no-op kept for symmetry with `set`); `add` into an array inserts instead of replacing; a `move` into the value's own child is refused.
- **A whole patch is atomic.** Ops run against a clone; the original is replaced only if every op, `test` included, succeeds (RFC 6902 §5). Per-op rollback logs were rejected as complexity without user benefit; the clone costs one file's worth of memory.
- **Merge is exactly RFC 7396:** `null` deletes (a missing key is not an error), maps merge recursively, anything else replaces, arrays are replaced whole (element-wise merge is non-standard and ambiguous). Setting a literal `null` goes through `set` or `patch`.
  Since then: a mapping patch on a root that is not a mapping replaces the root (RFC 7396 §2), and `merge` on a multi-document YAML stream is refused (exit 6).
- **`diff` is a plain recursive walk** emitting `add`/`remove`/`replace`, arrays compared index by index, no `move`/`copy` detection (finding them is a similarity-search problem; output stays linear). Key order is ignored. No `test` ops in the output: they are assertions for applying, not part of a diff. The `json-patch` crate's diff was rejected because it loses big-number precision.
- **Glob expansion in the bulk driver, not in clap** (argument parsing must not touch the filesystem). `globset` over `glob` (always-on `**`, cheap multi-pattern matching), walking with `walkdir` from the longest literal prefix of the pattern. A path with no glob characters takes the old single-file path unchanged. Relying on shell expansion alone was rejected: quoted patterns and non-POSIX shells would behave differently.
- **`--parallel` on a `rayon` pool,** default 1 (sequential), 0 meaning one thread per CPU. Per-file output is buffered and printed in match order so parallel runs stay deterministic. Hand-rolled `std::thread` pools were rejected.
- **`--check` is a third output mode** beside `-i` and `--diff`, mutually exclusive with both: nothing is written, exit 1 if any file would change. Exit 1 sits with `exists` answering "no". Rejected: overloading `--diff`, and a separate `check` subcommand.
- **Exit codes in bulk mode:** without `--continue-on-error` the run stops at the first failure with that failure's code; with it, any failure gives exit 7 and the summary; `--check` gives 0 or 1. **No cross-file transaction**: files written before a failure stay written, as with `xargs` or GNU `parallel`. Two-phase commit across files was ruled out for good.
- **`convert -i`** writes the converted file (extension swapped to the `-F` format) atomically and deletes the source only after that succeeded, so a crash leaves both files rather than neither. Converting to the same format is refused; `--backup` applies.
- **One `FileOp` trait and one `run_per_file` harness** for every write command, so bulk semantics have one implementation and one test surface.
- **`/-` (append to array)** is understood by pointers in write paths.

Since then: `--parallel N` on a single file is accepted and silently runs sequentially; the spec said it would be refused to surface typos. `--check --continue-on-error` exits 7 when a file failed, ahead of 0 and 1 (2026-11-22); it used to hide the failed files.

**Deferred or left out.** A `--no-glob` switch for literal file names containing `[` (shell escaping was judged enough; not added). An order-sensitive `diff --strict`. Patch dialects other than the two RFCs. Transactional bulk writes (never).

## 2026-05-04 — Formatting and normalisation (add-style-and-normalization)

Milestone M4: deterministic output.

**What it did.** Added `dq fmt` (re-emit a file through its format's writer, with all bulk flags), the global flags `--sort-keys` and `--indent N`, a `WriteOptions` struct in `dq-core` with a `Format::write_with_options` method, accepted `validate --check` as an alias, and shipped `.pre-commit-hooks.yaml`. See [reference/fmt.md](reference/fmt.md).

**Decisions and reasons**

- **`fmt` re-emits and drops comments, by design.** It is the explicit canonicaliser; textual edits remain the comment-preserving path for `set`/`del`/`patch`/`merge`. Walking the span tree to rewrite in place would need the comment-preserving emitter dq does not have. A `fmt` that does nothing without flags was rejected as surprising.
- **`WriteOptions` lives in `dq-core`, is `#[non_exhaustive]`, and the new trait method defaults to plain `write`,** so later formats and the jq layer reuse it and future fields do not break callers. A builder was judged ceremony for two fields.
- **`canonicalize_keys` returns a sorted deep copy** instead of sorting in place: the parsed tree is the source of truth for textual edits and must not change. Sorting before the writer was the only option for JSON, because `serde_json` with `preserve_order` cannot sort at write time.
- **Both flags are global,** like `-i`/`--diff`/`--check`, so users do not track which subcommand takes which flag; read commands accept and ignore them.
- **`--sort-keys` is a no-op on textual-edit commands.** Reordering would break the M2 contract; refusing the flag would break users who set it globally; running `fmt` implicitly after `set` would rewrite files behind the user's back. The documented workflow is `set` then `fmt`.
- **`--indent` applies where it means something:** JSON and JSONL honour it; YAML ignores it because the YAML writer exposed no indent setting; TOML's layout is fixed. Rejecting the flag per format was too aggressive for a global flag; post-processing YAML text was judged fragile. `convert` to a format that ignores the indent does not complain.
- **Alphabetic key order** is the canonical interpretation; RFC 8785 (JCS) ordering could come later as a separate option.
- **`validate --check` is accepted and means plain `validate`,** so pre-commit hook entries can use `--check` uniformly. Giving it a schema-validation meaning was rejected as conflating two ideas.
- **Pre-commit hooks** `dq-fmt-check` and `dq-validate` with `language: system`, expecting `dq` on `PATH`; switching to `language: rust` was planned for M6. Since then: still `language: system`.

**Deferred or left out.** `--quote-style`, `--flow-style` and `--strip-comments`, planned for M4: each needs a comment-preserving emitter (saphyr issue #103 again). They remain out of scope. Also out: per-format formatting flags, comment-preserving `fmt`, `fmt --watch` (use `entr`/`watchexec`), a separate `--read-only` probe (`--diff` already does it).

## 2026-05-04 — More formats (add-format-extensions)

Milestone M5: format coverage for the lint engine.

**What it did.** Added HCL (`.hcl`, `.tf`, `.tfvars`; read and write through `hcl-rs`), INI and `.properties` (`rust-ini`), `.env` (own scanner and writer), CSV and TSV (`csv`), Dockerfile (read-only), `.gitignore`/`.dockerignore` (read-only) and Markdown front matter (read and write). Write targets joined `-F`: `hcl`, `ini`, `dotenv`, `csv`, `tsv`, `frontmatter`. See [reference/formats.md](reference/formats.md).

**Decisions and reasons**

- **Every new format produces a value-only document** with no span map, so `set`/`del`/`patch`/`merge` on them fail with "write unavailable" (exit 7) while `fmt` and `convert` work through re-emit. Span maps for HCL (no stable position API in `hcl-rs`) and INI (doubles the work) were deferred; making `set` re-emit these formats was rejected because it would muddy the M2 contract.
- **Front matter is a wrapper format.** It detects a YAML (`---`), TOML (`+++`) or JSON (`{`) header, parses it with the existing parser, and keeps the body as opaque bytes in one new `Document` field; a file with no header is an empty map plus the whole file as body. A `---` only counts as front matter when a closing marker appears within the first ~64 KB. Rejected: a `--frontmatter` flag on every command (leaks the concept everywhere), putting the body in the value tree (`paths` would list `/body`), supporting only TOML (Jekyll, MkDocs and Obsidian use YAML).
  Since then: M9 made `.md` default to the full Markdown AST; this shape stays reachable with `-F frontmatter`.
- **CSV requires a header row and is an array of records whose cells are all strings.** Type inference was rejected because columns mixing `"42"` and `"42.0"` would surprise users. Writing requires an array of maps with the same keys and errors otherwise. TSV is the same parser with a tab delimiter. A `--csv-delimiter` flag was deferred in favour of registering more delimiter variants as formats if needed.
- **`.env` uses a hand-written scanner** (`KEY=VALUE`, `export`, double quotes with escapes, literal single quotes, inline comments). `dotenvy` was rejected because its API loads into the process environment: side effects and racing tests. No `${VAR}` interpolation (the runtime's job). Comments are dropped on write. Since then: the `dotenvy` dependency, first kept "for later", is gone.
- **INI keeps sections in source order**; keys before the first header go under the section `""`; `.properties` shares the parser. A planned shadow map to remember which values were quoted (`ini_meta`) does not exist in the code.
- **Dockerfile is read-only**, an array of `{instruction, arguments, line}`, parsed with `dockerfile-parser-rs` 3.3 (the 0.9 line named in planning was a different, deprecated crate). Round-trip writing was deferred until a fix engine needed it.
  Since then: dq parses Dockerfiles itself following BuildKit rules (commit f75171e, [crates/dq-core/src/parsers/dockerfile.rs](../crates/dq-core/src/parsers/dockerfile.rs)). Shell-form `arguments` is the text as written, JSON form is an array, `line` is the line the instruction starts on; the external crate is gone.
- **Ignore lists are a flat array of patterns;** comments and blank lines are dropped, `!` negation is not split out.
- **Write targets are a separate enum,** so `-F dockerfile` or `-F ignore-list` fails at argument parsing instead of at run time. The spec gives that failure as exit 6; since then it is a clap usage error, exit 2.
- **No new CLI flags.** `--sort-keys` and `--indent` apply where the format has a meaning for them.
- **All new crates are workspace dependencies** under MIT or Apache-2.0, checked by `cargo deny`. Since then: `hcl-rs` 0.20, `csv` 1.4.

**Deferred or left out.** Markdown body parsing (M9), XML (M11), HCL comment and formatting preservation and HCL spans (still out: Terraform diagnostics report at line 1), textual edits for any of these formats, CSV type inference, automatic detection of `.yaml.tpl`/`.toml.tpl`, `.envrc` (shell, not data), flattening HCL `for_each`/`dynamic`, and the formats the plan keeps on demand only (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL).

## 2026-05-04 — jq transforms (add-transform-layer)

Milestone M7. JSON Pointer covers "set this value"; anything more expressive needed a query language, and the M8 lint rules needed one too.

**What it did.** Turned `dq-transform` into a real crate wrapping jaq (`jaq-core`, `jaq-std`, `jaq-json`), added `dq query EXPR FILE` and `dq set --jq EXPR FILE`. See [reference/jq.md](reference/jq.md).

**Decisions and reasons**

- **jq, through jaq, rather than an own DSL.** Choosing jq here meant the lint engine could ship without reopening the query-language question. jaq is a pure-Rust, MIT-licensed jq implementation.
- **Values cross into jaq through `serde_json::Value`** (`arbitrary_precision` keeps number literals intact through every hop). Direct `Value` ↔ `jaq_json::Val` conversions were rejected: they would couple `dq-transform` to `dq-core` and duplicate an existing conversion.
- **A compiled `JqEngine` is `Send + Sync + Clone`** and is compiled once, then shared with `Arc` across `rayon` workers. Compiling per file or per thread wastes the expensive step. This needs `jaq-json`'s `sync` feature (`Arc` instead of `Rc`).
- **The `embedded-jq` feature is on by default;** without it every call returns a "feature disabled" error. Falling back to a system `jq` was rejected (different dialects behind one command); hiding `query` in such builds was rejected (an "unknown command" message is worse).
- **`query` is single-file and single-document;** `--doc all` hands the filter an array of all documents. Running the filter per document and concatenating the outputs was rejected as hard to consume. Argument order is `EXPR FILE`, like `jq` and `select`; `-` reads stdin with `-F`.
- **`set --jq` must produce exactly one value;** zero or several are exit 6. Taking the first output (silent data loss) or collecting into an array (changes the root type) were rejected. The pointer argument must be absent or `/`; `--jq` conflicts with a value argument, `--value-from` and the template-guard flags.
- **Exit codes:** a jq compile error is a parse error (3), a runtime error is generic (1) because both file and expression were fine, misuse is 6. New jq-specific codes were rejected.
- **`set --jq` re-emits through the format writer and drops comments.** Splicing an arbitrary jq result back into the original bytes would need a structural diff to drive edits; refusing `--jq` on comment-bearing formats would push users to shell redirection. Comment loss is documented and logged at debug level.

Since then: `jaq-core` 3.1 (the plan to pin the exact minor was dropped); `set --jq` on a YAML stream sees an array of documents and writes a stream back. The risk note promised CI would build `dq-transform` with `--no-default-features` to keep the feature-off path honest; CI only builds with `--all-features`.

**Deferred or left out.** jq variables (`--arg`, `--argjson`, `--slurpfile`), raw string output (`query -r`), `query --in-place` (redundant with `set --jq`), multi-file `query`, per-line queries over JSONL, streaming evaluation, the system-`jq` fallback, comment-preserving `set --jq`, and SARIF output for `query` (refused: results are not diagnostics).

## 2026-05-05 — Markdown as a tree format (add-markdown-tree-format)

Milestone M9, the first format whose natural model is a typed tree, which tested whether the format-agnostic design held up.

**What it did.** Added a `Markdown` format (CommonMark + GFM through `comrak`) producing a full AST, made it the default for `.md`/`.markdown`, folded front matter into the AST, and shipped `@std/markdown`. `-F markdown` became a write target. See [reference/formats.md](reference/formats.md) and [reference/std-rules.md](reference/std-rules.md).

**Decisions and reasons**

- **AST nodes are ordinary maps with a `type` field,** not a new typed `Tree` in `Document`. Pointers, jq, reporters and the bulk driver needed no changes, and the converter is one function. `.type == "heading"` in jq reads as clearly as a typed check would. A typed tree stays possible later without changing what rule authors see.
- **No new selector language.** JSON Pointer plus jq covers every rule in the library; a CSS-style selector DSL would double the surface for no concrete need. This closed an open question in the plan.
- **Front matter is folded in as `frontmatter: {kind, value}`** (or `null`), so rules can test for it and cross-check it against the body. A `kind` discriminator matches how every other node identifies itself.
- **`.md` now resolves to the Markdown AST,** the one breaking change: `get post.md /title` became `/frontmatter/value/title`, or `-F frontmatter` for the old shape. No shim or deprecation period; the escape hatch is one flag.
- **GFM extensions are always on** (tables, strikethrough, autolinks, task lists, footnotes, heading ids). TOML and JSON front matter are detected by a pre-scan, since comrak only knows `---`. comrak is built without default features (no bundled CLI or syntax highlighter).
- **Positions on block nodes only;** inline nodes carry none. Rules set their own location.
- **Writing:** an unchanged document is written back byte for byte; a changed one is re-rendered by comrak, losing trailing whitespace, fence characters, reference-definition placement and indent style. `fmt` on Markdown was allowed despite these losses because `fmt -i README.md` in pre-commit was judged more useful than refusing it.
- **Text stays segmented** the way comrak splits it around inline formatting; concatenating would lose structure. A `text()` jq helper was left for later.
- **No HTML sanitising:** raw HTML is carried as text and never executed by dq.
- **A `---` that turns out not to be front matter** (the inner parse fails) falls back to no front matter.
- **Source positions are always computed;** making them lazy was left to a performance pass.

**The rule roster.** 18 rules shipped: `code-block-fenced`, `code-blocks-have-lang`, `frontmatter-date-format`, `frontmatter-required-fields`, `heading-order`, `image-alt-required`, `link-text-not-here`, `no-bare-urls`, `no-duplicate-headings`, `no-empty-headings`, `no-empty-links`, `no-empty-paragraphs`, `no-inline-html`, `no-multiple-blank-lines`, `no-trailing-spaces-in-headings`, `single-h1`, `table-header-required`, `table-pipes-aligned`. Planned but not shipped: `no-trailing-whitespace`, `final-newline` and `no-broken-relative-links` (the last needs a filesystem check rules cannot make). M11 later added `code-blocks-yaml-valid`, for 19. This change's task list was never ticked off in the archive, although the format, rules and tests shipped.

Since then: `comrak` 1.0 moved `position.end` for some nodes (lists end at their last child; indented code ends at its last character); `set`/`del` refuse Markdown ("markdown has no textual edits").

**Deferred or left out.** A typed `Tree`, CSS-style selectors, inline-node positions, textual edits on Markdown, composite rules (done in M11), `@std/static-sites`, a `--no-gfm` switch, JSON Schema over front matter (possible since M11).

## 2026-05-06 — Distribution (add-distribution)

Milestone M6 (built 2026-05-04): install in one line, update in place, CI-readable output.

**What it did.** Added `dq completions <shell>`, `dq man [PAGE]`, `dq self check`, `dq self update [--to <ver>]` and the SARIF 2.1.0 reporter (`-F sarif`, first used by `validate`); `scripts/install.sh`; release and CI workflows; `Dockerfile` (Alpine) and `Dockerfile.scratch` (static musl); Homebrew formula and AUR PKGBUILD templates; and a Claude Code skill. See [reference/distribution.md](reference/distribution.md).

**Decisions and reasons**

- **`self check` and `self update` as two subcommands under `self`,** like `rustup self`. Checking is read-only and safe in CI; updating replaces the binary. A top-level `dq update` was rejected to keep that verb free.
- **Self-update through the `self_update` crate,** not a hand-rolled download-and-rename (the crate already handles release asset selection and replacing a running binary). TLS through rustls, not OpenSSL.
  Since then: `self_update` 1.3 over `ureq`. The design promised SHA256 verification against `dq-checksums.txt`; the crate's `checksums` feature is not enabled, so `dq self update` does not verify checksums. `install.sh` does.
- **`self check` is one GitHub API call with `ureq`;** `reqwest` was rejected (Tokio runtime for one request). Exit 0 for up to date, newer available or pre-release; 5 on network errors, with a `GITHUB_TOKEN` hint when rate-limited. `self update` exits 6 with a `sudo` hint when the binary's directory is not writable, and 7 when the replace fails. No update check on startup: surprise updates are a regression for automation.
- **SARIF takes the `{ "diagnostics": [...] }` document** that the lint engine later produced too; `-F sarif` on commands without diagnostics fails at run time with exit 6. Wrapping arbitrary values into a single SARIF result was rejected as misleading; validating per subcommand at parse time was more code for the same message. JUnit and TAP waited for the lint engine.
- **Native release builds per target,** with Linux aarch64 cross-compiled by installing the cross toolchain on the Ubuntu runner rather than in a `cross-rs` container; few third-party action vendors.
  Since then: three targets (x86_64 and aarch64 Linux gnu, aarch64 macOS) and no Windows; CI runs Linux (plus macOS on push), not Windows. Releases add an SLSA build-provenance attestation, a GHCR multi-arch image and a job that bumps the Homebrew tap. Version tags are calendar versions `vYYYY.WW.BUILD`, not `v0.x`.
- **`install.sh` installs to `--install-dir`, then `$DQ_INSTALL_DIR`, then `~/.local/bin` (non-root) or `/usr/local/bin` (root).** It prints a PATH hint instead of editing shell profiles. Finding an existing `dq` on `PATH` and overwriting it was rejected, because a package manager may own that file.
- **One `dq-checksums.txt` per release** in `sha256sum` format rather than a file per artifact. cosign signing runs only when the secret is configured, so forks stay green. Since then (2026-11-21) the cosign step is gone: no release ever had the key, and the build-provenance attestation, keyless through Sigstore, is the signature.
- **Alpine image by default** (a shell for debugging and `COPY --from` chains), plus a `FROM scratch` variant. Distroless needs a glibc build; Debian-based images dwarf the binary. Since then: `rust:1.100-alpine` and `alpine:3.24`.
- **`generate-docs` stays hidden** for packaging; `completions` and `man` are the user-facing versions writing to stdout.
- **The skill lives in this repository,** so `npx skills add mazuninky/dq` installs from it. Since then: it moved from `skill/` to `skills/dq/`; `dq skill install` installs it from the binary (2026-11-22), and no other way is documented.

This change's task list was never ticked off in the archive, although everything above except the items below shipped.

**Deferred or left out.** Benchmarks against other tools (criterion benches exist in `dq-core` and `dq-exec`, but nothing publishes numbers and neither CI nor the release workflow runs them, although the proposal said the release would); a Windows installer; `self update --dry-run`; editing `PATH` from `install.sh`; Apple notarisation and Windows Authenticode (paid certificates, deferred to v1.0); end-user `cosign verify` instructions; an AUR `.SRCINFO` (only the PKGBUILD exists, untested against `makepkg`); switching the pre-commit hooks to `language: rust`. The Homebrew tap repository and the skills.sh listing were left as post-release steps.

## 2026-05-06 — Lint engine and the first rules (add-exec-engine)

Milestone M8 (built 2026-05-04): dq becomes a platform for writing linters.

**What it did.** Built `dq-exec` (rule schema, rule sets, loader, evaluator, message templates, fixture test runner) and `dq-lint` (standard rules embedded in the binary), and added `lint`, `check`, `test`, `explain`, `rules list`, `rules add`, the JUnit and TAP reporters and the global `--strict`. See [reference/lint-engine.md](reference/lint-engine.md) and [reference/std-rules.md](reference/std-rules.md).

**Decisions and reasons**

- **One canonical `{ "diagnostics": [...] }` document** goes to every reporter (console, JSON, SARIF, JUnit, TAP). One serialisation surface is easier to test and extend; reporters do not depend on `dq-exec`, keeping the dependency arrow `cli → exec → core`.
- **Rule discovery is explicit first, implicit otherwise.** `--rules` (repeatable) wins. Without it, the loader binds every `@std/<namespace>` with a rule whose `match.format` matches a format among the input files, plus `./.dq/rules/` if present. Binding everything would spam mixed repositories; binding nothing is hostile; binding by file extension would tie the rule schema to filenames.
- **Rules are YAML only, with unknown fields rejected,** so typos fail loudly. The schema (`id`, `description`, `severity`, `match`, `check`, `fix`, `loc`, `references`) was treated as a public contract: growth after M8 is additive. JSON or TOML rule files and a bespoke DSL were rejected. Since then: rules are parsed with `serde_norway`.
- **Messages use a tiny template language:** `{{ . }}`, `{{ .field }}`, `{{ .a.b }}`, `{{ .arr.0 }}`, nothing else; a missing path renders `<missing>`. `handlebars` and `tera` were too heavy; jq string concatenation trips over `null` too easily. Conditional messages are made by shaping the violation in `check.jq`.
- **Locations default to the checked file at line 1, column 1,** with `loc.file` and `loc.line` jq overrides. The design assumed the evaluator would pick up parser positions automatically; it received span-less values, so line 1 stayed the norm until the IR change added `loc.pointer`.
- **Standard rules are embedded with `include_str!`,** no runtime directory scan, keeping the single binary self-contained. `include_dir` was an unnecessary dependency.
- **Exit codes reuse existing ones:** 0 clean, 4 at least one error-severity finding (a failed quality gate, like `validate`), 1 for warnings under `--strict`, 3 when a rule's jq fails to compile (the broken artifact is the rule), 6 for misuse such as an unknown rule. A new `LINT_FAIL` code was rejected; the highest severity decides.
- **JUnit:** one `<testsuite>`, one `<testcase>` per finding (one-per-file truncates in GitLab, one-per-rule hides files), a passing testcase per clean file, severity in the failure `type`. Hand-written, no XML dependency.
- **TAP version 13** with YAML diagnostic blocks, one test point per finding and an `ok` line per clean file so the plan count is honest.
- **`dq test` matches exactly but without order:** every expected violation must match one actual (by rule id, `message_contains` or `message_equals`, optional `line`) and no actual may remain unmatched. Subset matching would let over-firing rules pass; snapshot testing was rejected.
- **`rules add`** copies a namespace into `./.dq/rules/`, fails if the destination exists unless `--force`, offers `--symlink`, never prompts. **`lint` requires explicit paths**; walking the working directory by default would surprise users.
- **`check` runs one rule,** given as `--rule <id-or-path>` or `--inline <yaml>`. The rule is a flag so the trailing file list stays unambiguous.
- **Each rule's jq is compiled once** and shared across files and workers.

**The rule library.** The target was at least 40 rules in four namespaces (k8s 18, dockerfile 8, npm 8, github-actions 6), later raised by M11 to at least 57. It was never reached in that form. Today there are 66 rules in 9 namespaces: dockerfile 4, github-actions 4, jsonschema 3, k8s 14, markdown 19, npm 6, openapi 6, skills 2, terraform 8, under [crates/dq-lint/rules/](../crates/dq-lint/rules/). Named in the plan but absent: k8s `missing-security-context`, `default-capabilities`, `deployment-no-replicas`, `missing-namespace`; dockerfile `run-as-root`, `no-update-without-install`, `multiple-cmd`, `pin-base-image-by-digest` (and `hadolint-style-shell-command` from the proposal); npm `no-pinned-deps`, `lockfile-required`; github-actions `no-bash-curl-pipe`, `no-deprecated-actions`. The `skills` namespace came later; see [plans/add-std-skills-rules.md](plans/add-std-skills-rules.md).

Since then: every document of a multi-document YAML stream is linted on its own. The open question "should `dq lint -` read stdin with `-F`" was settled as yes, but `-F` selects the reporter for `lint`, so stdin input currently fails asking for `-F` (exit 6); whether that stays is open.

**Deferred or left out.** `fix` (M10), Markdown rules (M9), composite and JSON Schema rules (M11), a community registry and plugins (M12), `--watch`, jq variables in rules, a `--no-std` switch, `dq test --parallel`, conditionals in message templates, GitLab Code Quality, HTML and step-summary reporters, caching compiled rules across runs (still open; the cache that shipped lives for one evaluator, see [archive/perf-jq-compile-cache.md](archive/perf-jq-compile-cache.md)), benchmark-driven tuning, streaming evaluation. This change's task list was never ticked off in the archive, although the engine, commands and reporters shipped.

## 2026-05-06 — Autofix (add-autofix)

Milestone M10 (built 2026-05-05): rules that report a problem can also fix it.

**What it did.** Typed the rule's `fix:` block as `{ jq: <expr> }`, added a `Fixer` in `dq-exec`, the `dq fix` command and a fix-failure error, and gave two standard rules a fix. See [reference/lint-engine.md](reference/lint-engine.md).

**Decisions and reasons**

- **A fix is one whole-document jq transform.** The plan allowed "a jq expression or an explicit set of ops"; an ops vocabulary would have tripled the work (per-op validation, idempotency, fixture format, exit codes) for cases jq already handled in one line, so it was deferred "indefinitely".
  Since then: reversed the same day by the IR change below, which added `fix.ops` (a jq expression returning an RFC 6902 subset of `add`, `replace`, `remove`) that edits in place and keeps comments; `fix.jq` is deprecated but still works.
- **The `fix` field is typed with unknown fields rejected,** so an unsupported fix shape fails at load time instead of being ignored.
- **Idempotency is checked at run time:** the fix is applied twice and the results compared; a non-idempotent fix is skipped with a warning, because it is a rule-author bug, not a reason to fail the run. Proving idempotency statically was out of scope.
- **A fix runs only when its rule's `check.jq` finds a violation,** in rule declaration order, each rule seeing the previous fixes.
- **`dq fix` reuses the bulk driver,** so `-i`, `--diff`, `--check` (exit 1 when fixes are pending), `--backup`, `--continue-on-error` and `--parallel` behave as for `set`. A fix that errors or returns zero or several values is exit 3, alongside rule compile errors ("the rule author shipped buggy jq"). No new exit code.
- **Template-guard flags are refused:** re-emitting cannot keep template placeholders. Rules are bound as in `lint`.
- **Comment loss was accepted,** as for `set --jq`.

The two reference fixes: `k8s.image-pull-policy-always` (pinned image with `imagePullPolicy: Always` becomes `IfNotPresent`) and `npm.has-license` (a non-private package gets `"UNLICENSED"`). Since then: `has-license` uses `fix.ops`, covering both an empty and a missing `license`; the k8s rule still uses `fix.jq`. They remain the only standard rules with fixes.

**Deferred or left out.** Per-violation fixes and an ops vocabulary (both delivered by `fix.ops`), comment-preserving fixes (delivered for `fix.ops`; a `fix.jq` on a YAML stream now rewrites only its own document), static idempotency proofs, and fixes for the rest of the standard library.

## 2026-05-06 — IR, edit operations and plugin ABI (add-ir-foundation)

No milestone of its own: a foundation between M10 and M11 that also delivered the WASM-plugin half of M12 ahead of time.

**Why one change.** dq had three value representations (`dq_core::Value`, `serde_json::Value`, jaq's `Val`) and converted between them at every boundary. Three needs collided there: exact line and column in lint diagnostics (positions were dropped crossing into jq, so diagnostics said line 1), per-violation fixes that keep comments, and a stable ABI for third-party WASM rules. Solving them separately would have meant rewriting the `dq-core` ↔ `dq-transform` ↔ `dq-exec` boundary three times. The work was split into five phases, each landable on its own with green CI: IR types, span-aware lint, edit operations, `fix.ops`, plugin runtime.

**What it did.** Added `Ir`/`OwnedIr` with a provenance map, `loc.pointer` in rules, `EditOp`/`EditScript`, `fix.ops`, and the `dq-plugin` crate (WIT contract `dq:plugin@0.1.0` on wasmtime, behind `--features plugins`) with the global `--plugins <DIR>` flag on `lint` and `fix`. See [reference/ir-and-edit-ops.md](reference/ir-and-edit-ops.md) and [plugin-abi.md](plugin-abi.md).

**Decisions and reasons**

- **The IR is the existing `Value` plus a side map from canonical pointer to provenance** (`Original { pointer, span }` or `Synthetic { reason }`), plus the format. Renaming `Value` would touch every call site in five crates for a nicer name; a provenance field on every node would make clones expensive and force updates on every reshuffle; `serde_json::Value` loses big-number precision; passing `Document` around is too heavy (it carries the original bytes). The pointer-keyed map mirrors the existing span map, from which `Document::as_ir` builds it.
- **Positions through jq are pointer-based.** A rule that wants an exact location returns a JSON Pointer and names it with `loc.pointer`; the evaluator looks up the span. The order is `loc.pointer`, then the deprecated `loc.line`, then line 1. Rejected: wrapping jaq values with provenance (needs a jaq fork or a wrapper that breaks inside the runtime), evaluating every rule twice (double cost, two expressions drifting apart), and jaq's `path()` (returns segment arrays, not RFC 6901 strings).
- **Edit operations are an RFC 6902 subset: `add`, `replace`, `remove`.** The same format `dq patch` takes and plugin authors already know. Merge Patch cannot delete by path or touch arrays; a custom vocabulary was YAGNI. `move`, `copy` and `test` are rejected when parsed.
- **`EditScript::apply` is a layer over `set_at`/`del_at`,** reusing the textual-edit renderers, so comments are preserved without new write machinery. Rewriting `set_at` on top of ops was rejected because it creates a cycle. Callers clone the document first when they need all-or-nothing.
- **`fix.ops` is a jq expression returning a JSON Patch array.** At least one of `jq` and `ops` must be set; if both are, `ops` wins with a warning. An ops fix counts as idempotent only if re-evaluating it gives an empty script and the bytes did not change, both checks together.
- **Fixes compose sequentially** in declaration order, the last write to a node winning; no dependency graph or conflict detection.
- **Plugins use the WebAssembly Component Model with a WIT contract on wasmtime,** feature-gated so the default binary does not grow. Plugins see the document only through host imports (`ir` for read access, `jq` to compile and evaluate on the host) and export `lint` and `fix`; jq is not compiled into plugins. Sandbox: no WASI, a fuel budget of roughly one second of CPU, a 64 MiB memory cap. Marshalling is JSON in 0.1. The package version is semver and a major mismatch is refused. Rejected: a plain C ABI (bindings per language), core WASM without the Component Model (hand-written serialisers), wasmer. Details in [plugin-abi.md](plugin-abi.md).
- **Firm non-goals:** no merging `Value` with `serde_json::Value` (big-number literals and key order are needed for round-trip), no jaq fork, no filesystem, network or process access for plugins.

Since then: wasmtime 50, with the WIT contract unchanged. Of the 66 standard rules, 14 use `loc.pointer`, 19 still use `loc.line` and 33 set no location; the removal of `loc.line` and `fix.jq` is not scheduled. The design wanted separate CI jobs with no features and with `plugins`; CI runs a single `--all-features` job. It also mentioned a plugin-enabled release build; releases ship none. The reference plugin lives in-tree at [examples/plugin-rust/](../examples/plugin-rust/) rather than out of tree. A plugin runs once per document of a YAML stream.

**Deferred or left out.** CBOR marshalling (to be measured first), WIT 1.0 (after feedback from two or three third-party plugins), fix plugins seeing lint results from the same run, static analysis of which jq filters keep pointers, any sandbox escalation.

## 2026-05-07 — JSON Schema, composite rules, XML (add-validation-and-extended-formats)

Milestone M11, the last before the community registry (M12).

**What it did.** XML read and write through `quick-xml`; inline position offsets in the IR; `check:` became one of `jq`, `schema`, `schema_file` or `extract` + `nested`; JSON Schema validation through the `jsonschema` crate; composite cross-format rules; and three namespaces, `@std/jsonschema` (3 rules), `@std/terraform` (8) and `@std/openapi` (6). Delivered in five phases: XML, inline offsets, check variants with JSON Schema, composite rules, rulesets. See [reference/rule-checks.md](reference/rule-checks.md), [reference/formats.md](reference/formats.md) and [reference/std-rules.md](reference/std-rules.md).

**Why one change.** Composite rules need inline offsets to place nested diagnostics; a schema rule is a special case of "parse, then validate"; XML is the next format to exercise both. Separate changes would have refactored the same `dq-exec` ↔ IR boundary three times.

**Decisions and reasons**

- **`check` is an untagged variant,** so every existing `check: { jq, message }` rule parses unchanged; a custom error says when a rule has none or several of the variants. A required `kind` tag was rejected because it would break every rule.
- **JSON Schema 2020-12 through the `jsonschema` crate:** no unsafe code, the official test suite, library-only. `boon` (smaller, slower to update), `valico` (abandoned) and shelling out to `check-jsonschema` (breaks the single binary) were rejected. Validators compile once per run. Each error becomes one diagnostic; its instance path is already an RFC 6901 pointer, so line and column come from the IR, falling back to 1:1; the message includes the schema keyword location.
- **`schema_file` is resolved relative to the rule's directory;** absolute paths and `..` escapes are rejected after canonicalising. **`$ref` stays inside the schema:** HTTP and file references are rejected when compiling, and the validator never touches the network or the filesystem.
- **Composite rules:** `extract` is a jq expression returning `[{value, format, anchor}]`; each value is parsed in its format and checked by an inline `nested` rule, and the inner diagnostics are projected back through the anchor's position plus the inline offset. Rejected: an extraction DSL (jq already exists), referencing the nested rule by id (needs a resolver and cycle detection), a parse-only `validate.format` (cannot compose). A value that fails to parse is reported as `<outer>.parse-failed` at the anchor. A malformed extract item is reported as its own diagnostic instead of aborting the run.
- **Composite recursion stops at depth 4,** a constant changeable only from test code and never from rule YAML. Exceeding it is exit 1: an engine-level violation, not a parse error. This guards against self-similar extracts.
- **Inline offsets are an optional field on the IR provenance,** set for YAML block scalars and Markdown fenced code blocks, best effort for JSON strings, absent elsewhere; existing callers see no change.
- **XML maps onto ordinary maps with conventional keys** (`@attrs`, `#text`, `#comments`, `#cdata`, `#pi`, a top-level `#xml` for the declaration), child elements always in arrays so pointers stay stable, namespace prefixes kept as written. Round-trip is partial: mixed content folds into `#text` with a warning, and indentation is not preserved. A `Value::Element` variant was rejected (breaks every `Value` match); `quick-xml`'s serde mode suits known shapes, not arbitrary config; `xmltree` and `roxmltree` cannot write. XML has no textual edits.
- **OpenAPI** was planned on the typed `oas3` model behind a default-on `openapi` feature. It shipped as plain jq and JSON Schema rules over the YAML/JSON parsers, with no `oas3`, no feature flag and the namespace always present. The planned rule `schema-no-additional-properties-true` did not ship.

Since then: `jsonschema` 0.61, built with default features off and validators run offline. It now also validates schemas whose root `$schema` names draft-04, -06 or -07, applying 2020-12 semantics (0.34 silently skipped their root keywords); "only 2020-12" holds for semantics, not for the declared draft. `quick-xml` 0.42.

**Deferred or left out.** XSD, RelaxNG and Schematron validation (a separate `@std/xml-xsd` change never happened), OpenAPI runtime request/response validation, a combined XML/key-value document model, CUE and the other on-demand formats, inline offsets for plugins (the WIT contract is unchanged), typed OpenAPI validation, a strict XML writer that refuses mixed content, an `expected_error` key for composite test fixtures. Rules are addressed by namespace (`@std/jsonschema`); the `@std/<namespace>/<rule>` form in the spec is not supported by the loader. Still open: `markdown.code-blocks-yaml-valid` reports at 1:1, because Markdown code-block nodes carry block positions but no byte span for the projection to start from; HCL has no spans, so Terraform diagnostics report at line 1.

Since then: fenced code blocks carry a span, and `markdown.code-blocks-yaml-valid` reports at the YAML error inside the block (2026-11-20). Structural `set`, `del`, `patch` and `merge` work on XML by rewriting the file through the writer. HCL diagnostics still report at line 1.

## 2026-11-20 — Fix everything found after the dependency update (after-deps-update-fixes)

PR #40, on top of the dependency update (#39).

**What it did.** Updating every dependency brought a long list of bugs to light. A few were regressions; most were old. Behaviour that disagreed with the README turned up as well. The rule for this branch was to fix all of it here and leave no "not fixed here" list behind. The [CHANGELOG](../CHANGELOG.md) lists every user-visible change. In outline:
- **YAML.** Multi-document streams work everywhere.
- **Numbers.** Big numbers are exact in every reader and in the jq bridge.
- **XML.** The reader and the writer are strict about well-formedness.
- **HCL.** Repeated blocks keep their data.
- **`@std` rules.** They tolerate wrong-type fields, and a rule that raises fails `dq test`.
- **Positions.** Dockerfile and Markdown diagnostics have real positions.
- **CLI flags.** `-F` became output-only and `--input-format` was added.
- **Template guard.** It works by meaning on every command.
- **Rewrite formats.** Structural edits work on XML, HCL, INI, dotenv, CSV and JSON Lines.
- **jq.** Assignment creates missing paths, as jq does.
- **Self-update.** `dq self update` verifies its download.
- **Docs.** Every README and skill example was run against the binary.

**Decisions and reasons**

- **`-F` is the output format, and `--input-format` names the input.**
  - Before, `-F` did both jobs on most commands, so `dq paths config.yaml -F json` failed to parse.
  - "Let `-F` pick the parser only when detection fails" was turned down: one flag would still have two meanings, depending on the file name.
  - stdin needs `--input-format`. Sniffing the content was turned down for the same reason unknown extensions are an error: dq does not guess formats.
  - Write commands keep a file in its own format and refuse a `-F` that names another (exit 2). They neither ignore it silently nor convert. The file's own format is accepted, so old `-F yaml -i` scripts keep working.
  - `--input-format` on a command that reads no data is refused, the way `--doc` is.
- **stdin is read once, and only a literal `-` means stdin.** A glob that expands to a file named `-` reads that file. Two `-` inputs are a usage error, not a parse error halfway through.
- **`--no-pager` stays as a hidden no-op.** Removing it would break scripts that pass it, and a pager is not worth building for a tool whose output is usually piped.
- **Rule errors have a channel of their own.** `Evaluator::evaluate` returns the diagnostics and the rule errors separately. `dq lint` keeps logging a raising rule and does not fail on it, because one bad rule should not block linting. `dq test` and the fixture runner fail the case, so a fixture can catch a raising rule. Every `@std` rule guards its field reads with jq type filters (`objects`, `arrays`, `numbers`), and a sweep test enforces that across all rules and fixture inputs.
- **The template guard decides by meaning, not by marker.**
  - When a file has template markers, it is parsed twice: as written, and with placeholders. It passes if both readings agree once the templates are put back.
  - So GitHub Actions `${{ }}` passes, while a Helm `{{ .x }}` that turns a value into a flow mapping is refused.
  - The guard runs on reads and on writes.
  - An edit must keep every template it does not target. For textual edits that means byte for byte; for rewrites it means as the same data.
  - Refusing every marker, the old rule, made GitHub Actions workflows uneditable.
- **Formats without textual edits are edited by rewrite.**
  - XML, HCL, INI, dotenv, CSV/TSV and JSON Lines point edits change the value tree and re-emit the file through the format's writer.
  - A `Layout` on the document carries what the value tree cannot hold: HCL expression sources, expression keys and block shape; dotenv `export`; the CSV header; the XML top-level order.
  - The written file must read back as the edited data, after a per-format normalisation (scalars as text for INI, dotenv, CSV and XML). A mismatch is exit 7, like the textual read-back check.
  - An edit that changes no data leaves the bytes alone.
  - `patch`, `merge` and `fix.ops` stage every op and write once.
  - Textual edits for these formats, which would keep their comments, were left for later. Rewriting works for every format today, and the docs say what it loses.
- **HCL expressions are kept as source.** hcl-rs reads every expression as a `"${…}"` string, so the writer would turn `var.a` into a literal. The reader records each expression's source by pointer, and the writer emits it while the value is unchanged. What cannot be kept is refused, never written as a string.
- **Repeated HCL blocks read as a list of their bodies.** They are not merged, because merging loses data. Writers turn the list back into blocks.
- **jq assignment creates missing paths by rewriting the program, not by patching jaq.**
  - jaq-core's assignment operators fail on null.
  - dq rewrites `L op R` so that the paths of `L` exist first, and redefines `setpath`, `delpaths` and `del` as jq defines them.
  - Indices are capped as in jq, at 536870911.
  - The rewrite is found on jaq's AST, never by searching the text.
- **`dq self update` verifies `dq-checksums.txt` with `ring`.** The release already publishes the checksums. A refusal is exit 4, the validation-failure class; a download failure is exit 5.
- **Globs:** `*` and `?` stay inside one directory, and `**` crosses directories, as in shells and `.gitignore`. A rule's `match.glob` keeps globset's default, where `*` crosses `/`, so existing rules keep matching.
- **Lint reports survive files that do not parse.** Such a file becomes a `lint.parse` result, the run still exits 3, and SARIF is still written, so CI uploads it.
- **Output escaping.** Control characters (C0, DEL, C1) print as `\xNN`. Bidi controls and U+2028/U+2029 print as `\uNNNN`. This applies on the console and in error messages, so a file cannot rewrite the terminal or forge a line.
- **XML.** The owner decided that the writer never emits XML that is not well-formed. The reader refuses what is not well-formed instead of reading part of it. A `<?a:b x?>` target is refused on purpose.
- **TOON keeps the documented `.nan` / `.inf` strings,** as its spec has no non-finite numbers.

**Deferred or left out.**
- Kept as features in the [roadmap](roadmap.md), not treated as bugs: structured errors (a caret under the offending byte, a JSON error object on stderr) and the `DQ_*` environment overrides.
- Not built: textual (comment-keeping) edits for XML, HCL, INI, dotenv and CSV.
- XML: comments and PIs inside an element are written before its children.
- `fmt` copies TOML byte for byte, comments included.
- `set --jq .` rewrites unchanged data, because people use it to reformat.
- HCL diagnostics still report at line 1.
- jq differences that remain are listed in [reference/jq.md](reference/jq.md).

## 2026-11-21 — Linear JSON spans, cached jq compiles, in-place pointer walks (perf)

Three performance changes that shipped in 2026.20.2, archived together once their numbers were measured. The designs, with the alternatives turned down, are in [archive/perf-json-parse-linear.md](archive/perf-json-parse-linear.md), [archive/perf-jq-compile-cache.md](archive/perf-jq-compile-cache.md) and [archive/perf-pointer-recursive-walks.md](archive/perf-pointer-recursive-walks.md). The numbers are criterion `--quick` runs on an Apple M4 Pro; the before-numbers came from an M1 Pro, about 1.8× slower.

**What it did.**
- **JSON spans in linear time.** The span builder scanned back to the previous newline for every value, which is quadratic on compact JSON: 10,000 records took 13.9 s. A `LineIndex` built once (newline offsets, binary search, indent cached per line) replaced the scans. 10,000 records now parse in 18 ms, next to YAML (26 ms) and TOML (28 ms).
- **One compile per distinct jq expression.** `Evaluator::new` compiled every jq field of every rule. A cache scoped to one `Evaluator::new` shares an `Arc<JqEngine>` per expression, composite `extract` included. Building the `@std/k8s` evaluator went from about 49 compile-costs to about 25 (4.9 ms here).
- **Recursive walks extend one pointer.** `diff` and `merge` built each child pointer with `Pointer::with_segment`, a copy of the whole path. `push_segment` / `pop_segment` let them walk with one `&mut Pointer`. At depth 64 the walk costs 829 ns against 26.9 µs for a `with_segment` chain.
- New bench groups `transform/diff_deep` and `transform/apply_merge_deep` vary depth at a fixed leaf count.

**Decisions and reasons**

- **The JSON parse stays two-pass:** `serde_json` for the value, then a byte walk for spans. `serde_json` reports the reader's position, not one per value. A lazy span map only moves the cost, since `lint` and textual edits need every span. A single-pass parser would redo what `serde_json` already gets right.
- **The jq cache lives for one `Evaluator::new`, keyed on the exact expression text.** A process-wide cache only grows, shares state between tests that compile invalid expressions on purpose, and gains nothing in a CLI that builds one evaluator per run. Whitespace is not normalised, so two spellings never share an engine. Engines are behind `Arc` because jaq's `Filter` is not `Clone`, and they are `Send + Sync` through `jaq-json`'s `sync` feature.
- **Push and pop by hand, with no guard type.** `merge_into` pops before it applies `?`, so an error leaves the pointer as it found it. `with_segment` stays for callers that keep the parent. `Arc<Vec<Segment>>`, `SmallVec` and a persistent vector were turned down: each still copies, or adds a dependency for an O(log n) push.

**Deferred or left out.**
- Every jq compile re-parses jaq's standard definitions: compiling `.` costs as much as a pipe (193 µs each), so that setup is most of a compile. Sharing the parsed definitions would speed up every rule set and one-shot `query`.
- No guard holds a bench to a number; criterion only reports. The plan's 6 ms target for `evaluator_new` assumed 28 k8s rules; there are 14.
- Measuring deep documents found two costs that grow faster than depth. `diff_into` compares subtrees with `==` before descending, so a differing chain is re-scanned at every level. `merge_into` clones the target subtree through `read_value_at` at every level to see whether it is a map. Each textual edit also parses the file again to check it, so `merge`, `patch` and `fix.ops` cost changes × file size. That check is deliberate.
- From the plans: an incremental `pointer_for` in the JSON span builder; a shared `LineIndex` (no second user yet); `compile_composite` repeats the cache lookup instead of calling `compile_or_cached`. The YAML span-builder audit was dropped: YAML parses in linear time.

Since then: the `==` re-scan in `diff` and the subtree clones in `merge` are gone (2026-11-22); `transform/diff_deep` grows about 1.8× per doubling of depth. The read-back parse per edit stays.

## 2026-11-22 — The agent skill ships in the binary (`dq skill`)

**What it did.** `SKILL.md` and `references/` are embedded in the binary; `dq skill install | update | status | remove` manage the installed copy, and the first run of a newer dq brings an unedited one to its version. Behaviour: [reference/skill.md](reference/skill.md). Design: [plans/skill-command.md](plans/skill-command.md).

**Decisions and reasons**

- **An own command** (owner) instead of `npx skills`: no Node, no network, and the skill is always the one of this version.
- **The skill is updated on the first run of a new version** (owner), an exception to "one command, one operation, no hidden state". A package manager has no moment for it: Homebrew runs `post_install` in a sandbox with a temporary `HOME`. The exception is narrow: only files dq wrote and nobody edited, only where the user had it installed, only when the version changes, silent, without effect on the exit code, off with `DQ_NO_SKILL_UPDATE` and under `CI`. Turned down: a hint to run a command.
- **What dq wrote is known by a manifest with hashes**, `.dq-skill.json`. Without it a directory is someone else's, whoever made it, so there is no migration from other installers (owner).
- **`~/.agents/skills` and a link from Claude Code** (owner), not a list of agents to keep current. The copy is one; the link is made only by `install`.
- **The automatic update is in release builds only**, so that developing and testing dq cannot touch the developer's skill; CI runs its tests in a build without debug assertions.
- **The skill directory itself is the lock**, so nothing is left behind in the home directory. `remove` deletes what it has locked; whoever waited compares the directory with the path and starts over.
- **Two spellings of a name are one file when the directory lists one entry for both**, by device and inode: a file renamed in case only between versions is renamed on macOS too, and a hard link stays the user's.

**Deferred or left out.** Other agents' configuration, rules and instruction files; updating a project's skill on its own (it is under version control); Windows links. `atl` gets the same command with an implementation of its own.
