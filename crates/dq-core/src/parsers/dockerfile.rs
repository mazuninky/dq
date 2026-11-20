//! Dockerfile / Containerfile read-only parser.
//!
//! Top-level shape: `Array<Map { instruction: String, arguments, line: Int }>`
//! — one record per instruction, in source order. Comments, empty lines and
//! parser directives are not emitted.
//!
//! The parser follows the Dockerfile reference
//! (<https://docs.docker.com/reference/dockerfile/>) and the BuildKit
//! frontend it describes:
//!
//! - Parser directives (`# syntax=`, `# escape=`, `# check=`) are read from
//!   the comment lines at the top of the file, until the first line that is
//!   not one. `escape` switches the escape / line-continuation character
//!   between `\` and `` ` ``. A directive given twice is an error.
//! - An instruction line that ends with the escape character (optionally
//!   followed by blanks) continues on the next line; the escape character is
//!   removed and the lines are joined as written. Comment lines and empty
//!   lines inside a continuation are dropped.
//! - Leading whitespace before an instruction or a comment is allowed.
//! - Keywords are case-insensitive; `instruction` carries the upper-cased
//!   keyword.
//! - `RUN`, `COPY`, `ADD` (also as an `ONBUILD` trigger) accept heredocs:
//!   `<<EOF`, `<<-EOF`, `<<"EOF"`, `<<'EOF'`, several per line. The body and
//!   the terminator lines are appended to `arguments` verbatim, one `\n`
//!   before each line.
//!
//! ## `arguments`
//!
//! - Shell form: the argument text as written (flags such as `--chown=1:1`
//!   included, continuations joined) as a **string**.
//! - JSON form (`RUN`/`CMD`/`ENTRYPOINT`/`SHELL` exec form, `COPY`/`ADD`/
//!   `VOLUME` JSON form): an **array of strings** with the exact elements.
//!   Detection follows BuildKit: the argument text must start with `[` and be
//!   a valid JSON array; an array with a non-string element is an error, any
//!   other text is shell form. Flags before the array keep the shell-form
//!   string (`RUN --mount=… ["sh"]` stays a string).
//! - `ENV` / `LABEL`: a map of name to value, with shell quotes and escapes
//!   removed (no variable expansion). The legacy `ENV NAME value…` form maps
//!   the name to the rest of the line. Leading `--flag` words are skipped;
//!   with `--unset` the names map to `null`. Any other flag is accepted and
//!   ignored, which is more lenient than stable Docker (it rejects them).
//! - `ARG`: a map of name to default value; a name without a default maps to
//!   `null`.
//!
//! ## `line`
//!
//! The 1-based source line where the instruction starts (the first line of a
//! continuation, not a comment above it).
//!
//! ## Positions
//!
//! The document keeps the source text (a UTF-8 BOM removed) and a provenance
//! entry with a span for every node of every instruction, so a diagnostic's
//! pointer resolves to a `line:col`:
//!
//! - `/N`, `/N/instruction` and `/N/line` resolve to the instruction keyword
//!   on the line where the instruction starts.
//! - `/N/arguments` and every node under it (an `ENV` name, an exec-form
//!   element) resolve to the first argument character on that line, or to
//!   the keyword when the arguments begin on a continuation line.
//!
//! The document root has no entry: a finding about the whole file stays at
//! the top of the file.
//!
//! ## Errors
//!
//! An unknown keyword, a keyword that requires arguments given none, an
//! unterminated heredoc, an invalid parser directive or a malformed
//! `ENV`/`LABEL`/`SHELL` is an [`Error::Parse`] carrying the line where the
//! instruction starts. A file with no instructions parses to an empty array.
//!
//! Per design D6, this format is **read-only**: `Format::write` always
//! returns `Error::Format { format: "dockerfile", message: contains
//! "read-only" }`. `OutputFormat::Dockerfile` does not exist on the CLI side
//! so `dq convert -F dockerfile` is rejected at the clap layer.

use std::io::Write;
use std::ops::Range;

use indexmap::IndexMap;

use crate::Result;
use crate::document::spans::{SpanContext, ValueSpan};
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;
use crate::ir::{Provenance, ProvenanceMap};
use crate::pointer::{Pointer, Segment};

/// Dockerfile format implementation (read-only).
#[derive(Debug, Clone, Copy)]
pub struct Dockerfile;

impl Format for Dockerfile {
    fn name(&self) -> &'static str {
        "dockerfile"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["dockerfile", "containerfile"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in Dockerfile input: {e}"),
        })?;
        let instructions = parse_instructions(text)?;
        // The document keeps the text without a BOM, so a column on the
        // first line counts from the character after it; the instruction
        // offsets (and the parse errors' spans) count from the file start.
        let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
        let mut provenance = ProvenanceMap::new();
        let mut rows = Vec::with_capacity(instructions.len());
        for (index, instr) in instructions.into_iter().enumerate() {
            record_provenance(&mut provenance, index, &instr, bom);
            let mut row: IndexMap<String, Value> = IndexMap::with_capacity(3);
            row.insert("instruction".into(), Value::String(instr.keyword));
            row.insert("arguments".into(), instr.arguments);
            row.insert("line".into(), Value::Int(i64::from(instr.line)));
            rows.push(Value::Map(row));
        }
        Ok(Document::with_value_bytes_and_provenance(
            Value::Array(rows),
            text.as_bytes()[bom..].to_vec(),
            FormatTag::Dockerfile,
            provenance,
        ))
    }

    fn write(&self, _doc: &Document, _w: &mut dyn Write) -> Result<()> {
        Err(Error::Format {
            format: "dockerfile",
            message: "Dockerfile is read-only in M5".to_owned(),
        })
    }
}

/// Every instruction keyword the Dockerfile reference defines.
const KEYWORDS: &[&str] = &[
    "ADD",
    "ARG",
    "CMD",
    "COPY",
    "ENTRYPOINT",
    "ENV",
    "EXPOSE",
    "FROM",
    "HEALTHCHECK",
    "LABEL",
    "MAINTAINER",
    "ONBUILD",
    "RUN",
    "SHELL",
    "STOPSIGNAL",
    "USER",
    "VOLUME",
    "WORKDIR",
];

/// Keywords BuildKit accepts with no arguments at all.
const ARGLESS_KEYWORDS: &[&str] = &["RUN", "CMD", "ENTRYPOINT"];

/// Keywords whose arguments may open a heredoc.
const HEREDOC_KEYWORDS: &[&str] = &["RUN", "COPY", "ADD"];

/// Keywords whose arguments may be a JSON array.
const JSON_KEYWORDS: &[&str] = &["RUN", "CMD", "ENTRYPOINT", "SHELL", "COPY", "ADD", "VOLUME"];

/// One parsed instruction: the upper-cased keyword, its arguments in the IR
/// shape described in the module docs, the 1-based start line and where in
/// the source text it sits.
#[derive(Debug, Clone, PartialEq)]
struct Instruction {
    keyword: String,
    arguments: Value,
    line: u32,
    position: Position,
}

/// Byte offsets of one instruction in the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Position {
    /// The keyword's first byte.
    keyword: usize,
    /// The first argument byte on the start line; the keyword when the
    /// arguments begin on a continuation line or there are none.
    arguments: usize,
    /// Every physical line of the instruction (continuations and heredoc
    /// bodies included), with the last line's terminator.
    lines: Range<usize>,
    /// Leading blanks before the keyword.
    indent: u32,
}

/// Record a provenance entry with a span for `/index` and every node under
/// it: the row and its `instruction` / `line` at the keyword, `arguments`
/// and everything inside it at the first argument. `bom` is the length of
/// the BOM the document's bytes no longer carry.
fn record_provenance(
    provenance: &mut ProvenanceMap,
    index: usize,
    instr: &Instruction,
    bom: usize,
) {
    let pos = &instr.position;
    let lines = pos.lines.start - bom..pos.lines.end - bom;
    let span = |at: usize, context: SpanContext| ValueSpan {
        value_range: at - bom..at - bom,
        line_range: lines.clone(),
        indent: pos.indent,
        context,
    };
    let row = Pointer::new(vec![Segment::Index(index)]);
    let mut insert = |pointer: Pointer, span: ValueSpan| {
        provenance.insert(
            pointer.as_canonical(),
            Provenance::original(pointer, Some(span)),
        );
    };
    insert(row.clone(), span(pos.keyword, SpanContext::BlockSeqItem));
    for field in ["instruction", "line"] {
        insert(
            row.with_segment(Segment::Key(field.to_owned())),
            span(pos.keyword, SpanContext::BlockMapValue),
        );
    }
    let mut stack = vec![(
        row.with_segment(Segment::Key("arguments".to_owned())),
        &instr.arguments,
    )];
    while let Some((pointer, value)) = stack.pop() {
        match value {
            Value::Map(map) => stack.extend(
                map.iter()
                    .map(|(key, v)| (pointer.with_segment(Segment::Key(key.clone())), v)),
            ),
            Value::Array(items) => stack.extend(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (pointer.with_segment(Segment::Index(i)), v)),
            ),
            _ => {}
        }
        insert(pointer, span(pos.arguments, SpanContext::BlockMapValue));
    }
}

/// A source line without its line terminator, with its byte range.
#[derive(Debug, Clone, Copy)]
struct SourceLine<'a> {
    text: &'a str,
    start: usize,
}

impl SourceLine<'_> {
    fn span(&self) -> Range<usize> {
        self.start..self.start + self.text.len()
    }
}

/// A heredoc opened on an instruction line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Heredoc {
    /// Terminator word, quotes removed.
    name: String,
    /// `<<-`: leading tabs are stripped before matching the terminator.
    chomp: bool,
}

/// Split `text` into lines without terminators (`\n`, `\r\n`), keeping each
/// line's byte offset. A UTF-8 BOM at the start is skipped.
fn source_lines(text: &str) -> Vec<SourceLine<'_>> {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let mut start = bom;
    let mut out = Vec::new();
    for raw in text[bom..].split_inclusive('\n') {
        out.push(SourceLine {
            text: raw.trim_end_matches(['\r', '\n']),
            start,
        });
        start += raw.len();
    }
    out
}

/// Parse the whole Dockerfile into instructions.
fn parse_instructions(text: &str) -> Result<Vec<Instruction>> {
    let lines = source_lines(text);
    let escape = parse_directives(&lines)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let first = lines[i];
        let start_line = line_number(i);
        i += 1;
        let head = first.text.trim_start();
        if head.starts_with('#') {
            continue;
        }
        let indent = first.text.len() - head.len();
        let keyword_at = first.start + indent;
        let (joined, mut done) = trim_continuation(head, escape);
        let arguments_at = keyword_at + arguments_offset(joined);
        let mut joined = joined.to_owned();
        while !done && i < lines.len() {
            let next = lines[i].text;
            i += 1;
            let trimmed = next.trim_start();
            if trimmed.starts_with('#') || trimmed.is_empty() {
                continue;
            }
            let (part, end) = trim_continuation(next, escape);
            joined.push_str(part);
            done = end;
        }
        let logical = joined.trim();
        if logical.is_empty() {
            continue;
        }
        let err = |message: String| Error::Parse {
            file: None,
            line: start_line,
            col: 1,
            span: first.span(),
            snippet: first.text.to_owned(),
            message: format!("line {start_line}: {message}"),
        };
        let (word, rest) = split_keyword(logical);
        let keyword = word.to_ascii_uppercase();
        if !KEYWORDS.contains(&keyword.as_str()) {
            return Err(err(format!("unknown instruction `{word}`")));
        }
        if rest.is_empty() && !ARGLESS_KEYWORDS.contains(&keyword.as_str()) {
            return Err(err(format!("{keyword} requires at least one argument")));
        }
        let mut arguments = build_arguments(&keyword, rest, escape).map_err(err)?;

        if let Some(heredoc_text) = heredoc_source(&keyword, rest, logical) {
            for heredoc in heredocs_from_line(heredoc_text) {
                let mut terminated = false;
                while i < lines.len() {
                    let body = lines[i].text;
                    i += 1;
                    if let Value::String(s) = &mut arguments {
                        s.push('\n');
                        s.push_str(body);
                    }
                    let candidate = if heredoc.chomp {
                        body.trim_start_matches('\t')
                    } else {
                        body
                    };
                    if candidate == heredoc.name {
                        terminated = true;
                        break;
                    }
                }
                if !terminated {
                    return Err(err(format!(
                        "unterminated heredoc `{}`: no line `{}` before the end of file",
                        heredoc.name, heredoc.name
                    )));
                }
            }
        }

        let end = lines.get(i).map_or(text.len(), |next| next.start);
        out.push(Instruction {
            keyword,
            arguments,
            line: start_line,
            position: Position {
                keyword: keyword_at,
                arguments: arguments_at,
                lines: first.start..end,
                indent: u32::try_from(indent).unwrap_or(u32::MAX),
            },
        });
    }
    Ok(out)
}

/// Byte offset of the first argument in `head` — the start line from the
/// keyword on, without a trailing continuation — or `0` (the keyword) when
/// nothing follows the keyword on that line.
fn arguments_offset(head: &str) -> usize {
    let (word, _) = split_keyword(head);
    let after = &head[word.len()..];
    let blanks = after.len() - after.trim_start().len();
    if after.trim().is_empty() {
        0
    } else {
        word.len() + blanks
    }
}

/// 1-based line number for the 0-based index `i`, saturating at `u32::MAX`.
fn line_number(i: usize) -> u32 {
    u32::try_from(i + 1).unwrap_or(u32::MAX)
}

/// Read the parser directives at the top of the file and return the escape
/// character they select (`\` by default).
fn parse_directives(lines: &[SourceLine<'_>]) -> Result<char> {
    let mut escape = '\\';
    let mut seen: Vec<String> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let Some((name, value)) = directive(line.text) else {
            break;
        };
        let err = |message: String| Error::Parse {
            file: None,
            line: line_number(idx),
            col: 1,
            span: line.span(),
            snippet: line.text.to_owned(),
            message: format!("line {}: {message}", line_number(idx)),
        };
        if seen.contains(&name) {
            return Err(err(format!(
                "only one `{name}` parser directive can be used"
            )));
        }
        if name == "escape" {
            escape = match value {
                "\\" => '\\',
                "`" => '`',
                other => {
                    return Err(err(format!(
                        "invalid escape token `{other}`: must be ` or \\"
                    )));
                }
            };
        }
        seen.push(name);
    }
    Ok(escape)
}

/// Recognise one parser directive line (`# name=value`, any blanks around
/// the `=`). Returns `None` for any other line, including comments that name
/// an unknown directive — those end the directive block.
fn directive(line: &str) -> Option<(String, &str)> {
    let body = line.trim_start().strip_prefix('#')?.trim_start();
    let name_len = body
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())))
        .map_or(body.len(), |(i, _)| i);
    if name_len == 0 {
        return None;
    }
    let name = body[..name_len].to_ascii_lowercase();
    let value = body[name_len..].trim_start().strip_prefix('=')?.trim();
    if value.is_empty() || !matches!(name.as_str(), "syntax" | "escape" | "check") {
        return None;
    }
    Some((name, value))
}

/// Strip a trailing line-continuation (the escape character, optionally
/// followed by blanks) from `line`. Returns the remaining text and whether
/// the logical line ends here. A doubled escape character at the end is a
/// literal, not a continuation.
fn trim_continuation(line: &str, escape: char) -> (&str, bool) {
    let trimmed = line.trim_end_matches([' ', '\t']);
    let Some(before) = trimmed.strip_suffix(escape) else {
        return (line, true);
    };
    if before.ends_with(escape) {
        return (line, true);
    }
    (before, false)
}

/// Split a logical line into the keyword and the (trimmed) rest.
fn split_keyword(line: &str) -> (&str, &str) {
    match line.split_once([' ', '\t', '\u{b}', '\u{c}', '\r']) {
        Some((word, rest)) => (word, rest.trim()),
        None => (line, ""),
    }
}

/// The text to scan for heredoc openers, or `None` when the instruction
/// cannot carry one (not `RUN`/`COPY`/`ADD`, JSON form, or no `<<`).
fn heredoc_source<'a>(keyword: &str, rest: &str, logical: &'a str) -> Option<&'a str> {
    let (keyword, rest) = if keyword == "ONBUILD" {
        let (sub, sub_rest) = split_keyword(rest);
        (sub.to_ascii_uppercase(), sub_rest)
    } else {
        (keyword.to_owned(), rest)
    };
    let eligible = HEREDOC_KEYWORDS.contains(&keyword.as_str())
        && !matches!(parse_json_array(rest), Some(Ok(_)))
        && logical.contains("<<");
    eligible.then_some(logical)
}

/// Build the `arguments` value for one instruction.
fn build_arguments(keyword: &str, rest: &str, escape: char) -> std::result::Result<Value, String> {
    match keyword {
        "ENV" | "LABEL" => parse_name_values(keyword, rest, escape).map(Value::Map),
        "ARG" => parse_arg(rest, escape).map(Value::Map),
        "ONBUILD" => {
            let (sub, _) = split_keyword(rest);
            let sub = sub.to_ascii_uppercase();
            if KEYWORDS.contains(&sub.as_str()) {
                Ok(Value::String(rest.to_owned()))
            } else {
                Err(format!("unknown ONBUILD trigger instruction `{sub}`"))
            }
        }
        _ if JSON_KEYWORDS.contains(&keyword) => match parse_json_array(rest) {
            Some(Ok(items)) => {
                if keyword == "SHELL" && items.is_empty() {
                    return Err("SHELL requires at least one argument".to_owned());
                }
                Ok(Value::Array(items.into_iter().map(Value::String).collect()))
            }
            Some(Err(message)) => Err(message),
            None if keyword == "SHELL" => Err(
                "SHELL requires the arguments in JSON form: SHELL [\"executable\", \"parameters\"]"
                    .to_owned(),
            ),
            None => Ok(Value::String(rest.to_owned())),
        },
        _ => Ok(Value::String(rest.to_owned())),
    }
}

/// Parse `rest` as a JSON array of strings, the way BuildKit decides between
/// exec and shell form: `None` when the text is not a JSON array (shell
/// form), `Some(Err)` for a JSON array with a non-string element.
fn parse_json_array(rest: &str) -> Option<std::result::Result<Vec<String>, String>> {
    if !rest.starts_with('[') {
        return None;
    }
    let items: Vec<serde_json::Value> = serde_json::from_str(rest).ok()?;
    Some(
        items
            .into_iter()
            .map(|item| match item {
                serde_json::Value::String(s) => Ok(s),
                _ => Err(
                    "when using JSON array syntax, arrays must be comprised of strings only"
                        .to_owned(),
                ),
            })
            .collect(),
    )
}

/// Split `rest` into words on blanks, keeping quotes and escapes as written
/// (BuildKit's `parseWords`). A quoted blank does not split.
fn parse_words(rest: &str, escape: char) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            word.push(c);
            if c == q {
                quote = None;
            } else if c == escape && q != '\'' {
                word.extend(chars.next());
            }
            continue;
        }
        if c.is_whitespace() {
            if in_word {
                words.push(std::mem::take(&mut word));
                in_word = false;
            }
            continue;
        }
        in_word = true;
        word.push(c);
        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == escape {
            word.extend(chars.next());
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Remove shell quotes and escapes from a word without expanding variables:
/// `'…'` is literal, inside `"…"` the escape character escapes `"`, `$`,
/// `` ` `` and itself, outside quotes it escapes any character.
fn unquote(word: &str, escape: char) -> String {
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                for inner in chars.by_ref() {
                    if inner == '\'' {
                        break;
                    }
                    out.push(inner);
                }
            }
            '"' => {
                while let Some(inner) = chars.next() {
                    if inner == '"' {
                        break;
                    }
                    if inner == escape {
                        match chars.peek() {
                            Some(&next)
                                if next == '"' || next == '$' || next == '`' || next == escape =>
                            {
                                out.push(next);
                                chars.next();
                            }
                            _ => out.push(inner),
                        }
                    } else {
                        out.push(inner);
                    }
                }
            }
            c if c == escape => match chars.next() {
                Some(next) => out.push(next),
                None => out.push(c),
            },
            _ => out.push(c),
        }
    }
    out
}

/// Split the leading builder flags (`--name[=value]`) off `rest`, the way
/// BuildKit's `extractBuilderFlags` does; a bare `--` ends the flags.
fn split_flags(rest: &str) -> (Vec<&str>, &str) {
    let mut flags = Vec::new();
    let mut tail = rest.trim_start();
    while tail.starts_with("--") {
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let (flag, after) = tail.split_at(end);
        tail = after.trim_start();
        if flag == "--" {
            break;
        }
        flags.push(flag);
    }
    (flags, tail)
}

/// `ENV` / `LABEL` arguments: `name=value …`, or the legacy `name value…`
/// (the name maps to the rest of the line). Leading builder flags are
/// skipped; with `--unset` the words are names only and map to `null`.
fn parse_name_values(
    keyword: &str,
    rest: &str,
    escape: char,
) -> std::result::Result<IndexMap<String, Value>, String> {
    let (flags, rest) = split_flags(rest);
    if rest.is_empty() {
        return Err(format!("{keyword} requires at least one argument"));
    }
    let unset = flags.iter().any(|flag| {
        let (name, value) = flag.split_once('=').unwrap_or((flag, ""));
        name == "--unset" && !value.eq_ignore_ascii_case("false")
    });
    let words = parse_words(rest, escape);
    let mut out = IndexMap::new();
    if unset {
        for word in &words {
            if word.contains('=') {
                return Err(format!(
                    "{keyword} --unset does not accept values: `{word}`"
                ));
            }
            let name = unquote(word, escape);
            if name.is_empty() {
                return Err(format!("{keyword} names can not be blank"));
            }
            out.insert(name, Value::Null);
        }
        return Ok(out);
    }
    let mut insert = |name: &str, value: &str| {
        let name = unquote(name, escape);
        if name.is_empty() {
            return Err(format!("{keyword} names can not be blank"));
        }
        out.insert(name, Value::String(unquote(value, escape)));
        Ok(())
    };
    match words.first() {
        Some(first) if !first.contains('=') => {
            let Some((name, value)) = rest.split_once(char::is_whitespace) else {
                return Err(format!("{keyword} must have two arguments"));
            };
            insert(name, value.trim_start())?;
        }
        _ => {
            for word in &words {
                let Some((name, value)) = word.split_once('=') else {
                    return Err(format!(
                        "{keyword}: can't find = in `{word}`; must be of the form name=value"
                    ));
                };
                insert(name, value)?;
            }
        }
    }
    Ok(out)
}

/// `ARG` arguments: `name` (→ `null`) or `name=default`, several per line.
fn parse_arg(rest: &str, escape: char) -> std::result::Result<IndexMap<String, Value>, String> {
    let mut out = IndexMap::new();
    for word in parse_words(rest, escape) {
        let (name, value) = match word.split_once('=') {
            Some((name, value)) => (name, Value::String(unquote(value, escape))),
            None => (word.as_str(), Value::Null),
        };
        let name = unquote(name, escape);
        if name.is_empty() {
            return Err("ARG names can not be blank".to_owned());
        }
        out.insert(name, value);
    }
    Ok(out)
}

/// Find the heredoc openers on an instruction line: every shell word of the
/// form `[fd]<<[-]NAME` where `NAME` (quotes removed) is not empty. Quoted
/// `<<` does not open a heredoc.
fn heredocs_from_line(line: &str) -> Vec<Heredoc> {
    shell_words_raw(line)
        .iter()
        .filter_map(|word| heredoc_from_word(word))
        .collect()
}

fn heredoc_from_word(word: &str) -> Option<Heredoc> {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.strip_prefix("<<")?;
    let (chomp, rest) = match rest.strip_prefix('-') {
        Some(after) => (true, after),
        None => (false, rest),
    };
    let rest = rest.trim_start();
    if rest.is_empty() || rest.contains('<') {
        return None;
    }
    let name = unquote(rest, '\\');
    (!name.is_empty()).then_some(Heredoc { name, chomp })
}

/// Split a line into shell words, keeping quotes and backslash escapes in the
/// words (BuildKit lexes heredoc openers with `\` regardless of `escape`).
fn shell_words_raw(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if in_word {
                words.push(std::mem::take(&mut word));
                in_word = false;
            }
            continue;
        }
        in_word = true;
        word.push(c);
        match c {
            '\\' => word.extend(chars.next()),
            '\'' => {
                for inner in chars.by_ref() {
                    word.push(inner);
                    if inner == '\'' {
                        break;
                    }
                }
            }
            '"' => {
                while let Some(inner) = chars.next() {
                    word.push(inner);
                    if inner == '\\' {
                        word.extend(chars.next());
                    } else if inner == '"' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        Value::String(text.to_owned())
    }

    fn arr(items: &[&str]) -> Value {
        Value::Array(items.iter().map(|i| s(i)).collect())
    }

    fn parse(text: &str) -> Vec<(String, Value, u32)> {
        parse_instructions(text)
            .expect("parse")
            .into_iter()
            .map(|i| (i.keyword, i.arguments, i.line))
            .collect()
    }

    fn parse_err(text: &str) -> (u32, String) {
        match parse_instructions(text).expect_err("must fail") {
            Error::Parse { line, message, .. } => (line, message),
            other => panic!("expected Error::Parse, got {other:?}"),
        }
    }

    #[test]
    fn parse_simple_dockerfile() {
        let doc = Dockerfile.parse(b"FROM alpine\n").expect("parse");
        let Value::Array(items) = doc.value() else {
            panic!("expected array")
        };
        assert_eq!(items.len(), 1);
        let Value::Map(row) = &items[0] else {
            panic!("expected row map")
        };
        assert_eq!(row.get("instruction"), Some(&s("FROM")));
        assert_eq!(row.get("arguments"), Some(&s("alpine")));
        assert_eq!(row.get("line"), Some(&Value::Int(1)));
    }

    #[test]
    fn parse_rejects_invalid_utf8() {
        let err = Dockerfile.parse(b"FROM \xff\n").expect_err("invalid UTF-8");
        assert!(matches!(err, Error::Parse { .. }), "got {err:?}");
    }

    #[test]
    fn keywords_without_parser_crate_gaps_parse() {
        let got = parse(
            "HEALTHCHECK CMD true\nONBUILD RUN make\nMAINTAINER x <y@z>\n\
             WORKDIR /\nUSER 0\nEXPOSE 8\nfrom alpine\n",
        );
        assert_eq!(
            got,
            vec![
                ("HEALTHCHECK".into(), s("CMD true"), 1),
                ("ONBUILD".into(), s("RUN make"), 2),
                ("MAINTAINER".into(), s("x <y@z>"), 3),
                ("WORKDIR".into(), s("/"), 4),
                ("USER".into(), s("0"), 5),
                ("EXPOSE".into(), s("8"), 6),
                ("FROM".into(), s("alpine"), 7),
            ]
        );
    }

    #[test]
    fn shell_form_is_a_string_and_json_form_an_array() {
        let got = parse(
            "RUN curl -fsSL https://x | bash\nRUN [\"sh\",\"-c\",\"echo hi\"]\n\
             COPY --chown=1:1 a b\nCOPY [\"a b\", \"c\"]\nRUN [ -f x ] && y\nCMD\n",
        );
        assert_eq!(got[0].1, s("curl -fsSL https://x | bash"));
        assert_eq!(got[1].1, arr(&["sh", "-c", "echo hi"]));
        assert_eq!(got[2].1, s("--chown=1:1 a b"));
        assert_eq!(got[3].1, arr(&["a b", "c"]));
        assert_eq!(got[4].1, s("[ -f x ] && y"));
        assert_eq!(got[5].1, s(""));
    }

    #[test]
    fn continuation_joins_lines_and_keeps_the_start_line() {
        let got = parse(
            "# a comment\n\nRUN curl -fsSL https://x \\\n  # dropped\n\n    | bash\n  WORKDIR /app\n",
        );
        assert_eq!(
            got,
            vec![
                ("RUN".into(), s("curl -fsSL https://x     | bash"), 3),
                ("WORKDIR".into(), s("/app"), 7),
            ]
        );
    }

    #[test]
    fn escape_directive_switches_the_continuation_character() {
        let got = parse("# escape=`\nRUN dir c:\\ `\n  /b\nCOPY a\\ b\\\n");
        assert_eq!(got[0], ("RUN".into(), s("dir c:\\   /b"), 2));
        assert_eq!(got[1], ("COPY".into(), s("a\\ b\\"), 4));
    }

    #[test]
    fn doubled_escape_at_end_of_line_is_not_a_continuation() {
        assert_eq!(trim_continuation("a \\\\", '\\'), ("a \\\\", true));
        assert_eq!(trim_continuation("a \\  ", '\\'), ("a ", false));
        assert_eq!(trim_continuation("\\", '\\'), ("", false));
    }

    #[test]
    fn directive_recognises_known_names_only() {
        assert_eq!(directive("# escape=`"), Some(("escape".into(), "`")));
        assert_eq!(
            directive("#SYNTAX = docker/dockerfile:1"),
            Some(("syntax".into(), "docker/dockerfile:1"))
        );
        assert_eq!(directive("# foo=bar"), None);
        assert_eq!(directive("# just a comment"), None);
        assert_eq!(directive("RUN x"), None);
    }

    #[test]
    fn directives_after_the_first_other_line_are_comments() {
        let got = parse("FROM alpine\n# escape=`\nRUN a \\\n  b\n");
        assert_eq!(got[1], ("RUN".into(), s("a   b"), 3));
    }

    #[test]
    fn heredoc_bodies_are_appended_verbatim() {
        let got = parse("RUN <<EOT\necho hi\nEOT\nWORKDIR /\n");
        assert_eq!(
            got,
            vec![
                ("RUN".into(), s("<<EOT\necho hi\nEOT"), 1),
                ("WORKDIR".into(), s("/"), 4),
            ]
        );
    }

    #[test]
    fn heredoc_quoted_chomped_and_several_per_line() {
        let got = parse("COPY <<\"A\" <<-'B' /dst/\none\nA\n\ttwo\n\tB\nRUN echo '<<NOT' x\n");
        assert_eq!(got[0].1, s("<<\"A\" <<-'B' /dst/\none\nA\n\ttwo\n\tB"));
        assert_eq!(got[1], ("RUN".into(), s("echo '<<NOT' x"), 6));
    }

    #[test]
    fn heredocs_from_line_finds_openers() {
        assert_eq!(
            heredocs_from_line("RUN 3<<EOF cat <<-\"X Y\" '<<Q'"),
            vec![
                Heredoc {
                    name: "EOF".into(),
                    chomp: false
                },
                Heredoc {
                    name: "X Y".into(),
                    chomp: true
                },
            ]
        );
        assert!(heredocs_from_line("RUN a << b").is_empty());
    }

    #[test]
    fn env_label_arg_are_maps() {
        let got = parse(
            "ENV A 1\nENV B one two\nENV C=\"x y\" D='z' E=a\\ b\n\
             LABEL \"com.example\"=v other=w\nARG X\nARG Y=2 Z\n",
        );
        let map = |pairs: &[(&str, Value)]| {
            Value::Map(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
            )
        };
        assert_eq!(got[0].1, map(&[("A", s("1"))]));
        assert_eq!(got[1].1, map(&[("B", s("one two"))]));
        assert_eq!(
            got[2].1,
            map(&[("C", s("x y")), ("D", s("z")), ("E", s("a b"))])
        );
        assert_eq!(got[3].1, map(&[("com.example", s("v")), ("other", s("w"))]));
        assert_eq!(got[4].1, map(&[("X", Value::Null)]));
        assert_eq!(got[5].1, map(&[("Y", s("2")), ("Z", Value::Null)]));
    }

    #[test]
    fn env_and_label_skip_leading_flags() {
        let got = parse("ENV --unset A B\nLABEL --unset=false a=b\nENV -- C=1\n");
        let map = |pairs: &[(&str, Value)]| {
            Value::Map(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
            )
        };
        assert_eq!(got[0].1, map(&[("A", Value::Null), ("B", Value::Null)]));
        assert_eq!(got[1].1, map(&[("a", s("b"))]));
        assert_eq!(got[2].1, map(&[("C", s("1"))]));
    }

    #[test]
    fn split_flags_stops_at_the_first_other_word() {
        assert_eq!(
            split_flags("--a=1 --b x --c"),
            (vec!["--a=1", "--b"], "x --c")
        );
        assert_eq!(split_flags("-- --a"), (vec![], "--a"));
        assert_eq!(split_flags("x"), (vec![], "x"));
    }

    #[test]
    fn unquote_handles_quotes_and_escapes() {
        assert_eq!(unquote(r#""a \"b\" \$c \d""#, '\\'), r#"a "b" $c \d"#);
        assert_eq!(unquote(r"'a \b'", '\\'), r"a \b");
        assert_eq!(unquote(r"a\ b", '\\'), "a b");
        assert_eq!(unquote("$HOME/x", '\\'), "$HOME/x");
    }

    #[test]
    fn parse_words_keeps_quoted_blanks_together() {
        assert_eq!(
            parse_words(r#"A="x y" B=c\ d  'e f'"#, '\\'),
            vec![r#"A="x y""#, r"B=c\ d", "'e f'"]
        );
    }

    #[test]
    fn errors_carry_the_instruction_start_line() {
        let cases: &[(&str, u32, &str)] = &[
            ("FROM a\nFOO bar\n", 2, "unknown instruction `FOO`"),
            (
                "FROM a\n\nWORKDIR\n",
                3,
                "WORKDIR requires at least one argument",
            ),
            ("FROM a\nRUN <<EOF\necho\n", 2, "unterminated heredoc `EOF`"),
            ("CMD [\"a\", 1]\n", 1, "strings only"),
            ("SHELL sh -c\n", 1, "JSON form"),
            ("SHELL []\n", 1, "SHELL requires at least one argument"),
            ("ENV A\n", 1, "ENV must have two arguments"),
            ("LABEL a=b c\n", 1, "can't find ="),
            ("ENV =x\n", 1, "names can not be blank"),
            ("ARG =x\n", 1, "names can not be blank"),
            ("ENV --unset A=1\n", 1, "--unset does not accept values"),
            ("LABEL --unset\n", 1, "LABEL requires at least one argument"),
            ("ONBUILD FOO x\n", 1, "unknown ONBUILD trigger"),
            ("# escape=x\nFROM a\n", 1, "invalid escape token"),
            ("# escape=`\n# escape=\\\nFROM a\n", 2, "only one `escape`"),
        ];
        for (src, line, needle) in cases {
            let (got_line, message) = parse_err(src);
            assert_eq!(got_line, *line, "line for {src:?}: {message}");
            assert!(message.contains(needle), "{src:?}: {message}");
        }
    }

    /// `line:col` of every pointer in `pointers` for the Dockerfile `src`.
    fn positions(src: &str, pointers: &[&str]) -> Vec<Option<(u32, u32)>> {
        let doc = Dockerfile.parse(src.as_bytes()).expect("parse");
        let ir = doc.as_ir();
        pointers
            .iter()
            .map(|p| ir.line_col_for(&Pointer::parse(p).expect("pointer")))
            .collect()
    }

    #[test]
    fn pointers_resolve_to_the_instruction_start_line() {
        let src = "# syntax=docker/dockerfile:1\nFROM alpine\n\n  run apk add curl \\\n    # dropped\n    && curl x | sh\nRUN <<EOT\necho hi\nEOT\nENV A=1 B=2\nCMD\nRUN \\\n  echo late\nCOPY [\"a\", \"b\"]\n";
        assert_eq!(
            positions(
                src,
                &[
                    "/0",
                    "/0/instruction",
                    "/0/arguments",
                    "/0/line",
                    "/1",
                    "/1/arguments",
                    "/2",
                    "/2/arguments",
                    "/3/arguments/B",
                    "/4",
                    "/4/arguments",
                    "/5",
                    "/5/arguments",
                    "/6/arguments/1",
                ]
            ),
            [
                Some((2, 1)),
                Some((2, 1)),
                Some((2, 6)),
                Some((2, 1)),
                Some((4, 3)),
                Some((4, 7)),
                Some((7, 1)),
                Some((7, 5)),
                Some((10, 5)),
                Some((11, 1)),
                Some((11, 1)),
                Some((12, 1)),
                Some((12, 1)),
                Some((14, 6)),
            ]
        );
    }

    #[test]
    fn pointers_resolve_under_escape_crlf_and_a_bom() {
        let src = "\u{feff}# escape=`\r\nFROM alpine\r\nRUN echo a `\r\n  && b\r\nADD a /a\r\n";
        assert_eq!(
            positions(src, &["/0", "/1", "/1/arguments", "/2", "/2/arguments"]),
            [
                Some((2, 1)),
                Some((3, 1)),
                Some((3, 5)),
                Some((5, 1)),
                Some((5, 5)),
            ]
        );
        assert_eq!(
            positions("\u{feff}FROM alpine\n", &["/0/arguments"]),
            [Some((1, 6))]
        );
    }

    #[test]
    fn the_root_and_missing_pointers_have_no_position() {
        assert_eq!(
            positions("FROM alpine\n", &["", "/1", "/0/nope"]),
            [None, None, None]
        );
    }

    #[test]
    fn empty_and_comment_only_files_have_no_instructions() {
        assert!(parse("").is_empty());
        assert!(parse("\u{feff}# syntax=docker/dockerfile:1\n# hi\n\n").is_empty());
    }

    #[test]
    fn write_returns_format_error() {
        let doc = Document::value_only(Value::Array(vec![]), FormatTag::Dockerfile);
        let mut buf: Vec<u8> = Vec::new();
        let err = Dockerfile.write(&doc, &mut buf).expect_err("read-only");
        match err {
            Error::Format { format, message } => {
                assert_eq!(format, "dockerfile");
                assert!(
                    message.contains("read-only"),
                    "expected read-only message; got: {message}",
                );
            }
            other => panic!("expected Format error, got {other:?}"),
        }
    }
}
