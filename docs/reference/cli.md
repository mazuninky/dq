# CLI shell: commands, global flags, output, exit codes

This page describes the frame every `dq` subcommand runs in: how the binary is
put together, the global flags, how output is rendered, how errors surface and
which exit code each failure gets. What the individual commands do is in the
other reference pages (linked from the command table below).

## Commands

| Command | What it does | Reference |
|---|---|---|
| `get`, `exists`, `keys`, `values`, `len`, `type`, `paths` | Navigate a document with a JSON Pointer | [read-commands.md](read-commands.md) |
| `select` | RFC 9535 JSONPath query | [read-commands.md](read-commands.md) |
| `query` | jq expression (embedded jaq) | [read-commands.md](read-commands.md), [jq.md](jq.md) |
| `convert` | Re-emit in another format (`-F`), optionally in place | [read-commands.md](read-commands.md), [write-commands.md](write-commands.md) |
| `validate` | Parse and report parse errors | [read-commands.md](read-commands.md) |
| `diff` | Structural diff of two files (JSON Patch or unified) | [write-commands.md](write-commands.md) |
| `set`, `del` | Point edits (textual and comment-preserving on YAML, JSON, TOML; a rewrite through the writer on HCL, XML, INI, dotenv, CSV/TSV) | [write-commands.md](write-commands.md) |
| `set --jq` | Whole-document jq transform (re-emit) | [write-commands.md](write-commands.md) |
| `patch`, `merge` | RFC 6902 JSON Patch, RFC 7396 Merge Patch | [write-commands.md](write-commands.md) |
| `fmt` | Canonicalise through the format's writer | [fmt.md](fmt.md) |
| `lint`, `check`, `test`, `explain`, `rules list`, `rules add` | Lint engine | [lint-engine.md](lint-engine.md) |
| `fix` | Apply rule fixes | [lint-engine.md](lint-engine.md) |
| `completions <shell>` | Shell completion script on stdout (bash, zsh, fish, powershell, elvish) | below |
| `man [PAGE]` | troff man page on stdout (`dq man`, `dq man get`) | below |
| `self check`, `self update [--to VER] [--dry-run]` | Check for / install a newer release (SHA-256 checked against `dq-checksums.txt`) | [distribution.md](distribution.md) |
| `generate-docs --output-dir DIR` | Hidden; writes completions and man pages for packaging | [distribution.md](distribution.md) |

`dq config` and `dq init` from the original plan do not exist; see
[../roadmap.md](../roadmap.md).

## How the binary is built

The structure is a convention the code keeps on purpose, so it is worth
knowing before adding a command:

- `crates/dq-cli/src/main.rs` does only five things: restore `SIGPIPE` to
  `SIG_DFL` on Unix (so `dq paths big.yaml | head` ends quietly instead of
  panicking on a broken pipe), parse arguments with clap, initialise
  `tracing`, wire dependencies (colour decision, locked stdout/stderr), and
  call `dq::run`, mapping its error to an exit code. All command logic lives
  in `crates/dq-cli/src/commands/`.
- Handlers never print directly. They receive a `&dyn Reporter` and a
  `&mut dyn Write` and return a value; the reporter renders it. This keeps
  handlers testable against a `Vec<u8>`.
- Diagnostics (logs) go through `tracing` only, to stderr. stdout carries the
  command's output and nothing else. Libraries never `println!`/`eprintln!`.
- Errors are typed per crate with `thiserror` (`dq_core::Error`,
  `dq_exec::ExecError`, `dq_plugin::PluginError`, `dq_transform::JqError`);
  handlers use `anyhow` and the top level downcasts to pick the exit code.
  Every error kind has a stable `kind_name()` string that drives the mapping.
- The CLI is non-interactive: no prompts, spinners or progress bars. Output
  is byte-identical in a terminal and through a pipe, except for ANSI colour.

## Global flags

All of these are declared `global = true`, so clap accepts them on every
subcommand. A command that cannot use one either ignores it (where that is
harmless) or refuses it, as noted.

| Flag | Meaning |
|---|---|
| `-F, --format <FMT>` | Output format, on every command; never the format a file is read in. See [Input and output formats](#input-and-output-formats). Default `console`. |
| `--input-format <FMT>` | The format to read files, globs and stdin (`-`) in, instead of detecting it from the file name. Needed for stdin. No short form (`-i` is in-place). |
| `-v, --verbose` (repeatable) | Log level: default WARN, `-v` INFO, `-vv` DEBUG, `-vvv` TRACE. `RUST_LOG` overrides. |
| `-q, --quiet` | Log level ERROR. Conflicts with `-v` (clap usage error). |
| `--no-color` | No ANSI colour. |
| `--doc <N\|all>` | Pick a document of a multi-document YAML stream; see [read-commands.md](read-commands.md#multi-document-yaml-and---doc). |
| `-i, --in-place` | Write the result back atomically (write commands only). |
| `--diff` | Print a unified diff instead of writing. |
| `--backup` | With `-i`, copy the original to `<file>.bak` first. |
| `--check` | Write nothing; exit 1 if any file would change. |
| `--continue-on-error` | Bulk runs: keep going past failing files, exit 7 at the end if any failed. |
| `--parallel <N>` | Bulk runs: N worker threads (`0` = number of CPUs). |
| `--sort-keys` | Sort map keys on re-emit paths; no-op for textual edits and reads. |
| `--indent <N>` | Indent width for JSON / JSONL re-emit; ignored elsewhere. |
| `--allow-templates`, `--raw-template-strings` | Template guard escape hatches; mutually exclusive. See [template-guard.md](template-guard.md). |
| `--strict` | `lint` / `check`: warn-severity findings fail the run (exit 1). Ignored elsewhere. |
| `--plugins <DIR>` | Load `*.wasm` lint/fix plugins from DIR. See [../plugin-abi.md](../plugin-abi.md). |

Write-mode flags on a read-only command (`get`, `exists`, `keys`, `values`,
`len`, `type`, `paths`, `select`, `query`, `diff`, `completions`, `man`,
`self`) are refused with exit 6 and a message naming every offending flag.
`validate` accepts `--check` as a no-op so it can sit in the same pre-commit
chain as `fmt --check`. `--sort-keys`, `--indent` and `--strict` are tolerated
on reads because people put them in shell aliases.

Write-mode combinations refused by the handlers (exit 6):

- `-i` with `--diff`, `-i` with `--check`, `--diff` with `--check`;
- `--backup` without `-i`, `--backup` with `--check`.

Refused as usage errors (exit 2), before any file is written:

- a `-F` naming another format than the file's on `set`, `del`, `patch`,
  `merge`, `fmt` and `fix` (see below);
- `-i` or `--check` on stdin (`-`), which has no file to write back to or
  compare with.

`--parallel N` on a single file is accepted and simply runs sequentially (the
original design wanted it refused; the code downgrades silently).

Environment variables honoured: `RUST_LOG`, `NO_COLOR`, `CLICOLOR_FORCE`, and
`GITHUB_TOKEN` for `self check` / `self update` (rate limits). The `DQ_*`
overrides sketched in the original plan (`DQ_FORMAT`, `DQ_IN_PLACE`,
`DQ_NO_COLOR`, `DQ_CONFIG`) are not implemented.

`--no-pager` is accepted for compatibility and does nothing: dq has no pager.
It is hidden from `--help`.

### Input and output formats

`-F` is the output format on every command: the reporter a read command
prints through, the format `convert` writes, the diagnostics format of
`lint`, `check` and `validate`. It never picks the parser, so
`dq paths config.yaml -F json` reads YAML and prints JSON.

The format a file is read in comes from its extension or name (see
[formats.md](formats.md#detection)); `--input-format <FMT>` overrides that on
every command that reads data — a file, every match of a glob, and stdin. Its
values are the parser names: `json`, `yaml`, `toml`, `jsonl`, `hcl`, `ini`,
`dotenv` (alias `dot-env`), `csv`, `tsv`, `dockerfile`, `ignore-list`,
`markdown`, `frontmatter`, `xml`; anything else is a usage error (exit 2).
`diff` reads both files in it.

Stdin (`-` as the file) has no name to detect from, so every command reading
it needs `--input-format`; without it the command fails with a usage error
naming the flag (exit 2). That includes `lint -`, `check -` and `fix -`, whose
`-F` picks the reporter: `dq lint - --input-format yaml -F sarif < f.yaml`.
Stdin can be read once, so a command given `-` for more than one input —
`diff - -`, `patch - -`, `merge - -`, `set - /a -` (or `@-`, `--value-from -`),
`-` twice among the files of `lint`, `check` or `fix` — is refused with a
usage error naming them (exit 2) before anything is read.

`set`, `del`, `patch`, `merge`, `fmt` and `fix` keep a file in its own format
— the one it was read in — whether they write it back (`-i`), print it or
print a diff. A `-F` naming another format is refused with a usage error
(exit 2) that points to `--input-format` and `dq convert`; the same format is
accepted, so `dq set -F yaml -i config.yaml /a 1` works as it always did.
`convert -i` is the exception: it writes a sibling file in the `-F` format.

`exists`, `test`, `completions`, `man`, `self` and `rules add` print no
formatted value and do not use `-F`.

### Colour

Colour on stdout is decided in this order: `--no-color` → `NO_COLOR` set (any
value) → `CLICOLOR_FORCE` set → stdout is a terminal. Logs on stderr use the
same order against stderr. The decision is passed down as a `use_color: bool`
parameter; nothing mutates the environment.

## Output formats (`-F`)

| Value | Kind | Notes |
|---|---|---|
| `console` (default) | output | Human-readable; collections print in compact YAML-flow style. |
| `json` | file format | Pretty JSON; `--indent` applies on re-emit paths. |
| `yaml`, `toml`, `jsonl` | file format | |
| `toon` | output only | Token-Oriented Object Notation via the `toon-format` crate. |
| `hcl`, `ini`, `dot-env`, `csv`, `tsv`, `frontmatter`, `markdown`, `xml` | file format | `convert` targets; see [formats.md](formats.md). |
| `sarif` | output only | SARIF 2.1.0, one `run` with `dq` as the driver. |
| `junit` | output only | JUnit XML, one `<testcase>` per diagnostic. |
| `tap` | output only | TAP version 13. |

A "file format" can also be read, but only `--input-format` reads it: `-F`
names the output alone.

There is no `text` value; the default human format is `console`.

`console` never writes a control character raw: every C0 control (tab and
line feed included), DEL and every C1 control (U+0080–U+009F, among them
U+009B CSI, which starts a terminal escape sequence on its own, and U+0085
NEL) in a key, a value or a `validate` snippet prints as `\xNN`, `NN` being
the code point in lowercase hex — `\x1b`, `\x0a`, `\x9b` — as YAML spells
it. The invisible direction controls (U+200E LRM, U+200F RLM, U+202A–U+202E,
U+2066–U+2069 — the characters of a Trojan Source attack, which reorder the
text a terminal shows) and the line and paragraph separators U+2028 and
U+2029 print as `\uNNNN` in four lowercase hex digits — `\u202e`, `\u2028`
— as a YAML double-quoted string reads them. Other non-ASCII text (`é`,
NBSP, ZWJ, emoji) prints as is.

`sarif`, `junit` and `tap` only render the diagnostics shape produced by
`lint`, `check`, `fix` and `validate`:

```json
{ "diagnostics": [ { "path": "deploy.yaml", "line": 12, "col": 7,
                     "severity": "error", "message": "…",
                     "rule_id": "k8s.no-latest-tag", "references": ["…"] } ] }
```

Selecting one of them for any other command is refused with exit 6 ("SARIF
reporter expects an object with a `diagnostics` array…"). SARIF maps severity
`error → error`, `warn → warning`, `info → note` and fills
`physicalLocation` from `path` / `line` / `col`.

Every SARIF result names its rule: `ruleId` is the diagnostic's `rule_id`
and `ruleIndex` its position in `tool.driver.rules`. GitHub Code Scanning
groups alerts by `ruleId`, so the ids are stable: a rule's `id:`
(`k8s.no-latest-tag`, a `.dq/rules/` rule's own id), a plugin's id, a
composite's `<outer-id>.parse-failed`, and `validate.parse` for a file
`validate` cannot parse. `tool.driver.rules` describes each rule the results
name, once, in order of first appearance (empty when there are no results):

```json
{ "id": "dockerfile.has-healthcheck",
  "shortDescription": { "text": "Long-running Docker images benefit from a HEALTHCHECK directive so Docker / Kubernetes can detect frozen processes." },
  "fullDescription": { "text": "Long-running Docker images benefit …\n… worth confirming intent." },
  "helpUri": "https://docs.docker.com/engine/reference/builder/#healthcheck" }
```

`shortDescription` is the first sentence of the rule's `description` (its
first paragraph, lines joined); `fullDescription` is the whole description,
present when it says more; `helpUri` is the first `http(s)://` entry of
`references`. A rule dq has no description for — a plugin's, a composite's
`parse-failed`, a composite's nested rule — is described by `id` alone.

`dq validate` writes its diagnostics (in whichever format `-F` selects) to
**stderr**, not stdout, which is why CI examples redirect `2>`.

## Errors

Errors are printed on stderr. A path error names the pointer, the deepest
prefix that exists and up to three close sibling keys:

```
path '/server/prot' not found (matched up to '/server')
  matched prefix: /server
  did_you_mean: port
  pointer: /server/prot
```

Parse errors carry the file, line, column, message and the offending source
line. clap usage errors are rendered the way clap renders them (`error: …`,
usage line, `--help` hint), including the usage errors raised by handlers
(`--doc` refusals, stdin without `--input-format`, a `-F` a write command
cannot keep a file in).

Not implemented yet, although the original design asked for them: a caret
under the offending byte, and a JSON error object on stderr under `-F json`
(today `-F json` changes nothing about error rendering, except for
`validate`, whose diagnostics go through the reporter). Both are on the
[roadmap](../roadmap.md).

## Exit codes

The numbers are a public contract: new codes may be added, existing ones are
never renumbered, because CI scripts and agents branch on them. They are
defined as named constants in `crates/dq-cli/src/exit_code.rs`.

| Code | Name | Meaning |
|---|---|---|
| 0 | `SUCCESS` | Done. |
| 1 | `GENERIC` | Unclassified failure; `exists` answering "no"; `--check` with changes pending; `lint --strict` with warnings; a failed `patch` `test` op; a jq runtime error; a value the output format cannot hold (NaN or infinity in JSON or HCL, a number HCL/TOON cannot represent); `keys` / `values` / `len` on a node of the wrong type; a composite-rule extract error. |
| 2 | `NOT_FOUND` | A JSON Pointer that does not resolve; a `--doc N` the file does not have (or no file of the run has); **any clap usage error** (unknown flag, conflicting flags, invalid value, `--doc` on a command that cannot honour it, stdin without `--input-format`, a `-F` a write command cannot keep the file in, `-i` or `--check` on stdin, `-` for more than one input). |
| 3 | `PARSE_ERROR` | The input does not parse; a templated file without an escape hatch; a jq compile error; an invalid rule file (bad YAML, unknown field, bad `check`, schema compile failure, unsafe `schema_file`); a failing `fix` script; a plugin with the wrong ABI major or a malformed fix. |
| 4 | `VALIDATE_FAIL` | `validate` found a parse error; `lint` / `check` found an error-severity diagnostic; a plugin trapped, ran out of fuel or memory, or failed to load; `self update` refused a downloaded archive at its SHA-256 check. |
| 5 | `IO_ERROR` | Read-side I/O (missing file, unreadable file, a glob root that does not exist or a glob that matched nothing); network errors in `self check` / `self update` (incl. downloading `dq-checksums.txt`). |
| 6 | `INVALID_INPUT` | A flag combination a handler refuses; write flags on a read command; unsupported format or file extension; a reporter that does not fit the command; unknown rule id or `@std` namespace; bad fixture file; `--plugins` without the `plugins` feature; `dq merge` on a stream; `self update` into a path the user cannot write. |
| 7 | `WRITE_FAILED` | Writing failed (temp file, rename, backup); the format takes no point edits (`set` on JSON Lines, Markdown, Dockerfile, …); a textual edit refused because the file would not read back as the change (`edit_rejected`); a bulk run with `--continue-on-error` where any file failed; `self update` could not replace the binary. |

The split between 2 and 6 follows who raises the error: anything clap rejects
while parsing (and the usage errors handlers build as clap errors: `--doc`
refusals, stdin without `--input-format`, a `-F` a write command cannot keep
a file in, `-i` or `--check` on stdin, `-` for more than one input) exits 2, clap's own code; validation a handler performs after parsing exits 6. Older design
documents said clap errors exit 6; that was never true once clap's own exit
path was used.

How error kinds map (from `exit_code_for_error`):

- `dq_core::Error`: `io` → 5; `write_io`, `write_unavailable`,
  `edit_rejected` → 7; `parse`, `templated_file` → 3; `path` → 2;
  `unsupported_format` → 6; `format` → 1; `patch_test_failed` → 1.
- `dq_exec::ExecError`: `parse`, `rule_compile`, `glob_compile`,
  `check_mutually_exclusive`, `check_missing`, `composite_incomplete`,
  `schema_compile`, `schema_file_absolute_path`,
  `schema_file_escapes_rule_dir`, `fix_apply` → 3; `unknown_rule`,
  `test_fixture` → 6; `io` → 5; `composite_extract_not_array`,
  `composite_extract_malformed`, `composite_extract_unknown_format`,
  `composite_depth_exceeded` → 1.
- `dq_plugin::PluginError`: see [../plugin-abi.md](../plugin-abi.md).
- CLI markers: lint error findings → 4, `--strict` warnings → 1, `--check`
  pending → 1, bulk partial failure → 7, `--doc` out of range → 2.

## Completions and man pages

`dq completions <shell>` prints a completion script (bash, zsh, fish,
powershell, elvish) to stdout; an unknown shell is a clap error (exit 2).
`dq man` prints the top-level `dq(1)` page and `dq man <command>` the page of
one command; an unknown name exits 6. The hidden `dq generate-docs
--output-dir DIR` writes `completions/` and `man/` trees for packaging; the
release workflow uses it.
