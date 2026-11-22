//! Where a parse error is, for every format.
//!
//! Every [`Error::Parse`] carries the same position whatever parser raised
//! it: a 1-based line, a 1-based column in bytes (a leading byte order mark
//! is not counted), the byte range of the text at fault, the whole source
//! line without its line break, and a message without a position of its own.
//!
//! One row per malformed input. The expectations are literals; each is also
//! checked against [`place_of`], a plain scan of the input, so that a row
//! cannot expect a line, a column or a source line that are not those of the
//! byte its span starts at.
//!
//! The same rows feed three properties that hold for any of them: a comment
//! (or blank) line above moves the error down by one line and changes nothing
//! else, a byte order mark moves nothing but the span, and a multi-byte
//! character counts as many columns as an ASCII text of its length does.

use std::fmt::Write as _;
use std::ops::Range;

use dq_core::{Document, Error, by_name, parse_json_with_spans, parse_yaml_stream_with_spans};

const BOM: &str = "\u{feff}";

/// A malformed input and where its parse error is.
struct Row {
    name: &'static str,
    input: Vec<u8>,
    line: u32,
    col: u32,
    span: Range<usize>,
    snippet: String,
    /// The first words of the message.
    message: &'static str,
}

fn row(
    name: &'static str,
    input: impl AsRef<[u8]>,
    (line, col): (u32, u32),
    span: Range<usize>,
    snippet: impl Into<String>,
    message: &'static str,
) -> Row {
    Row {
        name,
        input: input.as_ref().to_vec(),
        line,
        col,
        span,
        snippet: snippet.into(),
        message,
    }
}

/// The fields of an [`Error::Parse`] that names no file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    line: u32,
    col: u32,
    span: Range<usize>,
    snippet: String,
    message: String,
}

fn place(result: dq_core::Result<Document>) -> Result<Place, String> {
    match result {
        Err(Error::Parse {
            file: None,
            line,
            col,
            span,
            snippet,
            message,
        }) => Ok(Place {
            line,
            col,
            span,
            snippet,
            message,
        }),
        Err(other) => Err(format!("expected a parse error, got {other:?}")),
        Ok(doc) => Err(format!("expected a parse error, got {:?}", doc.value())),
    }
}

fn parse(format: &str, input: &[u8]) -> Result<Place, String> {
    place(by_name(format).expect("known format").parse(input))
}

/// The line, the column and the source line of byte `at` of `bytes`, by
/// walking its lines from the first: they end in `\n` (`\r\n`), or in a lone
/// `\r` where there is no `\n`; a byte of a line break is on the line it
/// ends.
fn place_of(bytes: &[u8], at: usize) -> (u32, u32, String) {
    let line_break = if bytes.contains(&b'\n') { b'\n' } else { b'\r' };
    let mut start = if bytes.starts_with(BOM.as_bytes()) {
        BOM.len()
    } else {
        0
    };
    let mut line = 1;
    loop {
        let rest = &bytes[start..];
        let Some(len) = rest.iter().position(|&b| b == line_break) else {
            return (line, col_of(start, at), lossy(rest));
        };
        if at <= start + len {
            let text = rest[..len].strip_suffix(b"\r").unwrap_or(&rest[..len]);
            return (line, col_of(start, at), lossy(text));
        }
        start += len + 1;
        line += 1;
    }
}

fn col_of(line_start: usize, at: usize) -> u32 {
    u32::try_from(at.saturating_sub(line_start)).expect("small") + 1
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `bytes` for a failure message, long ones cut short.
fn shown(bytes: &[u8]) -> String {
    let text = format!("{:?}", String::from_utf8_lossy(bytes));
    if text.chars().count() <= 160 {
        return text;
    }
    let head: String = text.chars().take(120).collect();
    format!("{head}… ({} bytes)", bytes.len())
}

/// What `got` says otherwise than `row`, one line per field; empty when they
/// agree.
fn diff(row: &Row, got: &Place) -> String {
    let mut out = String::new();
    let mut field = |name: &str, want: String, got: String| {
        if want != got {
            writeln!(
                out,
                "    {name}: expected {}",
                shown(want.as_bytes()).trim_matches('"')
            )
            .expect("write");
            writeln!(
                out,
                "    {name}: got      {}",
                shown(got.as_bytes()).trim_matches('"')
            )
            .expect("write");
        }
    };
    field(
        "line:col",
        format!("{}:{}", row.line, row.col),
        format!("{}:{}", got.line, got.col),
    );
    field("span", format!("{:?}", row.span), format!("{:?}", got.span));
    field("snippet", row.snippet.clone(), got.snippet.clone());
    if !got.message.starts_with(row.message) {
        field("message", row.message.to_owned(), got.message.clone());
    }
    if got.message.contains('\n')
        || got
            .message
            .contains(&format!("line {} column {}", got.line, got.col))
    {
        field(
            "message",
            "one line without its own position".to_owned(),
            got.message.clone(),
        );
    }
    out
}

/// Check every row of `rows` against what `read` makes of it, and report all
/// that fail. `in_bytes`: the positions are those of the input's bytes (not
/// of a text decoded from them).
fn check_with(
    format: &str,
    rows: &[Row],
    in_bytes: bool,
    read: impl Fn(&[u8]) -> Result<Place, String>,
) {
    let mut failures = String::new();
    let mut names: Vec<&str> = Vec::new();
    for row in rows {
        assert!(
            !names.contains(&row.name),
            "{format}: two rows named '{}'",
            row.name
        );
        names.push(row.name);
        if in_bytes {
            assert_eq!(
                (row.line, row.col, row.snippet.clone()),
                place_of(&row.input, row.span.start),
                "{format} / {}: the row itself is not the place of byte {}",
                row.name,
                row.span.start
            );
        }
        let found = match read(&row.input) {
            Ok(got) => diff(row, &got),
            Err(other) => format!("    {other}\n"),
        };
        if !found.is_empty() {
            write!(
                failures,
                "\n  {format} / {}: {}\n{found}",
                row.name,
                shown(&row.input)
            )
            .expect("write");
        }
    }
    assert!(failures.is_empty(), "{failures}");
}

fn check(format: &str, rows: &[Row]) {
    check_with(format, rows, true, |input| parse(format, input));
}

/// A line of some 3000 characters with `fault` in the middle.
fn long(open: &str, fault: &str, close: &str) -> String {
    let filler = "x".repeat(1490);
    format!("{open}{filler}{fault}{filler}{close}")
}

const VALUE: &str = "expected JSON value (object, array, scalar)";

fn json() -> Vec<Row> {
    let long = long("{\"k\": \"", "\", \"y\": @, \"z\": \"", "\"}");
    vec![
        row(
            "double comma",
            "{\n  \"a\": 1,\n  \"b\": [1, 2,, 3]\n}\n",
            (3, 14),
            25..26,
            "  \"b\": [1, 2,, 3]",
            VALUE,
        ),
        row(
            "multi-byte before",
            "{\n  \"ключ\": \"я😀\", x\n}\n",
            (2, 25),
            26..27,
            "  \"ключ\": \"я😀\", x",
            "expected string key in object",
        ),
        row(
            "multi-byte at fault",
            "{\"я\": 😀}\n",
            (1, 8),
            7..11,
            "{\"я\": 😀}",
            VALUE,
        ),
        row(
            "tab",
            "{\n\t\"a\":\t@\n}\n",
            (2, 7),
            8..9,
            "\t\"a\":\t@",
            VALUE,
        ),
        row(
            "CRLF",
            "{\r\n  \"a\": 1,\r\n  \"b\": tru\r\n}\r\n",
            (3, 8),
            21..22,
            "  \"b\": tru",
            "expected 'true' or 'false'",
        ),
        row(
            "lone CR",
            "{\r  \"a\": 1,\r  \"b\": ]\r}\r",
            (3, 8),
            19..20,
            "  \"b\": ]",
            VALUE,
        ),
        row(
            "lone CR among LF",
            "{\r\"a\": 1,\n\"b\": ]}",
            (2, 6),
            15..16,
            "\"b\": ]}",
            VALUE,
        ),
        row(
            "BOM",
            "\u{feff}{\"a\": }\n",
            (1, 7),
            9..10,
            "{\"a\": }",
            VALUE,
        ),
        row(
            "BOM, second line",
            "\u{feff}{\n\"a\": }\n",
            (2, 6),
            10..11,
            "\"a\": }",
            VALUE,
        ),
        row(
            "two BOMs",
            "\u{feff}\u{feff}{}\n",
            (1, 1),
            3..6,
            "\u{feff}{}",
            VALUE,
        ),
        row(
            "end of file after LF",
            "{\n  \"a\": [1, 2\n",
            (3, 1),
            15..15,
            "",
            "expected ',' or ']' in array",
        ),
        row(
            "end of file",
            "{\n  \"a\": [1, 2",
            (2, 13),
            14..14,
            "  \"a\": [1, 2",
            "expected ',' or ']' in array",
        ),
        row(
            "end of file after CRLF",
            "{\r\n  \"a\": [1,\r\n",
            (3, 1),
            15..15,
            "",
            "unexpected EOF while parsing value",
        ),
        row(
            "end of file after CR",
            "{\r  \"a\": [1,\r",
            (3, 1),
            13..13,
            "",
            "unexpected EOF while parsing value",
        ),
        row("empty", "", (1, 1), 0..0, "", "EOF while parsing a value"),
        row(
            "BOM alone",
            BOM,
            (1, 1),
            3..3,
            "",
            "EOF while parsing a value",
        ),
        row(
            "blank line alone",
            "  \n",
            (2, 1),
            3..3,
            "",
            "EOF while parsing a value",
        ),
        row(
            "long line",
            format!("{long}\n"),
            (1, 1506),
            1505..1506,
            long,
            VALUE,
        ),
        row(
            "comment",
            "{\"a\": 1 // c\n}\n",
            (1, 9),
            8..9,
            "{\"a\": 1 // c",
            "comments are not valid JSON",
        ),
        row(
            "unclosed string",
            "{\"a\": \"xyz\n}\n",
            (3, 1),
            13..13,
            "",
            "unterminated string literal",
        ),
        // The scanner lets these through to `serde_json`.
        row(
            "escape",
            "{\"a\": \"я\\q\"}\n",
            (1, 11),
            10..11,
            "{\"a\": \"я\\q\"}",
            "invalid escape",
        ),
        row(
            "control character in a string",
            "{\n \"я\": \"x\ty\"}\n",
            (2, 10),
            11..12,
            " \"я\": \"x\ty\"}",
            "control character",
        ),
        row(
            "control character, lone CR",
            "{\r \"a\": \"x\ty\"}",
            (2, 9),
            10..11,
            " \"a\": \"x\ty\"}",
            "control character",
        ),
        row(
            "line break in a string",
            "{\"a\": \"x\r\ny\"}\r\n",
            (1, 9),
            8..9,
            "{\"a\": \"x",
            "control character",
        ),
        row(
            "second value",
            "{\"a\":1}\n{\"b\":2}\n",
            (2, 1),
            8..9,
            "{\"b\":2}",
            "trailing characters",
        ),
        row(
            "nested too deep",
            "[".repeat(200) + &"]".repeat(200),
            (1, 128),
            127..128,
            "[".repeat(200) + &"]".repeat(200),
            "recursion limit exceeded",
        ),
        row(
            "not UTF-8",
            b"{\n  \"a\": \"x\xFF\"\n}\n",
            (2, 10),
            11..12,
            "  \"a\": \"x\u{fffd}\"",
            "source is not valid UTF-8",
        ),
        row(
            "not UTF-8, cut short",
            b"{\"a\": \"\xE2\x82",
            (1, 8),
            7..9,
            "{\"a\": \"\u{fffd}",
            "source is not valid UTF-8",
        ),
        row(
            "UTF-16",
            b"\xFF\xFE{\0}\0",
            (1, 1),
            0..1,
            "\u{fffd}\u{fffd}{\0}\0",
            "source is not valid UTF-8",
        ),
    ]
}

#[test]
fn json_positions() {
    check("json", &json());
}

/// The entry point of the edits and of `lint`.
#[test]
fn json_positions_of_the_edit_entry_point() {
    check_with("json with spans", &json(), true, |input| {
        place(parse_json_with_spans(input))
    });
}

fn jsonl() -> Vec<Row> {
    let long = long("[\"", "\", @, \"", "\"]");
    vec![
        row(
            "second line",
            "{\"a\": 1}\n{\"b\": }\n{\"c\": 3}\n",
            (2, 7),
            15..16,
            "{\"b\": }",
            VALUE,
        ),
        row(
            "multi-byte before",
            "{\"я\": 1}\n{\"ключ\": \"😀\", x}\n",
            (2, 22),
            31..32,
            "{\"ключ\": \"😀\", x}",
            "expected string key in object",
        ),
        row(
            "tab",
            "{\"a\": 1}\n\t{\"b\":\t@}\n",
            (2, 8),
            16..17,
            "\t{\"b\":\t@}",
            VALUE,
        ),
        row(
            "CRLF",
            "{\"a\": 1}\r\n{\"b\": 2,}\r\n",
            (2, 9),
            18..19,
            "{\"b\": 2,}",
            "expected string key in object",
        ),
        row(
            "blank lines before",
            "\n\n  \n[1,]\n",
            (4, 4),
            8..9,
            "[1,]",
            VALUE,
        ),
        row(
            "lone CR",
            "{\"a\": 1}\r{\"b\": }\r",
            (2, 1),
            9..10,
            "{\"b\": }",
            "trailing characters",
        ),
        row(
            "BOM",
            "\u{feff}{\"a\": @}\n",
            (1, 7),
            9..10,
            "{\"a\": @}",
            VALUE,
        ),
        row(
            "BOM, second line",
            "\u{feff}{\"a\": 1}\n{\"b\": @}\n",
            (2, 7),
            18..19,
            "{\"b\": @}",
            VALUE,
        ),
        // The end of a line cut short is the end of that line.
        row(
            "line cut short",
            "{\"a\": 1}\n{\"b\": [1\n{\"c\": 2}\n",
            (2, 9),
            17..17,
            "{\"b\": [1",
            "expected ',' or ']' in array",
        ),
        row(
            "line cut short, CRLF",
            "{\"a\": 1}\r\n{\"b\": [1\r\n{\"c\": 2}\r\n",
            (2, 9),
            18..18,
            "{\"b\": [1",
            "expected ',' or ']' in array",
        ),
        row(
            "end of file",
            "{\"a\": 1}\n{\"b\": [1",
            (2, 9),
            17..17,
            "{\"b\": [1",
            "expected ',' or ']' in array",
        ),
        row(
            "two values on a line",
            "{\"a\": 1} {\"b\": 2}\n",
            (1, 10),
            9..10,
            "{\"a\": 1} {\"b\": 2}",
            "trailing characters",
        ),
        row(
            "escape, as serde_json has it",
            "{\"a\": 1}\r\n{\"я\": \"\\q\"}\r\n",
            (2, 10),
            19..20,
            "{\"я\": \"\\q\"}",
            "invalid escape",
        ),
        row(
            "long line",
            format!("{{\"a\": 1}}\n{long}\n"),
            (2, 1496),
            1504..1505,
            long,
            VALUE,
        ),
        row(
            "not UTF-8",
            b"{\"a\": 1}\n{\"b\": \"\xFF\"}\n",
            (2, 8),
            16..17,
            "{\"b\": \"\u{fffd}\"}",
            "invalid UTF-8 in JSONL input",
        ),
        row(
            "not UTF-8, BOM",
            b"\xEF\xBB\xBF{\"a\": \"\xFF\"}\n",
            (1, 8),
            10..11,
            "{\"a\": \"\u{fffd}\"}",
            "invalid UTF-8 in JSONL input",
        ),
    ]
}

#[test]
fn jsonl_positions() {
    check("jsonl", &jsonl());
}

const TOKEN: &str = "found character that cannot start any token";
const BLOCK_MAPPING: &str = "did not find expected key, while parsing a block mapping";
const NO_LINE_BREAK: &str = "did not find expected comment or line break";

/// YAML whose error has a place. `repetition limit exceeded` has none, see
/// [`yaml_without_a_place`].
fn yaml() -> Vec<Row> {
    let mut rows = yaml_syntax();
    rows.extend(yaml_meaning());
    rows
}

fn yaml_syntax() -> Vec<Row> {
    let long = long("k: [\"", "\", @, \"", "\"]");
    vec![
        row(
            "flow sequence",
            "a: 1\nb: 2\nitems: [1, 2\nc: 3\n",
            (4, 2),
            24..24,
            "c: 3",
            "did not find expected ',' or ']', while parsing a flow sequence at line 3 column 8",
        ),
        row(
            "flow mapping",
            "a: {b: 1, c: ]}\n",
            (1, 14),
            13..13,
            "a: {b: 1, c: ]}",
            "did not find expected node content, while parsing a flow node",
        ),
        row(
            "multi-byte before",
            "имя: я😀 \"x\": y\n",
            (1, 19),
            18..18,
            "имя: я😀 \"x\": y",
            "mapping values are not allowed in this context",
        ),
        row(
            "multi-byte before, in flow",
            "a: 1\nя: {\"😀\": 1, b: @}\n",
            (2, 20),
            24..24,
            "я: {\"😀\": 1, b: @}",
            TOKEN,
        ),
        row("tab", "a: 1\nb:\t@x\n", (2, 4), 8..8, "b:\t@x", TOKEN),
        row(
            "tab as indentation",
            "a:\n\tb: 1\n",
            (2, 1),
            3..3,
            "\tb: 1",
            TOKEN,
        ),
        row("CRLF", "a: 1\r\nb: @x\r\n", (2, 4), 9..9, "b: @x", TOKEN),
        row("lone CR", "a: 1\rb: @x\r", (2, 4), 8..8, "b: @x", TOKEN),
        // libyaml ends a line at a lone CR always, dq where there is no LF.
        row(
            "lone CR among LF",
            "a: 1\rb: @x\nc: 2\n",
            (1, 9),
            8..8,
            "a: 1\rb: @x",
            TOKEN,
        ),
        // Line breaks to libyaml alone.
        row(
            "NEL",
            "a: 1\u{85}b: @x\n",
            (1, 10),
            9..9,
            "a: 1\u{85}b: @x",
            TOKEN,
        ),
        row(
            "LS in a scalar",
            "a: \"x\u{2028}y\"\nb: @x\n",
            (2, 4),
            14..14,
            "b: @x",
            TOKEN,
        ),
        row("BOM", "\u{feff}a: @x\n", (1, 4), 6..6, "a: @x", TOKEN),
        row(
            "BOM, second line",
            "\u{feff}a: 1\nb: @x\n",
            (2, 4),
            11..11,
            "b: @x",
            TOKEN,
        ),
        row(
            "end of file after LF",
            "a: [1,\n",
            (2, 1),
            7..7,
            "",
            "did not find expected node content",
        ),
        row(
            "end of file",
            "a: [1,",
            (1, 7),
            6..6,
            "a: [1,",
            "did not find expected node content",
        ),
        row(
            "end of file after CRLF",
            "a: [1,\r\n",
            (2, 1),
            8..8,
            "",
            "did not find expected node content",
        ),
        row(
            "end of file after CR",
            "a: [1,\r",
            (2, 1),
            7..7,
            "",
            "did not find expected node content",
        ),
        row(
            "end of file in a quoted scalar",
            "a: \"xyz\n",
            (2, 1),
            8..8,
            "",
            "found unexpected end of stream, while scanning a quoted scalar at line 1 column 4",
        ),
        row(
            "end of file in a quoted scalar of two lines",
            "я: \"a\nb",
            (2, 2),
            8..8,
            "b",
            "found unexpected end of stream, while scanning a quoted scalar at line 1 column 5",
        ),
        row(
            "long line",
            format!("{long}\n"),
            (1, 1499),
            1498..1498,
            long,
            TOKEN,
        ),
        row(
            "third document",
            "a: 1\n---\nя: 2\n---\nc: @x\n",
            (5, 4),
            22..22,
            "c: @x",
            TOKEN,
        ),
        row(
            "unclosed in the second document",
            "a: 1\n---\nb: [1\n---\nc: 1\n",
            (4, 1),
            15..15,
            "---",
            "did not find expected ',' or ']', while parsing a flow sequence at line 3 column 4",
        ),
        row(
            "no document start after a document end",
            "a: 1\n...\nb: @x\n",
            (3, 1),
            9..9,
            "b: @x",
            "did not find expected <document start>",
        ),
        row(
            "nested block value",
            "a: 1\n  b: 2\n",
            (2, 4),
            8..8,
            "  b: 2",
            "mapping values are not allowed in this context",
        ),
        row(
            "sequence dedented",
            "a:\n  - 1\n - 2\n",
            (3, 2),
            10..10,
            " - 2",
            BLOCK_MAPPING,
        ),
        row(
            "two anchors",
            "a: &x &y 1\n",
            (1, 7),
            6..6,
            "a: &x &y 1",
            BLOCK_MAPPING,
        ),
        row(
            "anchor name",
            "a: & 1\n",
            (1, 5),
            4..4,
            "a: & 1",
            "did not find expected alphabetic or numeric character, while scanning an anchor",
        ),
        row(
            "block scalar dedented",
            "a: |\n  x\n y: 1\n",
            (3, 2),
            10..10,
            " y: 1",
            BLOCK_MAPPING,
        ),
        row(
            "block scalar header",
            "a: |x\n  y\n",
            (1, 5),
            4..4,
            "a: |x",
            NO_LINE_BREAK,
        ),
        row(
            "folded scalar header",
            "я: >- z\n  y\n",
            (1, 8),
            7..7,
            "я: >- z",
            NO_LINE_BREAK,
        ),
        row(
            "block scalar, tab as indentation",
            "a: |\n\tx\n",
            (2, 1),
            5..5,
            "\tx",
            "found a tab character where an indentation space is expected",
        ),
        row(
            "after a block scalar",
            "a: |\n  я: [\n  more\nb: ]\n",
            (4, 4),
            23..23,
            "b: ]",
            "did not find expected node content, while parsing a block node",
        ),
        row(
            "escape",
            "a: \"я\\q\"\n",
            (1, 7),
            6..6,
            "a: \"я\\q\"",
            "found unknown escape character, while parsing a quoted scalar",
        ),
        row(
            "two values",
            "я: ю: c\n",
            (1, 7),
            6..6,
            "я: ю: c",
            "mapping values are not allowed in this context",
        ),
        row(
            "version",
            "%YAML 3.0\n---\na: 1\n",
            (1, 1),
            0..0,
            "%YAML 3.0",
            "found incompatible YAML document",
        ),
        row(
            "control character",
            "я: 1\nb: \u{1}\n",
            (2, 4),
            9..9,
            "b: \u{1}",
            "control characters are not allowed",
        ),
        row(
            "not UTF-8",
            b"a: 1\nb: x\xFF\n",
            (2, 5),
            9..10,
            "b: x\u{fffd}",
            "invalid leading UTF-8 octet",
        ),
        row(
            "not UTF-8, BOM",
            b"\xEF\xBB\xBFb: \xFF\n",
            (1, 4),
            6..7,
            "b: \u{fffd}",
            "invalid leading UTF-8 octet",
        ),
        row(
            "not UTF-8, overlong",
            b"\xD1\x8F: 1\nb: \xC0\x80\n",
            (2, 4),
            9..10,
            "b: \u{fffd}\u{fffd}",
            "invalid length of a UTF-8 sequence",
        ),
        row(
            "not UTF-8, a surrogate",
            b"\xD1\x8F: 1\nb: \xED\xA0\x80\n",
            (2, 4),
            9..10,
            "b: \u{fffd}\u{fffd}\u{fffd}",
            "invalid Unicode character",
        ),
        row(
            "not UTF-8, cut short",
            b"a: 1\nb: \xE2\x82",
            (2, 4),
            8..10,
            "b: \u{fffd}",
            "incomplete UTF-8 octet sequence",
        ),
        // libyaml names the byte after the lead for these three.
        row(
            "not UTF-8, Latin-1",
            b"name: caf\xE9 au lait\n",
            (1, 10),
            9..10,
            "name: caf\u{fffd} au lait",
            "invalid trailing UTF-8 octet",
        ),
        row(
            "not UTF-8, two bytes",
            b"\xD1\x8F: \"\xC3(\"\n",
            (1, 6),
            5..6,
            "я: \"\u{fffd}(\"",
            "invalid trailing UTF-8 octet",
        ),
        row(
            "not UTF-8, three bytes",
            b"a: 1\nb: \xE2\x82(\n",
            (2, 4),
            8..10,
            "b: \u{fffd}(",
            "invalid trailing UTF-8 octet",
        ),
        row(
            "not UTF-8, four bytes",
            b"b: \xF0\x9F\x98x\n",
            (1, 4),
            3..6,
            "b: \u{fffd}x",
            "invalid trailing UTF-8 octet",
        ),
        row(
            "not UTF-8, past U+10FFFF",
            b"b: \xF4\x90\x80\x80\n",
            (1, 4),
            3..4,
            "b: \u{fffd}\u{fffd}\u{fffd}\u{fffd}",
            "invalid Unicode character",
        ),
        // Past what libyaml reads at once: another fault comes first to it.
        row(
            "not UTF-8, past a repeated key",
            [&b"a: 1\n".repeat(5000)[..], b"b: \xFF\n"].concat(),
            (5001, 4),
            25_003..25_004,
            "b: \u{fffd}",
            "invalid leading UTF-8 octet",
        ),
        row(
            "not UTF-8, past a syntax error",
            [b"a: ]\n", &b"a: 1\n".repeat(5000)[..], b"b: \xE9 x\n"].concat(),
            (5002, 4),
            25_008..25_009,
            "b: \u{fffd} x",
            "invalid trailing UTF-8 octet",
        ),
        row(
            "not UTF-8, past a control character",
            b"a: \x01\nb: \xE9 x\n",
            (2, 4),
            8..9,
            "b: \u{fffd} x",
            "invalid trailing UTF-8 octet",
        ),
        // The place of a context, in the lines and bytes of the file.
        row(
            "context, multi-byte before",
            "я: [😀,\nb: 2\n",
            (3, 1),
            16..16,
            "",
            "did not find expected ',' or ']', while parsing a flow sequence at line 1 column 5",
        ),
        row(
            "context, past a line separator",
            "a: \"x\u{2028}y\u{85}\"\r\nя: [1\r\nb: 2\r\n",
            (3, 2),
            23..23,
            "b: 2",
            "did not find expected ',' or ']', while parsing a flow sequence at line 2 column 5",
        ),
        row(
            "UTF-16",
            b"\xFF\xFEa\0:\0",
            (1, 1),
            0..1,
            "\u{fffd}\u{fffd}a\0:\0",
            "invalid leading UTF-8 octet",
        ),
    ]
}

/// Errors past the scanner: anchors, tags and repeated keys.
fn yaml_meaning() -> Vec<Row> {
    vec![
        row(
            "unknown alias",
            "base: &b {x: 1}\nдругой: *nope\n",
            (2, 15),
            30..30,
            "другой: *nope",
            "unknown anchor",
        ),
        row(
            "alias of an earlier document",
            "a: &x 1\n---\nb: *x\n",
            (3, 4),
            15..15,
            "b: *x",
            "unknown anchor",
        ),
        row(
            "alias of itself",
            "a: &a [*a]\n",
            (1, 4),
            3..3,
            "a: &a [*a]",
            "recursion limit exceeded",
        ),
        row(
            "nested too deep",
            "[".repeat(300),
            (1, 129),
            128..128,
            "[".repeat(300),
            "recursion limit exceeded",
        ),
        row(
            "tag handle",
            "я: !e!x 1\n",
            (1, 5),
            4..4,
            "я: !e!x 1",
            "found undefined tag handle",
        ),
        row(
            "tagged scalar",
            "a: 1\n---\nя: !!int x\n",
            (3, 5),
            13..13,
            "я: !!int x",
            "я: invalid value: string \"x\", expected an integer",
        ),
        // The key of the second entry.
        row(
            "repeated key",
            "a: 1\nb: 2\na: 3\n",
            (3, 1),
            10..11,
            "a: 3",
            "duplicate entry with key \"a\"",
        ),
        row(
            "repeated key, nested and multi-byte",
            "x:\n  я: 1\n  \"я\": 2\n",
            (3, 3),
            13..17,
            "  \"я\": 2",
            "x: duplicate entry with key \"я\"",
        ),
        row(
            "repeated key after a tab",
            "{я: 1,\tя: 2}\n",
            (1, 9),
            8..10,
            "{я: 1,\tя: 2}",
            "duplicate entry with key \"я\"",
        ),
        row(
            "repeated key, second document",
            "x: 1\n---\na: 1\na: 2\n",
            (4, 1),
            14..15,
            "a: 2",
            "duplicate entry with key \"a\"",
        ),
        row(
            "repeated key, BOM",
            "\u{feff}a: 1\na: 2\n",
            (2, 1),
            8..9,
            "a: 2",
            "duplicate entry with key \"a\"",
        ),
        row(
            "repeated key, CRLF",
            "a: 1\r\na: 2\r\n",
            (2, 1),
            6..7,
            "a: 2",
            "duplicate entry with key \"a\"",
        ),
        row(
            "repeated key, lone CR",
            "a: 1\ra: 2\r",
            (2, 1),
            5..6,
            "a: 2",
            "duplicate entry with key \"a\"",
        ),
        // Read by the exact-number walk.
        row(
            "repeated key beside a big number",
            "x: 1e400\nb: {я: 1, \"я\": 2}\n",
            (2, 12),
            20..24,
            "b: {я: 1, \"я\": 2}",
            "b: duplicate entry with key \"я\"",
        ),
        row(
            "alias as a repeated key",
            "a: &a 1\nb: *a\nc: {*a : 1, *a : 2}\n",
            (3, 13),
            26..28,
            "c: {*a : 1, *a : 2}",
            "c: duplicate entry with key 1",
        ),
    ]
}

#[test]
fn yaml_positions() {
    check("yaml", &yaml());
}

/// An expansion past the limit is no one place: it is reported at the start.
#[test]
fn yaml_without_a_place() {
    let mut text = "a: &a [x, x, x, x, x, x, x, x, x, x]\n".to_owned();
    for (name, of) in ["b", "c", "d", "e", "f", "g", "h", "i"]
        .iter()
        .zip(["a", "b", "c", "d", "e", "f", "g", "h"])
    {
        let items = vec![format!("*{of}"); 10].join(", ");
        writeln!(text, "{name}: &{name} [{items}]").expect("write");
    }
    check(
        "yaml",
        &[row(
            "repetition limit",
            text,
            (1, 1),
            0..0,
            "a: &a [x, x, x, x, x, x, x, x, x, x]",
            "repetition limit exceeded",
        )],
    );
}

/// The entry point of the edits and of `lint`, which reads a file with
/// spans: the same errors, at the same places.
#[test]
fn yaml_positions_of_the_edit_entry_point() {
    check_with("yaml with spans", &yaml(), true, |input| {
        place(parse_yaml_stream_with_spans(input))
    });
}

const TOML_VALUE: &str = "string values must be quoted, expected literal string";

fn toml() -> Vec<Row> {
    let long = long("a = [\"", "\", @, \"", "\"]");
    vec![
        row(
            "no value",
            "a = 1\nb = \n",
            (2, 5),
            10..10,
            "b = ",
            TOML_VALUE,
        ),
        row(
            "multi-byte before",
            "a = 1\n\"я\" = \"😀\" x\n",
            (2, 15),
            20..21,
            "\"я\" = \"😀\" x",
            "unexpected key or value, expected newline, `#`",
        ),
        row(
            "multi-byte at fault",
            "a = 1\nя = 2\n",
            (2, 1),
            6..8,
            "я = 2",
            "invalid unquoted key",
        ),
        row(
            "tab",
            "a = 1\n\tb\t=\t@\n",
            (2, 6),
            11..12,
            "\tb\t=\t@",
            TOML_VALUE,
        ),
        row(
            "CRLF",
            "a = 1\r\nb = @\r\n",
            (2, 5),
            11..12,
            "b = @",
            TOML_VALUE,
        ),
        row(
            "no value, CRLF",
            "a = \r\n",
            (1, 5),
            4..4,
            "a = ",
            TOML_VALUE,
        ),
        // TOML reads no lone CR; the byte it wanted to be `\n`.
        row(
            "lone CR",
            "a = 1\rb = @\r",
            (2, 1),
            6..7,
            "b = @",
            "carriage return must be followed by newline",
        ),
        row("BOM", "\u{feff}a = @\n", (1, 5), 7..8, "a = @", TOML_VALUE),
        row(
            "BOM, second line",
            "\u{feff}a = 1\nb = @\n",
            (2, 5),
            13..14,
            "b = @",
            TOML_VALUE,
        ),
        row(
            "unclosed array",
            "a = [1,\n",
            (1, 8),
            7..7,
            "a = [1,",
            "unclosed array, expected `]`",
        ),
        row(
            "unclosed array, CRLF",
            "a = [1,\r\n",
            (1, 8),
            7..7,
            "a = [1,",
            "unclosed array, expected `]`",
        ),
        row(
            "unclosed array, no line break",
            "a = [1,",
            (1, 8),
            7..7,
            "a = [1,",
            "unclosed array, expected `]`",
        ),
        row(
            "end of file after LF",
            "a = \"\"\"я\n",
            (2, 1),
            10..10,
            "",
            "invalid multi-line basic string, expected `\"`",
        ),
        row(
            "end of file",
            "a = 1\nb",
            (2, 2),
            7..7,
            "b",
            "key with no value, expected `=`",
        ),
        row(
            "end of file, table",
            "a = 1\r\n\t[b",
            (2, 4),
            10..10,
            "\t[b",
            "unclosed table, expected `]`",
        ),
        row(
            "long line",
            format!("{long}\n"),
            (1, 1500),
            1499..1500,
            long,
            TOML_VALUE,
        ),
        row(
            "repeated key",
            "a = 1\nb = 2\na = 3\n",
            (3, 1),
            12..13,
            "a = 3",
            "duplicate key",
        ),
        row(
            "repeated key, quoted and multi-byte",
            "\"я\" = 1\r\n\"я\" = 2\r\n",
            (2, 1),
            10..14,
            "\"я\" = 2",
            "duplicate key",
        ),
        row(
            "repeated table",
            "[a]\n[a]\n",
            (2, 2),
            5..6,
            "[a]",
            "duplicate key",
        ),
        row(
            "repeated key, inline",
            "a = { x = 1, x = 2 }\n",
            (1, 14),
            13..14,
            "a = { x = 1, x = 2 }",
            "duplicate key",
        ),
        row(
            "escape",
            "a = \"я\\q\"\n",
            (1, 9),
            8..9,
            "a = \"я\\q\"",
            "missing escaped value",
        ),
        row(
            "date",
            "d = 2020-13-01\n",
            (1, 5),
            4..14,
            "d = 2020-13-01",
            "invalid date, expected month between 01 and 12",
        ),
        row(
            "integer too big",
            "n = 99999999999999999999\n",
            (1, 5),
            4..24,
            "n = 99999999999999999999",
            "integer number overflowed",
        ),
        row(
            "not UTF-8",
            b"a = 1\nb = \"x\xFF\"\n",
            (2, 7),
            12..13,
            "b = \"x\u{fffd}\"",
            "invalid UTF-8 in TOML input: invalid utf-8 sequence of 1 bytes",
        ),
        row(
            "not UTF-8, BOM",
            b"\xEF\xBB\xBFb = \"\xFF\"\n",
            (1, 6),
            8..9,
            "b = \"\u{fffd}\"",
            "invalid UTF-8 in TOML input",
        ),
    ]
}

#[test]
fn toml_positions() {
    check("toml", &toml());
}

const CLOSES_ANOTHER: &str = "XML parse error: closing tag </c> does not match <";
const NOT_CLOSED: &str = "XML parse error: element <b> is not closed";

/// XML in UTF-8, whose positions are those of its bytes.
fn xml() -> Vec<Row> {
    let long = long("<a><b>", "</c>", "</a>");
    vec![
        row(
            "closing tag",
            "<a>\n  <b></c>\n</a>\n",
            (2, 6),
            9..9,
            "  <b></c>",
            CLOSES_ANOTHER,
        ),
        row(
            "closing tag too many",
            "<a/>\n</b>\n",
            (2, 1),
            5..5,
            "</b>",
            "XML parse error: unexpected closing tag </b>",
        ),
        row(
            "multi-byte before",
            "<a>\n  я😀 &nope;\n</a>\n",
            (2, 10),
            13..13,
            "  я😀 &nope;",
            "XML parse error: undeclared entity reference '&nope;'",
        ),
        row(
            "multi-byte before, in a tag",
            "<a>\n  <я x=\"😀\"></ю>\n</a>\n",
            (2, 16),
            19..19,
            "  <я x=\"😀\"></ю>",
            "XML parse error: closing tag </ю> does not match <я>",
        ),
        row(
            "tab",
            "<a>\n\t<b>\t</c>\n</a>\n",
            (2, 6),
            9..9,
            "\t<b>\t</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "CRLF",
            "<a>\r\n  <b></c>\r\n</a>\r\n",
            (2, 6),
            10..10,
            "  <b></c>",
            CLOSES_ANOTHER,
        ),
        row(
            "lone CR",
            "<a>\r  <b></c>\r</a>\r",
            (2, 6),
            9..9,
            "  <b></c>",
            CLOSES_ANOTHER,
        ),
        row(
            "lone CR among LF",
            "<a>\r<b>\n</c>\n</a>",
            (2, 1),
            8..8,
            "</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "BOM",
            "\u{feff}<a></c>\n",
            (1, 4),
            6..6,
            "<a></c>",
            CLOSES_ANOTHER,
        ),
        row(
            "BOM, second line",
            "\u{feff}<a>\n</c>\n",
            (2, 1),
            7..7,
            "</c>",
            CLOSES_ANOTHER,
        ),
        // At the end of the file, the element left open.
        row(
            "end of file after LF",
            "<a>\n  <b>\n",
            (2, 3),
            6..6,
            "  <b>",
            NOT_CLOSED,
        ),
        row(
            "end of file",
            "<a>\n  <b>",
            (2, 3),
            6..6,
            "  <b>",
            NOT_CLOSED,
        ),
        row(
            "end of file after CRLF",
            "<a>\r\n  <b>\r\n",
            (2, 3),
            7..7,
            "  <b>",
            NOT_CLOSED,
        ),
        row(
            "end of file after CR",
            "<a>\r  <b>\r",
            (2, 3),
            6..6,
            "  <b>",
            NOT_CLOSED,
        ),
        row(
            "end of file in a tag",
            "<a>\n  <b",
            (2, 3),
            6..6,
            "  <b",
            "XML parse error: the tag has no closing '>'",
        ),
        row(
            "end of file in a comment",
            "<a>\n <!-- я\n</a>\n",
            (2, 2),
            5..5,
            " <!-- я",
            "XML parse error: the comment has no closing '-->'",
        ),
        row(
            "empty",
            "",
            (1, 1),
            0..0,
            "",
            "XML parse error: a document must have exactly one root element; found 0",
        ),
        row(
            "BOM alone",
            BOM,
            (1, 1),
            3..3,
            "",
            "XML parse error: a document must have exactly one root element; found 0",
        ),
        row(
            "blank line alone",
            "  \n",
            (2, 1),
            3..3,
            "",
            "XML parse error: a document must have exactly one root element; found 0",
        ),
        row(
            "long line",
            format!("{long}\n"),
            (1, 1497),
            1496..1496,
            long,
            CLOSES_ANOTHER,
        ),
        row(
            "two roots",
            "<a/>\n<b/>\n",
            (2, 1),
            5..5,
            "<b/>",
            "XML parse error: a document must have exactly one root element; <b> is a second",
        ),
        row(
            "repeated attribute",
            "<a>\r\n<b x=\"я\"\r\n   x=\"3\"/>\r\n</a>",
            (3, 4),
            19..19,
            "   x=\"3\"/>",
            "XML parse error: attribute 'x' appears twice on <b>",
        ),
        row(
            "attribute value not quoted",
            "<a>\n <я x=1/>\n</a>\n",
            (2, 8),
            11..11,
            " <я x=1/>",
            "XML parse error: the value of attribute 'x' on <я> must be quoted",
        ),
        row(
            "control character",
            "<a>я\u{1}</a>\n",
            (1, 6),
            5..5,
            "<a>я\u{1}</a>",
            "XML parse error: the document holds U+0001",
        ),
        row(
            "entity past a DOCTYPE",
            "<!DOCTYPE a [\n<!ENTITY x \"y\">\n]>\n<a>&x;</c>",
            (4, 4),
            36..36,
            "<a>&x;</c>",
            "XML parse error: undeclared entity reference '&x;'",
        ),
        row(
            "prefix",
            "<a>\n <я:b/>\n</a>",
            (2, 3),
            6..6,
            " <я:b/>",
            "XML parse error: element <я:b> uses the prefix 'я'",
        ),
        row(
            "late declaration",
            " <?xml version=\"1.0\"?><a/>\n",
            (1, 2),
            1..1,
            " <?xml version=\"1.0\"?><a/>",
            "XML parse error: an XML declaration may stand only at the very start",
        ),
        row(
            "declared version",
            "<?xml version=\"2\"?>\n<a/>",
            (1, 16),
            15..15,
            "<?xml version=\"2\"?>",
            "XML parse error: the XML declaration's version \"2\"",
        ),
        row(
            "declared UTF-8",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<a>я</c>",
            (2, 6),
            45..45,
            "<a>я</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "unknown encoding",
            "<?xml version=\"1.0\" encoding=\"nope\"?>\n<a/>",
            (1, 31),
            30..30,
            "<?xml version=\"1.0\" encoding=\"nope\"?>",
            "XML parse error: the XML declaration's encoding \"nope\"",
        ),
        row(
            "encoding against the BOM",
            "\u{feff}<?xml version=\"1.0\" encoding=\"latin1\"?><a/>",
            (1, 31),
            33..33,
            "<?xml version=\"1.0\" encoding=\"latin1\"?><a/>",
            "XML parse error: the file begins with a UTF-8 byte order mark",
        ),
        row(
            "not UTF-8",
            b"<a>\n  x\xFF\n</a>\n",
            (2, 4),
            7..8,
            "  x\u{fffd}",
            "invalid UTF-8 in XML input",
        ),
        row(
            "not UTF-8, BOM",
            b"\xEF\xBB\xBF<a>\xFF</a>\n",
            (1, 4),
            6..7,
            "<a>\u{fffd}</a>",
            "invalid UTF-8 in XML input",
        ),
        row(
            "not US-ASCII",
            b"<?xml version='1.0' encoding='US-ASCII'?>\n<a>\xE9</a>",
            (2, 4),
            45..46,
            "<a>\u{fffd}</a>",
            "invalid US-ASCII in XML input: byte 0xE9",
        ),
    ]
}

#[test]
fn xml_positions() {
    check("xml", &xml());
}

/// `text` in UTF-16, `big` endian or not, behind the BOM of that order.
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for unit in std::iter::once(0xFEFF).chain(text.encode_utf16()) {
        bytes.extend(if big {
            unit.to_be_bytes()
        } else {
            unit.to_le_bytes()
        });
    }
    bytes
}

/// XML in an encoding that is not UTF-8: the line, the column (in the bytes
/// of the UTF-8 text) and the source line are those of the text the file
/// holds, the span is the file's.
fn xml_in_another_encoding() -> Vec<Row> {
    let declared = |label: &str, rest: &[u8]| {
        let mut bytes = format!("<?xml version=\"1.0\" encoding=\"{label}\"?>\n").into_bytes();
        bytes.extend(rest);
        bytes
    };
    let mut lone = utf16("<a>я", false);
    lone.extend(b"\x00\xD8<\0/\0a\0>\0");
    let mut odd = utf16("<a/>", false);
    odd.push(b'\n');
    vec![
        row(
            "UTF-16LE",
            utf16("<a>\n  я</c>\n</a>\n", false),
            (2, 5),
            16..16,
            "  я</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "UTF-16BE, two units",
            utf16("<a>\n  я😀</c>\n</a>\n", true),
            (2, 9),
            20..20,
            "  я😀</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "UTF-16, CRLF and tab",
            utf16("<a>\r\n\t<b>я</c>\r\n</a>", false),
            (2, 7),
            22..22,
            "\t<b>я</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "UTF-16, lone CR",
            utf16("<a>\r<b></c>\r</a>", true),
            (2, 4),
            16..16,
            "<b></c>",
            CLOSES_ANOTHER,
        ),
        row(
            "UTF-16, declared encoding",
            utf16("<?xml version=\"1.0\" encoding=\"nope\"?><a/>", false),
            (1, 31),
            62..62,
            "<?xml version=\"1.0\" encoding=\"nope\"?><a/>",
            "XML parse error: the XML declaration's encoding \"nope\"",
        ),
        row(
            "UTF-16, no BOM",
            &utf16(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<a>я</c>",
                true,
            )[2..],
            (2, 6),
            88..88,
            "<a>я</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "UTF-16, lone surrogate",
            lone,
            (1, 6),
            10..10,
            "<a>я\u{fffd}</a>",
            "invalid UTF-16LE in XML input",
        ),
        row(
            "UTF-16, half a unit",
            odd,
            (1, 5),
            10..10,
            "<a/>\u{fffd}",
            "invalid UTF-16LE in XML input",
        ),
        row(
            "ISO-8859-1",
            declared("ISO-8859-1", b"<a>\xE9\x85</c>"),
            (2, 8),
            49..49,
            "<a>é\u{85}</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "windows-1251",
            declared("windows-1251", b"<a>\xFF\xFF\n</c>"),
            (3, 1),
            52..52,
            "</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "Shift_JIS",
            declared("Shift_JIS", b"<a>\x82\xA0</c>"),
            (2, 7),
            48..48,
            "<a>あ</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "GBK",
            declared("GBK", b"<a>\xD6\xD0\xCE\xC4</c>"),
            (2, 10),
            44..44,
            "<a>中文</c>",
            CLOSES_ANOTHER,
        ),
        row(
            "Shift_JIS, malformed",
            declared("Shift_JIS", b"<a>\x82\xA0\x81</a>"),
            (2, 7),
            48..48,
            "<a>あ\u{fffd}</a>",
            "invalid Shift_JIS in XML input",
        ),
        // At the escape sequence ahead of the character.
        row(
            "ISO-2022-JP",
            declared("ISO-2022-JP", b"<a>\x1B$B$\"\x1B(B</c>"),
            (2, 7),
            53..53,
            "<a>あ</c>",
            CLOSES_ANOTHER,
        ),
    ]
}

#[test]
fn xml_positions_in_another_encoding() {
    check_with("xml", &xml_in_another_encoding(), false, |input| {
        parse("xml", input)
    });
}

const EXPRESSION: &str = "invalid expression; expected";

fn hcl() -> Vec<Row> {
    let long = long("a = [\"", "\", =, \"", "\"]");
    vec![
        row(
            "unclosed list",
            "a = 1\nb = [1, 2\nc = 3\n",
            (3, 1),
            16..16,
            "c = 3",
            "expected `]`",
        ),
        row(
            "multi-byte before",
            "a = 1\nb = \"я😀\" x\n",
            (2, 14),
            19..19,
            "b = \"я😀\" x",
            "expected newline or eof",
        ),
        row(
            "multi-byte at fault",
            "я = 1\na = 😀\n",
            (2, 5),
            11..11,
            "a = 😀",
            EXPRESSION,
        ),
        row(
            "tab",
            "a = 1\n\tb\t=\t=\n",
            (2, 6),
            11..11,
            "\tb\t=\t=",
            EXPRESSION,
        ),
        row(
            "CRLF",
            "a = 1\r\nb = =\r\n",
            (2, 5),
            11..11,
            "b = =",
            EXPRESSION,
        ),
        row(
            "CRLF, unclosed list",
            "я = 1\r\nb = [1,\r\nc = 2\r\n",
            (3, 3),
            19..19,
            "c = 2",
            "expected `]`",
        ),
        // HCL reads no lone CR.
        row(
            "lone CR",
            "a = 1\rb = =\r",
            (1, 6),
            5..5,
            "a = 1",
            "expected newline or eof",
        ),
        // Nor a byte order mark.
        row(
            "BOM",
            "\u{feff}a = =\n",
            (1, 1),
            0..0,
            "a = =",
            "unexpected token",
        ),
        row(
            "end of file after LF",
            "a = [1\n",
            (2, 1),
            7..7,
            "",
            "expected `]`",
        ),
        row(
            "end of file",
            "a = [1, я",
            (1, 11),
            10..10,
            "a = [1, я",
            "expected `]`",
        ),
        row(
            "end of file after CRLF",
            "a = [1\r\n",
            (2, 1),
            8..8,
            "",
            "expected `]`",
        ),
        row(
            "end of file in a block",
            "a {\n  b = 1\n",
            (3, 1),
            12..12,
            "",
            "invalid block body",
        ),
        row(
            "end of file in a block, no line break",
            "a {\n  b = 1",
            (2, 8),
            11..11,
            "  b = 1",
            "invalid block body",
        ),
        row(
            "long line",
            format!("{long}\n"),
            (1, 1500),
            1499..1499,
            long,
            EXPRESSION,
        ),
        row(
            "no value",
            "x {\n  я = 1\n  b = \n}\n",
            (3, 7),
            19..19,
            "  b = ",
            EXPRESSION,
        ),
        // A string left open: hcl-edit backtracks to the attribute.
        row(
            "unclosed string",
            "a = 1\nb = \"xyz\n",
            (2, 1),
            6..6,
            "b = \"xyz",
            "unexpected token",
        ),
        row(
            "unclosed interpolation",
            "a = 1\nя = \"${foo\"\nc = 1\n",
            (2, 12),
            17..17,
            "я = \"${foo\"",
            "expected `}`",
        ),
        row(
            "heredoc",
            "x = \"я\"\na = <<EOT\nx\ny",
            (3, 1),
            19..19,
            "x",
            "expected heredoc end delimiter",
        ),
        row(
            "repeated attribute",
            "x = \"я\"\nb {\n  a = \"😀\"\n  a = 2\n}\n",
            (4, 3),
            28..28,
            "  a = 2",
            "invalid attribute; expected unique attribute key",
        ),
        row(
            "attribute and block",
            "я = 1\na = 1\na {\n}\n",
            (3, 1),
            13..18,
            "a {",
            "HCL error: block \"a\" has the name of an attribute of the same body",
        ),
        row(
            "label counts",
            "a \"я\" {\n}\na {\n}\n",
            (3, 1),
            11..16,
            "a {",
            "HCL error: block \"a\" has 0 labels here and 1 label",
        ),
        row(
            "repeated object key",
            "я = 1\nx = { a = 1, \"a\" = 2 }\n",
            (2, 14),
            20..23,
            "x = { a = 1, \"a\" = 2 }",
            "invalid object item; expected unique object key",
        ),
        row(
            "not UTF-8",
            b"a = 1\nb = \"\xFF\"\n",
            (2, 6),
            11..12,
            "b = \"\u{fffd}\"",
            "invalid UTF-8 in HCL input",
        ),
    ]
}

#[test]
fn hcl_positions() {
    check("hcl", &hcl());
}

/// CSV, and TSV when `d` is a tab: the same inputs fail alike, their
/// messages naming the format.
fn delimited(d: char) -> Vec<Row> {
    let text = |text: &str| text.replace(',', &d.to_string());
    let bytes = |bytes: &[u8]| -> Vec<u8> {
        let delimiter = u8::try_from(d).expect("ASCII");
        bytes
            .iter()
            .map(|&b| if b == b',' { delimiter } else { b })
            .collect()
    };
    let (first, second, header) = if d == ',' {
        (
            "CSV record 0: CSV ",
            "CSV record 1: CSV error: found record",
            "failed to read CSV header: ",
        )
    } else {
        (
            "TSV record 0: CSV ",
            "TSV record 1: CSV error: found record",
            "failed to read TSV header: ",
        )
    };
    let long = format!("{},{},z", "x".repeat(1500), "y".repeat(1500));
    vec![
        row(
            "short record",
            text("a,b\n1,2\n3\n"),
            (3, 1),
            8..8,
            "3",
            second,
        ),
        row(
            "long record, multi-byte",
            text("я,b\n1,2\nя,😀,x\n4,5\n"),
            (3, 1),
            9..9,
            text("я,😀,x"),
            second,
        ),
        row(
            "CRLF",
            text("a,b\r\n1,2\r\n3\r\n"),
            (3, 1),
            10..10,
            "3",
            second,
        ),
        row("lone CR", text("a,b\r1,2\r3\r"), (3, 1), 8..8, "3", second),
        // A lone CR ends a record, but a line in a file with no LF only.
        row(
            "lone CR among LF",
            text("a,b\n1,2\r3\n"),
            (2, 5),
            8..8,
            text("1,2\r3"),
            second,
        ),
        row("BOM", text("\u{feff}a,b\n1\n"), (2, 1), 7..7, "1", first),
        row(
            "blank lines",
            text("a,b\n\n\n3\n"),
            (4, 1),
            6..6,
            "3",
            first,
        ),
        row(
            "blank lines, CRLF",
            text("a,b\r\n1,2\r\n\r\n3\r\n"),
            (4, 1),
            12..12,
            "3",
            second,
        ),
        row(
            "blank lines, lone CR",
            text("a,b\r\r3\r"),
            (3, 1),
            5..5,
            "3",
            first,
        ),
        row(
            "blank lines above",
            text("\n\na,b\n3\n"),
            (4, 1),
            6..6,
            "3",
            first,
        ),
        row(
            "after a record of two lines",
            text("a,b\n\"x\ny\",2\n3\n"),
            (4, 1),
            12..12,
            "3",
            second,
        ),
        row(
            "record of two lines",
            text("a,b\n1,2\n\"я\r\nя\",2,3\n"),
            (3, 1),
            8..8,
            "\"я",
            second,
        ),
        row("end of file", text("a,b\n3"), (2, 1), 4..4, "3", first),
        row(
            "unclosed quote",
            text("a,b\n\"x,2\n3,4\n"),
            (2, 1),
            4..4,
            text("\"x,2"),
            first,
        ),
        row(
            "long line",
            text(&format!("a,b\n{long}\n")),
            (2, 1),
            4..4,
            text(&long),
            first,
        ),
        row(
            "not UTF-8",
            bytes(b"a,b\n\xD1\x8F,x\xFF\n"),
            (2, 5),
            8..9,
            text("я,x\u{fffd}"),
            first,
        ),
        row(
            "not UTF-8, header",
            bytes(b"a,\xFF\n1,2\n"),
            (1, 3),
            2..3,
            text("a,\u{fffd}"),
            header,
        ),
        row(
            "not UTF-8, header, BOM",
            bytes(b"\xEF\xBB\xBFa,\xFF\n"),
            (1, 3),
            5..6,
            text("a,\u{fffd}"),
            header,
        ),
        row(
            "not UTF-8, in a quoted field of two lines",
            bytes(b"a,b\n\"\xD1\x8F\n\xFF\",2\n"),
            (3, 1),
            8..9,
            text("\u{fffd}\",2"),
            first,
        ),
        row(
            "not UTF-8, CRLF and a blank line",
            bytes(b"a,b\r\n\r\n1,\xFF\r\n"),
            (3, 3),
            9..10,
            text("1,\u{fffd}"),
            first,
        ),
        row(
            "not UTF-8, cut short",
            bytes(b"a,b\n1,\xE2\x82"),
            (2, 3),
            6..8,
            text("1,\u{fffd}"),
            first,
        ),
    ]
}

#[test]
fn csv_positions() {
    check("csv", &delimited(','));
}

#[test]
fn tsv_positions() {
    check("tsv", &delimited('\t'));
}

const PAIR: &str = "expected `KEY=VALUE` in .env line";
const DOUBLE: &str = "unterminated double-quoted value";
const SINGLE: &str = "unterminated single-quoted value";

fn dotenv() -> Vec<Row> {
    let long = format!("{}я=\"{}", "x".repeat(1500), "y".repeat(1500));
    vec![
        row("no value", "A=1\nfoo bar\n", (2, 1), 4..11, "foo bar", PAIR),
        row("tab", "\tA\n", (1, 2), 1..2, "\tA", PAIR),
        row(
            "multi-byte before",
            "A=1\nя=\"😀\n",
            (2, 4),
            7..12,
            "я=\"😀",
            DOUBLE,
        ),
        row("CRLF", "A=1\r\nB=\"x\r\n", (2, 3), 7..9, "B=\"x", DOUBLE),
        // Entries end at LF only; the position has the lines of the file.
        row("lone CR", "A=\"x\rB=1\r", (1, 3), 2..8, "A=\"x", DOUBLE),
        row(
            "lone CR among LF",
            "A=1\r\nB\rC\r\n",
            (2, 1),
            5..8,
            "B\rC",
            PAIR,
        ),
        row("BOM", "\u{feff}A=\"x\n", (1, 3), 5..7, "A=\"x", DOUBLE),
        row(
            "BOM, second line",
            "\u{feff}A=1\nB\n",
            (2, 1),
            7..8,
            "B",
            PAIR,
        ),
        row(
            "empty key",
            "A=1\n  =x\n",
            (2, 3),
            6..7,
            "  =x",
            "empty key in .env line",
        ),
        row(
            "empty key, exported",
            "export  =x\n",
            (1, 9),
            8..9,
            "export  =x",
            "empty key in .env line",
        ),
        row("export alone", "export A\n", (1, 1), 0..8, "export A", PAIR),
        row(
            "single quote",
            "  я = 'ю  \n",
            (1, 8),
            7..12,
            "  я = 'ю  ",
            SINGLE,
        ),
        row("end of file", "A=1\nB='x", (2, 3), 6..8, "B='x", SINGLE),
        row(
            "long line",
            format!("A=1\n{long}\n"),
            (2, 1504),
            1507..3008,
            long,
            DOUBLE,
        ),
        row(
            "not UTF-8",
            b"A=1\nB=\xFF\n",
            (2, 3),
            6..7,
            "B=\u{fffd}",
            "invalid UTF-8 in .env input",
        ),
    ]
}

#[test]
fn dotenv_positions() {
    check("dotenv", &dotenv());
}

const ESCAPE: &str = "unknown character in \\xHH form";

fn ini() -> Vec<Row> {
    let long = long("a=", "\\xzzzz", "");
    vec![
        row(
            "empty key",
            "[s]\na=1\n  = 2\n",
            (3, 3),
            10..11,
            "  = 2",
            "missing key",
        ),
        row(
            "multi-byte before",
            "😀=1\n😀=\"😀\\xzzzz\"\n",
            (2, 16),
            22..23,
            "😀=\"😀\\xzzzz\"",
            ESCAPE,
        ),
        row(
            "tab",
            "[s]\nя=1\n\t=\t2\n",
            (3, 2),
            10..11,
            "\t=\t2",
            "missing key",
        ),
        row(
            "CRLF",
            "a=1\r\n\r\n; c\r\n= 2\r\n",
            (4, 1),
            12..13,
            "= 2",
            "missing key",
        ),
        row(
            "CRLF, at a line break",
            "я=\\x12z\r\nb=1\r\n",
            (1, 9),
            8..9,
            "я=\\x12z",
            ESCAPE,
        ),
        row(
            "lone CR",
            "[s]\ra=1\r= 2\r",
            (3, 1),
            8..9,
            "= 2",
            "missing key",
        ),
        row(
            "BOM, second line",
            "\u{feff}a=1\n= 2\n",
            (2, 1),
            7..8,
            "= 2",
            "missing key",
        ),
        // Unterminated: at the end of the file, its start is not known.
        row(
            "end of file in a section name",
            "[s]\nя=1\n[t",
            (3, 3),
            11..11,
            "[t",
            "expecting \"[Some(']')]\" but found EOF.",
        ),
        row(
            "end of file in a section name, after LF",
            "[s]\na=1\n[t\n",
            (4, 1),
            11..11,
            "",
            "expecting \"[Some(']')]\" but found EOF.",
        ),
        row(
            "end of file after a key",
            "[s]\nя\n",
            (3, 1),
            7..7,
            "",
            "expecting \"[Some('='), Some(':')]\" but found EOF.",
        ),
        row(
            "end of file in a quoted value",
            "я='x\n",
            (2, 1),
            6..6,
            "",
            "expecting \"[Some('\\'')]\" but found EOF.",
        ),
        row(
            "end of file in an escape",
            "a=1\nя=\\x1",
            (2, 7),
            10..10,
            "я=\\x1",
            "expecting \"",
        ),
        row(
            "long line",
            format!("{long}\n"),
            (1, 1498),
            1497..1498,
            long,
            ESCAPE,
        ),
        row(
            "not UTF-8",
            b"[s]\na=\xFF\n",
            (2, 3),
            6..7,
            "a=\u{fffd}",
            "invalid UTF-8 in INI input",
        ),
        row(
            "not UTF-8, BOM",
            b"\xEF\xBB\xBFa=\xFF\n",
            (1, 3),
            5..6,
            "a=\u{fffd}",
            "invalid UTF-8 in INI input",
        ),
    ]
}

#[test]
fn ini_positions() {
    check("ini", &ini());
}

const UNKNOWN: &str = "unknown instruction `FRUM`";

/// Dockerfile: the whole first line of the instruction at fault.
fn dockerfile() -> Vec<Row> {
    let long = format!("FRUM {}", "x".repeat(3000));
    vec![
        row(
            "instruction",
            "FROM a\nFRUM b\n",
            (2, 1),
            7..13,
            "FRUM b",
            UNKNOWN,
        ),
        row(
            "multi-byte, continued",
            "FROM я\n  FRUM я \\\n  b\n",
            (2, 1),
            8..19,
            "  FRUM я \\",
            UNKNOWN,
        ),
        row(
            "tab",
            "FROM a\n\tFRUM b\n",
            (2, 1),
            7..14,
            "\tFRUM b",
            UNKNOWN,
        ),
        row(
            "CRLF",
            "FROM a\r\nFRUM b\r\n",
            (2, 1),
            8..14,
            "FRUM b",
            UNKNOWN,
        ),
        row("BOM", "\u{feff}FRUM a\n", (1, 1), 3..9, "FRUM a", UNKNOWN),
        row(
            "BOM, second line",
            "\u{feff}FROM a\nFRUM b\n",
            (2, 1),
            10..16,
            "FRUM b",
            UNKNOWN,
        ),
        row(
            "end of file",
            "FROM a\nFRUM b",
            (2, 1),
            7..13,
            "FRUM b",
            UNKNOWN,
        ),
        row(
            "no argument",
            "FROM a\nCOPY\n",
            (2, 1),
            7..11,
            "COPY",
            "COPY requires at least one argument",
        ),
        row(
            "heredoc",
            "FROM a\nRUN <<EOF\nx\n",
            (2, 1),
            7..16,
            "RUN <<EOF",
            "unterminated heredoc `EOF`",
        ),
        row(
            "repeated directive",
            "# escape=`\n# escape=`\nFROM a\n",
            (2, 1),
            11..21,
            "# escape=`",
            "only one `escape` parser directive can be used",
        ),
        row(
            "directive",
            "# escape=x\nFROM a\n",
            (1, 1),
            0..10,
            "# escape=x",
            "invalid escape token `x`",
        ),
        row(
            "long line",
            format!("FROM a\n{long}\n"),
            (2, 1),
            7..3012,
            long,
            UNKNOWN,
        ),
        row(
            "not UTF-8",
            b"FROM a\nRUN \xFF\n",
            (2, 5),
            11..12,
            "RUN \u{fffd}",
            "invalid UTF-8 in Dockerfile input",
        ),
    ]
}

#[test]
fn dockerfile_positions() {
    check("dockerfile", &dockerfile());
}

/// An ignore list is any text: only bytes that are no UTF-8 fail.
fn ignore_list() -> Vec<Row> {
    let message = "invalid UTF-8 in ignore-list input";
    vec![
        row(
            "not UTF-8",
            b"a\nb\xFF\n",
            (2, 2),
            3..4,
            "b\u{fffd}",
            message,
        ),
        row(
            "not UTF-8, BOM",
            b"\xEF\xBB\xBFb\xFF\n",
            (1, 2),
            4..5,
            "b\u{fffd}",
            message,
        ),
        row(
            "not UTF-8, lone CR",
            b"a\rb\xFF\r",
            (2, 2),
            3..4,
            "b\u{fffd}",
            message,
        ),
        row(
            "not UTF-8, CRLF, cut short",
            b"a\r\n\xE2\x82",
            (2, 1),
            3..5,
            "\u{fffd}",
            message,
        ),
    ]
}

#[test]
fn ignore_list_positions() {
    check("ignore-list", &ignore_list());
}

/// Front matter: the place in the file, not in the header.
fn front_matter() -> Vec<Row> {
    let long = long("a: [\"", "\", @, \"", "\"]");
    vec![
        row(
            "YAML",
            "---\ntitle: я\nя: @x\n---\n# Body\n",
            (3, 5),
            18..18,
            "я: @x",
            TOKEN,
        ),
        // The place a YAML message names for its context is the file's too.
        row(
            "YAML, context",
            "---\na: 1\nb: [x\n---\n# Body\n",
            (4, 1),
            15..15,
            "---",
            "did not find expected ',' or ']', while parsing a flow sequence at line 3 column 4",
        ),
        row(
            "YAML, repeated key",
            "---\na: 1\na: 2\n---\n# Body\n",
            (3, 1),
            9..10,
            "a: 2",
            "duplicate entry with key \"a\"",
        ),
        row(
            "YAML, CRLF",
            "---\r\na: 1\r\nb: @x\r\n---\r\n",
            (3, 4),
            14..14,
            "b: @x",
            TOKEN,
        ),
        row(
            "YAML, CRLF, context",
            "---\r\na: 1\r\nb: [x\r\n---\r\n# Body\r\n",
            (4, 1),
            18..18,
            "---",
            "did not find expected ',' or ']', while parsing a flow sequence at line 3 column 4",
        ),
        row(
            "YAML, BOM",
            "\u{feff}---\na: @x\n---\n",
            (2, 4),
            10..10,
            "a: @x",
            TOKEN,
        ),
        row(
            "YAML, tab",
            "---\na:\t@x\n---\n",
            (2, 4),
            7..7,
            "a:\t@x",
            TOKEN,
        ),
        row(
            "YAML, long line",
            format!("---\n{long}\n---\nb\n"),
            (2, 1499),
            1502..1502,
            long,
            TOKEN,
        ),
        row(
            "TOML",
            "+++\na = 1\n\"я\" = @\n+++\n# B\n",
            (3, 8),
            17..18,
            "\"я\" = @",
            TOML_VALUE,
        ),
        // The header ends where its closing line begins.
        row(
            "TOML, unclosed array",
            "+++\na = [1\n+++\n# B\n",
            (2, 7),
            10..10,
            "a = [1",
            "unclosed array, expected `]`",
        ),
        row(
            "TOML, BOM, CRLF",
            "\u{feff}+++\r\na = @\r\n+++\r\n",
            (2, 5),
            12..13,
            "a = @",
            TOML_VALUE,
        ),
        row(
            "JSON",
            "{\n  \"a\": 1,\n  \"я\": @\n}\n\n# Body\n",
            (3, 9),
            20..21,
            "  \"я\": @",
            VALUE,
        ),
        row(
            "JSON, BOM",
            "\u{feff}{\"я\": \"😀\", x}\n\n# Body\n",
            (1, 16),
            18..19,
            "{\"я\": \"😀\", x}",
            "expected string key in object",
        ),
        row(
            "YAML, not UTF-8",
            b"---\na: \xFF\n---\n",
            (2, 4),
            7..8,
            "a: \u{fffd}",
            "invalid leading UTF-8 octet",
        ),
        row(
            "YAML, not UTF-8, Latin-1",
            b"---\ntitle: caf\xE9 au lait\n---\n",
            (2, 11),
            14..15,
            "title: caf\u{fffd} au lait",
            "invalid trailing UTF-8 octet",
        ),
        row(
            "YAML, not UTF-8, a repeated key",
            b"---\na: 1\na: 2\nb: \xE9 x\n---\n",
            (4, 4),
            17..18,
            "b: \u{fffd} x",
            "invalid trailing UTF-8 octet",
        ),
        row(
            "TOML, not UTF-8",
            b"+++\na = \"\xFF\"\n+++\n",
            (2, 6),
            9..10,
            "a = \"\u{fffd}\"",
            "invalid UTF-8 in TOML input",
        ),
        row(
            "JSON, not UTF-8",
            b"{\n\"a\": \"\xFF\"\n}\n\n# B\n",
            (2, 7),
            8..9,
            "\"a\": \"\u{fffd}\"",
            "source is not valid UTF-8",
        ),
    ]
}

#[test]
fn front_matter_positions() {
    check("frontmatter", &front_matter());
}

/// Markdown reads the same headers, and a body of its own.
#[test]
fn markdown_positions() {
    let mut rows = front_matter();
    let message = "invalid UTF-8 in Markdown input";
    rows.extend([
        row(
            "body, not UTF-8",
            b"---\na: 1\n---\n# B\xFF\n",
            (4, 4),
            16..17,
            "# B\u{fffd}",
            message,
        ),
        row(
            "body alone, not UTF-8, lone CR",
            b"# B\rx\xFF\r",
            (2, 2),
            5..6,
            "x\u{fffd}",
            message,
        ),
        row(
            "body, not UTF-8, BOM",
            b"\xEF\xBB\xBF# B\xFF\n",
            (1, 4),
            6..7,
            "# B\u{fffd}",
            message,
        ),
    ]);
    check("markdown", &rows);
}

/// These formats read an empty input: nothing to place.
#[test]
fn an_empty_input_is_no_error_elsewhere() {
    for format in [
        "jsonl",
        "yaml",
        "toml",
        "hcl",
        "csv",
        "tsv",
        "dotenv",
        "ini",
        "dockerfile",
        "ignore-list",
        "frontmatter",
        "markdown",
    ] {
        by_name(format)
            .expect("known format")
            .parse(b"")
            .unwrap_or_else(|e| panic!("{format}: {e}"));
    }
}

/// Every row whose positions are those of its bytes.
fn corpus() -> Vec<(&'static str, Vec<Row>)> {
    vec![
        ("json", json()),
        ("jsonl", jsonl()),
        ("yaml", yaml()),
        ("toml", toml()),
        ("xml", xml()),
        ("hcl", hcl()),
        ("csv", delimited(',')),
        ("tsv", delimited('\t')),
        ("dotenv", dotenv()),
        ("ini", ini()),
        ("dockerfile", dockerfile()),
        ("ignore-list", ignore_list()),
        ("frontmatter", front_matter()),
    ]
}

/// Run `change` on every row of the corpus and check the ones it makes a new
/// row of; at least `floor` of them.
fn check_changed(floor: usize, change: impl Fn(&str, &Row) -> Option<Row>) {
    let mut checked = 0;
    let mut failures = String::new();
    for (format, rows) in corpus() {
        for row in &rows {
            let Some(changed) = change(format, row) else {
                continue;
            };
            checked += 1;
            let found = match parse(format, &changed.input) {
                Ok(got) => diff(&changed, &got),
                Err(other) => format!("    {other}\n"),
            };
            if !found.is_empty() {
                write!(
                    failures,
                    "\n  {format} / {}: {}\n{found}",
                    row.name,
                    shown(&changed.input)
                )
                .expect("write");
            }
        }
    }
    assert!(failures.is_empty(), "{failures}");
    assert!(checked >= floor, "only {checked} rows checked");
}

/// The bytes of a leading byte order mark.
fn bom_len(bytes: &[u8]) -> usize {
    if bytes.starts_with(BOM.as_bytes()) {
        BOM.len()
    } else {
        0
    }
}

/// `text` with its first words as they are and every other line number of a
/// YAML context left out: a context moves with the text above it.
fn without_context(message: &'static str) -> &'static str {
    message.split(" at line").next().unwrap_or_default()
}

/// A line more above the error: one line further down, the same column, the
/// same source line, and the span moved by that line's bytes — multi-byte
/// ones, which a parser that counts characters must not lose.
#[test]
fn a_line_above_moves_an_error_down_by_one() {
    check_changed(240, |format, row| {
        let bom = bom_len(&row.input);
        let cr_only = row.input.contains(&b'\r') && !row.input.contains(&b'\n');
        // Nothing may stand above these …
        let top_of_file = row.input[bom..].starts_with(b"<?xml")
            || row.input[bom..].starts_with(b"# escape")
            || format == "frontmatter";
        // … these read no comment that ends in a lone CR …
        let no_cr = cr_only && matches!(format, "toml" | "hcl" | "dotenv" | "ini" | "dockerfile");
        // … and these no blank or comment line behind a byte order mark.
        let no_bom = bom > 0 && matches!(format, "jsonl" | "dotenv" | "ini");
        if row.span.start < bom || top_of_file || no_cr || no_bom {
            return None;
        }
        let mut above = match format {
            "json" | "jsonl" | "csv" | "tsv" => "",
            "xml" => "<!-- я😀 -->",
            "ini" => "; я😀",
            _ => "# я😀",
        }
        .to_owned();
        above.push(if cr_only { '\r' } else { '\n' });
        let mut input = row.input.clone();
        input.splice(bom..bom, above.bytes());
        Some(Row {
            name: row.name,
            input,
            line: row.line + 1,
            col: row.col,
            span: row.span.start + above.len()..row.span.end + above.len(),
            snippet: row.snippet.clone(),
            message: without_context(row.message),
        })
    });
}

/// A byte order mark ahead of a file moves the span alone.
#[test]
fn a_byte_order_mark_moves_nothing_but_the_span() {
    check_changed(190, |format, row| {
        // HCL reads no mark; INI and dotenv read one into the first line.
        let refused = matches!(format, "hcl" | "ini" | "dotenv");
        // A declared encoding is the one thing a mark can contradict.
        let encoding = format == "xml" && row.input.windows(8).any(|w| w == b"US-ASCII");
        // JSON Lines reads no blank line behind a mark.
        let blank = format == "jsonl" && row.input.starts_with(b"\n");
        if refused || encoding || blank || bom_len(&row.input) > 0 {
            return None;
        }
        let mut input = BOM.as_bytes().to_vec();
        input.extend(&row.input);
        Some(Row {
            name: row.name,
            input,
            line: row.line,
            col: row.col,
            span: row.span.start + BOM.len()..row.span.end + BOM.len(),
            snippet: row.snippet.clone(),
            message: row.message,
        })
    });
}

/// A multi-byte character counts as the ASCII text of its length: putting
/// that text in its place moves nothing.
#[test]
fn columns_and_spans_count_bytes() {
    check_changed(50, |_, row| {
        let text = std::str::from_utf8(&row.input).ok()?;
        // Not where the character itself is at fault, nor the ones that are
        // more than a letter to a parser.
        let at_fault = row.input.get(row.span.start).is_some_and(|b| !b.is_ascii());
        let letter = |c: char| char::from(b'a' + (u32::from(c) % 26) as u8);
        let letters = |text: &str| {
            text.chars()
                .map(|c| match c {
                    '\u{feff}' | '\u{85}' | '\u{2028}' => c.to_string(),
                    c if c.is_ascii() => c.to_string(),
                    // Two characters stay two letters (`я` and `ю`).
                    c => letter(c).to_string().repeat(c.len_utf8()),
                })
                .collect::<String>()
        };
        let input = letters(text);
        if at_fault || input == text {
            return None;
        }
        Some(Row {
            name: row.name,
            input: input.into_bytes(),
            line: row.line,
            col: row.col,
            span: row.span.clone(),
            snippet: letters(&row.snippet),
            // It quotes names that changed.
            message: "",
        })
    });
}
