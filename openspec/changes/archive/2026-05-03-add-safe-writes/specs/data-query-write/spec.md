## ADDED Requirements

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

In M2 the binary SHALL NOT include any of the following: bulk-file globs (`dq set 'k8s/**/*.yaml' ...`), `patch` (RFC 6902), `merge` (RFC 7396), structural `diff` between two files, `convert -i`, `--sort-keys`, `--quote-style`, `--indent N`, `--flow-style`. They are reserved for M3 and M4. Attempts to use them SHALL produce clap "unknown argument" errors (exit 6).

#### Scenario: Glob pattern is not expanded
- **WHEN** the user runs `dq set 'k8s/**/*.yaml' /spec/replicas 3 -i`
- **THEN** the command treats `k8s/**/*.yaml` as a literal file path, fails with `IO_ERROR = 5` because no such file exists, and does NOT iterate matching files (M3 will add this)

#### Scenario: --sort-keys is unknown in M2
- **WHEN** the user runs `dq set f.yaml /x 1 -i --sort-keys`
- **THEN** clap exits with code 6 and "unrecognized argument" error
