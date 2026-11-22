# Structured errors: a caret under the offending byte, a JSON error object under `-F json`

**Status: in progress (2026-11-22).** Phase 1 (positions) is done; phases 2 to 4 are open. The owner accepted every recommendation; the decisions are at the end.

Agent-first error output was a founding goal of dq ([archive/dq-plan-2026-05.md](../archive/dq-plan-2026-05.md)): errors with line, column, span, snippet, suggestions, and a machine-readable form under `-F json`. The suggestions shipped (`did_you_mean`, the matched prefix). The caret and the JSON form did not. [roadmap.md](../roadmap.md) and [reference/cli.md](../reference/cli.md) list both as missing.

## Problem

What a user or an agent gets today, from the released `dq` 2026.47.3:

```
$ dq get -F json bad.json /a
parse error in bad.json: expected JSON value (object, array, scalar)
$ dq query .a. ok.json
parse error in ok.json: jq: expected key (parse error, got ``)
$ dq get missing.json /a
I/O error reading 'missing.json': No such file or directory (os error 2)

Caused by:
    No such file or directory (os error 2)
```

- **No position on most commands.** `Error::Parse` carries line, column, span and snippet, but `render_error` ([crates/dq-cli/src/lib.rs](../../crates/dq-cli/src/lib.rs)) prints only the message. Only `validate` shows them, as `key: value` lines, and `lint` through its diagnostics. `reference/cli.md` says "Parse errors carry the file, line, column, message and the offending source line"; the error does, the output does not.
- **No caret** anywhere.
- **`-F json` changes nothing** about errors except for `validate` (a diagnostics report on stderr) and `lint` (a diagnostics report on stdout). An agent that asks for JSON still has to scrape prose, and every error kind has a different shape.
- **Wrong attribution.** A jq compile error is turned into a `Parse` error of the *data* file (`jq_compile_to_parse` in [crates/dq-cli/src/commands/query.rs](../../crates/dq-cli/src/commands/query.rs)), and its column is 0-based while every other one is 1-based. `dq query .a. ok.json` blames `ok.json`.
- **Duplicated cause.** An I/O error prints its OS error twice.
- **Parse positions are uneven between formats**, so a caret drawn from them today would point at the wrong place. Audited with `Error::Parse` of every parser on one malformed file each (2026-11-22):

| format | line | column | span | snippet | message |
|---|---|---|---|---|---|
| JSON | right | bytes | exact | whole line | clean |
| JSONL | right | always 1 | `0..0` | the line | clean |
| YAML | right | characters, not bytes | `0..0` | empty | ends with "at line 4 column 4" |
| TOML | right | right | zero-width | the line | clean |
| XML | right | characters, not bytes | zero-width | ends with a literal `\n` | repeats the position |
| HCL | right | characters, not bytes | `0..0` | empty | a whole rendering with its own caret |
| CSV | right | 0 | `0..0` | empty | repeats the position |
| `.env` | right | 1 | `0..0` | the line | clean |
| Dockerfile | right | right (bytes) | exact | the line | clean |
| front matter | of the inner block, not the file | as YAML | `0..0` | empty | as YAML |
| non-UTF-8 input | 0 | 0 | `0..0` | empty | clean |

So the same bug class exists in three shapes: the column unit differs (YAML, XML and HCL count characters, JSON, TOML, Dockerfile and the lint diagnostics count bytes), the span is missing, and the message repeats the position.

## Goal

1. Every parse error of every format carries the same, correct position: 1-based line, 1-based byte column, a byte span, and the source line. One function computes them.
2. On the console, every error with a source position shows the line and a caret, on every command.
3. Under `-F json` (and `jsonl`), every failing command prints one JSON error object on stderr, with the same fields whatever the error kind. Exit codes do not change: they are a public contract.

Not a goal: coloured diagnostics in JSON, translated messages, SARIF for non-lint errors, HTML, `miette` as the error type (see [decisions.md](../decisions.md), 2026-05-03: it would force every library user of `dq-core` to depend on it).

## Design

### 1. Positions come from one place (dq-core)

New in [crates/dq-core/src/error.rs](../../crates/dq-core/src/error.rs):

```rust
impl Error {
    /// A parse error at byte `span` of `bytes`: line, column and the source line are derived here.
    pub fn parse_at(bytes: &[u8], span: Range<usize>, message: impl Into<String>) -> Self;
}
```

- `line` is 1-based. `col` is a 1-based **byte** column, the unit the docs already use for Dockerfile ("Columns count bytes, a leading BOM not included") and that lint diagnostics, JSON and TOML already report. The character columns of YAML, XML and HCL become byte columns (decision 6). A leading BOM is not counted.
- `span` is never `0..0` unless the error is at byte 0. A position with no known token is `offset..offset`.
- `snippet` is the source line, without its line break, whole. Windowing long lines is the renderer's job.
- The message carries no position of its own: the "at line 4 column 4" suffix of a YAML message, the prefix of an XML message and the record position of a CSV message go, as does the HCL parser's multi-line rendering (its last line, `expected ']'`, is the message). A YAML *context* position ("while parsing a flow sequence at line 3 column 8") stays: it names another place, the unclosed bracket.
- Line breaks: `\n`, `\r\n` and lone `\r` are all recognised, as the textual-edit code does (`LineBreak`). At end of file the position is one past the last byte, with an empty snippet line if the file ends with a line break.
- Invalid UTF-8 points at the first bad byte.
- Front matter positions are mapped to the outer file.

Every constructor of `Error::Parse` in the parsers moves to this helper. `parse_at_line_col` was planned for parsers that report a line and a character column; every parser turned out to offer a byte offset or something the helper must not share, so it was dropped. (The count at the time: 57 sites in 14 files.) `Error::Parse` itself keeps its fields, so `validate` and `lint` only get better values, not a new shape.

### 2. Console rendering (dq-cli)

New module `crates/dq-cli/src/output/error.rs`; `render_error` becomes a thin caller of it.

```
parse error in bad.json: expected JSON value (object, array, scalar)
  --> bad.json:2:14
   |
 2 |   "b": [1, 2,, 3]
   |              ^
```

- **The first line does not change**, so scripts that grep `parse error in …` keep working. What is new follows it.
- A span longer than one byte is underlined `^~~~` up to the end of the line.
- **Control characters.** The source line goes through `escape_control`, as all terminal output does ([crates/dq-cli/src/output/console.rs](../../crates/dq-cli/src/output/console.rs)): a file cannot rewrite the terminal. Escaping makes the line longer than its bytes (ESC prints as `\x1b`), so the caret position is computed by the same pass that escapes: it walks the line character by character, appends the escaped form, and records the display column when it reaches the byte the error is at.
- **Width.** Tabs print as four spaces. Wide characters (CJK, emoji) take two columns. This needs the `unicode-width` crate (small, no dependencies, MIT/Apache-2.0); it is not in `Cargo.lock` today.
- **Long lines.** A line wider than 120 columns is windowed around the caret, with `…` on the cut side. A 10 MB one-line JSON file does not print 10 MB.
- **Other errors.** `templated_file` has a line and a snippet, so it gets the same block without a caret column. A jq compile error (section 4) shows the expression with a caret. Path errors keep their block and gain nothing. Everything else prints its message.
- **Colour.** The caret and the `-->` line are dimmed only when `resolve_color` says so, never in JSON.
- **`Caused by:`** is dropped when its text is already part of the message above it.

`validate` on the console prints its position as `kind:` / `file:` / `line:` lines today. It prints the same block as every other command (decision 2).

### 3. The JSON error object

Under `-F json` and `-F jsonl`, a command that fails prints **one line** of JSON on stderr, after any log lines, and nothing else from `render_error`:

```json
{"error":{"kind":"parse","exit_code":3,"message":"expected JSON value (object, array, scalar)","file":"bad.json","line":2,"col":14,"span":{"start":22,"end":23},"snippet":"  \"b\": [1, 2,, 3]","snippet_col":14}}
{"error":{"kind":"path","exit_code":2,"message":"path '/server/prot' not found (matched up to '/server')","pointer":"/server/prot","matched_prefix":"/server","did_you_mean":["port"]}}
{"error":{"kind":"io","exit_code":5,"message":"I/O error reading 'missing.json': No such file or directory (os error 2)","file":"missing.json"}}
```

| field | when |
|---|---|
| `kind` | always: the error's `kind_name()`, or a CLI marker's name (below) |
| `exit_code` | always: the code the process exits with |
| `message` | always: what the console prints as the first line, without the position |
| `file` | the file is known; absent for stdin |
| `line`, `col` | a position is known; 1-based |
| `span` | a byte range is known: `{"start", "end"}`, 0-based, end exclusive |
| `snippet`, `snippet_col` | the source line, windowed to 160 characters around the position, and the 1-based column of the position inside it |
| `pointer`, `matched_prefix`, `did_you_mean` | `kind` is `path` |
| `hint` | `templated_file` |
| `pointer`, `expected`, `actual` | `patch_test_failed`; values as text |
| `expression`, `offset` | a jq compile error (section 4) |
| `files` | a bulk run (below) |
| `causes` | the error has causes that are not already in `message`: an array of strings |

- **Raw strings.** JSON carries the text as it is, unescaped apart from JSON's own escapes, the way machine-readable output already does (`reference/cli.md`). Escaping is for terminals.
- **One object, last line.** The `tracing` log goes to stderr as text, so a warning can precede the object; consumers read the last line. `-F json` does not change the log format.
- **`kind` is stable, `message` is not.** Kinds are the names the exit-code mapping already uses, plus the CLI markers: `usage`, `invalid_input`, `wrong_type`, `doc_out_of_range`, `check_pending`, `bulk_partial_failure`, `lint_fail`, `lint_warn_strict`, `validate_fail`, `checksum`, `jq_compile`, `other`. The table moves into `reference/cli.md` next to the exit codes.
- **Silent exits stay silent.** `exists` answering "no" prints nothing (exit 1).
- **`lint`, `check`, `fix`, `validate`.** They keep their reports (`{"diagnostics": […]}` on stdout or stderr). The error object comes in addition, for the reason the command failed: `lint_fail` with the count, or the parse error of the file that stopped it. `validate` renders its own report today and prints no second error; that stays, since the report already says everything.
- **Bulk runs** with `--continue-on-error`: `kind` is `bulk_partial_failure`, and `files` lists each failed file with its own error object (same fields, no `exit_code`). The text summary on stdout does not change.
- **Usage errors.** The ones handlers raise (`--doc` refusals, stdin without `--input-format`) become `kind: "usage"`, exit 2. The ones clap raises while parsing exit before `-F` is known, so the raw arguments are scanned for it (decision 3).

### 4. jq compile errors point at the expression

`JqError::Compile` already carries the byte `position` and a `snippet`. They map to `kind: "jq_compile"` (exit 3, as now), with `expression` and `offset` (0-based bytes) in JSON, and on the console:

```
jq compile error: expected key
  --> expression:1:4
   |
 1 | .a.
   |    ^
```

`query`, `set --jq` and rule compile errors all use it, so the data file is never blamed for the expression. `dq query .a. ok.json` stops saying "in ok.json".

## Phases

Each phase is one PR, releasable on its own.

1. **Positions (dq-core): done.** Also in this phase: the second line `  --> FILE:LINE:COL` on the console and the place in the bulk summary, because taking the position out of the messages would otherwise have left the console without one. `Error::parse_at`; migrate the parsers; byte columns in YAML; outer positions for front matter; line and column for CSV, JSONL, `.env`; first bad byte for invalid UTF-8; messages without positions; the XML snippet's stray `\n`. corpus of malformed files with expected line, column, span and snippet (`crates/dq-core/tests/parse_error_positions.rs`), a property test that line and column agree with a naive scan for random offsets in texts with `\n`, `\r\n`, lone `\r`, a BOM and multi-byte characters.
2. **Console (dq-cli).** `output/error.rs`; caret block; `Caused by:` de-duplication; `unicode-width`; insta snapshots per format, plus: tab, CJK, emoji, ESC in the line, 3,000-character line, position at end of file, CRLF, empty file.
3. **JSON (dq-cli).** `render_error` takes the output format; the JSON object; jq compile errors; bulk files; the table of kinds. Tests: every kind's object is valid JSON, its `exit_code` equals the process's, the object is the last line of stderr; a source line with an ESC byte reaches JSON unescaped and the terminal escaped.
4. **Docs and skill.** `reference/cli.md` (errors and the kind table), README, `skills/dq/SKILL.md`, the `lint` / SARIF claim in the README that a file that does not parse produces no SARIF (it does: `lint.parse`), CHANGELOG "Changed", a dated entry in `decisions.md`.

## Compatibility

- **Exit codes:** unchanged.
- **`-F json` text on stderr:** becomes a JSON line. Scripts that grep prose while asking for JSON change. CHANGELOG "Changed".
- **Console text:** the first line of each error is unchanged; new lines follow it. `validate`'s console layout changes.
- **Positions:** `col` of a YAML, XML or HCL error changes on lines with multi-byte characters before it; CSV, JSONL and front matter errors get real positions. `validate -F json` and `lint` carry them.
- **Messages:** position suffixes leave the YAML, XML and CSV messages; the HCL message is one line.

## Alternatives rejected

- **`miette` or `ariadne`.** Rejected for `miette` in 2026-05; a renderer crate would still need dq's own escaping and width handling, and dq draws one kind of block.
- **Let the CLI re-read the file to find the line.** stdin is gone by then, and a file can change between the parse and the render. The snippet travels with the error.
- **JSON errors for every machine format** (`yaml`, `toml`, `toon`, `sarif`…). Only JSON has a consumer that wants it today; the object would otherwise need a schema per format.
- **JSON logs under `-F json`.** Separate feature; the "last line" rule covers the need.
- **Characters as the column unit.** Bytes are the recommendation because they change the fewest reporters (decision 6). SARIF's `columnKind` is a related open issue: it knows only UTF-16 units and code points, so byte columns on a non-ASCII line are wrong in Code Scanning today. Out of scope here.

## Decisions (owner, 2026-11-22)

1. **Shape:** the `{"error": {…}}` wrapper, every field optional but `kind`, `exit_code` and `message`. A consumer can tell an error object from a command's own JSON.
2. **`validate` on the console** prints the caret block like every other command; `-F json` is for machines. Two snapshots pin the old layout and change.
3. **Usage errors clap raises while parsing** become JSON under `-F json`: the raw arguments are scanned for `-F json` / `--format json` before the error is rendered. A flag value that happens to look like one only changes the format of an error.
4. **`-F jsonl`** turns errors into JSON too.
5. **`unicode-width`** becomes a dependency of dq-cli, so the caret is right on lines with wide characters.
6. **Column unit: bytes**, for every format. Lint diagnostics, JSON, TOML and the Dockerfile docs already count bytes, and `span` is bytes. The character columns of YAML, XML and HCL parse errors become byte columns. SARIF's `columnKind` stays open.
