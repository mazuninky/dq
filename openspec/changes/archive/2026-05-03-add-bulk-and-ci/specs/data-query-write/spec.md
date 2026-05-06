## ADDED Requirements

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

## MODIFIED Requirements

### Requirement: Anti-scope for M2 write commands

In M3 the binary SHALL include `set`, `del`, `patch`, `merge`, `diff`, and `convert -i` (with glob expansion) in addition to the M2 baseline. It SHALL NOT include any of the following: `--sort-keys`, `--quote-style`, `--indent N`, `--flow-style`, `--strip-comments`, `dq fmt`, `dq query`, `set --jq`, linters (`lint`/`check`/`test`/`explain`/`rules`/`fix`), markdown support, JSON Schema, composite-rules, or transactional bulk writes (rolling back successful files when a later file fails). They are reserved for M4, M7, M8, M10, M11, and beyond. Attempts to use them SHALL produce clap "unknown argument" errors (exit 6).

#### Scenario: Glob pattern is now expanded (M2 anti-scope lifted)
- **WHEN** the user runs `dq set 'k8s/**/*.yaml' /spec/replicas 3 -i`
- **THEN** the command expands the glob and operates on every matching file (M3 contract — see `data-query-bulk` capability)

#### Scenario: --sort-keys is unknown in M3
- **WHEN** the user runs `dq set f.yaml /x 1 -i --sort-keys`
- **THEN** clap exits with code 6 and "unrecognized argument" error
