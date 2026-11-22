//! Where a file does not parse, on the terminal: the place, the source line
//! and a caret under the character at fault.
//!
//! ```text
//!   --> bad.json:2:14
//!    |
//!  2 |   "b": [1, 2,, 3]
//!    |              ^
//! ```
//!
//! The source line is shown as the terminal text of any other output is
//! ([`super::console`]), so a character of it can take several columns — an
//! ESC prints as `\x1b`, a wide character takes two, a combining mark none.
//! One list of [`Cell`]s gives both the text of the row and the column of
//! the caret, so the two cannot disagree.

use std::borrow::Cow;

use unicode_width::UnicodeWidthChar;

use super::console::{escape_control, escaped_char};
use crate::error::ParsePosition;

/// The most columns the source line takes; a wider one is cut around the
/// caret.
const MAX_COLUMNS: usize = 120;
/// A caret at most this far from an end of the line keeps that end shown.
const HALF: usize = MAX_COLUMNS / 2;
/// The most characters looked at on a side of the caret: a line of
/// characters that take no column is cut too.
const MAX_CHARS: usize = 4 * MAX_COLUMNS;
/// Columns a tab takes, wherever it stands.
const TAB: &str = "    ";
/// Marks the side of the source line that was cut; one column.
const CUT: char = '…';

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_BOLD_RED: &str = "\x1b[1;31m";

/// The lines that place a parse error: `  --> FILE:LINE:COL`, then the
/// source line between two empty gutter rows, the last with the caret. No
/// line break at the end. With `use_color` the same text, the place and the
/// gutter dim, the caret bold red.
pub(crate) fn source_block(at: &ParsePosition<'_>, use_color: bool) -> String {
    let paint = |style: &str, text: &str| {
        if use_color {
            format!("{style}{text}{ANSI_RESET}")
        } else {
            text.to_owned()
        }
    };
    let file = at.file.map_or(Cow::Borrowed("<stdin>"), |file| {
        escape_control(file.as_str())
    });
    let line = at.line.to_string();
    let row = Row::of(at);
    let place = paint(ANSI_DIM, &format!("  --> {file}:{line}:{}", at.col));
    let empty = paint(ANSI_DIM, &format!("{:1$}|", "", line.len() + 2));
    let numbered = paint(ANSI_DIM, &format!(" {line} |"));
    let sep = if row.text.is_empty() { "" } else { " " };
    let text = &row.text;
    let caret = paint(
        ANSI_BOLD_RED,
        &format!("^{}", "~".repeat(row.underline.saturating_sub(1))),
    );
    format!(
        "{place}\n{empty}\n{numbered}{sep}{text}\n{empty} {:1$}{caret}",
        "", row.caret
    )
}

/// A character of the source line, as the row shows it.
struct Cell {
    /// What is printed: the character, its escape, or a tab's spaces.
    text: String,
    /// The columns that takes.
    width: usize,
    /// The bytes the character takes in the line.
    len: usize,
}

impl Cell {
    fn of(c: char) -> Self {
        let (text, width) = if c == '\t' {
            (TAB.to_owned(), TAB.len())
        } else if let Some(escape) = escaped_char(c) {
            // An escape is ASCII.
            let width = escape.len();
            (escape, width)
        } else {
            (c.to_string(), c.width().unwrap_or(0))
        };
        Self {
            text,
            width,
            len: c.len_utf8(),
        }
    }
}

/// The characters of the line on one side of the caret, the nearest first,
/// as far as they can be shown.
struct Side {
    cells: Vec<Cell>,
    /// The columns of `cells`.
    width: usize,
    /// Whether `cells` reaches the end of the line on this side.
    whole: bool,
}

impl Side {
    /// Stops once a side is wider than the row: a line of 10 MB is not
    /// walked to its ends.
    fn of(chars: impl Iterator<Item = char>) -> Self {
        let mut side = Self {
            cells: Vec::new(),
            width: 0,
            whole: true,
        };
        for c in chars {
            if side.width > MAX_COLUMNS || side.cells.len() == MAX_CHARS {
                side.whole = false;
                break;
            }
            let cell = Cell::of(c);
            side.width += cell.width;
            side.cells.push(cell);
        }
        side
    }

    /// Whether the whole side fits in `columns`.
    fn fits(&self, columns: usize) -> bool {
        self.whole && self.width <= columns
    }

    /// Keep the nearest cells that fit in `columns`.
    fn cut(&mut self, columns: usize) {
        let mut width = 0;
        let mut kept = 0;
        for cell in &self.cells {
            if width + cell.width > columns {
                break;
            }
            width += cell.width;
            kept += 1;
        }
        self.cells.truncate(kept);
        self.width = width;
        self.whole = false;
    }
}

/// The source line, cut to [`MAX_COLUMNS`] around the caret.
struct Row {
    text: String,
    /// The columns before the caret.
    caret: usize,
    /// The columns the caret and its underline take.
    underline: usize,
}

impl Row {
    fn of(at: &ParsePosition<'_>) -> Self {
        let line = at.snippet;
        let col = usize::try_from(at.col).unwrap_or(usize::MAX);
        let byte = col.saturating_sub(1).min(line.len());
        // A column inside a character is at that character.
        let start = line.floor_char_boundary(byte);
        // Bytes of the line at fault, from `start` on.
        let at_fault = (byte - start).saturating_add(at.len);

        let mut left = Side::of(line[..start].chars().rev());
        let mut right = Side::of(line[start..].chars());
        // A caret past the last character takes a column too.
        let right_columns = right.width.max(1);
        if !(left.fits(MAX_COLUMNS) && right.fits(MAX_COLUMNS - left.width)) {
            if left.fits(HALF) {
                right.cut(MAX_COLUMNS - left.width - 1);
            } else if right.fits(HALF) {
                left.cut(MAX_COLUMNS - right_columns - 1);
            } else {
                left.cut(HALF - 1);
                right.cut(HALF - 1);
            }
            // A mark that was on a character cut off would be drawn on the
            // `…` instead.
            while !left.whole && left.cells.last().is_some_and(|cell| cell.width == 0) {
                left.cells.pop();
            }
        }

        let mut text = String::new();
        if !left.whole {
            text.push(CUT);
        }
        text.extend(left.cells.iter().rev().map(|cell| cell.text.as_str()));
        text.extend(right.cells.iter().map(|cell| cell.text.as_str()));
        if !right.whole {
            text.push(CUT);
        }
        Self {
            text,
            caret: left.width + usize::from(!left.whole),
            underline: underline(&right.cells, at_fault),
        }
    }
}

/// The columns of the characters of `cells` that hold their first `bytes`
/// bytes; 1 — a caret alone — for one character or none.
fn underline(cells: &[Cell], bytes: usize) -> usize {
    let mut offset = 0;
    let mut width = 0;
    let mut characters = 0;
    for cell in cells {
        if offset >= bytes {
            break;
        }
        offset += cell.len;
        width += cell.width;
        characters += 1;
    }
    if characters > 1 { width.max(1) } else { 1 }
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;
    use proptest::prelude::*;

    use super::*;

    /// The position `len` bytes from 1-based byte column `col` of `snippet`,
    /// line 2 of `f`.
    fn at(snippet: &str, col: u32, len: usize) -> ParsePosition<'_> {
        ParsePosition {
            file: Some(Utf8Path::new("f")),
            line: 2,
            col,
            len,
            snippet,
        }
    }

    /// The source row and the caret row of [`source_block`], past the gutter
    /// of a one-digit line and the space after it.
    fn rows(at: &ParsePosition<'_>) -> (String, String) {
        let block = source_block(at, false);
        let lines: Vec<_> = block.split('\n').collect();
        let [place, empty, source, caret] = lines.as_slice() else {
            panic!("four lines: {block}");
        };
        assert_eq!(*place, format!("  --> f:2:{}", at.col));
        assert_eq!(*empty, "   |");
        let row = |line: &str, gutter: &str| {
            let rest = line.strip_prefix(gutter).expect("the gutter");
            rest.strip_prefix(' ').unwrap_or(rest).to_owned()
        };
        (row(source, " 2 |"), row(caret, "   |"))
    }

    /// The rows for the error at the first `needle` of `snippet`, as long as
    /// it is.
    fn rows_at(snippet: &str, needle: &str) -> (String, String) {
        let start = snippet.find(needle).expect("needle");
        let col = u32::try_from(start + 1).expect("short");
        rows(&at(snippet, col, needle.len()))
    }

    /// The columns `text` takes.
    fn columns(text: &str) -> usize {
        text.chars().map(|c| c.width().unwrap_or(0)).sum()
    }

    /// The column before the caret of a caret row, which must hold nothing
    /// but it.
    fn caret_column(caret: &str) -> usize {
        let column = caret.find('^').expect("a caret");
        assert!(caret[..column].bytes().all(|b| b == b' '), "{caret:?}");
        assert!(caret[column + 1..].bytes().all(|b| b == b'~'), "{caret:?}");
        column
    }

    fn strings(source: &str, caret: &str) -> (String, String) {
        (source.to_owned(), caret.to_owned())
    }

    #[test]
    fn the_block_is_the_one_the_plan_shows() {
        let at = ParsePosition {
            file: Some(Utf8Path::new("bad.json")),
            line: 2,
            col: 14,
            len: 1,
            snippet: "  \"b\": [1, 2,, 3]",
        };
        assert_eq!(
            source_block(&at, false),
            "  --> bad.json:2:14\n   |\n 2 |   \"b\": [1, 2,, 3]\n   |              ^"
        );
    }

    #[test]
    fn the_gutter_is_as_wide_as_the_line_number() {
        let at = ParsePosition {
            line: 1234,
            file: None,
            ..at("ab", 2, 0)
        };
        assert_eq!(
            source_block(&at, false),
            "  --> <stdin>:1234:2\n      |\n 1234 | ab\n      |  ^"
        );
    }

    #[test]
    fn the_caret_moves_by_what_an_escape_takes() {
        assert_eq!(
            rows_at("a\u{1b}[31m!", "!"),
            strings("a\\x1b[31m!", "         ^")
        );
        assert_eq!(
            rows_at("\u{202e}\u{9b}\u{7f}!", "!"),
            strings("\\u202e\\x9b\\x7f!", "              ^")
        );
        // At the escaped character itself: under its first column.
        assert_eq!(rows_at("a\u{1b}b", "\u{1b}"), strings("a\\x1bb", " ^"));
    }

    #[test]
    fn a_tab_is_four_columns_wherever_it_stands() {
        assert_eq!(rows_at("\t!", "!"), strings("    !", "    ^"));
        assert_eq!(
            rows_at("a\tb\t!", "!"),
            strings("a    b    !", "          ^")
        );
    }

    #[test]
    fn a_wide_character_is_two_columns() {
        assert_eq!(rows_at("日本!", "!"), strings("日本!", "    ^"));
        assert_eq!(rows_at("\u{1f980}é!", "!"), strings("\u{1f980}é!", "   ^"));
        // One character is one caret, however wide.
        assert_eq!(rows_at("a日b", "日"), strings("a日b", " ^"));
    }

    #[test]
    fn a_combining_mark_takes_no_column() {
        assert_eq!(
            rows_at("e\u{301}x\u{200d}!", "!"),
            strings("e\u{301}x\u{200d}!", "  ^")
        );
    }

    #[test]
    fn a_caret_past_the_last_character_is_one_column_after_it() {
        assert_eq!(rows(&at("{\"a\"", 5, 0)), strings("{\"a\"", "    ^"));
        assert_eq!(rows(&at("日", 4, 0)), strings("日", "  ^"));
        // Further than that is clamped to it.
        assert_eq!(rows(&at("ab", 9, 3)), strings("ab", "  ^"));
        assert_eq!(rows(&at("ab", u32::MAX, usize::MAX)), strings("ab", "  ^"));
    }

    #[test]
    fn an_empty_line_has_an_empty_row_and_a_caret_in_column_1() {
        assert_eq!(
            source_block(&at("", 1, 0), false),
            "  --> f:2:1\n   |\n 2 |\n   | ^"
        );
        // A column with no line to be in.
        assert_eq!(rows(&at("", 4, 2)), strings("", "^"));
        assert_eq!(rows(&at("a", 0, 0)), strings("a", "^"));
    }

    #[test]
    fn a_span_of_several_characters_is_underlined() {
        assert_eq!(rows_at("a bcd e", "bcd"), strings("a bcd e", "  ^~~"));
        // By columns: an escape takes four, a wide character two, a
        // combining mark none.
        assert_eq!(
            rows_at("a\u{1b}日e\u{301}z", "\u{1b}日e\u{301}"),
            strings("a\\x1b日e\u{301}z", " ^~~~~~~")
        );
        assert_eq!(rows_at("e\u{301}z", "e\u{301}"), strings("e\u{301}z", "^"));
    }

    #[test]
    fn a_span_longer_than_the_line_is_underlined_to_its_end() {
        assert_eq!(rows(&at("a bc", 3, 400)), strings("a bc", "  ^~"));
        // The line break itself is no character of the line.
        assert_eq!(rows(&at("a bc", 4, 2)), strings("a bc", "   ^"));
    }

    #[test]
    fn a_span_with_no_length_is_one_caret() {
        assert_eq!(rows(&at("a bc", 3, 0)), strings("a bc", "  ^"));
    }

    #[test]
    fn a_column_inside_a_character_is_at_that_character() {
        // `日` is bytes 1 to 3, columns 2 to 4.
        for col in 2..=4 {
            assert_eq!(rows(&at("a日b", col, 0)), strings("a日b", " ^"), "{col}");
        }
        // The span ends where it would: `b` is not in it, then it is.
        assert_eq!(rows(&at("a日b", 3, 2)), strings("a日b", " ^"));
        assert_eq!(rows(&at("a日b", 3, 3)), strings("a日b", " ^~~"));
    }

    #[test]
    fn an_invalid_byte_is_one_character() {
        let line = String::from_utf8_lossy(b"a\xFFb");
        assert_eq!(rows(&at(&line, 2, 1)), strings("a\u{fffd}b", " ^"));
    }

    #[test]
    fn a_long_line_is_cut_around_the_caret() {
        let line = "abcdefghij".repeat(300);
        let (source, caret) = rows(&at(&line, 1501, 1));
        assert_eq!(columns(&source), MAX_COLUMNS);
        assert!(source.starts_with(CUT) && source.ends_with(CUT), "{source}");
        assert_eq!(caret_column(&caret), 60);
        assert_eq!(source.chars().nth(60), Some('a'));
    }

    #[test]
    fn a_long_line_keeps_the_end_the_caret_is_near() {
        let line = "abcdefghij".repeat(300);
        for (col, cut_left) in [(1, false), (61, false), (62, true), (2940, true)] {
            let (source, caret) = rows(&at(&line, col, 1));
            assert_eq!(columns(&source), MAX_COLUMNS, "{col}");
            assert_eq!(source.starts_with(CUT), cut_left, "{col}");
            assert!(source.ends_with(CUT), "{col}");
            let shown = source.chars().nth(caret_column(&caret));
            assert_eq!(shown, line.chars().nth(col as usize - 1), "{col}");
        }
        for (col, at_fault) in [(2941, 1), (3000, 1)] {
            let (source, caret) = rows(&at(&line, col, at_fault));
            assert_eq!(columns(&source), MAX_COLUMNS, "{col}");
            assert!(source.starts_with(CUT), "{col}");
            assert!(source.ends_with("hij"), "{col}");
            let shown = source.chars().nth(caret_column(&caret));
            assert_eq!(shown, line.chars().nth(col as usize - 1), "{col}");
        }
        // Past the end: the caret takes the last column.
        let (source, caret) = rows(&at(&line, 3001, 0));
        assert_eq!(columns(&source), MAX_COLUMNS - 1);
        assert_eq!(caret_column(&caret), MAX_COLUMNS - 1);
    }

    #[test]
    fn a_line_of_120_columns_is_whole() {
        let line = "a".repeat(MAX_COLUMNS);
        for col in [1, 60, 120, 121] {
            let (source, caret) = rows(&at(&line, col, 0));
            assert_eq!(source, line, "{col}");
            assert_eq!(caret_column(&caret), col as usize - 1);
        }
        let line = "a".repeat(MAX_COLUMNS + 1);
        assert_ne!(rows(&at(&line, 1, 0)).0, line);
    }

    #[test]
    fn a_cut_splits_no_escape_and_no_wide_character() {
        // 119 columns left for the 6-column escapes: 19 of them, 114.
        let line = "\u{202e}".repeat(100);
        let (source, caret) = rows(&at(&line, 1, 0));
        assert_eq!(source, format!("{}{CUT}", "\\u202e".repeat(19)));
        assert_eq!(caret_column(&caret), 0);
        // 59 columns on each side of the caret: 29 wide characters.
        let line = "日".repeat(200);
        let (source, caret) = rows(&at(&line, 301, 0));
        assert_eq!(source, format!("{CUT}{}{CUT}", "日".repeat(58)));
        assert_eq!(caret_column(&caret), 59);
    }

    #[test]
    fn an_underline_of_a_cut_line_ends_before_the_mark() {
        let line = "a".repeat(3000);
        let (source, caret) = rows(&at(&line, 1, 3000));
        assert_eq!(columns(&source), MAX_COLUMNS);
        assert_eq!(caret, format!("^{}", "~".repeat(MAX_COLUMNS - 2)));
    }

    #[test]
    fn a_mark_is_not_left_on_the_cut() {
        // The cut on the left falls between `e` and its mark.
        let line = format!(
            "{}e\u{301}{}!{}",
            "x".repeat(100),
            "y".repeat(59),
            "z".repeat(100)
        );
        let (source, caret) = rows_at(&line, "!");
        assert!(source.starts_with("…y"), "{source}");
        assert_eq!(caret_column(&caret), 60);
    }

    #[test]
    fn a_line_of_characters_with_no_width_is_cut_too() {
        let line = "\u{301}".repeat(100_000);
        let (source, _) = rows(&at(&line, 100_001, 0));
        assert!(source.chars().count() <= 2 * MAX_CHARS + 2);
    }

    /// `text` without the ANSI styles [`source_block`] writes.
    fn plain(text: &str) -> String {
        [ANSI_RESET, ANSI_DIM, ANSI_BOLD_RED]
            .iter()
            .fold(text.to_owned(), |text, style| text.replace(style, ""))
    }

    #[test]
    fn colour_dims_the_place_and_the_gutter_and_reddens_the_caret() {
        assert_eq!(
            source_block(&at("a bc", 3, 2), true),
            "\u{1b}[2m  --> f:2:3\u{1b}[0m\n\
             \u{1b}[2m   |\u{1b}[0m\n\
             \u{1b}[2m 2 |\u{1b}[0m a bc\n\
             \u{1b}[2m   |\u{1b}[0m   \u{1b}[1;31m^~\u{1b}[0m"
        );
    }

    #[test]
    fn the_file_name_holds_no_control() {
        let at = ParsePosition {
            file: Some(Utf8Path::new("a\n\u{1b}b\u{202e}")),
            ..at("x", 1, 0)
        };
        assert!(
            source_block(&at, false).starts_with("  --> a\\x0a\\x1bb\\u202e:2:1\n"),
            "{}",
            source_block(&at, false)
        );
    }

    /// Any character, the ones a terminal acts on as likely as the others.
    fn character() -> impl Strategy<Value = char> {
        prop_oneof![
            any::<char>(),
            proptest::char::range('\u{0}', '\u{9f}'),
            proptest::char::range('\u{2028}', '\u{202e}'),
            Just('日'),
            Just('\u{301}'),
        ]
    }

    proptest! {
        /// Whatever the line and the position: four lines, no raw control
        /// in any, none wider than the row, colour adding styles only, and
        /// the caret as far as the text before the character at fault.
        #[test]
        fn any_line_at_any_position_is_shown_safely(
            chars in proptest::collection::vec(character(), 0..300),
            col in 0_u32..1300,
            len in 0_usize..1300,
        ) {
            let line: String = chars.into_iter().collect();
            let at = at(&line, col, len);
            let block = source_block(&at, false);
            for row in block.split('\n') {
                prop_assert_eq!(escape_control(row), row);
            }
            prop_assert_eq!(block.split('\n').count(), 4);
            prop_assert_eq!(plain(&source_block(&at, true)), block);

            let (source, caret) = rows(&at);
            prop_assert!(columns(&source) <= MAX_COLUMNS);
            // A caret past the end of a line as wide as the row is past it.
            prop_assert!(caret_column(&caret) <= MAX_COLUMNS);
            prop_assert!(caret.len() <= MAX_COLUMNS + 1);
            // Whatever `len` counts (the span of a UTF-16 file is in the
            // bytes of the file), the underline ends with the line.
            prop_assert!(caret.len() <= columns(&source).max(caret_column(&caret) + 1));
        }

        /// A line of any length: the caret stands under the character at
        /// fault in what is shown of it, the `…` of a cut included, and the
        /// row is cut only when the line is wider than it.
        #[test]
        fn the_caret_is_under_the_character_of_a_cut_line_too(
            before in proptest::collection::vec(character(), 0..300),
            after in proptest::collection::vec(character(), 0..300),
        ) {
            // The one `!` of the line is the character at fault.
            let keep = |chars: &[char]| chars.iter().filter(|c| **c != '!').collect::<String>();
            let line = format!("{}!{}", keep(&before), keep(&after));
            let (source, caret) = rows_at(&line, "!");
            let shown = source.find('!').expect("the character at fault");
            prop_assert_eq!(caret_column(&caret), columns(&source[..shown]));
            prop_assert_eq!(caret, format!("{:1$}^", "", columns(&source[..shown])));
            let whole: String = line.chars().map(|c| Cell::of(c).text).collect();
            if columns(&whole) <= MAX_COLUMNS {
                prop_assert_eq!(source, whole);
            } else {
                prop_assert!(columns(&source) <= MAX_COLUMNS);
                // Of a cut line no more is left out than it has to: the next
                // character, six columns at the most, on each side cut.
                prop_assert!(columns(&source) > MAX_COLUMNS - 2 * 6, "{source}");
                for part in source.split(CUT) {
                    prop_assert!(whole.contains(part), "{part}");
                }
            }
        }

        /// A line that is not cut: the row is the line as the terminal shows
        /// it, the caret as far along as what is before the character.
        #[test]
        fn the_caret_of_a_whole_line_is_under_the_character(
            before in proptest::collection::vec(character(), 0..15),
            after in proptest::collection::vec(character(), 0..5),
        ) {
            let show = |chars: &[char]| {
                let text: String = chars.iter().collect();
                text.split('\t').map(escape_control).collect::<Vec<_>>().join(TAB)
            };
            let line: String = before.iter().chain(&after).collect();
            let start: usize = before.iter().map(|c| c.len_utf8()).sum();
            let (source, caret) = rows(&at(&line, u32::try_from(start + 1).expect("short"), 0));
            let shown = show(&before);
            prop_assert_eq!(caret, format!("{:1$}^", "", columns(&shown)));
            prop_assert_eq!(source, format!("{shown}{}", show(&after)));
        }
    }
}
