# data-query-write Specification

## Purpose

Defines the behavioural contract for `dq set` and `dq del` — the
point-mutation and point-deletion commands. Covers RFC 6901 JSON Pointer
semantics, value-source resolution (inline / stdin / `@<path>` /
`--value-from`), the JSON-literal heuristic vs `--value-string`,
mkdir-p vs `--no-create`, the three output modes (stdout / `-i` /
`--diff`), atomic in-place write contract, `.bak` backup naming, and
the M2 scope boundary against bulk operations / `patch` / `merge` /
`diff`-between-files (which arrive in M3). Consumers: end users editing
config files, AI agents in CI/CD pipelines, and downstream milestones
(M3 bulk, M10 autofix) that build on these primitives.
## Requirements
### Requirement: `dq set` command — point-mutation semantics

The CLI SHALL provide a `set` subcommand `dq set <FILE> <POINTER> [VALUE]` that mutates the value at the given JSON Pointer ([RFC 6901](https://www.rfc-editor.org/rfc/rfc6901)) inside `<FILE>`. By default, intermediate map and array nodes that do not exist along `<POINTER>` SHALL be created (mkdir-p semantics, mirroring `jq` `setpath`). The flag `--no-create` SHALL force exit code 2 (`NOT_FOUND`) when any segment of the pointer is missing.

#### Scenario: Set creates intermediate map nodes by default
- **WHEN** the user runs `dq set config.yaml /a/b/c hello` on a file where neither `/a` nor `/a/b` exist
- **THEN** the resulting document contains `{a: {b: {c: hello}}}`, the file is unchanged on disk (no `-i`), the new tree is written to stdout, and exit code is 0

#### Scenario: --no-create rejects missing intermediate
- **WHEN** the user runs `dq set config.yaml /a/b/c hello --no-create` on a file where `/a/b` is missing
- **THEN** the command writes a structured `Path` error naming the missing segment `b` and exits with code 2

#### Scenario: Replace existing scalar
- **WHEN** the user runs `dq set deploy.yaml /spec/replicas 5 -i` on a Kubernetes manifest with `spec.replicas: 3`
- **THEN** the file on disk has `spec.replicas: 5`, every other byte is preserved, and exit code is 0

### Requirement: `dq del` command — point-deletion semantics

The CLI SHALL provide a `del` subcommand `dq del <FILE> <POINTER>` that removes the value at the given pointer. Unlike `set`, `del` SHALL NOT silently succeed when the pointer is absent — a missing pointer SHALL produce a structured `Path` error and exit code 2 (`NOT_FOUND`).

#### Scenario: Delete existing key
- **WHEN** the user runs `dq del package.json /scripts/old-build -i` on a `package.json` with `scripts.old-build`
- **THEN** the key is removed, sibling keys keep their order, the file is updated atomically, and exit code is 0

#### Scenario: Delete missing pointer is not silent
- **WHEN** the user runs `dq del package.json /nonexistent`
- **THEN** the command writes a structured `Path` error and exits with code 2

#### Scenario: Delete from array shifts subsequent indices
- **WHEN** the user runs `dq del workflow.yaml /jobs/build/steps/2 -i` on a workflow whose `steps` is a 5-element array
- **THEN** the resulting `steps` is a 4-element array; what was at `/3` is now at `/2`; what was at `/4` is now at `/3`

### Requirement: Value source resolution for `set`

The `set` command SHALL accept the value to write from one of four sources, resolved in priority order: (1) inline argument string, (2) stdin via `<VALUE>` literal `-`, (3) external file via `<VALUE>` prefix `@<path>`, (4) explicit flag `--value-from <path>`. Inline string arguments SHALL be parsed as JSON literals when they begin with `{`, `[`, a digit, `-`, or match `true`/`false`/`null`; otherwise treated as a string. The flag `--value-string` SHALL force string interpretation even for JSON-literal-shaped inputs.

#### Scenario: Inline JSON-literal heuristic
- **WHEN** the user runs `dq set config.yaml /port 8080`
- **THEN** the value at `/port` becomes integer `8080`, not string `"8080"`

#### Scenario: --value-string forces string
- **WHEN** the user runs `dq set config.yaml /port 8080 --value-string`
- **THEN** the value at `/port` becomes string `"8080"`

#### Scenario: Stdin source
- **WHEN** the user runs `echo '{"x":1,"y":2}' | dq set config.yaml /metadata -`
- **THEN** the value at `/metadata` becomes the JSON object parsed from stdin

#### Scenario: File source via @ prefix
- **WHEN** the user runs `dq set config.yaml /spec @new-spec.json`
- **THEN** the value at `/spec` becomes the document parsed from `new-spec.json` (format detected by extension)

### Requirement: Write output mode — stdout vs in-place vs diff

`set` and `del` SHALL support three output modes governed by mutually exclusive flags: (a) **default — stdout**: the mutated document is written to stdout in the file's source format, the original file is untouched; (b) `-i/--in-place`: the file on disk is replaced atomically with the mutated content, nothing is written to stdout; (c) `--diff`: a unified diff (`source vs. mutated`) is written to stdout, the file is untouched. Combining `-i` with `--diff` SHALL be rejected by clap with exit code 6 (`INVALID_INPUT`).

#### Scenario: Default writes to stdout
- **WHEN** the user runs `dq set k8s/deploy.yaml /spec/replicas 5` (no `-i`, no `--diff`)
- **THEN** the original file is byte-identical on disk, and stdout contains the full document with `spec.replicas: 5`

#### Scenario: -i writes file atomically
- **WHEN** the user runs `dq set k8s/deploy.yaml /spec/replicas 5 -i`
- **THEN** the file on disk has the new content, stdout is empty, and exit code is 0

#### Scenario: --diff shows unified diff without writing
- **WHEN** the user runs `dq set k8s/deploy.yaml /spec/replicas 5 --diff`
- **THEN** stdout contains a unified diff with one `-replicas: 3` and one `+replicas: 5`, the file is unchanged, and exit code is 0

#### Scenario: -i with --diff is rejected
- **WHEN** the user runs `dq set f.yaml /x 1 -i --diff`
- **THEN** clap exits with code 6 and a structured error explaining the conflict

### Requirement: Atomic in-place write

`-i/--in-place` SHALL write the mutated content via `tempfile::NamedTempFile::new_in(<file's parent dir>)` followed by `persist(<original path>)`. The temp file MUST live in the same directory as the target so that the rename is atomic on every supported filesystem. On any failure during write or rename, the original file MUST remain unchanged.

#### Scenario: Crash between write and rename leaves original intact
- **WHEN** the process is killed with `SIGKILL` after the temp file is fully written but before `persist()` returns
- **THEN** the original file on disk is byte-identical to its pre-invocation content; the temp file may be left behind in the same directory

#### Scenario: Disk-full during write
- **WHEN** the temp file write fails with `ENOSPC`
- **THEN** the command writes a structured `Io` error, the original file is unchanged, and exit code is 7 (`WRITE_FAILED`)

### Requirement: `--backup` creates `.bak` before overwrite

When `-i` and `--backup` are both set, the command SHALL copy the original file to `<path>.bak` before performing the atomic rename. If `<path>.bak` already exists, it SHALL be overwritten without prompting (consistent with non-interactive contract). `--backup` without `-i` SHALL be rejected as `INVALID_INPUT` (exit 6).

#### Scenario: Backup file is created
- **WHEN** the user runs `dq set deploy.yaml /spec/replicas 5 -i --backup`
- **THEN** after the command, both `deploy.yaml` (with new content) and `deploy.yaml.bak` (with old content) exist, and exit code is 0

#### Scenario: Backup overwrites existing .bak
- **WHEN** `deploy.yaml.bak` already exists from a prior invocation and the user runs the command again
- **THEN** the prior `.bak` is overwritten with the now-current content; no prompt is shown

#### Scenario: --backup without -i is rejected
- **WHEN** the user runs `dq set f.yaml /x 1 --backup` without `-i`
- **THEN** clap exits with code 6 and a structured error

### Requirement: Format constraint — `-F` is incompatible with `-i`

When writing to disk with `-i`, the output format SHALL be the file's source format (detected by extension). Combining `-i` with `-F <fmt>` SHALL be rejected as `INVALID_INPUT` (exit 6) — format conversion in-place is the responsibility of the M3 `convert -i` work, not M2 `set`/`del`.

#### Scenario: -i with -F is rejected
- **WHEN** the user runs `dq set deploy.yaml /spec/replicas 5 -i -F json`
- **THEN** clap exits with code 6 and the error message says `-F` cannot be combined with `-i` (use `dq convert -F json` then `dq set` for cross-format edits)

### Requirement: New exit code `WRITE_FAILED = 7`

`crates/dq-cli/src/exit_code.rs` SHALL define `pub const WRITE_FAILED: i32 = 7`. The `exit_code_for_error` function SHALL return `WRITE_FAILED` for `Error::Io` errors that occurred during the write side of a `set`/`del` command (atomic temp create, content write, rename, backup copy). Read-side `Error::Io` (opening the source file) SHALL continue to map to `IO_ERROR = 5`.

#### Scenario: Write-side IO failure maps to 7
- **WHEN** the command fails because the parent directory is read-only and the temp file cannot be created
- **THEN** `exit_code_for_error` returns 7

#### Scenario: Read-side IO failure still maps to 5
- **WHEN** the command fails because the source file does not exist
- **THEN** `exit_code_for_error` returns 5

### Requirement: Big-int round-trip through `set`

A `set` operation that writes a big integer literal (value outside `i64::MIN..=i64::MAX`) and is followed by a `get` of the same pointer SHALL return the exact original textual representation, byte-for-byte. This applies to all three formats (YAML, JSON, TOML).

#### Scenario: Set big-int and read it back
- **WHEN** the user runs `dq set data.json /id 4722366482869645213696` followed by `dq get data.json /id`
- **THEN** stdout from `get` is exactly `4722366482869645213696` (no scientific notation, no truncation, no trailing zero loss)

### Requirement: Anti-scope for M2 write commands

In M7 the binary SHALL include `set`, `del`, `patch`, `merge`, `diff`, `convert -i`, and `dq fmt` with the M3 bulk driver, plus the new `dq query` read subcommand and the `set --jq EXPR` transform mode. It SHALL NOT include the linter family (`lint`/`check`/`test`/`explain`/`rules`/`fix`), markdown body parsing, JSON Schema validation, composite-rules, transactional bulk writes (rolling back successful files when a later file fails), or `dq query --in-place`. They are reserved for M8, M9, M10, and M11. Attempts to use them SHALL produce clap "unknown argument" errors (exit 6).

The previously-deferred YAML-emitter flags `--quote-style <double|single|auto>`, `--flow-style <block|flow|auto>`, and `--strip-comments` remain reserved (their implementation requires a comment-preserving emitter — see [dq-plan.md](../../../dq-plan.md)).

#### Scenario: Linter subcommand is still unreachable
- **WHEN** the user runs `dq lint config.yaml`
- **THEN** clap's standard "unknown subcommand" error is shown (exit 6)

#### Scenario: `--quote-style` is still unknown
- **WHEN** the user runs `dq fmt config.yaml --quote-style double`
- **THEN** clap exits with code 6 and "unrecognized argument" error

#### Scenario: `dq query` is reachable in M7
- **WHEN** the user runs `dq query --help`
- **THEN** clap prints the help for `query` and exits 0

#### Scenario: `dq set --jq` is reachable in M7
- **WHEN** the user runs `dq set --help`
- **THEN** the help text lists the `--jq <EXPR>` flag with its description and the conflict notes for POINTER / VALUE

### Requirement: `dq patch` command — RFC 6902 JSON Patch

The CLI SHALL provide a `patch` subcommand `dq patch <FILE> <OPS>` that applies an [RFC 6902](https://www.rfc-editor.org/rfc/rfc6902) JSON Patch to `<FILE>`. `<OPS>` accepts the same shapes as `set`'s value source: inline JSON literal, `-` for stdin, `@<path>` for a file, or explicit `--ops-from <path>`. A simplified line-format (`<op> <pointer> [json-value]` per line) is also accepted via `--line-format` for hand-written patches. Operations supported: `add`, `remove`, `replace`, `move`, `copy`, `test`. A failed `test` SHALL abort the entire patch atomically — the file SHALL be left bit-identical to its pre-invocation content.

#### Scenario: Apply a JSON Patch from stdin
- **WHEN** the user runs `echo '[{"op":"replace","path":"/spec/replicas","value":5}]' | dq patch deploy.yaml - -i`
- **THEN** the file is updated with `replicas: 5` atomically, the rest of the document is byte-preserved, and the exit code is 0

#### Scenario: Failed test op aborts the whole patch
- **WHEN** the user runs `dq patch deploy.yaml @ops.json -i` where `ops.json` contains `[{"op":"test","path":"/spec/replicas","value":99},{"op":"replace","path":"/spec/replicas","value":5}]`
- **THEN** the test op fails (actual replicas was 3, not 99), neither change is applied, the file is byte-identical to before, the exit code is non-zero, and stderr names the failing op

#### Scenario: Line-format input for ergonomic ops
- **WHEN** the user runs `dq patch deploy.yaml --line-format -i` with stdin containing two lines `replace /spec/replicas 5\nremove /metadata/annotations/old-key`
- **THEN** both ops apply, the file is updated, and the exit code is 0

### Requirement: `dq merge` command — RFC 7396 Merge Patch

The CLI SHALL provide a `merge` subcommand `dq merge <FILE> <PATCH>` that applies an [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396) JSON Merge Patch. `<PATCH>` accepts the same shapes as `set`'s value source. Semantics: `null` in the patch removes the key from the target; objects merge recursively; arrays and scalars replace.

#### Scenario: Merge replaces scalars and recurses into maps
- **WHEN** the user runs `dq merge deploy.yaml @patch.json -i` where `patch.json` is `{"spec":{"replicas":5,"strategy":{"type":"RollingUpdate"}}}` and the existing `spec` already has different values for those fields
- **THEN** `replicas` is set to `5`, `strategy.type` becomes `"RollingUpdate"`, every other field under `spec` is preserved, and the exit code is 0

#### Scenario: null in merge patch removes a key
- **WHEN** the user runs `dq merge deploy.yaml @patch.json -i` where `patch.json` is `{"metadata":{"annotations":{"old":null,"new":"value"}}}`
- **THEN** the `metadata.annotations.old` key is removed, `metadata.annotations.new` is set to `"value"`, and the exit code is 0

#### Scenario: Array in merge replaces target array
- **WHEN** the user runs `dq merge deploy.yaml @patch.json -i` where `patch.json` is `{"spec":{"containers":[{"name":"app"}]}}`
- **THEN** `spec.containers` is replaced wholesale with the new array (RFC 7396 §1: arrays do not element-merge)

### Requirement: `dq diff` command — structural diff between two files

The CLI SHALL provide a `diff` subcommand `dq diff <A> <B>` that emits a structural diff between two documents. By default, the diff SHALL be emitted as an [RFC 6902](https://www.rfc-editor.org/rfc/rfc6902) JSON Patch (a sequence of `add`/`remove`/`replace` ops) rendered through the active reporter (Console / JSON / YAML / TOML / TOON). With `--unified`, the diff SHALL be a textual unified diff over the rendered representations (using the existing `similar` integration). The diff SHALL be deterministic and minimal — equal subtrees produce no ops, and a parent `replace` SHALL NOT be followed by child ops.

#### Scenario: Default diff emits JSON Patch ops
- **WHEN** the user runs `dq diff prod-values.yaml staging-values.yaml -F json` and the only difference is `image.tag` (`v1.0.0` vs `v1.0.1`)
- **THEN** stdout contains exactly one op: `[{"op":"replace","path":"/image/tag","value":"v1.0.1"}]`

#### Scenario: Unified flag emits textual diff
- **WHEN** the user runs `dq diff a.yaml b.yaml --unified`
- **THEN** stdout contains a unified diff (`---`/`+++` headers, `@@` hunk markers) of the two documents rendered as YAML

#### Scenario: Equal documents produce empty output
- **WHEN** the user runs `dq diff a.yaml a.yaml -F json`
- **THEN** stdout is `[]` and the exit code is 0

#### Scenario: Diff round-trip
- **WHEN** the user runs `dq diff a.yaml b.yaml -F json -o ops.json` then `dq patch a.yaml @ops.json`
- **THEN** the result is structurally equal to `b.yaml`

### Requirement: `convert -i` — in-place format conversion

The `convert` command SHALL accept `-i` for in-place file rename across formats. `dq convert deploy.yaml -i -F json` SHALL: (1) read `deploy.yaml`, (2) parse it, (3) write the result atomically as `deploy.json`, (4) on success only, remove the source `deploy.yaml`. With `--keep-source`, the source file SHALL NOT be removed. `convert -i` to the same format (e.g., `-F yaml` on a `.yaml` file) SHALL be rejected as `INVALID_INPUT`.

#### Scenario: convert -i swaps extension and removes source
- **WHEN** the user runs `dq convert deploy.yaml -i -F json`
- **THEN** `deploy.json` exists with the converted content, `deploy.yaml` is removed, and the exit code is 0

#### Scenario: --keep-source preserves both files
- **WHEN** the user runs `dq convert deploy.yaml -i -F json --keep-source`
- **THEN** both `deploy.yaml` (unchanged) and `deploy.json` (converted) exist, and the exit code is 0

#### Scenario: convert -i with same format is rejected
- **WHEN** the user runs `dq convert deploy.yaml -i -F yaml`
- **THEN** the command exits 6 (`INVALID_INPUT`) with a message stating the conversion is a no-op (rejected by runtime validation in `compute_target_path` after argument parsing, not by clap)

### Requirement: `Pointer` understands `/-` array-append per RFC 6902

`Pointer::parse` SHALL recognise the trailing `-` segment as RFC 6902's "end-of-array" marker. `Document::set_at` SHALL resolve `-` to the current length of the target array (effectively appending). `Document::del_at` on a `-` segment SHALL return a structured `Error::Path { kind: TypeMismatch }` carrying `expected: "array index"` and `found: "array-append marker '-' is not deletable"` (RFC 6902 forbids `remove /-`). The CLI exit-code mapper routes this through the existing `path → NOT_FOUND (2)` mapping — the segment is structurally invalid as a deletion target, not a different class of caller-input error.

#### Scenario: Patch add to array tail
- **WHEN** the user runs `dq patch deploy.yaml '[{"op":"add","path":"/spec/containers/-","value":{"name":"sidecar"}}]' -i`
- **THEN** the new container is appended to the end of `spec.containers`, the existing containers are byte-preserved, and the exit code is 0

### Requirement: `dq set --jq EXPR` transform mode

The `set` subcommand SHALL accept an optional `--jq <EXPR>` flag that switches the handler into transform mode. When `--jq` is set:

1. The `<POINTER>` and `[VALUE]` positional arguments become **mutually exclusive** with `--jq`. Clap rejects `dq set FILE POINTER --jq EXPR` and `dq set FILE POINTER VALUE --jq EXPR` with `INVALID_INPUT` (exit 6) at the parse layer; the handler also re-checks at runtime to defend against any clap-level miss.
2. The handler reads `<FILE>` (format detected by extension or `-F`).
3. Parses through the format's standard `Format::parse` (NOT the write-aware `parse_yaml_with_spans` / `parse_json_with_spans`) — the textual-edit splice path is bypassed because jq can change the document's structure arbitrarily.
4. Converts the parsed `dq_core::Value` to a `serde_json::Value` via the existing `value_to_serde_json` helper.
5. Compiles the jq expression via `dq_transform::JqEngine::compile`. Compile errors are wrapped in `dq_core::Error::Parse` so the existing exit-code mapper picks `PARSE_ERROR = 3`.
6. Evaluates the filter via `JqEngine::run`. Runtime errors are wrapped in `anyhow::anyhow!(...)` and fall through to `GENERIC = 1`. The output stream MUST contain exactly **one** value; zero outputs and multi-output streams are rejected with `INVALID_INPUT` (exit 6) and a message naming the count.
7. Converts the single output back to a `dq_core::Value`.
8. Re-emits the document via `Format::write_with_options` against the global `WriteOptions` (so `--sort-keys` / `--indent` work).
9. The bulk driver receives the new bytes through `FileOpResult::Modified` and applies `-i` / `--diff` / `--check` exactly as for the existing `set` modes.

A `tracing::debug!` line on the splice-vs-re-emit fork notes that comments will be lost when `--jq` is active. The `set --help` text mentions the comment-loss tradeoff next to the `--jq` flag description.

`--jq` is compatible with every existing global flag: `-i`, `--diff`, `--check`, `--backup`, `--continue-on-error`, `--parallel`, glob expansion via the bulk driver. Filter compilation happens **once** outside the per-file loop; bulk runs share a single compiled engine via `Arc<JqEngine>` across rayon workers.

The handler SHALL reject `--jq` combined with the template-guard flags `--allow-templates` or `--raw-template-strings` (`INVALID_INPUT`, exit 6). The `--jq` path uses `Format::parse` directly without the M2 template-substitution pre-pass, and the re-emit step does not restore template placeholders — so the template-guard flags would be silently ignored if the combination were accepted. Future milestones may integrate template-guard support into the `--jq` path; until then the rejection is the documented contract.

#### Scenario: `--jq` rejected with `--allow-templates`
- **WHEN** the user runs `dq set helm-values.yaml --jq '.foo |= 2' -i --allow-templates`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and `--allow-templates`

#### Scenario: `--jq` rejected with `--raw-template-strings`
- **WHEN** the user runs `dq set helm-values.yaml --jq '.foo |= 2' -i --raw-template-strings`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and `--raw-template-strings`

#### Scenario: `--jq` increments a counter
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 1' -i` against a manifest with `spec.replicas: 3`
- **THEN** the file on disk has `spec.replicas: 4`, stdout is empty, and exit code is 0

#### Scenario: `--jq` adds a new key
- **WHEN** the user runs `dq set deploy.yaml --jq '. + {"newKey": "newValue"}' -i` against an object document
- **THEN** the file on disk contains the new top-level key with the new value, every existing key is preserved, and exit code is 0

#### Scenario: `--jq` removes a key
- **WHEN** the user runs `dq set deploy.yaml --jq 'del(.metadata.annotations.old)' -i`
- **THEN** the `metadata.annotations.old` key is removed, sibling keys preserve order, and exit code is 0

#### Scenario: `--jq` with a positional VALUE is rejected
- **WHEN** the user runs `dq set deploy.yaml /spec/replicas 5 --jq '. + 1'`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names both `--jq` and the positional VALUE as conflicting

#### Scenario: `--jq` with a positional POINTER is rejected
- **WHEN** the user runs `dq set deploy.yaml /spec/replicas --jq '. + 1'`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message states that POINTER is not accepted alongside `--jq` (the entire document is the transform target)

#### Scenario: `--jq` multi-output stream is rejected
- **WHEN** the user runs `dq set deploy.yaml --jq '.[]' -i` against an array document
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message names the count and suggests wrapping in `[...]` to collect

#### Scenario: `--jq` empty stream is rejected
- **WHEN** the user runs `dq set deploy.yaml --jq 'empty' -i`
- **THEN** the command exits with code 6 (`INVALID_INPUT`) and the error message states that the document would become empty

#### Scenario: `--jq` compile error maps to PARSE_ERROR
- **WHEN** the user runs `dq set deploy.yaml --jq '.foo |=' -i`
- **THEN** stderr contains a structured error mentioning the unterminated assignment and exit code is 3

#### Scenario: `--jq` runtime error maps to GENERIC
- **WHEN** the user runs `dq set string-only.yaml --jq '. + 1' -i` against a YAML file whose top-level value is a string
- **THEN** stderr contains the runtime type-error message and exit code is 1

#### Scenario: `--jq` re-emits via the native writer (comment loss)
- **WHEN** the user runs `dq set commented.yaml --jq '.foo |= 2' -i` against a YAML file with leading comments
- **THEN** the file on disk has the new `foo` value AND the comments are dropped (re-emit semantics, documented behaviour)

#### Scenario: `--jq` with `--diff` renders unified diff
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 1' --diff`
- **THEN** stdout contains a unified diff with `-replicas: 3` and `+replicas: 4`, the file on disk is unchanged, and exit code is 0

#### Scenario: `--jq` with `--check` reports pending change
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 1' --check` and the transform would change the file
- **THEN** the command exits with code 1 (`CheckPending` → `GENERIC`) and stderr names the file

#### Scenario: `--jq` is idempotent through `--check`
- **WHEN** the user runs `dq set deploy.yaml --jq '.spec.replicas |= . + 0' --check` (a no-op transform)
- **THEN** the command exits with code 0 (no file would be modified)

#### Scenario: `--jq` works across glob expansion
- **WHEN** the user runs `dq set 'k8s/**/*.yaml' --jq '.spec.template.spec.containers[0].image |= sub(":latest"; ":v1")' -i`
- **THEN** every matching file with a container[0] image ending in `:latest` is updated, the bulk summary lists the modified files, and exit code is 0

#### Scenario: `--jq` shares one compiled engine across rayon workers
- **WHEN** the user runs `dq set 'k8s/**/*.yaml' --jq '.spec.replicas |= . + 1' -i --parallel 4`
- **THEN** the filter is compiled exactly once (verified by a `tracing::debug!` count assertion in the integration test) and the parallel workers share the engine via `Arc`

