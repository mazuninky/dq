//! Properties of the position of an [`Error::Parse`]; the rows of
//! `parse_error_positions.rs` pin single ones.
//!
//! - [`Error::parse_at`] agrees with [`Scan`], a plain walk over the lines of
//!   the text, for any text and byte, and clamps any span.
//! - The error of a broken document is consistent: its line, its column and
//!   its source line are those of the byte its span starts at.
//! - Input that is no UTF-8 fails at the first byte that is none.
//! - Columns and spans count bytes: the ASCII text of the same length in
//!   place of every multi-byte character moves no error.
//! - A document read with `--raw-template-strings` has its errors placed in
//!   the file, whatever templates stand before them.

use std::ops::Range;

use dq_core::template_guard::{TemplateMode, guarded_parse};
use dq_core::{Error, by_name};
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::Config as ProptestConfig;

const BOM: &str = "\u{feff}";

/// The position of a parse error, without its message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    line: usize,
    col: usize,
    span: Range<usize>,
    snippet: String,
}

fn place(err: Error) -> Place {
    match err {
        Error::Parse {
            file: None,
            line,
            col,
            span,
            snippet,
            ..
        } => Place {
            line: line as usize,
            col: col as usize,
            span,
            snippet,
        },
        other => panic!("expected a parse error with no file, got {other:?}"),
    }
}

/// The lines of `bytes`, one after the other, as `(start, line)`: past a byte
/// order mark, without their breaks. A line break is `\n` or `\r\n`, or a
/// lone `\r` in bytes with no `\n`.
struct Scan<'a>(Vec<(usize, &'a [u8])>);

impl<'a> Scan<'a> {
    fn of(bytes: &'a [u8]) -> Self {
        let lf = bytes.contains(&b'\n');
        let mut start = if bytes.starts_with(BOM.as_bytes()) {
            BOM.len()
        } else {
            0
        };
        let mut lines = Vec::new();
        let mut at = start;
        while at < bytes.len() {
            let ends = if lf {
                bytes[at] == b'\n'
            } else {
                bytes[at] == b'\r'
            };
            if ends {
                let line = &bytes[start..at];
                lines.push((start, line.strip_suffix(b"\r").unwrap_or(line)));
                start = at + 1;
            }
            at += 1;
        }
        lines.push((start, &bytes[start..]));
        Self(lines)
    }

    /// Where byte `at` is, with the empty span there.
    fn place(&self, at: usize) -> Place {
        let line = self
            .0
            .iter()
            .rposition(|&(start, _)| start <= at)
            .unwrap_or(0);
        let (start, text) = self.0[line];
        Place {
            line: line + 1,
            col: at.saturating_sub(start) + 1,
            span: at..at,
            snippet: String::from_utf8_lossy(text).into_owned(),
        }
    }
}

/// `got` has the line, the column and the source line of the byte of `bytes`
/// its span starts at, and a span within `bytes`.
fn consistent(bytes: &[u8], got: &Place) -> Result<(), TestCaseError> {
    prop_assert!(got.span.start <= got.span.end, "{:?}", got.span);
    prop_assert!(got.span.end <= bytes.len(), "{:?}", got.span);
    let want = Scan::of(bytes).place(got.span.start);
    prop_assert_eq!(
        (got.line, got.col, &got.snippet),
        (want.line, want.col, &want.snippet)
    );
    Ok(())
}

/// A text of ASCII, every line break, tabs, multi-byte characters and emoji,
/// with a byte order mark or not, in one line break style or a mix.
fn text() -> impl Strategy<Value = String> {
    let breaks = prop_oneof![
        Just(vec!["\n"]),
        Just(vec!["\r\n"]),
        Just(vec!["\r"]),
        Just(vec!["\n", "\r\n", "\r", "\r\r\n"]),
    ];
    breaks.prop_flat_map(|breaks| {
        let fragment = prop_oneof![
            4 => select(vec![
                "a", "key: ", "{\"x\": 1}", "  ", "\t", "я", "ключ", "😀", "👩‍👩‍👧", "中文",
                "\u{85}", "\u{2028}", "\0", "\u{1b}[31m", BOM,
            ]),
            3 => select(breaks),
        ];
        (any::<bool>(), proptest::collection::vec(fragment, 0..24)).prop_map(|(bom, parts)| {
            let mut text = if bom { BOM.to_owned() } else { String::new() };
            text.extend(parts);
            text
        })
    })
}

/// A text and the offset of one of its characters, or its end.
fn text_and_offset() -> impl Strategy<Value = (String, usize)> {
    (text(), any::<prop::sample::Index>()).prop_map(|(text, index)| {
        let mut offsets: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
        offsets.push(text.len());
        let at = offsets[index.index(offsets.len())];
        (text, at)
    })
}

/// Documents of one format that all parse, their strings and comments
/// multi-byte.
fn documents(format: &str) -> &'static [&'static str] {
    match format {
        "json" => &[
            "{\n  \"ключ\": \"я😀\",\n  \"b\": [1, 2.5, true, null],\n\t\"c\": {\"d\": \"\\n\"}\n}\n",
            "\u{feff}{\r\n  \"a\": \"中文\",\r\n  \"b\": -1e3\r\n}\r\n",
            "{\r \"a\": \"я\",\r \"b\": []\r}\r",
            "[{\"x\": \"😀\"}, {\"x\": \"👩‍👩‍👧\"}, \"z\"]",
        ],
        "yaml" => &[
            "# ключ\nключ: я😀\nb: [1, \"中文\", {c: d}]\ne:\n  - f: 'я'\n  - |\n    я\n    g\n",
            "\u{feff}a: \"я\"\r\nb:\r\n  - 😀\r\n  - {c: 1}\r\n",
            "a: я\rb:\r  - 1\r  - \"x\"\r",
            "a: &x {я: 1}\n---\nb: &y 中\nc: *y\n---\n- d: 1e400\n- е: 18446744073709551616\n",
        ],
        "toml" => &[
            "# ключ\na = \"я😀\"\n\"ключ\" = [1, 2]\n\n[t]\nb = { c = \"中文\", d = 1.5 }\n",
            "\u{feff}a = 'я'\r\n[[t]]\r\n\"b\" = \"\"\"я\r\n😀\"\"\"\r\n",
        ],
        "xml" => &[
            "<a>\n  <!-- ключ -->\n  <b x=\"я\">😀</b>\n  <c/>\n\t<d>中文&amp;</d>\n</a>\n",
            "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<a я=\"1\">\r\n<б/>\r\n</a>\r\n",
            "<a>\r<b>я</b>\r</a>\r",
        ],
        "hcl" => &[
            "# ключ\na = \"я😀\"\nb = [1, \"中文\"]\nc \"я\" {\n  d = { e = 1, \"f\" = \"g\" }\n}\n",
            "a = \"я\"\r\nb {\r\n  c = f(\"😀\", 1)\r\n}\r\n",
        ],
        "ini" => &[
            "; ключ\n[секция]\nа=я\nb=\"😀\"\n\n[s]\nc=中文\n",
            "a=я\r\n[s]\r\nb='😀'\r\n",
        ],
        "dotenv" => &[
            "# ключ\nA=\"я😀\"\nexport B='中文'\nC=1\n",
            "A=я\r\nB=\"😀\"\r\n",
        ],
        "csv" => &[
            "ключ,b\nя,\"😀\"\n1,\"a\nb\"\n中文,2\n",
            "\u{feff}a,b\r\nя,1\r\n\r\n😀,2\r\n",
        ],
        other => panic!("no documents for {other}"),
    }
}

const FORMATS: [&str; 8] = ["json", "yaml", "toml", "xml", "hcl", "ini", "dotenv", "csv"];

/// The formats that decode the input before they read it: no other error
/// comes first.
const DECODED_WHOLE: [&str; 7] = ["json", "yaml", "toml", "xml", "hcl", "ini", "dotenv"];

/// A format of `formats` and a document of it.
fn document(formats: &[&'static str]) -> impl Strategy<Value = (&'static str, &'static str)> {
    select(formats.to_vec())
        .prop_flat_map(|format| (Just(format), select(documents(format).to_vec())))
}

/// Characters that break a document in most places.
const BREAKERS: [&str; 20] = [
    "@", "]", "}", "{", "[", ",", ":", "=", "\"", "'", "<", ">", "&", "\\", "\t", "\n", "\r", "|",
    "x", "\u{1}",
];

/// `text` with every multi-byte character replaced by as many ASCII letters
/// as it has bytes; a byte order mark stays one.
fn ascii_twin(text: &str) -> String {
    let (bom, rest) = text
        .strip_prefix(BOM)
        .map_or(("", text), |rest| (BOM, rest));
    let mut twin = bom.to_owned();
    for c in rest.chars() {
        if c.is_ascii() {
            twin.push(c);
        } else {
            // Two characters of a length stay two letters, or a key could
            // become another one's double.
            let letter = char::from(b'a' + (u32::from(c) % 26) as u8);
            twin.extend(std::iter::repeat_n(letter, c.len_utf8()));
        }
    }
    twin
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 4096,
        // No regressions file next to the test: none is tracked here.
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn parse_at_agrees_with_a_plain_scan((text, at) in text_and_offset()) {
        let bytes = text.as_bytes();
        let got = place(Error::parse_at(bytes, at..at, "x"));
        prop_assert_eq!(&got, &Scan::of(bytes).place(at));
        // The column counts bytes from the start of the line shown: the
        // byte is there, or in the line break that ends the line, or it is
        // in the byte order mark, which the line does not hold.
        let bom = if bytes.starts_with(BOM.as_bytes()) { BOM.len() } else { 0 };
        if at >= bom {
            let line_start = at - (got.col - 1);
            prop_assert!(bytes[line_start..].starts_with(got.snippet.as_bytes()));
            let past = (got.col - 1).saturating_sub(got.snippet.len());
            prop_assert!(past <= 1, "{} bytes past the line", past);
        } else {
            prop_assert_eq!((got.line, got.col), (1, 1));
        }
        prop_assert!(!got.snippet.contains('\n'));
    }

    #[test]
    fn parse_at_takes_any_bytes_and_span(
        bytes in proptest::collection::vec(
            prop_oneof![
                3 => any::<u8>(),
                2 => select(b"\r\n\n\rab".to_vec()),
                1 => select(BOM.as_bytes().to_vec()),
            ],
            0..64,
        ),
        bom in any::<bool>(),
        start in prop_oneof![0..80_usize, Just(usize::MAX)],
        end in prop_oneof![0..80_usize, Just(usize::MAX)],
    ) {
        let bytes = if bom { [BOM.as_bytes(), &bytes].concat() } else { bytes };
        let got = place(Error::parse_at(&bytes, Range { start, end }, "x"));
        consistent(&bytes, &got)?;
        // Clamped, not moved.
        prop_assert_eq!(got.span.start, start.min(bytes.len()));
        prop_assert_eq!(got.span.end, end.min(bytes.len()).max(got.span.start));
    }

    #[test]
    fn the_error_of_a_broken_document_is_consistent(
        (format, text) in document(&FORMATS),
        index in any::<prop::sample::Index>(),
        with in any::<u8>(),
    ) {
        // Any byte, so that the result may be no UTF-8 either.
        let mut bytes = text.as_bytes().to_vec();
        let at = index.index(bytes.len());
        bytes[at] = with;
        if let Err(err) = by_name(format).expect("known format").parse(&bytes) {
            consistent(&bytes, &place(err))?;
        }
    }

    #[test]
    fn bytes_that_are_no_utf8_are_placed_at_the_first_of_them(
        (format, text) in document(&DECODED_WHOLE),
        index in any::<prop::sample::Index>(),
        with in 0x80..=0xFF_u8,
    ) {
        // The declaration names the encoding to read in.
        if text.contains("<?xml") {
            return Ok(());
        }
        let mut bytes = text.as_bytes().to_vec();
        let at = index.index(bytes.len());
        bytes[at] = with;
        let Err(bad) = std::str::from_utf8(&bytes) else {
            return Ok(());
        };
        let err = by_name(format).expect("known format").parse(&bytes).expect_err("no UTF-8");
        let message = err.to_string();
        let got = place(err);
        prop_assert!(
            message.contains("UTF-8") || message.contains("invalid Unicode character"),
            "{}",
            message
        );
        prop_assert_eq!(got.span.start, bad.valid_up_to());
        consistent(&bytes, &got)?;
    }

    #[test]
    fn a_multi_byte_character_counts_its_bytes(
        (format, text) in document(&FORMATS),
        index in any::<prop::sample::Index>(),
        breaker in select(BREAKERS.to_vec()),
    ) {
        let offsets: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
        // Past the byte order mark.
        let at = offsets[index.index(offsets.len())]
            .max(if text.starts_with(BOM) { BOM.len() } else { 0 });
        let broken = format!("{}{breaker}{}", &text[..at], &text[at..]);
        let twin = ascii_twin(&broken);
        prop_assert_eq!(twin.len(), broken.len());
        let parse = |text: &str| by_name(format).expect("known format").parse(text.as_bytes());
        let (Err(of_broken), Err(of_twin)) = (parse(&broken), parse(&twin)) else {
            // One of the two is none, a letter being more to a format than
            // another character.
            return Ok(());
        };
        let (of_broken, of_twin) = (place(of_broken), place(of_twin));
        // Where the character itself is at fault, a letter is not.
        let is_ascii = |at: usize| broken.as_bytes().get(at).is_none_or(u8::is_ascii);
        prop_assume!(is_ascii(of_broken.span.start) && is_ascii(of_twin.span.start));
        // Where the text at fault ends is the parser's: a letter may end a
        // token another character does not.
        prop_assert_eq!(
            (of_broken.line, of_broken.col, of_broken.span.start),
            (of_twin.line, of_twin.col, of_twin.span.start),
            "{} / {}", broken, twin
        );
    }

    #[test]
    fn an_error_past_templates_is_placed_in_the_file(
        blocks in proptest::collection::vec(
            select(vec![
                "{{ x }}", "{{x}}", "{{ .Values.a.very.long.path }}", "${{ github.ref }}",
                "{{ `я😀` | quote }}", "{{- include `ключ` . -}}",
            ]),
            0..6,
        ),
        before in select(vec!["", "я", "x "]),
        line_break in select(vec!["\n", "\r\n"]),
        separator in select(vec![" ", "\t", "\n", "\r\n\t "]),
        format in select(vec!["json", "yaml", "toml"]),
    ) {
        let text = match format {
            // A TOML document has a pair a line.
            "toml" => {
                let mut text = String::new();
                for (n, block) in blocks.iter().enumerate() {
                    text.push_str(&format!("k{n} = '{before}{block}'{line_break}"));
                }
                text + "last = @" + line_break
            }
            _ => {
                let quote = if format == "json" { '"' } else { '\'' };
                let mut text = "[".to_owned();
                for block in &blocks {
                    text.push_str(&format!("{quote}{before}{block}{quote},{separator}"));
                }
                text + "@]" + line_break
            }
        };
        let at = text.rfind('@').expect("there is one");
        let err = guarded_parse(text.as_bytes(), TemplateMode::RawStrings, |bytes| {
            by_name(format).expect("known format").parse(bytes)
        })
        .expect_err("`@` starts no value");
        let got = place(err);
        prop_assert_eq!(got.span.start, at, "{}", text);
        consistent(text.as_bytes(), &got)?;
    }
}

/// The documents the properties break parse as they are.
#[test]
fn the_documents_parse() {
    for format in FORMATS {
        for text in documents(format) {
            for text in [(*text).to_owned(), ascii_twin(text)] {
                by_name(format)
                    .expect("known format")
                    .parse(text.as_bytes())
                    .unwrap_or_else(|e| panic!("{format} {text:?}: {e}"));
            }
        }
    }
}
