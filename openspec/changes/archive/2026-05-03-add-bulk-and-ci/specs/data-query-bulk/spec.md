## ADDED Requirements

### Requirement: Glob expansion for write commands

Every write command (`set`, `del`, `patch`, `merge`, `convert`) SHALL accept a glob pattern as `<FILE>`. When the literal argument contains any of `*`, `?`, `[`, `{`, the CLI SHALL treat it as a `globset`-style pattern, walk the file system from the longest non-meta prefix of the pattern, and apply the operation to every matching file. When the argument contains none of those characters, behaviour SHALL be byte-identical to a single-file invocation (M2 contract preserved).

#### Scenario: Glob matches multiple files
- **WHEN** the user runs `dq set 'k8s/**/*.yaml' /spec/replicas 3 -i` against a directory tree with 50 matching YAML files
- **THEN** every matching file is processed, each is updated atomically with the new replicas value, and a summary line `Modified: 47, Skipped: 3 (already up to date), Failed: 0` is written to stdout

#### Scenario: Literal path is not glob-expanded
- **WHEN** the user runs `dq set k8s/deploy.yaml /spec/replicas 3 -i` (no glob metacharacters)
- **THEN** the operation runs in single-file mode with no summary, identical to M2

#### Scenario: Glob with no matches
- **WHEN** the user runs `dq set 'no-such/*.yaml' /x 1 -i`
- **THEN** the command exits with code 5 (`IO_ERROR`) and stderr names the pattern that matched nothing

### Requirement: `--continue-on-error` for partial-failure tolerance

When the `--continue-on-error` flag is set on a bulk write command, the driver SHALL NOT abort on the first failing file. It SHALL run the operation against every matched file, accumulate per-file results, write a summary to stdout, and return exit code 7 (`WRITE_FAILED`) if any file failed (and 0 if every file succeeded). Without `--continue-on-error`, the driver SHALL abort at the first failure and return the exit code matching that failure.

#### Scenario: Continue-on-error reports per-file failures and exits 7
- **WHEN** the user runs `dq set 'k8s/*.yaml' /spec/replicas 3 -i --continue-on-error` against 10 files where 2 are templated (no `--allow-templates`)
- **THEN** the 8 non-templated files are updated, the summary reports `Modified: 8, Failed: 2`, stderr lists the two failing files with their `TemplatedFile` errors, and the exit code is 7

#### Scenario: Without --continue-on-error the first failure aborts
- **WHEN** the user runs `dq set 'k8s/*.yaml' /spec/replicas 3 -i` against 10 files where the third one is templated
- **THEN** the first two files are updated, the third triggers `TemplatedFile` (exit 3), files 4-10 are NOT touched, and the exit code is 3 — matching the abort failure

### Requirement: `--parallel <N>` for bulk throughput

The `--parallel <N>` flag SHALL run up to N file operations concurrently via a `rayon` thread pool. `--parallel 0` SHALL use `rayon::current_num_threads()`. Default (without the flag) is `1` (sequential). Per-file atomic-write contract (M2) SHALL be preserved — each file still goes through `tempfile + persist` independently. Output ordering SHALL match the order of matched files regardless of execution order; the driver buffers per-file output and flushes serially at the end.

#### Scenario: Parallel run preserves per-file atomicity
- **WHEN** the user runs `dq set 'k8s/*.yaml' /spec/replicas 3 -i --parallel 4` and one file's parent directory becomes read-only mid-run
- **THEN** the file with the read-only parent fails with `WRITE_FAILED`, and every other matching file is either fully updated or untouched (no half-written file remains)

#### Scenario: Parallel output ordering matches file ordering
- **WHEN** the user runs `dq set 'k8s/*.yaml' /spec/replicas 3 --parallel 4 --diff` (diff mode in bulk)
- **THEN** the per-file diffs in stdout appear in the same order as the matched files (alphabetic by path), even though execution order may differ

### Requirement: `--check` mode — idempotency gate without writing

The `--check` flag SHALL turn every write command into a no-op idempotency check. The driver SHALL: (1) read source, (2) apply the transformation in memory, (3) compare result bytes to source bytes, (4) NEVER write to disk, (5) exit 0 if every matched file is byte-identical to its prospective output, exit 1 if any file would be modified. `--check` SHALL be mutually exclusive with `-i`, `--diff`, and `--backup`.

#### Scenario: Check mode exits 0 when no changes pending
- **WHEN** the user runs `dq set 'k8s/*.yaml' /spec/replicas 3 --check` and every file already has `spec.replicas: 3`
- **THEN** the exit code is 0, no file is modified, and stdout reports "0 files would be modified"

#### Scenario: Check mode exits 1 when at least one file would change
- **WHEN** the user runs `dq set 'k8s/*.yaml' /spec/replicas 3 --check` and 5 files have a different replicas value
- **THEN** the exit code is 1, no file is modified, and stdout lists the 5 paths that would change

#### Scenario: Check is mutually exclusive with -i / --diff
- **WHEN** the user runs `dq set f.yaml /x 1 --check -i`
- **THEN** clap exits with code 6 (`INVALID_INPUT`) and the error explains the conflict

### Requirement: Bulk summary reporter

A bulk run (more than one matched file) SHALL print a summary line on stdout after all per-file output. The format SHALL be `Modified: N, Skipped: M (already up to date), Failed: K`. Single-file invocations (one literal file or a glob matching exactly one file) SHALL NOT print the summary — preserving M2 byte-identical output.

#### Scenario: Summary is printed only in bulk mode
- **WHEN** the user runs `dq set 'k8s/*.yaml' /spec/replicas 3 -i` against 5 matching files
- **THEN** stdout ends with `Modified: 5, Skipped: 0, Failed: 0` exactly once, after any per-file output

#### Scenario: Single-file mode has no summary
- **WHEN** the user runs `dq set 'k8s/deploy-only.yaml' /spec/replicas 3 -i` (a glob matching exactly one file)
- **THEN** stdout matches the M2 single-file contract — no summary line

### Requirement: Empty-glob and bulk-fail exit-code mapping

Bulk-mode exit codes SHALL aggregate as follows: (a) every file succeeds → 0, (b) without `--continue-on-error`, abort on first error and return that error's exit code, (c) with `--continue-on-error`, return 7 (`WRITE_FAILED`) if any file failed and 0 otherwise, (d) `--check` with no changes pending → 0, (e) `--check` with changes pending → 1, (f) glob matches zero files → 5 (`IO_ERROR`).

#### Scenario: Mixed results with --continue-on-error map to 7
- **WHEN** any file in a `--continue-on-error` bulk run fails for any reason (templated, invalid pointer, write IO)
- **THEN** the process exits with code 7 regardless of the underlying per-file failure cause

#### Scenario: --check changes-pending maps to 1
- **WHEN** `--check` reports any prospective modification
- **THEN** the process exits with code 1
