# Bulk runs: globs, `--check`, `--continue-on-error`, `--parallel`

The write commands `set`, `del`, `patch`, `merge`, `fmt`, `fix` and
`convert -i` accept a glob instead of a file and then run once per matched
file through one driver, `crates/dq-cli/src/bulk.rs` (`run_per_file`).
`lint` and `check` take several files or globs too, through the lint
pipeline. Read commands and `convert` to stdout take a single file.

A single file — a literal path, or a glob that matches exactly one file —
behaves byte for byte like a plain single-file invocation: no per-file
markers, no summary.

## Glob expansion

- The argument is a glob when it contains `*`, `?`, `[` or `{` and nothing
  exists at that literal path: a shell-expanded `a[1].yaml` is a plain path.
  A pattern for a file that does not exist yet is still a glob; escape the
  metacharacter (`a\[1\].yaml`) to name such a file literally.
- `*` and `?` match within one path component and never match `/`: `k8s/*.yaml`
  matches `k8s/a.yaml`, not `k8s/sub/x.yaml`. `**` crosses directories:
  `k8s/**/*.yaml` matches both (and `k8s/a.yaml` itself). `[abc]` classes
  and `{a,b}` alternation work as usual (`globset`). Quote the pattern so the
  shell does not expand it.
- dq walks the file tree from the longest leading part of the pattern that
  has no metacharacter (`k8s/**/*.yaml` walks `k8s/`) and keeps the files the
  pattern matches; without `**`, the walk goes no deeper than the pattern
  does. A pattern with no such leading part (`2-*.yaml`, `{a,b}/*.yaml`) is
  matched in the current directory, and the files are named as the pattern
  spells them (`2-a.yaml`, not `./2-a.yaml`). Results are sorted, so output
  order is stable.
- A walk root that does not exist, or a glob matching no file, is exit 5
  (exit 6 for `lint`, `check` and `fix`, where it is a caller mistake).

## Per-file output and summary

In stdout mode each file's output is preceded by `=== <path> ===`; in
`--diff` mode each diff is labelled the same way. After a run over several
files the driver prints:

```
Modified: N, Skipped: M, Failed: K
```

`Skipped` counts files that had nothing to do (no document N for `--doc N`,
noted on stderr). Observed on this branch: with `-i`, files whose bytes did
not change are still counted as Modified; see [../roadmap.md](../roadmap.md).

## `--check`

The idempotency gate for pre-commit hooks and CI: each file is read and
transformed in memory, never written. Files that would change are listed as
`would modify: <path>` followed by `N file(s) would be modified`; exit 1 if
any would change, otherwise `0 files would be modified` and exit 0. `--check`
excludes `-i`, `--diff` and `--backup`.

## Failure handling

- Without `--continue-on-error`, the run stops at the first failing file and
  exits with that failure's code; files before it are already written, files
  after it are untouched.
- With `--continue-on-error`, every file is attempted; failures are listed
  under the summary (`- path: error`) and the run exits 7 if any file failed,
  whatever the cause (parse error, missing pointer, templated file, write
  error). On a single file the flag changes nothing.
- There is no rollback across files: each file is atomic on its own (temp file
  + rename), the run as a whole is not.

## `--parallel <N>`

Runs up to N files at once on a `rayon` pool; `0` means the number of CPUs;
default 1. Each file still goes through its own atomic write. Per-file output
is buffered and flushed in matched-file order, so output is identical to a
sequential run. A compiled jq expression (`set --jq`) or rule set (`fix`) is
built once and shared by the workers. `--parallel` on a single file runs
sequentially.

## Exit codes of a bulk run

| Outcome | Exit |
|---|---|
| every file succeeded | 0 |
| first failure, no `--continue-on-error` | that failure's code |
| any failure with `--continue-on-error` | 7 |
| `--check`, nothing would change | 0 |
| `--check`, something would change | 1 |
| glob matched nothing / walk root missing | 5 |
| `--doc N` held by no file | 2 |
